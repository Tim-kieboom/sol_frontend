//! The small, unchanging-per-module context every codegen struct needs: the
//! LLVM context/module, the resolved declarations, and the target
//! platform's int widths. Every field is a reference or a small `Copy`
//! struct, so `CodegenCtx` itself is `Copy` — `ModuleCodegen` and
//! `FunctionCodegen` each just hold their own copy of it instead of one
//! borrowing pieces off the other (which is what `FunctionCodegen` used to
//! do field-by-field, and what motivated giving both structs a matching
//! `llvm_type` wrapper method in the first place).

use ast_model::{SolType, TypeId, declare_store::DeclareStore};
use inkwell::{context::Context, module::Module, types::BasicTypeEnum};
use sol_utils::{
    collections::module_store::ModuleStore,
    compiler_options::CompilerOptions,
    span::{ModuleId, Span},
};

use crate::{fault::CodegenResult, types::llvm_type};

#[derive(Clone, Copy)]
pub(crate) struct CodegenCtx<'ctx, 'a> {
    pub(crate) context: &'ctx Context,
    pub(crate) module: &'a Module<'ctx>,
    pub(crate) declares: &'a DeclareStore,
    /// Resolves a `Span`'s `ModuleId` back to the source file it came from —
    /// needed only to format a panic's `"file:line:col"` location
    /// (`terminator::codegen_assert`), nothing else in codegen touches it.
    pub(crate) modules: &'a ModuleStore,
    pub(crate) options: &'a CompilerOptions,
}

impl<'ctx, 'a> CodegenCtx<'ctx, 'a> {
    /// Resolves a `mir_model::Type` (an interned `TypeId`) back to the
    /// `SolType` it names. Every `TypeId` reaching codegen off a MIR
    /// structure (`LocalDecl.ty`, `ExternFunction`'s param/return types) was
    /// interned by `mir_parser`'s lowering — this can't fail on real input.
    pub(crate) fn resolve_type(&self, ty: TypeId) -> &'a SolType {
        self.declares
            .get_type(ty)
            .expect("mir::Type is always an interned TypeId")
    }

    pub(crate) fn llvm_type(
        &self,
        module: Option<ModuleId>,
        ty: &SolType,
        span: Option<Span>,
    ) -> CodegenResult<BasicTypeEnum<'ctx>> {
        llvm_type(
            self.context,
            &self.options.platform,
            self.declares,
            module,
            ty,
            span,
        )
    }
}
