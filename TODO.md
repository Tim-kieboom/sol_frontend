# Soul Compiler — Roadmap / TODO

Source of truth for design decisions lives in [`docs/`](docs/):
[compiler-pipeline-plan.md](docs/compiler-pipeline-plan.md),
[mir-design.md](docs/mir-design.md),
[crate-system-plan.md](docs/crate-system-plan.md),
[reflection-system.md](docs/reflection-system.md),
[ANCHORED-crate.md](docs/ANCHORED-crate.md).
This file just tracks status and next steps — update it as work lands, don't duplicate design
rationale from those docs here.

## Pipeline

```
lexer → parser → AST → name/typecheck → MIR → LLVM IR (inkwell) → .exe
```

## Milestones

### M1 — first real exe (complete)

Concrete, non-generic subset: ints, arithmetic, `println`, functions, structs, non-generic traits.
No `Res`/`.pass`/`?T`, no unions, no generics, no borrow checking yet.

- [x] Tokenizer, parser, name resolver (pre-existing, ahead of the rest of the pipeline)
- [x] MIR shapes defined (`soul_mir/mir_model`) — see [mir-design.md](docs/mir-design.md)
- [x] MIR lowering: assignments, primitive checks (`mir_run` / lowering, per recent commits)
- [x] MIR lowering: if/for/break/continue
- [x] MIR lowering: comparisons and logical ops
- [x] MIR lowering: none-returning functions and calls
- [x] Bare assert/panic and undefined-fn checks
- [x] MIR display/serialization, fault plumbing
- [x] Struct name resolution: `DeclareStore.struct_names: VecMap<ModuleId, HashMap<SharedStr, NodeId>>`
      (`get_struct_by_name`), populated in `try_insert_struct`, lets a later pass resolve a
      struct-typed `SoulType::Stub`'s bare name back to its `Struct` declaration without re-walking
      scopes — keyed by `ModuleId` first (rather than a flat `HashMap<(SharedStr, ModuleId), _>`) so a
      lookup hashes a bare `&str` instead of allocating an owned `SharedStr`. Turned out narrower than
      first scoped during grill-me ("build a whole declare+resolve system mirroring functions") —
      `soul_name_resolver` already had working struct field-type-checking via
      `lookup_type`/`get_custom_type` (proved by the passing `field_access_type_tests.rs`); the only
      real gap was this one name index, not missing anywhere.
- [x] Struct field reads/writes/construction lowered through the whole pipeline and proven via real
      exes (`09_struct_field_read.soul`, `10_struct_field_write.soul`, per the "prove one vertical
      slice before extending further" decision from grill-me):
  - `mir_parser`: `FunctionLowerer` resolves each function's module once via `declares.get_function`,
    accepts struct-typed params/locals/returns (`require_lowerable`), lowers `Struct{..}` construction
    to `Rvalue::Aggregate(AggregateKind::Struct, ..)` with operands reordered to the struct's
    *declared* field order (not literal order), and lowers `variable.field` reads/writes to a `Place`
    with a `PlaceElem::Field(index)` projection reusing the variable's own storage (no copy) — shared
    via `resolve_field_place`, used from both `lower_operand` (read) and `lower_assignment` (write);
    mutability isn't enforced here (that's the M2 borrow checker's job)
  - `mir_codegen`: `llvm_type` builds an LLVM struct type per resolved `Stub`; `resolve_place` walks
    an arbitrary-length `Field` projection chain via `build_struct_gep` for both loads and stores (no
    codegen changes were needed to support nested chains — it was already general); `Rvalue::Aggregate`
    codegens via `get_undef`+`build_insert_value` per field
  - [x] Nested field chains (`o.inner.x`, both reads and writes): `resolve_field_place` in `mir_parser`
    now recurses when the field-access object is itself a field access, building one `Place` with a
    multi-element `Field` projection rather than a chain of temporaries
  - Not yet supported: struct-typed binary-op operands' signedness (`operand_is_signed` doesn't look
    through a `Field` projection — doesn't matter for a bare field read, would matter for `p.x - 1`
    on a signed field)
- [x] Array literal construction, `&arr`-to-slice, and slice indexing (read+write), plus bounds
      checking on slice indexing — proven via `12_slice_index.soul` and `13_slice_bounds_check.soul`.
      Scoped to fixed-size arrays (`[N]T`, only as the thing you *reference*) and slices
      (`[&]T`/`[&mut]T`, only as the thing you *index*) — not wildcard-sized (`[_]T`) or heap (`[]T`)
      arrays:
  - `mir_parser`: generalized `resolve_field_place` into `resolve_place_expression`, a shared place
    resolver dispatching on variable/field-access/index (so `o.items[i].x` composes into one `Place`
    with a three-element projection, same pattern as nested field chains). `[1, 2]` lowers to
    `Rvalue::Aggregate(AggregateKind::Array, ..)` in literal order (arity trusted from the resolver,
    same as struct constructors). `&arr` on a fixed-size-array place lowers to *two* statements — a
    plain `Rvalue::Ref` into a pointer temp, then `Aggregate(Array, [ptr, compile-time-constant len])`
    — deliberately not a single-step `Rvalue::Ref`, so `Ref` itself stays bare-pointer-only per the
    original grill-me decision. `collection[index]` lowers to a `Place` with a `PlaceElem::Index`
    projection; the index expression is always materialized into a `uint` temp (`operand_local`) to
    avoid width-mismatch risk from re-typing whatever concrete int type it already had.
  - `mir_codegen`: `llvm_type` maps `[N]T` to a real LLVM array type and `[&]T`/`[&mut]T` to a
    `{ptr, len}` struct (`len` at pointer width). `resolve_place`'s walk now tracks the *Soul* type
    (not just the LLVM type) through each projection step — unlike a struct field (queryable straight
    off its LLVM `StructType`), an opaque LLVM pointer carries no pointee-type info at all, so the
    element type after an `Index` step has to come from the `ArrayType` on the Soul side instead.
    `Rvalue::Ref` codegens for the first time (just the address `resolve_place` computes, no load —
    by construction it's never handed an array-typed place, see above). `codegen_aggregate` (renamed
    from the struct-only version) now branches on `StructType` vs `ArrayType` destinations, since a
    fixed-size array's `insertvalue` target isn't struct-shaped.
  - Bounds checking and overflow checking were originally implemented as codegen-level ad hoc
    branch-splitting (see history), then **moved into MIR itself**, mirroring rustc's own `Len`/
    `CheckedBinaryOp`/`Assert` shapes — see the two `[x]` entries directly below. `mir_codegen` no
    longer decides *when* to trap at all; it just implements the (now fully generic) MIR primitives
    that make the trap, and every check is visible in the MIR dump instead of only in the LLVM IR.
- [x] Bounds checking on slice indexing, **at the MIR level** (moved off the original codegen-level
      implementation — see history at the bottom of this file for that version) — proven via
      `13_slice_bounds_check.soul`.
  - `mir_model`: new `Rvalue::Len(Place)` (the slice's own runtime `len`, mirroring rustc's `Len`).
  - `mir_parser`: `resolve_index_place`'s `emit_bounds_check` now emits, ahead of the actual
    `PlaceElem::Index` projection: `len_local = Len(collection)`; casts the index to `uint` first if
    it isn't already one (`operand_local` reuses a bare-`Variable` index's own declared type as-is,
    so it isn't always pre-normalized — see the new `Rvalue::Cast` below); `cond = index < len`; then
    seals the block with `Terminator::Assert { cond, expected: true, msg: "index out of bounds",
    target: next }`. Bounds checking is now indistinguishable, MIR-shape-wise, from a hand-written
    `assert(i < s.len())` — `mir_codegen` doesn't know or care that it came from indexing.
  - `mir_codegen`: `Rvalue::Len` loads field `1` of the slice's `{ptr, len}` fat pointer
    (`rvalue::codegen_len`). `step_into_index` no longer does any bounds checking at all — it trusts
    the index is in range, exactly as if the MIR simply never proved otherwise.
  - Not yet supported: indexing a raw `[N]T` directly (only a slice can be indexed — reference it
    first), `&`/`@` mutability not checked against `[&]`/`[&mut]` (that's the M2 borrow checker's job,
    same as struct field mutability)
- [x] Overflow checking on arithmetic ops (`+`/`-`/`*`), **at the MIR level** (moved off the original
      codegen-level implementation) — proven via `14_arith_overflow_check.soul` (`i32::MAX + 1`
      aborts instead of wrapping).
  - `mir_model`: new `Rvalue::CheckedBinaryOp(op, left, right)`, producing a `(T, bool)` tuple
    (result, overflowed) — mirrors rustc's own `CheckedBinaryOp` shape exactly. Reuses
    `AggregateKind`'s pre-existing (until now unused) `Tuple` variant's *type* side —
    `SoulType::TupleKind(TupleKind::Tuple(..))` — not a new `Rvalue` aggregate-construction case
    (the tuple value itself is never built via `Aggregate`; the intrinsic call's own return value
    *is* the tuple, see below).
  - `mir_parser`: `is_checked_arith_op` routes `Add`/`Sub`/`Mul` (only) through
    `lower_checked_binary_op` instead of a plain `Rvalue::BinaryOp` — assigns `CheckedBinaryOp` into
    a fresh tuple-typed temp, seals the block with `Terminator::Assert { cond: tuple.1, expected:
    false, msg: "attempt to {add,subtract,multiply} with overflow", target: next }`, then returns
    `Rvalue::Use(Copy(tuple.0))` as if this had been an ordinary `BinaryOp` all along — every
    existing call site (`lower_operand`'s nested-expression materialization, a top-level
    `lower_assignment`/`lower_variable`/`return`) needed no change at all. The tuple's element type
    is derived from whichever *operand* actually carries a place-backed type (a new `operand_type`/
    `place_type` pair, mirroring `mir_codegen::rvalue`'s own `operand_type` one layer up) rather than
    the resolver's per-expression type table — needed because the resolver never types a
    `FieldAccess`/`Index` *expression* (`s[0] + s[1]`'s own type would otherwise be unresolvable),
    falling back to the resolver's whole-expression type only when both operands are bare constants
    (`3 + 4`, neither a place).
  - `mir_codegen`: `Rvalue::CheckedBinaryOp` calls the matching LLVM `{s,u}{add,sub,mul}.with.overflow`
    intrinsic and returns its `{result, i1 overflowed}` struct *directly* as the tuple value — LLVM
    uniques anonymous struct types structurally, so the intrinsic's own return type and the
    synthesized tuple `StructType` are the same type, no repacking needed. Deciding whether to trap
    isn't `mir_codegen`'s job any more; it only computes the pair.
  - New `Rvalue::Cast(Operand, Type)` codegen (previously declared in `mir_model` but never
    implemented) — sign-extends/zero-extends/truncates an int operand to the destination width,
    based on the *source*'s signedness. Currently only reachable from the bounds-check index
    normalization above; general implicit/explicit int-to-int casts elsewhere in the language aren't
    wired to it yet.
  - `step_into_field` (struct-field `Place` resolution) now also accepts a positional-tuple
    `SoulType::TupleKind(TupleKind::Tuple(..))` destination alongside a declared struct — needed so
    `PlaceElem::Field(0)`/`Field(1)` can address a `CheckedBinaryOp` tuple's result/overflow halves,
    not just a real struct's fields; `llvm_type` gained a matching `TupleKind::Tuple` → anonymous
    LLVM `StructType` mapping.
  - Div/Mod were untouched by this pass — division overflow (`INT_MIN / -1`) and div-by-zero are
    handled separately below.
- [x] Overflow checking on `/`/`%` (division by zero, and — signed only — `MIN / -1`/`MIN % -1`) —
      proven via `17_div_by_zero_check.soul`, `18_mod_by_zero_check.soul`, `19_div_overflow_check.soul`,
      `21_mod_overflow_check.soul` (all abort with the matching message+location), plus
      `20_div_mod_normal.soul` as a regression guard that ordinary division/remainder still compute
      the right answer with checking on.
  - Unlike `+`/`-`/`*`, there's no `{s,u}div/rem.with.overflow` LLVM intrinsic to call, so this
    doesn't produce a `(T, bool)` tuple the way `CheckedBinaryOp` does — `mir_parser`'s new
    `lower_checked_div` instead emits explicit MIR-level `Assert`s ahead of an ordinary, now-safe
    `Rvalue::BinaryOp(Div/Mod, ..)`, mirroring rustc's own checked-division lowering (which does the
    same thing for the same reason): `assert(divisor != 0, "attempt to {divide,calculate the
    remainder with a divisor of} ... zero")`, then — only when the operand type is signed —
    `assert(!(dividend == MIN && divisor == -1), "attempt to {divide,calculate the remainder} with
    overflow")`. `mir_codegen` needed zero changes: by the time `Div`/`Mod` reach
    `codegen_binary_op`, the checks already ran, so the existing `build_int_signed_div`/`_rem`/
    `_unsigned_div`/`_rem` arms are unchanged.
  - Computing the actual `MIN` constant needed the operand's concrete bit width, which — for
    `int`/`cint` — is platform-sized (`PlatformInfo.pointer_bits`/`c_int_bits`) and only known once
    `mir_parser` is handed a `&CompilerOptions` (already true, from the `MirOptions` toggle work).
    New `signed_primitive_min(prim, platform)` is the one place in `mir_parser` that reads
    `PlatformInfo` for this reason, despite that struct's own doc comment saying nothing upstream of
    codegen normally needs to — there's no way to express "the minimum value of whatever width this
    turns out to be" as a single width-agnostic MIR constant the way `0`/`-1` already are.
  - `mir_model::Operand` gained `Clone` (needed to reuse the same dividend/divisor operand across
    both the division itself and its guard-condition comparisons).
  - Gated behind the same `MirOptions::CHECK_ALGORITHMIC_OVERFLOW` flag as `+`/`-`/`*` rather than a
    separate flag — division-by-zero and overflow are bucketed with the rest of "checked arithmetic"
    here, not split out on their own.
- [x] Rust-`panic!`-style panic runtime (message, no backtrace, no unwinding) — every panicking
      construct is now an ordinary MIR `Terminator::Assert` (bounds check, overflow check,
      `assert(cond)`/`panic(msg)` — see the bounds-checking/overflow-checking entries above for how
      the first two now get there), and `codegen_assert` is the *only* place `mir_codegen` ever calls
      the panic runtime from. Proven via `15_assert_panic_message.soul` and
      `16_panic_intrinsic_message.soul` (stdout matched against `panic: <message>`), plus
      `expect_stdout` added to `13_slice_bounds_check.soul`/`14_arith_overflow_check.soul`.
  - `mir_codegen/src/terminator.rs`: `panic_function` lazily declares *and defines* (once per module)
      a `soul_panic(msg: cstr)` function — `printf("panic: %s\n", msg)`, `fflush(NULL)`, `abort()`,
      `unreachable` — building its body with the same per-function `self.builder` used for the
      function currently being codegen'd (saves/restores the builder's insertion point around it,
      since there's no separate builder per LLVM function). `codegen_assert` passes `Assert`'s own
      `msg` operand straight through to it (already codegen'able via the existing `cstr` operand
      path) — no bespoke codegen-level "materialize a message, split a block, trap" helper exists any
      more (an earlier version of this pass had `trap_if`/`trap_with_message` for that; both were
      deleted once bounds/overflow checking moved into MIR, since every trap now just *is* an
      `Assert`).
  - Real bug found and fixed along the way: `printf`'s output sat in a fully-buffered `stdout` and was
      silently lost, since `abort()` terminates the process immediately without libc's normal at-exit
      flush — caught by actually running a built exe and checking its stdout, not just its exit code
      (an `expect_stdout` match would've silently short-circuited to "test never printed anything" had
      it not been checked by hand first). Fixed with an `fflush(NULL)` call between `printf` and
      `abort()`.
  - Also found and fixed: `codegen_assert` indexed `self.blocks[*target]` unconditionally, but an
      unconditional `panic(msg)` lowers to an `Assert` whose `target` is never given a real block
      (`lower_panic_intrinsic`'s own docs say so — the "ok" path is provably unreachable) — this
      panicked on the very first exe test that actually exercised `panic(msg)` end-to-end. Fixed by
      branching straight to the panic block when `self.blocks.get(*target)` is `None`, instead of
      indexing.
  - [x] Panic location (`file:line:col`, à la `thread 'main' panicked at src/main.rs:4:5`) — proven
      via `expect_stdout` assertions on all four panic-message exe tests (`13`-`16`) matching
      `"<file>.soul:<line>:"`. Still no backtrace (explicitly out of scope, per the user's own
      framing).
    - `mir_model::Terminator::Assert` gained a `span: Span` field — the only MIR shape change needed;
      `mir_parser` already had the span in hand at every `Assert`-emission site
      (`emit_bounds_check`, `lower_checked_binary_op`, `lower_assert_intrinsic`,
      `lower_panic_intrinsic`) and just had to stop discarding it.
    - `mir_codegen` needed a way to turn a `Span`'s `ModuleId` into an actual file path, which nothing
      in `mir_codegen`/`mir_parser` carries today (only the top-level driver's own
      `soul_utils::collections::module_store::ModuleStore` does — the same one
      `soul_tester::display::fault` already uses for its own compile-time diagnostics). Threaded a
      `&ModuleStore` through `to_llvm` → `codegen_module` → `CodegenCtx` (a new field alongside
      `declares`/`platform`) so `terminator::codegen_assert`'s new `location_string` helper can
      resolve it and format `"{path}:{line}:{col}"` (the `Span`'s *start* position only — a single
      point, like Rust's own panic locations, not the `start..end` range `Span`'s `Debug` impl prints
      for compile-time diagnostics) as its own global string constant, reusing
      `codegen_string_constant` (back to `pub(crate)`, shared with `rvalue.rs`).
    - `soul_panic`'s signature widened to `(msg: cstr, location: cstr)`, printing
      `"panic: {msg}\n  at {location}\n"`.
    - `scripts/run_codegen_tests.py`'s `// expect_stdout:` was single-shot (first match only, later
      ones silently ignored) — generalized to collect every `expect_stdout` line in a file so a test
      can assert both the message and the location independently.
- [x] Non-generic trait support (static dispatch only, no `Trait<T>`) — proven via
      `26_trait_impl_dispatch.soul` (a real `impl Greeter for Bar` method called through a `Bar`
      value, returning the receiver's own field).
  - `soul_name_resolver`: entirely resolver-level, as scoped — dispatch itself needed no new
    mechanism, since `impl Trait { .. }` methods already got `method_type` set to the concrete
    implementing struct (same as a plain inherent `use Bar { .. }` method) and resolved through the
    existing name+owner-type lookup; traits only add conformance checking on top of that.
  - New `resolve_use_block`-driven `check_impl_conformance` (resolve phase, hard error): an
    `impl Trait for X` must supply **exactly** `Trait`'s declared method set — no fewer (`Impl
    MissingTraitMethod`), no extra (`ImplHasExtraTraitMethod`) — each with a matching signature
    compared **positionally by type only** (parameter types + return type; names don't have to
    match). Safe to run as a single resolve-phase pass with no extra ordering mechanism, because
    collect always fully finishes (recursively, across all imports) before any resolve/typecheck
    runs — a trait's full method list is already known by the time any of its impls are checked.
  - The signature comparison deliberately **excludes the receiver**: a trait method's `method_type`
    is parsed (`from_keyword.rs`) as a `Stub` carrying the *trait's own name* (there's no real
    `Self`/`This` placeholder yet — see the `This` type entry below), while the impl method's
    `method_type` is the concrete implementing type — the two are never expected to be equal, by
    construction, not by bug.
  - `find_function`/`find_function_with_module` in `DeclareStore` collapsed into one
    ambiguity-aware `find_function` returning a new `FunctionLookup { Found, NotFound, Ambiguous }`
    — the old "return the first name+owner-type match" silently picked a winner when two different
    trait impls on the same type both defined a same-named method; now that's a hard
    `AmbiguousMethodCall` resolve error instead (deliberately *not* disambiguated via a Rust-`<X as
    Trait>::method()`-style qualified-call syntax — that's fast-follow work, see below). Had to
    dedupe by `FunctionId` while doing this: each function is registered into `DeclareStore` twice
    (once from `collect_function`, once from `resolve_function`), so a same-`FunctionId` match
    seen twice is not itself a collision — only a match against a genuinely different `FunctionId`
    is.
  - `mir_parser`/`mir_codegen`: no trait-specific changes at all, exactly as scoped — but proving
    the feature via a *real running exe* (not just a resolver-level test) surfaced a separate,
    pre-existing gap this pass had to fix anyway: **no receiver method call
    (`receiver.method(args)`) had ever lowered to MIR**, trait or not — `lower_call` unconditionally
    rejected any call with a callee expression. Fixed generally (not trait-specific):
    - `lower_call` (`function/statement.rs`) now accepts a `FunctionCalleeKind::Expression` callee,
      lowers it as the receiver operand, and prepends it ahead of the explicit call arguments —
      gated on the *callee function's own* `function_kind` (`!Static && !Ctor && !ArrayCtor`), not
      on the resolver's `ignore_callee` flag (which only reflects whether the callee looked like a
      type-qualifier at the call site, not whether the resolved target actually expects a
      receiver). A `FunctionCalleeKind::Type` callee (`Type::method()`-style explicit qualification)
      is still rejected — out of scope here, unrelated to traits.
    - The callee side needed a `this` local to exist at all: `this` was never a real AST
      `Parameter` (`collect_function` binds it as a synthetic scope-only `NodeId` via
      `node_generator.alloc()`), so `mir_parser` had no way to find it. New
      `DeclareStore::{insert,get}_receiver_binding(FunctionId, NodeId)` exposes that same synthetic
      id; `FunctionLowerer::lower` (`function/mod.rs`) allocates a `this` local from it — always
      **argument 0** (`locals[0..arg_count]` are parameters *by position*, so the callee's `this`
      local has to line up with the receiver operand `lower_call` prepends at the call site) —
      whenever `function_kind` isn't `Static`/`Ctor`/`ArrayCtor`.
    - Mutability (`&this`/`this`/`&mut this`) is **not** distinguished — every receiver is passed
      as a plain by-value copy of the struct, same "not enforced yet, M2 borrow checker's job"
      simplification already accepted elsewhere in this lowerer (struct field mutability through a
      reference, etc.). No exe test exercises mutation through `&mut this`, so this isn't yet a
      proven gap, just a known one.
  - Deliberately deferred to a fast-follow (each cuts real scope, not laziness):
    - Default trait method bodies (`trait Foo { fn bar() { .. } }`) — the AST already models a
      bodied vs. signature-only function (`FunctionKind::Normal`/`Signature`) and reuses it for
      trait methods, but the trait-body parser (`from_keyword.rs`) hardcodes
      `FunctionKind::Signature` and never checks for a following `{ }` block.
    - A `This` type (Soul's `Self`-equivalent, for a trait method parameter that needs to refer to
      "whatever type eventually implements this trait", e.g. `fn eq(&this, other: This): bool`) —
      doesn't exist anywhere yet (only unrelated `This.(..)`/`This.[T](..)` constructor-call syntax
      does); would need new tokenizer/parser/AST/resolver work.
    - `<X as Trait>::method(x)`-style qualified-call syntax to disambiguate the
      same-named-method-across-traits case that's currently just a hard `AmbiguousMethodCall`
      error.
  - `Trait<T>` / generic trait bounds are explicitly out of scope for M1 entirely — this pass is
    static dispatch only, no generics; see M3's "Full trait resolution" and "Extend borrow checker
    to generic MIR with `AutoCopy` bounds" entries below for where that lands.
- [x] Implicit ("trailing") return of a function body's tail expression — both an `=>`
      single-expression body and a `{ }` block whose last statement is a bare expression with no
      `return` and no trailing `;` — proven via `27_trailing_return.soul` (an `=>` body, a
      block-tail, and an exhaustive `if`-tail all feeding into one result).
  - Entirely a `mir_parser` gap: `soul_name_resolver` already type-checked this (`check_tail_
    return_type`, shared with lambda return-type inference) with the convention "a block's last
    statement, if it's an `Expression` with `ends_semicolon == false`, is the tail" — but it's a
    pure diagnostic pass with no marker left behind for MIR to consult, so `mir_parser` has to
    re-derive the same convention independently.
  - New `in_tail_position: bool` parameter on `lower_body`, computed per-statement into an `is_tail`
    flag passed to `lower_statement`/`lower_expression_statement` (`is_last_statement &&
    !ends_semicolon`). Only two places ever pass `true`: `FunctionLowerer::lower` for the whole
    function body (the root of the recursion — this is also what makes an `=>` body work for free,
    since the parser already desugars it into a one-statement block), and `lower_if`'s `then`/`else`
    branches — and only when the `if` is *exhaustive* (has an `else`); a bodyless-else `if` gets
    `false` on both branches, falling through to the pre-existing `MissingReturnStatement` error
    exactly as before (mirrors the resolver's own `non_exhaustive_if_tail_is_skipped`). `lower_for`
    always passes `false` for its loop body, regardless of the `for`'s own position — a loop has no
    well-defined trailing value (mirrors the resolver's tail-walk, which recurses through
    `If`/`Match`/nested `Block` but never `For`).
  - Explicitly out of scope, matching the resolver's own recursion: a nested bare `{ }` block used
    as an expression, and `match` (already unimplemented in `mir_parser` until M3).
  - Found and deliberately **not** fixed while proving this (out of scope, pre-existing, unrelated to
    tail-return): a `BinaryOp` whose *both* operands are bare literal constants (e.g. a standalone
    `0 - 4`, no variable/place operand on either side) mistypes as unsigned 64-bit regardless of
    context — even `y: i32 = 0 - 4` triggers it, silently trapping via the overflow-check machinery
    instead of computing `-4`. `27_trailing_return.soul`'s `neg`/`abs` helpers route every literal
    subtraction through a real parameter (`0 - n`) to sidestep it. Needs its own fix: `mir_parser`'s
    `lower_checked_binary_op`/`lower_checked_div`'s "both operands are bare constants, fall back to
    the resolver's whole-expression type" path resolves to the wrong type.
- [x] `soul_mir/mir_codegen` — LLVM IR emission via `inkwell` (`features = ["llvm16-0"]`, Windows
      only) implemented for scalar/pointer/struct locals, arithmetic/comparison/logical ops, if/while,
      function calls, `extern "C"` functions (incl. `cstr`/pointer params and correct C-vs-Soul
      integer widths via `PlatformInfo`, see below), string/cstr constants, and process-exit-code
      `main` codegen. Structured fault system (`CodegenErrorKind`) replaces `anyhow`.
  - [x] `f32`/`f64` arithmetic (`+`/`-`/`*`/`/`/`%`) and comparisons (`==`/`!=`/`<`/`>`/`<=`/`>=`) —
        proven via `22_float_arithmetic.soul`, `23_float_ops.soul` (all six ops + all six
        comparisons on `f64`), `24_f32_arithmetic.soul`. `f16` deliberately still unsupported (falls
        through to `UnsupportedPrimitiveType` in `llvm_type` — no direct C ABI use for it yet).
    - `ast_model`/tokenizer/`soul_name_resolver` already had everything needed (`PrimitiveTypes::
      Float16/32/64/UntypedFloat`, `Literal::Float(f64)`, float-literal lexing, and the resolver's
      numeric-promotion tables) — this was purely a `mir_codegen`/`mir_parser` gap, not a frontend one.
    - `mir_codegen::types`: `llvm_type` maps `Float32`/`Float64` (and `UntypedFloat`, defaulting to
      `f64` the same way an untyped int literal defaults to `int`) to the matching LLVM `FloatType`.
      New `const_float`/`expect_float` mirror `const_int`/`expect_int`.
    - `mir_codegen::rvalue`: `codegen_binary` branches to a new `codegen_float_binary_op` when the
      resolved operand type is a `FloatType` — no signed/unsigned split (floats have none), using
      `build_float_add/sub/mul/div/rem` and `build_float_compare` with the *ordered* (`O*`)
      `FloatPredicate`s (`NaN` compares false against everything per IEEE 754, matching how every
      other language defines float `==`/`<`/etc.). `codegen_constant` gained a `FloatType` arm.
    - `mir_parser`: floats are explicitly kept **out** of the checked-arithmetic/checked-div lowering
      (`is_float_operand`, checked before routing into `lower_checked_binary_op`/`lower_checked_div`)
      — IEEE 754 overflow saturates to `inf`/`-inf` rather than being UB the way integer overflow and
      `INT_MIN / -1` are, and there's no `{s,u}*.with.overflow`-style intrinsic for floats anyway, so
      a float `+`/`-`/`*`/`/`/`%` always lowers to a plain, unchecked `Rvalue::BinaryOp`.
    - Not yet supported: `f16`, int↔float casts (`Rvalue::Cast` codegen still assumes an `IntType`
      destination), float constants/locals inside structs or arrays (structurally should already work
      once `llvm_type` recurses into a float field/element, but untested).
- [x] Wire codegen output through to an actual `.exe` — `scripts/run_codegen_tests.py` drives
      `clang.exe` (`C:\llvm-16\bin\clang.exe`) over the emitted `.ll`, then runs the resulting exe
      and checks both exit code (`// expect: N`) and stdout (`// expect_stdout: <substring>`); this
      is currently the real correctness oracle for the pipeline (`soul_tester/soul/src/codegen_tests/`,
      28 passing exe tests). Still manual/script-driven, not integrated into `cargo test`.
- [x] Audited `soul_name_resolver`'s test coverage of the resolver/typecheck-flavored
      `AstErrorKind` variants (the "Name resolution" section of `ast_model/src/fault.rs`, ~34
      variants — parser-only syntax-error variants were out of scope, and so was `mir_parser`,
      already the gold standard this was measured against: every fault path there already gets a
      dedicated `assert_rejected_with(..., SpecificErrorKind)` test). "Covered" meant a test
      asserting the *specific* variant, not just "some fault happened" — grepped each variant name
      across `soul_name_resolver`'s test files, then manually cross-checked the misses.
  - 7 of ~34 variants had zero test hits. Of those:
    - `ParentChildFunctionSameName`, `FunctionNameTripleUnderscore`, `FunctionNotFoundIn` were
      real, reachable gaps — fixed with new tests (`collect/function_and_variable_name_tests.rs`,
      plus two cases added to `collect/import_tests.rs` for the module-qualified-call case).
    - `FunctionNameEmpty`, `FunctionNameInvalidStart`, `VariableNameEmpty`,
      `VariableNameInvalidStart` were **dead code**, not a test gap: `soul_tokenizer`'s
      `lex_ident` (`soul_tokenizer/src/lexer.rs`) is only ever entered on a char already
      satisfying `is_alphabetic() || '_'`, so no `TokenKind::Ident` this compiler produces can be
      empty or start wrong — and every `Ident` reaching these checks (including the two hardcoded
      synthesized constructor names, `This__ctor`/`This__arrayCtor`) traces back to a real
      tokenizer token or one of those two constants. Removed: the 4 `AstErrorKind` variants, the
      checks that constructed them (`check_variable_name` deleted entirely — it had no other
      purpose left; `check_function_name` now only checks `FunctionNameTripleUnderscore`), and
      `check_variable_name`'s 3 now-pointless call sites in `collect_var_pattern`.
  - The remaining ~27 covered variants were spot-checked, not exhaustively re-verified line by
    line — the grep pass is what did the real work of finding the misses.
- [x] Fixed a real crash: `x := Struct{field: value}` (an untyped `:=` declaration whose initializer
      is a struct constructor) panicked mid-`mir_parser`-lowering ("variable has no resolved type")
      instead of either working or erroring cleanly — `resolve/typecheck/expression.rs`'s
      `expression_type` (the function `backfill_variable_type` calls to infer an untyped
      declaration's type) had no `StructConstructor` arm at all, only `Literal`/`Variable`/
      `Lambda`/`FieldAccess`; everything else silently fell through to `_ => None`.
  - Root-caused, then generalized: audited every `ExpressionKind` variant `expression_type` was
    missing, scoped to the subset `mir_parser` can actually lower today (the rest — `Match`,
    `TypeOf`, `New`/`NewArray`, `Sizeof`, `Copy`, `Pass`, `Tuple`, `NamedTuple`, `Deref`,
    `StringFormat`, `Constructor` — are M2/M3 surface `mir_parser` rejects outright, so an inferred
    type for them could never reach a running program). Added `StructConstructor` (trivial — the
    target type is already spelled out in the syntax, no real inference needed), `Index` (mirrors
    `foreach_collection_element_type`'s own `SoulType::Array` unwrap), `Unary` (only `!`/`Not` — `-`
    isn't lowered by `mir_parser` yet, so left unhandled same as the M2/M3 kinds), `Ref` (mirrors
    `mir_parser::function::place::lower_ref`'s own array-to-slice-vs-bare-reference bifurcation,
    which had never been ported to the resolver's own type system before), and `Array` literals
    (reuses the existing private `array_literal_element_type` helper for the element type, adds
    `ArrayKind::StackArray(len)`).
  - Proven via `28_untyped_struct_constructor.soul` (the exact reported crash, now compiling and
    running correctly) plus a `mir_parser` regression test and 6 new resolver-level
    `variable_type_backfill_tests.rs` cases (struct field access, indexed array element, `!`,
    referencing-then-indexing a slice — both the "infers fine" and "still catches a real type
    mismatch" sides of each).
  - Also added `expression_type` arms for `TypeOf` (split by `TypeofKind`: only `Value`
    (`expr.typeof`) reflects an actual type, `Null`/`NotNull`/`Union{..}` are boolean checks — the
    initial pass treated all four as `SoulType::Type`, which would have mistyped e.g. `expr typeof
    Type.Variant` used as an `if`/`&&` condition), `Sizeof` (`Uint`), and a general `Unary` fallback
    (any operator's result is its operand's own type, covering `-`/`Neg` alongside `!`/`Not`).
    `Sizeof`/`Neg` aren't lowered by `mir_parser` either (same as the M2/M3 kinds already excluded
    above) — inert today, not incorrect, just outside the "only what can reach a running program"
    scope this pass otherwise held to.
  - `expr typeof Type.Variant`/`expr typeof null` (`TypeofKind::Union`/`Null`/`NotNull`) is
    considered outdated syntax going forward — the intended replacement is `expr.typeof == Type`
    (already `TypeofKind::Value`, no parser change needed) plus a new `if type Variant(binding) =
    expr` pattern-binding form, with `Null`/`NotNull`/`Union` retired from `TypeofKind` entirely.
    Not done here (deliberately deferred) — `parse_typeof_operator`
    (`ast_parser/src/parse/expression/mod.rs`) still parses the old syntax today, so the
    `expression_type` fix above targets the AST as it currently stands, not the future one.
- [x] Fixed a real gap: `value: bool = 1` (an explicit `x: T = value` annotation) silently
      compiled with no type-mismatch check at all — unlike the no-annotation case (`x := value`,
      see the entry above), which infers via `expression_type`, an explicit annotation is never
      inferred, so nothing ever compared it against the initializer. New
      `check_variable_declaration` (`resolve/typecheck/statement.rs`, mirrors `check_assignment`'s
      own lvalue-vs-rvalue check almost exactly) runs whenever `resolve_variable` sees an explicit
      `variable.ty`, instead of falling through to `backfill_variable_type`.
  - Surfaced two latent bugs in `combine_resolved_operand_types` once real programs started
    exercising it here (both previously unreachable, since no declaration check ever ran):
    1. Two `SoulType::Array`s only combined via strict structural equality, so `[&mut]T`/`[&]T`
       (which differ only in `ArrayKind::MutSlice` vs `ConstSlice`) never matched each other — but
       slice mutability isn't checked anywhere else in this compiler yet either (M2 borrow
       checker's job), so `s: [&mut]i32 = &a` (a plain, non-`mut` `&`, which already codegens
       fine) was wrongly rejected. Fixed: `combine_array_types` treats any `MutSlice`/`ConstSlice`
       pairing as compatible.
    2. An array literal's inferred element type (`expression_type`'s `Array` arm) went through
       `array_literal_element_type`, which *defaults* an untyped element (`[1, 2]`'s `UntypedInt`)
       to a concrete type (`Int`) before the declaration check ever saw it — so `a: [2]i32 = [1,
       2]` failed as `[2]i32` vs `[2]int`, the same class of bug `x: i32 = 1` already coerces
       around for a bare scalar. Fixed: the `Array` arm now reads the raw (still-untyped) element
       type directly instead of the defaulting helper, and `combine_array_types` recurses through
       `combine_resolved_operand_types` for the element type too, so the existing
       untyped-literal-coercion rule now also applies one level down, inside an array.
  - Proven via 4 new `assignment_tests.rs` cases (matching/mismatched explicit declarations, a
    generic-parameter skip, the untyped-array-literal coercion, and the slice-mutability case) —
    the latter two were only caught by re-running the full exe suite (`12_slice_index.soul`/
    `13_slice_bounds_check.soul` briefly regressed before the `combine_resolved_operand_types` fix
    above), underscoring why that suite — not just resolver unit tests — is the real oracle here.
- [x] `PlatformInfo` (`soul_utils::compiler_options`) gained a real (if still one-armed)
      selection mechanism, `PlatformInfo::host()` — a `const fn` that picks `new_windows_x86_64()`
      only when `cfg!(all(target_os = "windows", target_arch = "x86_64"))`, and `panic!`s (a
      compile-time error in a `const` context) on anything else, instead of every call site just
      reaching for `new_windows_x86_64()` by name. There's still no cross-compilation support
      anywhere in this compiler (the clang invocation, panic exit codes, and forced LLVM target
      triple are all Windows-specific too), so "the build target" and "the machine `mir_codegen`
      emits code for" are always the same one — `host()` is the right frame for that, not a
      hypothetical `--target` flag. `CompilerOptions::const_default()`,
      `soul_tester::config::COMPILER_OPTIONS`, and the pipeline benchmark's `COMPILER_OPTIONS` all
      route through it now. A real second target still needs its own arm added directly in
      `host()`, not a caller choosing a different named constructor.
  - Proven via 2 new `soul_utils::compiler_options_tests` cases (`host()` matches
    `new_windows_x86_64()`'s values on this target; `CompilerOptions::const_default()`'s platform
    matches `host()`).

### M2 — borrow/move checking (not started)

Scoped via `/grill-me`: "M2" is really a chain of prerequisite pieces, not one task —
`mir_parser` doesn't lower dereference places, doesn't fix `&this`/`&mut this` receivers to
actually borrow (they're still always a full by-value copy — a known M1 shortcut), doesn't emit
`Move`/`MarkMoved`/`SetDropFlag`/`Drop` anywhere, and `FunctionLowerer` has no lexical-scope
tracking at all (one flat locals map) — so "implement the borrow checker" is blocked on all of
that landing first, in order.

- [x] Deref places: `mir_parser` can now lower `*ptr` (explicit) and auto-derefs through a
      `&T`/`&mut T`/`*T` when resolving a field/index access's object/collection (so
      `this.field`/`ref.field` keeps working once a receiver becomes a real reference-typed place
      instead of a value copy — the very next item below). `PlaceElem::Deref` existed in
      `mir_model` already but nothing constructed or consumed it; `mir_codegen`'s `resolve_place`
      previously hard-errored on it (`PlaceProjectionUnsupported`).
  - `mir_parser::function::place`: new `resolve_deref_place` (explicit `*ptr`, mirrors
    `resolve_field_place`/`resolve_index_place`'s shape) and `auto_deref` (a free fn — deref
    once if the type is a reference, pass through otherwise — called before resolving a field's
    object or an index's collection).
  - `require_lowerable` (`function/mod.rs`) gained `SoulType::Reference`/`Pointer` — a bare
    reference-typed *local* (e.g. `p := &x`) was never accepted before this, a separate, more
    basic gap this surfaced: you couldn't even declare a pointer-typed variable to dereference in
    the first place.
  - `mir_codegen::function::resolve_place` gained a `step_into_deref` step: `ptr` holds the
    *address of* the reference's own storage at that point (same invariant every other step
    maintains), so dereferencing means loading the pointer value out of it to get the address it
    actually points at.
  - Also fixed two related bugs in `soul_name_resolver::resolve::typecheck::expression`'s
    `expression_type` found while wiring this up: `Deref` was listed under "always types as
    `none`" (wrong — `*ptr`'s type is whatever `ptr` points at; the same "untyped declaration
    silently crashes" class of bug fixed twice earlier for `StructConstructor` and array
    literals), and `struct_field_type` didn't auto-deref through a reference either (so
    `object.field`'s resolver-level type-checking would have broken the moment `object` became
    reference-typed, even though `mir_parser`'s own lowering now handles it).
  - Proven via `29_deref_places.soul` (explicit deref read+write, plus field access through a
    reference) and 3 new `mir_parser` unit tests (`deref_read_...`, `deref_write_...`,
    `dereferencing_a_non_reference_is_rejected`).
- [x] Fix `&this`/`&mut this` receiver lowering to actually pass a reference instead of a
      by-value copy. `FunctionLowerer::lower` now allocates the `this` local as
      `Reference(method_type)` (mutable for `&mut this`, immutable for `&this`) instead of a bare
      `method_type` value; `this` (consuming) is unchanged. `lower_call`'s receiver-passing branch
      now matches on the callee's `function_kind`: `&this`/`&mut this` build an actual borrow via
      new `lower_receiver_ref` (mirrors `lower_ref`'s non-array path — resolves the receiver
      expression's place, emits an `Rvalue::Ref` into a fresh reference-typed temp, passes that),
      `this` still goes through the existing `lower_operand` by-value copy.
  - No new auto-deref machinery needed: `this.field`/`this.field = ..` inside a `&this`/`&mut
    this` method body already goes through `resolve_field_place`'s existing `auto_deref` call
    (built for the deref-places item above) once `this`'s own local is reference-typed — proven by
    a new unit test asserting the exact `[Deref, Field(0)]` projection.
  - Proven via `30_mut_receiver.soul` (a `&mut this` method mutating a field across two calls,
    then a `&this` method reading it back — this is the concrete case the by-value-copy shortcut
    silently broke: the mutation used to land on a throwaway copy and never persist) and 4 new
    `mir_parser` unit tests (`mut_this_receiver_is_passed_as_a_mutable_reference`,
    `const_this_receiver_is_passed_as_an_immutable_reference`,
    `mut_this_body_accesses_fields_through_a_deref_projection`,
    `consuming_this_receiver_is_still_passed_by_value`).
- [x] Straight-line-only `Drop`/`SetDropFlag` lowering. Scope narrowed from the original bullet
      once the doc's own `add(a, b)` worked example was checked against literally: it drops only
      the body-declared `c`, never the parameters `a`/`b` — so "every local" turned out to mean
      "every `body_locals` entry" (locals bound to an explicit `x := ..`/`x: T = ..` declaration
      inside the function via `lower_variable`), not literally every entry in the flat `locals`
      map. Parameters, `this`, the return local, and every compiler-internal temp (checked-overflow
      tuples, bounds-check bookkeeping, ref/deref temps, ...) are deliberately never tracked —
      unconditional across those would have meant no shape at all (unlike the doc's own
      pre-overflow-check-era sketch, this lowerer already builds several internal temps per
      expression) instead of the intended minimal, provable slice.
  - `FunctionLowerer` gained `body_locals: Vec<LocalId>` (declaration order) and `nesting_depth:
    usize`. New `push_assign(place, rvalue)` replaces every raw `self.statements.push(Assign(..))`
    call site and appends `SetDropFlag(place.local, true)` right after, but only when
    `place.local` is a tracked `body_locals` entry (mirrors the doc's own rule, "every place
    written via Assign", narrowed the same way as the Drop scope above — the doc's own example
    shows no `SetDropFlag` for `c` either, since it's a plain primitive and no
    `AutoCopy`/move-only classification exists yet; that's still the next TODO item below, and
    this pass's own `SetDropFlag` emission stays unconditional per tracked local, not gated by
    type, until that classification lands).
  - New `seal_return()` replaces every `self.seal(Terminator::Return, None)` call site
    (`function/mod.rs`'s implicit-fallthrough, and all four explicit-`return`/tail-return sites in
    `control_flow.rs`). At `nesting_depth == 0` (the function's own top level — not nested inside
    an `if`/`for`) it emits one `Drop` terminator per `body_locals` entry, in reverse declaration
    order, chained through fresh blocks ahead of the final `Return`; at depth > 0 it's unchanged
    from before this feature existed (a plain `Return`, no drops) — there's no scope stack yet to
    know which locals actually belong to the branch being exited, so a `return` nested inside an
    `if`/`for` is exactly the deferred "no if/for/early-exit handling yet" case. `nesting_depth` is
    incremented/decremented around `lower_if`'s both branches and `lower_for`'s body.
  - `mir_codegen`'s `Terminator::Drop` (previously `DropUnsupported` — an error, since nothing
    constructed one before this) now just branches to its `target`, same as `Goto`: no destructor
    exists anywhere in this compiler yet, so every `Drop` is a pure scope-exit marker with no
    runtime effect until M2's checker (and, eventually, real struct/array drop glue) exist.
  - This did ripple through several existing `mir_parser` unit tests that asserted exact
    statement/block counts for a body-declared local's own function (`lowers_arithmetic_with_a_
    variable_and_a_return`, `block_tail_expression_with_no_return_or_semicolon_is_an_implicit_
    return`, `array_reference_lowers_to_a_ref_plus_fat_pointer_aggregate`, `none_returning_
    function_can_fall_off_the_end`) — updated to assert the new, larger shape explicitly rather
    than just bumping magic numbers.
  - Proven via 7 new `mir_parser` unit tests: `multiple_body_locals_are_dropped_in_reverse_
    declaration_order`, `reassigning_a_body_local_sets_its_drop_flag_again`, `reassigning_a_
    parameter_gets_no_drop_flag_and_is_never_dropped`, `a_return_nested_inside_an_if_gets_no_drop_
    chain_yet`, `a_struct_typed_body_local_is_dropped_the_same_as_a_primitive_one` (plus the 4
    rewritten pre-existing tests above) — no new exe test needed since `Drop`/`SetDropFlag` have no
    runtime effect yet; the full 30-test exe suite re-passing unchanged is what proves this feature
    didn't silently break anything it now runs through on every single function.
- [x] `Move`/`MarkMoved` lowering for straight-line code (function-call arguments,
      struct-constructor fields, array-literal elements, plain reassignment/declarations) — every
      sub-item below is done. No opt-in mechanism for a struct to become `AutoCopy` exists (a
      non-generic trait can't express a marker bound), so this stays the concrete (M1) subset
      only; extending it is M3's job once generics/traits do more.
  - [x] `is_auto_copy(SoulType)` classification. `/grill-me`'d once the user's own `SoulType`
        `TypeId`-flattening refactor landed (nested fields like `ReferenceType.inner`/
        `ArrayType.of_type` are now interned `TypeId`s, not inline `Box<SoulType>`), to re-derive
        the classification against the actual current shape instead of a stale assumption.
    - Corrected the "structs and arrays are move-only" shorthand from the M2-kickoff interview:
      slices (`MutSlice`/`ConstSlice`) are fat pointers, non-owning, so `AutoCopy` — only
      `StackArray`/`HeapArray` (owning array storage) are move-only.
    - `fn is_auto_copy(&self, ty: &SoulType, span: Span) -> MirResult<bool>` (`function/mod.rs`,
      next to `require_lowerable`) only ever needs an opinion on `require_lowerable`'s own
      accepted subset — calls it first, so "can this become a local" and "is it `AutoCopy`" are
      governed by one accept list, not two independent matches that could drift apart as
      `SoulType` grows new variants. Fallible (returns the same `NonPrimitiveType` error
      `require_lowerable` would), not a panic.
    - Proven via unit tests driving a `FunctionLowerer` directly (no indirect call path exists
      yet at the time): primitives/references/pointers/slices → `true`; a `StackArray` and a
      resolved struct (`Stub`) → `false`; a non-`require_lowerable` type → the same rejection.
  - [x] Function-call arguments (`/grill-me`'d separately, once `is_auto_copy` existed to wire
        up). New `lower_call_operand` (`statement.rs`, next to `lower_call`) — deliberately *not*
        added to `lower_operand` itself, which is shared by every other operand site (binary-op
        flattening, `return`, variable-declaration initializers, ...) and would have silently
        turned "function-call arguments first" into "every operand everywhere" in one shot.
        `lower_call.arguments`'s loop and the consuming-`this` receiver branch (`obj.consume()`
        reads `obj` exactly like `consume(obj)` would — same treatment) now call it instead of
        `lower_operand` directly.
    - Scoped to a bare `Variable` argument only — anything else (a literal, a nested computation,
      a struct-constructor literal passed inline, or a move-only value reached through a
      field/index/deref projection like `consume(container.item)`) falls through to the ordinary
      `lower_operand`/`Copy` path unchanged. The projection case specifically isn't a shortcut:
      `SetDropFlag` is per-`LocalId`, not per-place, so a field-projection move has no sound way
      to express "only this one field moved" — that needs partial-move tracking, which doesn't
      exist. Emits `MarkMoved(local)` + `SetDropFlag(local, false)` ahead of the `Call` terminator
      when `is_auto_copy` says the argument's own local type is move-only; a plain `Copy` matching
      today's behavior otherwise.
    - Move-eligibility is per-local (via `self.locals[local].ty`), not gated on `body_locals`
      (Drop-chain scope tracking is a separate concern) — a parameter or `this` qualifies exactly
      the same as a body-declared variable.
    - `mir_codegen` already treats `Operand::Copy`/`Move` identically and already no-ops
      `MarkMoved`/`SetDropFlag` (from the Drop/SetDropFlag pass) — confirmed *before* implementing,
      unlike `Drop` which briefly hard-errored the first time it was constructed. Zero codegen risk,
      so the full 30-test exe suite re-passing unchanged is the actual proof nothing broke.
    - Proven via 4 new `mir_parser` unit tests (`a_move_only_struct_argument_is_moved_not_copied`,
      `an_autocopy_primitive_argument_is_still_copied`,
      `a_move_only_argument_reached_through_a_field_projection_is_still_copied`,
      `a_move_only_parameter_argument_is_moved_the_same_as_a_body_local`) plus updating
      `consuming_this_receiver_is_still_passed_by_value` (renamed
      `..._is_still_passed_directly_with_no_ref_rvalue`), whose premise — a consuming receiver is
      always `Copy` — stopped holding the moment its `Number` struct became correctly move-only.
  - [x] Plain reassignment (`x = y`), extended in the same pass to declaration initializers
        (`x := y`) too — `/grill-me`'d: the TODO wording only said "plain reassignment," but
        `lower_variable`'s initializer and `lower_assignment`'s right-hand side both bottom out in
        the exact same `lower_rvalue` call for a bare-variable source, so scoping Move to only one
        of the two would have made `x = y` move `y` while `x := y` silently didn't, even though
        `mir-design.md`'s own worked example (`s := Session.new()`, later moved into `consume`) is
        precisely the declaration case.
    - New `move_eligible_operand` (`statement.rs`) factors out the shared "is `expr_id` a bare
      `Variable`, and is its local move-only" decision — `None` for anything else (a literal, a
      nested computation, a struct-constructor literal, a field/index/deref projection), `Some(_)`
      otherwise. `lower_call_operand` (the function-call-arguments slice above) now delegates to
      it instead of duplicating the logic; new `lower_movable_rvalue` wraps it as `Rvalue::Use(_)`
      for `lower_variable`/`lower_assignment` to call instead of `lower_rvalue` directly —
      `lower_rvalue` itself stays untouched, same discipline as `lower_operand` before it.
    - Same field-projection scoping as the call-argument case, for the same reason (`SetDropFlag`
      is per-`LocalId`, not per-place): `t := c.item` still falls through to a plain `Copy`.
    - Proven via 4 new `mir_parser` unit tests: `a_move_only_declaration_initializer_moves_the_
      source`, `a_move_only_reassignment_moves_the_source`, `an_autocopy_declaration_initializer_
      is_still_copied`, `a_move_only_declaration_initializer_reached_through_a_field_projection_
      is_still_copied`. No exe-test regressions (still all 30 passing) — same zero-codegen-risk
      reasoning as the call-argument slice: `Operand::Move` already codegens identically to `Copy`.
  - [x] Struct-constructor fields, array-literal elements — `/grill-me`'d as "what's next" once
        plain reassignment landed; the TODO originally implied treating these as separate slices
        the way call-arguments/declarations had been, but `lower_struct_constructor` and
        `lower_array_literal` (`rvalue.rs`) turned out to be structurally identical adjacent
        functions (`operands.push(self.lower_operand(value_id)?)` in a loop, nothing else) — no
        real complexity boundary between them, so done together in one pass instead of two.
    - `lower_call_operand` renamed to `lower_move_aware_operand` (`statement.rs`) and made
      `pub(super)`, since its body was already fully generic — "move-aware operand, falling back
      to `lower_operand`" — with nothing call-specific about it; `lower_struct_constructor`/
      `lower_array_literal` now call it directly instead of a third near-duplicate wrapper.
    - Same field-projection scoping as every prior slice, same reason (`SetDropFlag` is
      per-`LocalId`, not per-place): a field/index/deref-projection field/element value still
      falls through to a plain `Copy`.
    - Pre-flight checked `mir_codegen`'s `codegen_aggregate` before implementing (same discipline
      as the call-argument slice): both the struct and array branches route every operand through
      the same `codegen_operand` call already confirmed `Copy`/`Move`-agnostic — zero codegen risk.
    - Proven via 4 new `mir_parser` unit tests: `a_move_only_struct_constructor_field_moves_its_
      source`, `an_autocopy_struct_constructor_field_is_still_copied`, `move_only_array_literal_
      elements_move_their_sources`, `an_autocopy_array_literal_is_still_copied`. All 30 exe tests
      still pass unchanged.
- [x] Scope-stack tracking in `FunctionLowerer`, for `if`/`else` branches (`/grill-me`'d "what's
      next" down from the full bullet: `for`/`break`/`continue` deferred — a `break`/`continue`
      only unwinds up to its nearest enclosing *loop*, a materially different target than
      `return`'s "unwind everything," and `for` needs a per-iteration re-drop/rebuild story this
      slice doesn't build; see the follow-up bullet below).
  - `body_locals: Vec<LocalId>` (a single flat list) became `scopes: Vec<Vec<LocalId>>` — a real
    stack of frames, frame `0` the function's own top level, `lower_if` pushing one fresh frame
    per branch body (`then`, and a plain `else { .. }`) it lowers. An `else if` isn't a body of
    statements at that level at all — it's a compound statement whose own recursive `lower_if`
    call already manages its own frames, so that position gets no frame of its own, matching its
    behavior from before this slice.
  - Two distinct scope-exit shapes, both new methods on `FunctionLowerer`: `seal_scope_exit`
    (falling through a branch to its join block — pops *just* that branch's own frame, drops it,
    `Goto`) and `seal_return` (rewritten to unwind the *entire* `scopes` stack, innermost frame
    first) — confirmed with the user before implementing: a branch's own locals go out of scope
    at its own join point regardless of whether the branch returns early, but only an actual
    `return` needs to unwind enclosing scopes too. Both share a new `drop_chain` helper.
  - `nesting_depth: usize` (used for both `if` and `for`) narrowed to `for_loop_depth: usize`
    (`lower_for` only) — confirmed explicitly: a `return` reached through *any* enclosing `for`,
    however many `if`s are also on the way, is still a hard stop with zero drops, even for
    otherwise-safe-to-drop locals declared entirely outside the loop. `seal_return` checks this
    before touching `scopes` at all.
  - `push_assign`'s "is this local tracked" check now searches every open frame (`scopes.iter()`),
    not just one flat list.
  - Proven via 4 new `mir_parser` unit tests (`an_if_branch_local_is_dropped_at_its_own_join_
    point_not_the_functions_end`, `return_inside_an_if_branch_drops_branch_then_enclosing_locals_
    in_order`, `for_loop_is_a_hard_stop_for_return_unwinding`) plus rewriting one whose premise —
    "a return inside an if gets zero drops" — stopped holding the moment `if`/`else` got real
    scope tracking (renamed `a_return_nested_inside_an_if_now_drops_enclosing_scope_locals`). All
    30 exe tests still pass unchanged.
- [x] Extend scope-stack tracking to `for` bodies plus `break`/`continue` unwinding to the correct
      loop-boundary frame. `/grill-me`'d "what's next": surfaced that once a `for` body gets a real
      frame (this bullet's own first half), `return`'s old hard-stop-on-any-enclosing-`for` rule
      (from the previous slice) no longer has a reason to exist — a `return` diverges straight out
      of the function, so there's no per-iteration concern the way `break`/`continue` genuinely
      have; confirmed with the user and lifted it as part of this same pass.
  - A `for` body is now scoped exactly like an `if`-branch: `lower_for` pushes a fresh `scopes`
    frame before lowering the body, and at the end either `seal_scope_exit(header_id)` (fell
    through normally — pop the frame, drop it, loop back via the *same* per-iteration edge every
    normal iteration takes) or a plain pop (already terminated via an internal `break`/`continue`/
    `return`, each of which already emitted its own drop chain on the way out).
  - `break`/`continue` need a *partial*-stack unwind, not `seal_return`'s full one — from the
    innermost frame down to (and including) the loop's own frame, never past it (locals declared
    outside the loop stay alive). `LoopTargets` gained `loop_frame_index: usize`
    (`self.scopes.len()` at the moment the loop's own frame was pushed) so a `break`/`continue`
    inside a *nested* loop unwinds to that loop's own boundary, not an outer one. New
    `seal_loop_exit(from_index, target)` and a shared `unwind_from(from_index)` — `seal_return`
    is just `unwind_from(0)` now, no `for_loop_depth` gate at all (the field was deleted outright
    once nothing read it anymore, not left as a dead vestige).
  - Zero new codegen risk: only `Drop`/`Goto`/`Return`, all already safe from earlier slices.
  - Proven via 7 new `mir_parser` unit tests (`a_for_loop_body_local_is_dropped_on_the_normal_
    back_edge_to_the_header`, `break_drops_the_loop_bodys_own_local_before_exiting`,
    `continue_drops_the_loop_bodys_own_local_before_looping_back`, `break_inside_a_nested_if_
    drops_the_ifs_and_loops_frames_but_not_an_outer_one`) plus rewriting two whose premise —
    "a return through an enclosing for gets zero drops" — stopped holding the moment `for` got
    real scope tracking (`a_return_through_an_enclosing_for_now_drops_it_too`, replacing
    `for_loop_is_a_hard_stop_for_return_unwinding`). All 30 exe tests still pass unchanged.
- [ ] Implement borrow checker as a MIR pass over the concrete (M1) subset. `/grill-me`'d "what's
      next"; settled scope before writing anything:
  - **Move-checking only** — real borrow/lifetime-conflict analysis (do two live `&`/`&mut`
    borrows of the same place overlap) is a fundamentally different, much bigger algorithm
    (needs actual liveness tracking; nothing in the MIR records borrow lifetimes at all) and is
    explicitly deferred to a later M2 slice.
  - Confirmed the user wants to genuinely **mirror rustc's own approach**: a real flow-sensitive
    dataflow analysis over the CFG, not a flow-insensitive "moved anywhere ⇒ reject" scan (which
    would wrongly reject valid branch-exclusive-move code, e.g. moving the same local in each arm
    of an `if`/`else`).
  - That, in turn, means fixed-point iteration is eventually required — a `for` loop's back-edge
    makes the CFG cyclic, and a single forward pass can't correctly answer "is this moved" for
    code whose answer depends on a previous iteration. **First slice deliberately narrowed to
    straight-line functions only** (no `if`/`for` at all) to avoid needing that machinery (or even
    CFG-join/predecessor computation) on day one — any function containing a `SwitchInt` (both
    `if` and `for` lower to one) is skipped entirely, unanalyzed, rather than crashed on or
    incorrectly analyzed. `if`/`else` join-merging and `for`'s fixed point are explicit, separate
    follow-ups.
  - **Wired into the real pipeline now** (`mir_run::to_mir`, right after lowering) as a **hard
    error** (`Fault::error_with_kind`, not a warning) — this is genuinely new territory: every
    prior M2 slice only ever *constructed* MIR shapes, nothing ever rejected a program before this.
    Verified zero regression risk *before* wiring it in: manually audited (and cross-checked with
    an independent background scan) all 8 struct-using `.soul` exe tests for a bare-variable
    struct/array local used twice with the first use in a move position — none exist; every
    existing move in the suite is either a single use or a method call through `&this`/`&mut this`
    (a borrow, not a move).
  - New `mir_parser::move_check::check_moves(&Function) -> Vec<Fault<MirErrorKind>>`, new
    `MirErrorKind::UseAfterMove`. State is derived entirely from where `Operand::Move`/`Copy`
    actually appear and `Assign`'s own whole-place reinitializing writes — **deliberately not**
    from the `MarkMoved`/`SetDropFlag` statements the lowerer also emits: those are pushed
    immediately *before* the very terminator that embeds the `Operand::Move` they correspond to,
    so treating `MarkMoved` itself as "moved from here on" flagged a value's own first, legitimate
    move against itself (a real bug caught by the checker's own unit tests, then fixed by deriving
    state from the operands directly instead).
  - Proven via 5 new `mir_parser` unit tests (double-move, reassign-then-reuse, branch-skipping,
    `AutoCopy` exemption, borrowing an already-moved value) plus 2 new `mir_run` integration tests
    exercising the real `to_mir` wiring end-to-end (a genuine violation faults; the same pattern
    reached only through a branch doesn't, yet). All 30 exe tests still pass unchanged — the actual
    proof this shipped with zero regressions, since the exe-test harness has no mechanism for an
    expected-to-fail-to-compile program (a separate, bigger tooling gap, not addressed here).
- [ ] **Pipeline architecture cleanup** (`/grill-me`'d 2026-09-17, paused the borrow-checker's own
      "extend move-check to `if`/`for`" slice to do this first) — considered adding a HIR stage
      (`AST → HIR → MIR`) to fix a felt "MIR does too much" discomfort, then talked it back down:
      a HIR would cost a whole new crate + lowering pass + rewriting every `soul_name_resolver`/
      `mir_parser` consumer to read it, for a problem that turned out to be about **where specific
      logic lives**, not about missing a canonical desugared tree. Decision: **no HIR** — keep
      `lexer → parser → AST → name/typecheck → MIR → LLVM` as-is, and instead separate two things
      that had drifted into the wrong place:
  - The real axis, generalized from this interview (mirrors why rustc type-checks on HIR but
    borrow-checks on MIR): **flow-insensitive** logic (the answer only depends on a type/expression
    *shape*, never on control-flow position — e.g. `is_auto_copy(ty)`) belongs in the
    resolver/`DeclareStore`, computed once and cached per `TypeId`. **Flow-sensitive** logic (the
    answer depends on *where in the CFG* you are — move-checking, drop-chain/scope tracking) is
    correctly already in `mir_parser` and stays there.
  - A full-pipeline scan (beyond the flow-insensitive audit below) also found a second, related
    smell already precedented by the tail-return case (`soul_name_resolver`'s `check_tail_return_
    type` vs. `mir_parser` having to "re-derive the same convention independently", per the
    Implicit-return entry above): **convention drift** — the same rule reimplemented in two crates
    with no shared source, kept in sync only by comment. `mir_codegen`'s own `resolve_struct`
    (`types.rs`) admits this directly in its doc comment: *"mirrors `mir_parser`'s own
    `resolve_struct`"*.
  - [x] **Part A1** — pure, `DeclareStore`-independent classifiers moved to `ast_model`/`soul_utils`
        as inherent methods (no caching needed, lowest risk — cheap to recompute, callers just no
        longer duplicate the rule):
    - `PrimitiveTypes::{is_signed, is_float, signed_min}` (`soul_utils::soul_names`) — absorbs
      `mir_parser`'s former `is_float_primitive`/`signed_primitive_min` *and* `mir_codegen`'s own
      independently-reimplemented `is_signed` (`types.rs`), found in the full-pipeline scan; both
      crates' call sites (`operators.rs`'s `signed_primitive_min` call,
      `rvalue.rs`'s `is_float_primitive` match, `mir_codegen::rvalue`'s `operand_is_signed`) now
      call the one shared method instead of three independent copies of the same rule.
    - `BinaryOperatorKind::{is_supported, is_checked_arith, is_checked_div}` (`ast_model::ast::
      operators`) — absorbs `mir_parser`'s former `is_supported_binary_op`/`is_checked_arith_op`/
      `is_checked_div_op` (`function/rvalue.rs`).
    - `SoulType::is_primitive` (`ast_model::ast::soul_type`) — the pure shape-check half of
      `require_primitive`; `require_primitive` itself stays in `mir_parser` (it constructs a
      `MirErrorKind`-specific `Fault`, not reusable across crates) but now delegates the actual
      classification instead of re-matching `SoulType::Primitive(_)` inline.
    - Proven by the full `cargo test --workspace` (all crates green, zero warnings) + all 33 exe
      tests (`scripts/run_codegen_tests.py`) passing unchanged — none of these were behavior
      changes, purely relocations.
  - [x] **Part A2** — moved `resolve_struct`/`is_lowerable`/`is_auto_copy` (formerly
        `mir_parser::function::mod`'s `require_lowerable`/`is_auto_copy` and `function::type`'s
        `resolve_struct`) onto `DeclareStore` itself (`ast_model::declare_store`), as pure
        classification methods `mir_parser` now calls as thin wrappers (`require_lowerable` keeps
        its own `Fault`-constructing shape, but delegates the actual classification).
    - **No cache** — dropped from the original plan after a `/grill-me` follow-up surfaced a real
      correctness hazard: `SoulType::Stub` only carries a bare `name` (no module qualifier), so two
      *different* modules' same-named-but-different structs intern to the **same** `TypeId` — a
      `TypeId`-only cache would silently answer with whichever module's struct got cached first.
      Since the actual computation being deduplicated (a `BiMap` lookup + an enum `match`, plus one
      already-module-scoped `HashMap` lookup for struct resolution) was never a measured performance
      problem — only a *duplicated-across-crates* one — the fix was to just take `module` as an
      explicit parameter everywhere (same cost as before, correctness-safe by construction) rather
      than force a cache to be correct. Two follow-up ideas surfaced by that discussion, logged
      rather than acted on:
      - **Smaller stopgap (not done):** give `Stub` an `Option<ModuleId>` field that participates in
        its own `Eq`/`Hash`, so structurally-identical-but-different-module `Stub`s intern to
        genuinely different `TypeId`s (making a future `TypeId`-only cache sound). Bounded to 4
        production construction sites (`ast_parser`'s `parse/soul_type.rs` ×2,
        `parse/statements/{objects,from_keyword}.rs`, each of which already has the current module
        via `ParseInfo.id`) plus ~20 existing test assertions that construct
        `Stub::new("Name")`/compare against a parsed result, which would need the right module
        threaded through too (the module field can't be excluded from `Eq`/`Hash` without silently
        defeating the fix). Considered and deliberately not done in this pass — real but bounded
        cost, revisit if `resolve_struct`-family calls ever actually show up in a profile.
      - **Bigger, root-cause redesign (not done):** stop interning bare-name `Stub` references into
        the long-lived `SoulType`/`TypeId` system at all — have the resolver replace a `Stub` with a
        real resolved reference (e.g. `SoulType::Struct(StructId)`, pointing straight at the
        declaration) the moment it resolves it, so every downstream consumer (`mir_parser`,
        `mir_codegen`) carries an unambiguous, self-contained type with **no module parameter needed
        anywhere** — the specific caching hazard above stops being possible by construction, not by
        careful parameter-threading. Would also mean an unresolved name becomes a hard resolve-time
        error instead of a `Stub` that every consumer downstream has to independently reject. Not
        attempted here: it ripples through the *entire* already-completed `TypeId`-flattening series
        (every AST-node-attached type field would need a "still just a name, ask the resolver to
        finalize it" story), since types are interned eagerly *during parsing*
        (`ast_parser::intern_type`), before the resolver — the only pass with full module/import
        context — has run at all.
    - Proven by the same full `cargo test --workspace` (zero warnings) + all 33 exe tests
      (`scripts/run_codegen_tests.py`) bar as A1 — pure relocation, no behavior change.
  - [x] **Part A3** — moved `is_type_boolean`/`expression_is_bool` (`mir_parser::function::type`)
        to `ExpressionId::is_boolean` (`ast_model::ast::expression`, plus a private
        `is_variable_boolean` helper) — not literally "into the `soul_name_resolver` crate" as
        originally worded: `mir_parser` only depends on `ast_model`/`mir_model`/`soul_utils` in
        production code (`soul_name_resolver` is a dev-dependency, tests only), so anything
        `mir_parser` needs to call has to live somewhere both crates actually share — `ast_model`,
        same destination as A2's `DeclareStore` methods, not the resolver crate itself.
    - `is_float_operand` turned out to need **no further move**: its only flow-insensitive piece
      (is this primitive float-typed) was already extracted in A1
      (`PrimitiveTypes::is_float`) — what's left is walking `mir::Operand`/`mir::Place`
      projections via `place_type`/`operand_type`, which is inherently MIR-only structure (doesn't
      exist before MIR lowering) and belongs with A5's `place_type`/`operand_type` partial
      extraction, not here.
    - Proven by the same full `cargo test --workspace` (zero warnings) + all 33 exe tests bar as
      A1/A2 — pure relocation, no behavior change.
  - [x] **Part A4** — new `SoulType::deref_once(&self, declares) -> Option<SoulType>`
        (`ast_model::ast::soul_type`): the shared type decision (is this `&T`/`&mut T`/`*T`, what's
        the inner type — `None` for anything else, no automatic pass-through). Three previously
        independent copies of this exact match now all delegate to it:
    - `mir_parser::function::place`'s `auto_deref` (free fn) — kept its `&mut mir::Place`
      side-effect signature, but the match itself is now `match ty.deref_once(declares) { Some(inner)
      => { push Deref; inner } None => ty }`.
    - `mir_parser::function::place`'s `resolve_deref_place` (explicit `*ptr` lowering) — had its own
      *third*, previously-unflagged copy of the same match (found while doing this slice, not
      caught by the original scan since it fails instead of passing through) — now
      `value_ty.deref_once(self.declares)`, faulting via `DerefTargetNotAReference` on `None`
      instead of matching `Reference`/`Pointer` inline.
    - `soul_name_resolver`'s `expression_type`'s `Deref` arm and `struct_field_type`'s auto-deref
      prelude (`resolve/typecheck/expression.rs`) — previously the two comment-synced ("mirrors
      `mir_parser::function::place::{resolve_deref_place,auto_deref}` one layer up") duplicates;
      now both call `deref_once` directly instead of re-implementing the match.
    - Proven by the same full `cargo test --workspace` (zero warnings) + all 33 exe tests bar as
      every A-slice above — pure relocation, no behavior change. `29_deref_places.soul` specifically
      exercises both `mir_parser` call sites end-to-end.
  - [x] **Part A5** — partial, as scoped, plus a correction found while doing it: the function
        originally named (`mir_codegen::rvalue::operand_type`) turned out **not** to be the real
        duplicate — it only ever looks at a bare local with an *empty* projection and returns an
        LLVM `BasicTypeEnum`, never walking `Field`/`Index`/`Deref` at all. The actual duplicate of
        `place_type`'s per-step walk was `mir_codegen::function::resolve_place`'s
        `step_into_field`/`step_into_index`/`step_into_deref`, which compute the same Soul-type
        transition interleaved with the LLVM pointer/GEP mechanics.
    - New `PlaceElem::step_type(&self, ty, declares, module) -> Option<SoulType>` (`mir_model`, not
      `ast_model` — `PlaceElem` is MIR-only, and `mir_model` already depends on `ast_model`) is the
      one shared "given a type and a projection step, what's the resulting type" rule.
      `mir_parser::place_type`'s loop is now a two-line `for elem in &place.projection { ty =
      elem.step_type(&ty, self.declares, self.module)?; }` (previously a 27-line inline match); its
      `Deref` arm was *also* still duplicating `SoulType::deref_once` (A4) inline — folded in too,
      a gap the earlier A4 pass missed.
    - `mir_codegen::function::resolve_place`'s three steps: `step_into_deref` now calls
      `soul_ty.deref_once(declares)` directly (a clean full swap — no new `mir_model` code needed,
      A4's helper already covered it exactly); `step_into_field`/`step_into_index` now get their
      `field_ty`/`element_ty` from `PlaceElem::{Field,Index}(..).step_type(..)` instead of
      re-deriving it inline, while keeping their own GEP-shape dispatch and (for `step_into_index`)
      the extra slice-kind guard (`MutSlice`/`ConstSlice` only) as an explicit check *before*
      calling `step_type` — preserving that real strictness difference from `place_type` (which
      doesn't care what kind of array it is, since it's just re-deriving an already-validated
      place) rather than blurring it into one shared function.
    - `mir_codegen::types::resolve_struct`'s only call site inside `function.rs` (`step_into_field`)
      is gone as a side effect — it now goes through `step_type` → `DeclareStore::resolve_struct`
      (A2) instead. `types::resolve_struct` itself still exists for `stub_type`'s own call site;
      fully retiring it is Part C's job, not this one.
    - Proven by the same full `cargo test --workspace` (zero warnings) + all 33 exe tests bar as
      every A-slice above.
  - [x] **Part C** — `operand_is_signed` was already retired in A1; `mir_codegen::rvalue::operand_type`
        turned out (per A5's correction) not to be a real duplicate at all, so nothing to retire
        there. What was left:
    - `mir_codegen::types::resolve_struct` — its only remaining call site (`stub_type`, within the
      same file; `function.rs`'s `step_into_field` call site was already removed in A5) now goes
      through `declares.resolve_struct(ty, module)` (A2) directly; the free function itself is
      deleted.
    - New `ArrayKind::is_lowerable(self) -> bool` (`ast_model::ast::soul_type`) — the actual shared
      "which array kinds does this pipeline represent" rule, needing no `DeclareStore` at all (pure
      enum classification). `DeclareStore::is_lowerable`'s array arm and `mir_codegen::array_type`'s
      accept/reject guard both now call it, instead of each independently spelling out
      `StackArray(_) | MutSlice | ConstSlice`. `array_type`'s own kind-`match` still does its own
      per-kind LLVM-representation dispatch (that part is inherently codegen-specific, not shape
      classification) — only the accept/reject decision moved, with its final
      `StackArrayWildcard | HeapArray` arm now `unreachable!` (rejected by the guard above it).
    - Struct-field-ordering was checked and found to be nothing to extract: `stub_type`'s field loop
      is a plain `for field in struct_.fields` — the "same order as `mir_parser`'s `Aggregate`
      operands" invariant is just both sides independently iterating a `Vec` in its natural
      declared order, not a computed ordering rule; there's no function to share.
    - Proven by the same full `cargo test --workspace` (zero warnings) + all 33 exe tests bar as
      every A-slice above.
  - [ ] **Part B** — checked, premise didn't hold, **not done, no scaffolding created**: there's no
        `foreach`/`while` rewrite in `control_flow.rs` to relocate. `ast::ForCondition` has three
        variants (`Loop`, `While(ExpressionId)`, `Foreach { index, element_kind, collection }`), but
        `lower_for` only handles `While` — `Loop`/`Foreach` both fall straight through to
        `UnsupportedLoopCondition`, unimplemented. And what *is* there for `While` isn't really
        "desugaring" at all — it's a direct, single-step translation of an already-primitive
        condition into MIR blocks (header/body/exit + `SwitchInt`), not a rewrite of one surface
        form into a simpler one first. A real `mir_parser::desugar` submodule only earns its place
        once `Foreach` lowering is actually implemented (`for x in xs { .. }` → an index variable +
        a `While`-shaped loop + an increment, rewritten before/during lowering) — revisit this
        bullet then, not before.
  - [ ] Once A/B/C land: resume the paused borrow-checker slice — CFG traversal (back-edge/cycle
        detection via DFS) + two-state definite/maybe dataflow for `if`/`else` move-checking (see
        the borrow-checker entry above for the full scope of that slice).
- [ ] **Replace `SoulType::Stub` with resolved type references** — the "bigger, root-cause redesign"
      logged above, picked up on user request (`/grill-me`'d into a full plan first: 3 parallel
      Explore passes, then a 7-slice plan, `C:\Users\tim_k\.claude\plans\vast-wiggling-galaxy.md`).
      Scope turned out bigger than the earlier note implied: `Stub` isn't struct-only — it's the
      single, undifferentiated representation for **five** kinds of names (struct, enum, trait,
      type-alias, and generic type parameters, the last disambiguated today purely by matching
      `stub.name` against the enclosing `Generic` list) — user confirmed covering all five, not
      just structs. Mechanism settled on: **not** AST mutation (`soul_name_resolver`'s `NameResolver`
      holds only `&AstStore`, never `&mut` — confirmed by research, so "rewrite the AST node's own
      type field once resolved" isn't achievable without restructuring its whole borrow-splitting
      pattern) but a `DeclareStore`-owned side table (`type_resolves`, a later slice), mirroring the
      already-working `variable_resolves`/`function_resolves` pattern — a referencing id maps to a
      resolved declaration id, no AST mutation needed.
  - [x] **Slice 1 — identity only.** `Stub` gained `occurrence: NodeId` (`ast_model::ast::
        soul_type`), participating in its own `Eq`/`Hash` — a fresh id allocated once per syntactic
        occurrence (`ast_parser`'s existing `self.alloc_node()`), not shared/deduplicated the way
        `name`/`generics` alone were. This is the load-bearing move: every syntactic occurrence now
        interns to its own unique `TypeId`, fixing the cross-module collision bug as a side effect
        and giving a future `type_resolves` table a sound key to use. `Stub::new_at(name,
        occurrence)` is the real constructor (used at all 4 production sites — `parse/soul_type.rs`
        ×2, `parse/statements/{objects,from_keyword}.rs`); `Stub::new(name)` stays as a test/fixture
        convenience defaulting to `NodeId::ERROR`, documented as never equal to a genuinely parsed
        `Stub`.
    - **The correctness risk the plan called out as the crux of this slice, found and fixed**: any
      comparison relying on "same `TypeId`/`SoulType` ⇒ same type" for a `Stub`-shaped value now
      silently breaks, since two independently-written occurrences of the same name (e.g. a
      parameter's declared type vs. an argument's inferred type) are never `==` any more. New
      `DeclareStore::{types_equal_ignoring_occurrence, soul_types_equal_ignoring_occurrence}` — a
      recursive structural-equality check that treats two `Stub`s as equal whenever `name`/
      `generics` match, ignoring `occurrence`, recursing through every `TypeId`-nesting `SoulType`
      variant (`Array`, `Reference`/`Pointer`, `RawPtr`, `Res`, `Optional`/`ImplTrait`,
      `NamedVariant`, `Function`, `TupleKind`). Explicitly a stopgap (documented on the function
      itself): restores today's pre-`occurrence` "same name = same type" semantics without needing
      `type_resolves` yet; once that lands, this should compare *resolved declarations* instead of
      raw names, which would also fix the cross-module-collision case this still can't (same
      limitation `resolve_struct` already had).
    - Three real call sites found and fixed (an Explore audit pass, cross-checked by hand):
      `combine_resolved_operand_types` (`soul_name_resolver`'s single choke point for
      argument-vs-parameter, declared-vs-initializer, struct-field, enum-variant-argument, and
      binary-operand type-checking — fixing this one spot fixed ~8 call sites transitively);
      `signatures_match` (`check_impl_conformance`'s trait-vs-impl signature comparison — gained a
      `&DeclareStore` parameter it didn't have before, since it was comparing raw `TypeId`s with no
      alias-resolution safety net at all); `DeclareStore::find_function`'s owner-type matching
      (method-call resolution — this one wasn't caught by the static audit at all, only by the test
      suite: it broke 3 `mir_parser` receiver tests and 1 `soul_name_resolver` ambiguous-impl test
      with `FunctionCallHasNoResolvedTarget`/lookup failures, since a method's declared owner type
      and a call site's receiver type are exactly two independent occurrences of the same struct
      name).
    - New regression test proving the fix actually works (per the plan's own verification bar):
      `struct_argument_matches_parameter_across_independently_parsed_occurrences`
      (`soul_name_resolver`'s `function_call_argument_tests.rs`) — a struct named in 3 separate
      syntactic positions (parameter annotation, variable declaration, constructor target),
      confirmed to fail without the fix (temporarily reverted `combine_resolved_operand_types`'s own
      call to prove it) and pass with it.
    - ~25 existing test sites across `ast_parser`'s `tests/{mod,use_block,big_test,functions}.rs`
      needed updating (a background agent's mechanical sweep, verified): full-`Stub`-equality
      assertions against a parsed/resolved value can never hold any more (a hand-built `Stub` always
      carries the `NodeId::ERROR` placeholder), converted to a new `Stub::matches_ignoring_occurrence`
      helper that checks `name`/`generics` only.
    - Proven by full `cargo test --workspace` (zero warnings, including the new test) + all 33 exe
      tests (`scripts/run_codegen_tests.py`) passing unchanged.
  - [x] **Slice 2** — `type_resolves: VecMap<TypeId, TypeResolve>` + the `TypeResolve` enum (all
        five variants defined now, only `Struct` ever produced this slice — the rest exist so
        matches stay exhaustive against the shape later slices populate) landed on `DeclareStore`,
        with `insert_type_resolve`/`get_type_resolve`.
    - **Mechanism settled via a `/grill-me` detour**: the obvious hook — `NameResolver::collect_type`,
      a pre-existing empty stub already wired into ~19 type-occurrence call sites — turned out
      *unsafe* to populate eagerly from: `collect_module` is a single linear pass in file order
      (confirmed by reading `collect_scopeless_block`), so a struct declared *after* the code
      referencing it wouldn't be registered yet, silently breaking forward references that work
      today (struct resolution normally happens later, in the resolve/typecheck phase, which only
      runs after collection *fully* finishes). User chose the narrower, safe path: populate
      `type_resolves` only at the two resolve-phase call sites that already resolve structs today
      (`struct_field_type`, `check_struct_constructor` in `resolve/typecheck/expression.rs`) —
      correct, zero regression risk, but **partial coverage by design**: a bare parameter/
      return-type annotation never used in a field-access or constructor expression anywhere in
      its function stays unresolved by this cache. `DeclareStore::resolve_struct` checks the cache
      first, falling back to the existing `module`-scoped name lookup when absent — so every
      existing caller (`mir_parser`, `mir_codegen`) transparently benefits without any call-site
      changes, and correctness never depends on coverage being complete.
    - Proven three ways: the existing struct-heavy exe tests (`09`–`11`, `25`, `26`, `28`) plus full
      `cargo test --workspace` (zero warnings); a new `ast_model::declare_store_tests` case
      (`resolve_struct_prefers_the_cached_resolution_over_the_name_lookup`) proving the cache
      actually wins over the name lookup even when they'd disagree — two different modules'
      same-named structs, the cache correctly picks the one that was actually cached rather than
      whichever the name lookup happens to find; a plain round-trip test for
      `insert_type_resolve`/`get_type_resolve`.
  - [x] **Slice 3** — `check_enum_variant_construction` (`resolve/typecheck/function_call.rs`) now
        interns its `owner_type` and populates `type_resolves` with `TypeResolve::Enum(node_id)`.
        Unlike the struct case, there was no existing duplicate-resolution problem to fix here (only
        one call site ever resolves an enum-typed `Stub` today) — this slice is purely "start
        populating the cache," not "migrate a consumer off a duplicated lookup."
    - Found and fixed a real gap while wiring this up: `owner_type`'s two production construction
      sites (`parse_owner_type`, `parse_owner_from_field_access`, both in `resolve/function_call.rs`)
      were still using the **test-fixture** `Stub::new(name)` (i.e. `occurrence: NodeId::ERROR`) —
      the Slice-1 test-fixing pass had correctly left these alone as "production code, don't touch,"
      but they genuinely needed the same fix production parsing sites already got: a real, unique
      occurrence, via `Stub::new_at(name, self.node_generator.alloc())` (`NameResolver` already
      carries its own `node_generator`, same allocator `collect_function` uses for `this`). Without
      this fix, every enum/struct name reached through this synthetic-owner-type path (`EnumName.
      Variant(..)`, `Std.Io.Stdout`-style module-qualified access) would have shared one placeholder
      `TypeId`, silently reintroducing Slice 1's cross-module collision bug for exactly this path.
    - Proven by the same full `cargo test --workspace` (zero warnings) + all 33 exe tests bar as
      every slice above (no new dedicated test added — the underlying `type_resolves`/
      `insert_type_resolve` mechanism is already covered at the `ast_model` level by Slice 2's own
      tests; this slice is a second, structurally identical producer of the same table).
  - [x] **Slice 4** — `check_impl_conformance` (`resolve/statement.rs`) now populates
        `type_resolves` with `TypeResolve::Trait(entry.node_id)`, keyed directly on
        `impl_block.impl_trait` (already the occurrence's own `TypeId` — no `get_type_id`
        re-derivation needed here, unlike the struct/enum cases). Same shape as Slice 3: only one
        call site ever resolves a trait-typed `Stub` today, so this is pure population, not a
        duplicate-lookup migration. Proven by the same full `cargo test --workspace` (zero
        warnings) + all 33 exe tests bar as every slice above (`26_trait_impl_dispatch.soul`
        exercises `check_impl_conformance` directly).
  - [x] **Slice 5** — `resolve_type_alias` (`resolve/typecheck/expression.rs`) gained `&mut self`
        (was `&self`) and now interns its final resolved type, recording
        `TypeResolve::Alias(resolved_id)` against the *original* occurrence's `TypeId` (recovered via
        `get_type_id`, same as `struct_field_type`) — but **only** when the alias chain actually
        resolved at least once (`resolved_any`); an occurrence that never named an alias is left
        uncached here, since it might still be a struct/enum/trait/generic that its own call site is
        responsible for caching — a no-op "alias" entry would have shadowed that. Proven by the same
        full `cargo test --workspace` (zero warnings) + all 33 exe tests bar as every slice above.
  - [ ] **Slice 6** — `TypeResolve::Generic`, `is_generic_parameter`/`generic_name_of` migrate.
  - [ ] **Slice 7** — hard error (`UnresolvedTypeName`) at resolve time for a `Stub` occurrence that
        resolves to none of the above, once all five kinds correctly populate `type_resolves` first.
- [x] `new(expr)` heap allocation (`*T`, an owning pointer — Rust's `Box<T>`), motivated by the
      user asking whether `*int` gets a real `dealloc` at `Drop` yet (it didn't — `Drop` is a pure
      no-op for every type right now). `/grill-me`'d and sequenced into three slices, since it
      touches parsing, resolution, a brand-new MIR shape, and codegen all at once:
  - [x] **Slice 1** — prerequisite correctness fixes, landed first since they were needed
        regardless of how `new` itself gets built:
    - Corrected 3 existing (unimplemented, forward-declared-only) intrinsics' doc comments —
      `array.toRaw`/`ptr.toSlice`/`ptr.offset` — from `*T` to `RawPtr<T>`. They're raw,
      non-owning pointer operations (a view into existing storage, or arithmetic on an existing
      pointer) — `*T` is reserved for an *owning* heap pointer, the very thing `new(expr)` will
      produce, and reusing it for these would make an owning and a non-owning pointer the exact
      same `SoulType`, indistinguishable to `is_auto_copy`/`Drop`.
    - `is_auto_copy` no longer treats `SoulType::Pointer` (`*T`) as `AutoCopy` — corrected to
      move-only, same as a struct. `&T`/`&mut T` (`SoulType::Reference`) is unaffected, still
      `AutoCopy` (a reference is always non-owning, unlike `*T`). Zero regression risk: nothing in
      the current pipeline constructs a `Pointer`-typed *value* anywhere (only the type-annotation
      parser path exists so far — confirmed by grep before landing), so this couldn't have
      affected any existing exe test.
    - Updated/split the existing `is_auto_copy_treats_primitives_references_and_pointers_as_
      autocopy` unit test into `..._primitives_and_references_as_autocopy` (unchanged behavior)
      and a new `is_auto_copy_treats_an_owning_pointer_as_move_only` (the corrected behavior).
  - [x] **Slice 2** — `new(expr)` allocates via `malloc` and produces a usable `*T`. Deliberately
        *not* freed yet — proving allocation/use works in isolation first.
    - Went in assuming `new(expr)` needed building as a new bare-callable intrinsic (like
      `assert`) — wrong assumption, caught by actually tracing why a first attempt silently failed
      to type `p := new(1)`: `ExpressionKind::New(ExpressionId)`/`NewArray(AnyArray)` already exist
      in the AST (parser, collect-pass, and resolve-pass dispatch all already handle them), grouped
      under `expression_type`'s own "not yet impl → `None`" bucket — the intrinsic-based attempt
      was reverted once this surfaced (dead code that never even matched, since `new(9)` never
      becomes a `FunctionCall` in the first place).
    - The one real gap: `expression_type`'s `New(_)` arm computed `*T` correctly when called
      directly, but never *persisted* it — `resolve_expression`'s own dispatch just recursed into
      the inner value without calling `insert_expression_type`. MIR lowering's `get_expression_type`
      only ever reads that persisted cache, never recomputes, so it silently saw no type at all.
      Fixed by adding `check_new_expression` (mirrors `check_binary_expression`'s own persist-at-
      resolve-time pattern) and calling it from `resolve_expression`'s `New` arm.
    - New `mir_model::Rvalue::HeapAlloc(Type, Operand)` — carries the element type explicitly
      (like `Cast`), since `mir_codegen` can't recover the pointee's size from context alone: the
      destination place's own LLVM type is just an opaque `ptr`. A plain `Rvalue`, not its own
      terminator — no OOM check (an unconditional `Assert` on a null result) is emitted yet, and
      unlike a real Soul function call there's nothing to branch on, so it doesn't need `Call`'s
      terminator shape.
    - `mir_codegen` declares `malloc(size_t) -> ptr` lazily, the same pattern as
      `abort`/`exit`/`printf`/`fflush`; `codegen_heap_alloc` computes the element's size via
      inkwell's own `BasicType::size_of()`.
    - Proven via a new `mir_parser` unit test (`new_expression_lowers_to_a_heap_alloc_rvalue`), a
      new resolver unit test (`variable_from_new_expression_can_have_its_dereferenced_value_used_
      in_a_binary_expression`), and a new exe test (`31_new_expression.soul`: `new(9)` then `*p`
      returns `9`) — the first real proof this allocates and round-trips correctly, not just that
      it constructs a plausible-looking shape.
  - [x] **Slice 3a** — fixed `return`'s own Move-awareness: `return p` (and the implicit-tail-return
        fallback, `p` as a block's last bare expression) now goes through `lower_movable_rvalue`
        (same as `x := p`/`x = p`), not raw `lower_rvalue` — previously scoped out of every earlier
        Move/MarkMoved slice, so `f(): *int { p := new(1); return p }` would MarkMoved never fire for
        `p`, and (once Slice 3b's free-at-Drop lands) `p` would get freed at the function's own end
        right after handing that same pointer back to the caller. `lower_movable_rvalue` bumped from
        private to `pub(super)` so `control_flow.rs` (a sibling module of the file it lives in,
        `statement.rs`) can call it. Proven via two new `mir_parser` unit tests (`returning_a_move_
        only_variable_moves_it_into_the_return_local`, `an_implicit_tail_return_of_a_move_only_
        variable_moves_it`); full workspace test suite and all 31 exe tests still pass unchanged,
        confirming this doesn't alter any existing AutoCopy-return behavior.
  - [x] **Slice 3b** — `Drop` actually calls `free()` for a `*T`, gated entirely at *compile time*
        (no runtime drop-flag storage needed, since move-checking is already static).
        `FunctionLowerer` gained a `moved: HashSet<LocalId>` tracked live *during* lowering
        (mirroring `move_check`'s own state-derivation logic, but inline instead of a separate
        post-pass): `move_variable_operand` inserts a local the moment it emits `MarkMoved`/
        `SetDropFlag(_, false)`, `push_assign` removes one on a whole-place reinitializing write
        (mirrors `move_check`'s own "reassign clears moved" rule), and `drop_chain` now skips any
        local still in that set instead of emitting a `Terminator::Drop` for it at all — otherwise
        a moved `*T` would free the same allocation twice once codegen started actually calling
        `free()`. Deliberately *not* saved/restored per `if`/`else` branch or reset between loop
        iterations: an outer-scope local moved on only one path stays flagged moved for whatever
        lowers next (including a sibling branch and code after the join), which can only cause a
        still-live value on some untaken path to be silently leaked, never double-freed — full
        per-path precision needs real dataflow (see M3's "extend borrow checker to generic MIR").
        `mir_codegen`'s `Terminator::Drop` arm now checks the dropped local's own type: `*T`
        (`SoulType::Pointer`) loads the pointer and calls `free()` (declared lazily via the same
        `declare_void_libc_fn` helper `abort`/`exit` already use); every other type is still a
        pure no-op scope-exit marker, unchanged. Proven via a new `mir_parser` unit test
        (`a_moved_body_local_is_excluded_from_its_own_scopes_drop_chain`) and a new exe test
        (`32_free_at_drop.soul`: a `new(7)` pointer that's read via `*p` but never moved or
        returned still frees cleanly at its own scope exit, exit code 7); full workspace test
        suite and all 32 exe tests pass, confirming the `return`-move-awareness fix from Slice 3a
        was in fact load-bearing for this (without it, `f(): *int { p := new(1); return p }` would
        double-free `p`).
    - The conditional-move imprecision this slice's docs call out (an outer-scope local moved on
      only one `if`/`else` branch leaks on the untaken path, never double-frees) is now itself
      proven, not just described: a new `mir_parser` unit test
      (`a_conditional_move_in_only_one_if_branch_leaks_on_the_untaken_path_but_never_double_frees`)
      lowers `consume(p: *int) {}; f(cond: bool) { p := new(1); if cond { consume(p) } }` and
      asserts `p` gets **no** `Drop` at all — confirming the accepted leak-not-crash tradeoff
      actually holds, and will catch it if a future dataflow-based fix changes this behavior.
    - Found and fixed a real bug while writing that test: `new(1)`'s bare literal argument was
      typing as `*untypedUint` (`1`'s own `UntypedUint` leaking straight through into `*T`
      unchanged), not `*int` — `new(expr)` has no declared target type for its argument to coerce
      against the way `x: i64 = 1` or an array literal's own element type do, so `check_new_
      expression` and `expression_type`'s `New` arm now both call `default_concrete_type` on
      `value`'s own type before wrapping it in `*T` (the same helper `array_literal_element_type`
      already uses for the exact same reason). Proven via a new `mir_parser` unit test
      (`new_expressions_bare_literal_argument_defaults_to_a_concrete_type`), confirmed to actually
      catch the bug by reverting the fix and re-running it (failed with `UntypedUint` vs `Int`, as
      expected). This also surfaced that `31_new_expression.soul`/`32_free_at_drop.soul` had been
      silently relying on the bug: once `new(...)`'s argument concretely defaults to `int` (not a
      coercible untyped literal), returning it through an `i32`-typed function is a genuine type
      mismatch — both exe tests now declare an explicit `n: i32 = ..` and pass `new(n)` instead of
      a bare literal, matching how every other exe test already threads a concrete type through.
  - [x] **Slice 3c** — fixed a real leak: an owning parameter (or by-value `this`) was never
        dropped at all, since `scopes` — the set of locals a function's own end-of-body `Drop`
        chain covers — deliberately excluded every parameter/receiver outright (correct back when
        `Drop` was a universal no-op, but not once Slice 3b made it actually call `free()` for a
        `*T`): `consume(v: *int) {}` took ownership of `v` and then simply never freed it on any
        call, a leak on every single call, not just the documented conditional-move edge case.
        `FunctionLowerer::lower` now pushes an owning (non-`AutoCopy`) parameter/by-value-receiver
        local onto `scopes[0]` (the function's own top-level frame) right after allocating it, the
        same frame body locals already live in — an `AutoCopy` parameter/receiver (a primitive, a
        `&T`/`&mut T`) still isn't tracked at all, unchanged. This also surfaced a latent, unrelated
        inconsistency in `push_assign`: it emitted `SetDropFlag(local, true)` for *any* write into a
        tracked local, including a partial field write (`p.x = 5`) — harmless while no parameter was
        ever tracked, but now double-counts a struct parameter's own field write as if it were a
        whole-place reinit. Fixed to match `moved`'s own existing whole-place-only rule (mirrors
        `move_check`'s "reassign clears moved"). Proven via a new `mir_parser` unit test
        (`an_owning_pointer_parameter_is_dropped_at_its_own_functions_end`) and a new exe test
        (`33_owning_parameter_dropped.soul`); full workspace suite and all 33 exe tests pass.

### M3 — unions, generics, full traits (not started)

- [ ] Generic functions/structs (`f<T>(x: T)`, `struct Box<T> { .. }`) and generic trait bounds
      (`Trait<T>`) — explicitly out of scope for M1's non-generic trait support (static dispatch
      only, no `Trait<T>`, see M1 above); needs monomorphization (below) to reach codegen at all
- [ ] Monomorphization (expansion to concrete types before LLVM)
- [ ] Full trait resolution incl. multi-impl-by-output-type-generic case (§7)
- [ ] `Res` / `.pass` / `?T` / match-chains
- [ ] Extend borrow checker to generic MIR with `AutoCopy` bounds
- [ ] `limit N` on `for` loops (blocked on `Res` error shape)
- [ ] `SwitchInt` discriminant handling for union tags (`Rvalue::Discriminant`)

### M4 — everything else (not sequenced)

- [ ] Swap the runtime allocator from raw libc `malloc`/`free` to **mimalloc**, statically linked,
      as a single cross-platform default (decided via /grill-me). One allocator on every target
      rather than a per-platform choice — a Windows/Linux/macOS split was considered and rejected:
      it multiplies the build matrix and debugging surface for a gain that's unmeasured (there's no
      non-Windows target to profile against yet, per `PlatformInfo::host()` below), whereas mimalloc
      already performs well on all three from the same codebase. This is orthogonal to, and lands
      independently of, the arena-allocation idea directly below — mimalloc is the fallback
      allocator for anything that isn't arena-eligible even after that lands. Not yet scoped: where
      the `malloc`/`free` calls actually live today (`mir_codegen::rvalue`/`terminator`, currently
      calling libc's `malloc`/`free` by name) and what the static-link step looks like per platform
      (mimalloc's CMake-built static lib needs to be vendored/fetched and linked alongside whatever
      `clang`/linker invocation each target uses; POSIX builds add a `pthreads` dependency Windows
      doesn't need).
- [ ] Arena allocation for non-escaping-from-allocating-frame values (idea, not designed — see
      [soul-lang.md §11](soul-lang.md#11-ownership--borrowing)). Deliberately narrowed from a full
      region-inference design (ML Kit-style, à la Tofte-Talpin regions) after grill-me: general
      cross-scope regions are a hard, failure-prone static analysis (a region can be kept alive
      indefinitely by one small still-referenced object), and a non-escaping, fixed-size value
      doesn't need heap allocation at all — it should just be a stack value. The scoped case that's
      actually worth it: a heap value (typically a growable one, e.g. a `List<T>` built and consumed
      locally) that's provably never returned, never moved into a longer-lived place, and never
      captured by an escaping closure/spawn from the function frame that allocated it. That's an
      extension of M2's own move-tracking proof obligations, not a new analysis — bump-allocate it
      out of a per-frame arena, bulk-free the arena at frame exit instead of an individual free.
      Expected payoff is real but narrow: bump allocation + one bulk free is meaningfully cheaper
      than per-value malloc/free on allocation-heavy hot paths (rough estimate: 10-30% there), but
      close to zero everywhere else — this is profiling-driven follow-up work, not a default.
      Explicitly out of scope: values escaping via return/longer-lived-container/capture (no
      arena — falls back to the ordinary allocator), and multi-frame/loop-iteration regions. Blocked
      on M2 (borrow/move checker) landing first.
- [ ] `extern "rust"` — calling into Rust directly, not just C (idea, not designed — see
      [soul-lang.md §11](soul-lang.md#11-ownership--borrowing)). The hope: since Soul's borrow
      checker uses the same strict aliasing rule as Rust's (exactly one `&mut` or any number of
      `&`), a Soul→Rust call could be checked against the callee's real `&`/`&mut`/owned signature
      instead of trusted blindly the way `extern "C"` has to be (C has no aliasing info to check
      against at all). Open, unscoped questions: whether Soul's and rustc's type/ABI
      representations are compatible enough to avoid a translation layer, how a generic or
      trait-object Rust signature maps to a Soul one, and whether the two checkers' rules are
      close enough to actually trust each other or just look similar.
- [ ] `async`/`await` + structured concurrency runtime — spec (§15) now also covers `task.block { }`,
      the sync-side blocking counterpart to `task { }` (plus brace-optional single-statement form
      for both), and disallows it from nested-async call sites; not implemented yet
- [ ] Const generics (`Limit<T, RANGE>`)
- [ ] Full reflection (`intrinsic.typeinfo`) — AST/compile-time design exists in
      [reflection-system.md](docs/reflection-system.md), backend (HIR/MIR/LLVM) not started
- [ ] `TypeId` interning for `any` (a runtime `void*` + `TypeId` payload) and `var.typeof == int`
      (requires `SoulType::Type` to carry a comparable `TypeId`). Scoped down via `/grill-me` from
      an original "single global `BiMap<TypeId, SoulType>`, flatten every recursive `SoulType`
      field (`ArrayType.of_type`, `Reference.inner`, `Optional`, etc.) to `TypeId`, used everywhere"
      plan:
  - Slice is `any`/`typeof` only for now — `trait_impls` (a comptime-built dispatch table also
    keyed by `TypeId`) is a separate, deferred follow-up, not part of this work.
  - `SoulType`'s internal recursive structure stays `Box<SoulType>` as-is; only boundary/leaf
    usages (an AST node's own type, an `any` payload, a `typeof` comparison target) get interned —
    not `ArrayType.of_type`/`Reference.inner`/etc. Full flattening was dropped: it would force every
    `SoulType`-constructing site across `soul_ast`/`soul_mir` (~30 files) to carry interner access,
    and there's no way to `Debug`-print a bare `TypeId` without a context parameter `fmt::Debug`
    can't carry.
  - Because monomorphized generics aren't implemented yet, there's no codegen-time type discovery
    to worry about: the interner can just grow as the parser/resolver encounter types — no
    incremental-during-codegen insertion, no risk of a runtime table being emitted before all
    `TypeId`s exist. Revisit this once generics/monomorphization land (`M3`) — that reopens the
    incremental-discovery problem this slice sidesteps.
  - [x] Foundation: `TypeId` (`impl_soul_ids!(TypeId)` in `ast_model/src/ast/soul_type.rs`, a plain
        monotonic `usize` like every other id type) plus `DeclareStore::{intern_type, get_type,
        get_type_id}` wrapping the existing `soul_utils::collections::bimap::BiMap<TypeId,
        SoulType>` (this collection already existed — no new interning mechanism had to be built).
        `SoulType` and its nested types (`TupleKind`, `ArrayType`, `ReferenceType`, `Stub`) gained
        `Eq`/`Hash` (plus `Mutable` in `soul_utils`) so they can key the `BiMap`; no other behavior
        changed. Proven via 5 `ast_model::declare_store_tests` cases (same-value interning dedupes,
        structurally-equal nested types built separately canonicalize to the same id, different
        types get different ids, `get_type`/`get_type_id` round-trip).
  - [x] Comptime `TypeId` constants for every context-free (parameterless) `SoulType` —
        `TypeId::{NONE, NEVER, STRING, FORMAT_STRING, ANY, TYPE, ERROR_TYPE}` plus one
        `TypeId::PRIM_*` per `PrimitiveTypes` variant (28 of them) — as literal `TypeId(N)` consts on
        `impl TypeId` in `soul_type.rs`. `DeclareStore::new` interns all 35 in that exact order via
        `register_well_known_types` (the *only* place the raw ordering is spelled out — a
        `debug_assert_eq!` per entry catches any drift between the hand-written constant and what
        `intern_type` actually returns immediately in any debug build/test run, since there's no way
        to derive matching literal `usize`s from a macro-repetition list without much more machinery
        than 35 straight-line entries warrant). Lets a hot comparison like "is this a `none`-returning
        function" become a plain `TypeId` equality (`signature.return_type == TypeId::NONE`) instead
        of `declares.get_type(signature.return_type) == Some(&SoulType::None)` — applied at the 3
        `mir_parser`/`DeclareStore`/`soul_name_resolver` call sites that were doing exactly that
        (`FunctionLowerer::lower`'s/`lower_extern_signature`'s/the call-lowering in
        `function/statement.rs`'s none-return checks skip resolving the `SoulType` entirely now
        unless it's actually needed; `DeclareStore::find_function`'s static-method-owner check;
        `is_main`). `DeclareStore` lost its `#[derive(Default)]` in favor of a manual
        `impl Default { fn default() { Self::new() } }`, so there's no construction path that skips
        registration. Proven by 2 new `declare_store_tests` cases (the well-known ids resolve back to
        their `SoulType`; interning a fresh equivalent `SoulType` returns the same constant) plus the
        same full `cargo test --workspace` + 30-exe-test bar as every field conversion above.
  - [x] `ast_parser::Parser` threaded a `&mut DeclareStore` (new `ParseInfo.declares` field, plumbed
        through both module-parsing entry points) so it can call `intern_type` at the exact point an
        AST node's type gets built — not a later pass, so the AST node's own field is genuinely
        `TypeId` from the moment it exists, never a transient `SoulType`.
  - [x] First real field converted end-to-end: `Parameter.ty` (`SoulType` -> `TypeId`) —
        `ast_parser`'s 3 construction sites now call `self.intern_type(..)`; every consumer
        (`soul_name_resolver::collect::statement`'s `collect_extern_signature`/`collect_function`,
        `soul_name_resolver::resolve::typecheck::function_call`'s argument-type checking,
        `mir_parser`'s `FunctionLowerer::lower` and `lower_extern_signature`, `soul_tester`'s AST
        display) now resolves the `TypeId` back to `&SoulType` via `declares.get_type` before using
        it. Proven by the full `cargo test --workspace` suite plus all 30 real exe tests
        (`scripts/run_codegen_tests.py`) still passing unchanged — this field is on the hot path for
        every function parameter in every one of those programs.
  - [x] `Variable.ty` (`Option<SoulType>` -> `Option<TypeId>`) — this is the one field shared by
        both plain variable declarations *and* struct fields (`Field.value` is a `Variable`), so it
        touched the widest consumer set so far: `ast_parser`'s single `parse_variable`/
        `try_parse_from_mut` interning sites; `soul_name_resolver`'s `collect_variable` (backfill vs.
        declared-type precedence — `declared_ty` is now resolved once and reused for both the
        `insert_variable_type` call and the `collect_type` call), `resolve_variable`'s explicit-
        annotation check, and `struct_field_type`/struct-constructor field-type-checking in
        `resolve/typecheck/expression.rs`; `mir_parser`'s `resolve_field_place`/`place_type` (struct
        field reads) and `mir_codegen`'s `resolve_place`/`stub_type` (struct field codegen +
        LLVM struct-type construction); `soul_tester`'s AST display for both a plain variable's type
        annotation and a struct field's. Proven by the same full `cargo test --workspace` +
        30-exe-test bar as `Parameter.ty` — struct field types are exercised by `09_struct_field_
        read.soul`/`10_struct_field_write.soul`/`11_nested_struct_field.soul` and others.
  - [x] `InnerFunctionSignature.{method_type, return_type}` (`SoulType` -> `TypeId`, both always
        present — no `Option`, `SoulType::None` is its own canonical interned entry for "no method
        type"/"no return type"). Widest consumer set yet, since a function signature is read from
        every layer of the pipeline: `ast_parser`'s 2 signature-construction sites
        (`inner_parse_function_signature`, the array-constructor synthesis in
        `parse_array_contructor`) plus the `This.()` ctor's `return_type` backpatch all now intern;
        `DeclareStore::find_function`'s owner-type matching now resolves through `get_type` instead
        of comparing `SoulType` directly; `soul_name_resolver`'s `collect_extern_signature`/
        `collect_function` (plus `is_main`, which needed a `&DeclareStore` parameter added since it's
        a free function, not a resolver method), `check_tail_return_type`/`check_return_statement`'s
        return-type checking, and `finish_call_resolution`'s call-return-type/argument-checking all
        resolve once per use; `mir_parser`'s `FunctionLowerer::lower` (receiver-type + return-type
        local allocation), `lower_extern_signature`, and the call-lowering in `function/statement.rs`
        all resolve via `self.declares`/an explicit `declares` parameter; `soul_tester`'s AST display.
        Proven by the same full `cargo test --workspace` + 30-exe-test bar as the two fields above —
        every single one of the 30 exe tests calls at least one function, so this field is exercised
        end-to-end by all of them, not just a subset.
  - [x] `TypeDef.{new_type, old_type}` (`SoulType` -> `TypeId`, `type X := Y`/`type X := distinct Y`)
        — smaller, well-contained surface: `ast_parser`'s single `parse_typedef` interning site;
        `soul_name_resolver`'s `collect_statement` (both types resolved once up front so the
        `!is_distinct` alias-registration branch, which still stores a real `SoulType` in
        `DeclareStore::type_aliases`, doesn't need a second lookup); `soul_tester`'s AST display.
        Proven by the same full `cargo test --workspace` + 30-exe-test bar as the fields above.
  - [x] `UseBlock.ty` (`SoulType` -> `TypeId`, `use Foo { .. }`'s own type). 3 `ast_parser`
        construction sites (`parse_use_block`'s two exit paths, `parse_impl_statement`) now intern;
        `soul_name_resolver`'s `collect_use_block` and `soul_tester`'s AST display resolve back.
        `ImplBlock.impl_trait` (also read by `collect_use_block`/`check_impl_conformance`) is a
        separate, still-`SoulType` field — deliberately not touched here. Proven by the same full
        `cargo test --workspace` + 30-exe-test bar as the fields above.
  - [x] `ImplBlock.impl_trait` (`SoulType` -> `TypeId`, `impl Trait { .. }`'s target). Single
        `ast_parser` interning site (`parse_impl_block`, shared by both its early-return and
        curly-brace exit paths); `soul_name_resolver`'s `check_impl_conformance` (the
        `let SoulType::Stub(stub) = &impl_block.impl_trait else { .. }` pattern became a `let
        Some(SoulType::Stub(stub)) = self.declares.get_type(..) else { .. }`, same shape) and
        `collect_use_block`; `soul_tester`'s `write_impl`. Proven by the same full
        `cargo test --workspace` + 30-exe-test bar as the fields above (`26_trait_impl_dispatch.soul`
        specifically exercises this field at runtime).
  - [x] `Enum.impl_type` (`Option<SoulType>` -> `Option<TypeId>`, `enum Foo: int { .. }`'s backing
        type). The smallest surface yet — no `soul_name_resolver`/`mir_parser` consumer reads this
        field at all today (it's parsed and stored but never type-checked or used for discriminant
        lowering; `union`'s own `Enum` value always passes `impl_type: None` and isn't affected).
        Only `ast_parser`'s `parse_enum` interning site and `soul_tester`'s `write_enum` needed
        changes. Proven by the same full `cargo test --workspace` + 30-exe-test bar as the fields
        above (none of the 30 exe tests use an enum backing type — this field genuinely isn't
        exercised by codegen yet, only by the parser test that round-trips it).
  - [x] `Trait.typedefs` (`Vec<SoulType>` -> `Vec<TypeId>`, `trait Foo { type Bar; .. }`'s associated
        type declarations) and `UnionKind::{Tuple, NamedTuple}`'s `parameters` (`Vec<SoulType>`/
        `Vec<(Ident, SoulType)>` -> `Vec<TypeId>`/`Vec<(Ident, TypeId)>`, a union/enum variant's
        payload types). `Trait.typedefs` has no resolver/mir consumer at all (parsed and displayed
        only, like `Enum.impl_type`); `UnionKind`'s parameters are read by
        `check_enum_variant_construction`/`enum_variant_name` in `resolve/typecheck/function_call.rs`
        (argument-type checking against a tuple-style variant's declared parameter types) and by
        `soul_tester`'s enum-variant display. `ast_parser`'s 3 interning sites: `parse_trait`'s
        `type Bar;` loop, `parse_enum_tuple_union`, `parse_enum_named_union`. Proven by the same full
        `cargo test --workspace` + 30-exe-test bar as the fields above.
  - [x] `StructConstructor.struct_type` (`Point{x:1}`'s own target type) and `Array`/`ArrayFiller`'s
        `element_type`/`collection_type` (`List.[1,2,3]`/`List.[for 3 => 0]`'s optional explicit
        element/collection types — despite the earlier wording here, there's no separate `Ref`
        struct with these fields; they live on `Array`/`ArrayFiller`, both variants of `AnyArray`).
        Widest single-pass consumer set of this whole series: `ast_parser`'s 4 interning sites
        (`parse_struct_contructor`, the wildcard-array branch and both exit paths of
        `parse_array`/`parse_array_filler`/`parse_array_literal`); `soul_name_resolver`'s
        `collect_struct_constructor`/`collect_any_array`, `check_struct_constructor` (the
        `let SoulType::Stub(stub) = &struct_constructor.struct_type` pattern became `let Some(
        SoulType::Stub(stub)) = self.declares.get_type(..)`, same shape as `check_impl_conformance`
        earlier), and `expression_type`'s `StructConstructor`/`Array` arms plus
        `array_literal_element_type`; `mir_parser`'s `lower_struct_constructor` (resolves
        `struct_type` once up front, same pattern as `Parameter.ty`); `soul_tester`'s
        `StructConstructor`/`Array`/`ArrayFiller` display. Proven by the same full
        `cargo test --workspace` + 30-exe-test bar as every field above —
        `25_ctor_and_methode.soul`/`28_untyped_struct_constructor.soul` exercise `struct_type`
        specifically; heap/wildcard arrays (where `element_type`/`collection_type` actually get used)
        aren't lowered by `mir_parser` yet, so those two fields are proven only at the parser/resolver
        level, same caveat as `Enum.impl_type`.
        This closes out every AST-node-attached `SoulType` field identified when this series started
        — `SoulType`'s own internal recursive fields (`ArrayType.of_type`, `Reference.inner`,
        `Optional`'s payload, etc.) remained intentionally un-interned at this point, per the scope
        decision at the top of this entry.
  - [x] The full flatten, reversing the scope decision above: every `SoulType`-internal recursive
        field (`ArrayType.of_type`, `ReferenceType.inner`, `RawPtr`'s/`Optional`'s/`ImplTrait`'s
        payload, `Res`'s `ok`/`err`, `Stub.generics`, `NamedVariant.base`, `Function.return_type`,
        `TupleKind`'s `Tuple`/`NamedTuple` element types) is `TypeId` now, not `Box<SoulType>`/
        `SoulType`. Also swept up two AST-node fields missed by the earlier series while here:
        `FunctionCall.generics` and the (never-actually-constructed) `FunctionCalleeKind::Type`.
    - The blocker flagged before starting — `SoulType`'s `Debug` impl recursing through owned
      `Box<SoulType>` fields with no way to resolve a bare `TypeId` — is real and was resolved by
      dropping the custom `Debug` (now `#[derive(Debug)]`, printing raw `TypeId`s, fine for internal
      debugging) and adding `ast_model::{PrintType, print_type}`: a `Display` wrapper carrying `&
      DeclareStore` alongside the `&SoulType`, resolving each nested `TypeId` as it recurses. Every
      site that used to do `format!("{ty:?}")` for a user-facing error/fault message — ~30 of them
      across `soul_name_resolver`'s typecheck modules, `mir_parser`, `mir_codegen` — now goes through
      a small `print_ty(&self, ty: &SoulType) -> String` helper (one per crate that needs it) wrapping
      `print_type`. Missing even one of these is a *silent* regression, not a compile error — a
      derived-`Debug` string like `Function { arity: 0, return_type: TypeId(18) }` is still valid
      `Box<str>`/`SharedStr`, so nothing fails to build; only fault-message-content assertions in
      tests (`soul_name_resolver`'s `lambda_type_tests`, checking `got.contains("-> int")`) caught it
      here (7 tests failed on exactly this before the sweep — grep for `format!("{` + a `:?}` type
      specifier is what actually found the rest, not the compiler). One further, non-`SoulType`
      instance of the same shape: `EnumVariantArgumentTypeMismatch` stored raw `SoulType` fields and
      formatted them in its own `Display` impl (in `ast_model::fault`, with no interner reachable
      either) — changed to pre-rendered `Box<str>` fields, computed at its one construction site
      (`resolve/typecheck/function_call.rs`) where `self.print_ty` is available, matching every other
      `AstErrorKind` variant's existing convention.
    - Every one of the ~30 direct `SoulType`-recursive-variant construction sites across
      `ast_parser`'s single type-parsing module (`parse/soul_type.rs`) now interns as it builds
      nested structure — `parse_tuple`/`parse_named_tuple`, the wrapper loop in `inner_parse_type`
      (`&`/`*`/`?`/array wrapping, interning the *previous* iteration's type before embedding it in
      the next), `parse_raw_ptr`/`parse_res`/`get_base_type`'s `impl Trait` arm. `parse_generic_define`
      (parses a `<T, U, ..>` argument list) now interns each argument as it's parsed and returns
      `Vec<TypeId>` directly, since every caller only ever embeds the result into another interned
      type or an AST node's own `generics` field — this single signature change is what surfaced
      `FunctionCall.generics`/`FunctionCalleeKind::Type` as needing the same conversion, since they're
      downstream of its return value.
    - `mir_parser::FunctionLowerer.declares` had to become `&'a mut DeclareStore` (previously `&'a
        DeclareStore`, since this crate — unlike `soul_name_resolver` — only ever *read* through the
        interner before): lowering a `&this`/`&mut this` receiver or a checked-arithmetic op's
        `(T, bool)` result tuple builds a brand-new `SoulType` on the fly that has to be interned to
        get a `TypeId` for the wrapping `ReferenceType`/`TupleKind`. `MirLowerer` (the outer driver)
        does *not* get its own separate `&mut DeclareStore` field — it would alias
        `FunctionLowerer`'s — instead exposing `FunctionLowerer::declares_mut()` for the one thing
        `MirLowerer` itself needs it for (`lower_extern_signature`, read-only). This required
        widening `&AstTree` to `&mut AstTree` up the whole MIR-lowering call chain:
        `mir_run::to_mir`, `soul_tester::main`'s own `fn mir`.
    - In `soul_name_resolver`, several previously-`&self` methods that build a fresh nested
        `SoulType` needed to become `&mut self` for the same reason (`expression_type`,
        `combine_operand_types`/`combine_resolved_operand_types`/`combine_array_types`,
        `get_owner_kind`, `first_lambda_return_type`, `array_literal_element_type`,
        `foreach_collection_element_type`) — traced transitively from every call site already being
        inside a `&mut self` resolve-phase method, so no further API widening was needed past this
        crate's own boundary, unlike `mir_parser`'s case above. One real aliasing hazard surfaced by
        this: `check_struct_constructor` held a `&Stub`/`&Struct` borrowed from `self.declares` across
        a later call to `check_struct_fields` (needing `&mut self`) — fixed by cloning the small
        `Stub`/`Struct` values before the call, same fix shape as `NameResolver.declares: &'a mut
        DeclareStore` (not `&'a DeclareStore`) requires everywhere a resolved reference is held across
        a later mutable call, unlike the read-only `mir_parser`/`mir_codegen`/`soul_tester` crates.
    - Two comptime `TypeId` constants got real use here for the first time: `literal_type`'s
        `Literal::Str` arm and a `RawPtr<int>`/`Res<int, str>` parser test both build a `SoulType`
        value using `TypeId::STRING`/`TypeId::PRIM_INT` directly instead of interning a fresh
        `SoulType::String`/`SoulType::Primitive(Int)` — the exact use case the constants exist for.
    - Proven by the same full `cargo test --workspace` (all crates, all green, including the 7
      lambda-message tests once the `print_ty` sweep was complete) + 30-exe-test bar as every
      preceding entry in this series.
  - [ ] Wire `any`'s runtime representation (`{ptr, TypeId}`) into `mir_parser`/`mir_codegen` (today
        `require_lowerable` rejects `SoulType::Any` outright).
  - [ ] `.typeof`/`==` comparison lowering. No syntax exists yet for "a type name used as a value"
        (e.g. the `int` in `var.typeof == int`) — needs a language-design decision, not just an
        implementation, before this can be parsed at all. `ExpressionKind::TypeOf` currently has
        zero `mir_parser`/`mir_codegen` lowering (resolver-only, typed as `SoulType::Type`).
- [ ] Operator overloading
- [ ] Goul

## Crate system (parallel track)

Tracked in detail in [crate-system-plan.md](docs/crate-system-plan.md) and
[ANCHORED-crate.md](docs/ANCHORED-crate.md). `CrateForest` is wired through parser/name-resolver
and the build is green (143 tests passing as of last update there).

- [x] `CrateId` + `Linkage` in `soul_utils`
- [x] `CrateForest` replacing flat `AstModuleStore` in `AstTree`
- [x] Parser routes modules via `forest`
- [x] Name resolver routes via `forest`
- [x] `ast_run` passes forest through
- [ ] `soul_tester` orchestration: dependency compilation, timestamp-based freshness check,
      `.soulo` rebuild
- [ ] `.soulo` prebuilt artifact: write/read the `SOULO` header + object bitcode format
- [ ] Wire `CrateForest.external` into name resolver for cross-crate name resolution
- [ ] Codegen separation using `ExternalCrateData` (static vs dynamic linkage)
</content>
