use crate::{
    fault::{MirErrorKind, MirResult},
    function::FunctionLowerer,
};
use ast_model::{self as ast, TypeId};
use mir_model as mir;
use sol_utils::{TypeModifier, fault::Fault, intrinsics::IntrinsicFunction, span::Span};

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

        let ty = self.declares.intern_type(ty);
        self.require_lowerable(ty, binding.ident.span())?;
        let rvalue = self.lower_movable_rvalue(init)?;
        let local = self.alloc_local(ty, *modifier, binding.ident.span());
        self.node_to_local.insert(binding.id, local);
        // Tracked in the *innermost* currently-open `scopes` frame — before
        // `push_assign`, so this very declaration's own initializing write
        // already gets its `SetDropFlag(local, true)` — matching
        // `docs/mir-design.md`'s "every place written via Assign" rule, and
        // making the local eligible for that frame's own scope-exit `Drop`
        // chain (`seal_return`/`seal_scope_exit`). Parameters/`this`/the
        // return local, and every compiler-internal temp elsewhere in this
        // lowerer, are deliberately never added to any frame — see
        // `scopes`'s own docs.
        self.scopes
            .last_mut()
            .expect("scopes always has at least the function's own top-level frame")
            .push(local);
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

        let rvalue = self.lower_movable_rvalue(assignment.right)?;
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

        let is_last_variadic = signature.parameters.last().is_some_and(|p| p.is_variadic);

        if is_last_variadic && signature.parameters.len() != call.arguments.len() {
            let span = self.last_argument_span(call).unwrap_or(call.name.span());

            return Err(Fault::error_with_kind(
                MirErrorKind::MissingVarargs,
                Some(span),
            ));
        }

        // The `varargs` marker is always the last declared parameter (see
        // `ast_parser::parse::function`) — its corresponding call argument
        // is never a plain value but the compile-time-only `varargs.[...]`
        // list (rules 2/3 of the `varargs` design, enforced by the
        // resolver's own `check_varargs_argument`), which flattens into zero
        // or more extra trailing operands rather than a single argument.
        // Computed up front, before `signature`'s borrow of `self.declares`
        // would otherwise conflict with the `&mut self` calls below.
        let variadic_arg_index = if is_last_variadic {
            Some(call.arguments.len().saturating_sub(1))
        } else {
            None
        };

        signature
            .parameters
            .last()
            .is_some_and(|p| p.is_variadic)
            .then(|| call.arguments.len().saturating_sub(1));

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
        let mut deferred_receiver = None;
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
            match signature.function_kind {
                // Borrowed only once the arguments are evaluated (right before
                // the call), so an argument reading the receiver —
                // `c.set(c.get())` — never overlaps this `&mut`. A path with
                // no index in it has no side effects, so nothing observable
                // moves.
                ast::FunctionThisKind::MutRef if self.is_plain_place(receiver_expr) => {
                    deferred_receiver = Some(receiver_expr);
                }
                ast::FunctionThisKind::MutRef => {
                    args.push(self.lower_receiver_ref(receiver_expr, true, span)?);
                }
                ast::FunctionThisKind::ConstRef => {
                    args.push(self.lower_receiver_ref(receiver_expr, false, span)?);
                }
                // A consuming `this` receiver is structurally the same bare-
                // variable operand as an ordinary argument below (`obj.consume()`
                // reads `obj` exactly like `consume(obj)` would) — same
                // Move-eligibility treatment.
                _ => args.push(self.lower_move_aware_operand(receiver_expr)?),
            }
        }
        for (index, argument) in call.arguments.iter().enumerate() {
            if argument.name.is_some() {
                return Err(Fault::error_with_kind(
                    MirErrorKind::UnsupportedCallShape,
                    Some(span),
                ));
            }
            if Some(index) == variadic_arg_index {
                self.lower_varargs_argument(argument.value, span, &mut args)?;
            } else {
                args.push(self.lower_move_aware_operand(argument.value)?);
            }
        }

        if let Some(receiver_expr) = deferred_receiver {
            let receiver = self.lower_receiver_ref(receiver_expr, true, span)?;
            args.insert(0, receiver);
        }

        let is_none_return = return_type_id == TypeId::NONE;
        let destination_local = if want_result && !is_none_return {
            self.require_lowerable(return_type_id, span)?;
            Some(self.alloc_local(return_type_id, TypeModifier::Immut, span))
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

    // A variable, or a chain of field accesses and derefs on one: a place
    // whose evaluation has no side effects.
    fn is_plain_place(&self, expr_id: ast::ExpressionId) -> bool {
        match &self.store.expressions[expr_id].node {
            ast::ExpressionKind::Variable(_) => true,
            ast::ExpressionKind::FieldAccess(field_access) => {
                self.is_plain_place(field_access.object)
            }
            ast::ExpressionKind::Deref(deref) => self.is_plain_place(deref.value),
            _ => false,
        }
    }

    /// Flattens `varargs.[a, b, c]` into zero or more extra trailing
    /// operands pushed straight onto `args` — never a single aggregate
    /// operand (rule 3 of the `varargs` design: this is a purely compile-
    /// time construct, not a runtime array/slice). Each element is lowered
    /// exactly like any other call argument; C default-argument promotion
    /// (`f32` -> `f64`, narrower-than-`int` -> `int`) and untyped-literal
    /// defaulting happen later, in `mir_codegen`'s own trailing-argument
    /// codegen (`codegen_variadic_argument`) — not here, since that's purely
    /// about the LLVM operand's own type, not the MIR shape.
    ///
    /// The resolver's `check_varargs_argument` already guarantees `expr_id`
    /// has exactly this `Array` shape for a `varargs` parameter's argument
    /// (rule 2: always written explicitly as `varargs.[...]`) — the error
    /// path here is defensive, same as elsewhere in this lowerer.
    fn lower_varargs_argument(
        &mut self,
        expr_id: ast::ExpressionId,
        span: Span,
        args: &mut Vec<mir::Operand>,
    ) -> MirResult<()> {
        let expr = &self.store.expressions[expr_id];
        let ast::ExpressionKind::Array(ast::AnyArray::Array(array)) = &expr.node else {
            return Err(Fault::error_with_kind(
                MirErrorKind::ExpectedVarargs {
                    expression_variant: expr.node.variant_name(),
                },
                Some(span),
            ));
        };

        let values = array.values.clone();
        for value in values.iter() {
            args.push(self.lower_move_aware_operand(*value)?);
        }
        Ok(())
    }

    /// Lowers `expr_id` to an `Operand`, choosing `Move` over `Copy` when
    /// it's a place (a variable, or a field/index/deref projection) of a
    /// move-only type (per `is_auto_copy`) — `docs/mir-design.md`'s
    /// move/drop mechanics. Anything that isn't a place at all (a literal, a
    /// nested computation, a struct-constructor literal, ...) has no
    /// existing place to move from, so it falls through to `lower_operand`.
    ///
    /// Shared by every "operand read at a point that can actually move its
    /// source" site: call arguments and the consuming-`this` receiver
    /// (`lower_call`), and — via `lower_movable_rvalue` below — declaration
    /// initializers and plain reassignment. Struct-constructor field values
    /// and array-literal elements (`rvalue.rs`) call this directly too, for
    /// the exact same reason: each is "read an existing place, write it
    /// into a fresh one," identical in shape to a call argument.
    pub(super) fn lower_move_aware_operand(
        &mut self,
        expr_id: ast::ExpressionId,
    ) -> MirResult<mir::Operand> {
        match self.move_eligible_operand(expr_id) {
            Some(result) => result,
            None => self.lower_operand(expr_id),
        }
    }

    /// Lowers `expr_id` the same way `lower_rvalue` would, except a bare
    /// `Variable` right-hand side goes through `move_eligible_operand`
    /// first — the exact same decision `lower_move_aware_operand` makes.
    /// Used by `lower_variable`'s initializer and `lower_assignment`'s
    /// right-hand side: `x := y` and `x = y` are both, structurally, "read
    /// `y`, write into `x`," so both get the same Move-eligibility
    /// treatment. Also used by `control_flow.rs`'s `Return` lowering (both
    /// the explicit `return <expr>` arm and the implicit-tail-return
    /// fallback): `return p` is, structurally, the exact same "read an
    /// existing place, write it into a fresh one" as `x := p` — the return
    /// local is just the destination — so it needs the same move-vs-copy
    /// treatment, or an owning `*T` returned by value would never be marked
    /// moved and would wrongly get dropped (then double-freed) in the
    /// returning scope.
    pub(super) fn lower_movable_rvalue(
        &mut self,
        expr_id: ast::ExpressionId,
    ) -> MirResult<mir::Rvalue> {
        match self.move_eligible_operand(expr_id) {
            Some(result) => Ok(mir::Rvalue::Use(result?)),
            None => self.lower_rvalue(expr_id),
        }
    }

    /// Shared move-vs-copy decision for a place operand — every caller
    /// listed on `lower_move_aware_operand`'s docs needs the exact same
    /// check. Returns `None` when `expr_id` isn't a place at all (a literal,
    /// a nested computation, a struct-constructor literal, ...) — it's on
    /// the caller's own fallback (`lower_operand`/`lower_rvalue`) to lower
    /// those. A bare variable gets `Operand::Copy` when `is_auto_copy` says
    /// so, otherwise `Operand::Move` plus the `MarkMoved`/
    /// `SetDropFlag(local, false)` bookkeeping `docs/mir-design.md`'s
    /// move/drop rules call for. A field/index/deref projection of a
    /// move-only type becomes an `Operand::Move` of that place with no
    /// bookkeeping: drop flags are per-`LocalId`, and a partial move never
    /// decides the whole local's `Drop` (see `move_check`, which also
    /// rejects moves out from behind a reference, pointer, or index).
    fn move_eligible_operand(
        &mut self,
        expr_id: ast::ExpressionId,
    ) -> Option<MirResult<mir::Operand>> {
        let expr = &self.store.expressions[expr_id];
        match &expr.node {
            ast::ExpressionKind::Variable(var) => Some(self.move_variable_operand(var, expr.span)),
            ast::ExpressionKind::FieldAccess(_)
            | ast::ExpressionKind::Index(_)
            | ast::ExpressionKind::Deref(_) => Some(self.move_projected_operand(expr_id)),
            _ => None,
        }
    }

    // A place whose type can't be resolved is treated as move-only: a false
    // move is a spurious diagnostic, a false copy a second owner.
    fn move_projected_operand(&mut self, expr_id: ast::ExpressionId) -> MirResult<mir::Operand> {
        let operand = self.lower_operand(expr_id)?;
        let mir::Operand::Copy(place) = operand else {
            return Ok(operand);
        };

        let auto_copy = self
            .place_type(&place)
            .is_some_and(|ty| self.declares.is_auto_copy(&ty));
        if auto_copy {
            Ok(mir::Operand::Copy(place))
        } else {
            Ok(mir::Operand::Move(place))
        }
    }

    fn move_variable_operand(
        &mut self,
        var: &ast::VariableExpression,
        span: Span,
    ) -> MirResult<mir::Operand> {
        let local = self.resolve_local(var, span)?;
        let ty = self.locals[local].ty;
        let place = mir::Place::local(local);
        if self.is_auto_copy(ty, span)? {
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
        let msg = mir::Operand::Constant(mir::ConstValue::Str("assertion failed".into()));

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

    fn last_argument_span(&self, call: &ast::FunctionCall) -> Option<Span> {
        let last = call.arguments.last()?;
        self.store.expressions.get(last.value).map(|e| e.span)
    }
}
