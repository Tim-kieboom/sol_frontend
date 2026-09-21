//! Terminator codegen: the block-ending constructs (`Call`, `Assert`,
//! `SwitchInt`, `Return`, ...) that `function`'s per-block driver dispatches
//! to — split out since each is its own small, self-contained concern.

use ast_model::SolType;
use inkwell::{
    AddressSpace, IntPredicate,
    module::Linkage,
    types::{BasicMetadataTypeEnum, BasicTypeEnum, IntType},
    values::{BasicValueEnum, FunctionValue, PointerValue},
};
use mir_model::{BlockId, ConstValue, LocalId, Operand, Place, Terminator};
use sol_utils::{FunctionId, compiler_options::PanicMode, sol_names::PrimitiveTypes, span::Span};

use crate::{
    err,
    fault::{CodegenErrorKind, CodegenResult},
    function::FunctionCodegen,
    llvm_err,
    types::{expect_float, expect_int, platform_int_type},
};

impl<'ctx, 'a> FunctionCodegen<'ctx, 'a> {
    pub(crate) fn codegen_terminator(&mut self, terminator: &Terminator) -> CodegenResult<()> {
        match terminator {
            Terminator::Goto(target) => {
                self.builder
                    .build_unconditional_branch(self.blocks[*target])
                    .map_err(llvm_err)?;
            }
            Terminator::SwitchInt {
                discriminant,
                targets,
                otherwise,
            } => {
                self.codegen_switchint(discriminant, targets, otherwise)?;
            }
            Terminator::Call {
                id,
                arguments,
                destination,
                target,
            } => {
                self.codegen_call(*id, arguments, destination, target)?;
            }
            Terminator::Assert {
                cond,
                expected,
                msg,
                target,
                span,
            } => {
                self.codegen_assert(cond, *expected, msg, target, *span)?;
            }
            Terminator::Return => self.codegen_return()?,
            Terminator::Unreachable => {
                self.builder.build_unreachable().map_err(llvm_err)?;
            }
            // No destructor exists for a struct/array yet (M2's borrow
            // checker is what will eventually give those real drop glue) —
            // an owning `*T` is the only type with drop glue so far
            // (`codegen_drop` frees it; every other type is still a pure
            // scope-exit marker with no runtime effect).
            Terminator::Drop { place, target } => {
                self.codegen_drop(place)?;
                self.builder
                    .build_unconditional_branch(self.blocks[*target])
                    .map_err(llvm_err)?;
            }
        }
        Ok(())
    }

    /// `mir_parser`'s `drop_chain` only ever emits `Terminator::Drop` for a
    /// bare local (`mir::Place::local`, no projection) — a moved-out-of
    /// local is excluded before this ever runs (see `FunctionLowerer::
    /// moved`'s own docs), so every `Drop` reaching codegen is for a value
    /// still owned by its local. Only `*T` (`SolType::Pointer`) has real
    /// drop glue right now — `free()`'ing the heap allocation `new(expr)`
    /// made; every other type falls through as a no-op, same as before this
    /// existed.
    fn codegen_drop(&mut self, place: &Place) -> CodegenResult<()> {
        let decl = &self.function.locals[place.local];
        if !matches!(self.ctx.resolve_type(decl.ty), SolType::Pointer(_)) {
            return Ok(());
        }

        let ptr_ty = self.ctx.context.ptr_type(AddressSpace::default());
        let value = self
            .builder
            .build_load(ptr_ty, self.locals[place.local], "drop_ptr")
            .map_err(llvm_err)?;

        let free_fn = self.free_function();
        self.builder
            .build_call(free_fn, &[value.into()], "free_call")
            .map_err(llvm_err)?;
        Ok(())
    }

    /// libc's `free(ptr)`, declared lazily (once per module) — pairs with
    /// `mir_codegen::rvalue`'s `malloc_function`, the allocator every `*T`
    /// (`new(expr)`) is allocated through.
    fn free_function(&self) -> FunctionValue<'ctx> {
        let ptr_ty = self.ctx.context.ptr_type(AddressSpace::default());
        self.declare_void_libc_fn("free", &[ptr_ty.into()])
    }

    fn codegen_return(&mut self) -> CodegenResult<()> {
        if let Some(local) = self.function.return_local {
            let ty = self.local_type(local)?;
            let value = self
                .builder
                .build_load(ty, self.locals[local], "ret")
                .map_err(llvm_err)?;

            if self.is_entry_point {
                self.build_entry_point_return(value)?;
            } else {
                self.builder.build_return(Some(&value)).map_err(llvm_err)?;
            }

            return Ok(());
        }

        if self.is_entry_point {
            let zero = self.ctx.context.i32_type().const_zero();
            self.builder.build_return(Some(&zero)).map_err(llvm_err)?;
            return Ok(());
        }

        self.builder.build_return(None).map_err(llvm_err)?;
        Ok(())
    }

    /// `msg` is the panic message `assert(cond)`/`panic(msg)` lowering
    /// already attaches to this terminator (a `cstr`/`str` operand) — codegen
    /// for both used to discard it and call bare `abort()`; it now flows
    /// through to `panic_function` so a failing assert/panic actually prints
    /// its message before aborting. `span` is `Assert`'s own source location,
    /// turned into a `"file:line:col"` string (`location_string`) and passed
    /// alongside `msg`.
    fn codegen_assert(
        &mut self,
        cond: &Operand,
        expected: bool,
        msg: &Operand,
        target: &BlockId,
        span: Span,
    ) -> CodegenResult<()> {
        let bool_ty = self.ctx.context.bool_type().into();
        let cond = expect_int(self.codegen_operand(cond, bool_ty)?)?;
        let expect_true = self
            .ctx
            .context
            .bool_type()
            .const_int(u64::from(expected), false);

        let ok = self
            .builder
            .build_int_compare(IntPredicate::EQ, cond, expect_true, "assert_ok")
            .map_err(llvm_err)?;

        let ptr_ty = self.ctx.context.ptr_type(AddressSpace::default()).into();
        let msg_ptr = self.codegen_operand(msg, ptr_ty)?.into_pointer_value();
        let location_ptr = self.location_string(span);

        let panic_block = self
            .ctx
            .context
            .insert_basic_block_after(self.builder.get_insert_block().unwrap(), "panic");

        // An unconditional `panic(msg)` lowers to an `Assert` whose `target`
        // is never given a real block (see `lower_panic_intrinsic`'s docs:
        // the "ok" path is provably unreachable, so no block is inserted for
        // it) — branching straight to `panic_block` instead of indexing
        // `self.blocks[*target]` avoids an out-of-bounds panic on that case.
        match self.blocks.get(*target) {
            Some(&ok_block) => {
                self.builder
                    .build_conditional_branch(ok, ok_block, panic_block)
                    .map_err(llvm_err)?;
            }
            None => {
                self.builder
                    .build_unconditional_branch(panic_block)
                    .map_err(llvm_err)?;
            }
        }

        self.builder.position_at_end(panic_block);
        let panic_fn = self.panic_function()?;
        self.builder
            .build_call(
                panic_fn,
                &[msg_ptr.into(), location_ptr.into()],
                "panic_call",
            )
            .map_err(llvm_err)?;

        self.builder.build_unreachable().map_err(llvm_err)?;
        Ok(())
    }

    /// `"{path}:{line}:{col}"` for `span`'s *start* position (a single point,
    /// like Rust's own panic locations — not the `start..end` range `Span`'s
    /// `Debug` impl prints for diagnostics), materialized as its own global
    /// string constant (`codegen_string_constant`, reused from `rvalue.rs`).
    /// Falls back to `"<unknown location>"` if `span.module` isn't in
    /// `self.ctx.modules` — should never happen in practice, but this is a
    /// diagnostics nicety, not worth failing the whole codegen pass over.
    fn location_string(&self, span: Span) -> PointerValue<'ctx> {
        let location = match self.ctx.modules.get_path(span.module) {
            Some(path) => format!(
                "{}:{}:{}",
                path.display(),
                span.start.line,
                span.start.offset
            ),
            None => "<unknown location>".to_string(),
        };
        self.codegen_string_constant(&location)
    }

    fn codegen_call(
        &mut self,
        id: FunctionId,
        arguments: &[Operand],
        destination: &Option<Place>,
        target: &Option<BlockId>,
    ) -> CodegenResult<()> {
        let callee_module = self
            .ctx
            .declares
            .get_function(id)
            .map(|(_, module)| *module);
        let param_types = if let Some(callee_mir) = self.functions.get(id) {
            let callee_param_locals: Vec<LocalId> = callee_mir
                .locals
                .entries()
                .take(callee_mir.arg_count)
                .map(|(id, _)| id)
                .collect();

            callee_param_locals
                .iter()
                .map(|&local| {
                    let decl = &callee_mir.locals[local];
                    self.ctx.llvm_type(
                        callee_module,
                        self.ctx.resolve_type(decl.ty),
                        Some(decl.span),
                    )
                })
                .collect::<CodegenResult<Vec<_>>>()?
        } else if let Some(extern_fn) = self.externs.get(id) {
            extern_fn
                .parameters
                .iter()
                .map(|&ty| {
                    self.ctx
                        .llvm_type(callee_module, self.ctx.resolve_type(ty), None)
                })
                .collect::<CodegenResult<Vec<_>>>()?
        } else {
            return Err(err(CodegenErrorKind::CallHasNoMirBody { id }));
        };

        let callee_value = *self
            .function_values
            .get(id)
            .ok_or_else(|| err(CodegenErrorKind::CallNeverDeclared { id }))?;

        // A genuine C variadic extern (`is_variadic`) is the only callee
        // allowed extra trailing arguments beyond its declared fixed
        // parameters — those trailing operands are `varargs.[...]`'s
        // flattened elements (see `mir_parser`'s call lowering), each
        // appended as its own LLVM call operand per the C variadic ABI
        // (never packed into a collection).
        let is_variadic_extern = self.externs.get(id).is_some_and(|e| e.is_variadic);
        if arguments.len() > param_types.len() && !is_variadic_extern {
            return Err(err(CodegenErrorKind::CallArgumentCountMismatch { id }));
        }

        let mut args = Vec::with_capacity(arguments.len());
        for (arg, param_ty) in arguments.iter().zip(param_types.iter()) {
            args.push(self.codegen_operand(arg, *param_ty)?.into());
        }
        for arg in arguments.iter().skip(param_types.len()) {
            args.push(self.codegen_variadic_argument(arg)?.into());
        }

        let call = self
            .builder
            .build_call(callee_value, &args, "call")
            .map_err(llvm_err)?;

        if let Some(place) = destination {
            if !place.projection.is_empty() {
                return Err(err(CodegenErrorKind::PlaceProjectionUnsupported));
            }

            let result = call
                .try_as_basic_value()
                .left()
                .ok_or_else(|| err(CodegenErrorKind::CallResultIsNone))?;

            self.builder
                .build_store(self.locals[place.local], result)
                .map_err(llvm_err)?;
        }

        match target {
            Some(target) => {
                self.builder
                    .build_unconditional_branch(self.blocks[*target])
                    .map_err(llvm_err)?;
            }
            None => {
                self.builder.build_unreachable().map_err(llvm_err)?;
            }
        }

        Ok(())
    }

    /// A trailing `varargs.[...]` element, appended straight onto the LLVM
    /// `call` as its own operand (never a pointer to a collection — see the
    /// module-level design note in `mir_parser`'s call lowering). Applies C's
    /// default argument promotions, which are never user-visible and never
    /// need an explicit cast at the source level: `f32` -> `f64`, and any
    /// integer type narrower than the C ABI's `int` width -> that `int`
    /// width (sign/zero-extended per the source type's own signedness).
    /// Every other type (already-`int`-or-wider integers, `bool`, `cstr`, an
    /// untyped literal already defaulted to `i32`/`f64` by
    /// `variadic_operand_sol_type`) is emitted as-is.
    fn codegen_variadic_argument(
        &mut self,
        operand: &Operand,
    ) -> CodegenResult<BasicValueEnum<'ctx>> {
        let natural = self.variadic_operand_sol_type(operand);

        if matches!(natural, SolType::Primitive(PrimitiveTypes::Float32)) {
            let f32_ty = self.ctx.context.f32_type().into();
            let value = expect_float(self.codegen_operand(operand, f32_ty)?)?;
            let f64_ty = self.ctx.context.f64_type();
            return Ok(self
                .builder
                .build_float_ext(value, f64_ty, "vararg_f64_promote")
                .map_err(llvm_err)?
                .into());
        }

        let natural_llvm_ty = self.ctx.llvm_type(None, &natural, None)?;
        if let BasicTypeEnum::IntType(int_ty) = natural_llvm_ty {
            let c_int_bits = self.ctx.options.platform.c_int_bits;
            let is_bool = matches!(natural, SolType::Primitive(PrimitiveTypes::Boolean));
            if !is_bool && int_ty.get_bit_width() < c_int_bits {
                let target = platform_int_type(self.ctx.context, c_int_bits);
                return self.codegen_cast(operand, target.into());
            }
        }

        self.codegen_operand(operand, natural_llvm_ty)
    }

    /// The Sol type a `varargs.[...]` element operand is naturally typed
    /// as, before any C default-argument promotion: a place-backed operand
    /// keeps its declared local type; a bare constant defaults per rule 6
    /// (an untyped int literal -> `i32`, an untyped float literal -> `f64`)
    /// since it has no declared/expected type of its own to coerce against
    /// inside `varargs.[...]` (unlike an ordinary typed parameter slot).
    fn variadic_operand_sol_type(&self, operand: &Operand) -> SolType {
        match operand {
            Operand::Copy(place) | Operand::Move(place) if place.projection.is_empty() => self
                .ctx
                .resolve_type(self.function.locals[place.local].ty)
                .clone(),
            Operand::Constant(ConstValue::Uint(_)) => SolType::Primitive(PrimitiveTypes::Uint32),
            Operand::Constant(ConstValue::Float(_)) => SolType::Primitive(PrimitiveTypes::Float64),
            Operand::Constant(ConstValue::Bool(_)) => SolType::Primitive(PrimitiveTypes::Boolean),
            Operand::Constant(ConstValue::Cstr(_)) => SolType::Primitive(PrimitiveTypes::CStr),
            Operand::Constant(ConstValue::Str(_)) => SolType::String,
            // `Int`, `Char`, and any other/place-with-projection shape this
            // first slice doesn't otherwise special-case.
            _ => SolType::Primitive(PrimitiveTypes::Int32),
        }
    }

    fn codegen_switchint(
        &mut self,
        discriminant: &Operand,
        targets: &[(ConstValue, BlockId)],
        otherwise: &BlockId,
    ) -> CodegenResult<()> {
        let bool_ty = self.ctx.context.bool_type().into();
        let cond = expect_int(self.codegen_operand(discriminant, bool_ty)?)?;
        let [(value, target)] = targets else {
            return Err(err(CodegenErrorKind::SwitchIntTargetCountUnsupported));
        };
        let ConstValue::Bool(expect_true) = value else {
            return Err(err(CodegenErrorKind::SwitchIntTargetValueUnsupported));
        };
        let (then_block, else_block) = if *expect_true {
            (self.blocks[*target], self.blocks[*otherwise])
        } else {
            (self.blocks[*otherwise], self.blocks[*target])
        };
        self.builder
            .build_conditional_branch(cond, then_block, else_block)
            .map_err(llvm_err)?;

        Ok(())
    }

    /// `main`'s LLVM-level return is forced to `i32` (see
    /// `declare_all_functions`) since that's what the C ABI/process exit
    /// code convention needs, regardless of Sol's declared return type.
    /// Narrower int types (`bool`, `u8`, ...) are zero-extended; `i32`
    /// itself passes through unchanged (LLVM's `zext` requires the
    /// destination to be strictly wider than the source — a same-width
    /// "extension" is invalid IR, not a no-op); anything wider, or a
    /// pointer, is a real, reported error rather than a silent truncation
    /// or a panic.
    fn build_entry_point_return(&self, value: BasicValueEnum<'ctx>) -> CodegenResult<()> {
        let value = expect_int(value)?;
        let i32_ty = self.ctx.context.i32_type();
        let bits = value.get_type().get_bit_width();
        let value = match bits.cmp(&32) {
            std::cmp::Ordering::Less => self
                .builder
                .build_int_z_extend(value, i32_ty, "exit_code")
                .map_err(llvm_err)?,
            std::cmp::Ordering::Equal => value,
            std::cmp::Ordering::Greater => {
                return Err(err(CodegenErrorKind::EntryPointReturnTypeTooWide));
            }
        };
        self.builder.build_return(Some(&value)).map_err(llvm_err)?;
        Ok(())
    }

    /// Declares a `void`-returning libc function lazily (once per module) —
    /// shared by `abort_function`/`exit_function`, which differ only in name
    /// and parameter list. Neither `abort()` nor `exit()` actually returns,
    /// but declaring them `void` (rather than `noreturn`) is enough: every
    /// call site follows up with its own `build_unreachable`.
    fn declare_void_libc_fn(
        &self,
        name: &str,
        param_types: &[BasicMetadataTypeEnum<'ctx>],
    ) -> FunctionValue<'ctx> {
        if let Some(existing) = self.ctx.module.get_function(name) {
            return existing;
        }
        let fn_type = self.ctx.context.void_type().fn_type(param_types, false);
        self.ctx.module.add_function(name, fn_type, None)
    }

    /// The C runtime's `abort()` — used only by `panic_function` now (every
    /// panicking construct funnels through that instead of calling `abort()`
    /// directly).
    pub(crate) fn abort_function(&self) -> FunctionValue<'ctx> {
        self.declare_void_libc_fn("abort", &[])
    }

    /// The C runtime's `exit(status)` — `cint_type` is passed in rather than
    /// recomputed here, since the caller (`panic_function`) already needs
    /// the same `IntType` to build the exit-code constant.
    pub(crate) fn exit_function(&self, cint_type: IntType<'ctx>) -> FunctionValue<'ctx> {
        self.declare_void_libc_fn("exit", &[cint_type.into()])
    }

    /// libc's variadic `printf`, declared lazily (once per module) — used
    /// only by `panic_function` to print a panic message before aborting.
    fn printf_function(&self) -> FunctionValue<'ctx> {
        if let Some(existing) = self.ctx.module.get_function("printf") {
            return existing;
        }
        let ptr_ty = self.ctx.context.ptr_type(AddressSpace::default());
        let fn_type = self.ctx.context.i32_type().fn_type(&[ptr_ty.into()], true);
        self.ctx.module.add_function("printf", fn_type, None)
    }

    /// libc's `fflush`, declared lazily (once per module) — `panic_function`
    /// calls this with a null `FILE*` (meaning "every open stream") right
    /// before `abort()`, and *only* before `abort()`: without it, `printf`'s
    /// message sits in a fully-buffered `stdout` and is silently lost, since
    /// `abort()` terminates the process immediately without running libc's
    /// normal at-exit flush. `exit()` needs no such help — flushing/closing
    /// every open stream is already part of its own standard behavior.
    fn fflush_function(&self) -> FunctionValue<'ctx> {
        if let Some(existing) = self.ctx.module.get_function("fflush") {
            return existing;
        }
        let ptr_ty = self.ctx.context.ptr_type(AddressSpace::default());
        let fn_type = self.ctx.context.i32_type().fn_type(&[ptr_ty.into()], false);
        self.ctx.module.add_function("fflush", fn_type, None)
    }

    /// `"panic: %s\n  at %s\n"`, null-terminated — the one format string
    /// `panic_function` prints every message+location pair through. A
    /// private global rather than a per-call constant since there's only
    /// ever one of these per module (unlike `codegen_string_constant`'s
    /// per-literal globals).
    fn panic_format_string(&self) -> PointerValue<'ctx> {
        let const_str = self.ctx.context.const_string(b"panic: %s\n  at %s\n", true);
        let global = self
            .ctx
            .module
            .add_global(const_str.get_type(), None, "panic_fmt");
        global.set_initializer(&const_str);
        global.set_constant(true);
        global.set_linkage(Linkage::Private);
        global.as_pointer_value()
    }

    /// The panic runtime: prints `msg` and `location` (both `cstr`-typed
    /// pointers) via `printf`, then terminates the process via `abort()` or
    /// `exit()` (see `CompilerOptions::panic_mode`) — a Rust-`panic!`-style
    /// trap (no unwinding, no backtrace: this compiler has no unwinding
    /// model) instead of a bare, silent `abort()`. Declared *and defined*
    /// lazily (once per module, cached the same way as `abort_function`) the
    /// first time any panicking construct — an out-of-bounds slice index, an
    /// arithmetic overflow, a MIR-level `assert`/`panic()` — is actually
    /// codegen'd; every one of those funnels through this single function.
    /// Building its body reuses `self.builder` (there's no separate builder
    /// per LLVM function), so the caller's own insertion point is saved and
    /// restored around it.
    pub(crate) fn panic_function(&self) -> CodegenResult<FunctionValue<'ctx>> {
        if let Some(existing) = self.ctx.module.get_function("sol_panic") {
            return Ok(existing);
        }

        let ptr_ty = self.ctx.context.ptr_type(AddressSpace::default());
        let fn_type = self
            .ctx
            .context
            .void_type()
            .fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let function = self.ctx.module.add_function("sol_panic", fn_type, None);

        let resume_block = self.builder.get_insert_block();
        let entry = self.ctx.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);

        let format = self.panic_format_string();
        let msg = function
            .get_nth_param(0)
            .ok_or_else(|| err(CodegenErrorKind::MissingParameterValue { index: 0 }))?
            .into_pointer_value();

        let location = function
            .get_nth_param(1)
            .ok_or_else(|| err(CodegenErrorKind::MissingParameterValue { index: 1 }))?
            .into_pointer_value();

        let printf_fn = self.printf_function();
        self.builder
            .build_call(
                printf_fn,
                &[format.into(), msg.into(), location.into()],
                "printf_call",
            )
            .map_err(llvm_err)?;

        match self.ctx.options.panic_mode {
            PanicMode::Abort => {
                // abort() skips libc's normal at-exit cleanup entirely, so
                // the message printf just wrote would otherwise be lost in
                // stdout's buffer — flush it explicitly first.
                let fflush_fn = self.fflush_function();
                let null_stream = ptr_ty.const_null();
                self.builder
                    .build_call(fflush_fn, &[null_stream.into()], "fflush_call")
                    .map_err(llvm_err)?;

                let abort_fn = self.abort_function();
                self.builder
                    .build_call(abort_fn, &[], "abort_call")
                    .map_err(llvm_err)?;
            }
            PanicMode::Exit => {
                // A Windows fastfail-style NTSTATUS code (0xC0000409,
                // STATUS_STACK_BUFFER_OVERRUN) — chosen to match what
                // `abort()` itself already reports as this compiler's exit
                // code on its only current target, so switching PanicMode
                // doesn't change what `// expect: N` assertions observe.
                // Assumes a 32-bit C `int` (true for every target this
                // compiler builds today) — the debug_assert below catches a
                // narrower `c_int_bits` instead of silently truncating.
                const PANIC_CODE: u64 = 3221226505;
                debug_assert_eq!(
                    self.ctx.options.platform.c_int_bits, 32,
                    "PANIC_CODE assumes a 32-bit C `int`"
                );

                // exit() already flushes and closes every open stream as
                // part of its own standard behavior — no separate fflush
                // needed here (unlike the Abort arm above).
                let cint_type =
                    platform_int_type(self.ctx.context, self.ctx.options.platform.c_int_bits);
                let failure_value = cint_type.const_int(PANIC_CODE, false);
                let exit_fn = self.exit_function(cint_type);
                self.builder
                    .build_call(exit_fn, &[failure_value.into()], "exit_call")
                    .map_err(llvm_err)?;
            }
        }

        self.builder.build_unreachable().map_err(llvm_err)?;

        if let Some(block) = resume_block {
            self.builder.position_at_end(block);
        }

        Ok(function)
    }
}
