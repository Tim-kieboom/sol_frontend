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
//! - **Straight-line functions only** (any branch — a `SwitchInt`, what
//!   both `if` and `for` lower to — skips the whole function, unanalyzed):
//!   same precedent as `move_check`'s and `escape_check`'s own first
//!   slices.
//! - **Conflict matrix**: `&mut` vs `&mut`, and `&mut` vs `&`, overlapping
//!   in time — reject. `&` vs `&` overlapping — always fine (unlimited
//!   simultaneous shared borrows, same as real Rust).
//! - **Real NLL-accurate liveness, not a lexical-scope heuristic**: a
//!   borrow's live range is `[the point it's (re)assigned, the last point
//!   it's actually read]`, not "until its enclosing block ends." A borrow
//!   with zero further reads is live only at its own creation point.
//!
//! # Mechanism
//!
//! The straight-line body is flattened into an ordered timeline of
//! "points" (one per statement, plus one per terminator that itself reads
//! operands — a `Call`'s arguments, an `Assert`'s `cond`/`msg`). Walking
//! that timeline once builds, per local, its sequence of **generations**
//! (the span between one whole-place assignment and the next, or the end of
//! the function) — a local reassigned partway through gets a fresh
//! generation, and each generation's own last-read index is its live
//! range's end.
//!
//! A generation counts as an actual **borrow** only if its own assignment
//! traces back — through a chain of bare-local aliasing
//! (`Use(Copy(place))`/`Use(Move(place))`, `place` unprojected, the same
//! "trace through intermediate locals" mechanism `escape_check` uses) — to
//! a fresh `Rvalue::Ref { place, mutable }` whose own `place` contains no
//! `Deref` step. A `Deref` there means this borrows *through* an existing
//! reference/pointer (external memory, not a local this function owns the
//! whole lifetime of) — out of scope for this whole-locals-only slice, same
//! as it was a *safe* terminus for `escape_check`. Tracing that reaches a
//! local with **no recorded generation at all** (a parameter, in
//! straight-line code) means the aliased value isn't a borrow of anything
//! this function itself declared — not tracked, not flagged.
//!
//! Every borrow found this way is grouped by the root local it borrows;
//! within each group, every pair is checked for a live-range overlap where
//! at least one side is mutable.

use std::collections::HashMap;

use mir_model::{self as mir, LocalId};
use soul_utils::fault::Fault;

use crate::fault::{MirErrorKind, MirFault};

/// See the module's own docs.
pub fn check_borrow_overlaps(function: &mir::Function) -> Vec<MirFault> {
    let mut faults = Vec::new();

    let Some(points) = build_timeline(function) else {
        // Contains a branch — not analyzed yet, see the module's own docs.
        return faults;
    };

    let generations = collect_generations(&points);
    let borrows = classify_borrows(&generations);

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
                if !intervals_overlap(a.range(), b.range()) {
                    continue;
                }
                let later = if a.def_index >= b.def_index { a } else { b };
                faults.push(Fault::error_with_kind(
                    MirErrorKind::OverlappingBorrows,
                    Some(function.locals[later.local].span),
                ));
            }
        }
    }

    faults
}

/// One flattened program point in execution order — straight-line only, so
/// this order is unambiguous.
struct PointInfo<'a> {
    /// A whole-place `Assign(local, rvalue)` happening at this point, if
    /// any (a write through a projection isn't a fresh generation — see
    /// the module's own docs, same convention `move_check`/`escape_check`
    /// use).
    assign: Option<(LocalId, &'a mir::Rvalue)>,
    /// Every local read at this point, from the assignment's own rvalue or
    /// (for the one point built per terminator) the terminator's operands.
    reads: Vec<LocalId>,
}

/// One local's value between one whole-place assignment and the next (or
/// the end of the function).
struct Generation<'a> {
    def_index: usize,
    /// `None` for a parameter's implicit generation 0 — it's never actually
    /// pushed (see `collect_generations`), so this field only ever holds a
    /// real, explicit assignment's rvalue.
    rvalue: &'a mir::Rvalue,
    reads: Vec<usize>,
}

struct Borrow {
    /// The local currently holding this borrow — used for the fault's span.
    local: LocalId,
    /// The root local actually being borrowed (whole-locals only — see the
    /// module's own docs).
    place: LocalId,
    mutable: bool,
    def_index: usize,
    last_use: usize,
}
impl Borrow {
    fn range(&self) -> (usize, usize) {
        (self.def_index, self.last_use)
    }
}

fn intervals_overlap(a: (usize, usize), b: (usize, usize)) -> bool {
    a.0 <= b.1 && b.0 <= a.1
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

/// Flattens the function into execution order — `None` if it contains any
/// branch (a `SwitchInt`, what both `if` and `for` lower to), unanalyzed.
fn build_timeline(function: &mir::Function) -> Option<Vec<PointInfo<'_>>> {
    if function
        .blocks
        .values()
        .any(|block| matches!(block.terminator, mir::Terminator::SwitchInt { .. }))
    {
        return None;
    }

    let mut points = Vec::new();
    let Some((entry, _)) = function.blocks.entries().next() else {
        return Some(points);
    };
    let mut current = entry;
    while let Some(block) = function.blocks.get(current) {
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
            mir::Terminator::SwitchInt { .. } => {
                unreachable!("bailed out above for any function containing a SwitchInt")
            }
        };
        if !terminator_reads.is_empty() {
            points.push(PointInfo {
                assign: None,
                reads: terminator_reads,
            });
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

    Some(points)
}

/// Walks the timeline once, splitting each local's history into
/// generations. A read at a point where the local has no generation yet at
/// all (a parameter, never (re)assigned in this straight-line body) is
/// simply not recorded anywhere — parameters aren't themselves borrow
/// candidates in this slice, only a possible *terminus* for tracing an
/// alias chain back to (see `trace_borrow`).
fn collect_generations<'a>(points: &[PointInfo<'a>]) -> HashMap<LocalId, Vec<Generation<'a>>> {
    let mut generations: HashMap<LocalId, Vec<Generation<'a>>> = HashMap::new();

    for (index, point) in points.iter().enumerate() {
        for &local in &point.reads {
            if let Some(gens) = generations.get_mut(&local)
                && let Some(current) = gens.last_mut()
            {
                current.reads.push(index);
            }
        }
        if let Some((local, rvalue)) = point.assign {
            generations.entry(local).or_default().push(Generation {
                def_index: index,
                rvalue,
                reads: Vec::new(),
            });
        }
    }

    generations
}

fn active_generation_before<'a, 'b>(
    generations: &'b HashMap<LocalId, Vec<Generation<'a>>>,
    local: LocalId,
    before_index: usize,
) -> Option<&'b Generation<'a>> {
    generations
        .get(&local)?
        .iter()
        .rev()
        .find(|generation| generation.def_index < before_index)
}

/// Follows a bare-local alias chain back from `rvalue` (assigned at
/// `at_index`) to whatever produced the value: a fresh, `Deref`-free
/// `Rvalue::Ref` — a genuine borrow of its own `place.local` (any
/// `Field`/`Index` projection is coarsened away, whole-locals only) — or
/// anything else (a parameter terminus, a projected alias, a non-`Ref`/
/// non-alias rvalue, or a `Ref` reached through a `Deref`) — `None`, not a
/// tracked local-place borrow.
fn trace_borrow<'a>(
    generations: &HashMap<LocalId, Vec<Generation<'a>>>,
    mut rvalue: &'a mir::Rvalue,
    mut at_index: usize,
) -> Option<(LocalId, bool)> {
    loop {
        match rvalue {
            mir::Rvalue::Use(mir::Operand::Copy(place) | mir::Operand::Move(place)) => {
                if !place.projection.is_empty() {
                    return None;
                }
                let generation = active_generation_before(generations, place.local, at_index)?;
                rvalue = generation.rvalue;
                at_index = generation.def_index;
            }
            mir::Rvalue::Ref { place, mutable } => {
                let has_deref = place
                    .projection
                    .iter()
                    .any(|elem| matches!(elem, mir::PlaceElem::Deref));
                return (!has_deref).then_some((place.local, *mutable));
            }
            _ => return None,
        }
    }
}

fn classify_borrows(generations: &HashMap<LocalId, Vec<Generation<'_>>>) -> Vec<Borrow> {
    let mut borrows = Vec::new();
    for (&local, gens) in generations {
        for generation in gens {
            let Some((place, mutable)) =
                trace_borrow(generations, generation.rvalue, generation.def_index)
            else {
                continue;
            };
            let last_use = generation
                .reads
                .iter()
                .copied()
                .max()
                .unwrap_or(generation.def_index);
            borrows.push(Borrow {
                local,
                place,
                mutable,
                def_index: generation.def_index,
                last_use,
            });
        }
    }
    borrows
}
