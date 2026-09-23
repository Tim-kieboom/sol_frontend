//! Dangling-reference ("escape") checking — the first slice of M2's real
//! borrow/lifetime-conflict work (see `docs/mir-design.md` and `TODO.md`'s
//! M2 section), distinct from move-checking: does a function return a
//! reference whose *storage* doesn't outlive the call. `/grill-me`'d first:
//! no lifetime-parameter syntax (`'a`) exists or is planned — this traces
//! where a returned reference's storage *actually* comes from, backward
//! through the MIR, rather than inferring lifetime relationships the way
//! Rust's elision rules do (which also sidesteps elision's own "ambiguous,
//! needs an explicit annotation" dead end for a function with more than one
//! reference parameter — tracing finds the one true origin instead of
//! guessing which parameter an output might be tied to).
//!
//! Handles the full concrete (M1) CFG shape, `for` loops included
//! (`/grill-me`'d twice to get here: first straight-line only, then
//! `if`/`else`, then loops too), **and** interprocedural call-site tracing
//! (`/grill-me`'d a fourth time — see "Interprocedural tracing" below): a
//! reference-typed parameter is no longer trusted as unconditionally safe —
//! its own safety is now tied to whatever the function's *callers* actually
//! pass, verified transitively across the whole call graph.
//!
//! The backward trace, from wherever the return value is used, has exactly
//! four outcomes now (three plus the new interprocedural one):
//!  - It reaches a local with **no recorded assignment** at all, walking all
//!    the way back to the function's own entry block with nothing found —
//!    in code with no partial-move/partial-init tracking, this must be a
//!    parameter (including `this`/`&this`/`&mut this`) — its own safety is
//!    now **tied to that specific parameter index** (`Origin::TiedToParams`)
//!    rather than unconditionally `Safe` — see "Interprocedural tracing".
//!  - It reaches an `Rvalue::Ref { place, .. }` (a fresh reference
//!    construction) whose `place`'s projection has a `Deref` as its very
//!    *first* element (a bare local being directly dereferenced, e.g.
//!    `&this.field` when `this: &T`, or `&*p`) — classified by `place.local`'s
//!    own type: an **owning heap pointer** (`*T`) is unconditionally
//!    `Safe` (heap memory doesn't depend on any caller-passed reference
//!    staying valid); a **reference** (`&T`/`&mut T`) is only as safe as
//!    whatever *that* local's own origin is — traced the same way a
//!    bare-local alias would be, which (if it's itself a parameter) now also
//!    ties to a parameter index instead of collapsing to `Safe`. A `Deref`
//!    buried *later* in the projection (reached through a field first, e.g.
//!    `o.ptrField.innerField`) isn't refined by this slice — still
//!    unconditionally `Safe`, a known, narrower, explicitly accepted gap
//!    (no exe test or unit test exercises this shape).
//!  - It reaches an `Rvalue::Ref { place, .. }` whose projection contains
//!    **no** `Deref` at all — **dangling**: `place.local`'s own storage (a
//!    body-declared local, or a parameter's own by-value slot) belongs to
//!    this function's frame, gone the moment it returns.
//!  - It reaches a local that's the **destination of a `Call` terminator**
//!    (not a `Statement::Assign` at all) — resolved via the callee's own
//!    summary (see "Interprocedural tracing").
//!
//! Any other shape the trace meets along the way — an alias through a
//! *projected* place (`Use(Copy(x.field))`), or a non-`Ref`/non-alias/
//! non-`Call`-result rvalue reaching a reference-typed local — isn't
//! analyzed: the trace stops and nothing is reported for that path, the
//! same "unfamiliar shape ⇒ skip, don't guess" discipline `move_check`
//! itself uses. That discipline applies to the *whole* combined answer, not
//! just one path: if any path the trace has to walk to answer "is this
//! local's value always safe" turns out unfamiliar, the combined answer is
//! unfamiliar too (`None`) rather than guessing from whichever paths *did*
//! resolve — a false "safe" here would be a real unsoundness, not just a
//! missed diagnostic.
//!
//! At an `if`/`else` join, a value's origin is only trusted as safe when
//! it's safe on **every** incoming path — the trace forks at the join and
//! walks back through each predecessor independently, combining every
//! path's own origin via `combine` (see below) rather than requiring a flat
//! boolean.
//!
//! # `for` loops — real fixed-point convergence
//!
//! A `for` loop's back edge means a block's own "value reaching here" can
//! depend on itself (the loop body's own end feeds back into the loop
//! header). Mirrors `move_check`'s own two-phase discipline exactly (start
//! optimistic, converge monotonically toward the conservative verdict,
//! never report mid-convergence): round 0 treats every not-yet-settled
//! `(LocalId, BlockId)` pair optimistically as `Safe`; a round after that
//! falls back to whatever the *previous* round settled on for any pair
//! still "in progress" on the current round's own call stack (a genuine
//! cycle). Rounds repeat until one produces byte-for-byte the same table as
//! the round before it. Sound and terminating for the same reason
//! `move_check`'s own fixed point is: a loop header always has at least one
//! predecessor *outside* the loop (the initial entry edge, never part of
//! any cycle), so every reachable pair bottoms out in a real, non-circular
//! answer somewhere, and each round can only ever move a pair's verdict
//! toward the more conservative end of the `Origin` lattice, never back.
//! Rather than a fine-grained worklist (discovering which `(LocalId,
//! BlockId)` pairs even matter needs the same demand-driven recursive trace
//! this module already has), this re-runs the *entire* demand-driven trace
//! once per round — simpler, less efficient, the same trade this
//! compiler's own established preference has consistently made throughout
//! M2 (`move_check`'s own plain queue over a priority worklist).
//!
//! # Interprocedural tracing
//!
//! `/grill-me`'d at length before writing anything — the deferred half of
//! escape-checking's own original scope cut (a reference-typed parameter
//! was trusted as safe without ever checking what the caller passed in).
//! The motivating example: `lifetime(obj: &Obj): &Obj { return obj }`,
//! whose own body is straightforwardly safe in isolation, called as
//! `lifetime(&Obj{})` — the *call site* passes a reference to a temporary
//! that dies before the caller ever sees it, which this module could never
//! catch without knowing what `lifetime` does with its own parameter.
//!
//! **Function summaries.** Each function's own `Origin` (the same lattice
//! its return-value trace already produces) is computed once for the whole
//! program and reused everywhere: `Origin::Safe` (doesn't depend on any
//! caller-passed value), `Origin::Dangling(local)` (a real, already-reported
//! intraprocedural bug — a caller's own trace treats this as unfamiliar
//! (`None`) rather than cascading a *derived* fault into every caller; the
//! bug is reported once, at its own source, not once per call site), or
//! `Origin::TiedToParams(indices)` — the function's return value is only as
//! safe as whichever of *its own* parameters (by index) the caller actually
//! passes. A function can have more than one `Return` block; its own
//! summary folds all of them together via `combine` (the same "worse wins"
//! join the CFG-level trace already uses at branch points) — a caller can't
//! know which return path executes, so it has to satisfy every one.
//!
//! **The call graph can be cyclic too** (direct or mutual recursion) — this
//! compiler's own resolver/lowerer/codegen are all already structurally
//! recursion-safe (no ordering dependency anywhere), even though nothing
//! exercises it yet. Rather than gating recursive functions out (the
//! "narrow first" precedent every other CFG-cycle slice used), this goes
//! straight to real convergence, matching every prior "which rigor level"
//! choice in this series: `converge_summaries` is a flat, un-nested outer
//! fixed point — round 0 initializes every function's summary to the most
//! permissive `Origin::Safe`; each subsequent round recomputes every
//! function's own summary using the *previous* round's summaries as the
//! read-only background table for any `Call` it traces through (including
//! a self-call, which just reads its own previous-round entry — no special
//! in-progress bookkeeping needed at this level, since there's no recursive
//! descent *into* another function's own computation, just a flat lookup).
//! Rounds repeat until the whole table stops changing. Slower than a
//! topologically-ordered pass over the call graph's DAG-shaped part (it can
//! take one extra round per "hop" in a call chain before precision fully
//! propagates), but simpler, and the same trade already accepted for the
//! CFG-level fixed point above.
//!
//! **Routing a `Call` result through the trace.** A block whose own
//! terminator is `Call { id, arguments, destination: Some(place), .. }`
//! defines `place.local` (whole-locals only, matching the module's own
//! established scope) the instant the call returns — a definition site the
//! trace previously never looked at (it only ever searched
//! `Statement::Assign`s). `trace_from_block_start` now checks this first:
//! if the block's own terminator is a matching `Call`, `place.local`'s
//! origin comes from the callee's settled summary — `Safe`/`Dangling`
//! pass straight through (`Dangling` becomes unfamiliar/`None`, per the
//! no-cascading-faults rule above), `TiedToParams(indices)` recurses into
//! tracing each tied argument's own origin (still whole-locals-only:
//! `Operand::Copy(place)`/`Operand::Move(place)` with an empty projection
//! only) within the *same* block (arguments are evaluated immediately
//! before the call fires, so this searches the call's own block's
//! statements in full), combining every tied argument's origin via
//! `combine` — if `lifetime`'s `obj` parameter ties to index 0 and the call
//! site passes `&Obj{}` (a fresh `Ref` with no `Deref`, i.e. dangling), the
//! whole call's own origin comes out `Dangling`, exactly like any other
//! locally-dangling `Ref` would.
//!
//! **Fault reporting is unaffected**: `check_escapes` still walks every
//! `Terminator::Return` block in every function and reports a
//! `DanglingReference` fault wherever the (now interprocedurally aware)
//! settled origin is `Dangling` — the only change is that `Dangling` can
//! now be discovered through a call chain, not just local aliasing.

use ast_model::{SolType, declare_store::DeclareStore};
use mir_model::{self as mir, BlockId, LocalId};
use sol_utils::{
    FunctionId,
    collections::{
        vec_map::{VecMap, VecMapIndex},
        vec_set::VecSet,
    },
    fault::Fault,
};

use crate::fault::{MirErrorKind, MirFault};

use super::move_check::successors;

/// See the module's own docs. Returns one fault per return path (across
/// every function) whose value provably dangles — a function can have more
/// than one `Terminator::Return` once `if`/`else` is in play (each
/// `return`/implicit tail-return statement seals its own).
pub fn check_escapes(
    functions: &VecMap<FunctionId, mir::Function>,
    declares: &DeclareStore,
) -> Vec<MirFault> {
    let summaries = converge_summaries(functions, declares);

    let mut faults = Vec::new();
    for (_, function) in functions.entries() {
        let Some(return_local) = function.return_local else {
            continue;
        };
        if !is_reference_typed(function, return_local, declares) {
            continue;
        }

        let predecessors = build_predecessors(function);
        let return_blocks = return_blocks_of(function);
        let settled = converge(
            return_local,
            &return_blocks,
            function,
            &predecessors,
            declares,
            &summaries,
        );

        for &block_id in &return_blocks {
            if let Some(Some(Origin::Dangling(local))) = settled.get(&(return_local, block_id)) {
                faults.push(Fault::error_with_kind(
                    MirErrorKind::DanglingReference,
                    Some(function.locals[*local].span),
                ));
            }
        }
    }

    faults
}

#[derive(Clone, PartialEq, Eq)]
enum Origin {
    /// Doesn't depend on any caller-supplied value — either a heap-pointer
    /// deref, or (once every path a function's own return can take has been
    /// folded together) a function with no reference-typed dependency at
    /// all on its own callers.
    Safe,
    /// The local whose own storage doesn't outlive this function — used to
    /// look up its declaration span for the fault (mirrors `move_check`'s
    /// `UseAfterMove` — MIR statements don't carry per-statement spans yet).
    Dangling(LocalId),
    /// Only as safe as whichever of *this function's own* parameters (by
    /// index) the caller actually passes — see the module's own
    /// "Interprocedural tracing" docs.
    TiedToParams(VecSet<usize>),
}

/// The "worse wins" join every branch point (CFG-level `if`/`else`/`for`,
/// and a function's own multiple `Return` blocks folding into one summary)
/// uses: `Dangling` absorbs everything (a single bad path condemns the
/// whole join), two `TiedToParams` sets union (either path could be the one
/// that actually executes, so the caller has to satisfy both), and `Safe`
/// is the identity element.
fn combine(a: Origin, b: Origin) -> Origin {
    match (a, b) {
        (Origin::Dangling(local), _) | (_, Origin::Dangling(local)) => Origin::Dangling(local),
        (Origin::Safe, other) | (other, Origin::Safe) => other,
        (Origin::TiedToParams(mut a_set), Origin::TiedToParams(b_set)) => {
            a_set.extend(b_set.entries());
            Origin::TiedToParams(a_set)
        }
    }
}

type Table = std::collections::HashMap<(LocalId, BlockId), Option<Origin>>;
type Summaries = VecMap<FunctionId, Origin>;

/// Everything a single `converge` round's trace needs that stays fixed for
/// its whole duration — bundled so the trace functions below (each already
/// recursing into several of the others) don't have to carry every one of
/// these individually as its own parameter.
struct TraceCtx<'a> {
    function: &'a mir::Function,
    predecessors: &'a VecMap<BlockId, Vec<BlockId>>,
    declares: &'a DeclareStore,
    summaries: &'a Summaries,
    previous_round: &'a Table,
}

/// The mutable, per-round memoization state the trace functions thread
/// through their recursion — see `TraceCtx` for the read-only half.
struct TraceState<'a> {
    in_progress: &'a mut std::collections::HashSet<(LocalId, BlockId)>,
    this_round: &'a mut Table,
}

fn is_reference_typed(function: &mir::Function, local: LocalId, declares: &DeclareStore) -> bool {
    matches!(
        declares.get_type(function.locals[local].ty),
        Some(SolType::Reference(_))
    )
}

fn return_blocks_of(function: &mir::Function) -> Vec<BlockId> {
    function
        .blocks
        .entries()
        .filter(|(_, block)| matches!(block.terminator, mir::Terminator::Return))
        .map(|(block_id, _)| block_id)
        .collect()
}

/// The real fixed point over the whole call graph — see the module's own
/// "Interprocedural tracing" docs on why this is flat (no per-function
/// in-progress recursion needed) and why it's sound despite the call graph
/// potentially being cyclic.
fn converge_summaries(
    functions: &VecMap<FunctionId, mir::Function>,
    declares: &DeclareStore,
) -> Summaries {
    let mut summaries: Summaries = functions
        .entries()
        .map(|(id, _)| (id, Origin::Safe))
        .collect();
    loop {
        let mut next: Summaries = Summaries::new();
        for (id, function) in functions.entries() {
            next.insert(id, compute_function_origin(function, declares, &summaries));
        }
        if next == summaries {
            return next;
        }
        summaries = next;
    }
}

/// One function's own combined `Origin`, folding every one of its `Return`
/// blocks together — a caller can't know which one executes, so it has to
/// satisfy all of them. `None` (unfamiliar) anywhere folds to `Origin::Safe`
/// for the *summary*'s own purposes — the same "prefer under- over
/// over-reporting" discipline `check_escapes` itself already applies to a
/// single function; this just extends it one level up, so an unresolved
/// intraprocedural shape never makes a *caller* more conservative than the
/// callee's own (unreported) uncertainty would justify.
fn compute_function_origin(
    function: &mir::Function,
    declares: &DeclareStore,
    summaries: &Summaries,
) -> Origin {
    let Some(return_local) = function.return_local else {
        return Origin::Safe;
    };
    if !is_reference_typed(function, return_local, declares) {
        return Origin::Safe;
    }

    let predecessors = build_predecessors(function);
    let return_blocks = return_blocks_of(function);
    let settled = converge(
        return_local,
        &return_blocks,
        function,
        &predecessors,
        declares,
        summaries,
    );

    let mut combined = Origin::Safe;
    for &block_id in &return_blocks {
        let origin = settled
            .get(&(return_local, block_id))
            .cloned()
            .flatten()
            .unwrap_or(Origin::Safe);
        combined = combine(combined, origin);
    }
    combined
}

/// Repeatedly traces every `(return_local, return_block)` pair (and
/// whatever upstream `(LocalId, BlockId)` pairs that demands, transitively)
/// to a shared, round-settled table — see the module's own docs on why
/// this is a real, sound fixed point, not just a heuristic. `return_blocks`
/// is fixed, deterministic input, so every round discovers exactly the
/// same *set* of `(LocalId, BlockId)` keys (only their *values* can still
/// be settling) — that's what makes a plain `Table` equality check a valid
/// convergence test.
fn converge(
    return_local: LocalId,
    return_blocks: &[BlockId],
    function: &mir::Function,
    predecessors: &VecMap<BlockId, Vec<BlockId>>,
    declares: &DeclareStore,
    summaries: &Summaries,
) -> Table {
    let mut previous_round = Table::new();
    loop {
        let mut this_round = Table::new();
        let mut in_progress = std::collections::HashSet::new();
        let ctx = TraceCtx {
            function,
            predecessors,
            declares,
            summaries,
            previous_round: &previous_round,
        };
        let mut state = TraceState {
            in_progress: &mut in_progress,
            this_round: &mut this_round,
        };
        for &block_id in return_blocks {
            trace_from_block_start(return_local, block_id, &ctx, &mut state);
        }
        if this_round == previous_round {
            return this_round;
        }
        previous_round = this_round;
    }
}

/// The value `local` holds at the *start* of `block_id` (i.e. before any of
/// that block's own statements run). Checks first whether `block_id`'s own
/// terminator is a whole-place `Call` defining `local` (see the module's
/// own "Routing a `Call` result through the trace" docs) — if not, falls
/// through to `trace_within_block`'s ordinary statement search. Three-way
/// memoization within a single round, exactly mirroring the discipline
/// `move_check`'s own fixed point uses: a `this_round` hit is reused, an
/// `in_progress` hit (a genuine cycle) falls back to `previous_round`'s
/// settled answer (`Safe` if this is round 0 and nothing settled there
/// yet), and only the outermost frame for a given key ever writes into
/// `this_round`.
fn trace_from_block_start(
    local: LocalId,
    block_id: BlockId,
    ctx: &TraceCtx,
    state: &mut TraceState,
) -> Option<Origin> {
    let key = (local, block_id);
    if let Some(settled) = state.this_round.get(&key) {
        return settled.clone();
    }
    if state.in_progress.contains(&key) {
        return ctx
            .previous_round
            .get(&key)
            .cloned()
            .unwrap_or(Some(Origin::Safe));
    }

    state.in_progress.insert(key);
    let result = match call_destination_origin(local, block_id, ctx, state) {
        Some(origin) => origin,
        None => {
            let statement_count = ctx.function.blocks[block_id].statements.len();
            trace_within_block(local, block_id, statement_count, ctx, state)
        }
    };
    state.in_progress.remove(&key);
    state.this_round.insert(key, result.clone());
    result
}

/// If `block_id`'s own terminator is a whole-place `Call` whose destination
/// is exactly `local`, resolves `local`'s origin via the callee's settled
/// summary — `Some(origin)` (`origin` itself `None` if unresolvable, e.g.
/// the callee's own summary is `Dangling` — see the module's own docs on
/// why that doesn't cascade). `None` (the outer `Option`) means the
/// terminator isn't a matching `Call` at all — the caller should fall
/// through to the ordinary statement search.
fn call_destination_origin(
    local: LocalId,
    block_id: BlockId,
    ctx: &TraceCtx,
    state: &mut TraceState,
) -> Option<Option<Origin>> {
    let mir::Terminator::Call {
        id,
        arguments,
        destination: Some(place),
        ..
    } = &ctx.function.blocks[block_id].terminator
    else {
        return None;
    };
    if place.local != local || !place.projection.is_empty() {
        return None;
    }

    let Some(summary) = ctx.summaries.get(*id) else {
        return Some(None);
    };
    let origin = match summary {
        Origin::Dangling(_) => None,
        Origin::Safe => Some(Origin::Safe),
        Origin::TiedToParams(indices) => {
            let statement_count = ctx.function.blocks[block_id].statements.len();
            let mut combined = Origin::Safe;
            let mut sound = true;
            for index in indices.entries() {
                let arg_origin = arguments.get(index).and_then(|operand| match operand {
                    mir::Operand::Copy(place) | mir::Operand::Move(place)
                        if place.projection.is_empty() =>
                    {
                        trace_within_block(place.local, block_id, statement_count, ctx, state)
                    }
                    _ => None,
                });
                match arg_origin {
                    Some(origin) => combined = combine(combined, origin),
                    None => {
                        sound = false;
                        break;
                    }
                }
            }
            if sound { Some(combined) } else { None }
        }
    };
    Some(origin)
}

/// Searches `block_id`'s own statements *before* `before_index`, in reverse,
/// for a bare-local assignment to `local`. Found: follows the same
/// alias-chain-or-classify-`Ref` rule (continuing the search within the
/// same block, from that assignment's own index, for an alias). Not found:
/// falls through to whatever reaches this block's own start — this block's
/// predecessors, forked and combined via `combine` if there's more than
/// one, or a `TiedToParams` origin naming `local`'s own parameter index if
/// there are none at all (the function's own entry block) and `local`
/// really is one of its parameters.
fn trace_within_block(
    local: LocalId,
    block_id: BlockId,
    before_index: usize,
    ctx: &TraceCtx,
    state: &mut TraceState,
) -> Option<Origin> {
    let block = &ctx.function.blocks[block_id];
    for (index, statement) in block.statements.as_slice()[..before_index]
        .iter()
        .enumerate()
        .rev()
    {
        let mir::Statement::Assign(place, rvalue) = statement else {
            continue;
        };
        if place.local != local || !place.projection.is_empty() {
            continue;
        }
        return match rvalue {
            mir::Rvalue::Use(mir::Operand::Copy(alias) | mir::Operand::Move(alias)) => {
                if !alias.projection.is_empty() {
                    None
                } else {
                    trace_within_block(alias.local, block_id, index, ctx, state)
                }
            }
            mir::Rvalue::Ref {
                place: ref_place, ..
            } => classify_ref(ref_place, block_id, index, ctx, state),
            _ => None,
        };
    }

    let preds = ctx
        .predecessors
        .get(block_id)
        .filter(|preds| !preds.is_empty());
    let Some(preds) = preds else {
        // No predecessor recorded assignment reaches here — the function's
        // own entry block, `local` unassigned: a parameter (see the
        // module's own "Interprocedural tracing" docs — no longer an
        // unconditional `Safe`). `LocalId` values start at 1 (0 is reserved
        // as `LocalId::ERROR`), and parameters are allocated first, so a
        // parameter's own 0-based index is `local.index() - 1`.
        return Some(
            if local.index() >= 1 && local.index() <= ctx.function.arg_count {
                Origin::TiedToParams(VecSet::from([local.index() - 1]))
            } else {
                Origin::Safe
            },
        );
    };

    let mut combined = Origin::Safe;
    for &pred in preds {
        let pred_origin = trace_from_block_start(local, pred, ctx, state)?;
        combined = combine(combined, pred_origin);
    }
    Some(combined)
}

/// Classifies a fresh `Rvalue::Ref { place: ref_place, .. }`: no `Deref`
/// anywhere in `ref_place`'s own projection ⇒ dangling (`ref_place.local`'s
/// own storage belongs to this frame). A `Deref` as the projection's very
/// *first* element ⇒ classify by `ref_place.local`'s own type — an owning
/// heap pointer is unconditionally safe, a reference recurses into tracing
/// `ref_place.local`'s own origin (see the module's own docs on why a
/// `Deref` reached any other way isn't refined by this slice).
fn classify_ref(
    ref_place: &mir::Place,
    block_id: BlockId,
    at_index: usize,
    ctx: &TraceCtx,
    state: &mut TraceState,
) -> Option<Origin> {
    let deref_position = ref_place
        .projection
        .iter()
        .position(|elem| matches!(elem, mir::PlaceElem::Deref));
    match deref_position {
        None => Some(Origin::Dangling(ref_place.local)),
        Some(0) => match ctx
            .declares
            .get_type(ctx.function.locals[ref_place.local].ty)
        {
            Some(SolType::Pointer(_)) => Some(Origin::Safe),
            Some(SolType::Reference(_)) => {
                trace_within_block(ref_place.local, block_id, at_index, ctx, state)
            }
            _ => None,
        },
        // A `Deref` exists somewhere in the projection but isn't the first
        // element (reached through a field first) — not refined by this
        // slice, a known, narrower gap; unconditionally safe as before.
        Some(_) => Some(Origin::Safe),
    }
}

/// Every block's direct predecessors, derived from every other block's own
/// successors (mirrors `move_check::analyze`'s own construction, minus the
/// reachable-from-entry restriction — a backward trace only ever visits
/// blocks reachable *from* wherever it starts, so an unreachable block
/// simply never gets asked about).
fn build_predecessors(function: &mir::Function) -> VecMap<BlockId, Vec<BlockId>> {
    let mut predecessors: VecMap<BlockId, Vec<BlockId>> = VecMap::new();
    for (block_id, block) in function.blocks.entries() {
        for successor in successors(&block.terminator) {
            if function.blocks.get(successor).is_some() {
                predecessors.entry(successor).or_default().push(block_id);
            }
        }
    }
    predecessors
}
