//! Use-after-move checking — M2's move checker (see`TODO.md`'s M2 section).
//! A forward dataflow pass over a function's whole
//! CFG, loops included, tracking which locals have been moved and rejecting
//! any read of a place after it was moved without an intervening
//! whole-place reinitializing write.
//!
//! This is move-checking only: whether a place was read after being moved
//! out of. Real borrow/lifetime-conflict checking (do two live borrows of
//! the same place overlap) is a separate analysis this doesn't attempt.

use std::collections::VecDeque;

use mir_model::{self as mir, BlockId, LocalId};
use sol_utils::{
    collections::{vec_map::VecMap, vec_set::VecSet},
    fault::{Fault, FaultCollector},
};

use crate::fault::{MirErrorKind, MirFault};

/// Checks `function` for reads of a place after it was moved. Returns one
/// fault per violating read.
pub fn check_moves(function: &mir::Function) -> Vec<MirFault> {
    let Some(analysis) = analyze(function) else {
        return vec![];
    };
    check_moves_with(function, &analysis)
}

/// Rewrites `function`'s `Drop` terminators in place to reflect whether the
/// dropped local was actually moved by that point: a `Drop` becomes a no-op
/// `Goto` when the local is moved on every path reaching it, is marked
/// `guarded` (a runtime check of the local's own drop flag, see
/// `mir_model::Terminator::Drop`) when it's moved on only some paths, and is
/// left unconditional when it's never moved on any path.
pub fn elaborate_drops(function: &mut mir::Function) {
    let Some(analysis) = analyze(function) else {
        return;
    };
    elaborate_drops_with(function, &analysis);
}

/// Runs `check_moves` and `elaborate_drops` off one shared `analyze` pass —
/// both otherwise recompute the same fixed point over `function`'s CFG
/// independently, which is wasted work when a caller wants both anyway.
pub fn check_and_elaborate_moves(function: &mut mir::Function) -> Vec<MirFault> {
    let Some(analysis) = analyze(function) else {
        return Vec::new();
    };
    let faults = check_moves_with(function, &analysis);
    elaborate_drops_with(function, &analysis);
    faults
}

fn check_moves_with(function: &mir::Function, analysis: &Analysis) -> Vec<MirFault> {
    let mut faults = FaultCollector::default();

    for &block_id in &analysis.order {
        let mut state = join(
            block_id,
            &analysis.predecessors,
            &analysis.out_states,
            &analysis.all_locals,
        );

        check_block(
            function,
            &function.blocks[block_id],
            &mut state,
            &mut faults,
        );
    }

    faults.into_vec()
}

fn elaborate_drops_with(function: &mut mir::Function, analysis: &Analysis) {
    for &block_id in &analysis.order {
        let Some(state) = analysis.out_states.get(block_id) else {
            continue;
        };

        let block = &mut function.blocks[block_id];
        let mir::Terminator::Drop {
            place,
            target,
            guarded,
        } = &block.terminator
        else {
            continue;
        };

        if !place.projection.is_empty() {
            continue;
        }

        if state.definite.contains(place.local) {
            block.terminator = mir::Terminator::Goto(*target);
        } else if state.maybe.contains(place.local) && !guarded {
            block.terminator = mir::Terminator::Drop {
                place: place.clone(),
                target: *target,
                guarded: true,
            };
        }
    }
}

struct Analysis {
    order: Vec<BlockId>,
    predecessors: VecMap<BlockId, Vec<BlockId>>,
    all_locals: VecSet<LocalId>,
    out_states: VecMap<BlockId, MoveState>,
}

// Tracked two ways: "maybe" (moved on *any* path reaching this point, union
// at a join) and "definite" (moved on *every* path reaching this point,
// intersection at a join). `check_moves` only reads "maybe" (the sound
// answer for a use); `elaborate_drops` reads both to tell apart "never
// moved," "always moved," and "moved on some paths" for a given `Drop`.
#[derive(Clone, PartialEq, Eq, Default)]
struct MoveState {
    maybe: VecSet<LocalId>,
    definite: VecSet<LocalId>,
}

fn analyze(function: &mir::Function) -> Option<Analysis> {
    let (entry, _) = function.blocks.entries().next()?;

    let mut order = super::postorder(function, entry);
    order.reverse();

    let mut predecessors: VecMap<BlockId, Vec<BlockId>> = VecMap::new();
    for &block_id in &order {
        for successor in super::successors(&function.blocks[block_id].terminator) {
            if function.blocks.get(successor).is_none() {
                continue;
            }

            predecessors.entry(successor).or_default().push(block_id);
        }
    }

    let all_locals: VecSet<LocalId> = function.locals.entries().map(|(id, _)| id).collect();
    let out_states = fixed_point(function, &order, &predecessors, &all_locals);

    Some(Analysis {
        order,
        predecessors,
        all_locals,
        out_states,
    })
}

fn fixed_point(
    function: &mir::Function,
    order: &[BlockId],
    predecessors: &VecMap<BlockId, Vec<BlockId>>,
    all_locals: &VecSet<LocalId>,
) -> VecMap<BlockId, MoveState> {
    let mut out_states: VecMap<BlockId, MoveState> = VecMap::new();
    let mut queued: VecSet<BlockId> = order.iter().copied().collect();
    let mut queue: VecDeque<BlockId> = order.iter().copied().collect();

    while let Some(block_id) = queue.pop_front() {
        queued.remove(block_id);

        let mut state = join(block_id, predecessors, &out_states, all_locals);
        check_block(
            function,
            &function.blocks[block_id],
            &mut state,
            &mut FaultCollector::default(),
        );

        let changed = match out_states.get(block_id) {
            Some(existing) => *existing != state,
            None => true,
        };

        if !changed {
            continue;
        }

        out_states.insert(block_id, state);
        for successor in super::successors(&function.blocks[block_id].terminator) {
            if function.blocks.get(successor).is_some() && queued.insert(successor).is_none() {
                queue.push_back(successor);
            }
        }
    }

    out_states
}

fn join(
    block_id: BlockId,
    predecessors: &VecMap<BlockId, Vec<BlockId>>,
    out_states: &VecMap<BlockId, MoveState>,
    all_locals: &VecSet<LocalId>,
) -> MoveState {
    let Some(preds) = predecessors.get(block_id) else {
        return MoveState::default();
    };

    let mut maybe = VecSet::new();
    let mut definite: Option<VecSet<LocalId>> = None;
    for pred in preds {
        let Some(state) = out_states.get(*pred) else {
            continue;
        };

        maybe.extend(state.maybe.entries());
        definite = Some(match definite {
            None => state.definite.clone(),
            Some(acc) => acc.intersection(&state.definite).collect(),
        });
    }

    MoveState {
        maybe,
        // A predecessor not yet processed contributes nothing to "definite":
        // starting from the universal set (not empty) keeps that the case.
        definite: definite.unwrap_or_else(|| all_locals.clone()),
    }
}

fn check_block(
    function: &mir::Function,
    block: &mir::BasicBlock,
    state: &mut MoveState,
    faults: &mut FaultCollector<MirErrorKind>,
) {
    for statement in &block.statements {
        match statement {
            mir::Statement::Assign(place, rvalue) => {
                check_rvalue(function, rvalue, state, faults);
                // A whole-place write reinitializes the local. A write
                // through a projection (`x.field = ..`) doesn't, so it's
                // deliberately left in the moved sets.
                if place.projection.is_empty() {
                    state.maybe.remove(place.local);
                    state.definite.remove(place.local);
                }
            }
            mir::Statement::MarkMoved(_)
            | mir::Statement::SetDropFlag(..)
            | mir::Statement::StorageDead(_) => {}
        }
    }

    match &block.terminator {
        mir::Terminator::Call { arguments, .. } => {
            for argument in arguments {
                check_operand(function, argument, state, faults);
            }
        }
        mir::Terminator::Assert { cond, msg, .. } => {
            check_operand(function, cond, state, faults);
            check_operand(function, msg, state, faults);
        }
        mir::Terminator::SwitchInt { discriminant, .. } => {
            check_operand(function, discriminant, state, faults);
        }
        mir::Terminator::Goto(_)
        | mir::Terminator::Drop { .. }
        | mir::Terminator::Return
        | mir::Terminator::Unreachable => {}
    }
}

fn check_rvalue(
    function: &mir::Function,
    rvalue: &mir::Rvalue,
    state: &mut MoveState,
    faults: &mut FaultCollector<MirErrorKind>,
) {
    match rvalue {
        mir::Rvalue::Use(operand) => check_operand(function, operand, state, faults),
        mir::Rvalue::BinaryOp(_, left, right) | mir::Rvalue::CheckedBinaryOp(_, left, right) => {
            check_operand(function, left, state, faults);
            check_operand(function, right, state, faults);
        }
        mir::Rvalue::UnaryOp(_, operand)
        | mir::Rvalue::Cast(operand, _)
        | mir::Rvalue::HeapAlloc(_, operand, _) => {
            check_operand(function, operand, state, faults);
        }
        mir::Rvalue::Ref { place, .. } | mir::Rvalue::Len(place) => {
            check_place(function, place, &state.maybe, faults);
        }
        mir::Rvalue::Aggregate(_, operands) => {
            for operand in operands {
                check_operand(function, operand, state, faults);
            }
        }
    }
}

fn check_operand(
    function: &mir::Function,
    operand: &mir::Operand,
    state: &mut MoveState,
    faults: &mut FaultCollector<MirErrorKind>,
) {
    match operand {
        mir::Operand::Copy(place) => check_place(function, place, &state.maybe, faults),
        mir::Operand::Move(place) => {
            // Check against what was moved *before* this move, then fold
            // this one in — so a later read is what gets flagged, not this
            // move against itself.
            check_place(function, place, &state.maybe, faults);
            if place.projection.is_empty() {
                state.maybe.insert(place.local);
                state.definite.insert(place.local);
            }
        }
        mir::Operand::Constant(_) => {}
    }
}

fn check_place(
    function: &mir::Function,
    place: &mir::Place,
    maybe: &VecSet<LocalId>,
    faults: &mut FaultCollector<MirErrorKind>,
) {
    // Only the root local is checked: there's no partial-move tracking, so a
    // moved local's fields are exactly as invalid as the local itself.
    if maybe.contains(place.local) {
        let span = function.locals[place.local].span;
        faults.push(Fault::error_with_kind(
            MirErrorKind::UseAfterMove,
            Some(span),
        ));
    }
}
