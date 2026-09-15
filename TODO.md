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
- [ ] `Move`/`MarkMoved` lowering for straight-line code (function-call arguments,
      struct-constructor fields, array-literal elements, plain reassignment), now that
      `is_auto_copy` (below) exists — no opt-in mechanism for a struct to become `AutoCopy` yet (a
      non-generic trait can't express a marker bound), so this is still the concrete (M1) subset
      only.
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
    - Scope deliberately stopped at the classification itself, per the interview: no call site
      yet (marked `#[allow(dead_code)]`, pointing at this bullet) — wiring `Copy`/`Move` selection
      through it is this same bullet's own remaining (unchecked) work above.
    - Proven via unit tests driving a `FunctionLowerer` directly (no indirect call path exists
      yet): primitives/references/pointers/slices → `true`; a `StackArray` and a resolved struct
      (`Stub`) → `false`; a non-`require_lowerable` type → the same rejection.
- [ ] Scope-stack tracking in `FunctionLowerer` (currently one flat locals map, no concept of
      "which lexical block a local belongs to") plus `Drop` emission for every early-exit path
      (`break`/`continue`/`return`, in addition to normal fallthrough) through however many nested
      scopes each one jumps out of — needed before `if`/`for` bodies can be in scope for the
      `Drop`/move lowering above.
- [ ] Implement borrow checker as a MIR pass over the concrete (M1) subset

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
