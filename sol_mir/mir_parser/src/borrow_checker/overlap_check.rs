//! Mutable/shared-borrow overlap checking — the third slice of M2's real
//! borrow/lifetime-conflict work (see `TODO.md`'s M2 section),
//! separate from escape-checking (does a reference outlive its target)
//! and move-checking (was a place read after being moved out of):
//! do two *live* borrows of the same local conflict?
//!
//! Handles the full concrete (M1) CFG shape, loops included, with real
//! NLL-accurate liveness (a borrow is live from its definition through every
//! point a later read can still reach it, not just "until its enclosing
//! block ends") and field-level disjointness for struct fields (`&o.a` and
//! `&mut o.b` don't conflict; array-index projections still coarsen to
//! "same root ⇒ conflict"). Conflict matrix: `&mut` vs `&mut`, and `&mut`
//! vs `&`, overlapping in time — reject; `&` vs `&` — always fine.
//!
//! `check_move_while_borrowed` handles the related move-vs-borrow question:
//! does a move of `x` happen while a still-live borrow of `x` exists.

use std::collections::VecDeque;

use mir_model::{self as mir, BlockId, LocalId};
use sol_utils::{
    collections::{vec_map::VecMap, vec_set::VecSet},
    fault::Fault,
};

use crate::fault::{MirErrorKind, MirFault};

/// Checks `function` for two live borrows of the same place that conflict
/// (a mutable borrow overlapping any other live borrow of that place).
/// Returns one fault per conflicting pair.
pub fn check_borrow_overlaps(function: &mir::Function) -> Vec<MirFault> {
    let mut faults = vec![];

    let Some((entry, _)) = function.blocks.entries().next() else {
        return faults;
    };

    let mut exec_order = super::postorder(function, entry);
    exec_order.reverse();
    let block_rank: VecMap<BlockId, usize> = exec_order
        .iter()
        .enumerate()
        .map(|(rank, &block)| (block, rank))
        .collect();

    let Analysis { borrows, .. } = analyze(function);

    let mut by_place: VecMap<LocalId, Vec<&Borrow>> = VecMap::new();
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
                if fields_disjoint(&a.projection, &b.projection) {
                    continue;
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

/// Checks `function` for a whole-place `Move` of a local while a still-live
/// borrow of it exists (rustc's "cannot move out of x because it is
/// borrowed") — a move conflicts with *any* live borrow of its target,
/// mutable or shared, since it invalidates the underlying storage entirely.
/// Returns one fault per such move.
pub fn check_move_while_borrowed(function: &mir::Function) -> Vec<MirFault> {
    let mut faults = Vec::new();
    let Analysis {
        points_per_block,
        borrows,
    } = analyze(function);

    let mut by_place: VecMap<LocalId, Vec<&Borrow>> = VecMap::new();
    for borrow in &borrows {
        by_place.entry(borrow.place).or_default().push(borrow);
    }

    for (block_id, points) in points_per_block.entries() {
        for (index, point) in points.iter().enumerate() {
            for &moved_local in &point.moves {
                let Some(group) = by_place.get(moved_local) else {
                    continue;
                };
                if group
                    .iter()
                    .any(|borrow| borrow.live_points.contains(&(block_id, index)))
                {
                    faults.push(Fault::error_with_kind(
                        MirErrorKind::MoveWhileBorrowed,
                        Some(function.locals[moved_local].span),
                    ));
                }
            }
        }
    }

    faults
}

struct Analysis<'a> {
    points_per_block: VecMap<BlockId, Vec<PointInfo<'a>>>,
    borrows: Vec<Borrow>,
}

fn analyze(function: &mir::Function) -> Analysis<'_> {
    let points_per_block = build_points_per_block(function);
    let predecessors = build_predecessors(function);

    let block_live_out = compute_block_liveness(function, &points_per_block, &predecessors);
    let point_live_in = compute_point_liveness(&points_per_block, &block_live_out);
    let settled_aliases = converge_aliases(&points_per_block, &predecessors);
    let alias_ctx = AliasCtx {
        points_per_block: &points_per_block,
        predecessors: &predecessors,
        previous_round: &settled_aliases,
    };

    let mut borrows: Vec<Borrow> = Vec::new();
    for (block_id, points) in points_per_block.entries() {
        for (index, point) in points.iter().enumerate() {
            let Some((local, rvalue)) = point.assign else {
                continue;
            };
            let Resolved::Value(place, projection, mutable) =
                classify_generation(rvalue, block_id, index, &alias_ctx)
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
                projection,
                mutable,
                def: (block_id, index),
                live_points,
            });
        }
    }

    Analysis {
        points_per_block,
        borrows,
    }
}

fn rank_of(block_rank: &VecMap<BlockId, usize>, point: (BlockId, usize)) -> (usize, usize) {
    (block_rank.get(point.0).copied().unwrap_or(0), point.1)
}

fn fields_disjoint(a: &[mir::PlaceElem], b: &[mir::PlaceElem]) -> bool {
    for (elem_a, elem_b) in a.iter().zip(b.iter()) {
        match (elem_a, elem_b) {
            (mir::PlaceElem::Field(index_a), mir::PlaceElem::Field(index_b)) => {
                if index_a != index_b {
                    return true;
                }
            }
            _ => return false,
        }
    }
    false
}

struct PointInfo<'a> {
    assign: Option<(LocalId, &'a mir::Rvalue)>,
    reads: Vec<LocalId>,
    moves: Vec<LocalId>,
}

struct Borrow {
    local: LocalId,
    place: LocalId,
    projection: Vec<mir::PlaceElem>,
    mutable: bool,
    def: (BlockId, usize),
    live_points: std::collections::HashSet<(BlockId, usize)>,
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

fn collect_operand_moves(operand: &mir::Operand) -> Vec<LocalId> {
    match operand {
        mir::Operand::Move(place) if place.projection.is_empty() => vec![place.local],
        mir::Operand::Move(_) | mir::Operand::Copy(_) | mir::Operand::Constant(_) => Vec::new(),
    }
}

fn collect_moves(rvalue: &mir::Rvalue) -> Vec<LocalId> {
    match rvalue {
        mir::Rvalue::Use(operand) => collect_operand_moves(operand),
        mir::Rvalue::BinaryOp(_, left, right) | mir::Rvalue::CheckedBinaryOp(_, left, right) => {
            let mut moves = collect_operand_moves(left);
            moves.extend(collect_operand_moves(right));
            moves
        }
        mir::Rvalue::UnaryOp(_, operand)
        | mir::Rvalue::Cast(operand, _)
        | mir::Rvalue::HeapAlloc(_, operand, _) => collect_operand_moves(operand),
        // Neither `Ref`'s nor `Len`'s own place is ever itself moved.
        mir::Rvalue::Ref { .. } | mir::Rvalue::Len(_) => Vec::new(),
        mir::Rvalue::Aggregate(_, operands) => {
            operands.iter().flat_map(collect_operand_moves).collect()
        }
    }
}

fn build_points_per_block(function: &mir::Function) -> VecMap<BlockId, Vec<PointInfo<'_>>> {
    let mut points_per_block = VecMap::new();
    for (block_id, block) in function.blocks.entries() {
        let mut points = Vec::new();
        for statement in &block.statements {
            match statement {
                mir::Statement::Assign(place, rvalue) => {
                    let reads = collect_reads(rvalue);
                    let moves = collect_moves(rvalue);
                    let assign = place.projection.is_empty().then_some((place.local, rvalue));
                    points.push(PointInfo {
                        assign,
                        reads,
                        moves,
                    });
                }
                mir::Statement::MarkMoved(_)
                | mir::Statement::SetDropFlag(..)
                | mir::Statement::StorageDead(_) => {}
            }
        }

        let (terminator_reads, terminator_moves): (Vec<LocalId>, Vec<LocalId>) =
            match &block.terminator {
                mir::Terminator::Call { arguments, .. } => (
                    arguments.iter().flat_map(collect_operand_reads).collect(),
                    arguments.iter().flat_map(collect_operand_moves).collect(),
                ),
                mir::Terminator::Assert { cond, msg, .. } => {
                    let mut reads = collect_operand_reads(cond);
                    reads.extend(collect_operand_reads(msg));
                    let mut moves = collect_operand_moves(cond);
                    moves.extend(collect_operand_moves(msg));
                    (reads, moves)
                }
                mir::Terminator::Goto(_)
                | mir::Terminator::Drop { .. }
                | mir::Terminator::Return
                | mir::Terminator::Unreachable => (Vec::new(), Vec::new()),
                mir::Terminator::SwitchInt { discriminant, .. } => (
                    collect_operand_reads(discriminant),
                    collect_operand_moves(discriminant),
                ),
            };
        if !terminator_reads.is_empty() {
            points.push(PointInfo {
                assign: None,
                reads: terminator_reads,
                moves: terminator_moves,
            });
        }

        points_per_block.insert(block_id, points);
    }
    points_per_block
}

fn build_predecessors(function: &mir::Function) -> VecMap<BlockId, Vec<BlockId>> {
    let mut predecessors: VecMap<BlockId, Vec<BlockId>> = VecMap::new();
    for (block_id, block) in function.blocks.entries() {
        for successor in super::successors(&block.terminator) {
            if function.blocks.get(successor).is_some() {
                predecessors.entry(successor).or_default().push(block_id);
            }
        }
    }
    predecessors
}

struct BlockSummary {
    use_set: VecSet<LocalId>,
    def_set: VecSet<LocalId>,
}

fn summarize_block(points: &[PointInfo]) -> BlockSummary {
    let mut use_set = VecSet::new();
    let mut def_set = VecSet::new();
    for point in points {
        for &read in &point.reads {
            if !def_set.contains(read) {
                use_set.insert(read);
            }
        }
        if let Some((local, _)) = point.assign {
            def_set.insert(local);
        }
    }
    BlockSummary { use_set, def_set }
}

fn compute_block_liveness(
    function: &mir::Function,
    points_per_block: &VecMap<BlockId, Vec<PointInfo<'_>>>,
    predecessors: &VecMap<BlockId, Vec<BlockId>>,
) -> VecMap<BlockId, VecSet<LocalId>> {
    let summaries: VecMap<BlockId, BlockSummary> = points_per_block
        .entries()
        .map(|(block_id, points)| (block_id, summarize_block(points)))
        .collect();

    let mut live_in: VecMap<BlockId, VecSet<LocalId>> = VecMap::new();
    let mut live_out: VecMap<BlockId, VecSet<LocalId>> = VecMap::new();

    let Some((entry, _)) = function.blocks.entries().next() else {
        return live_out;
    };

    let initial_order = super::postorder(function, entry);
    let mut queued: VecSet<BlockId> = initial_order.iter().copied().collect();
    let mut queue: VecDeque<BlockId> = initial_order.into_iter().collect();

    while let Some(block_id) = queue.pop_front() {
        queued.remove(block_id);

        let mut out = VecSet::new();
        if let Some(block) = function.blocks.get(block_id) {
            for successor in super::successors(&block.terminator) {
                if let Some(succ_in) = live_in.get(successor) {
                    out.extend(succ_in.entries());
                }
            }
        }

        let Some(summary) = summaries.get(block_id) else {
            continue;
        };
        let mut inn = out.clone();
        for def in summary.def_set.entries() {
            inn.remove(def);
        }
        inn.extend(summary.use_set.entries());

        let changed = match live_in.get(block_id) {
            Some(existing) => *existing != inn,
            None => true,
        };
        live_out.insert(block_id, out);
        if changed {
            live_in.insert(block_id, inn);
            for &pred in predecessors.get(block_id).into_iter().flatten() {
                if queued.insert(pred).is_none() {
                    queue.push_back(pred);
                }
            }
        }
    }

    live_out
}

fn compute_point_liveness(
    points_per_block: &VecMap<BlockId, Vec<PointInfo<'_>>>,
    block_live_out: &VecMap<BlockId, VecSet<LocalId>>,
) -> std::collections::HashMap<(BlockId, usize), VecSet<LocalId>> {
    let mut live_in = std::collections::HashMap::new();
    for (block_id, points) in points_per_block.entries() {
        let mut live_out = block_live_out.get(block_id).cloned().unwrap_or_default();
        for index in (0..points.len()).rev() {
            let mut current = live_out.clone();
            if let Some((local, _)) = points[index].assign {
                current.remove(local);
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

// The lattice `value_of`-style alias resolution converges over: `Unknown` is
// the join identity (an in-progress cyclic reference, skipped rather than
// treated as a disagreement); `Disagreement` is final (two different real
// answers, or a value reaching an untracked parameter).
#[derive(Clone, PartialEq, Eq)]
enum Resolved {
    Unknown,
    Value(LocalId, Vec<mir::PlaceElem>, bool),
    Disagreement,
}

fn join_resolved(a: Resolved, b: Resolved) -> Resolved {
    match (&a, &b) {
        (Resolved::Unknown, _) => b,
        (_, Resolved::Unknown) => a,
        (Resolved::Disagreement, _) | (_, Resolved::Disagreement) => Resolved::Disagreement,
        (Resolved::Value(..), Resolved::Value(..)) => {
            if a == b {
                a
            } else {
                Resolved::Disagreement
            }
        }
    }
}

type AliasTable = std::collections::HashMap<(LocalId, BlockId), Resolved>;

struct AliasCtx<'a> {
    points_per_block: &'a VecMap<BlockId, Vec<PointInfo<'a>>>,
    predecessors: &'a VecMap<BlockId, Vec<BlockId>>,
    previous_round: &'a AliasTable,
}

struct AliasState<'a> {
    in_progress: &'a mut std::collections::HashSet<(LocalId, BlockId)>,
    this_round: &'a mut AliasTable,
}

fn converge_aliases(
    points_per_block: &VecMap<BlockId, Vec<PointInfo<'_>>>,
    predecessors: &VecMap<BlockId, Vec<BlockId>>,
) -> AliasTable {
    let mut previous_round = AliasTable::new();
    loop {
        let mut this_round = AliasTable::new();
        let mut in_progress = std::collections::HashSet::new();
        let ctx = AliasCtx {
            points_per_block,
            predecessors,
            previous_round: &previous_round,
        };

        let mut state = AliasState {
            in_progress: &mut in_progress,
            this_round: &mut this_round,
        };

        for (block_id, points) in points_per_block.entries() {
            for (index, point) in points.iter().enumerate() {
                let Some((_, rvalue)) = point.assign else {
                    continue;
                };
                if let mir::Rvalue::Use(mir::Operand::Copy(place) | mir::Operand::Move(place)) =
                    rvalue
                    && place.projection.is_empty()
                {
                    resolve_before_index(place.local, block_id, index, &ctx, &mut state);
                }
            }
        }

        if this_round == previous_round {
            return this_round;
        }

        previous_round = this_round;
    }
}

fn resolve_from_block_start(
    local: LocalId,
    block_id: BlockId,
    ctx: &AliasCtx,
    state: &mut AliasState,
) -> Resolved {
    let key = (local, block_id);
    if let Some(settled) = state.this_round.get(&key) {
        return settled.clone();
    }

    if state.in_progress.contains(&key) {
        return ctx
            .previous_round
            .get(&key)
            .cloned()
            .unwrap_or(Resolved::Unknown);
    }

    state.in_progress.insert(key);
    let statement_count = ctx.points_per_block[block_id].len();
    let result = resolve_before_index(local, block_id, statement_count, ctx, state);
    state.in_progress.remove(&key);
    state.this_round.insert(key, result.clone());
    result
}

fn resolve_before_index(
    local: LocalId,
    block_id: BlockId,
    before_index: usize,
    ctx: &AliasCtx,
    state: &mut AliasState,
) -> Resolved {
    let points = &ctx.points_per_block[block_id];
    for index in (0..before_index).rev() {
        let Some((assigned_local, rvalue)) = points[index].assign else {
            continue;
        };
        if assigned_local != local {
            continue;
        }
        return classify_generation_resolved(rvalue, block_id, index, ctx, state);
    }

    let preds = ctx
        .predecessors
        .get(block_id)
        .filter(|preds| !preds.is_empty());

    let Some(preds) = preds else {
        return Resolved::Disagreement;
    };

    let mut combined = Resolved::Unknown;
    for &pred in preds {
        let pred_value = resolve_from_block_start(local, pred, ctx, state);
        combined = join_resolved(combined, pred_value);
    }
    combined
}

fn classify_generation_resolved(
    rvalue: &mir::Rvalue,
    block_id: BlockId,
    at_index: usize,
    ctx: &AliasCtx,
    state: &mut AliasState,
) -> Resolved {
    match rvalue {
        mir::Rvalue::Use(mir::Operand::Copy(place) | mir::Operand::Move(place)) => {
            if !place.projection.is_empty() {
                return Resolved::Disagreement;
            }
            resolve_before_index(place.local, block_id, at_index, ctx, state)
        }
        mir::Rvalue::Ref { place, mutable } => {
            let has_deref = place
                .projection
                .iter()
                .any(|elem| matches!(elem, mir::PlaceElem::Deref));
            if has_deref {
                Resolved::Disagreement
            } else {
                Resolved::Value(place.local, place.projection.clone(), *mutable)
            }
        }
        _ => Resolved::Disagreement,
    }
}

fn classify_generation(
    rvalue: &mir::Rvalue,
    block_id: BlockId,
    at_index: usize,
    ctx: &AliasCtx,
) -> Resolved {
    let mut in_progress = std::collections::HashSet::new();
    let mut scratch = AliasTable::new();
    let mut state = AliasState {
        in_progress: &mut in_progress,
        this_round: &mut scratch,
    };
    classify_generation_resolved(rvalue, block_id, at_index, ctx, &mut state)
}

fn generation_live_points(
    local: LocalId,
    def_block: BlockId,
    def_index: usize,
    function: &mir::Function,
    points_per_block: &VecMap<BlockId, Vec<PointInfo<'_>>>,
    point_live_in: &std::collections::HashMap<(BlockId, usize), VecSet<LocalId>>,
) -> std::collections::HashSet<(BlockId, usize)> {
    let mut result = std::collections::HashSet::new();
    let mut visited = std::collections::HashSet::new();
    let mut stack = point_successors(def_block, def_index, function, points_per_block);

    while let Some((block_id, index)) = stack.pop() {
        if !visited.insert((block_id, index)) {
            continue;
        }
        let Some(live) = point_live_in.get(&(block_id, index)) else {
            continue;
        };
        if !live.contains(local) {
            continue;
        }
        result.insert((block_id, index));

        let redefines = points_per_block[block_id][index]
            .assign
            .is_some_and(|(assigned, _)| assigned == local);
        if !redefines {
            stack.extend(point_successors(
                block_id,
                index,
                function,
                points_per_block,
            ));
        }
    }

    result
}

fn point_successors(
    block_id: BlockId,
    index: usize,
    function: &mir::Function,
    points_per_block: &VecMap<BlockId, Vec<PointInfo<'_>>>,
) -> Vec<(BlockId, usize)> {
    let points = &points_per_block[block_id];
    if index + 1 < points.len() {
        return vec![(block_id, index + 1)];
    }

    let mut result = Vec::new();
    let mut visited = VecSet::new();
    let Some(block) = function.blocks.get(block_id) else {
        return result;
    };

    let mut stack = super::successors(&block.terminator);
    while let Some(next) = stack.pop() {
        if visited.insert(next).is_some() {
            continue;
        }

        match points_per_block.get(next) {
            Some(pts) if !pts.is_empty() => result.push((next, 0)),
            _ => {
                if let Some(next_block) = function.blocks.get(next) {
                    stack.extend(super::successors(&next_block.terminator));
                }
            }
        }
    }
    result
}
