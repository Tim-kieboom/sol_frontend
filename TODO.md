# Sol Compiler — Roadmap / TODO

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

### M2 — borrow/move checking (core checklist complete; deferred gaps remain)

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
  - [x] **Extend escape-checking to `for`** — the deferred half of the slice above. `/grill-me`'d at
        length before writing anything: the first design sketch (a per-block "in progress" marker
        defaulting straight to `Dangling` the moment a cycle was hit) needed a fallback span for the
        "no real local to blame yet" case — surfaced that neither `BasicBlock` nor `mir::Function`
        actually carry a span today, so that sketch would have meant a real MIR shape change (and,
        mid-interview, one more question in: that whole sketch turned out to be the *wrong* mechanism
        anyway, not just one needing an extra field).
    - **Reconsidered against `move_check`'s own precedent**: `move_check`'s loop extension didn't
      default pessimistically on a cycle hit — it ran a real fixed point, starting optimistic
      ("nothing moved"), converging monotonically toward the conservative verdict, with fault
      collection deliberately deferred until *after* convergence. Confirmed doing the same here
      instead: no synthetic fallback span needed at all, because every `Dangling` verdict that
      survives convergence still traces back to a real local — a loop header always has at least one
      predecessor *outside* the loop (the entry edge, never part of any cycle), which grounds the
      analysis in a genuine, non-circular answer.
    - **Worklist shape confirmed as a deliberate simplification, not an oversight**: `move_check`'s
      fixed point is a block-level Kildall worklist (cheap incremental reprocessing, since "maybe
      moved" is a monotonically-growing set with an obvious update rule). Escape-checking's
      `(LocalId, BlockId)` query space doesn't have an equally cheap incremental story — discovering
      which pairs matter needs the same demand-driven recursive trace this module already has. Chose
      this over a hand-rolled dependency-tracked worklist: **repeat the entire demand-driven trace
      once per round**, each round falling back to the *previous* round's settled answer for any pair still
      "in progress" on the current round's own call stack (round 0 falls back to `Safe`, the same
      optimistic starting point `move_check` uses), stopping once a round produces byte-for-byte the
      same table as the one before it. Confirmed sound (monotonic, so it can only ever move a verdict
      from `Safe` toward `Dangling`, never back — a finite descending lattice, guaranteed to settle)
      and accepted as a real efficiency-vs-simplicity trade, consistent with this compiler's own
      established preference throughout M2 (`move_check`'s own plain queue over a priority worklist).
    - New `converge` (`escape_check.rs`) replaces the old direct `trace_from_block_start` entry point:
      runs rounds, each building a fresh `this_round: HashMap<(LocalId, BlockId), Option<Origin>>` by
      tracing every `(return_local, return_block)` pair (a function can have more than one `Return`
      block, per the `if`/`else` slice above) against the *previous* round's table, until two
      consecutive rounds compare equal (`Origin` gained `PartialEq`/`Eq` for this). `trace_from_block_
      start`/`trace_within_block` gained an `in_progress: HashSet<(LocalId, BlockId)>` (this round's
      own call stack) alongside the existing `this_round` memo — a cache hit still short-circuits as
      before, an `in_progress` hit returns the previous round's settled answer instead of recursing
      forever, and only the *outermost* frame for a given key ever writes into `this_round` (an inner,
      cycle-detecting hit reads `previous_round` without touching `this_round` at all — the same
      "don't corrupt the round's own record with an in-flight guess" discipline `move_check`'s fixed
      point uses by only collecting faults in a separate, post-convergence pass).
    - `check_escapes` itself barely changed: still walks every `Terminator::Return` block, but now
      reads each one's origin out of `converge`'s settled table instead of calling the trace directly
      — the `has_back_edge` gate is gone entirely, since every CFG shape is analyzed uniformly now.
    - Proven via 3 new/rewritten `mir_parser` unit tests, chosen specifically to distinguish real
      convergence from both the old blanket-skip gate and a cruder "any cycle ⇒ reject" heuristic:
      `check_escapes_now_flags_a_dangling_return_reached_through_a_for_loop` (rewrites the old
      "still skips for loops" test — the same source now gets a real fault);
      `check_escapes_allows_a_value_reassigned_safely_inside_a_for_loop` — a loop present but nothing
      ever unsafe, proving this isn't just "any loop ⇒ reject" in disguise;
      `check_escapes_flags_a_value_that_only_dangles_via_the_loop_back_edge` — the falsifying case
      named up front: a value starts safe, is *conditionally* reassigned to a dangling loop-local
      inside the body, and resolving the loop header's own reaching value requires walking through the
      body's own bodyless-`if` join whose untaken path loops back to the header's own not-yet-settled
      value — a genuine self-referential dependency only real convergence (not a one-shot trace or a
      pessimistic cycle default) resolves correctly. All 3 passed on the first implementation attempt.
      Full `cargo test --workspace` (135 `mir_parser` tests, up from 133; the same unrelated
      pre-existing `ast_parser` failure noted earlier, unaffected) and all 35 exe tests pass unchanged.
  - [x] **Extend overlap-checking to `for`** — the deferred half of the slice above. `/grill-me`'d at
        length: unlike escape-checking's `for` slice (one fixed point, for one computation), this
        needed two *independent* fixed points, each scoped separately because they have different
        soundness profiles:
    - **Liveness** (`compute_block_liveness`): converted from a single postorder pass to a real
      block-level Kildall worklist, the same shape `move_check`'s own loop extension uses, just run
      backward (reprocess a block whenever a *successor's* `live_in` changes, by pushing that
      successor's own predecessors back onto the queue). Confirmed as a genuine precision
      requirement, not just style — a single non-iterating pass over a cyclic CFG would
      *under*-approximate liveness (a loop body's own "still needed" fact never propagates back
      through the header more than once), shrinking live ranges and only ever causing *missed*
      conflicts. That's technically sound under this codebase's own "prefer false negatives"
      discipline, but `move_check` and escape-checking's own loop extensions both chose the more
      rigorous fixed point anyway even where a cheaper sound-but-imprecise shortcut existed — matched
      that precedent rather than settling for less.
    - **Alias tracing** (`value_of`) surfaced a real design gap mid-interview: naively mirroring
      escape-checking's `converge` doesn't work here. Escape-checking's `Origin` lattice has an
      obvious optimistic starting value (`Safe`) a cyclic fallback can default to; `value_of`'s join
      instead collapses to `None` via `?` the instant *any* predecessor is unresolved, and a fresh
      cyclic reference has nothing to fall back to in round 0 except `None` — every later round would
      just read back the same `None` it wrote, "converging" instantly but recovering zero precision.
      Fixed with a third lattice state, `Resolved::Unknown`, distinct from both `Value(place, mutable)`
      and `Disagreement`: a join identity that's simply skipped over rather than treated as a
      disagreement, so an in-progress cyclic reference no longer poisons the result before it's had a
      chance to settle. Only `Disagreement` (two genuinely different real answers, or a value reaching
      an untracked parameter) is absorbing/final.
    - `converge_aliases` (round driver) + `resolve_from_block_start`/`resolve_before_index`
      (mirroring `escape_check`'s `trace_from_block_start`/`trace_within_block` pairing exactly in
      shape, join rule aside) replace the old direct `value_of`/`classify_rvalue`. A separate, final
      `classify_generation` reads the fully-settled table read-only (a fresh, throwaway
      `in_progress`/`this_round` pair — no real recursion happens there, since the settled table
      already has a direct answer for anything it could reach) to build the actual `borrows` list —
      same two-phase "compute the fixed point, then do a separate final pass" split every other M2
      fixed point in this codebase uses.
    - `has_back_edge`'s only two callers (`escape_check`, `overlap_check`) are both gone now, so the
      function itself was deleted from `move_check` rather than left as dead code.
    - Proven via 3 new/rewritten `mir_parser` unit tests, chosen to distinguish real analysis from the
      old blanket-skip gate: `check_borrow_overlaps_now_flags_an_overlap_reached_through_a_for_loop`
      (rewrites the old "still skips for loops" test — the same source now gets a real fault);
      `check_borrow_overlaps_allows_a_borrow_confined_to_one_loop_iteration` — a loop present but
      nothing ever conflicts, proving this isn't "any loop ⇒ reject" in disguise;
      `check_borrow_overlaps_flags_two_mutable_borrows_nested_inside_a_loop_body` — a pre-loop `&mut`
      borrow staying live across a conditionally-taken inner `if` that itself creates a second `&mut`
      borrow of the same place, a real nested-borrow conflict the old gate would have missed entirely
      (not analyzing a single statement of any `for`-containing function before this slice). Note: this
      last case, unlike escape-checking's own loop test, turned out resolvable via ordinary forward
      reachability within one pass through the body — a genuinely back-edge-load-bearing overlap
      example (one that a single-pass liveness computation would get *wrong*, not just "hadn't been
      tried yet") wasn't found by hand within this slice's own time budget; the Kildall worklist's
      correctness rests on the general dataflow argument in the module's own docs, not a test that
      isolates the back edge specifically. All 3 new tests passed on the first implementation attempt.
      Full `cargo test --workspace` (137 `mir_parser` tests, up from 135; same unrelated pre-existing
      `ast_parser` failure, unaffected) and all 35 exe tests pass unchanged.
  - [x] **Interprocedural call-site tracing for escape-checking** — the deferred half of the user's
        own original motivating example (`lifetime(obj: &Obj): &Obj { return obj }` called as
        `lifetime(&Obj{})`). `/grill-me`'d four separate times before writing anything — this turned
        out to be the largest single slice in this whole series:
    - **Mechanism confirmed**: extend the existing backward trace itself to recurse through
      `Terminator::Call` (consulting the callee's own summary, then tracing whichever argument(s) it
      ties to), rather than a separate, unconditional per-call-site check — the latter would flag a
      dangling result that's created and immediately discarded, never actually read, a real false
      positive this codebase's whole "prefer under- over over-reporting" discipline argues against.
      Matches escape-checking's own very first slice, which treated "chase through intermediate
      locals" as core scope from day one, not a narrowing.
    - **Call-graph cycles (recursion) confirmed to need real convergence**, same rigor level as every
      CFG-cycle choice already made this series — even though this compiler's resolver/lowerer/
      codegen are all already structurally recursion-safe, nothing has ever exercised it, so the
      call graph a real program builds could genuinely be cyclic (self- or mutual recursion).
    - **Reborrow-through-a-reference-parameter refinement included**, widening scope beyond the
      original ask: the pre-existing "any `Deref` in a `Ref`'s own place ⇒ unconditionally `Safe`"
      rule conflated two different cases — dereferencing an *owning* heap pointer (genuinely safe
      regardless of caller) and reborrowing through a *reference* parameter (only as safe as whatever
      the caller passed for *that* parameter, the exact same class of problem this whole slice is
      about). Scoped to `Deref` as the projection's own *first* element only (the two cases every
      existing test actually exercises, `&this.field` and `&*p`) — a `Deref` reached through a field
      first (e.g. `o.ptrField.innerField`) stays unconditionally `Safe`, an explicitly accepted,
      narrower gap no test exercises.
    - **Integration**: replaced `check_escapes`'s own machinery outright (its signature changed from
      one function to the whole program's `VecMap<FunctionId, mir::Function>`) rather than adding a
      parallel, duplicated `check_interprocedural_escapes` — the new call-aware trace is a strict
      superset of the old one, and every other refactor this series (`overlap_check`'s `analyze()`
      extraction, for instance) already leaned toward consolidating over duplicating. Touched all 13
      pre-existing `check_escapes` unit tests (each now wraps its one function in a `VecMap`) and
      `mir_run`'s own wiring (a single whole-program call, not a per-function loop) — the largest
      blast radius of any change in this series, confirmed explicitly before starting.
    - **Mechanism, concretely**: `Origin` gained `TiedToParams(HashSet<usize>)` (replacing the old
      flat `Safe` for "reached an unassigned parameter"); a new `combine(a, b)` join (`Dangling`
      absorbs, two `TiedToParams` sets union, `Safe` is the identity) generalizes the old boolean
      "require all paths safe" rule and is reused both at CFG-level branch points and to fold a
      function's own multiple `Return` blocks into one summary. `converge_summaries` is a *flat*
      outer fixed point over the whole call graph (round 0 = every function `Safe`; each round
      recomputes every function's own summary reading the *previous* round's table for any `Call` it
      routes through, including a self-call — no per-function in-progress recursion needed at this
      level, since it's a flat lookup, not a recursive descent into another function's own
      computation) — slower than a topologically-ordered DAG pass (~1 extra round per call-chain
      hop), simpler, the same trade already accepted for the CFG-level fixed point.
    - **A real off-by-one bug found and fixed while testing** (not by design review — the first
      version of every new test failed with zero faults instead of one): `LocalId` values start at 1,
      not 0 (`LocalId::ERROR` reserves 0), so the "is this local one of the function's own parameters"
      check needs `local.index() - 1` as the 0-based parameter index once `1 <= local.index() <=
      arg_count`, not a bare `local.index() < arg_count`. Caught immediately by the new tests
      resolving to `Safe` instead of `TiedToParams` — exactly the kind of bug a test-then-verify
      discipline exists to catch.
    - Proven via 4 new `mir_parser` unit tests (the core motivating example — a dangling temporary
      passed through a tied parameter; the same shape called safely instead; a genuine two-hop call
      chain, proving real transitive propagation through an intermediate function; self-recursion,
      proving the call-graph fixed point actually converges on a cyclic graph rather than hanging or
      guessing wrong) plus 1 new `mir_run` integration test. All 13 pre-existing `check_escapes` tests
      passed unchanged after the signature migration. Full `cargo test --workspace` (146 `mir_parser`
      tests, up from 137; 11 `mir_run` tests, up from 10; same unrelated pre-existing `ast_parser`
      failure, unaffected) and all 35 exe tests pass unchanged.
    - **This closes the fifth and final checklist item** this `/grill-me` series set as the working
      definition of "M2 borrow checker done" (see this section's own opening note) — all five are now
      done. What's left for *real* borrow/lifetime-conflict checking beyond that self-imposed bar:
      per-array-element (`Index`) disjointness for overlap-checking, and the narrower
      Deref-not-first-element gap this slice explicitly left in escape-checking's own reborrow
      refinement — both logged as deliberate deferrals, not gaps this pass found by surprise.
  - [x] **First exe-level "expected to fail to compile" test** — every borrow-checker fault up to this
        point was only ever proven by a MIR-level unit/integration test calling a checker function (or
        `mir_run::to_mir`) directly; nothing proved a rejection actually fires through the real CLI
        end-to-end. `scripts/run_codegen_tests.py` gained a `// expect_fail` file convention (skips the
        clang-build/run steps entirely; asserts `sol_tester` itself reported failure, plus an optional
        `// expect_fault: <substring>` check against its combined stdout/stderr) and a new test,
        `36_dangling_reference_rejected.sol`, reusing this session's own interprocedural
        dangling-reference motivating example.
    - **A real, previously-silent bug found and fixed along the way, not by design review**:
      `sol_tester::main`'s own `frontend()` returned `Ok(!ast_failed)` — a program with an AST-level
      pass but a *MIR*-level fault (e.g. any borrow-checker rejection) printed `"success"` to stdout
      regardless, since `mir_failed` (computed from the same, already-MIR-fault-inclusive
      `all_faults`) was silently never checked. Fixed to `Ok(!mir_failed)` — the correct combined
      signal, since AST faults are never removed from `all_faults` by the time MIR faults are folded
      in. This is what made a meaningful `// expect_fail` check possible at all (checking for
      `"success"` not appearing is now a real signal, not a coincidence).
    - **That fix immediately surfaced a second, real, previously-masked bug** in an *existing, passing*
      test: `26_trait_impl_dispatch.sol` started failing, because `mir_run`'s own lowering loop
      unconditionally called `lower_function` on *every* declared function id, including a trait's own
      bodyless interface method declarations (`greet(&this): i32` inside `trait Greeter { .. }`) —
      which `lower_function` itself, by design, hard-errors on (confirmed intentional: an existing
      unit test, `non_extern_signature_only_function_is_rejected`, asserts exactly this). Every
      trait-using program had silently carried this fault the whole time; it was invisible only
      because of the `Ok(!ast_failed)` bug above. Asked the user how to fix it rather than deciding
      unilaterally (three options: skip trait interfaces in the loop, downgrade the fault, or revert
      the `Ok` fix) — chose skip-in-the-loop: `mir_run::to_mir`'s own loop now checks
      `FunctionKind::Signature`'s `external` field *before* calling `lower_function` at all, skipping
      (not lowering, not faulting) exactly the case with `external: None` — structurally always a
      trait interface stub (the *only* other source of `FunctionKind::Signature` is an `extern "C"`
      declaration, always `external: Some(C)`, confirmed by checking both AST-parser construction
      sites). `lower_function` itself is unchanged, so its own existing rejection test for a bare
      direct call stays valid.
    - Proven by the full exe suite passing again (36/36, the previously-broken `26_trait_impl_
      dispatch.sol` fixed *and* the new `36_dangling_reference_rejected.sol` passing) plus the full
      `cargo test --workspace` run (zero regressions, same unrelated pre-existing `ast_parser`
      failure).
  - [x] **Move-vs-borrow interaction** — a live borrow of a place should block a *move* out of it
        (rustc's `cannot move out of x because it is borrowed`). `/grill-me`'d first: settled that
        checking "is place `x` moved while a borrow of `x` is still live" doesn't need real
        cross-module wiring with `move_check`'s own forward dataflow at all — `overlap_check` already
        computes, per generation, a live-point *set* for every tracked borrow of a place, so the whole
        check is a self-contained lookup against that existing machinery: for every whole-place
        `Operand::Move`, is its own point a member of any tracked borrow's `live_points`.
    - **Conflict matrix confirmed as asymmetric from `check_borrow_overlaps`'s own rule**: unlike
      overlap-checking (which only cares about mutable exclusivity, unlimited simultaneous shared
      borrows are fine), a move invalidates the underlying storage entirely — it conflicts with *any*
      live borrow of its target, mutable or shared alike, since a shared borrow read after the move
      would also be dereferencing gone storage.
    - **CFG scope confirmed as inherited, not restarted**: every other new M2 checker began
      straight-line-only and earned `if`/`else`/`for` slice by slice. This one skips that entirely —
      `overlap_check`'s own borrow/live-point data is already fully general (the `if`/`else` and `for`
      slices above), so the new check just reuses it as-is, with no separate CFG-shape scoping of its
      own.
    - `overlap_check.rs`'s `check_borrow_overlaps` refactored: its "build every tracked borrow" logic
      (previously inline) extracted into a shared `analyze(function) -> Analysis` (points-per-block +
      the borrows list), so a second consumer doesn't have to recompute or duplicate it.
    - `PointInfo` gained a `moves: Vec<LocalId>` field alongside `reads` — liveness only ever needs
      "was this local read here" (`Copy` and `Move` alike), but this new check specifically needs
      *which* reads are `Move`s; new `collect_operand_moves`/`collect_moves` (mirroring
      `collect_operand_reads`/`collect_reads`'s own shape) populate it, whole-locals-only (a
      projected `Move`, e.g. `consume(container.item)`, isn't tracked — same scope cut every other
      M2 pass already makes, and the lowerer doesn't emit those as `Operand::Move` in the first place
      per `move_eligible_operand`'s own docs).
    - New `pub fn check_move_while_borrowed`: for every point's own `moves`, looks up tracked borrows
      of that same root local and flags one whose `live_points` contains this exact point. New
      `MirErrorKind::MoveWhileBorrowed`, reported at the moved local's own declaration span (same
      convention as `UseAfterMove`/`DanglingReference`). Wired into `mir_run::to_mir` right alongside
      `check_borrow_overlaps`. In passing, fixed two other `MirErrorKind` doc comments
      (`UseAfterMove`/`DanglingReference`) that still said "straight-line functions only" — stale
      since both checkers' own `if`/`for` extensions above.
    - Proven via 2 new `mir_parser` unit tests (a shared borrow flagged when its target is moved while
      still needed; the same shape accepted once the move comes strictly after the borrow's own last
      use — proving real liveness, not a lexical "the borrow variable is still in scope" heuristic)
      plus 1 new `mir_run` integration test exercising the real `to_mir` wiring. Full
      `cargo test --workspace` (139 `mir_parser` tests, up from 137; 10 `mir_run` tests, up from 9;
      same unrelated pre-existing `ast_parser` failure, unaffected) and all 35 exe tests pass
      unchanged.
  - [x] **Per-field disjointness for overlap-checking** — `check_borrow_overlaps` no longer treats
        every borrow of the same root local as conflicting regardless of field. `/grill-me`'d first,
        two scope cuts settled before writing anything:
    - **Field projections only** — `arr[i]`/`arr[j]` still coarsen to "same root ⇒ conflict," deferred
      to its own later slice: an index is always a runtime local in this MIR, never a compile-time
      constant (per the bounds-checking design), so there's no sound way to prove two array-element
      borrows disjoint without real symbolic range analysis.
    - **A prefix relationship is always overlapping** — `&o` (the whole struct) and `&mut o.a` (one of
      its fields) still conflict exactly as before; only two *genuinely disjoint* field paths (differing
      at some `Field` index, neither a prefix of the other) are newly accepted. Confirmed this is the
      correct default direction *opposite* every other "unfamiliar shape ⇒ skip, don't guess" rule in
      this module (which defaults to *not* reporting): here, "can't prove disjoint" has to still mean
      "conflict," since that's what the code already did before this slice — the refinement can only
      *narrow* the set of flagged conflicts relative to that baseline, never introduce a new missed one.
    - `PlaceElem` (`mir_model`) gained `PartialEq`/`Eq` (previously only `Debug`/`Clone`) — needed to
      compare two borrows' field projections at all. `Borrow` gained a `projection: Vec<PlaceElem>`
      field alongside its existing root-`LocalId` `place`; new `fields_disjoint` walks two projections
      in lockstep, the first differing `Field` index proving disjointness, either side ending first
      (a prefix) or an `Index`/`Deref` step anywhere falling back to "not proven disjoint." Wired into
      `check_borrow_overlaps`'s own pairwise loop as one more early-continue guard, alongside the
      existing mutability and live-point-overlap checks.
    - `Resolved::Value` (the alias-tracing lattice's own "real answer" state) now carries the borrowed
      place's projection too, not just its root local — otherwise two branches resolving to different
      fields of the same root would wrongly agree at a join. Since `Vec<PlaceElem>` isn't `Copy`,
      `Resolved` dropped its own `Copy` derive (kept `Clone`); the handful of call sites that relied on
      copying it (`this_round.get(&key)`/`previous_round.get(&key)` lookups, the settled result written
      back after `resolve_before_index` returns) switched to explicit `.clone()`/`.cloned()`.
    - `check_move_while_borrowed` needed **no changes at all**: a tracked move's own place always has
      an *empty* projection (whole-locals only, already true before this slice) — an empty projection
      is a prefix of every other place sharing its root, so a move of `o` already, correctly, conflicts
      with a borrow of any of `o`'s individual fields, with no new logic required.
    - Proven via 3 new `mir_parser` unit tests: `check_borrow_overlaps_allows_two_mutable_borrows_of_
      disjoint_fields` (the core new-acceptance case — two live `&mut` borrows of different fields of
      the same struct), `check_borrow_overlaps_flags_two_mutable_borrows_of_the_same_field` (proves
      the refinement isn't over-broad — the same field still conflicts), and `check_borrow_overlaps_
      flags_a_whole_struct_borrow_overlapping_a_field_borrow` (the prefix-relationship half). All 3
      passed on the first implementation attempt; all 9 pre-existing `check_borrow_overlaps` tests and
      both `check_move_while_borrowed` tests passed unchanged. Full `cargo test --workspace`
      (142 `mir_parser` tests, up from 139; same unrelated pre-existing `ast_parser` failure,
      unaffected) and all 35 exe tests pass unchanged.
    - **Closes out 4 of the 5 checklist items this `/grill-me` series set as the working definition of
      "M2 borrow checker done"** (see this section's own opening note) — only interprocedural
      call-site tracing for escape-checking remains open from that list. What's left beyond that
      self-imposed bar: that interprocedural tracing, plus per-array-element (`Index`) disjointness for
      overlap-checking — both already logged above as their own explicit deferrals, not new gaps this
      pass found.
  - [x] **Real per-edge drop elaboration for `*T`** — after the original 5-item checklist finished
        (see the opening note above), `/grill-me`'d "what's next" among the deliberate deferrals still
        open (`Index` disjointness, the Deref-not-first-element reborrow gap, and this) and picked this
        one as the biggest correctness gap: a value moved on only one `if`/`else` branch still leaked
        (never double-freed) on the untaken path, since `elaborate_drops` only ever had a single "maybe
        moved" set to work with — moved-on-any-path and moved-on-every-path looked identical to it, so
        it had to prune the shared `Drop` outright the moment *either* was true.
    - **Mechanism corrected mid-interview**: the obvious-sounding fix ("split control flow so the
      untaken path gets its own `Drop`") turned out to be the wrong shape entirely. Checked
      `mir_codegen`'s own `SetDropFlag` handling first (`mir_codegen/src/function.rs`, pre-this-slice):
      it codegen'd to nothing at all — "Move/drop tracking has no runtime effect yet." Since the MIR
      already carries `SetDropFlag(local, true/false)` statements at exactly the right points (every
      whole-place reinit and every bare-variable move, regardless of type), the real fix is a *runtime*
      drop flag: keep one `Drop` site, but gate the actual `free()` on a runtime check instead of
      statically pruning or duplicating the site. Confirmed against Rust's own actual solution to this
      exact problem, which also uses a runtime flag, not CFG splitting.
    - **Fast path confirmed kept**: `elaborate_drops` still statically prunes to a no-op `Goto` when a
      local is moved on *every* incoming path (the cheap, common case), and leaves a `Drop` plain and
      unconditional when it's *never* moved on any path (no runtime check needed at all) — the new
      runtime-flag machinery only engages for the genuinely ambiguous "moved on some but not all paths"
      case, not universally.
    - **New prerequisite surfaced and built**: `move_check`'s dataflow only tracked one "maybe moved"
      set before this — no way to distinguish "moved on any path" from "moved on every path" existed at
      all. Extended to a definite/maybe pair: "maybe" joins via union (unchanged, still what
      `check_moves` reads for use-after-move); "definite" joins via *intersection*, starting from the
      universal set (every local in the function) as the identity for an as-yet-unprocessed predecessor,
      mirroring "maybe"'s empty-set identity — this is what lets the fixed point converge to the correct
      answer instead of transiently overclaiming something as definite before all its predecessors are
      known. Within one block's own straight-line statements the two sets move in lockstep (a move
      inserts into both, a whole-place reinit clears both); they only diverge at a CFG join, via the two
      different join rules.
    - **Type scope narrowed twice during the interview**: first to "owned `*T` and `[]T`" (the two
      heap-owning types), then `[]T` was dropped entirely after checking `mir_codegen/src/types.rs`
      directly — `ArrayKind::HeapArray` is rejected by `is_lowerable` outright
      (`unreachable!("already rejected above by is_lowerable")`), so `[]T` has no runtime behavior to
      fix yet and no way to even test it. Final scope: `*T` only. A struct/array that merely *owns* a
      `*T` field (recursive drop) is also out of scope — it keeps leaking on the untaken path exactly as
      before, an explicit, agreed-to deferral, not a gap found by surprise.
    - `mir_model::Terminator::Drop` gained a `guarded: bool` field. `mir_parser::function`'s own
      lowering always emits `guarded: false`; `elaborate_drops` is what sets it, reading both the
      "definite" and "maybe" sets to choose between pruning to `Goto`, leaving it unconditional, or
      setting `guarded: true`.
    - `mir_codegen::FunctionCodegen` gained `drop_flags: VecMap<LocalId, PointerValue>` — one `i1`
      alloca per `*T` local, seeded `true` right after allocation (needed because an owning *parameter*
      never gets an explicit `SetDropFlag` from the lowerer at all — `push_assign` is only ever called
      for a user-written assignment, not parameter binding — so this is what keeps a guarded `Drop` of an
      untouched owning parameter from reading uninitialized alloca contents). `Statement::SetDropFlag`
      now actually stores into this alloca (a no-op for any local without one, i.e. anything not `*T`,
      matching `codegen_drop`'s own type gate); a `guarded` `Drop` loads the flag and conditionally
      branches to a small extra block that calls `codegen_drop` before rejoining the original target,
      versus branching straight past it when the flag is false.
    - Proven via one rewritten `mir_parser` unit test (`a_conditional_move_in_only_one_if_branch_no_
      longer_leaks_on_the_untaken_path` — previously asserted the leak as accepted behavior; now asserts
      the `Drop` survives with `guarded: true`) plus two new exe-level tests exercising the actual
      codegen path at runtime on both outcomes: `37_conditional_drop_guarded_moved.sol` (flag ends up
      false, proving no double free) and `38_conditional_drop_guarded_not_moved.sol` (flag stays true,
      proving the real free still runs on the untaken-in-the-old-code path). Full
      `cargo test --workspace` (same 146 `mir_parser` tests, one rewritten not added; same unrelated
      pre-existing `ast_parser` failure) and all 38 exe tests (up from 36) pass.
    - **Follow-up fix, caught by inspecting the actual LLVM IR, not by review**: the first version of
      this allocated a drop-flag alloca (plus its seeding `store`) for *every* `*T` local unconditionally
      — including in a function like `consume(p: *int) {}`, whose own `Drop` of `p` is never `guarded` at
      all (it's a single, unconditional scope exit), so the flag was written once and never read. Fixed
      by scanning `function.blocks` up front for which locals actually have a `guarded: true` `Drop`
      anywhere in that function (a `VecSet<LocalId>`, `guarded_locals`) and only allocating flag storage
      for those — restores the "fast path has zero overhead" property the design was supposed to have.
    - Still explicitly open after this: the struct/array-owning-a-`*T`-field case just deferred above,
      plus the two older deferrals (`Index` disjointness for overlap-checking, the Deref-not-first-
      element reborrow gap) — none touched by this slice.
    - [ ] **Future optimization, not scoped**: critical-edge splitting as a flag-free alternative for the
          narrow case where it actually pays off — an *acyclic* join with exactly *one* conditionally-
          moved value reaching it (this slice's own motivating example) can drop the runtime flag
          entirely by giving each incoming edge its own small dedicated block that does or skips the
          `Drop` before rejoining the shared continuation, rather than one shared guarded `Drop` reading
          a flag. Confirmed *not* a general replacement for the flag mechanism during a follow-up
          `/grill-me`: it stops being free the moment either (a) more than one conditionally-moved local
          reaches the same join (edges would need splitting per *combination* of drop obligations —
          2 values ⇒ 4 edge variants, 3 ⇒ 8) or (b) the join is reached through a loop back-edge (there's
          no static edge count to split against "how many times around the loop"). Mirrors why rustc's
          own `ElaborateDrops` isn't pure edge-splitting either: it runs the same definite/maybe dataflow
          this slice just built, elaborates for free wherever the answer is definite, and only falls back
          to a runtime flag for the genuinely ambiguous cases — edge-splitting would only ever be a size/
          speed micro-optimization layered on top of that fallback for its one narrow acyclic single-
          value case, not a replacement for it.
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
