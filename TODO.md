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

### M2 — borrow/move checking (complete — no known soundness holes; logged leaks and deferrals remain)

M2 built the borrow/move checker as a chain of prerequisite pieces (deref places, real `&this`/
`&mut this` borrowing, `Move`/`Drop` lowering, lexical scope tracking), then three checkers, each
grown straight-line → `if`/`else` → `for` (fixed-point dataflow, Kildall-style, once loops made the
CFG cyclic) before being widened to close every known soundness hole:

- [x] **Prerequisites**: deref-place lowering (explicit `*ptr` + auto-deref through `&T`/`&mut T`/
      `*T`); real `&this`/`&mut this` borrowing (was a by-value copy); straight-line `Drop`/
      `SetDropFlag`/`Move`/`MarkMoved` lowering (`is_auto_copy` classification: primitives/refs/
      pointers/slices copy, owning arrays/structs move); `FunctionLowerer` scope-stack tracking
      (`if`/`else` join points, `for` + `break`/`continue` unwinding to the right loop frame).
- [x] **Move checker** (`move_check`) — flags use-after-move. Deliberately mirrors rustc: real
      flow-sensitive dataflow, not a flow-insensitive "moved anywhere ⇒ reject" scan. Grown
      straight-line → `if`/`else` (union-join "maybe moved") → `for` (Kildall fixed point); wired
      into `mir_run::to_mir` as a hard error. `elaborate_drops` reuses the same dataflow to prune
      dead `Drop`s. Later extended to a definite/maybe pair (intersection-join for "definite") so a
      value moved on only *one* branch gets a **runtime drop flag** (`Terminator::Drop::guarded`,
      one `i1` alloca per `*T` local) instead of leaking or double-freeing on the untaken path — the
      same mechanism rustc itself uses, not CFG/edge-splitting. Also gained **partial-move**
      tracking (`b.p` moved out of struct `b` leaves disjoint sibling fields usable; moving out from
      behind a reference/owning pointer/index is rejected as `MoveOutOfBorrow`).
- [x] **Escape checker** (`escape_check`) — flags a returned reference that dangles. Traces a
      reference's origin back to a parameter (trusted) or fresh memory (safe only through an
      existing allocation — a reborrow or heap-pointer deref). Grown straight-line → `if`/`else` →
      `for` → **interprocedural** (a call's return traced through the callee's own summary, with
      call-graph recursion handled by the same fixed-point convergence) → struct-field reborrow
      tracing. Finally **rewritten wholesale as a forward points-to dataflow** (abstract memory
      locations, not syntactic paths, so slice/field aliasing is precise) to close every remaining
      "unrecognized shape defaults to `Safe`" soundness hole, including owning-pointer dereferences
      and writes through aliases/`&mut` callee parameters (must/may-write summaries). **No known
      soundness hole remains**; only accepted false positives (an extern handed `&mut`, a write
      through a call's returned reference, `[*]`/recursive-field weak updates) are left.
- [x] **Overlap checker** (`overlap_check`) — flags conflicting live borrows of the same place.
      Started as NLL-style liveness per local "generation" (straight-line → `if`/`else` → `for`),
      then widened to: move-vs-borrow conflicts (a move invalidates storage, conflicts with *any*
      live borrow); per-field disjointness (`&mut o.a` vs `&mut o.b` accepted, any prefix
      relationship still conflicts); and finally a full rewrite to **loan-based (NLL-style)
      conflicts** sharing the escape checker's points-to engine (a `Ref` creates a loan that rides
      through every derived reference; conflict is "access to memory overlapping a live loan except
      through that loan, unless both sides are shared") — needed because honest ref-vs-ref checking
      rejected ordinary reborrow patterns like `this.inner.grow()` then `this.count = 1`.
- [x] **Verification infrastructure**: an exe-level `// expect_fail` test convention (proved a
      rejection fires through the real CLI, not just a MIR-level unit test) — building it surfaced
      and fixed two real bugs: `sol_tester` reported "success" even on a MIR-level fault, and that
      fix then exposed a pre-existing fault in every trait-using program (fixed by skipping
      bodyless trait interface stubs in `mir_run`'s lowering loop instead of erroring on them).
- [x] **Pipeline architecture cleanup** — considered a HIR stage to fix a "MIR does too much"
      feeling, rejected it (real issue was *where* logic lives, not a missing desugared tree).
      Moved flow-insensitive classifiers (`is_auto_copy`, `is_lowerable`, `resolve_struct`,
      `deref_once`, `PlaceElem::step_type`, etc.) out of duplicated per-crate copies and onto
      `DeclareStore`/`ast_model`/`mir_model` as the single source of truth; flow-sensitive logic
      (move-checking, drop/scope tracking) stays in `mir_parser`. A planned "relocate the
      `foreach`/`while` desugaring" part turned out to have no such desugaring to relocate yet
      (`Foreach` isn't lowered at all) — moved to M3 since real `Foreach` lowering needs `Iterator`,
      which is bound to traits.
- [x] Replaced `SolType::Stub` with a `DeclareStore`-owned `type_resolves` side table mapping each
      syntactic type occurrence to its resolved declaration (struct/enum/trait/alias/generic),
      mirroring the existing `variable_resolves`/`function_resolves` pattern; done across 7 slices,
      closing two previously-silent "nothing matched" fallbacks into real `UndefinedType` errors.
- [x] `new(expr)` heap allocation (`*T`, an owning pointer): parsed/typed/lowered/codegen'd via
      `malloc`, with a real `free()` at `Drop` gated at compile time by move-checking, and two
      real leaks fixed along the way (a returned pointer wasn't Move-aware; an owning parameter/
      by-value `this` was never dropped at all).

**Exit state**: no known soundness hole in any of the three checkers. Remaining deferrals were
deliberately moved out rather than left as stray checkboxes: per-array-element (`Index`)
disjointness for overlap-checking and a `mir_codegen` performance audit are in M4 as accepted,
non-blocking limitations; critical-edge splitting (a drop-flag optimization) is in M4 pending the
full pipeline; and the `foreach`/`while` desugaring question is in M3 pending `Iterator`/traits.

### M3 — unions, generics, full traits (not started)

- [ ] Generic functions/structs (`f<T>(x: T)`, `struct Box<T> { .. }`) and generic trait bounds
      (`Trait<T>`) — explicitly out of scope for M1's non-generic trait support (static dispatch
      only, no `Trait<T>`, see M1 above); needs monomorphization (below) to reach codegen at all
  - [ ] **Wildcard stack array (`[_]T`)** — moved here 2026-09-24 (found while surveying array
        gaps for M3): a stack array whose length isn't fixed at the type level is itself a generic
        (parametric over the length), so it belongs with generics/monomorphization rather than as
        its own array feature. Currently rejected by `ArrayKind::is_lowerable`
        (`sol_ast/ast_model/src/ast/sol_type.rs:366`) alongside `HeapArray`.
- [ ] Monomorphization (expansion to concrete types before LLVM)
- [ ] Full trait resolution incl. multi-impl-by-output-type-generic case (§7)
  - [ ] **Orphan-rule coherence checking** — added 2026-09-24 (`/grill-me`; spec'd in `sol-lang.md`
        §7, general Rust-style rule, not `AutoCopy`-specific: reject `impl Trait for Type` unless
        the trait or the type is local to the current crate). Not tracked anywhere before this;
        genuinely missing from "full trait resolution" above, which covers dispatch ambiguity, not
        impl-legality checking. Needed before `AutoCopy` (below) can actually reject a user
        `impl AutoCopy for int`.
  - [ ] **`This.(name: T)`/`This.[T](param)` constructor dispatch, desugared to trait impls** —
        added 2026-09-24 (`/grill-me`; spec'd in `sol-lang.md` §6/§7), found missing from TODO
        during a spec-vs-roadmap diff. Confirmed these aren't a separate "constructor overload"
        mechanism: `This.(name: T)` desugars to a `From<T>`-style trait impl per parameter type,
        `This.[T](param)` likewise per element type, both reusing this same multi-impl-by-generic-
        parameter dispatch — no bespoke logic needed beyond the desugaring itself. `This.[T]` has
        an AST field already (`sol_ast/ast_model/src/ast/statements.rs:408`) but no confirmed
        dispatch-by-argument-type resolver logic for either form yet.
- [ ] **`AutoCopy` as a real, user-implementable trait** — added 2026-09-24 (`/grill-me`; spec'd in
      `sol-lang.md` §11). Confirmed scope: this becomes the user-facing surface for what
      `DeclareStore::is_auto_copy` already hardcodes internally for M2's move-checker
      (primitives/refs/pointers/slices copy, structs/arrays move-only) — a user struct should be
      able to opt in via `use Type impl AutoCopy {}` and have M2's checker treat it as copy instead
      of move-only. Depends on the orphan rule above (primitives are already `AutoCopy` inside the
      compiler's own crate, so a user `impl AutoCopy for int` must be rejected as a foreign-impl
      violation, not special-cased). Once real, `is_auto_copy` should read actual trait impls
      instead of its current hardcoded type-shape match.
- [ ] `Res` / `.pass` / `?T`
- [ ] **`match` and match-chain (`.Variant{}`) type inference + MIR lowering** — corrected/split out
      2026-09-24 (found while surveying M3 scope): further along than the old single bullet
      suggested, but only front-end. `match` (`ast::ExpressionKind::Match`,
      `sol_ast/ast_model/src/ast/expression.rs:83`) and match-chain (internally `MatchMethod`,
      `expression.rs:86`, design doc `docs/sol-lang.md:487-520`) are both fully parsed
      (`sol_ast/ast_parser/src/parse/expression/conditionals.rs:104`,
      `sol_ast/ast_parser/src/parse/expression/access.rs:278-332`) and name-resolved
      (`sol_ast/sol_resolver/src/resolve/expression.rs:49,83,155`), but expression-type inference
      explicitly isn't implemented for either (`// not yet impl`,
      `sol_ast/sol_resolver/src/resolve/typecheck/expression.rs:367-381`), and neither has any MIR
      lowering at all (zero hits for `Match`/`MatchMethod` in `sol_mir/mir_parser` or
      `sol_mir/mir_codegen`) — so nothing using `match` or a match-chain can reach codegen yet,
      despite an existing end-to-end test for it (`sol_tester/sol/src/testCompiler.sol:233-245`).
- [ ] **Map-chain (`->Variant{}`)** — 0% implemented, not just missing lowering. Documented as a
      distinct single-variant `map`/`map_err` operation in `docs/sol-lang.md:525-531`, but the `->`
      token doesn't exist in the lexer at all, so there's no AST node, parser rule, resolver, or MIR
      support anywhere. An end-to-end test already references it
      (`sol_tester/sol/src/testCompiler.sol:248-257`, `ok->Ok{str.(it)}`) but can't even tokenize
      today.
- [ ] Extend borrow checker to generic MIR with `AutoCopy` bounds
- [ ] **Auto-derived `Eq`, opt-in `Ord`** — added 2026-09-24 (`/grill-me`; spec'd in `sol-lang.md`
      §14). `Eq` is structural and automatic (field-by-field/variant-by-variant), opt-out only, with
      non-`Eq` propagating upward from any non-`Eq` field with no annotation needed; `Ord` on
      structs/unions is opt-in via `use Type impl Ord { .. }`, unlike `Eq` — not auto-derived even
      when every field/variant is itself `Ord`. No derive/auto-implementation logic exists anywhere
      in the resolver or `ast_model` yet for either.
- [ ] `limit N` on `for` loops (blocked on `Res` error shape)
- [ ] `SwitchInt` discriminant handling for union tags (`Rvalue::Discriminant`)
- [ ] **Heap-allocated array (`[]T`)** — moved here 2026-09-24 (found while surveying array gaps for
      M3), corrected 2026-09-24 (`/grill-me`: confirmed `sol-lang.md` §3.3 is the correct spec — a
      `[]T` is heap-backed but **fixed-length once created**, no `push`/`pop`/`cap`; that's a
      separate kind, see `[dyn]T` below). Currently has no representation past the AST:
      `ArrayKind::is_lowerable` (`sol_ast/ast_model/src/ast/sol_type.rs:366`) and
      `DeclareStore::is_lowerable` (`sol_ast/ast_model/src/declare_store.rs:396`) both reject it
      before MIR building starts, and `mir_codegen::types::array_type`
      (`sol_mir/mir_codegen/src/types.rs:190`) rejects it again as `NonPrimitiveType` — the matching
      `HeapArray` codegen arm (`types.rs:220`) is a dead `unreachable!`. Needs a from-scratch runtime
      representation (length + heap buffer, no capacity/growth), not an extension of the existing
      fixed-array/slice path.
- [ ] **Growable dynamic array (`[dyn]T`)** — added 2026-09-24 (`sol-lang.md` §3.3; found missing
      entirely from TODO.md during a spec-vs-roadmap diff). A distinct array kind from `[]T` above:
      heap-backed with a real `cap`/`push`/`pop` growth API. No `dyn` array-kind token or AST variant
      exists yet at all (checked `sol_ast/ast_model/src/ast/sol_type.rs`'s `ArrayKind` enum) — this
      needs new syntax/tokenizing, not just a codegen gap like `[]T` above.
- [ ] **Item-level visibility (`pub(crate)`/`pub(super)`) + file-capitalization module visibility**
      — added 2026-09-24 (`sol-lang.md` §2; found missing entirely from TODO.md during a
      spec-vs-roadmap diff). Not implemented at all: no `Visibility` enum, no `pub(crate)`/
      `pub(super)` handling, no capitalization-based module-visibility logic anywhere in `sol_ast`
      (checked declaration parsing/resolving). Distinct from the crate-system track elsewhere in
      this file, which covers cross-crate linkage/`.solo` packaging, not intra-crate item scoping.
- [ ] **`mod foo { .. }` — nested child modules within one file** — added 2026-09-24 (`/grill-me`;
      spec'd in `sol-lang.md` §2). Unlike Rust's `mod foo;`, never used to pull in another source
      file (`import` already owns that); only carves a named, nested scope out of one file, needing
      its own `ModuleId`-style identity distinct from its containing file's. Visibility follows the
      item-level `pub`/`pub(super)` rule above (private by default, `pub mod` reachable from outside
      the file via `import File.foo.Thing`), not file capitalization — depends on the item-level
      visibility bullet above landing first. No `mod` keyword, nested-scope resolution, or
      `ModuleId` nesting exists anywhere in `sol_ast` yet.
- [ ] **`Foreach` lowering (`for x in xs { .. }`), via the `Iterator` trait** — moved here 2026-09-24
      (found while surveying array gaps for M3). `lower_for`
      (`sol_mir/mir_parser/src/function/control_flow.rs:220`) only handles
      `ast::ForCondition::While`; `Loop`/`Foreach` both fall straight through to
      `UnsupportedLoopCondition`, unimplemented. The resolver side is already done — `resolve_for`/
      `backfill_foreach_element_type` fully type-resolve `Foreach`, including inferring the element
      type from the collection's array/slice type — so this is MIR-lowering-only work, blocked here
      (not on arrays) because iterating anything is meant to go through a real `Iterator` trait
      (`next(&mut this): Option<T>` or equivalent), which needs M3's full trait resolution to exist
      first.
- [ ] **Part B of the M2 pipeline-architecture cleanup** — moved here from M2 (deferred
      2026-09-24: revisiting the "desugar `foreach`/`while` into a shared `mir_parser::desugar`
      submodule" question only makes sense once `Foreach` lowering (above) actually exists). Once
      `for x in xs { .. }` is lowered (an index/iterator variable + a `While`-shaped loop + an
      increment/`next()` call, rewritten before/during lowering), revisit whether that rewrite
      belongs in a real `desugar` submodule rather than inline in `control_flow.rs`.

### M4 — everything else (not sequenced)

- [ ] **Critical-edge splitting as a flag-free alternative to the M2 runtime drop-flag** — moved
      here from M2 (deferred 2026-09-24: this is a pure optimization, and optimization work should
      wait until after the full pipeline works, not be threaded into M2 while it's still landing
      correctness). Not scoped: an *acyclic* join with exactly *one* conditionally-moved value
      reaching it (the M2 runtime-drop-flag slice's own motivating example) can drop the runtime
      flag entirely by giving each incoming edge its own small dedicated block that does or skips
      the `Drop` before rejoining the shared continuation, rather than one shared guarded `Drop`
      reading a flag. Confirmed *not* a general replacement for the flag mechanism during a
      follow-up `/grill-me`: it stops being free the moment either (a) more than one
      conditionally-moved local reaches the same join (edges would need splitting per
      *combination* of drop obligations — 2 values ⇒ 4 edge variants, 3 ⇒ 8) or (b) the join is
      reached through a loop back-edge (there's no static edge count to split against "how many
      times around the loop"). Mirrors why rustc's own `ElaborateDrops` isn't pure edge-splitting
      either: it runs the same definite/maybe dataflow the M2 slice already built, elaborates for
      free wherever the answer is definite, and only falls back to a runtime flag for the genuinely
      ambiguous cases — edge-splitting would only ever be a size/speed micro-optimization layered
      on top of that fallback for its one narrow acyclic single-value case, not a replacement for
      it.
- [ ] **Per-array-element (`Index`) disjointness for overlap-checking** — deliberately deferred out
      of M2 (`/grill-me`'d 2026-09-24). `check_borrow_overlaps`'s `fields_disjoint`
      (`sol_mir/mir_parser/src/borrow_checker/overlap_check.rs:193`) already proves two borrows of
      different *struct fields* disjoint (`&mut o.a` vs `&mut o.b` is accepted), but falls through
      to "not proven disjoint ⇒ conflict" the instant it hits an `Index` or `Deref` projection step
      — so `&mut arr[0]` and `&mut arr[1]` are rejected exactly like `&mut arr[i]` vs `&mut arr[i]`,
      even though the former is provably safe. This is a **false positive, not a soundness bug**:
      nothing unsafe is ever accepted, real but safe array-processing patterns (e.g. swapping two
      elements via two runtime indices) just don't compile yet. Confirmed not worth implementing
      right now — indices are always runtime MIR locals here (no constant folding ahead of this
      pass), so proving disjointness for real would need genuine symbolic range analysis, a much
      bigger analysis than anything else in the M2 checklist, for a purely ergonomic (not
      correctness) payoff. Documented here as a known, accepted limitation rather than planned
      work; revisit only if this actually blocks a real program.
- [ ] **`mir_codegen` performance audit** — a `big.sol` benchmark (14,211 lines) showed `codegen`
      (70.55ms) taking 3x+ longer than `ast` (19.99ms), `name_resolve` (14.89ms), or `mir`
      (19.51ms) out of a 124.94ms total. Investigated (read-only, no fix applied yet) and found the
      likely causes, ranked by expected impact:
  - **No caching in `llvm_type()`** (`sol_mir/mir_codegen/src/types.rs:95-165`, `stub_type` at
    `:226`) — every call recursively re-derives the LLVM type from the `SolType`/struct fields from
    scratch, including rebuilding a fresh `Vec<BasicTypeEnum>` and calling `context.struct_type(..)`
    again, with no `TypeId -> BasicTypeEnum` memo anywhere on `CodegenCtx`/`ModuleCodegen`. Called
    ~19 times across the crate, several per-statement/per-projection, so the same type gets
    relowered thousands of times over a large file. Fix direction: cache by `TypeId` (e.g.
    `RefCell<VecMap<TypeId, BasicTypeEnum>>`) and check it before the recursive walk.
  - **`codegen_call` re-derives the whole callee signature per call site**
    (`sol_mir/mir_codegen/src/terminator.rs:279-321`) instead of reusing the `FunctionType` already
    attached to the callee's `FunctionValue` (declared once in `module.rs:87-120`). Every call
    instruction re-walks the callee's MIR locals and re-lowers every parameter type via the
    uncached `llvm_type` above — likely the single biggest contributor given how many call sites a
    14k-line file has into a shared set of functions. Fix direction: use
    `callee_value.get_type().get_param_types()` (inkwell) instead of re-deriving from MIR/Sol types.
  - **Redundant per-operation type resolution** (`rvalue.rs:152-174` `codegen_binary`,
    `function.rs:238-245` `local_type`, `rvalue.rs:487-494` `operand_type`) — a single binary op/
    cast/call argument re-resolves and re-lowers the same local's type 3-4 times independently
    (`operand_type` → `local_type` → `llvm_type`, plus `resolve_place` doing it again). Mostly
    self-heals once the `llvm_type` cache above exists, but structurally these should resolve a
    place's type once and thread it through.
  - **Uncached per-occurrence string/global creation** (`terminator.rs:259-277` `location_string`,
    `rvalue.rs:34-47` `codegen_string_constant`) — every `Assert` (bounds/overflow checks, pervasive
    in arithmetic-heavy code) formats a fresh `file:line:col` string and adds a brand-new,
    uniquely-named LLVM global rather than deduplicating by span/location. Lower priority than the
    two above (constant-factor, not scaling with usage count), but a steady source of allocation +
    LLVM name-table churn.
  - **Missing `with_capacity`** (`function.rs:101-102`) — `locals`/`drop_flags` `VecMap`s are
    built with `VecMap::new()` even though `function.locals.len()` is known up front; minor,
    amortized-growth cost repeated once per function.
  - Not yet implemented — this is a profiling lead, not a scoped task. Revisit once M2/M3 work
    settles or codegen speed becomes a real bottleneck for a workflow.
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
  - **`Send`/`Sync` deliberately deferred** (`/grill-me`'d 2026-09-24, found missing from TODO
    during a spec-vs-roadmap diff): spec §15 documents them (moving into `spawn` needs `Send`,
    sharing by reference across `spawn`s needs `Sync`, both auto-derived-when-possible), but
    confirmed the concurrency model is single-threaded for now — no real multi-threaded `spawn`
    exists to make either trait meaningful yet. Revisit once/if real multi-threading is actually
    designed, not as part of this async/await item.
- [ ] **Sol's own `#[test]` framework** — added 2026-09-24 (`/grill-me`; spec'd in `sol-lang.md`
      §17: compiler-recognized `#[test]` attribute, `test{Name}.sol` file-naming convention,
      per-test panic isolation), found missing from TODO during a spec-vs-roadmap diff. Confirmed
      low priority, deliberately deferred — "eventually, for Sol users writing Sol programs," not
      worth competing with the core type-system work still open in M3. Today the whole suite
      (`sol_tester`, the `.sol` exe tests) runs through the Rust-side harness, not anything
      Sol-native; no `#[test]`/`test{Name}.sol` handling exists in the compiler itself.
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
