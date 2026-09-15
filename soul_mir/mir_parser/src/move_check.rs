//! Straight-line use-after-move checking — the first slice of M2's move
//! checker (see `docs/mir-design.md` and `TODO.md`'s M2 section). Walks a
//! function's statements/terminators in execution order, tracking which
//! locals are currently moved-out-of, and flags any read (`Operand::Copy`,
//! or a `Ref`/`Len`'s own place) of one, or a second `Operand::Move` of one
//! that's already moved.
//!
//! State is derived entirely from `Operand::Move`/`Copy` occurrences and
//! `Assign`'s own reinitializing writes — deliberately *not* from the
//! `MarkMoved`/`SetDropFlag` statements `mir_parser::function` also emits
//! alongside every move. Those are pushed as statements immediately
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
//! Deliberately scoped to straight-line functions only: any function
//! containing a `SwitchInt` (i.e. any `if`/`for` at all — both lower to
//! `SwitchInt`, see `mir_parser::function::control_flow`) is skipped
//! entirely, not analyzed at all. Correctly checking a branch means
//! computing predecessors and merging state at the join (an `if`/`else`'s
//! join block, a loop header) — and a `for` loop's back-edge makes the CFG
//! cyclic, which a single forward pass can't answer correctly at all; that
//! needs real fixed-point dataflow, the way rustc's own move-checker works.
//! Both are real, follow-on slices (see M2's TODO.md entry), not attempted
//! here — skipping unfamiliar shapes outright avoids both crashing on them
//! and, worse, silently mis-analyzing them into a false accusation.
//!
//! Real borrow/lifetime-conflict checking (do two live borrows of the same
//! place overlap) is a different, much bigger analysis this doesn't attempt
//! at all yet — nothing in the MIR tracks borrow liveness. This is move-
//! checking only: was this place read after being moved out of.

use std::collections::HashSet;

use mir_model::{self as mir, LocalId};
use soul_utils::fault::Fault;

use crate::fault::{MirErrorKind, MirFault};

/// See the module's own docs. Returns one fault per violating read; an
/// empty `Vec` means either "no violations" or "not analyzed" (a branching
/// function) — the two aren't distinguished by the return type, since
/// neither should block compilation right now.
pub fn check_moves(function: &mir::Function) -> Vec<MirFault> {
    if function
        .blocks
        .values()
        .any(|block| matches!(block.terminator, mir::Terminator::SwitchInt { .. }))
    {
        return Vec::new();
    }

    let mut faults = Vec::new();
    let mut moved: HashSet<LocalId> = HashSet::new();

    let Some((entry, _)) = function.blocks.entries().next() else {
        return faults;
    };
    let mut current = entry;
    while let Some(block) = function.blocks.get(current) {
        for statement in &block.statements {
            match statement {
                mir::Statement::Assign(place, rvalue) => {
                    check_rvalue(function, rvalue, &mut moved, &mut faults);
                    // A whole-place write reinitializes the local — legal to
                    // reassign after a move (`docs/mir-design.md`'s own move/
                    // drop rule). A write through a projection
                    // (`x.field = ..`) doesn't fully reinitialize `x` as a
                    // whole, so it's deliberately not cleared here — a
                    // narrower, separate gap (see the module's own docs).
                    if place.projection.is_empty() {
                        moved.remove(&place.local);
                    }
                }
                // Deliberately not used for state — see the module's own
                // docs on why deriving state from `Operand::Move` directly
                // avoids the ordering trap these would otherwise cause.
                mir::Statement::MarkMoved(_)
                | mir::Statement::SetDropFlag(..)
                | mir::Statement::StorageDead(_) => {}
            }
        }

        current = match &block.terminator {
            mir::Terminator::Goto(target) | mir::Terminator::Drop { target, .. } => *target,
            mir::Terminator::Call {
                arguments, target, ..
            } => {
                for argument in arguments {
                    check_operand(function, argument, &mut moved, &mut faults);
                }
                let Some(target) = target else {
                    break;
                };
                *target
            }
            mir::Terminator::Assert {
                cond, msg, target, ..
            } => {
                check_operand(function, cond, &mut moved, &mut faults);
                check_operand(function, msg, &mut moved, &mut faults);
                *target
            }
            mir::Terminator::Return | mir::Terminator::Unreachable => break,
            mir::Terminator::SwitchInt { .. } => {
                unreachable!("bailed out above for any function containing a SwitchInt")
            }
        };
    }

    faults
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
        | mir::Rvalue::HeapAlloc(_, operand) => {
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
