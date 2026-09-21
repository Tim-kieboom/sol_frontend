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
//! Handles the full concrete (M1) CFG shape now, including `for` loops
//! (`/grill-me`'d twice to get here: first straight-line only, then `if`/
//! `else`, now loops too). Only checks a function's *own* return value, not
//! its callers: a reference-typed parameter is trusted as safe without
//! verifying what the caller actually passed in — that's real
//! interprocedural propagation, a separate, later slice (see `TODO.md`'s M2
//! entry).
//!
//! The backward trace, from wherever the return value is used, has exactly
//! three outcomes:
//!  - It reaches a local with **no recorded assignment** at all, walking all
//!    the way back to the function's own entry block with nothing found —
//!    in code with no partial-move/partial-init tracking, this must be a
//!    parameter (including `this`/`&this`/`&mut this`) — **safe**, trusted
//!    without checking the caller (see above).
//!  - It reaches an `Rvalue::Ref { place, .. }` (a fresh reference
//!    construction) whose `place`'s projection contains a `Deref` step
//!    *anywhere* — **safe**. This single rule covers both "reborrowing
//!    through an already-received reference" (`&this.field` when `this` is
//!    `&T` — `resolve_field_place`'s own `auto_deref` already inserts the
//!    `Deref`) and "dereferencing an owning heap pointer" (`&*p` for
//!    `p: *T`) — both point at memory outside this function's own stack
//!    frame, for the same underlying reason (indirection through *some*
//!    pointer/reference means the pointee isn't this frame's own storage).
//!  - It reaches an `Rvalue::Ref { place, .. }` whose projection contains
//!    **no** `Deref` at all — **dangling**: `place.local`'s own storage (a
//!    body-declared local, or a parameter's own by-value slot) belongs to
//!    this function's frame, gone the moment it returns.
//!
//! Any other shape the trace meets along the way — an alias through a
//! *projected* place (`Use(Copy(x.field))`), or a non-`Ref`/non-alias
//! rvalue reaching a reference-typed local — isn't analyzed: the trace
//! stops and nothing is reported for that path, the same "unfamiliar shape
//! ⇒ skip, don't guess" discipline `move_check` itself uses. That discipline
//! applies to the *whole* combined answer, not just one path: if any path
//! the trace has to walk to answer "is this local's value always safe"
//! turns out unfamiliar, the combined answer is unfamiliar too (`None`)
//! rather than guessing from whichever paths *did* resolve — a false "safe"
//! here would be a real unsoundness, not just a missed diagnostic.
//!
//! At an `if`/`else` join, a value's origin is only trusted as safe when
//! it's safe on **every** incoming path — the trace forks at the join and
//! walks back through each predecessor independently, requiring all of them
//! to resolve safe (mirrors requiring a moved-on-*any*-path value to be
//! rejected in `move_check`, just for the opposite conclusion: here, only
//! *all*-paths-safe is trusted).
//!
//! # `for` loops — real fixed-point convergence
//!
//! `/grill-me`'d at length before writing anything: a `for` loop's back edge
//! means a block's own "value reaching here" can depend on itself (the loop
//! body's own end feeds back into the loop header). A first design sketch
//! reached for a per-block "in progress" marker defaulting straight to
//! `Dangling` the moment a cycle was hit, with a fallback span on
//! `BasicBlock` for the case with no real local to blame — abandoned once it
//! became clear that's strictly *worse* than just doing what `move_check`'s
//! own loop extension already does: a real fixed point.
//!
//! Mirrors `move_check`'s own two-phase discipline exactly (start optimistic,
//! converge monotonically toward the conservative verdict, never report
//! mid-convergence) — but `move_check`'s fixed point is a block-level
//! Kildall worklist (only the *set* of blocks needing reprocessing has to be
//! tracked, since "maybe moved" is a monotonically-growing set with a cheap,
//! well-understood update rule). Escape-checking's `(LocalId, BlockId)`
//! query space doesn't have an equally cheap incremental update — discovering
//! *which* pairs even matter requires the same demand-driven recursive trace
//! this module already has. So instead of a fine-grained worklist, this
//! re-runs the *entire* demand-driven trace once per round: round 0 treats
//! every not-yet-settled `(LocalId, BlockId)` pair optimistically as `Safe`
//! (the same starting point `move_check`'s "nothing moved yet" uses); a
//! round after that falls back to whatever the *previous* round settled on
//! for any pair still "in progress" on the current round's own call stack
//! (a genuine cycle). Rounds repeat until one produces byte-for-byte the
//! same table as the round before it. This is sound and terminates: a loop
//! header always has at least one predecessor *outside* the loop (the
//! initial entry edge, never part of any cycle), so every reachable pair
//! bottoms out in a real, non-circular answer somewhere, and each round can
//! only ever move a pair's verdict from `Safe` toward `Dangling` (a
//! predecessor `?`-propagating `None` or resolving to `Dangling` can only
//! make a join's own result *more* conservative than the previous round's,
//! never less) — a finite lattice, descending, so it settles.
//!
//! Less efficient than a targeted worklist, but far simpler, and this
//! compiler's own established preference (`move_check`'s plain queue, not a
//! priority worklist; escape-checking's own per-round recursion, not a
//! hand-rolled dependency graph) has consistently favored correctness and
//! clarity over micro-optimized dataflow scheduling.

use std::collections::{HashMap, HashSet};

use ast_model::{SolType, declare_store::DeclareStore};
use mir_model::{self as mir, BlockId, LocalId};
use sol_utils::fault::Fault;

use crate::fault::{MirErrorKind, MirFault};

use super::move_check::successors;

/// See the module's own docs. Returns one fault per return path whose value
/// provably dangles — a function can have more than one `Terminator::Return`
/// once `if`/`else` is in play (each `return`/implicit tail-return statement
/// seals its own).
pub fn check_escapes(function: &mir::Function, declares: &DeclareStore) -> Vec<MirFault> {
    let mut faults = Vec::new();

    let Some(return_local) = function.return_local else {
        return faults;
    };
    let is_reference = matches!(
        declares.get_type(function.locals[return_local].ty),
        Some(SolType::Reference(_))
    );
    if !is_reference {
        return faults;
    }

    let predecessors = build_predecessors(function);
    let return_blocks: Vec<BlockId> = function
        .blocks
        .entries()
        .filter(|(_, block)| matches!(block.terminator, mir::Terminator::Return))
        .map(|(block_id, _)| block_id)
        .collect();

    let settled = converge(return_local, &return_blocks, function, &predecessors);

    for &block_id in &return_blocks {
        if let Some(Some(Origin::Dangling(local))) = settled.get(&(return_local, block_id)) {
            faults.push(Fault::error_with_kind(
                MirErrorKind::DanglingReference,
                Some(function.locals[*local].span),
            ));
        }
    }

    faults
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Origin {
    Safe,
    /// The local whose own storage doesn't outlive this function — used to
    /// look up its declaration span for the fault (mirrors `move_check`'s
    /// `UseAfterMove` — MIR statements don't carry per-statement spans yet).
    Dangling(LocalId),
}

type Table = HashMap<(LocalId, BlockId), Option<Origin>>;

/// Repeatedly traces every `(return_local, return_block)` pair (and whatever
/// upstream `(LocalId, BlockId)` pairs that demands, transitively) to a
/// shared, round-settled table — see the module's own docs on why this is a
/// real, sound fixed point, not just a heuristic. `return_blocks` is fixed,
/// deterministic input, so every round discovers exactly the same *set* of
/// `(LocalId, BlockId)` keys (only their *values* can still be settling) —
/// that's what makes a plain `Table` equality check a valid convergence
/// test.
fn converge(
    return_local: LocalId,
    return_blocks: &[BlockId],
    function: &mir::Function,
    predecessors: &HashMap<BlockId, Vec<BlockId>>,
) -> Table {
    let mut previous_round: Table = HashMap::new();
    loop {
        let mut this_round: Table = HashMap::new();
        let mut in_progress: HashSet<(LocalId, BlockId)> = HashSet::new();
        for &block_id in return_blocks {
            trace_from_block_start(
                return_local,
                block_id,
                function,
                predecessors,
                &previous_round,
                &mut in_progress,
                &mut this_round,
            );
        }
        if this_round == previous_round {
            return this_round;
        }
        previous_round = this_round;
    }
}

/// The value `local` holds at the *start* of `block_id` (i.e. before any of
/// that block's own statements run). Three-way memoization within a single
/// round: `this_round` (already fully settled this round — reused, not
/// recomputed), `in_progress` (currently being computed higher up *this same
/// round's* own call stack — a genuine back-edge cycle, resolved via
/// `previous_round`'s own settled answer, `Safe` if this is round 0 and
/// nothing settled yet), or neither (compute it now).
fn trace_from_block_start(
    local: LocalId,
    block_id: BlockId,
    function: &mir::Function,
    predecessors: &HashMap<BlockId, Vec<BlockId>>,
    previous_round: &Table,
    in_progress: &mut HashSet<(LocalId, BlockId)>,
    this_round: &mut Table,
) -> Option<Origin> {
    let key = (local, block_id);
    if let Some(&settled) = this_round.get(&key) {
        return settled;
    }
    if in_progress.contains(&key) {
        return previous_round
            .get(&key)
            .copied()
            .unwrap_or(Some(Origin::Safe));
    }

    in_progress.insert(key);
    let statement_count = function.blocks[block_id].statements.len();
    let result = trace_within_block(
        local,
        block_id,
        statement_count,
        function,
        predecessors,
        previous_round,
        in_progress,
        this_round,
    );
    in_progress.remove(&key);
    this_round.insert(key, result);
    result
}

/// Searches `block_id`'s own statements *before* `before_index`, in reverse,
/// for a bare-local assignment to `local`. Found: follows the same
/// alias-chain-or-classify-`Ref` rule the straight-line version always used
/// (continuing the search within the same block, from that assignment's own
/// index, for an alias). Not found: falls through to whatever reaches this
/// block's own start — this block's predecessors, forked and require-all-
/// safe if there's more than one, or `Origin::Safe` (an unassigned local —
/// a parameter) if there are none at all (the function's own entry block).
fn trace_within_block(
    local: LocalId,
    block_id: BlockId,
    before_index: usize,
    function: &mir::Function,
    predecessors: &HashMap<BlockId, Vec<BlockId>>,
    previous_round: &Table,
    in_progress: &mut HashSet<(LocalId, BlockId)>,
    this_round: &mut Table,
) -> Option<Origin> {
    let block = &function.blocks[block_id];
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
                    trace_within_block(
                        alias.local,
                        block_id,
                        index,
                        function,
                        predecessors,
                        previous_round,
                        in_progress,
                        this_round,
                    )
                }
            }
            mir::Rvalue::Ref {
                place: ref_place, ..
            } => {
                let escapes_this_frame = ref_place
                    .projection
                    .iter()
                    .any(|elem| matches!(elem, mir::PlaceElem::Deref));
                Some(if escapes_this_frame {
                    Origin::Safe
                } else {
                    Origin::Dangling(ref_place.local)
                })
            }
            _ => None,
        };
    }

    let preds = predecessors.get(&block_id).filter(|preds| !preds.is_empty());
    let Some(preds) = preds else {
        // No predecessor recorded assignment reaches here — the function's
        // own entry block, `local` unassigned: a parameter, safe.
        return Some(Origin::Safe);
    };

    let mut dangling = None;
    for &pred in preds {
        match trace_from_block_start(
            local,
            pred,
            function,
            predecessors,
            previous_round,
            in_progress,
            this_round,
        )? {
            Origin::Safe => {}
            Origin::Dangling(dangling_local) => {
                dangling.get_or_insert(dangling_local);
            }
        }
    }
    Some(match dangling {
        Some(dangling_local) => Origin::Dangling(dangling_local),
        None => Origin::Safe,
    })
}

/// Every block's direct predecessors, derived from every other block's own
/// successors (mirrors `move_check::analyze`'s own construction, minus the
/// reachable-from-entry restriction — a backward trace only ever visits
/// blocks reachable *from* wherever it starts, so an unreachable block
/// simply never gets asked about).
fn build_predecessors(function: &mir::Function) -> HashMap<BlockId, Vec<BlockId>> {
    let mut predecessors: HashMap<BlockId, Vec<BlockId>> = HashMap::new();
    for (block_id, block) in function.blocks.entries() {
        for successor in successors(&block.terminator) {
            if function.blocks.get(successor).is_some() {
                predecessors.entry(successor).or_default().push(block_id);
            }
        }
    }
    predecessors
}
