use crate::{
    fault::{MirErrorKind, MirResult},
    function::FunctionLowerer,
};
use ast_model::{self as ast, TypeId};
use mir_model as mir;
use soul_utils::{TypeModifier, fault::Fault, intrinsics::IntrinsicFunction, span::Span};

impl<'a> FunctionLowerer<'a> {
    /// `is_tail`: whether `statement` is the last entry of a block currently
    /// in tail position (see `lower_body`) — only meaningful for
    /// `StatementKind::Expression`, where it's further narrowed by
    /// `!ends_semicolon` before being handed to `lower_expression_statement`
    /// (a trailing `;` always suppresses the implicit-return treatment,
    /// same as the resolver's own tail-position convention).
    pub(super) fn lower_statement(
        &mut self,
        return_local: Option<mir::LocalId>,
        statement: &ast::Statement,
        is_tail: bool,
    ) -> MirResult<()> {
        match &statement.node {
            ast::StatementKind::Variable(variable) => self.lower_variable(statement, variable),
            ast::StatementKind::Assignment(assignment) => self.lower_assignment(assignment),
            ast::StatementKind::Expression {
                expression,
                ends_semicolon,
            } => self.lower_expression_statement(
                return_local,
                statement,
                *expression,
                is_tail && !ends_semicolon,
            ),
            _ => Err(Fault::error_with_kind(
                MirErrorKind::UnsupportedStatementKind,
                Some(statement.span),
            )),
        }
    }

    fn lower_variable(
        &mut self,
        stmt: &ast::Statement,
        var: &ast::Variable,
    ) -> Result<(), Fault<MirErrorKind>> {
        let ast::VarPattern::Simple { binding, modifier } = &var.pattern else {
            return Err(Fault::error_with_kind(
                MirErrorKind::NonSimpleVariablePatternUnsupported,
                Some(stmt.span),
            ));
        };

        let Some(init) = var.initialize_value else {
            return Err(Fault::error_with_kind(
                MirErrorKind::UninitializedVariableUnsupported,
                Some(stmt.span),
            ));
        };

        let ty = self
            .declares
            .get_variable_type(binding.id)
            .and_then(|(_, ty, _)| ty.clone())
            .ok_or_else(|| {
                Fault::error_with_kind(
                    MirErrorKind::VariableHasNoResolvedType,
                    Some(binding.ident.span()),
                )
            })?;

        self.require_lowerable(&ty, binding.ident.span())?;
        let rvalue = self.lower_rvalue(init)?;
        let local = self.alloc_local(ty, *modifier, binding.ident.span());
        self.node_to_local.insert(binding.id, local);
        // Tracked as a `body_locals` entry *before* `push_assign` so this
        // very declaration's own initializing write already gets its
        // `SetDropFlag(local, true)` — matching `docs/mir-design.md`'s "every
        // place written via Assign" rule, and making the local eligible for
        // `seal_return`'s scope-exit `Drop` chain (parameters/`this`/the
        // return local, and every compiler-internal temp elsewhere in this
        // lowerer, are deliberately never added here — see `body_locals`'s
        // own docs).
        self.body_locals.push(local);
        self.push_assign(mir::Place::local(local), rvalue);
        Ok(())
    }

    /// Dispatches an intrinsic call reached in statement position. `Ok(None)`
    /// means `call` isn't an intrinsic at all (no `IntrinsicResolve` stored
    /// for it) — the caller should fall through to the ordinary
    /// `FunctionResolve`-based `lower_call` path instead.
    pub(super) fn try_lower_intrinsic_statement(
        &mut self,
        call: &ast::FunctionCall,
        span: Span,
    ) -> MirResult<Option<()>> {
        let Some(resolve) = self.declares.get_intrinsic_resolve(call.id) else {
            return Ok(None);
        };
        match resolve.kind {
            IntrinsicFunction::Assert => self.lower_assert_intrinsic(call, span)?,
            IntrinsicFunction::Panic => self.lower_panic_intrinsic(call, span)?,
            other => {
                return Err(Fault::error_with_kind(
                    MirErrorKind::UnsupportedIntrinsic {
                        name: other.as_str().into(),
                    },
                    Some(span),
                ));
            }
        }
        Ok(Some(()))
    }

    /// Lowers `left = right` (compound assignments like `n -= 1` are already
    /// desugared by the parser into `left = left - 1` before this ever runs,
    /// so `lower_rvalue` handles the right-hand side with no special-casing).
    /// `left` is a bare, already-declared variable, or any other place
    /// expression `resolve_place_expression` accepts (a struct field write, a
    /// slice-index write, a `*p` dereference write).
    fn lower_assignment(&mut self, assignment: &ast::Assignment) -> MirResult<()> {
        let left = &self.store.expressions[assignment.left];
        let place = match &left.node {
            ast::ExpressionKind::Variable(var) => {
                mir::Place::local(self.resolve_local(var, left.span)?)
            }
            ast::ExpressionKind::FieldAccess(_)
            | ast::ExpressionKind::Index(_)
            | ast::ExpressionKind::Deref(_) => {
                self.resolve_place_expression(assignment.left, left.span)?.0
            }
            _ => {
                return Err(Fault::error_with_kind(
                    MirErrorKind::AssignmentTargetUnsupported,
                    Some(left.span),
                ));
            }
        };

        let rvalue = self.lower_rvalue(assignment.right)?;
        self.push_assign(place, rvalue);
        Ok(())
    }

    /// Lowers a free-function call `name(args...)` or a receiver method call
    /// `receiver.name(args...)`. Only these plain shapes are supported this
    /// slice: no type-qualified callee (`Type::method()`), no generics, no
    /// named arguments, no `defer f()` — each of those gets
    /// `UnsupportedCallShape`.
    ///
    /// A call is a *terminator* (`mir_model::Terminator::Call`), not a plain
    /// statement, because it can diverge — so this seals the current block
    /// with the call and opens a fresh one as the continuation, returning the
    /// operand that reads the result (if any) out of that new block. Every
    /// caller of this (an operand deep inside `a + f(b)`, a loop condition,
    /// etc.) only ever looks at the *returned* operand, never assumes which
    /// block is current afterward, so this composes with the rest of the
    /// lowerer for free — nothing needs to change at the call sites.
    pub(super) fn lower_call(
        &mut self,
        call: &ast::FunctionCall,
        span: Span,
        want_result: bool,
    ) -> MirResult<Option<mir::Operand>> {
        let receiver_expr = match &call.callee {
            None => None,
            Some(ast::FunctionCallee {
                kind: ast::FunctionCalleeKind::Expression(id),
                ..
            }) => Some(*id),
            Some(ast::FunctionCallee {
                kind: ast::FunctionCalleeKind::Type(_),
                ..
            }) => {
                return Err(Fault::error_with_kind(
                    MirErrorKind::UnsupportedCallShape,
                    Some(span),
                ));
            }
        };
        if !call.generics.is_empty() {
            return Err(Fault::error_with_kind(
                MirErrorKind::UnsupportedCallShape,
                Some(span),
            ));
        }

        let Some(resolve) = self.declares.get_function_resolve(call.id) else {
            return Err(Fault::error_with_kind(
                MirErrorKind::FunctionCallHasNoResolvedTarget,
                Some(span),
            ));
        };
        if resolve.is_defer {
            return Err(Fault::error_with_kind(
                MirErrorKind::UnsupportedCallShape,
                Some(span),
            ));
        }

        let Some((signature, _)) = self.declares.get_function(resolve.id) else {
            return Err(Fault::error_with_kind(
                MirErrorKind::FunctionCallHasNoResolvedTarget,
                Some(span),
            ));
        };
        let return_type_id = signature.return_type;
        // Whichever of `func(..)`/`&this`/`this`/`&mut this` the *callee*
        // declares its receiver as: `&this`/`&mut this` borrow the receiver
        // (matching the callee's own `Reference`-typed local set up in
        // `FunctionLowerer::lower`), `this` still passes a plain by-value
        // copy. Enforcing that the caller doesn't e.g. pass `&mut` into a
        // `&this` call, or mutate through a shared borrow, is still the
        // borrow checker's job (M2), not this lowerer's.
        let expects_receiver = !matches!(
            signature.function_kind,
            ast::FunctionThisKind::Static
                | ast::FunctionThisKind::Ctor
                | ast::FunctionThisKind::ArrayCtor
        );

        let mut args = Vec::with_capacity(call.arguments.len() + expects_receiver as usize);
        if expects_receiver {
            let Some(receiver_expr) = receiver_expr else {
                // Defensive: the resolver only ever matches a receiver-typed
                // function against a call whose callee actually supplied a
                // receiver value — trust that, same as elsewhere in this
                // lowerer.
                return Err(Fault::error_with_kind(
                    MirErrorKind::FunctionCallHasNoResolvedTarget,
                    Some(span),
                ));
            };
            let receiver_operand = match signature.function_kind {
                ast::FunctionThisKind::MutRef => {
                    self.lower_receiver_ref(receiver_expr, true, span)?
                }
                ast::FunctionThisKind::ConstRef => {
                    self.lower_receiver_ref(receiver_expr, false, span)?
                }
                // A consuming `this` receiver is structurally the same bare-
                // variable operand as an ordinary argument below (`obj.consume()`
                // reads `obj` exactly like `consume(obj)` would) — same
                // Move-eligibility treatment.
                _ => self.lower_call_operand(receiver_expr)?,
            };
            args.push(receiver_operand);
        }
        for argument in &call.arguments {
            if argument.name.is_some() {
                return Err(Fault::error_with_kind(
                    MirErrorKind::UnsupportedCallShape,
                    Some(span),
                ));
            }
            args.push(self.lower_call_operand(argument.value)?);
        }

        let is_none_return = return_type_id == TypeId::NONE;
        let destination_local = if want_result && !is_none_return {
            let return_type = self
                .declares
                .get_type(return_type_id)
                .cloned()
                .expect("InnerFunctionSignature.return_type is always an interned TypeId");
            self.require_lowerable(&return_type, span)?;
            Some(self.alloc_local(return_type, TypeModifier::Immut, span))
        } else {
            None
        };
        let destination = destination_local.map(mir::Place::local);

        let next = self.new_block();
        self.seal(
            mir::Terminator::Call {
                id: resolve.id,
                arguments: args.into(),
                destination,
                target: Some(next),
            },
            Some(next),
        );

        Ok(destination_local.map(|local| mir::Operand::Copy(mir::Place::local(local))))
    }

    /// Lowers a call argument (or a consuming-`this` receiver, which reads
    /// exactly the same way — see `lower_call`) to an `Operand`, choosing
    /// `Move` over `Copy` when it's a bare `Variable` of a move-only type
    /// (per `is_auto_copy`) — `docs/mir-design.md`'s move/drop mechanics,
    /// deliberately scoped for now to exactly this bare-variable case:
    /// `SetDropFlag` is per-`LocalId`, not per-place, so a field/index/deref
    /// projection (`consume(container.item)`) has no sound way to express
    /// "only this one field moved" yet — that stays a plain `Copy` (falls
    /// through to `lower_operand`, same as before this existed) until
    /// partial-move tracking lands (see M2's TODO.md entry). Anything that
    /// isn't a bare variable at all (a literal, a nested computation, a
    /// struct-constructor literal passed inline, ...) has no existing place
    /// to move from in the first place, so it falls through the same way.
    fn lower_call_operand(&mut self, expr_id: ast::ExpressionId) -> MirResult<mir::Operand> {
        let expr = &self.store.expressions[expr_id];
        let ast::ExpressionKind::Variable(var) = &expr.node else {
            return self.lower_operand(expr_id);
        };
        let span = expr.span;
        let local = self.resolve_local(var, span)?;
        let ty = self.locals[local].ty.clone();
        let place = mir::Place::local(local);
        if self.is_auto_copy(&ty, span)? {
            return Ok(mir::Operand::Copy(place));
        }
        self.statements.push(mir::Statement::MarkMoved(local));
        self.statements
            .push(mir::Statement::SetDropFlag(local, false));
        Ok(mir::Operand::Move(place))
    }

    /// Lowers `assert(cond)`: continues normally if `cond` is `true`,
    /// otherwise panics. `cond` goes through the same `lower_bool_condition`
    /// machinery as an `if`/`while` condition — same restrictions apply.
    fn lower_assert_intrinsic(&mut self, call: &ast::FunctionCall, span: Span) -> MirResult<()> {
        let cond_expr = self.intrinsic_sole_argument(call, IntrinsicFunction::Assert, span)?;
        let cond = self.lower_bool_condition(cond_expr)?;
        let msg = mir::Operand::Constant(mir::ConstValue::Str("assertion failed".to_string()));

        let next = self.new_block();
        self.seal(
            mir::Terminator::Assert {
                cond,
                expected: true,
                msg,
                target: next,
                span,
            },
            Some(next),
        );
        Ok(())
    }

    /// Lowers `panic(msg)`: unconditionally diverges. Modeled as an `Assert`
    /// that's always false against `expected: true`, so it always takes the
    /// panic path — `target` is allocated (the shape needs a `BlockId`) but
    /// genuinely unreachable, so no block is ever inserted for it, and the
    /// cursor becomes unreachable afterward (`next: None`), same as `return`.
    fn lower_panic_intrinsic(&mut self, call: &ast::FunctionCall, span: Span) -> MirResult<()> {
        let msg_expr = self.intrinsic_sole_argument(call, IntrinsicFunction::Panic, span)?;
        let msg = self.lower_operand(msg_expr)?;

        let dead = self.new_block();
        self.seal(
            mir::Terminator::Assert {
                cond: mir::Operand::Constant(mir::ConstValue::Bool(false)),
                expected: true,
                msg,
                target: dead,
                span,
            },
            None,
        );
        Ok(())
    }

    /// The one argument an `assert`/`panic` intrinsic call takes, guarded
    /// against arity mismatches — the resolver logs a fault on a wrong count
    /// but still stores the resolution and lets the call through, so this
    /// must not assume `call.arguments` has the expected length.
    fn intrinsic_sole_argument(
        &self,
        call: &ast::FunctionCall,
        kind: IntrinsicFunction,
        span: Span,
    ) -> MirResult<ast::ExpressionId> {
        match call.arguments.as_slice() {
            [argument] => Ok(argument.value),
            _ => Err(Fault::error_with_kind(
                MirErrorKind::IntrinsicArityMismatch {
                    name: kind.as_str().into(),
                    expected: kind.arity(),
                    got: call.arguments.len(),
                },
                Some(span),
            )),
        }
    }
}
