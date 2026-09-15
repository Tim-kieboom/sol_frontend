use std::collections::HashSet;

use ast_model::{self as ast, SoulType, TypeId, declare_store::DeclareStore};
use mir_model as mir;
use soul_utils::{
    TypeModifier,
    collections::vec_map::VecMap,
    compiler_options::CompilerOptions,
    fault::Fault,
    ids::IdGenerator,
    span::{ModuleId, Span},
};

use crate::fault::{MirErrorKind, MirResult};
mod control_flow;
mod operators;
mod place;
mod rvalue;
mod statement;
mod r#type;

/// The `(header, exit)` block pair of an enclosing loop, so a nested `break`/
/// `continue` can find its target regardless of how deep inside `if`s it is.
/// Soul has no labeled break/continue, so this is a plain stack: `break`/
/// `continue` always target the innermost entry.
struct LoopTargets {
    header: mir::BlockId,
    exit: mir::BlockId,
    /// `self.scopes.len()` at the moment this loop's own body frame was
    /// pushed — i.e. the index that frame occupies. `break`/`continue`
    /// unwind `scopes[loop_frame_index..]` (the loop's own frame plus
    /// whatever `if`-branch frames are nested inside it at the break/
    /// continue site), never anything below that — locals declared outside
    /// the loop stay alive, `break`/`continue` only exit the loop itself,
    /// not the enclosing function. Recorded per-loop (not just read off
    /// `self.scopes.len()` fresh each time) so a `break`/`continue` inside a
    /// nested loop unwinds to *that* loop's own boundary, not an outer one.
    loop_frame_index: usize,
}

pub struct FunctionLowerer<'a> {
    store: &'a ast::AstStore,
    declares: &'a mut DeclareStore,
    module: Option<ModuleId>,
    local_alloc: IdGenerator<mir::LocalId>,
    node_to_local: VecMap<ast::NodeId, mir::LocalId>,
    locals: VecMap<mir::LocalId, mir::LocalDecl>,

    block_alloc: IdGenerator<mir::BlockId>,
    blocks: VecMap<mir::BlockId, mir::BasicBlock>,

    current: Option<mir::BlockId>,
    statements: Vec<mir::Statement>,
    loops: Vec<LoopTargets>,
    /// A stack of lexical scope frames, each holding the locals bound to an
    /// explicit, user-written `x := ..`/`x: T = ..` declaration inside that
    /// scope (via `lower_variable`), in declaration order — parameters, the
    /// receiver, the return local, and every compiler-internal temp
    /// (checked-overflow tuples, bounds-check bookkeeping, ref/deref temps,
    /// ...) are deliberately excluded everywhere, mirroring
    /// `docs/mir-design.md`'s own worked example (`add(a, b)`'s `Drop(_3)`
    /// covers only `c`, never `_0`/`_1`). Frame `0` is the function's own top
    /// level, always present; `lower_if` pushes one fresh frame per branch
    /// body it lowers (see `seal_scope_exit`) and pops it again once that
    /// branch is done, whether it fell through or returned early — so at any
    /// point during lowering, `scopes` holds exactly the frames for every
    /// scope currently open, innermost last. `lower_for` pushes one frame per
    /// loop body too, popped the same way (see `seal_scope_exit`, used for
    /// a loop body's normal per-iteration fallthrough back to its header,
    /// exactly like an `if`-branch falling through to its join).
    scopes: Vec<Vec<mir::LocalId>>,

    /// Which locals are currently moved-out-of, tracked live *during*
    /// lowering (not a separate post-pass, unlike `move_check`'s own
    /// straight-line-only analysis) so `drop_chain` can exclude an
    /// already-moved local from its own scope's `Drop` — otherwise a moved
    /// `*T` would get `free()`'d a second time once `Terminator::Drop`
    /// actually calls it (see M2's TODO.md entry). Updated at exactly the
    /// two places that already emit `MarkMoved`/`SetDropFlag`:
    /// `move_variable_operand` inserts (a move just happened),
    /// `push_assign` removes on a whole-place write (a reinit — mirrors
    /// `move_check`'s own "reassign clears moved" rule).
    ///
    /// Deliberately *not* saved/restored per `if`/`else` branch or reset
    /// between loop iterations: an outer-scope local moved in only one
    /// branch (or only some loop iterations) stays flagged moved for
    /// whatever lowers next, including the sibling branch and any code
    /// after the join. This can only ever cause a still-live value on some
    /// untaken/not-yet-moved runtime path to be silently skipped by its own
    /// later `Drop` (a leak), never the reverse (a double free) — full
    /// per-path precision needs real dataflow, out of scope here (see M3's
    /// TODO.md entry on extending the borrow checker to generic MIR).
    moved: HashSet<mir::LocalId>,

    options: &'a CompilerOptions,
}
impl<'a> FunctionLowerer<'a> {
    pub(crate) fn new(
        store: &'a ast::AstStore,
        declares: &'a mut DeclareStore,
        options: &'a CompilerOptions,
    ) -> Self {
        Self {
            store,
            options,
            declares,
            module: None,
            current: None,
            loops: vec![],
            statements: vec![],
            scopes: vec![Vec::new()],
            moved: HashSet::new(),
            locals: VecMap::new(),
            blocks: VecMap::new(),
            node_to_local: VecMap::new(),
            local_alloc: IdGenerator::new(),
            block_alloc: IdGenerator::new(),
        }
    }

    pub(crate) fn lower(&mut self, function: &ast::Function) -> MirResult<mir::Function> {
        self.reset();

        let signature = &function.signature.value;
        let fn_span = function.signature.span;
        self.module = self
            .declares
            .get_function(signature.id)
            .map(|(_, module)| *module);

        // `this` — when present — is always argument 0: `lower_call` prepends
        // the receiver operand ahead of the explicit call arguments, so the
        // callee's own locals need it first too (`locals[0..arg_count]` are
        // parameters *by position*, see `mir::Function::arg_count`). Not a
        // real `Parameter` in the AST (`this` is a synthetic scope binding —
        // see `collect_function`), so it's looked up via
        // `get_receiver_binding` instead of `signature.parameters`.
        // `&this`/`&mut this` receive an actual reference — the local itself
        // is `Reference(method_type)`, so `this.field` auto-derefs through it
        // via `auto_deref`, same as any other reference-typed place (see
        // `resolve_field_place`/`resolve_index_place`). `this` (by value)
        // keeps the bare `method_type` local, matching `lower_call`'s
        // corresponding by-value-copy branch.
        let expects_receiver = !matches!(
            signature.function_kind,
            ast::FunctionThisKind::Static
                | ast::FunctionThisKind::Ctor
                | ast::FunctionThisKind::ArrayCtor
        );
        let mut has_receiver_local = false;
        if expects_receiver
            && let Some(this_node) = self.declares.get_receiver_binding(signature.id)
        {
            let span = signature.name.span();
            let method_type = signature.method_type;
            self.require_lowerable(method_type, span)?;
            let receiver_ty = match signature.function_kind {
                ast::FunctionThisKind::MutRef => {
                    self.declares
                        .intern_type(SoulType::Reference(ast::ReferenceType {
                            inner: method_type,
                            lifetime: None,
                            mutable: soul_utils::Mutable::Mut,
                        }))
                }
                ast::FunctionThisKind::ConstRef => {
                    self.declares
                        .intern_type(SoulType::Reference(ast::ReferenceType {
                            inner: method_type,
                            lifetime: None,
                            mutable: soul_utils::Mutable::Immut,
                        }))
                }
                _ => method_type,
            };
            let local = self.alloc_local(receiver_ty, TypeModifier::Mut, span);
            self.node_to_local.insert(this_node, local);
            has_receiver_local = true;
        }

        for parameter in &signature.parameters {
            let span = parameter.name.span();
            let ty = parameter.ty;
            self.require_lowerable(ty, span)?;
            let modifier = parameter.mutable.to_type_modifier();
            let local = self.alloc_local(ty, modifier, span);
            self.node_to_local.insert(parameter.id, local);
        }

        let arg_count = signature.parameters.len() + has_receiver_local as usize;
        // A `none`(void)-returning function has nothing to hold a return value
        // in, so it gets no `return_local` at all — see `Function::return_local`.
        // Checked via the comptime `TypeId::NONE` first, so the common non-none
        // case still resolves the actual `SoulType` exactly once, and the
        // none case skips that lookup entirely.
        let is_none_return = signature.return_type == TypeId::NONE;
        let return_local = if is_none_return {
            None
        } else {
            let return_type = signature.return_type;
            self.require_lowerable(return_type, signature.name.span())?;
            Some(self.alloc_local(return_type, TypeModifier::Mut, signature.name.span()))
        };

        let entry = self.new_block();
        self.current = Some(entry);

        let statement_ids = self.store.blocks[function.block].statements.clone();
        // The function body itself is the root of tail-position recursion —
        // its own last (non-`;`-terminated) statement is an implicit
        // `return`, same as an `=>` single-expression body (which the parser
        // desugars into a one-statement block with no trailing `;`).
        const IN_TAIL_POSITION: bool = true;
        self.lower_body(return_local, &statement_ids, IN_TAIL_POSITION)?;

        if self.current.is_some() {
            if is_none_return {
                // Falling off the end of a `none`-returning function is valid
                // (an implicit `return`) — unlike every other return type,
                // where it's `MissingReturnStatement`.
                self.seal_return();
            } else {
                return Err(Fault::error_with_kind(
                    MirErrorKind::MissingReturnStatement,
                    Some(fn_span),
                ));
            }
        }

        Ok(mir::Function {
            id: signature.id,
            locals: std::mem::take(&mut self.locals),
            blocks: std::mem::take(&mut self.blocks),
            arg_count,
            return_local,
        })
    }

    fn reset(&mut self) {
        self.loops.clear();
        self.current = None;
        self.module = None;
        self.blocks.clear();
        self.locals.clear();
        self.statements.clear();
        self.node_to_local.clear();
        self.local_alloc = IdGenerator::new();
        self.block_alloc = IdGenerator::new();
        self.scopes.clear();
        self.scopes.push(Vec::new());
        self.moved.clear();
    }

    /// Pushes an `Assign`, followed by `SetDropFlag(place.local, true)` when
    /// `place.local` is a tracked `scopes` entry (any currently-open frame,
    /// not just the innermost) — i.e. this write (re)initializes a
    /// user-declared variable, per `docs/mir-design.md`'s move/drop rules
    /// ("every place written via Assign emits `SetDropFlag(place.local,
    /// true)`"). A write into anything else (a parameter, `this`, the return
    /// local, or a compiler-internal temp — none of which are ever added to
    /// `scopes`) gets no drop-flag bookkeeping, mirroring
    /// `docs/mir-design.md`'s own worked example (`_2 = Use(Copy(_3))`'s
    /// write into the *return* local gets no `SetDropFlag` either).
    pub(super) fn push_assign(&mut self, place: mir::Place, rvalue: mir::Rvalue) {
        let local = place.local;
        // A whole-place write reinitializes `local` — same "reassign clears
        // moved" rule `move_check` applies, and the same reason `moved` is
        // only ever cleared here, not on a projected write (`x.field = ..`
        // doesn't make the rest of `x` any less moved than it already was).
        let is_whole_place = place.projection.is_empty();
        self.statements.push(mir::Statement::Assign(place, rvalue));
        if self.scopes.iter().any(|frame| frame.contains(&local)) {
            self.statements
                .push(mir::Statement::SetDropFlag(local, true));
        }
        if is_whole_place {
            self.moved.remove(&local);
        }
    }

    /// Seals the current block on a `return` — reached from any depth of
    /// nested `if`/`else`/`for` scopes, or the true end of a straight-line
    /// function. Unwinds the *entire* `scopes` stack (`unwind_from(0)`):
    /// every tracked local in every currently-open frame gets a `Drop`
    /// terminator, innermost frame first and reverse declaration order
    /// within each frame, chained through fresh blocks ahead of the final
    /// `Return`. A `for` body's own frame is walked exactly like an
    /// `if`-branch's — a `return` diverges straight out of the function, so
    /// there's no per-iteration re-drop/rebuild concern the way `break`/
    /// `continue` have (see `seal_loop_exit`). Deliberately doesn't touch
    /// `self.scopes` itself (no popping) — `scopes` is one stack shared
    /// across the whole function, and a `return` reached from inside one
    /// branch must leave it exactly as-is for whichever sibling branch or
    /// enclosing code lowers next (`lower_if`/`lower_for` are what actually
    /// pop the frame each one pushed, once its own body is fully done — see
    /// `seal_scope_exit`).
    pub(super) fn seal_return(&mut self) {
        let locals = self.unwind_from(0);
        self.drop_chain(locals);
        self.seal(mir::Terminator::Return, None);
    }

    /// Pops the innermost `scopes` frame and seals the current block with a
    /// `Drop` chain over just that frame's own locals (reverse declaration
    /// order), finally sealing with `Terminator::Goto(target)` — the normal,
    /// non-early-exit way an `if`/`else` branch or a `for` body's own scope
    /// ends: falling through to the join block (an `if`/`else`) or looping
    /// back to the header for the next iteration (a `for` body) — same
    /// shape either way, since a loop's own back-edge is exactly "this
    /// scope's locals go out of scope, then control returns to wherever
    /// comes next."
    pub(super) fn seal_scope_exit(&mut self, target: mir::BlockId) {
        let frame = self.scopes.pop().expect(
            "seal_scope_exit is only ever called for a frame lower_if/lower_for itself just pushed",
        );
        self.drop_chain(frame);
        // `next: None`, not `Some(target)` — the caller (`lower_if`/
        // `lower_for`) doesn't necessarily want `target` current yet (e.g.
        // `lower_if` still has the *other* branch left to lower, with its
        // own `self.current` set independently) and sets `self.current`
        // itself, once, after all its own bookkeeping is done. Setting it
        // here too could make a caller's later step silently continue
        // inside `target` instead of its own intended block.
        self.seal(mir::Terminator::Goto(target), None);
    }

    /// Seals the current block on a `break`/`continue`: unwinds
    /// `scopes[from_index..]` (the loop's own frame, plus whatever
    /// `if`-branch frames are nested inside it at the break/continue site —
    /// see `LoopTargets::loop_frame_index`) and seals with
    /// `Terminator::Goto(target)` (the loop's `exit` for `break`, its
    /// `header` for `continue` — the only difference between the two).
    /// Unlike `seal_scope_exit`, never pops `scopes` itself, for the same
    /// reason `seal_return` doesn't: a `break`/`continue` reached from
    /// inside a nested `if`-branch must leave the shared stack intact for
    /// whatever sibling branch lowers next.
    pub(super) fn seal_loop_exit(&mut self, from_index: usize, target: mir::BlockId) {
        let locals = self.unwind_from(from_index);
        self.drop_chain(locals);
        self.seal(mir::Terminator::Goto(target), None);
    }

    /// Every tracked local in `scopes[from_index..]`, flattened in forward
    /// declaration order (frame `from_index`'s locals, then the next
    /// frame's, ...) — `drop_chain` is what turns this into the actual drop
    /// order (innermost frame first, reverse declaration order within each
    /// frame) by reversing the whole thing once: reversing
    /// `[f0_a, f0_b, f1_c, f1_d]` gives `[f1_d, f1_c, f0_b, f0_a]`, exactly
    /// frame `f1` (innermost) before `f0`, each frame's own locals in
    /// reverse-declaration order.
    fn unwind_from(&self, from_index: usize) -> Vec<mir::LocalId> {
        self.scopes[from_index..]
            .iter()
            .flatten()
            .copied()
            .collect()
    }

    /// Chains one `Drop` terminator per local in `locals`, *in reverse*,
    /// each through a fresh block — shared by `seal_return`/`seal_loop_exit`
    /// (an `unwind_from`-flattened multi-frame unwind ending in
    /// `Return`/`Goto`) and `seal_scope_exit` (one frame's own locals, in
    /// declaration order — reversing here gives the same "last-declared-
    /// first" result). Leaves `self.current` set to the last fresh block the
    /// chain opened (or unchanged if `locals` is empty) — it's the caller's
    /// job to `seal` that final block with whatever terminator actually
    /// ends the scope exit.
    fn drop_chain(&mut self, locals: Vec<mir::LocalId>) {
        for local in locals.into_iter().rev() {
            // Already moved out of — nothing left here to free; emitting a
            // `Drop` anyway would double-free an owning `*T` once codegen
            // actually calls `free()` for one (see `moved`'s own docs).
            if self.moved.contains(&local) {
                continue;
            }
            let next = self.new_block();
            self.seal(
                mir::Terminator::Drop {
                    place: mir::Place::local(local),
                    target: next,
                },
                Some(next),
            );
        }
    }

    /// Pretty-prints `ty` for a fault message — `SoulType`'s own `Debug` is
    /// the plain derived one (its internal fields are bare `TypeId`s), so
    /// every fault-message site that used to do `format!("{ty:?}")` goes
    /// through here instead.
    fn print_ty(&self, ty: &SoulType) -> String {
        ast::print_type(ty, self.declares).to_string()
    }

    fn alloc_local(&mut self, ty: mir::Type, mutability: TypeModifier, span: Span) -> mir::LocalId {
        let id = self.local_alloc.alloc();
        self.locals.insert(
            id,
            mir::LocalDecl {
                ty,
                mutability,
                span,
            },
        );
        id
    }

    /// Accepts primitives, structs whose name resolves to a declaration in
    /// this function's module, fixed-size-array/slice-typed arrays
    /// (`[N]T`, `[&]T`, `[&mut]T`), and bare `&T`/`&mut T`/`*T` references —
    /// the boundary this lowering slice actually knows how to turn into MIR
    /// locals/places. A reference is just an opaque pointer-sized value here
    /// (no recursive check on `T`: a place built by dereferencing it is
    /// re-validated on its own terms wherever it's actually used, the same
    /// way a struct field's type already is). Everything else
    /// (wildcard/heap arrays, generics, an undeclared/unresolvable name)
    /// still faults, same as before struct support existed.
    fn require_lowerable(&self, ty: mir::Type, span: Span) -> MirResult<()> {
        let resolved = self
            .declares
            .get_type(ty)
            .expect("mir::Type is always an interned TypeId");
        let is_lowerable_array = matches!(
            resolved,
            SoulType::Array(array) if matches!(
                array.kind,
                ast::ArrayKind::StackArray(_) | ast::ArrayKind::MutSlice | ast::ArrayKind::ConstSlice
            )
        );
        if matches!(
            resolved,
            SoulType::Primitive(_) | SoulType::Reference(_) | SoulType::Pointer(_)
        ) || self.resolve_struct(resolved).is_some()
            || is_lowerable_array
        {
            return Ok(());
        }
        Err(Fault::error_with_kind(
            MirErrorKind::NonPrimitiveType {
                ty: self.print_ty(resolved).into(),
            },
            Some(span),
        ))
    }

    /// Classifies `ty` as `AutoCopy` (`Operand::Copy` is safe — reading it
    /// doesn't invalidate the source) or move-only (`Operand::Move` needed —
    /// see `docs/mir-design.md`'s move/drop section), for the upcoming
    /// `Move`/`MarkMoved` lowering. Only ever needs an opinion on the subset
    /// `require_lowerable` already accepts — calls it first, so both
    /// "can this even become a MIR local" and "is it AutoCopy" are governed
    /// by the exact same accept list instead of two independent matches that
    /// could silently drift apart as `SoulType` grows new variants.
    ///
    /// Primitives and references are `AutoCopy` — a `&T`/`&mut T` reference
    /// never owns what it points at, so copying it is always sound
    /// regardless of what's on the other end. A slice (`[&]T`/`[&mut]T`) is
    /// the same fat-pointer case: non-owning, so `AutoCopy` too, even though
    /// it's a `SoulType::Array`.
    ///
    /// `SoulType::Pointer` (`*T`) is deliberately **not** in that list —
    /// unlike `&T`/`RawPtr<T>`, a `*T` is an *owning* heap pointer (the
    /// `new(expr)` intrinsic's own result type; its `Drop` frees the
    /// allocation), so it's move-only, same as a struct: copying it would
    /// produce two "owners" of the same allocation, both trying to free it.
    ///
    /// A resolved struct (`Stub`) and an *owning* array (`StackArray`/
    /// `HeapArray`) are also move-only — everything else that reaches this
    /// point (only those two `SoulType::Array` kinds and `Pointer` remain
    /// possible once `require_lowerable` has already accepted `ty`) falls
    /// through to that move-only default.
    pub(crate) fn is_auto_copy(&self, ty: mir::Type, span: Span) -> MirResult<bool> {
        self.require_lowerable(ty, span)?;
        let resolved = self
            .declares
            .get_type(ty)
            .expect("mir::Type is always an interned TypeId");
        Ok(match resolved {
            SoulType::Primitive(_) | SoulType::Reference(_) => true,
            SoulType::Array(array) => {
                matches!(
                    array.kind,
                    ast::ArrayKind::MutSlice | ast::ArrayKind::ConstSlice
                )
            }
            _ => false,
        })
    }

    fn new_block(&mut self) -> mir::BlockId {
        self.block_alloc.alloc()
    }

    fn is_terminated(&self) -> bool {
        self.current.is_none()
    }

    fn seal(&mut self, terminator: mir::Terminator, next: Option<mir::BlockId>) {
        if let Some(current) = self.current.take() {
            let statements = std::mem::take(&mut self.statements).into();
            self.blocks.insert(
                current,
                mir::BasicBlock {
                    terminator,
                    statements,
                },
            );
        }
        self.current = next;
    }

    fn resolve_local(&self, var: &ast::VariableExpression, span: Span) -> MirResult<mir::LocalId> {
        let Some(resolved) = self.declares.get_variable_resolve(var.id) else {
            return Err(Fault::error_with_kind(
                MirErrorKind::VariableHasNoResolvedBinding,
                Some(span),
            ));
        };

        let Some(local) = self.node_to_local.get(resolved) else {
            return Err(Fault::error_with_kind(
                MirErrorKind::VariableNotBoundToLocal,
                Some(span),
            ));
        };

        Ok(*local)
    }

    fn lower_bool_condition(&mut self, expr_id: ast::ExpressionId) -> MirResult<mir::Operand> {
        if !self.expression_is_bool(expr_id) {
            let span = self.store.expressions[expr_id].span;
            return Err(Fault::error_with_kind(
                MirErrorKind::UnsupportedConditionExpression,
                Some(span),
            ));
        }
        self.lower_operand(expr_id)
    }
}
