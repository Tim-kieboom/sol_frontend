//! Mutable/shared-borrow overlap checking — the third slice of M2's real
//! borrow/lifetime-conflict work (see `docs/mir-design.md` and `TODO.md`'s
//! M2 section), separate from escape-checking (does a reference outlive its
//! target) and move-checking (was a place read after being moved out of):
//! do two *live* borrows of the same local conflict? `/grill-me`'d at
//! length before writing anything — settled scope:
//!
//! - **Whole-locals only**: two borrows of the same *root local* conflict
//!   regardless of which field each one actually touches (`&o.a` and
//!   `&mut o.b` are wrongly treated as conflicting even though they're
//!   disjoint) — real per-field precision is a deferred, later slice.
//! - **Reference-vs-reference only**: a live borrow blocking a *move* of its
//!   target (`cannot move out of x because it is borrowed`) is a separate,
//!   later slice, not attempted here.
//! - **`if`/`else` is now handled** (`/grill-me`'d again to extend past
//!   straight-line-only); `for`'s back edge is still gated out entirely,
//!   unanalyzed (see `has_back_edge`) — a separate, later slice, same
//!   precedent as `escape_check`'s own `if`/`else`-then-`for` sequencing.
//! - **Conflict matrix**: `&mut` vs `&mut`, and `&mut` vs `&`, overlapping
//!   in time — reject. `&` vs `&` overlapping — always fine (unlimited
//!   simultaneous shared borrows, same as real Rust).
//! - **Real NLL-accurate liveness, not a lexical-scope heuristic**: a
//!   borrow's live range is its own definition point plus every point
//!   forward-reachable from it (without passing through a redefinition of
//!   the same local) where the local is still genuinely live (some future
//!   read reaches it without an intervening redefinition) — not "until its
//!   enclosing block ends." A borrow with zero further reads is live only
//!   at its own creation point.
//!
//! # Mechanism — two genuinely separate computations
//!
//! `/grill-me`'d again once `if`/`else` was in scope: the old straight-line
//! version conflated two different questions into one linear scan (only
//! possible because straight-line code has a single total order). Once a
//! local can have more than one reaching generation, they have to be split:
//!
//! 1. **Alias tracing** (`value_of`) — finding which `Rvalue::Ref` a
//!    generation's own value ultimately traces back to. Reuses
//!    `escape_check`'s exact fork-at-a-join shape: reads a point's own
//!    statements backward for the traced local's most recent assignment,
//!    falling through to every predecessor block (forked) the moment
//!    nothing is found in-block. Unlike `escape_check`'s "require every
//!    path safe" join (a binary safe/dangling question), this asks "what
//!    does this alias resolve to" — a richer answer than yes/no — so the
//!    join rule generalizes to "require every path resolve to the *same*
//!    `(place, mutable)` answer, else `None`" (still the same "unfamiliar/
//!    disagreeing ⇒ skip, don't guess" discipline every M2 pass uses, just
//!    generalized from a boolean to an equality check).
//! 2. **Liveness** (`generation_live_points`) — a real backward dataflow
//!    pass, computed once for the whole function and reused by every
//!    generation, entirely separate from alias tracing. Two levels, the
//!    standard textbook technique (avoids needing point-level successor
//!    edges for the CFG-wide fixed point, only within a block):
//!      - Block-level backward liveness (`live_in`/`live_out` per block, via
//!        each block's own upward-exposed-uses/kill summary) — a single pass
//!        over blocks in postorder (successors settle before predecessors)
//!        suffices, since `if`/`else` alone is acyclic; no fixed-point loop
//!        needed yet (mirrors `move_check`'s own pre-loop-support first
//!        slice).
//!      - An intra-block backward scan, seeded by each block's own
//!        `live_out`, produces the exact per-point `live_in` set used below.
//!    A specific generation's own live-point set is then a forward walk from
//!    its definition, following real point-level successor edges (an
//!    ordinary CFG walk, no fixed point needed for a DAG), stopping down any
//!    path the moment either the local is no longer live there (dead, per
//!    the already-computed sets above) or gets redefined (a new generation
//!    begins there instead). A point reachable via more than one branch from
//!    genuinely different generations (e.g. `if c { r = &a } else { r = &b
//!    }` followed by a shared read of `r`) legitimately belongs to *both*
//!    generations' own live sets — this is correct, not a bug: whichever
//!    branch actually ran, its value is the one flowing through that shared
//!    read, so both possibilities have to be treated as live.
//!
//! Two borrows (from possibly different generations, of possibly different
//! locals) conflict when they resolve to the same root place, at least one
//! is mutable, and their live-point sets share any point at all — set
//! intersection, not integer-interval overlap, since a live range is no
//! longer necessarily contiguous once it can span more than one branch.

use std::collections::{HashMap, HashSet};

use mir_model::{self as mir, BlockId, LocalId};
use sol_utils::fault::Fault;

use crate::fault::{MirErrorKind, MirFault};

use super::move_check::{has_back_edge, postorder, successors};

/// See the module's own docs.
pub fn check_borrow_overlaps(function: &mir::Function) -> Vec<MirFault> {
    let mut faults = Vec::new();

    if has_back_edge(function) {
        // Contains a `for` loop — not analyzed yet, see the module's own docs.
        return faults;
    }
    let Some((entry, _)) = function.blocks.entries().next() else {
        return faults;
    };

    let points_per_block = build_points_per_block(function);
    let predecessors = build_predecessors(function);

    // Postorder (successors settle before their predecessors) is exactly
    // the processing order a single backward pass over a DAG needs.
    let backward_order = postorder(function, entry);
    let block_live_out = compute_block_liveness(function, &points_per_block, &backward_order);
    let point_live_in = compute_point_liveness(&points_per_block, &block_live_out);

    // Reversed postorder (entry-first, a valid topological order for a DAG)
    // ranks blocks for the fault-reporting "which generation is later" tie
    // break below — not load-bearing for correctness, just determinism.
    let mut exec_order = backward_order;
    exec_order.reverse();
    let block_rank: HashMap<BlockId, usize> = exec_order
        .iter()
        .enumerate()
        .map(|(rank, &block)| (block, rank))
        .collect();

    let mut memo = HashMap::new();
    let mut borrows: Vec<Borrow> = Vec::new();
    for (&block_id, points) in &points_per_block {
        for (index, point) in points.iter().enumerate() {
            let Some((local, rvalue)) = point.assign else {
                continue;
            };
            let Some((place, mutable)) =
                classify_rvalue(rvalue, block_id, index, &points_per_block, &predecessors, &mut memo)
            else {
                continue;
            };
            let mut live_points = generation_live_points(
                local,
                block_id,
                index,
                function,
                &points_per_block,
                &point_live_in,
            );
            live_points.insert((block_id, index));
            borrows.push(Borrow {
                local,
                place,
                mutable,
                def: (block_id, index),
                live_points,
            });
        }
    }

    let mut by_place: HashMap<LocalId, Vec<&Borrow>> = HashMap::new();
    for borrow in &borrows {
        by_place.entry(borrow.place).or_default().push(borrow);
    }

    for group in by_place.values() {
        for i in 0..group.len() {
            for j in (i + 1)..group.len() {
                let (a, b) = (group[i], group[j]);
                if !(a.mutable || b.mutable) {
                    continue; // shared vs. shared — always fine
                }
                if a.live_points.is_disjoint(&b.live_points) {
                    continue;
                }
                let later = if rank_of(&block_rank, a.def) >= rank_of(&block_rank, b.def) {
                    a
                } else {
                    b
                };
                faults.push(Fault::error_with_kind(
                    MirErrorKind::OverlappingBorrows,
                    Some(function.locals[later.local].span),
                ));
            }
        }
    }

    faults
}

fn rank_of(block_rank: &HashMap<BlockId, usize>, point: (BlockId, usize)) -> (usize, usize) {
    (block_rank.get(&point.0).copied().unwrap_or(0), point.1)
}

/// One flattened program point within its own block, in that block's own
/// execution order.
struct PointInfo<'a> {
    /// A whole-place `Assign(local, rvalue)` happening at this point, if any
    /// (a write through a projection isn't a fresh generation — see the
    /// module's own docs, same convention `move_check`/`escape_check` use).
    assign: Option<(LocalId, &'a mir::Rvalue)>,
    /// Every local read at this point, from the assignment's own rvalue or
    /// (for the one point built per terminator) the terminator's operands.
    reads: Vec<LocalId>,
}

struct Borrow {
    /// The local currently holding this borrow — used for the fault's span.
    local: LocalId,
    /// The root local actually being borrowed (whole-locals only — see the
    /// module's own docs).
    place: LocalId,
    mutable: bool,
    def: (BlockId, usize),
    live_points: HashSet<(BlockId, usize)>,
}

fn collect_operand_reads(operand: &mir::Operand) -> Vec<LocalId> {
    match operand {
        mir::Operand::Copy(place) | mir::Operand::Move(place) => vec![place.local],
        mir::Operand::Constant(_) => Vec::new(),
    }
}

fn collect_reads(rvalue: &mir::Rvalue) -> Vec<LocalId> {
    match rvalue {
        mir::Rvalue::Use(operand) => collect_operand_reads(operand),
        mir::Rvalue::BinaryOp(_, left, right) | mir::Rvalue::CheckedBinaryOp(_, left, right) => {
            let mut reads = collect_operand_reads(left);
            reads.extend(collect_operand_reads(right));
            reads
        }
        mir::Rvalue::UnaryOp(_, operand)
        | mir::Rvalue::Cast(operand, _)
        | mir::Rvalue::HeapAlloc(_, operand, _) => collect_operand_reads(operand),
        mir::Rvalue::Ref { place, .. } | mir::Rvalue::Len(place) => vec![place.local],
        mir::Rvalue::Aggregate(_, operands) => {
            operands.iter().flat_map(collect_operand_reads).collect()
        }
    }
}

/// Every block's own points, in execution order — same per-statement/
/// per-reading-terminator shape the old flat `build_timeline` used, just
/// keyed by block instead of concatenated into one global list.
fn build_points_per_block(function: &mir::Function) -> HashMap<BlockId, Vec<PointInfo<'_>>> {
    let mut points_per_block = HashMap::new();
    for (block_id, block) in function.blocks.entries() {
        let mut points = Vec::new();
        for statement in &block.statements {
            match statement {
                mir::Statement::Assign(place, rvalue) => {
                    let reads = collect_reads(rvalue);
                    let assign = place.projection.is_empty().then_some((place.local, rvalue));
                    points.push(PointInfo { assign, reads });
                }
                mir::Statement::MarkMoved(_)
                | mir::Statement::SetDropFlag(..)
                | mir::Statement::StorageDead(_) => {}
            }
        }

        let terminator_reads = match &block.terminator {
            mir::Terminator::Call { arguments, .. } => {
                arguments.iter().flat_map(collect_operand_reads).collect()
            }
            mir::Terminator::Assert { cond, msg, .. } => {
                let mut reads = collect_operand_reads(cond);
                reads.extend(collect_operand_reads(msg));
                reads
            }
            mir::Terminator::Goto(_)
            | mir::Terminator::Drop { .. }
            | mir::Terminator::Return
            | mir::Terminator::Unreachable => Vec::new(),
            mir::Terminator::SwitchInt { discriminant, .. } => {
                collect_operand_reads(discriminant)
            }
        };
        if !terminator_reads.is_empty() {
            points.push(PointInfo {
                assign: None,
                reads: terminator_reads,
            });
        }

        points_per_block.insert(block_id, points);
    }
    points_per_block
}

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

/// Per-block upward-exposed-use / kill summary — the standard two-level
/// liveness technique's block-granularity input.
struct BlockSummary {
    use_set: HashSet<LocalId>,
    def_set: HashSet<LocalId>,
}

fn summarize_block(points: &[PointInfo]) -> BlockSummary {
    let mut use_set = HashSet::new();
    let mut def_set = HashSet::new();
    for point in points {
        for &read in &point.reads {
            if !def_set.contains(&read) {
                use_set.insert(read);
            }
        }
        if let Some((local, _)) = point.assign {
            def_set.insert(local);
        }
    }
    BlockSummary { use_set, def_set }
}

/// Block-level backward liveness — a single pass over `order` (postorder:
/// every successor is processed before its predecessors), which is enough
/// to settle a DAG in one shot; no fixed-point loop needed yet (`for`'s
/// cyclic case is deferred, see the module's own docs).
fn compute_block_liveness(
    function: &mir::Function,
    points_per_block: &HashMap<BlockId, Vec<PointInfo<'_>>>,
    order: &[BlockId],
) -> HashMap<BlockId, HashSet<LocalId>> {
    let summaries: HashMap<BlockId, BlockSummary> = points_per_block
        .iter()
        .map(|(&block_id, points)| (block_id, summarize_block(points)))
        .collect();

    let mut live_in: HashMap<BlockId, HashSet<LocalId>> = HashMap::new();
    let mut live_out: HashMap<BlockId, HashSet<LocalId>> = HashMap::new();

    for &block_id in order {
        let mut out = HashSet::new();
        if let Some(block) = function.blocks.get(block_id) {
            for successor in successors(&block.terminator) {
                if let Some(succ_in) = live_in.get(&successor) {
                    out.extend(succ_in.iter().copied());
                }
            }
        }

        let summary = &summaries[&block_id];
        let mut inn = out.clone();
        for def in &summary.def_set {
            inn.remove(def);
        }
        inn.extend(summary.use_set.iter().copied());

        live_out.insert(block_id, out);
        live_in.insert(block_id, inn);
    }

    live_out
}

fn compute_point_liveness(
    points_per_block: &HashMap<BlockId, Vec<PointInfo<'_>>>,
    block_live_out: &HashMap<BlockId, HashSet<LocalId>>,
) -> HashMap<(BlockId, usize), HashSet<LocalId>> {
    let mut live_in = HashMap::new();
    for (&block_id, points) in points_per_block {
        let mut live_out = block_live_out.get(&block_id).cloned().unwrap_or_default();
        for index in (0..points.len()).rev() {
            let mut current = live_out.clone();
            if let Some((local, _)) = points[index].assign {
                current.remove(&local);
            }
            for &read in &points[index].reads {
                current.insert(read);
            }
            live_in.insert((block_id, index), current.clone());
            live_out = current;
        }
    }
    live_in
}

/// The value `local` holds immediately before position `before_index` in
/// `block_id` — searches this block's own points in reverse, falling
/// through to every predecessor (forked, requiring every path to resolve to
/// the *same* answer) the moment nothing is found. Mirrors `escape_check`'s
/// `trace_within_block`/`trace_from_block_start` pairing exactly, save for
/// the join rule (equality, not "all safe" — see the module's own docs).
fn value_of(
    local: LocalId,
    block_id: BlockId,
    before_index: usize,
    points_per_block: &HashMap<BlockId, Vec<PointInfo<'_>>>,
    predecessors: &HashMap<BlockId, Vec<BlockId>>,
    memo: &mut HashMap<(LocalId, BlockId), Option<(LocalId, bool)>>,
) -> Option<(LocalId, bool)> {
    let points = &points_per_block[&block_id];
    for index in (0..before_index).rev() {
        let Some((assigned_local, rvalue)) = points[index].assign else {
            continue;
        };
        if assigned_local != local {
            continue;
        }
        return classify_rvalue(rvalue, block_id, index, points_per_block, predecessors, memo);
    }

    if let Some(&cached) = memo.get(&(local, block_id)) {
        return cached;
    }
    let preds = predecessors.get(&block_id).filter(|preds| !preds.is_empty());
    let result = match preds {
        None => None, // entry block, no earlier assignment: a parameter, not a tracked borrow.
        Some(preds) => {
            let mut agreed: Option<(LocalId, bool)> = None;
            let mut sound = true;
            for &pred in preds {
                let pred_points = points_per_block[&pred].len();
                let pred_result = value_of(local, pred, pred_points, points_per_block, predecessors, memo);
                match (agreed, pred_result) {
                    (_, None) => {
                        sound = false;
                        break;
                    }
                    (None, Some(value)) => agreed = Some(value),
                    (Some(existing), Some(value)) if existing == value => {}
                    _ => {
                        sound = false;
                        break;
                    }
                }
            }
            if sound { agreed } else { None }
        }
    };
    memo.insert((local, block_id), result);
    result
}

/// Classifies one specific, already-known assignment's rvalue: a fresh,
/// `Deref`-free `Rvalue::Ref` is a genuine borrow of its own `place.local`
/// (any `Field`/`Index` projection coarsened away, whole-locals only — see
/// the module's own docs); a bare-local alias continues the search via
/// `value_of`; anything else isn't a tracked local-place borrow.
fn classify_rvalue(
    rvalue: &mir::Rvalue,
    block_id: BlockId,
    at_index: usize,
    points_per_block: &HashMap<BlockId, Vec<PointInfo<'_>>>,
    predecessors: &HashMap<BlockId, Vec<BlockId>>,
    memo: &mut HashMap<(LocalId, BlockId), Option<(LocalId, bool)>>,
) -> Option<(LocalId, bool)> {
    match rvalue {
        mir::Rvalue::Use(mir::Operand::Copy(place) | mir::Operand::Move(place)) => {
            if !place.projection.is_empty() {
                return None;
            }
            value_of(place.local, block_id, at_index, points_per_block, predecessors, memo)
        }
        mir::Rvalue::Ref { place, mutable } => {
            let has_deref = place
                .projection
                .iter()
                .any(|elem| matches!(elem, mir::PlaceElem::Deref));
            (!has_deref).then_some((place.local, *mutable))
        }
        _ => None,
    }
}

/// Every point forward-reachable from `(def_block, def_index)` without
/// crossing a redefinition of `local`, restricted to points where `local`
/// is actually still live (per the already-computed dataflow) — see the
/// module's own docs on why a point reached via more than one branch from
/// genuinely different generations is correct, not a bug.
fn generation_live_points(
    local: LocalId,
    def_block: BlockId,
    def_index: usize,
    function: &mir::Function,
    points_per_block: &HashMap<BlockId, Vec<PointInfo<'_>>>,
    point_live_in: &HashMap<(BlockId, usize), HashSet<LocalId>>,
) -> HashSet<(BlockId, usize)> {
    let mut result = HashSet::new();
    let mut visited = HashSet::new();
    let mut stack = point_successors(def_block, def_index, function, points_per_block);

    while let Some((block_id, index)) = stack.pop() {
        if !visited.insert((block_id, index)) {
            continue;
        }
        let Some(live) = point_live_in.get(&(block_id, index)) else {
            continue;
        };
        if !live.contains(&local) {
            continue;
        }
        result.insert((block_id, index));

        let redefines = points_per_block[&block_id][index]
            .assign
            .is_some_and(|(assigned, _)| assigned == local);
        if !redefines {
            stack.extend(point_successors(block_id, index, function, points_per_block));
        }
    }

    result
}

/// The point(s) immediately following `(block_id, index)`: the next point
/// in the same block, or — at a block's last point — the first point of
/// each CFG-successor block, skipping transitively through any successor
/// with no points of its own (an all-`Drop`/`Goto` scope-exit chain, the
/// same shape `escape_check`'s backward walk falls through in reverse).
fn point_successors(
    block_id: BlockId,
    index: usize,
    function: &mir::Function,
    points_per_block: &HashMap<BlockId, Vec<PointInfo<'_>>>,
) -> Vec<(BlockId, usize)> {
    let points = &points_per_block[&block_id];
    if index + 1 < points.len() {
        return vec![(block_id, index + 1)];
    }

    let mut result = Vec::new();
    let mut visited = HashSet::new();
    let Some(block) = function.blocks.get(block_id) else {
        return result;
    };
    let mut stack = successors(&block.terminator);
    while let Some(next) = stack.pop() {
        if !visited.insert(next) {
            continue;
        }
        match points_per_block.get(&next) {
            Some(pts) if !pts.is_empty() => result.push((next, 0)),
            _ => {
                if let Some(next_block) = function.blocks.get(next) {
                    stack.extend(successors(&next_block.terminator));
                }
            }
        }
    }
    result
}
