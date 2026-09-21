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
//! Only a "maybe moved" set is tracked (never cleared except by a whole-place
//! reinitializing write) — that's the sound, conservative answer both
//! consumers of this analysis need. `check_moves` uses it to decide whether a
//! *use* is legal; `elaborate_drops` uses the exact same settled state to
//! decide whether a `Drop` `mir_parser::function` already emitted (always
//! unconditionally — lowering itself is deliberately move-unaware, mirroring
//! how rustc's own MIR building over-approximates drops too) can safely be
//! turned into a no-op `Goto` instead, fixing the leak an earlier,
//! lowering-order-only heuristic used to have (see M2's TODO.md entry). A
//! separate "definitely moved on every path" set (which would let a `Drop`'s
//! runtime check be elided *and* correctly kept on paths that never actually
//! moved the value, rather than `elaborate_drops`'s current all-or-nothing
//! per-`Drop`-site decision) isn't computed here — nothing needs the extra
//! precision yet, and this compiler still only supports a purely
//! compile-time yes/no per `Drop` site, not a real conditional-at-runtime one.
//!
//! Real borrow/lifetime-conflict checking (do two live borrows of the same
//! place overlap) is a different, much bigger analysis this doesn't attempt
//! at all yet — nothing in the MIR tracks borrow liveness. This is move-
//! checking only: was this place read after being moved out of.

use std::collections::{HashMap, HashSet, VecDeque};

use mir_model::{self as mir, BlockId, LocalId};
use sol_utils::fault::Fault;

use crate::fault::{MirErrorKind, MirFault};

/// See the module's own docs. Returns one fault per violating read.
pub fn check_moves(function: &mir::Function) -> Vec<MirFault> {
    let mut faults = Vec::new();

    let Some(analysis) = analyze(function) else {
        return faults;
    };

    // Final, settled-state pass — the only place faults are ever collected.
    for &block_id in &analysis.order {
        let mut moved = join(block_id, &analysis.predecessors, &analysis.out_states);
        check_block(
            function,
            &function.blocks[block_id],
            &mut moved,
            &mut faults,
        );
    }

    faults
}

/// Rewrites `function` in place: `mir_parser::function`'s own lowering emits
/// an unconditional `Drop` for every tracked owning local at every scope
/// exit, with no move-awareness at all (see the module's own docs on why
/// that's deliberate, not an oversight). This is what turns a `Drop` into a
/// no-op `Goto` wherever this same "maybe moved" dataflow proves the value
/// might already be gone by the time that particular `Drop` would run —
/// mirrors rustc's own `elaborate_drops` pass (minus its runtime drop-flag
/// machinery, since nothing in this compiler's codegen reads a drop flag at
/// runtime yet — every decision here is baked in at compile time).
pub fn elaborate_drops(function: &mut mir::Function) {
    let Some(analysis) = analyze(function) else {
        return;
    };

    for block_id in analysis.order {
        // The state right before a block's own terminator fires is exactly
        // its settled outgoing state: `check_block`'s terminator handling
        // never itself adds to or removes from `moved` (a `Drop`/`Goto`/
        // `Return`/... reads or redirects control, it doesn't move anything),
        // so `out_states[block_id]` — computed *including* that block's own
        // statements — already is "what's moved by the time this Drop runs."
        let Some(moved) = analysis.out_states.get(&block_id) else {
            continue;
        };
        let block = &mut function.blocks[block_id];
        let mir::Terminator::Drop { place, target } = &block.terminator else {
            continue;
        };
        if place.projection.is_empty() && moved.contains(&place.local) {
            block.terminator = mir::Terminator::Goto(*target);
        }
    }
}

/// The shared dataflow computation both `check_moves` and `elaborate_drops`
/// consume: every block reachable from the function's entry, a good
/// processing order for it, its predecessor map, and each block's settled
/// "maybe moved" outgoing state. `None` only when `function` has no blocks
/// at all (nothing to analyze).
struct Analysis {
    order: Vec<BlockId>,
    predecessors: HashMap<BlockId, Vec<BlockId>>,
    out_states: HashMap<BlockId, HashSet<LocalId>>,
}

fn analyze(function: &mir::Function) -> Option<Analysis> {
    let (entry, _) = function.blocks.entries().next()?;

    // Reverse postorder is just a good *initial* processing order (forward
    // edges converge in one pass); it's not load-bearing for correctness the
    // way it was before loops were handled — `fixed_point` reprocesses
    // whatever the worklist tells it to, however many times that takes.
    let mut order = postorder(function, entry);
    order.reverse();

    let mut predecessors: HashMap<BlockId, Vec<BlockId>> = HashMap::new();
    for &block_id in &order {
        for successor in successors(&function.blocks[block_id].terminator) {
            if function.blocks.get(successor).is_some() {
                predecessors.entry(successor).or_default().push(block_id);
            }
        }
    }

    let out_states = fixed_point(function, &order, &predecessors);

    Some(Analysis {
        order,
        predecessors,
        out_states,
    })
}

/// Runs the block-transfer function to a fixed point over the whole CFG
/// (loops included) and returns each block's settled outgoing "maybe moved"
/// state. Never collects faults — see the module's own docs on why that has
/// to wait for a separate, final pass over the settled result.
fn fixed_point(
    function: &mir::Function,
    order: &[BlockId],
    predecessors: &HashMap<BlockId, Vec<BlockId>>,
) -> HashMap<BlockId, HashSet<LocalId>> {
    let mut out_states: HashMap<BlockId, HashSet<LocalId>> = HashMap::new();
    let mut queued: HashSet<BlockId> = order.iter().copied().collect();
    let mut queue: VecDeque<BlockId> = order.iter().copied().collect();

    while let Some(block_id) = queue.pop_front() {
        queued.remove(&block_id);

        let mut moved = join(block_id, predecessors, &out_states);
        // A scratch, discarded fault list — this call is purely to compute
        // the resulting state, never to report anything (see module docs).
        check_block(
            function,
            &function.blocks[block_id],
            &mut moved,
            &mut Vec::new(),
        );

        let changed = match out_states.get(&block_id) {
            Some(existing) => *existing != moved,
            None => true,
        };
        if changed {
            out_states.insert(block_id, moved);
            for successor in successors(&function.blocks[block_id].terminator) {
                if function.blocks.get(successor).is_some() && queued.insert(successor) {
                    queue.push_back(successor);
                }
            }
        }
    }

    out_states
}

/// A block's incoming state: the union of its predecessors' current
/// outgoing state (an absent predecessor — not yet processed — contributes
/// nothing yet; the worklist guarantees it'll be revisited once that
/// predecessor's own state exists).
fn join(
    block_id: BlockId,
    predecessors: &HashMap<BlockId, Vec<BlockId>>,
    out_states: &HashMap<BlockId, HashSet<LocalId>>,
) -> HashSet<LocalId> {
    let Some(preds) = predecessors.get(&block_id) else {
        return HashSet::new();
    };
    let mut joined = HashSet::new();
    for pred in preds {
        if let Some(state) = out_states.get(pred) {
            joined.extend(state.iter().copied());
        }
    }
    joined
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
        visited: &mut HashSet<BlockId>,
        order: &mut Vec<BlockId>,
    ) {
        if !visited.insert(block_id) {
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

    let mut visited = HashSet::new();
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
/// against `moved` (mutated in place) and appending any violation found.
fn check_block(
    function: &mir::Function,
    block: &mir::BasicBlock,
    moved: &mut HashSet<LocalId>,
    faults: &mut Vec<MirFault>,
) {
    for statement in &block.statements {
        match statement {
            mir::Statement::Assign(place, rvalue) => {
                check_rvalue(function, rvalue, moved, faults);
                // A whole-place write reinitializes the local — legal to
                // reassign after a move (`docs/mir-design.md`'s own move/
                // drop rule). A write through a projection (`x.field = ..`)
                // doesn't fully reinitialize `x` as a whole, so it's
                // deliberately not cleared here — a narrower, separate gap
                // (see the module's own docs).
                if place.projection.is_empty() {
                    moved.remove(&place.local);
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
                check_operand(function, argument, moved, faults);
            }
        }
        mir::Terminator::Assert { cond, msg, .. } => {
            check_operand(function, cond, moved, faults);
            check_operand(function, msg, moved, faults);
        }
        mir::Terminator::SwitchInt { discriminant, .. } => {
            check_operand(function, discriminant, moved, faults);
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
    moved: &mut HashSet<LocalId>,
    faults: &mut Vec<MirFault>,
) {
    match rvalue {
        mir::Rvalue::Use(operand) => check_operand(function, operand, moved, faults),
        mir::Rvalue::BinaryOp(_, left, right) | mir::Rvalue::CheckedBinaryOp(_, left, right) => {
            check_operand(function, left, moved, faults);
            check_operand(function, right, moved, faults);
        }
        mir::Rvalue::UnaryOp(_, operand)
        | mir::Rvalue::Cast(operand, _)
        | mir::Rvalue::HeapAlloc(_, operand, _) => {
            check_operand(function, operand, moved, faults);
        }
        mir::Rvalue::Ref { place, .. } | mir::Rvalue::Len(place) => {
            // Read-only: borrowing/measuring a place never moves it, so
            // unlike `Operand::Move` this never inserts into `moved`.
            check_place(function, place, moved, faults);
        }
        mir::Rvalue::Aggregate(_, operands) => {
            for operand in operands {
                check_operand(function, operand, moved, faults);
            }
        }
    }
}

fn check_operand(
    function: &mir::Function,
    operand: &mir::Operand,
    moved: &mut HashSet<LocalId>,
    faults: &mut Vec<MirFault>,
) {
    match operand {
        mir::Operand::Copy(place) => check_place(function, place, moved, faults),
        mir::Operand::Move(place) => {
            check_place(function, place, moved, faults);
            // The point of use *is* the move — check against whatever was
            // moved before it, then fold this one in, so a second read
            // (another `Move`, or any later `Copy`/`Ref`) of the same local
            // is what actually gets flagged, never this one against itself.
            if place.projection.is_empty() {
                moved.insert(place.local);
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
/// markable as moved in the first place.
fn check_place(
    function: &mir::Function,
    place: &mir::Place,
    moved: &HashSet<LocalId>,
    faults: &mut Vec<MirFault>,
) {
    if moved.contains(&place.local) {
        let span = function.locals[place.local].span;
        faults.push(Fault::error_with_kind(
            MirErrorKind::UseAfterMove,
            Some(span),
        ));
    }
}
