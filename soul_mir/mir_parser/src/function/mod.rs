use ast_model::{self as ast, SoulType, declare_store::DeclareStore};
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
}

pub struct FunctionLowerer<'a> {
    store: &'a ast::AstStore,
    declares: &'a DeclareStore,
    module: Option<ModuleId>,
    local_alloc: IdGenerator<mir::LocalId>,
    node_to_local: VecMap<ast::NodeId, mir::LocalId>,
    locals: VecMap<mir::LocalId, mir::LocalDecl>,

    block_alloc: IdGenerator<mir::BlockId>,
    blocks: VecMap<mir::BlockId, mir::BasicBlock>,

    current: Option<mir::BlockId>,
    statements: Vec<mir::Statement>,
    loops: Vec<LoopTargets>,

    options: &'a CompilerOptions,
}
impl<'a> FunctionLowerer<'a> {
    pub(crate) fn new(
        store: &'a ast::AstStore,
        declares: &'a DeclareStore,
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
        // `get_receiver_binding` instead of `signature.parameters`; mutability
        // (`&this`/`this`/`&mut this`) isn't distinguished here, same as
        // elsewhere in this lowerer (M2 borrow checker's job).
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
            self.require_lowerable(&signature.method_type, span)?;
            let local = self.alloc_local(signature.method_type.clone(), TypeModifier::Mut, span);
            self.node_to_local.insert(this_node, local);
            has_receiver_local = true;
        }

        for parameter in &signature.parameters {
            let span = parameter.name.span();
            self.require_lowerable(&parameter.ty, span)?;
            let modifier = parameter.mutable.to_type_modifier();
            let local = self.alloc_local(parameter.ty.clone(), modifier, span);
            self.node_to_local.insert(parameter.id, local);
        }

        let arg_count = signature.parameters.len() + has_receiver_local as usize;
        // A `none`(void)-returning function has nothing to hold a return value
        // in, so it gets no `return_local` at all — see `Function::return_local`.
        let is_none_return = matches!(signature.return_type, SoulType::None);
        let return_local = if is_none_return {
            None
        } else {
            self.require_lowerable(&signature.return_type, signature.name.span())?;
            Some(self.alloc_local(
                signature.return_type.clone(),
                TypeModifier::Mut,
                signature.name.span(),
            ))
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
                self.seal(mir::Terminator::Return, None);
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
    /// this function's module, and fixed-size-array/slice-typed arrays
    /// (`[N]T`, `[&]T`, `[&mut]T`) — the boundary this lowering slice
    /// actually knows how to turn into MIR locals/places. Everything else
    /// (wildcard/heap arrays, references, generics, an undeclared/
    /// unresolvable name) still faults, same as before struct support
    /// existed.
    fn require_lowerable(&self, ty: &SoulType, span: Span) -> MirResult<()> {
        let is_lowerable_array = matches!(
            ty,
            SoulType::Array(array) if matches!(
                array.kind,
                ast::ArrayKind::StackArray(_) | ast::ArrayKind::MutSlice | ast::ArrayKind::ConstSlice
            )
        );
        if matches!(ty, SoulType::Primitive(_))
            || self.resolve_struct(ty).is_some()
            || is_lowerable_array
        {
            return Ok(());
        }
        Err(Fault::error_with_kind(
            MirErrorKind::NonPrimitiveType {
                ty: format!("{ty:?}").into(),
            },
            Some(span),
        ))
    }

    fn new_block(&mut self) -> mir::BlockId {
        self.block_alloc.alloc()
    }

    fn is_terminated(&self) -> bool {
        self.current.is_none()
    }

    fn seal(&mut self, terminator: mir::Terminator, next: Option<mir::BlockId>) {
        if let Some(current) = self.current.take() {
            let statements = std::mem::take(&mut self.statements);
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
