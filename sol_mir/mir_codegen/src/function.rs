//! Per-function codegen: lowers a single MIR `Function`'s blocks/statements
//! into LLVM IR against an already-declared LLVM function value. Terminator
//! codegen lives in `terminator` and operand/rvalue-to-`BasicValueEnum`
//! codegen lives in `rvalue` instead — this file owns the per-block/
//! per-statement driver and place resolution.

use std::cell::Cell;

use ast_model::{ArrayKind, AstStore, SolType};
use inkwell::{
    AddressSpace,
    basic_block::BasicBlock as LlvmBlock,
    builder::Builder,
    types::BasicTypeEnum,
    values::{FunctionValue, PointerValue},
};
use mir_model::{
    BlockId, ExternFunction, Function, LocalId, Place, PlaceElem, Statement, Terminator,
};
use sol_utils::{
    FunctionId,
    collections::{
        vec_map::{VecMap, VecMapIndex},
        vec_set::VecSet,
    },
    span::ModuleId,
};

use crate::{
    ctx::CodegenCtx,
    err,
    fault::{CodegenErrorKind, CodegenResult},
    llvm_err,
    module::ModuleCodegen,
    types::expect_int,
};

pub(crate) struct FunctionCodegen<'ctx, 'a> {
    pub(crate) ctx: CodegenCtx<'ctx, 'a>,
    /// The Sol module this function was declared in — not to be confused
    /// with `ctx.module`, the LLVM `Module` being emitted into.
    pub(crate) sol_module: Option<ModuleId>,
    pub(crate) builder: Builder<'ctx>,
    pub(crate) function: &'a Function,
    pub(crate) is_entry_point: bool,
    pub(crate) locals: VecMap<LocalId, PointerValue<'ctx>>,
    /// Runtime drop-flag storage — only for a local whose type is `*T`
    /// (`codegen_drop`'s only real, non-no-op case), and only ever *read* by
    /// a `guarded` `Terminator::Drop` (see `move_check::elaborate_drops`'s
    /// docs for when that happens). Absent for every other local: a struct/
    /// array's own `Drop` is a no-op regardless of this flag's value, so
    /// there's nothing worth tracking for it here.
    pub(crate) drop_flags: VecMap<LocalId, PointerValue<'ctx>>,
    pub(crate) blocks: VecMap<BlockId, LlvmBlock<'ctx>>,
    pub(crate) function_values: &'a VecMap<FunctionId, FunctionValue<'ctx>>,
    pub(crate) functions: &'a VecMap<FunctionId, Function>,
    pub(crate) externs: &'a VecMap<FunctionId, ExternFunction>,
    pub(crate) string_counter: &'a Cell<usize>,
}
impl<'ctx, 'a> ModuleCodegen<'ctx, 'a> {
    pub(crate) fn codegen_function(
        &mut self,
        id: FunctionId,
        function: &Function,
    ) -> CodegenResult<()> {
        let name = function_name(self.ast, id)?;
        let module = self.module_of(id);
        let fn_value = *self
            .function_values
            .get(id)
            .ok_or_else(|| err(CodegenErrorKind::MissingAstEntry { id }))?;

        let builder = self.ctx.context.create_builder();

        // A dedicated `entry` block holding only the parameter/local allocas,
        // ahead of the MIR blocks proper — keeps every alloca in the
        // function's first block (what LLVM's mem2reg pass expects) without
        // having to special-case MIR's own entry block for it.
        let entry = self.ctx.context.append_basic_block(fn_value, "entry");
        builder.position_at_end(entry);

        // Only a local that actually has at least one `guarded` `Drop`
        // somewhere in this function ever needs runtime flag storage at all
        // — `elaborate_drops` already proved every other `*T` local's own
        // `Drop`(s) either definitely do or definitely don't run, with no
        // runtime check involved (see that pass's own docs), so allocating a
        // flag for those would just be dead storage nothing ever reads.
        let guarded_locals: VecSet<LocalId> = function
            .blocks
            .entries()
            .filter_map(|(_, block)| match &block.terminator {
                Terminator::Drop {
                    place,
                    guarded: true,
                    ..
                } if place.projection.is_empty() => Some(place.local),
                _ => None,
            })
            .collect();

        let mut locals: VecMap<LocalId, PointerValue<'ctx>> = VecMap::new();
        let mut drop_flags: VecMap<LocalId, PointerValue<'ctx>> = VecMap::new();
        let bool_ty = self.ctx.context.bool_type();
        for (local_id, decl) in function.locals.entries() {
            let resolved_ty = self.ctx.resolve_type(decl.ty);
            let ty = self.ctx.llvm_type(module, resolved_ty, Some(decl.span))?;
            let slot = builder
                .build_alloca(ty, &format!("_{}", local_id.index()))
                .map_err(llvm_err)?;
            locals.insert(local_id, slot);

            // Seeded `true` ("currently owns a live value") right away: a
            // body local's own first `SetDropFlag(local, true)` (emitted
            // alongside its initializing `Assign`) overwrites this before any
            // `Drop` of it can run, but an owning *parameter* never gets an
            // explicit `SetDropFlag` at all (`push_assign` is never called
            // for parameter binding) — this is what makes a guarded `Drop` of
            // an untouched owning parameter still see a correct, defined
            // flag value rather than uninitialized alloca contents.
            if matches!(resolved_ty, SolType::Pointer(_)) && guarded_locals.contains(local_id) {
                let flag_ptr = builder
                    .build_alloca(bool_ty, &format!("_{}_drop_flag", local_id.index()))
                    .map_err(llvm_err)?;
                builder
                    .build_store(flag_ptr, bool_ty.const_int(1, false))
                    .map_err(llvm_err)?;
                drop_flags.insert(local_id, flag_ptr);
            }
        }

        // Params are `locals[0..arg_count]` *by position*: the actual
        // `LocalId`s backing them aren't necessarily `0..arg_count` as
        // values (whatever `IdGenerator` happens to start at), so they're
        // read off `function.locals` itself rather than reconstructed.
        let param_locals: Vec<LocalId> = function
            .locals
            .entries()
            .take(function.arg_count)
            .map(|(id, _)| id)
            .collect();

        for (i, &param_local) in param_locals.iter().enumerate() {
            let param_value = fn_value
                .get_nth_param(i as u32)
                .ok_or_else(|| err(CodegenErrorKind::MissingParameterValue { index: i }))?;

            builder
                .build_store(locals[param_local], param_value)
                .map_err(llvm_err)?;
        }

        let mut blocks: VecMap<BlockId, LlvmBlock<'ctx>> = VecMap::new();
        for (block_id, _) in function.blocks.entries() {
            let llvm_block = self
                .ctx
                .context
                .append_basic_block(fn_value, &format!("bb{}", block_id.index()));
            blocks.insert(block_id, llvm_block);
        }

        // MIR's entry block is always whichever `BlockId` was allocated first
        // in `lower()` (every construct allocates its own block ids only
        // after that), so it's always the lowest-index — and first via
        // `entries()` — block in the function.
        let (mir_entry_id, _) = function
            .blocks
            .entries()
            .next()
            .ok_or_else(|| err(CodegenErrorKind::FunctionHasNoBlocks))?;

        builder
            .build_unconditional_branch(blocks[mir_entry_id])
            .map_err(llvm_err)?;

        let mut fn_codegen = FunctionCodegen {
            ctx: self.ctx,
            sol_module: module,
            function_values: &self.function_values,
            functions: &self.mir.functions,
            externs: &self.mir.externs,
            string_counter: &self.string_counter,
            builder,
            function,
            is_entry_point: name == "main",
            locals,
            drop_flags,
            blocks,
        };

        for (block_id, block) in function.blocks.entries() {
            fn_codegen.codegen_block(block_id, block)?;
        }

        Ok(())
    }
}

impl<'ctx, 'a> FunctionCodegen<'ctx, 'a> {
    pub(crate) fn codegen_block(
        &mut self,
        block_id: BlockId,
        block: &mir_model::BasicBlock,
    ) -> CodegenResult<()> {
        self.builder.position_at_end(self.blocks[block_id]);
        for statement in &block.statements {
            self.codegen_statement(statement)?;
        }
        self.codegen_terminator(&block.terminator)
    }

    fn codegen_statement(&mut self, statement: &Statement) -> CodegenResult<()> {
        match statement {
            Statement::Assign(place, rvalue) => {
                let (ptr, ty) = self.resolve_place(place)?;
                let value = self.codegen_rvalue(rvalue, ty)?;
                self.builder.build_store(ptr, value).map_err(llvm_err)?;
                Ok(())
            }
            Statement::SetDropFlag(local, value) => {
                // Only a `*T` local has flag storage at all (see
                // `drop_flags`'s own docs) — a struct/array local still gets
                // this statement emitted (the lowerer doesn't distinguish by
                // type), but there's no flag to update for it.
                if let Some(&flag_ptr) = self.drop_flags.get(*local) {
                    let bool_ty = self.ctx.context.bool_type();
                    self.builder
                        .build_store(flag_ptr, bool_ty.const_int(u64::from(*value), false))
                        .map_err(llvm_err)?;
                }
                Ok(())
            }
            // Move tracking has no runtime effect (see `docs/mir-design.md`)
            // — nothing to codegen for these.
            Statement::MarkMoved(_) | Statement::StorageDead(_) => Ok(()),
        }
    }

    pub(crate) fn local_type(&self, local: LocalId) -> CodegenResult<BasicTypeEnum<'ctx>> {
        let decl = &self.function.locals[local];
        self.ctx.llvm_type(
            self.sol_module,
            self.ctx.resolve_type(decl.ty),
            Some(decl.span),
        )
    }

    /// Resolves a `Place` to the pointer it reads/writes through and the
    /// LLVM type at that location — the base local's own alloca and type for
    /// an empty projection, then one step per projection element: a `GEP`
    /// through a struct field for `Field(index)`, or a load-then-`GEP`
    /// through a slice's data pointer for `Index(local)` (see
    /// `step_into_index`). Tracks the *Sol* type (not just the LLVM type)
    /// through the walk — unlike a struct's fields (queryable straight off
    /// its LLVM `StructType`), an opaque LLVM pointer carries no pointee-type
    /// info at all, so the element type after an `Index` step has to come
    /// from the Sol-level `ArrayType` instead. `Deref` isn't produced by any
    /// MIR lowering yet, so it still faults here.
    pub(crate) fn resolve_place(
        &self,
        place: &Place,
    ) -> CodegenResult<(PointerValue<'ctx>, BasicTypeEnum<'ctx>)> {
        let mut ptr = self.locals[place.local];
        let mut sol_ty = self
            .ctx
            .resolve_type(self.function.locals[place.local].ty)
            .clone();

        for elem in &place.projection {
            sol_ty = match elem {
                PlaceElem::Field(index) => self.step_into_field(&mut ptr, &sol_ty, *index)?,
                PlaceElem::Index(index_local) => {
                    self.step_into_index(&mut ptr, &sol_ty, *index_local)?
                }
                PlaceElem::Deref => self.step_into_deref(&mut ptr, &sol_ty)?,
            };
        }

        let ty = self.ctx.llvm_type(self.sol_module, &sol_ty, None)?;
        Ok((ptr, ty))
    }

    /// One `Field(index)` step: `sol_ty` must be either a declared struct or
    /// a positional tuple (the `(T, bool)` a `CheckedBinaryOp` assigns into,
    /// or a real struct — this function doesn't care which, same as
    /// `codegen_aggregate`'s destination-type-only dispatch, since both map
    /// to an LLVM `StructType`). GEPs `ptr` to that field's address and
    /// returns the field's own type.
    fn step_into_field(
        &self,
        ptr: &mut PointerValue<'ctx>,
        sol_ty: &SolType,
        index: usize,
    ) -> CodegenResult<SolType> {
        let field_ty = PlaceElem::Field(index)
            .step_type(sol_ty, self.ctx.declares, self.sol_module)
            .ok_or_else(|| err(CodegenErrorKind::PlaceProjectionUnsupported))?;

        let BasicTypeEnum::StructType(struct_llvm_ty) =
            self.ctx.llvm_type(self.sol_module, sol_ty, None)?
        else {
            return Err(err(CodegenErrorKind::PlaceProjectionUnsupported));
        };
        *ptr = self
            .builder
            .build_struct_gep(struct_llvm_ty, *ptr, index as u32, "field_ptr")
            .map_err(llvm_err)?;

        Ok(field_ty)
    }

    /// One `Deref` step: `sol_ty` must be a `&T`/`&mut T`/`*T`. `ptr`
    /// currently holds the *address of the reference/pointer's own storage*
    /// (same invariant every other step maintains) — load the pointer value
    /// out of it to get the address it actually points at, which becomes the
    /// new `ptr` for whatever projection comes next (or the place itself, if
    /// this was the last step).
    fn step_into_deref(
        &self,
        ptr: &mut PointerValue<'ctx>,
        sol_ty: &SolType,
    ) -> CodegenResult<SolType> {
        let inner_ty = sol_ty
            .deref_once(self.ctx.declares)
            .ok_or_else(|| err(CodegenErrorKind::PlaceProjectionUnsupported))?;

        let opaque_ptr_ty = self.ctx.context.ptr_type(AddressSpace::default());
        let pointee = self
            .builder
            .build_load(opaque_ptr_ty, *ptr, "deref")
            .map_err(llvm_err)?;
        *ptr = pointee.into_pointer_value();

        Ok(inner_ty)
    }

    /// One `Index(index_local)` step: `sol_ty` must be a slice
    /// (`[&]T`/`[&mut]T`). Loads the slice's data pointer out of its `ptr`
    /// field (field 0 of the `{ptr, len}` fat pointer built by `array_type`),
    /// loads the runtime index out of `index_local`, then GEPs the data
    /// pointer by that index (element-sized steps, since the GEP is typed as
    /// the element's own LLVM type). Bounds checking against the slice's own
    /// `len` field happens at the MIR level now (`mir_parser`'s
    /// `emit_bounds_check`, an ordinary `Rvalue::Len` + comparison +
    /// `Terminator::Assert`) rather than here — this step trusts the index is
    /// already in range.
    fn step_into_index(
        &self,
        ptr: &mut PointerValue<'ctx>,
        sol_ty: &SolType,
        index_local: LocalId,
    ) -> CodegenResult<SolType> {
        let SolType::Array(array) = sol_ty else {
            return Err(err(CodegenErrorKind::PlaceProjectionUnsupported));
        };
        if !matches!(array.kind, ArrayKind::MutSlice | ArrayKind::ConstSlice) {
            return Err(err(CodegenErrorKind::PlaceProjectionUnsupported));
        }
        let element_ty = PlaceElem::Index(index_local)
            .step_type(sol_ty, self.ctx.declares, self.sol_module)
            .ok_or_else(|| err(CodegenErrorKind::PlaceProjectionUnsupported))?;

        let data_ptr = self.slice_data_ptr(*ptr, sol_ty)?;

        let index_llvm_ty = self.local_type(index_local)?;
        let index_value = self
            .builder
            .build_load(index_llvm_ty, self.locals[index_local], "index")
            .map_err(llvm_err)?;
        let index_value = expect_int(index_value)?;

        let element_llvm_ty = self.ctx.llvm_type(self.sol_module, &element_ty, None)?;
        *ptr = unsafe {
            self.builder
                .build_gep(element_llvm_ty, data_ptr, &[index_value], "elem_ptr")
                .map_err(llvm_err)?
        };

        Ok(element_ty)
    }

    /// Loads a slice place's data pointer out of its `ptr` field (field 0 of
    /// the `{ptr, len}` fat pointer) — the part `step_into_index` and
    /// `codegen_len` (in `rvalue.rs`) both need.
    pub(crate) fn slice_data_ptr(
        &self,
        slice_ptr: PointerValue<'ctx>,
        slice_sol_ty: &SolType,
    ) -> CodegenResult<PointerValue<'ctx>> {
        let BasicTypeEnum::StructType(slice_llvm_ty) =
            self.ctx.llvm_type(self.sol_module, slice_sol_ty, None)?
        else {
            return Err(err(CodegenErrorKind::PlaceProjectionUnsupported));
        };
        let ptr_field_addr = self
            .builder
            .build_struct_gep(slice_llvm_ty, slice_ptr, 0, "slice_ptr_addr")
            .map_err(llvm_err)?;
        let opaque_ptr_ty = self.ctx.context.ptr_type(AddressSpace::default());
        self.builder
            .build_load(opaque_ptr_ty, ptr_field_addr, "slice_ptr")
            .map_err(llvm_err)
            .map(|v| v.into_pointer_value())
    }
}

pub(crate) fn function_name(ast: &AstStore, id: FunctionId) -> CodegenResult<&str> {
    let kind = ast
        .functions
        .get(id)
        .ok_or_else(|| err(CodegenErrorKind::MissingAstEntry { id }))?;

    Ok(kind.signature().name.as_str())
}
