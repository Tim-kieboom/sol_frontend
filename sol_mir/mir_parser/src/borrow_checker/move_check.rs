//! Use-after-move checking — M2's move checker (see `docs/mir-design.md` and
//! `TODO.md`'s M2 section). A forward dataflow pass over the function's
//! whole CFG, including loops: a block's incoming "maybe moved" state is the
//! *union* of its predecessors' outgoing state (a local counts as
//! maybe-moved if it was moved on *any* path reaching this point) — this is
//! what makes an `if`/`else`'s two branches see independent state (a move
//! inside one arm never poisons the other) while still correctly rejecting a
//! use after the join that isn't safe on every path. Mirrors rustc's own
//! move-checker shape (a `MaybeInitializedPlaces`-style dataflow, not a
//! flow-insensitive "moved anywhere ⇒ reject" scan).
//!
//! A `for` loop's back edge makes the CFG cyclic: a loop header's own
//! incoming state depends on the state at the end of the loop body, which
//! depends on the header — a single top-to-bottom pass can't answer that.
//! This is solved with a real fixed-point worklist (Kildall's algorithm):
//! every block starts at "nothing moved," gets reprocessed whenever a
//! predecessor's outgoing state changes, and the whole pass stops once
//! nothing changes anywhere. This always terminates: a block's own
//! move/reassignment statements are a fixed, deterministic function of
//! whatever comes in, so its outgoing state can only ever *grow* across
//! successive reprocessings (more locals flagged maybe-moved, never fewer),
//! and there are only finitely many locals to flag.
//!
//! The fixed-point computation itself never collects faults — an
//! intermediate, not-yet-converged state can look safe when it isn't (a
//! loop body's own move hasn't propagated back to the header yet), so
//! checking for violations mid-convergence risks both false negatives and
//! duplicate reports as a block gets reprocessed. Faults are only ever
//! collected in a separate final pass, once every block's state has
//! settled — the same split rustc itself uses (compute the dataflow first,
//! run diagnostics over the settled result second).
//!
//! Per-block state is derived entirely from `Operand::Move`/`Copy`
//! occurrences and `Assign`'s own reinitializing writes — deliberately *not*
//! from the `MarkMoved`/`SetDropFlag` statements `mir_parser::function` also
//! emits alongside every move. Those are pushed as statements immediately
//! *before* the terminator (or `Rvalue`) that structurally embeds the very
//! `Operand::Move` they correspond to — so treating `MarkMoved(local)` as
//! "moved from this point on" would flag that same, legitimate move as a
//! use-after-move against itself the moment its own consuming terminator is
//! checked. Deriving state straight from where `Operand::Move` actually
//! appears sidesteps this entirely: a `Move` is checked against the
//! already-moved set *before* being folded into it, exactly once, at its own
//! true point of use — no ambiguity about which `MarkMoved` belongs to which
//! read.
//!
//! Two parallel sets are tracked per block: "maybe moved" (a local counts the
//! instant it's moved on *any* path reaching this point — union at a join,
//! never cleared except by a whole-place reinitializing write) and
//! "definitely moved" (a local only counts once it's moved on *every* path
//! reaching this point — intersection at a join). `check_moves` only ever
//! reads "maybe": that's the sound, conservative answer a *use* needs (reject
//! the moment a move is possible on any path). `elaborate_drops` reads both,
//! to tell apart three cases for a given `Drop` site: not in "maybe" at all
//! (never moved — leave the `Drop` unconditional, the cheap common case);
//! in "definite" (moved on every path — prune to a no-op `Goto`, as before);
//! in "maybe" but not "definite" (moved on some but not all paths — neither
//! prior answer is sound, so the `Drop` is marked `guarded` instead, pushing
//! the actual decision to a runtime check of the local's own drop flag at
//! codegen; see `mir_model::Terminator::Drop`'s own docs). Before this,
//! `elaborate_drops` only had "maybe" to work with, so it had to treat
//! "maybe" and "definite" as the same case (prune on *any* hint of a move),
//! which leaked on the untaken branch of a value moved on only one arm of an
//! `if`/`else` — see M2's TODO.md entry.
//!
//! The join rule for "definite" is the mirror image of "maybe"'s: a join
//! starts from the *universal* set (every local in the function) and
//! shrinks via intersection as each predecessor's own settled state is
//! folded in, rather than starting from empty and growing via union. This
//! keeps the same "an unprocessed predecessor contributes nothing" shape
//! `join`'s "maybe" half already uses — for "maybe" (union), contributing
//! nothing means the identity element is the empty set; for "definite"
//! (intersection), it's the universal set instead. A block with zero real
//! predecessors (the entry block) still gets an empty "definite" set for
//! both, same as "maybe" — nothing has moved yet at the very start of the
//! function, definitely or otherwise.
//!
//! Real borrow/lifetime-conflict checking (do two live borrows of the same
//! place overlap) is a different, much bigger analysis this doesn't attempt
//! at all yet — nothing in the MIR tracks borrow liveness. This is move-
//! checking only: was this place read after being moved out of.

use std::collections::VecDeque;

use mir_model::{self as mir, BlockId, LocalId};
use sol_utils::{
    collections::{vec_map::VecMap, vec_set::VecSet},
    fault::Fault,
};

use crate::fault::{MirErrorKind, MirFault};

/// A block's own moved-state, tracked two ways at once — see the module's
/// own docs for why both are needed and how each one's join rule differs.
#[derive(Clone, PartialEq, Eq, Default)]
struct MoveState {
    maybe: VecSet<LocalId>,
    definite: VecSet<LocalId>,
}

/// See the module's own docs. Returns one fault per violating read.
pub fn check_moves(function: &mir::Function) -> Vec<MirFault> {
    let mut faults = Vec::new();

    let Some(analysis) = analyze(function) else {
        return faults;
    };

    // Final, settled-state pass — the only place faults are ever collected.
    // Only "maybe" is relevant to a use-after-move check (see module docs).
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

    faults
}

/// Rewrites `function` in place: `mir_parser::function`'s own lowering emits
/// an unconditional (`guarded: false`) `Drop` for every tracked owning local
/// at every scope exit, with no move-awareness at all (see the module's own
/// docs on why that's deliberate, not an oversight). This is what turns each
/// `Drop` into its real, settled shape: a no-op `Goto` when the local is
/// definitely moved by then on every path; `guarded: true` (a runtime check
/// of the local's own drop flag, per `mir_model::Terminator::Drop`'s docs)
/// when it's moved on some but not all paths; left unconditional when it's
/// never moved at all on any path reaching this point — mirrors rustc's own
/// `elaborate_drops` pass, drop-flag machinery included this time (an earlier
/// version of this pass predates the "definite" half of this analysis and
/// only had the unconditional/no-op choice — see M2's TODO.md entry on the
/// leak that caused).
pub fn elaborate_drops(function: &mut mir::Function) {
    let Some(analysis) = analyze(function) else {
        return;
    };

    for block_id in analysis.order {
        // The state right before a block's own terminator fires is exactly
        // its settled outgoing state: `check_block`'s terminator handling
        // never itself adds to or removes from `state` (a `Drop`/`Goto`/
        // `Return`/... reads or redirects control, it doesn't move anything),
        // so `out_states[block_id]` — computed *including* that block's own
        // statements — already is "what's moved by the time this Drop runs."
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

/// The shared dataflow computation both `check_moves` and `elaborate_drops`
/// consume: every block reachable from the function's entry, a good
/// processing order for it, its predecessor map, every local in the function
/// (the "definite" join's own universal-set identity — see module docs), and
/// each block's settled outgoing `MoveState`. `None` only when `function` has
/// no blocks at all (nothing to analyze).
struct Analysis {
    order: Vec<BlockId>,
    predecessors: VecMap<BlockId, Vec<BlockId>>,
    all_locals: VecSet<LocalId>,
    out_states: VecMap<BlockId, MoveState>,
}

fn analyze(function: &mir::Function) -> Option<Analysis> {
    let (entry, _) = function.blocks.entries().next()?;

    // Reverse postorder is just a good *initial* processing order (forward
    // edges converge in one pass); it's not load-bearing for correctness the
    // way it was before loops were handled — `fixed_point` reprocesses
    // whatever the worklist tells it to, however many times that takes.
    let mut order = postorder(function, entry);
    order.reverse();

    let mut predecessors: VecMap<BlockId, Vec<BlockId>> = VecMap::new();
    for &block_id in &order {
        for successor in successors(&function.blocks[block_id].terminator) {
            if function.blocks.get(successor).is_some() {
                predecessors.entry(successor).or_default().push(block_id);
            }
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

/// Runs the block-transfer function to a fixed point over the whole CFG
/// (loops included) and returns each block's settled outgoing `MoveState`.
/// Never collects faults — see the module's own docs on why that has to wait
/// for a separate, final pass over the settled result.
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
        // A scratch, discarded fault list — this call is purely to compute
        // the resulting state, never to report anything (see module docs).
        check_block(
            function,
            &function.blocks[block_id],
            &mut state,
            &mut Vec::new(),
        );

        let changed = match out_states.get(block_id) {
            Some(existing) => *existing != state,
            None => true,
        };
        if changed {
            out_states.insert(block_id, state);
            for successor in successors(&function.blocks[block_id].terminator) {
                if function.blocks.get(successor).is_some() && queued.insert(successor).is_none() {
                    queue.push_back(successor);
                }
            }
        }
    }

    out_states
}

/// A block's incoming state. "Maybe": the union of its predecessors' current
/// outgoing "maybe" state (an absent predecessor — not yet processed —
/// contributes nothing yet, the empty set being union's identity element;
/// the worklist guarantees this block is revisited once that predecessor's
/// own state exists). "Definite": the mirror image — the *intersection* of
/// only the predecessors that have been processed so far, an absent
/// predecessor contributing the universal set (`all_locals`), intersection's
/// own identity element — see the module's own docs for why starting from
/// "everything" and shrinking down converges to the correct answer instead
/// of transiently overclaiming something as definite that a not-yet-seen
/// predecessor would rule out. A block with no real predecessors at all
/// (the entry block) gets the empty set for both — nothing has moved yet.
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
        if let Some(state) = out_states.get(*pred) {
            maybe.extend(state.maybe.entries());
            definite = Some(match definite {
                None => state.definite.clone(),
                Some(acc) => acc.intersection(&state.definite).collect(),
            });
        }
    }
    MoveState {
        maybe,
        definite: definite.unwrap_or_else(|| all_locals.clone()),
    }
}

/// Every block reachable from `entry`, in postorder (a block is pushed only
/// after every block reachable from it has already been pushed) — reversing
/// this gives a reasonable initial worklist order. Terminates on a cyclic
/// CFG same as an acyclic one: `visited` just stops a back edge from being
/// followed again, it isn't treated as an error the way it was before loops
/// were handled.
pub(super) fn postorder(function: &mir::Function, entry: BlockId) -> Vec<BlockId> {
    fn visit(
        function: &mir::Function,
        block_id: BlockId,
        visited: &mut VecSet<BlockId>,
        order: &mut Vec<BlockId>,
    ) {
        if visited.insert(block_id).is_some() {
            return;
        }
        let Some(block) = function.blocks.get(block_id) else {
            return;
        };
        for successor in successors(&block.terminator) {
            visit(function, successor, visited, order);
        }
        order.push(block_id);
    }

    let mut visited = VecSet::new();
    let mut order = Vec::new();
    visit(function, entry, &mut visited, &mut order);
    order
}

pub(super) fn successors(terminator: &mir::Terminator) -> Vec<BlockId> {
    match terminator {
        mir::Terminator::Goto(target) | mir::Terminator::Drop { target, .. } => vec![*target],
        mir::Terminator::SwitchInt {
            targets, otherwise, ..
        } => {
            let mut result: Vec<BlockId> = targets.iter().map(|(_, id)| *id).collect();
            result.push(*otherwise);
            result
        }
        mir::Terminator::Call { target, .. } => target.iter().copied().collect(),
        mir::Terminator::Assert { target, .. } => vec![*target],
        mir::Terminator::Return | mir::Terminator::Unreachable => Vec::new(),
    }
}

/// Walks one block's own statements and terminator, checking every operand
/// against `state.maybe` (mutated in place, alongside `state.definite` in
/// lockstep — within one block's own straight-line statements, both sets
/// only ever change together: a move inserts into both, a whole-place reinit
/// removes from both. They diverge only at a join between blocks, via
/// `join`'s two different rules — see the module's own docs) and appending
/// any violation found.
fn check_block(
    function: &mir::Function,
    block: &mir::BasicBlock,
    state: &mut MoveState,
    faults: &mut Vec<MirFault>,
) {
    for statement in &block.statements {
        match statement {
            mir::Statement::Assign(place, rvalue) => {
                check_rvalue(function, rvalue, state, faults);
                // A whole-place write reinitializes the local — legal to
                // reassign after a move (`docs/mir-design.md`'s own move/
                // drop rule). A write through a projection (`x.field = ..`)
                // doesn't fully reinitialize `x` as a whole, so it's
                // deliberately not cleared here — a narrower, separate gap
                // (see the module's own docs).
                if place.projection.is_empty() {
                    state.maybe.remove(place.local);
                    state.definite.remove(place.local);
                }
            }
            // Deliberately not used for state — see the module's own docs
            // on why deriving state from `Operand::Move` directly avoids
            // the ordering trap these would otherwise cause.
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
    faults: &mut Vec<MirFault>,
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
            // Read-only: borrowing/measuring a place never moves it, so
            // unlike `Operand::Move` this never inserts into `state`.
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
    faults: &mut Vec<MirFault>,
) {
    match operand {
        mir::Operand::Copy(place) => check_place(function, place, &state.maybe, faults),
        mir::Operand::Move(place) => {
            check_place(function, place, &state.maybe, faults);
            // The point of use *is* the move — check against whatever was
            // moved before it, then fold this one in, so a second read
            // (another `Move`, or any later `Copy`/`Ref`) of the same local
            // is what actually gets flagged, never this one against itself.
            if place.projection.is_empty() {
                state.maybe.insert(place.local);
                state.definite.insert(place.local);
            }
        }
        mir::Operand::Constant(_) => {}
    }
}

/// Only the place's *root* local is checked, regardless of its own
/// projection — moving a whole local invalidates everything reachable
/// through it, including a field/index/deref read (there's no partial-move
/// tracking, see the module's own docs and M2's TODO.md entry, so a moved
/// local's fields are exactly as invalid as the local itself). An index
/// projection's own `LocalId` (`PlaceElem::Index`) is never itself checked —
/// harmless, since an index is always a primitive, always `AutoCopy`, never
/// markable as moved in the first place. Checked against `maybe` only — the
/// sound answer for "was this legal to read" (see module docs).
fn check_place(
    function: &mir::Function,
    place: &mir::Place,
    maybe: &VecSet<LocalId>,
    faults: &mut Vec<MirFault>,
) {
    if maybe.contains(place.local) {
        let span = function.locals[place.local].span;
        faults.push(Fault::error_with_kind(
            MirErrorKind::UseAfterMove,
            Some(span),
        ));
    }
}
