use ast_model as ast;
use mir_model as mir;
use sol_utils::{fault::Fault, sol_error_internal, span::Span};

use crate::{
    fault::{MirErrorKind, MirResult},
    function::{FunctionLowerer, LoopTargets},
};

impl<'a> FunctionLowerer<'a> {
    /// `is_tail`: whether `expression` is the implicit-return candidate for
    /// the enclosing function — i.e. this is the last statement of a block
    /// currently in tail position (see `lower_body`) *and* it has no
    /// trailing `;` (`!ends_semicolon`, checked by `lower_statement` before
    /// calling this). When true and the expression isn't already handled by
    /// its own arm (`Return`/`If`/`For`/...), it's lowered as if it had been
    /// written `return <expr>` — mirrors the resolver's own
    /// `check_tail_return_type` convention (last statement + `!ends_
    /// semicolon`), which has no MIR-side marker to reuse, so this
    /// re-derives it independently.
    pub(super) fn lower_expression_statement(
        &mut self,
        return_local: Option<mir::LocalId>,
        stmt: &ast::Statement,
        expression: ast::ExpressionId,
        is_tail: bool,
    ) -> MirResult<()> {
        let expr = &self.store.expressions[expression];
        match &expr.node {
            ast::ExpressionKind::Return(Some(value_id)) => {
                // A `return <expr>` in a `none`-returning function would mean
                // the resolver let a value flow into a `none` context, which
                // it type-checks against — trust that and treat this as
                // defensive, not user-reachable.
                let Some(return_local) = return_local else {
                    return Err(Fault::error_with_kind(
                        MirErrorKind::UnexpectedReturnValue,
                        Some(expr.span),
                    ));
                };
                let rvalue = self.lower_movable_rvalue(*value_id)?;
                self.push_assign(mir::Place::local(return_local), rvalue);
                self.seal_return();
                Ok(())
            }
            ast::ExpressionKind::Return(None) => {
                self.seal_return();
                Ok(())
            }
            ast::ExpressionKind::If(if_expr) => {
                self.lower_if(return_local, if_expr, expr.span, is_tail)
            }
            ast::ExpressionKind::For(for_expr) => self.lower_for(return_local, for_expr, expr.span),
            ast::ExpressionKind::Break => self.lower_break(expr.span),
            ast::ExpressionKind::Continue => self.lower_continue(expr.span),
            ast::ExpressionKind::FunctionCall(call) => {
                if self
                    .try_lower_intrinsic_statement(call, expr.span)?
                    .is_some()
                {
                    return Ok(());
                }
                if is_tail && let Some(return_local) = return_local {
                    const WANTS_RESULT: bool = true;
                    let Some(operand) = self.lower_call(call, expr.span, WANTS_RESULT)? else {
                        // Defensive: the resolver only allows a tail call
                        // here when its return type matches the function's
                        // own (non-`none`) return type, so `lower_call`
                        // (given `WANTS_RESULT`) always hands back an
                        // operand in that case — trust that, same category
                        // as elsewhere in this lowerer.
                        let message = "should be unreachable resolver-verified tail call against a non-none return type always produces a result";
                        return Err(sol_error_internal!(message, Some(stmt.span)).into_kind());
                    };
                    self.push_assign(mir::Place::local(return_local), mir::Rvalue::Use(operand));
                    self.seal_return();
                    return Ok(());
                }
                const WANTS_NO_RESULT: bool = false;
                self.lower_call(call, expr.span, WANTS_NO_RESULT)?;
                Ok(())
            }
            _ if is_tail && return_local.is_some() => {
                let return_local = return_local.expect("checked by this arm's own guard");
                let rvalue = self.lower_movable_rvalue(expression)?;
                self.push_assign(mir::Place::local(return_local), rvalue);
                self.seal_return();
                Ok(())
            }
            _ => Err(Fault::error_with_kind(
                MirErrorKind::NonReturnTerminalStatementUnsupported,
                Some(stmt.span),
            )),
        }
    }

    /// `in_tail_position`: whether `statements`' own last entry — if it's a
    /// bare expression with no trailing `;` — is itself the enclosing
    /// function's implicit return value. Only the function's own top-level
    /// body (`FunctionLowerer::lower`) and an *exhaustive* `if`/`elif`/`else`
    /// chain's branches (`lower_if`) ever pass `true`; a `for` body never
    /// does (see `lower_for`) — mirrors the resolver's own tail-position
    /// walk (`check_tail_if`), which recurses through `If`/`Match`/nested
    /// `Block` but never `For`.
    pub(super) fn lower_body(
        &mut self,
        return_local: Option<mir::LocalId>,
        statements: &[ast::StatementId],
        in_tail_position: bool,
    ) -> MirResult<()> {
        for (index, &id) in statements.iter().enumerate() {
            let statement = &self.store.statements[id];
            if self.is_terminated() {
                return Err(Fault::warning_with_kind(
                    MirErrorKind::UnreachableStatement,
                    Some(statement.span),
                ));
            }
            let is_last = index + 1 == statements.len();
            self.lower_statement(return_local, statement, in_tail_position && is_last)?;
        }
        Ok(())
    }

    fn lower_if(
        &mut self,
        return_local: Option<mir::LocalId>,
        if_expr: &ast::If,
        span: Span,
        is_tail: bool,
    ) -> MirResult<()> {
        let ast::IfCondition::Expression(condition_id) = &if_expr.condition else {
            return Err(Fault::error_with_kind(
                MirErrorKind::UnsupportedConditionExpression,
                Some(span),
            ));
        };
        let discriminant = self.lower_bool_condition(*condition_id)?;

        let then_id = self.new_block();
        let join_id = self.new_block();
        let else_id = if_expr.branch.as_ref().map(|_| self.new_block());
        let false_target = else_id.unwrap_or(join_id);

        self.seal(
            mir::Terminator::SwitchInt {
                discriminant,
                targets: vec![(mir::ConstValue::Bool(true), then_id)].into(),
                otherwise: false_target,
            },
            Some(then_id),
        );

        // Only an *exhaustive* `if` (has an `else`) can have its branches be
        // the enclosing function's tail — a bodyless-else `if` in tail
        // position isn't tail-checked by the resolver either (see
        // `non_exhaustive_if_tail_is_skipped`), so it falls through to the
        // ordinary `MissingReturnStatement` error at the end of
        // `FunctionLowerer::lower` exactly as before this feature existed.
        let branch_is_tail = is_tail && if_expr.branch.is_some();

        // Each branch body (the `then` block, and a plain `else { .. }`
        // block) is its own lexical scope: a fresh `scopes` frame is pushed
        // before lowering it, and popped once the branch is done — either
        // via `seal_scope_exit` (fell through: drops that frame's own
        // locals, then `Goto(join_id)`), or a plain pop (errored, or
        // returned early — either way, if it returned, `seal_return`'s own
        // full-stack unwind already dropped this frame's contents on its
        // way out, so there's nothing left to do here but discard the
        // bookkeeping). An `else if` (`IfBranch::If`) is different: it's not
        // a body of statements at this level at all, just a compound
        // statement whose *recursive* `lower_if` call already manages its
        // own frames internally — this level only ever forwards its own
        // join to this `if`'s join, no frame of its own (matches this
        // arm's behavior from before per-branch scope tracking existed).
        let then_statements = self.store.blocks[if_expr.block].statements.clone();
        self.scopes.push(Vec::new());
        let then_result = self.lower_body(return_local, &then_statements, branch_is_tail);
        let then_reaches_join = then_result.is_ok() && !self.is_terminated();
        if !then_reaches_join {
            self.scopes.pop();
        }
        then_result?;
        if then_reaches_join {
            self.seal_scope_exit(join_id);
        }

        let else_reaches_join = match &if_expr.branch {
            None => true,
            Some(ast::IfBranch::Else(block_id)) => {
                self.current = else_id;
                self.scopes.push(Vec::new());
                let else_statements = self.store.blocks[*block_id].statements.clone();
                let else_result = self.lower_body(return_local, &else_statements, branch_is_tail);
                let reaches = else_result.is_ok() && !self.is_terminated();
                if !reaches {
                    self.scopes.pop();
                }
                else_result?;
                if reaches {
                    self.seal_scope_exit(join_id);
                }
                reaches
            }
            Some(ast::IfBranch::If(nested_if)) => {
                self.current = else_id;
                self.lower_if(return_local, nested_if, span, branch_is_tail)?;
                let reaches = !self.is_terminated();
                if reaches {
                    self.seal(mir::Terminator::Goto(join_id), None);
                }
                reaches
            }
        };

        self.current = (then_reaches_join || else_reaches_join).then_some(join_id);
        Ok(())
    }

    fn lower_for(
        &mut self,
        return_local: Option<mir::LocalId>,
        for_expr: &ast::For,
        span: Span,
    ) -> MirResult<()> {
        let ast::ForCondition::While(condition_id) = &for_expr.condition else {
            return Err(Fault::error_with_kind(
                MirErrorKind::UnsupportedLoopCondition,
                Some(span),
            ));
        };

        let header_id = self.new_block();
        let body_id = self.new_block();
        let exit_id = self.new_block();

        self.seal(mir::Terminator::Goto(header_id), Some(header_id));

        let discriminant = self.lower_bool_condition(*condition_id)?;
        self.seal(
            mir::Terminator::SwitchInt {
                discriminant,
                targets: vec![(mir::ConstValue::Bool(true), body_id)].into(),
                otherwise: exit_id,
            },
            Some(body_id),
        );

        // The loop body is its own lexical scope, same treatment as an
        // `if`-branch: a fresh `scopes` frame, popped once the body's own
        // lowering is done — either via `seal_scope_exit` (fell through to
        // the end of the body normally: drops the frame's own locals, then
        // loops back via `Goto(header_id)`, the same per-iteration edge
        // every normal iteration takes) or a plain pop (the body already
        // terminated itself, via an internal `break`/`continue`/`return` —
        // each of those already emitted its own drop chain on the way out,
        // so there's nothing left to do here but discard the bookkeeping).
        let loop_frame_index = self.scopes.len();
        self.scopes.push(Vec::new());
        self.loops.push(LoopTargets {
            header: header_id,
            exit: exit_id,
            loop_frame_index,
        });
        let body_statements = self.store.blocks[for_expr.block].statements.clone();
        // A loop body is never in tail position, regardless of the `for`
        // statement's own position — the resolver's own tail-walk
        // (`check_tail_if`) never recurses into `For` either, since a loop
        // has no well-defined "trailing value" (it may run zero times).
        const IN_TAIL_POSITION: bool = false;
        let body_result = self.lower_body(return_local, &body_statements, IN_TAIL_POSITION);
        self.loops.pop();
        let body_reaches_backedge = body_result.is_ok() && !self.is_terminated();
        if !body_reaches_backedge {
            self.scopes.pop();
        }
        body_result?;
        if body_reaches_backedge {
            self.seal_scope_exit(header_id);
        }

        // The exit block is always reachable via the header's false edge,
        // regardless of how the body ended.
        self.current = Some(exit_id);
        Ok(())
    }

    fn lower_break(&mut self, span: Span) -> MirResult<()> {
        let Some(loop_targets) = self.loops.last() else {
            return Err(Fault::error_with_kind(
                MirErrorKind::BreakOutsideLoop,
                Some(span),
            ));
        };
        let (from_index, target) = (loop_targets.loop_frame_index, loop_targets.exit);
        self.seal_loop_exit(from_index, target);
        Ok(())
    }

    fn lower_continue(&mut self, span: Span) -> MirResult<()> {
        let Some(loop_targets) = self.loops.last() else {
            return Err(Fault::error_with_kind(
                MirErrorKind::ContinueOutsideLoop,
                Some(span),
            ));
        };
        let (from_index, target) = (loop_targets.loop_frame_index, loop_targets.header);
        self.seal_loop_exit(from_index, target);
        Ok(())
    }
}
