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
//! Scoped to straight-line functions only (any branch — anything lowering
//! to a `SwitchInt` — is skipped entirely, unanalyzed; same precedent as
//! move-checking's own first slice): every local has exactly one reaching
//! assignment, so tracing "where did this local's value come from" is a
//! simple backward walk with no CFG-join needed.
//!
//! Only checks a function's *own* return value, not its callers: a
//! reference-typed parameter is trusted as safe without verifying what the
//! caller actually passed in — that's real interprocedural propagation, a
//! separate, later slice (see `TODO.md`'s M2 entry).
//!
//! The backward trace has exactly three outcomes:
//!  - It reaches a local with **no recorded assignment** at all — in
//!    straight-line code, every body-declared local gets exactly one, so
//!    this must be a parameter (including `this`/`&this`/`&mut this`) —
//!    **safe**, trusted without checking the caller (see above).
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
//! stops and nothing is reported, the same "unfamiliar shape ⇒ skip, don't
//! guess" discipline `move_check` itself uses.

use std::collections::HashMap;

use ast_model::{SolType, declare_store::DeclareStore};
use mir_model::{self as mir, LocalId};
use sol_utils::fault::Fault;

use crate::fault::{MirErrorKind, MirFault};

/// See the module's own docs. Returns at most one fault: a function has at
/// most one reachable `return` in straight-line code, so there's at most
/// one return value to trace.
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

    let Some(assignments) = straight_line_assignments(function) else {
        // Contains a branch — not analyzed yet, see the module's own docs.
        return faults;
    };

    if let Some(Origin::Dangling(local)) = trace_origin(return_local, &assignments) {
        faults.push(Fault::error_with_kind(
            MirErrorKind::DanglingReference,
            Some(function.locals[local].span),
        ));
    }

    faults
}

enum Origin {
    Safe,
    /// The local whose own storage doesn't outlive this function — used to
    /// look up its declaration span for the fault (mirrors `move_check`'s
    /// `UseAfterMove` — MIR statements don't carry per-statement spans yet).
    Dangling(LocalId),
}

/// Follows a bare-local alias chain (`Use(Copy(place))`/`Use(Move(place))`
/// where `place` has no projection) back to whatever produced the value —
/// either a local with no recorded assignment (a parameter: safe) or a
/// fresh `Rvalue::Ref` (classified by whether its own place's projection
/// passes through a `Deref`). `None` the moment the chain hits anything
/// else — see the module's own docs on why that's a skip, not a guess.
fn trace_origin(local: LocalId, assignments: &HashMap<LocalId, &mir::Rvalue>) -> Option<Origin> {
    let mut current = local;
    loop {
        let Some(rvalue) = assignments.get(&current) else {
            return Some(Origin::Safe);
        };
        match rvalue {
            mir::Rvalue::Use(mir::Operand::Copy(place) | mir::Operand::Move(place)) => {
                if !place.projection.is_empty() {
                    return None;
                }
                current = place.local;
            }
            mir::Rvalue::Ref { place, .. } => {
                let escapes_this_frame = place
                    .projection
                    .iter()
                    .any(|elem| matches!(elem, mir::PlaceElem::Deref));
                return Some(if escapes_this_frame {
                    Origin::Safe
                } else {
                    Origin::Dangling(place.local)
                });
            }
            _ => return None,
        }
    }
}

/// Every bare-local assignment (`place.projection.is_empty()`) recorded in
/// program order, keyed by local — later assignments overwrite earlier ones,
/// matching straight-line control flow's own "current value" semantics.
/// `None` if the function contains any branch (`Terminator::SwitchInt`, what
/// both `if` and `for` lower to) — not analyzed yet, see the module's own
/// docs.
fn straight_line_assignments(function: &mir::Function) -> Option<HashMap<LocalId, &mir::Rvalue>> {
    if function
        .blocks
        .values()
        .any(|block| matches!(block.terminator, mir::Terminator::SwitchInt { .. }))
    {
        return None;
    }

    let mut assignments = HashMap::new();
    let Some((entry, _)) = function.blocks.entries().next() else {
        return Some(assignments);
    };
    let mut current = entry;
    while let Some(block) = function.blocks.get(current) {
        for statement in &block.statements {
            if let mir::Statement::Assign(place, rvalue) = statement {
                if place.projection.is_empty() {
                    assignments.insert(place.local, rvalue);
                }
            }
        }

        current = match &block.terminator {
            mir::Terminator::Goto(target) | mir::Terminator::Drop { target, .. } => *target,
            mir::Terminator::Call { target, .. } => {
                let Some(target) = target else { break };
                *target
            }
            mir::Terminator::Assert { target, .. } => *target,
            mir::Terminator::Return | mir::Terminator::Unreachable => break,
            mir::Terminator::SwitchInt { .. } => {
                unreachable!("bailed out above for any function containing a SwitchInt")
            }
        };
    }

    Some(assignments)
}
