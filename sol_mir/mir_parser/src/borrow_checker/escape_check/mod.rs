//! Dangling-reference ("escape") checking — see `TODO.md`'s M2 section.
//! Rejects a function whose returned value holds a reference into storage
//! that doesn't outlive the call, or that leaves such a reference in memory
//! its caller owns (through a `&mut` parameter).
//!
//! A forward dataflow over each function's CFG tracks, for every abstract
//! memory location, which locations the references stored there may point
//! to; a reference parameter's safety is tied to whatever the function's
//! callers pass, verified transitively across the call graph. Anything the
//! analysis can't follow is treated as dangling: a false "safe" here would
//! be unsoundness, not just a missed diagnostic.

mod dataflow;
mod locations;

use std::collections::BTreeMap;

use ast_model::declare_store::DeclareStore;
use mir_model::{self as mir, BlockId, LocalId};
use sol_utils::{
    FunctionId,
    collections::{vec_map::VecMap, vec_set::VecSet},
    fault::{Fault, FaultCollector},
};

use crate::fault::{MirErrorKind, MirFault};
use dataflow::{Analyzer, State, Targets};
use locations::{Base, Location, Types};

/// Checks every function in `functions` for a returned value, or a value
/// stored through a `&mut` parameter, holding a reference whose storage
/// doesn't outlive the call. Returns one fault per offending `return`.
pub fn check_escapes(
    functions: &VecMap<FunctionId, mir::Function>,
    declares: &DeclareStore,
) -> Vec<MirFault> {
    let summaries = converge_summaries(functions, declares);

    let mut faults = FaultCollector::default();
    for function in functions.values() {
        EscapeChecker::new(function, declares, &summaries).check(&mut faults);
    }

    faults.into_vec()
}

// The fixed point over the whole call graph: each round recomputes every
// function's summary against the *previous* round's summaries, until the
// table stops changing. Sound even for a cyclic call graph, since a self-
// or mutually-recursive call just reads its own previous-round entry.
// Merging each round into the previous one keeps summaries growing
// monotonically, so the rounds always settle.
//
// Settled twice: the first pass discovers every location a function may
// write, but starting from "writes nothing" it can never prove a recursive
// function writes on every path. The second pass restarts from just those
// locations, assumed written on every return with nothing yet, and lets the
// rounds grow what's written and wear the coverage down to what actually
// holds — the usual greatest fixed point for a must-property (a path that
// only avoids writing by recursing forever never returns at all). Either
// way the settled table is self-consistent: re-analyzing any function
// against it yields nothing it doesn't already contain.
fn converge_summaries(
    functions: &VecMap<FunctionId, mir::Function>,
    declares: &DeclareStore,
) -> Summaries {
    let empty: Summaries = functions
        .keys()
        .map(|id| (id, Summary::default()))
        .collect();

    let mut optimistic = settle_summaries(functions, declares, empty);
    for summary in optimistic.values_mut() {
        summary.returned = Origin::Safe;
        for write in summary.writes.values_mut() {
            write.targets.clear();
            write.coverage = Coverage::EveryReturn;
        }
    }
    settle_summaries(functions, declares, optimistic)
}

fn settle_summaries(
    functions: &VecMap<FunctionId, mir::Function>,
    declares: &DeclareStore,
    mut summaries: Summaries,
) -> Summaries {
    loop {
        let mut next = Summaries::new();
        for (id, function) in functions.entries() {
            let computed = EscapeChecker::new(function, declares, &summaries).summary();
            let merged = match summaries.get(id) {
                Some(previous) => previous.merge(computed),
                None => computed,
            };
            next.insert(id, merged);
        }

        if next == summaries {
            return next;
        }
        summaries = next;
    }
}

struct EscapeChecker<'ctx> {
    analyzer: Analyzer<'ctx>,
}

impl<'ctx> EscapeChecker<'ctx> {
    fn new(
        function: &'ctx mir::Function,
        declares: &'ctx DeclareStore,
        summaries: &'ctx Summaries,
    ) -> Self {
        let types = Types::new(function, declares);
        Self {
            analyzer: Analyzer::new(types, summaries),
        }
    }

    fn check(&self, faults: &mut FaultCollector<MirErrorKind>) {
        let states = self.analyzer.fixed_point();
        for (_, state) in self.return_states(&states) {
            if let Some(Origin::Dangling(local)) = self.returned_origin(state) {
                faults.push(self.err_dangling_fault(local));
            }
            if let Some(local) = self.stored_escape(state) {
                faults.push(self.err_dangling_fault(local));
            }
        }
    }

    // What the function's return value depends on, and what it may leave in
    // its caller's memory, joined over every `return`.
    fn summary(&self) -> Summary {
        let states = self.analyzer.fixed_point();
        let returns: Vec<&State> = self
            .return_states(&states)
            .map(|(_, state)| state)
            .collect();

        let mut summary = Summary::default();
        for state in &returns {
            if let Some(origin) = self.returned_origin(state) {
                summary.returned = combine(summary.returned, origin);
            }
            for (location, targets) in self.caller_memory_writes(state) {
                let write = summary
                    .writes
                    .entry(location)
                    .or_insert_with(|| ParamWrite {
                        targets: Targets::new(),
                        coverage: Coverage::EveryReturn,
                    });
                write.targets.extend(targets);
            }
        }

        for (location, write) in &mut summary.writes {
            let always = returns
                .iter()
                .all(|state| state.is_always_written(location));
            if !always {
                write.coverage = Coverage::SomeReturns;
            }
        }
        summary
    }

    // Every caller-owned location `state` holds something other than its
    // incoming value in, with that content in terms a caller can translate.
    fn caller_memory_writes(&self, state: &State) -> Vec<(Location, Targets)> {
        let mut writes = Vec::new();
        for (location, targets) in state.written() {
            if !matches!(location.base(), Base::Param(_) | Base::ParamDeep(_)) {
                continue;
            }

            let root = Location::root(Base::new_incoming(location));
            let unchanged = Targets::from([root]);
            if *targets == unchanged {
                continue;
            }
            writes.push((location.clone(), self.caller_visible(state, targets)));
        }
        writes
    }

    // Drops targets a caller can't observe: this function's own storage (a
    // store of it is already this function's own fault) and call results
    // (replaced by the parameters they were derived from).
    fn caller_visible(&self, state: &State, targets: &Targets) -> Targets {
        let mut visible = Targets::new();
        for target in targets {
            match target.base() {
                Base::Param(_) | Base::ParamDeep(_) | Base::Incoming(_) => {
                    visible.insert(target.clone());
                }
                Base::CallResult(_) => {
                    let single = Targets::from([target.clone()]);
                    let Origin::TiedToParams(indices) = self.analyzer.origin_of(state, &single)
                    else {
                        continue;
                    };
                    for index in indices.entries() {
                        visible.insert(Location::root(Base::Param(index)));
                        visible.insert(Location::root(Base::ParamDeep(index)));
                    }
                }
                Base::Frame(_) | Base::Unknown(_) => {}
            }
        }
        visible
    }

    fn return_states<'s>(
        &self,
        states: &'s VecMap<BlockId, State>,
    ) -> impl Iterator<Item = (BlockId, &'s State)> {
        let function = self.analyzer.types().function();
        states.entries().filter(|(block_id, _)| {
            matches!(
                function.blocks[*block_id].terminator,
                mir::Terminator::Return
            )
        })
    }

    fn returned_origin(&self, state: &State) -> Option<Origin> {
        let return_local = self.analyzer.types().function().return_local?;
        let targets = self.analyzer.local_value_targets(state, return_local)?;
        Some(self.analyzer.origin_of(state, &targets))
    }

    // A local of this function whose storage is referenced from memory the
    // caller owns once this function returns.
    fn stored_escape(&self, state: &State) -> Option<LocalId> {
        let caller_owned = state
            .written()
            .filter(|(location, _)| matches!(location.base(), Base::Param(_) | Base::ParamDeep(_)))
            .map(|(_, targets)| targets)
            .chain(std::iter::once(state.leaked()));

        for targets in caller_owned {
            if let Origin::Dangling(local) = self.analyzer.origin_of(state, targets) {
                return Some(local);
            }
        }
        None
    }

    fn err_dangling_fault(&self, local: LocalId) -> MirFault {
        let span = self.analyzer.types().function().locals[local].span;
        Fault::error_with_kind(MirErrorKind::DanglingReference, Some(span))
    }
}

#[derive(Clone, PartialEq, Eq)]
enum Origin {
    // Doesn't depend on any caller-supplied value.
    Safe,
    // The local whose storage doesn't outlive this function.
    Dangling(LocalId),
    // Only as safe as whichever of this function's own parameters (by
    // index) the caller actually passes.
    TiedToParams(VecSet<usize>),
}

// The "worse wins" join used at every branch point and when folding a
// function's multiple `Return` blocks into one summary: `Dangling` absorbs
// everything, two `TiedToParams` sets union, `Safe` is the identity.
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

#[derive(Clone, PartialEq, Eq)]
struct Summary {
    returned: Origin,
    // Keyed by the `Param`/`ParamDeep` location written, in the callee's own
    // terms; targets only ever name `Param`/`ParamDeep`/`Incoming` bases.
    writes: BTreeMap<Location, ParamWrite>,
}

impl Default for Summary {
    fn default() -> Self {
        Self {
            returned: Origin::Safe,
            writes: BTreeMap::new(),
        }
    }
}

impl Summary {
    // Only ever grows: targets union, and a write stays `SomeReturns` once it
    // has been seen as one (or once a round no longer makes it at all).
    fn merge(&self, next: Summary) -> Summary {
        let mut writes = self.writes.clone();
        for (location, write) in &mut writes {
            if !next.writes.contains_key(location) {
                write.coverage = Coverage::SomeReturns;
            }
        }
        for (location, next_write) in next.writes {
            match writes.get_mut(&location) {
                Some(write) => {
                    write.targets.extend(next_write.targets);
                    if next_write.coverage == Coverage::SomeReturns {
                        write.coverage = Coverage::SomeReturns;
                    }
                }
                None => {
                    writes.insert(location, next_write);
                }
            }
        }
        Summary {
            returned: combine(self.returned.clone(), next.returned),
            writes,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct ParamWrite {
    targets: Targets,
    coverage: Coverage,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Coverage {
    // Written on every path to every `return`: replaces the caller's value.
    EveryReturn,
    // Only joins with the caller's value.
    SomeReturns,
}

type Summaries = VecMap<FunctionId, Summary>;
