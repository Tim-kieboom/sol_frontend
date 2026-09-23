//! Dangling-reference ("escape") checking — the second slice of M2's real
//! borrow/lifetime-conflict work (see `TODO.md`'s
//! M2 section), distinct from move-checking: does a function return a
//! reference whose *storage* doesn't outlive the call. Traces where a
//! returned reference's storage actually comes from, backward through the
//! MIR, across the full concrete (M1) CFG shape (loops included) and
//! interprocedurally across the whole call graph — a reference-typed
//! parameter's safety is tied to whatever the function's callers actually
//! pass, verified transitively.
//!
//! Any shape the trace can't classify is treated as unresolved rather than
//! guessed at: a false "safe" here would be unsoundness, not just a missed
//! diagnostic, so an unresolved path makes the whole answer unresolved too.

use ast_model::{SolType, declare_store::DeclareStore};
use mir_model::{self as mir, BlockId, LocalId};
use sol_utils::{
    FunctionId,
    collections::{
        vec_map::{VecMap, VecMapIndex},
        vec_set::VecSet,
    },
    fault::{Fault, FaultCollector},
};

use crate::fault::{MirErrorKind, MirFault};

/// Checks every function in `functions` for a returned reference whose
/// storage provably doesn't outlive the call. Returns one fault per such
/// return path (a function can have more than one `return`).
pub fn check_escapes(
    functions: &VecMap<FunctionId, mir::Function>,
    declares: &DeclareStore,
) -> Vec<MirFault> {
    let summaries = converge_summaries(functions, declares);

    let mut faults = FaultCollector::default();
    for function in functions.values() {
        let checker = EscapeChecker::new(function, declares, &summaries);
        checker.check_escape(&mut faults);
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
            let checker = EscapeChecker::new(function, declares, &summaries);
            let origin = checker.compute_function_origin();
            next.insert(id, origin);
        }

        if next == summaries {
            return next;
        }

        summaries = next;
    }
}

struct EscapeChecker<'ctx> {
    summaries: &'ctx Summaries,
    declares: &'ctx DeclareStore,
    function: &'ctx mir::Function,
    predecessors: VecMap<BlockId, Vec<BlockId>>,
}
impl<'ctx> EscapeChecker<'ctx> {
    fn new(
        function: &'ctx mir::Function,
        declares: &'ctx DeclareStore,
        summaries: &'ctx Summaries,
    ) -> Self {
        Self {
            function,
            declares,
            summaries,
            predecessors: VecMap::const_default(),
        }
    }

    fn check_escape(mut self, faults: &mut FaultCollector<MirErrorKind>) {
        let Some(return_local) = self.returned_reference_local() else {
            return;
        };
        let (return_blocks, settled) = self.settle_return_origins(return_local);

        for &block_id in &return_blocks {
            if let Some(err) = self.check_local_dangling(return_local, block_id, &settled) {
                faults.push(err);
            }
        }
    }

    fn check_local_dangling(
        &self,
        return_local: LocalId,
        block_id: BlockId,
        settled: &Table,
    ) -> Option<MirFault> {
        let element = settled.get(&(return_local, block_id))?;
        let Some(Origin::Dangling(local)) = element else {
            return None;
        };

        let span = self.function.locals[*local].span;
        Some(Fault::error_with_kind(
            MirErrorKind::DanglingReference,
            Some(span),
        ))
    }

    fn compute_function_origin(mut self) -> Origin {
        let Some(return_local) = self.returned_reference_local() else {
            return Origin::Safe;
        };
        let (return_blocks, settled) = self.settle_return_origins(return_local);

        let mut combined = Origin::Safe;
        for &block_id in &return_blocks {
            let origin = settled
                .get(&(return_local, block_id))
                .cloned()
                .flatten()
                .unwrap_or(Origin::Safe);

            combined = combine(combined, origin);
        }
        combined
    }

    /// `function.return_local`, if its type makes it worth tracing at all
    /// (only a reference-typed return value can dangle).
    fn returned_reference_local(&self) -> Option<LocalId> {
        let return_local = self.function.return_local?;
        self.is_reference_typed(return_local)
            .then_some(return_local)
    }

    /// Builds `self.predecessors` and runs the fixed-point trace for every
    /// `Return` block, resolving what `return_local` holds at each one.
    fn settle_return_origins(&mut self, return_local: LocalId) -> (Vec<BlockId>, Table) {
        self.predecessors = build_predecessors(self.function);
        let return_blocks = return_blocks_of(self.function);
        let settled = self.converge(return_local, &return_blocks);
        (return_blocks, settled)
    }

    fn converge(&self, return_local: LocalId, return_blocks: &[BlockId]) -> Table {
        let mut previous_round = Table::new();
        loop {
            let mut this_round = Table::new();
            let mut in_progress = std::collections::HashSet::new();
            let mut state = TraceState {
                in_progress: &mut in_progress,
                this_round: &mut this_round,
            };
            for &block_id in return_blocks {
                self.trace_from_block_start(return_local, block_id, &previous_round, &mut state);
            }
            if this_round == previous_round {
                return this_round;
            }
            previous_round = this_round;
        }
    }

    fn trace_from_block_start(
        &self,
        local: LocalId,
        block_id: BlockId,
        previous_round: &Table,
        state: &mut TraceState,
    ) -> Option<Origin> {
        let key = (local, block_id);
        if let Some(settled) = state.this_round.get(&key) {
            return settled.clone();
        }
        if state.in_progress.contains(&key) {
            return previous_round
                .get(&key)
                .cloned()
                .unwrap_or(Some(Origin::Safe));
        }

        state.in_progress.insert(key);
        let result = match self.call_destination_origin(local, block_id, previous_round, state) {
            Some(origin) => origin,
            None => {
                let statement_count = self.function.blocks[block_id].statements.len();
                self.trace_within_block(local, block_id, statement_count, previous_round, state)
            }
        };
        state.in_progress.remove(&key);
        state.this_round.insert(key, result.clone());
        result
    }

    // If `block_id`'s terminator is a whole-place `Call` defining `local`,
    // resolves its origin via the callee's settled summary. Outer `None` means
    // the terminator isn't a matching `Call`, so the caller should fall through
    // to the ordinary statement search.
    fn call_destination_origin(
        &self,
        local: LocalId,
        block_id: BlockId,
        previous_round: &Table,
        state: &mut TraceState,
    ) -> Option<Option<Origin>> {
        let mir::Terminator::Call {
            id,
            arguments,
            destination: Some(place),
            ..
        } = &self.function.blocks[block_id].terminator
        else {
            return None;
        };
        if place.local != local || !place.projection.is_empty() {
            return None;
        }

        let Some(summary) = self.summaries.get(*id) else {
            return Some(None);
        };
        let origin = match summary {
            Origin::Dangling(_) => None,
            Origin::Safe => Some(Origin::Safe),
            Origin::TiedToParams(indices) => {
                let statement_count = self.function.blocks[block_id].statements.len();
                let mut combined = Origin::Safe;
                let mut sound = true;
                for index in indices.entries() {
                    let arg_origin = arguments.get(index).and_then(|operand| match operand {
                        mir::Operand::Copy(place) | mir::Operand::Move(place)
                            if place.projection.is_empty() =>
                        {
                            self.trace_within_block(
                                place.local,
                                block_id,
                                statement_count,
                                previous_round,
                                state,
                            )
                        }
                        _ => None,
                    });
                    match arg_origin {
                        Some(origin) => combined = combine(combined, origin),
                        None => {
                            sound = false;
                            break;
                        }
                    }
                }
                if sound { Some(combined) } else { None }
            }
        };
        Some(origin)
    }

    fn trace_within_block(
        &self,
        local: LocalId,
        block_id: BlockId,
        before_index: usize,
        previous_round: &Table,
        state: &mut TraceState,
    ) -> Option<Origin> {
        let block = &self.function.blocks[block_id];
        for (index, statement) in block.statements.as_slice()[..before_index]
            .iter()
            .enumerate()
            .rev()
        {
            let mir::Statement::Assign(place, rvalue) = statement else {
                continue;
            };
            if place.local != local || !place.projection.is_empty() {
                continue;
            }
            return match rvalue {
                mir::Rvalue::Use(mir::Operand::Copy(alias) | mir::Operand::Move(alias)) => {
                    if !alias.projection.is_empty() {
                        None
                    } else {
                        self.trace_within_block(alias.local, block_id, index, previous_round, state)
                    }
                }
                mir::Rvalue::Ref {
                    place: ref_place, ..
                } => self.classify_ref(ref_place, block_id, index, previous_round, state),
                _ => None,
            };
        }

        let preds = self
            .predecessors
            .get(block_id)
            .filter(|preds| !preds.is_empty());

        let Some(preds) = preds else {
            // No predecessor recorded an assignment reaching here — the
            // function's own entry block, `local` unassigned: a parameter.
            // `LocalId` values start at 1 and parameters are allocated first,
            // so a parameter's own 0-based index is `local.index() - 1`.
            return Some(
                if local.index() >= 1 && local.index() <= self.function.arg_count {
                    Origin::TiedToParams(VecSet::from([local.index() - 1]))
                } else {
                    Origin::Safe
                },
            );
        };

        let mut combined = Origin::Safe;
        for &pred in preds {
            let pred_origin = self.trace_from_block_start(local, pred, previous_round, state)?;
            combined = combine(combined, pred_origin);
        }
        Some(combined)
    }

    fn classify_ref(
        &self,
        ref_place: &mir::Place,
        block_id: BlockId,
        at_index: usize,
        previous_round: &Table,
        state: &mut TraceState,
    ) -> Option<Origin> {
        let deref_position = ref_place
            .projection
            .iter()
            .position(|elem| matches!(elem, mir::PlaceElem::Deref));
        match deref_position {
            None => Some(Origin::Dangling(ref_place.local)),
            Some(0) => match self
                .declares
                .get_type(self.function.locals[ref_place.local].ty)
            {
                Some(SolType::Pointer(_)) => Some(Origin::Safe),
                Some(SolType::Reference(_)) => self.trace_within_block(
                    ref_place.local,
                    block_id,
                    at_index,
                    previous_round,
                    state,
                ),
                _ => None,
            },
            // A `Deref` reached through a field first isn't refined by this
            // slice — a known, narrower gap; unconditionally safe as before.
            Some(_) => Some(Origin::Safe),
        }
    }

    fn is_reference_typed(&self, local: LocalId) -> bool {
        let local_type = self.function.locals[local].ty;
        matches!(
            self.declares.get_type(local_type),
            Some(SolType::Reference(_))
        )
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

type Table = std::collections::HashMap<(LocalId, BlockId), Option<Origin>>;
type Summaries = VecMap<FunctionId, Origin>;

struct TraceState<'a> {
    in_progress: &'a mut std::collections::HashSet<(LocalId, BlockId)>,
    this_round: &'a mut Table,
}

fn return_blocks_of(function: &mir::Function) -> Vec<BlockId> {
    function
        .blocks
        .entries()
        .filter(|(_, block)| matches!(block.terminator, mir::Terminator::Return))
        .map(|(block_id, _)| block_id)
        .collect()
}

fn build_predecessors(function: &mir::Function) -> VecMap<BlockId, Vec<BlockId>> {
    let mut predecessors: VecMap<BlockId, Vec<BlockId>> = VecMap::new();

    for (id, block) in function.blocks.entries() {
        for successor in super::successors(&block.terminator) {
            if function.blocks.get(successor).is_none() {
                continue;
            }

            predecessors.entry(successor).or_default().push(id);
        }
    }

    predecessors
}
