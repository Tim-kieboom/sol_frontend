# Sol Compiler — Roadmap / TODO

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
- [x] MIR shapes defined (`sol_mir/mir_model`) — see [mir-design.md](docs/mir-design.md)
- [x] MIR lowering: assignments, primitive checks
- [x] MIR lowering: if/for/break/continue
- [x] MIR lowering: comparisons and logical ops
- [x] MIR lowering: none-returning functions and calls
- [x] Bare assert/panic and undefined-fn checks
- [x] MIR display/serialization, fault plumbing
- [x] Struct name resolution via `DeclareStore.struct_names`, keyed by `ModuleId` for a fast
      bare-name-to-declaration lookup.
- [x] Struct field reads/writes/construction lowered through the whole pipeline, proven via real
      exes.
  - [x] Nested field chains (`o.inner.x`) resolve to one `Place` with a multi-step field
        projection.
- [x] Array literal construction, `&arr`-to-slice, and slice indexing (read+write) with bounds
      checking, scoped to fixed-size arrays and slices only (not wildcard-sized or heap arrays).
- [x] Bounds checking on slice indexing moved to the MIR level (new `Rvalue::Len`,
      `Terminator::Assert`) instead of ad hoc codegen-level branch-splitting.
- [x] Overflow checking on `+`/`-`/`*` at the MIR level via new `Rvalue::CheckedBinaryOp` and the
      matching LLVM `with.overflow` intrinsics.
- [x] Overflow/zero checking on `/`/`%` via explicit MIR-level `Assert`s ahead of the division,
      mirroring rustc's own checked-division lowering.
- [x] Rust-`panic!`-style panic runtime (message, no backtrace/unwinding) — every panic path is now
      a MIR `Terminator::Assert`, with `sol_panic` printing and aborting.
  - [x] Panic location (`file:line:col`) threaded through `Span` → `Terminator::Assert` → codegen,
        resolved via `ModuleStore`.
- [x] Non-generic trait support (static dispatch only) — conformance checking, ambiguity-aware
      method lookup, and (as a side effect) the first working receiver method call
      (`receiver.method(args)`) lowering to MIR.
- [x] Implicit ("trailing") return of a function body's tail expression, for both `=>` bodies and
      `{ }` block tails, including through an exhaustive `if`.
- [x] `sol_mir/mir_codegen` — LLVM IR emission via `inkwell` for scalars/pointers/structs,
      arithmetic/comparison/logical ops, if/while, calls, `extern "C"` functions, string/cstr
      constants, and `main` codegen.
  - [x] `f32`/`f64` arithmetic and comparisons (`f16` still unsupported).
- [x] Wired codegen output through to real `.exe`s via `scripts/run_codegen_tests.py` (clang + run +
      check exit code/stdout) — the pipeline's real correctness oracle, still not integrated into
      `cargo test`.
- [x] Audited `sol_name_resolver`'s test coverage of resolver/typecheck `AstErrorKind` variants;
      fixed 3 real gaps, removed 4 dead-code variants.
- [x] Fixed a crash on `x := Struct{field: value}` (untyped `:=` with a struct-constructor
      initializer) by filling in `expression_type`'s missing arms (`StructConstructor`, `Index`,
      `Unary`, `Ref`, `Array`, `TypeOf`, `Sizeof`).
- [x] Fixed a gap where `x: bool = 1`-style explicit-annotation mismatches went unchecked; added
      `check_variable_declaration`, plus two related `combine_resolved_operand_types` fixes
      (slice-mutability compatibility, untyped array-literal element coercion).
- [x] `PlatformInfo::host()` — a real (if still Windows-only) platform-selection mechanism replacing
      ad hoc `new_windows_x86_64()` calls everywhere.

### M2 — borrow/move checking (not started)

Scoped via `/grill-me`: "M2" is really a chain of prerequisite pieces, not one task —
`mir_parser` doesn't lower dereference places, doesn't fix `&this`/`&mut this` receivers to
actually borrow (they're still always a full by-value copy — a known M1 shortcut), doesn't emit
`Move`/`MarkMoved`/`SetDropFlag`/`Drop` anywhere, and `FunctionLowerer` has no lexical-scope
tracking at all (one flat locals map) — so "implement the borrow checker" is blocked on all of
that landing first, in order.

- [x] Deref places: `mir_parser` lowers explicit `*ptr` and auto-derefs through `&T`/`&mut T`/`*T`
      when resolving a field/index access, plus related resolver `expression_type`/
      `struct_field_type` fixes.
- [x] Fixed `&this`/`&mut this` receivers to actually pass a reference instead of a by-value copy.
- [x] Straight-line-only `Drop`/`SetDropFlag` lowering for body-declared locals (parameters/`this`/
      temps excluded), with drops emitted in reverse declaration order at function end.
- [x] `Move`/`MarkMoved` lowering for straight-line code (function-call args, struct/array-literal
      fields, plain reassignment/declarations) — `AutoCopy` types still copied, field/index/deref-
      projection sources still copied (no partial-move tracking yet).
  - [x] `is_auto_copy(SolType)` classification — primitives/references/pointers/slices are
        `AutoCopy`; owning arrays/structs are move-only.
  - [x] Function-call arguments moved (not copied) when the argument is a bare move-only variable.
  - [x] Plain reassignment and declaration initializers (`x = y`, `x := y`) move a bare move-only
        source the same way.
  - [x] Struct-constructor fields and array-literal elements move a bare move-only source the same
        way.
- [x] Scope-stack tracking in `FunctionLowerer` for `if`/`else` branches — a real
      `scopes: Vec<Vec<LocalId>>` stack, each branch dropped at its own join point; `return` unwinds
      the whole stack.
- [x] Extended scope-stack tracking to `for` bodies plus `break`/`continue` unwinding to the correct
      loop-boundary frame (partial-stack unwind via `loop_frame_index`).
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
    an independent background scan) all 8 struct-using `.sol` exe tests for a bare-variable
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
  - [x] Extended to `if`/`else`: a real dataflow pass over the built MIR CFG (reverse-postorder,
        union-join of "maybe moved" state at each join point) rather than folding precise tracking
        into the lowerer itself; only a "maybe moved" set is tracked (not a definite/maybe pair);
        `for` loops still skipped entirely (any back edge).
  - [x] Extended to `for` loops: replaced the back-edge skip with a real worklist fixed point
        (Kildall's algorithm) over the whole CFG including cycles; faults are only ever collected
        after the fixed point fully converges, never mid-convergence.
  - [x] Wired into the lowerer's own drop-chain decisions via a new `elaborate_drops` pass reusing
        `check_moves`'s dataflow: prunes an unconditional `Drop` to a no-op `Goto` wherever the
        local is maybe-moved by that point. Fixes a moved-taint leaking past an early return into
        unrelated code; a value conditionally moved on only one bodyless-`if` branch still leaks
        (never double-frees) on the untaken path — a known, still-open imprecision needing real
        per-edge drop elaboration (splitting control flow so the untaken path keeps its own `Drop`).
  - Both slices above complete "move-checking only" over the full concrete (M1) CFG shape
    (straight-line, `if`/`else`, `for`), now wired into real drop decisions too. What's left for M2:
    real borrow/lifetime-conflict analysis (do two live borrows of the same place overlap — a
    fundamentally bigger algorithm needing actual liveness tracking, nothing in the MIR records borrow
    lifetimes at all yet), and, separately, real per-edge drop elaboration (splitting control flow so
    a conditionally-moved value's untaken path keeps its own `Drop`, per the still-open case
    documented just above) — a bigger, CFG-restructuring follow-up to the prune-only pass that shipped
    here.
- [x] Dangling-reference ("escape") checking for straight-line functions: traces where a returned
      reference's storage actually comes from, backward through the MIR, to either a parameter
      (trusted safe, not verified at call sites) or a fresh `Ref` (safe only when its place derefs
      through existing memory — a reborrow or a heap-pointer deref; otherwise a local's own frame,
      flagged dangling). No `'a` lifetime syntax exists or is planned.
  - [x] Extended escape-checking to `if`/`else`: origin trusted safe only when safe on every
        incoming path (memoized fork-and-join backward trace, same back-edge gate reused from
        move-checking); `for` still gated out entirely.
  - [x] Mutable/shared-borrow overlap checking (straight-line only, whole-locals only,
        reference-vs-reference only — not move-vs-borrow, not per-field): real NLL-accurate
        liveness per local "generation," conflict on any `&mut` vs `&mut`/`&` range overlap, `&`
        vs `&` always fine.
  - [x] Extended overlap-checking to `if`/`else`: real two-level (block + intra-block) backward
        liveness dataflow plus join-aware alias tracing (reusing escape-checking's fork shape,
        widened to an equality check); overlap is live-point-**set** intersection, not interval
        overlap; `for` still gated out.
  - [ ] **Extend escape-checking to `for`** — the deferred half of the slice above: needs the
        backward trace to terminate on a revisited-but-not-yet-settled block (via a visited-blocks
        set defaulting to *unsafe* until proven otherwise, per the soundness guard already confirmed
        above but not yet exercised) instead of infinitely recursing around the loop's own back edge.
  - [ ] **Extend overlap-checking to `for`** — the deferred half of the slice above, same shape as
        escape-checking's own deferred `for` slice: needs the liveness dataflow to become a real
        fixed point (Kildall's algorithm, mirroring `move_check`'s own loop-support upgrade) instead
        of a single postorder pass, since a loop's back edge means a block's live-in can depend on a
        later block's own live-in (the loop body), which a single backward pass can't settle in one
        shot.
  - [ ] **Interprocedural call-site tracing for escape-checking** — the deferred half of the user's
        own original motivating example (`lifetime(obj: &Obj): &Obj { return obj }` called as
        `lifetime(&Obj{})`): today a reference-typed parameter is trusted as safe the moment
        `trace_origin` reaches it, without checking what any actual caller passed in. Needs tracing
        into a callee's own return-value dependency on its reference parameters, then checking each
        call site's actual arguments against that.
  - [ ] **Move-vs-borrow interaction** — a live borrow of a place should block a *move* out of it
        (rustc's `cannot move out of x because it is borrowed`); today `move_check` and
        `overlap_check` run as two fully independent passes with no shared state, so a move through
        an outstanding borrow isn't rejected by either.
  - [ ] **Per-field disjointness for overlap-checking** — `check_borrow_overlaps` currently treats two
        borrows of the same root local as conflicting regardless of which field each actually
        touches (`&o.a` and `&mut o.b` wrongly flagged as overlapping); needs the borrow-timeline
        generations to key on the full projection (place), not just the root `LocalId`.
- [ ] **Pipeline architecture cleanup** (`/grill-me`'d 2026-09-17, paused the borrow-checker's own
      "extend move-check to `if`/`for`" slice to do this first) — considered adding a HIR stage
      (`AST → HIR → MIR`) to fix a felt "MIR does too much" discomfort, then talked it back down:
      a HIR would cost a whole new crate + lowering pass + rewriting every `sol_name_resolver`/
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
    smell already precedented by the tail-return case (`sol_name_resolver`'s `check_tail_return_
    type` vs. `mir_parser` having to "re-derive the same convention independently", per the
    Implicit-return entry above): **convention drift** — the same rule reimplemented in two crates
    with no shared source, kept in sync only by comment. `mir_codegen`'s own `resolve_struct`
    (`types.rs`) admits this directly in its doc comment: *"mirrors `mir_parser`'s own
    `resolve_struct`"*.
  - [x] Part A1: moved pure, `DeclareStore`-independent classifiers (`PrimitiveTypes::{is_signed,
        is_float, signed_min}`, `BinaryOperatorKind::{is_supported, is_checked_arith,
        is_checked_div}`, `SolType::is_primitive`) into `ast_model`/`sol_utils`, replacing three
        independently-reimplemented copies.
  - [x] Part A2: moved `resolve_struct`/`is_lowerable`/`is_auto_copy` onto `DeclareStore` as pure
        classification methods; deliberately **no cache** (a `TypeId`-only cache would be unsound
        across same-named structs in different modules — `module` is passed explicitly instead).
  - [x] Part A3: moved `is_type_boolean`/`expression_is_bool` to `ExpressionId::is_boolean` in
        `ast_model` (the only crate both `mir_parser` and the resolver actually share).
  - [x] Part A4: new `SolType::deref_once` in `ast_model` replacing three independent copies of the
        same "is this `&T`/`&mut T`/`*T`, what's the inner type" match.
  - [x] Part A5: new `PlaceElem::step_type` in `mir_model` replacing duplicated per-step Sol-type
        derivation between `mir_parser::place_type` and `mir_codegen::resolve_place`'s
        `step_into_field`/`step_into_index`/`step_into_deref`.
  - [x] Part C: retired `mir_codegen::types::resolve_struct` (now routes through
        `DeclareStore::resolve_struct`) and added `ArrayKind::is_lowerable` in `ast_model`,
        replacing another duplicated accept/reject rule.
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
  - [x] A and C landed, B confirmed unnecessary — resumed the paused borrow-checker slice (see the
        "Extended to `if`/`else`" move-checking entry above).
- [x] Replaced `SolType::Stub` (the single, undifferentiated representation for struct/enum/trait/
      type-alias/generic-parameter names) with a `DeclareStore`-owned `type_resolves` side table
      mapping each syntactic occurrence to its resolved declaration, mirroring the existing
      `variable_resolves`/`function_resolves` pattern — the "bigger, root-cause redesign" logged
      earlier, done across 7 slices (plan: `C:\Users\tim_k\.claude\plans\vast-wiggling-galaxy.md`).
  - [x] Slice 1: `Stub` gained a per-occurrence `NodeId` participating in its own `Eq`/`Hash`,
        fixing a real cross-module name-collision bug; added
        `DeclareStore::{types_equal_ignoring_occurrence, sol_types_equal_ignoring_occurrence}` as a
        stopgap so existing "same name = same type" comparisons still work.
  - [x] Slice 2: added `type_resolves: VecMap<TypeId, TypeResolve>` (all 5 kinds defined, only
        `Struct` populated) at the two resolve-phase call sites that already resolve structs, with
        `DeclareStore::resolve_struct` checking the cache first — deliberately partial coverage,
        zero regression risk.
  - [x] Slice 3: `check_enum_variant_construction` now populates `type_resolves` with
        `TypeResolve::Enum`; also fixed a real gap where two `owner_type` construction sites still
        used test-fixture `Stub::new` instead of a real per-occurrence id.
  - [x] Slice 4: `check_impl_conformance` now populates `type_resolves` with `TypeResolve::Trait`.
  - [x] Slice 5: `resolve_type_alias` now populates `type_resolves` with `TypeResolve::Alias`, only
        when the alias chain actually resolved.
  - [x] Slice 6: `is_generic_parameter`/`generic_name_of` now populate `type_resolves` with
        `TypeResolve::Generic` (widest call-site touch of the series — 7 sites).
  - [x] Slice 7: turned two previously-silent "nothing matched" fallbacks
        (`check_struct_constructor`, `check_enum_variant_construction`) into a real
        `AstErrorKind::UndefinedType` error, closing out the series — `type_resolves` coverage
        remains deliberately partial.
- [x] `new(expr)` heap allocation (`*T`, an owning pointer) — parsed/typed/lowered/codegen'd across
      three slices, plus a real `free()` at `Drop` gated at compile time by move-checking.
  - [x] Slice 1: prerequisite fixes — corrected 3 intrinsics' doc comments from `*T` to `RawPtr<T>`,
        and made `is_auto_copy` treat `*T` as move-only (not `AutoCopy`) like a struct.
  - [x] Slice 2: `new(expr)` allocates via `malloc` and produces a usable `*T` (new
        `Rvalue::HeapAlloc`), not yet freed — proven via `31_new_expression.sol`.
  - [x] Slice 3a: fixed `return`'s own Move-awareness so a returned pointer is moved, not silently
        left flagged for a same-function free.
  - [x] Slice 3b: `Drop` actually calls `free()` for a `*T`, gated at compile time via
        move-checking so a moved pointer is never double-freed; a value conditionally moved on only
        one `if`/`else` branch still leaks (not double-frees) on the untaken path — a known,
        proven-via-test imprecision.
  - [x] Slice 3c: fixed a real leak — an owning parameter/by-value `this` was never dropped at all;
        now tracked in the function's own top-level scope frame like a body local.

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
      [sol-lang.md §11](sol-lang.md#11-ownership--borrowing)). Deliberately narrowed from a full
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
- [x] `extern "C"` variadic functions (`name: varargs` as an extern's last parameter, called as
      `f(fixed_args, varargs.[a, b, c])`) — parsed, type-checked (whitelisted element types), lowered
      to trailing MIR operands, and codegen'd with C's default argument promotions; fixed a
      prerequisite bug where `is_var_arg` was hardcoded false, silently corrupting real variadic
      calls like `printf`. Proven via `34_extern_variadic_printf_fixed.sol`,
      `35_extern_varargs_feature.sol`.
- [ ] `extern "rust"` — calling into Rust directly, not just C (idea, not designed — see
      [sol-lang.md §11](sol-lang.md#11-ownership--borrowing)). The hope: since Sol's borrow
      checker uses the same strict aliasing rule as Rust's (exactly one `&mut` or any number of
      `&`), a Sol→Rust call could be checked against the callee's real `&`/`&mut`/owned signature
      instead of trusted blindly the way `extern "C"` has to be (C has no aliasing info to check
      against at all). Open, unscoped questions: whether Sol's and rustc's type/ABI
      representations are compatible enough to avoid a translation layer, how a generic or
      trait-object Rust signature maps to a Sol one, and whether the two checkers' rules are
      close enough to actually trust each other or just look similar.
- [ ] `async`/`await` + structured concurrency runtime — spec (§15) now also covers `task.block { }`,
      the sync-side blocking counterpart to `task { }` (plus brace-optional single-statement form
      for both), and disallows it from nested-async call sites; not implemented yet
- [ ] Const generics (`Limit<T, RANGE>`)
- [ ] Full reflection (`intrinsic.typeinfo`) — AST/compile-time design exists in
      [reflection-system.md](docs/reflection-system.md), backend (HIR/MIR/LLVM) not started
- [ ] `TypeId` interning for `any` (a runtime `void*` + `TypeId` payload) and `var.typeof == int`
      (requires `SolType::Type` to carry a comparable `TypeId`). Scoped down via `/grill-me` from
      an original "single global `BiMap<TypeId, SolType>`, flatten every recursive `SolType`
      field (`ArrayType.of_type`, `Reference.inner`, `Optional`, etc.) to `TypeId`, used everywhere"
      plan:
  - Slice is `any`/`typeof` only for now — `trait_impls` (a comptime-built dispatch table also
    keyed by `TypeId`) is a separate, deferred follow-up, not part of this work.
  - `SolType`'s internal recursive structure stays `Box<SolType>` as-is; only boundary/leaf
    usages (an AST node's own type, an `any` payload, a `typeof` comparison target) get interned —
    not `ArrayType.of_type`/`Reference.inner`/etc. Full flattening was dropped: it would force every
    `SolType`-constructing site across `sol_ast`/`sol_mir` (~30 files) to carry interner access,
    and there's no way to `Debug`-print a bare `TypeId` without a context parameter `fmt::Debug`
    can't carry.
  - Because monomorphized generics aren't implemented yet, there's no codegen-time type discovery
    to worry about: the interner can just grow as the parser/resolver encounter types — no
    incremental-during-codegen insertion, no risk of a runtime table being emitted before all
    `TypeId`s exist. Revisit this once generics/monomorphization land (`M3`) — that reopens the
    incremental-discovery problem this slice sidesteps.
  - [x] Foundation: interned `TypeId` type plus `DeclareStore::{intern_type, get_type,
        get_type_id}` wrapping the existing `BiMap`; `SolType` and its nested types gained
        `Eq`/`Hash`.
  - [x] Added comptime `TypeId` constants for every context-free `SolType` (`NONE`, `NEVER`,
        `STRING`, primitives, etc.), registered in a fixed order with debug-assert-checked
        literals, letting hot type comparisons become plain `TypeId` equality.
  - [x] `ast_parser::Parser` threaded a `&mut DeclareStore` so it can intern a type the moment an
        AST node's type field is built.
  - [x] Converted `Parameter.ty` to `TypeId` end-to-end (parser interning site, every
        resolver/`mir_parser`/`mir_codegen` consumer resolving back via `get_type`).
  - [x] Converted `Variable.ty` to `TypeId` (shared by plain variable declarations and struct
        fields) across parser/resolver/`mir_parser`/`mir_codegen`/display.
  - [x] Converted `InnerFunctionSignature.{method_type, return_type}` to `TypeId` — the widest
        consumer set yet, since every layer reads a function signature.
  - [x] Converted `TypeDef.{new_type, old_type}` to `TypeId`.
  - [x] Converted `UseBlock.ty` to `TypeId`.
  - [x] Converted `ImplBlock.impl_trait` to `TypeId`.
  - [x] Converted `Enum.impl_type` to `TypeId` (smallest surface — no resolver/MIR consumer reads
        it yet).
  - [x] Converted `Trait.typedefs` and `UnionKind::{Tuple, NamedTuple}`'s `parameters` to `TypeId`.
  - [x] Converted `StructConstructor.struct_type` and `Array`/`ArrayFiller`'s
        `element_type`/`collection_type` to `TypeId` — closes out every AST-node-attached `SolType`
        field; only `SolType`'s own internal recursive fields remained un-interned at this point.
  - [x] Reversed the earlier "boundary-only" scope decision and did the full flatten: every
        `SolType`-internal recursive field (array/reference/pointer/optional/result/tuple element
        types, generics, etc.) is now `TypeId`. Dropped the custom `Debug` impl in favor of a
        `DeclareStore`-aware `PrintType`/`print_type` display wrapper, swept ~30 call sites off raw
        `{ty:?}` formatting, and widened `mir_parser`/`sol_name_resolver` to hold/thread
        `&mut DeclareStore` wherever a fresh nested `SolType` needs interning on the fly.
  - [ ] Wire `any`'s runtime representation (`{ptr, TypeId}`) into `mir_parser`/`mir_codegen` (today
        `require_lowerable` rejects `SolType::Any` outright).
  - [ ] `.typeof`/`==` comparison lowering. No syntax exists yet for "a type name used as a value"
        (e.g. the `int` in `var.typeof == int`) — needs a language-design decision, not just an
        implementation, before this can be parsed at all. `ExpressionKind::TypeOf` currently has
        zero `mir_parser`/`mir_codegen` lowering (resolver-only, typed as `SolType::Type`).
- [ ] Operator overloading
- [ ] Goul

## Crate system (parallel track)

Tracked in detail in [crate-system-plan.md](docs/crate-system-plan.md) and
[ANCHORED-crate.md](docs/ANCHORED-crate.md). `CrateForest` is wired through parser/name-resolver
and the build is green (143 tests passing as of last update there).

- [x] `CrateId` + `Linkage` in `sol_utils`
- [x] `CrateForest` replacing flat `AstModuleStore` in `AstTree`
- [x] Parser routes modules via `forest`
- [x] Name resolver routes via `forest`
- [x] `ast_run` passes forest through
- [ ] `sol_tester` orchestration: dependency compilation, timestamp-based freshness check,
      `.solo` rebuild
- [ ] `.solo` prebuilt artifact: write/read the `SOLO` header + object bitcode format
- [ ] Wire `CrateForest.external` into name resolver for cross-crate name resolution
- [ ] Codegen separation using `ExternalCrateData` (static vs dynamic linkage)
