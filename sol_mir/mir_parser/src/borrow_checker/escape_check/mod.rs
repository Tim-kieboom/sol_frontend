//! Dangling-reference ("escape") checking — see `TODO.md`'s M2 section.
//! Rejects a function whose returned value holds a reference into storage
//! that doesn't outlive the call, or that leaves such a reference in memory
//! its caller owns (through a `&mut` parameter).
//!
//! Reads its verdicts from the shared points-to dataflow (`points_to`): a
//! reference parameter's safety is tied to whatever the function's callers
//! pass, verified transitively across the call graph. Anything the analysis
//! can't follow is treated as dangling: a false "safe" here would be
//! unsoundness, not just a missed diagnostic.

use ast_model::declare_store::DeclareStore;
use mir_model::{self as mir, LocalId};
use sol_utils::{
    FunctionId,
    collections::vec_map::VecMap,
    fault::{Fault, FaultCollector},
};

use super::points_to::{Base, FunctionAnalysis, Origin, State, Summaries, converge_summaries};
use crate::fault::{MirErrorKind, MirFault};

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
        check_function(function, declares, &summaries, &mut faults);
    }

    faults.into_vec()
}

fn check_function(
    function: &mir::Function,
    declares: &DeclareStore,
    summaries: &Summaries,
    faults: &mut FaultCollector<MirErrorKind>,
) {
    let analysis = FunctionAnalysis::new(function, declares, summaries);
    let states = analysis.analyzer().fixed_point();
    for (_, state) in analysis.return_states(&states) {
        if let Some(Origin::Dangling(local)) = analysis.returned_origin(state) {
            faults.push(err_dangling_fault(function, local));
        }
        if let Some(local) = stored_escape(&analysis, state) {
            faults.push(err_dangling_fault(function, local));
        }
    }
}

// A local of this function whose storage is referenced from memory the
// caller owns once this function returns.
fn stored_escape(analysis: &FunctionAnalysis, state: &State) -> Option<LocalId> {
    let caller_owned = state
        .written()
        .filter(|(location, _)| matches!(location.base(), Base::Param(_) | Base::ParamDeep(_)))
        .map(|(_, targets)| targets)
        .chain(std::iter::once(state.leaked()));

    for targets in caller_owned {
        if let Origin::Dangling(local) = analysis.analyzer().origin_of(state, targets) {
            return Some(local);
        }
    }
    None
}

fn err_dangling_fault(function: &mir::Function, local: LocalId) -> MirFault {
    let span = function.locals[local].span;
    Fault::error_with_kind(MirErrorKind::DanglingReference, Some(span))
}
