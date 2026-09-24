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

use ast_model::declare_store::DeclareStore;
use mir_model::{self as mir, BlockId, LocalId};
use sol_utils::{
    FunctionId,
    collections::{vec_map::VecMap, vec_set::VecSet},
    fault::{Fault, FaultCollector},
};

use crate::fault::{MirErrorKind, MirFault};
use dataflow::{Analyzer, State};
use locations::{Base, Types};

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
fn converge_summaries(
    functions: &VecMap<FunctionId, mir::Function>,
    declares: &DeclareStore,
) -> Summaries {
    let mut summaries: Summaries = functions.keys().map(|id| (id, Origin::Safe)).collect();

    loop {
        let mut next = Summaries::new();
        for (id, function) in functions.entries() {
            let origin = EscapeChecker::new(function, declares, &summaries).summary();
            next.insert(id, origin);
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
                faults.push(self.dangling_fault(local));
            }
            if let Some(local) = self.stored_escape(state) {
                faults.push(self.dangling_fault(local));
            }
        }
    }

    // What the function's return value depends on, joined over every
    // `return`.
    fn summary(&self) -> Origin {
        let states = self.analyzer.fixed_point();
        let mut summary = Origin::Safe;
        for (_, state) in self.return_states(&states) {
            if let Some(origin) = self.returned_origin(state) {
                summary = combine(summary, origin);
            }
        }
        summary
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

    fn dangling_fault(&self, local: LocalId) -> MirFault {
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

type Summaries = VecMap<FunctionId, Origin>;
