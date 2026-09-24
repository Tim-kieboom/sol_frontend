//! The forward "what does each location point to" dataflow escape-checking
//! reads its verdicts from.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use ast_model::SolType;
use mir_model::{self as mir, BlockId, LocalId};
use sol_utils::{
    FunctionId,
    collections::{array::Arr, vec_map::VecMap, vec_set::VecSet},
};

use super::{
    Coverage, Origin, Summaries, Summary, combine,
    locations::{Base, Location, Step, Types, is_pointer_like},
};

pub(super) type Targets = BTreeSet<Location>;

#[derive(Clone, PartialEq, Eq, Default)]
pub(super) struct State {
    // Only locations written so far; every other location holds its
    // `default_content`.
    contents: BTreeMap<Location, Targets>,
    // Pointees written into memory the analysis couldn't name; any read may
    // observe them.
    leaked: Targets,
    // Locations strongly written on every path reaching this point.
    always_written: BTreeSet<Location>,
}

impl State {
    pub(super) fn written(&self) -> impl Iterator<Item = (&Location, &Targets)> {
        self.contents.iter()
    }

    pub(super) fn is_always_written(&self, location: &Location) -> bool {
        self.always_written.contains(location)
    }

    pub(super) fn leaked(&self) -> &Targets {
        &self.leaked
    }
}

#[derive(Clone, Copy)]
enum Update {
    Strong,
    Weak,
}

pub(super) struct Analyzer<'ctx> {
    types: Types<'ctx>,
    summaries: &'ctx Summaries,
}

impl<'ctx> Analyzer<'ctx> {
    pub(super) fn new(types: Types<'ctx>, summaries: &'ctx Summaries) -> Self {
        Self { types, summaries }
    }

    pub(super) fn types(&self) -> &Types<'ctx> {
        &self.types
    }

    // Out-state of every reachable block once the analysis has converged.
    pub(super) fn fixed_point(&self) -> VecMap<BlockId, State> {
        let function = self.types.function();
        let mut out_states = VecMap::new();
        let Some((entry, _)) = function.blocks.entries().next() else {
            return out_states;
        };

        let mut order = crate::borrow_checker::postorder(function, entry);
        order.reverse();
        let predecessors = predecessors(function, &order);

        let mut queued: VecSet<BlockId> = order.iter().copied().collect();
        let mut queue: VecDeque<BlockId> = order.iter().copied().collect();

        while let Some(block_id) = queue.pop_front() {
            queued.remove(block_id);

            let mut state = self.in_state(block_id, entry, &predecessors, &out_states);
            self.transfer_block(&mut state, block_id);

            if out_states.get(block_id) == Some(&state) {
                continue;
            }

            out_states.insert(block_id, state);
            let terminator = &function.blocks[block_id].terminator;
            for successor in super::super::successors(terminator) {
                if function.blocks.get(successor).is_some() && queued.insert(successor).is_none() {
                    queue.push_back(successor);
                }
            }
        }

        out_states
    }

    // The worst origin among `targets`, looking through a call result to
    // whatever it points into.
    pub(super) fn origin_of(&self, state: &State, targets: &Targets) -> Origin {
        let mut visited_calls = BTreeSet::new();
        self.origin_of_inner(state, targets, &mut visited_calls)
    }

    // Everything stored in the references of `local`'s own value.
    pub(super) fn local_value_targets(&self, state: &State, local: LocalId) -> Option<Targets> {
        let ty = self.types.local_type(local)?;
        let root = Location::root(Base::Frame(local));
        let mut targets = Targets::new();
        for sub in self.types.ref_subpaths(&ty) {
            targets.extend(self.content(state, &self.types.extend(&root, &sub)));
        }
        Some(targets)
    }

    fn origin_of_inner(
        &self,
        state: &State,
        targets: &Targets,
        visited_calls: &mut BTreeSet<BlockId>,
    ) -> Origin {
        let mut origin = Origin::Safe;
        for location in targets {
            let next = match location.base() {
                Base::Frame(local) | Base::Unknown(local) => Origin::Dangling(*local),
                Base::Param(index) | Base::ParamDeep(index) => {
                    Origin::TiedToParams(VecSet::from([*index]))
                }
                Base::Incoming(prior) => match prior.base().param() {
                    Some(index) => Origin::TiedToParams(VecSet::from([index])),
                    None => Origin::Safe,
                },
                Base::CallResult(block_id) => {
                    if !visited_calls.insert(*block_id) {
                        continue;
                    }
                    let pointees = self.content(state, location);
                    self.origin_of_inner(state, &pointees, visited_calls)
                }
            };
            origin = combine(origin, next);
        }
        origin
    }

    fn in_state(
        &self,
        block_id: BlockId,
        entry: BlockId,
        predecessors: &VecMap<BlockId, Vec<BlockId>>,
        out_states: &VecMap<BlockId, State>,
    ) -> State {
        let mut incoming: Vec<&State> = Vec::new();
        let initial = State::default();
        if block_id == entry {
            incoming.push(&initial);
        }
        if let Some(preds) = predecessors.get(block_id) {
            incoming.extend(preds.iter().filter_map(|pred| out_states.get(*pred)));
        }

        let mut joined = State::default();
        let keys: BTreeSet<&Location> = incoming
            .iter()
            .flat_map(|state| state.contents.keys())
            .collect();
        for key in keys {
            let mut targets = Targets::new();
            for state in &incoming {
                targets.extend(self.stored_or_default(state, key));
            }
            joined.contents.insert(key.clone(), targets);
        }
        for state in &incoming {
            joined.leaked.extend(state.leaked.iter().cloned());
        }
        if let Some((first, rest)) = incoming.split_first() {
            joined.always_written = first
                .always_written
                .iter()
                .filter(|location| {
                    rest.iter()
                        .all(|state| state.always_written.contains(*location))
                })
                .cloned()
                .collect();
        }
        joined
    }

    fn transfer_block(&self, state: &mut State, block_id: BlockId) {
        let block = &self.types.function().blocks[block_id];
        for statement in block.statements.iter() {
            if let mir::Statement::Assign(place, rvalue) = statement {
                self.assign(state, place, rvalue);
            }
        }

        if let mir::Terminator::Call {
            id,
            arguments,
            destination,
            ..
        } = &block.terminator
        {
            self.call(
                state,
                block_id,
                *id,
                arguments.as_slice(),
                destination.as_ref(),
            );
        }
    }

    fn assign(&self, state: &mut State, place: &mir::Place, rvalue: &mir::Rvalue) {
        let Some(ty) = self.types.place_type(place) else {
            state.leaked.insert(unknown(place.local));
            return;
        };
        let subs = self.types.ref_subpaths(&ty);
        if subs.is_empty() {
            return;
        }

        let values: Vec<Targets> = subs
            .iter()
            .map(|sub| self.rvalue_targets(state, rvalue, sub, place.local))
            .collect();
        self.store(state, place, &subs, values);
    }

    fn call(
        &self,
        state: &mut State,
        block_id: BlockId,
        callee: FunctionId,
        arguments: &[mir::Operand],
        destination: Option<&mir::Place>,
    ) {
        let summary = self.summaries.get(callee);
        let result = match destination {
            Some(destination) => self.call_result(state, block_id, arguments, summary, destination),
            None => None,
        };

        match summary {
            Some(summary) => self.apply_callee_writes(state, summary, arguments),
            // An extern: nothing is known about what it stores through a
            // mutable argument.
            None => {
                for argument in arguments {
                    self.clobber_through_mutable_argument(state, argument);
                }
            }
        }

        if let (Some(destination), Some((subs, value))) = (destination, result) {
            let values = vec![value; subs.len()];
            self.store(state, destination, &subs, values);
        }
    }

    // The reference sub-paths of `destination` and what each of them holds
    // once the call returns, or `None` when the result holds no reference.
    fn call_result(
        &self,
        state: &mut State,
        block_id: BlockId,
        arguments: &[mir::Operand],
        summary: Option<&Summary>,
        destination: &mir::Place,
    ) -> Option<(Vec<Arr<Step>>, Targets)> {
        let Some(ty) = self.types.place_type(destination) else {
            state.leaked.insert(unknown(destination.local));
            return None;
        };
        let subs = self.types.ref_subpaths(&ty);
        if subs.is_empty() {
            return None;
        }

        let Some(summary) = summary else {
            return Some((subs, Targets::from([unknown(destination.local)])));
        };
        let indices = match &summary.returned {
            Origin::TiedToParams(indices) => indices,
            // The callee's own check already rejects a dangling return.
            Origin::Safe | Origin::Dangling(_) => return Some((subs, Targets::new())),
        };

        let mut reached = Targets::new();
        for index in indices.entries() {
            let Some(argument) = arguments.get(index) else {
                return Some((subs, Targets::from([unknown(destination.local)])));
            };
            let start = self.operand_value_targets(state, argument);
            reached.extend(self.reachable(state, start));
        }

        let result = Location::root(Base::CallResult(block_id));
        self.write(state, result.clone(), reached, Update::Weak);
        Some((subs, Targets::from([result])))
    }

    // Replays what the callee leaves in memory reachable from its
    // arguments, translated into this function's own locations. Everything
    // is read from the pre-call state before anything is written.
    fn apply_callee_writes(
        &self,
        state: &mut State,
        summary: &Summary,
        arguments: &[mir::Operand],
    ) {
        let mut writes: Vec<(Location, Targets, Update)> = Vec::new();
        let mut leaked = Targets::new();

        for (location, write) in &summary.writes {
            let Some(argument) = location
                .base()
                .param()
                .and_then(|index| arguments.get(index))
            else {
                continue;
            };
            let value = self.translate(state, &write.targets, arguments);

            if !matches!(location.base(), Base::Param(_)) {
                let start = self.operand_value_targets(state, argument);
                let (slots, reached_opaque) = self.reference_slots(state, start);
                if reached_opaque {
                    leaked.extend(value.iter().cloned());
                }
                for slot in slots {
                    writes.push((slot, value.clone(), Update::Weak));
                }
                continue;
            }

            let pointees = self.as_pointees(self.operand_targets(state, argument, &[]));
            let single = pointees.len() == 1;
            for pointee in &pointees {
                if pointee.base().is_opaque() {
                    leaked.extend(value.iter().cloned());
                    continue;
                }
                let key = self.types.extend(pointee, location.path());
                let update =
                    if single && write.coverage == Coverage::EveryReturn && !key.is_summary() {
                        Update::Strong
                    } else {
                        Update::Weak
                    };
                writes.push((key, value.clone(), update));
            }
        }

        state.leaked.extend(leaked);
        for (key, value, update) in writes {
            self.write(state, key, value, update);
        }
    }

    // Callee-space targets (`Param`/`ParamDeep`/`Incoming` bases only)
    // rewritten as this function's own locations at the call site.
    fn translate(&self, state: &State, targets: &Targets, arguments: &[mir::Operand]) -> Targets {
        let mut translated = Targets::new();
        for target in targets {
            let Some(argument) = target.base().param().and_then(|index| arguments.get(index))
            else {
                continue;
            };

            match target.base() {
                Base::Param(_) => {
                    for pointee in self.as_pointees(self.operand_targets(state, argument, &[])) {
                        translated.insert(self.types.extend(&pointee, target.path()));
                    }
                }
                Base::Incoming(prior) if matches!(prior.base(), Base::Param(_)) => {
                    for pointee in self.as_pointees(self.operand_targets(state, argument, &[])) {
                        let slot = self.types.extend(&pointee, prior.path());
                        translated.extend(self.content(state, &slot));
                    }
                }
                // Somewhere deeper in the memory reachable from the argument.
                _ => {
                    let start = self.operand_value_targets(state, argument);
                    translated.extend(self.reachable(state, start));
                }
            }
        }
        translated
    }

    // A callee handed a mutable reference it has no summary for may store
    // anything, so every reference reachable through it may now dangle.
    fn clobber_through_mutable_argument(&self, state: &mut State, argument: &mir::Operand) {
        let (mir::Operand::Copy(place) | mir::Operand::Move(place)) = argument else {
            return;
        };
        let Some(ty) = self.types.place_type(place) else {
            state.leaked.insert(unknown(place.local));
            return;
        };
        if !self.types.contains_mutable_reference(&ty) {
            return;
        }

        let clobber = Targets::from([unknown(place.local)]);
        let start = self.operand_value_targets(state, argument);
        let (slots, reached_opaque) = self.reference_slots(state, start);
        if reached_opaque {
            state.leaked.extend(clobber.iter().cloned());
        }
        for slot in slots {
            self.write(state, slot, clobber.clone(), Update::Weak);
        }
    }

    // Every location holding a reference somewhere in the memory reachable
    // from `start`, plus whether any of that memory can't be named at all.
    fn reference_slots(&self, state: &State, start: Targets) -> (Vec<Location>, bool) {
        let mut slots = Vec::new();
        let mut reached_opaque = false;
        for location in self.reachable(state, start) {
            if location.base().is_opaque() {
                reached_opaque = true;
                continue;
            }
            // An untyped location (a `ParamDeep` summary) holds its
            // references directly rather than at typed sub-paths.
            let Some(location_ty) = self.types.location_type(&location) else {
                slots.push(location);
                continue;
            };
            for sub in self.types.ref_subpaths(&location_ty) {
                slots.push(self.types.extend(&location, &sub));
            }
        }
        (slots, reached_opaque)
    }

    fn store(
        &self,
        state: &mut State,
        place: &mir::Place,
        subs: &[Arr<Step>],
        values: Vec<Targets>,
    ) {
        let Some(destinations) = self.place_locations(state, place) else {
            for value in values {
                state.leaked.extend(value);
            }
            return;
        };

        let single = destinations.len() == 1;
        for destination in &destinations {
            if destination.base().is_opaque() {
                for value in &values {
                    state.leaked.extend(value.iter().cloned());
                }
                continue;
            }

            for (sub, value) in subs.iter().zip(&values) {
                let key = self.types.extend(destination, sub);
                let update = if single && !key.is_summary() {
                    Update::Strong
                } else {
                    Update::Weak
                };
                self.write(state, key, value.clone(), update);
            }
        }
    }

    fn write(&self, state: &mut State, key: Location, value: Targets, update: Update) {
        let merged = match update {
            Update::Strong => {
                state.always_written.insert(key.clone());
                value
            }
            Update::Weak => {
                let mut merged = self.content(state, &key);
                merged.extend(value);
                merged
            }
        };
        state.contents.insert(key, merged);
    }

    fn rvalue_targets(
        &self,
        state: &State,
        rvalue: &mir::Rvalue,
        sub: &[Step],
        blame: LocalId,
    ) -> Targets {
        use mir::{AggregateKind, Rvalue};

        match (rvalue, sub) {
            (Rvalue::Use(operand) | Rvalue::Cast(operand, _), _) => {
                self.operand_targets(state, operand, sub)
            }
            (Rvalue::Ref { place, .. }, []) => self
                .place_locations(state, place)
                .unwrap_or_else(|| Targets::from([unknown(place.local)])),
            (
                Rvalue::Aggregate(AggregateKind::Struct | AggregateKind::Tuple, operands),
                [Step::Field(index), rest @ ..],
            ) => match operands.get(*index) {
                Some(operand) => self.operand_targets(state, operand, rest),
                None => Targets::from([unknown(blame)]),
            },
            (Rvalue::Aggregate(AggregateKind::Array, operands), [Step::AnyIndex, rest @ ..]) => {
                let mut targets = Targets::new();
                for operand in operands {
                    targets.extend(self.operand_targets(state, operand, rest));
                }
                targets
            }
            (Rvalue::Aggregate(AggregateKind::Slice, operands), []) => match operands.first() {
                Some(pointer) => self.operand_targets(state, pointer, &[]),
                None => Targets::from([unknown(blame)]),
            },
            (Rvalue::HeapAlloc(_, operand, _), [Step::OwnedDeref, rest @ ..]) => {
                self.operand_targets(state, operand, rest)
            }
            _ => Targets::from([unknown(blame)]),
        }
    }

    fn operand_targets(&self, state: &State, operand: &mir::Operand, sub: &[Step]) -> Targets {
        let (mir::Operand::Copy(place) | mir::Operand::Move(place)) = operand else {
            return Targets::new();
        };
        let Some(sources) = self.place_locations(state, place) else {
            return Targets::from([unknown(place.local)]);
        };

        let mut targets = Targets::new();
        for source in &sources {
            targets.extend(self.content(state, &self.types.extend(source, sub)));
        }
        targets
    }

    fn operand_value_targets(&self, state: &State, operand: &mir::Operand) -> Targets {
        let (mir::Operand::Copy(place) | mir::Operand::Move(place)) = operand else {
            return Targets::new();
        };
        let Some(ty) = self.types.place_type(place) else {
            return Targets::from([unknown(place.local)]);
        };

        let mut targets = Targets::new();
        for sub in self.types.ref_subpaths(&ty) {
            targets.extend(self.operand_targets(state, operand, &sub));
        }
        targets
    }

    // The locations `place` names, or `None` when its shape can't be
    // followed.
    fn place_locations(&self, state: &State, place: &mir::Place) -> Option<Targets> {
        let mut ty = self.types.local_type(place.local)?;
        let mut locations = Targets::from([Location::root(Base::Frame(place.local))]);

        for elem in &place.projection {
            let next_ty = self.types.place_elem_type(&ty, elem)?;
            locations = match (elem, &ty) {
                (mir::PlaceElem::Field(index), _) => self.step_all(&locations, Step::Field(*index)),
                (mir::PlaceElem::Deref, SolType::Reference(_)) => self.pointees(state, &locations),
                (mir::PlaceElem::Deref, SolType::Pointer(_)) => {
                    self.step_all(&locations, Step::OwnedDeref)
                }
                (mir::PlaceElem::Index(_), _) if is_pointer_like(&ty) => {
                    self.step_all(&self.pointees(state, &locations), Step::AnyIndex)
                }
                (mir::PlaceElem::Index(_), SolType::Array(_)) => {
                    self.step_all(&locations, Step::AnyIndex)
                }
                _ => return None,
            };
            ty = next_ty;
        }

        Some(locations)
    }

    fn step_all(&self, locations: &Targets, step: Step) -> Targets {
        locations
            .iter()
            .map(|location| self.types.extend(location, &[step]))
            .collect()
    }

    fn pointees(&self, state: &State, locations: &Targets) -> Targets {
        let mut pointees = Targets::new();
        for location in locations {
            pointees.extend(self.content(state, location));
        }
        self.as_pointees(pointees)
    }

    // A reference's stored value, read as the memory it points at: an
    // `Incoming` value points somewhere into its parameter's memory.
    fn as_pointees(&self, targets: Targets) -> Targets {
        targets
            .into_iter()
            .map(|target| match target.base() {
                Base::Incoming(prior) => match prior.base().param() {
                    Some(index) => Location::root(Base::ParamDeep(index)),
                    None => target,
                },
                _ => target,
            })
            .collect()
    }

    fn reachable(&self, state: &State, start: Targets) -> Targets {
        let mut reached = Targets::new();
        let mut pending: Vec<Location> = self.as_pointees(start).into_iter().collect();

        while let Some(location) = pending.pop() {
            if !reached.insert(location.clone()) {
                continue;
            }

            let Some(ty) = self.types.location_type(&location) else {
                pending.extend(self.as_pointees(self.content(state, &location)));
                continue;
            };
            for sub in self.types.ref_subpaths(&ty) {
                let key = self.types.extend(&location, &sub);
                pending.extend(self.as_pointees(self.content(state, &key)));
            }
        }

        reached
    }

    fn content(&self, state: &State, location: &Location) -> Targets {
        let mut targets = self.stored_or_default(state, location);
        targets.extend(state.leaked.iter().cloned());
        targets
    }

    fn stored_or_default(&self, state: &State, location: &Location) -> Targets {
        match state.contents.get(location) {
            Some(targets) => targets.clone(),
            None => self.default_content(location),
        }
    }

    // What a location holds before this function writes to it.
    fn default_content(&self, location: &Location) -> Targets {
        match location.base() {
            Base::Frame(local) => match self.types.param_index(*local) {
                Some(index) if location_is_root(location) => {
                    Targets::from([Location::root(Base::Param(index))])
                }
                Some(index) => Targets::from([Location::root(Base::ParamDeep(index))]),
                None => Targets::new(),
            },
            Base::Param(_) | Base::ParamDeep(_) => {
                Targets::from([Location::root(Base::Incoming(Box::new(location.clone())))])
            }
            Base::CallResult(_) | Base::Incoming(_) => Targets::new(),
            Base::Unknown(local) => Targets::from([unknown(*local)]),
        }
    }
}

fn location_is_root(location: &Location) -> bool {
    *location == Location::root(location.base().clone())
}

fn unknown(local: LocalId) -> Location {
    Location::root(Base::Unknown(local))
}

fn predecessors(function: &mir::Function, order: &[BlockId]) -> VecMap<BlockId, Vec<BlockId>> {
    let mut predecessors: VecMap<BlockId, Vec<BlockId>> = VecMap::new();
    for &block_id in order {
        for successor in super::super::successors(&function.blocks[block_id].terminator) {
            if function.blocks.get(successor).is_some() {
                predecessors.entry(successor).or_default().push(block_id);
            }
        }
    }
    predecessors
}
