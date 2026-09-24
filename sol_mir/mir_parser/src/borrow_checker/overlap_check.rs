//! Borrow-conflict checking — see `TODO.md`'s M2 section. Loan-based, in the
//! style of rustc's NLL: a `Ref` creates a *loan* of a place, and every
//! reference derived from it (copies, aliases joined across branches, the
//! fields it's stored in, call results, reborrows) carries that loan — all
//! tracked by the shared points-to dataflow (`points_to`). A loan is live
//! while any live local's value, or memory the caller owns, still carries it.
//!
//! An access to memory overlapping a live loan conflicts unless the access
//! goes *through* that loan (a reborrow using its parent, or a write through
//! the borrow itself) or both the access and the loan are shared. Moves are
//! reported as their own kind of conflict (`check_move_while_borrowed`).
//! A loan whose own creation already conflicts isn't enforced afterwards, so
//! one mistake is reported once.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use ast_model::declare_store::DeclareStore;
use mir_model::{self as mir, BlockId, LocalId};
use sol_utils::{
    FunctionId,
    collections::{vec_map::VecMap, vec_set::VecSet},
    fault::Fault,
};

use super::points_to::{
    Analyzer, Base, FunctionAnalysis, LoanId, Location, PlaceAccess, State, Summaries, Targets,
    converge_summaries,
};
use crate::fault::{MirErrorKind, MirFault};

/// Checks every function in `functions` for an access (a read, a write, or a
/// new borrow) to memory a conflicting live borrow still covers. Returns one
/// fault per conflicting access.
pub fn check_borrow_overlaps(
    functions: &VecMap<FunctionId, mir::Function>,
    declares: &DeclareStore,
) -> Vec<MirFault> {
    check_conflicts(functions, declares, Reported::Borrows)
}

/// Checks every function in `functions` for a move of a value, or of one of
/// its fields, while a live borrow still covers it. Returns one fault per
/// such move.
pub fn check_move_while_borrowed(
    functions: &VecMap<FunctionId, mir::Function>,
    declares: &DeclareStore,
) -> Vec<MirFault> {
    check_conflicts(functions, declares, Reported::Moves)
}

/// Runs `check_borrow_overlaps` and `check_move_while_borrowed` off one
/// shared analysis.
pub fn check_overlaps_and_moves_while_borrowed(
    functions: &VecMap<FunctionId, mir::Function>,
    declares: &DeclareStore,
) -> Vec<MirFault> {
    check_conflicts(functions, declares, Reported::All)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Reported {
    Borrows,
    Moves,
    All,
}

fn check_conflicts(
    functions: &VecMap<FunctionId, mir::Function>,
    declares: &DeclareStore,
    reported: Reported,
) -> Vec<MirFault> {
    let summaries = converge_summaries(functions, declares);
    let mut faults = Vec::new();
    for function in functions.values() {
        faults.extend(function_conflicts(function, declares, &summaries, reported));
    }
    faults
}

fn function_conflicts(
    function: &mir::Function,
    declares: &DeclareStore,
    summaries: &Summaries,
    reported: Reported,
) -> Vec<MirFault> {
    let analysis = FunctionAnalysis::new(function, declares, summaries);
    let in_states = analysis.analyzer().settled_in_states();
    let mut checker = ConflictChecker {
        analyzer: analysis.analyzer(),
        function,
        liveness: Liveness::compute(function),
        loans: collect_loans(analysis.analyzer(), function, &in_states),
        unenforced: BTreeSet::new(),
        reported,
        faults: Vec::new(),
    };

    for (block_id, state) in in_states.entries() {
        checker.check_block(block_id, state.clone());
    }
    checker.faults
}

struct Loan {
    locations: Targets,
    mutable: bool,
}

// Every `Ref` in `function`, with the memory it borrows.
fn collect_loans(
    analyzer: &Analyzer,
    function: &mir::Function,
    in_states: &VecMap<BlockId, State>,
) -> BTreeMap<LoanId, Loan> {
    let mut loans = BTreeMap::new();
    for (block_id, state) in in_states.entries() {
        let mut state = state.clone();
        let block = &function.blocks[block_id];
        for (index, statement) in block.statements.iter().enumerate() {
            if let mir::Statement::Assign(_, mir::Rvalue::Ref { mutable, place }) = statement {
                let locations = match analyzer.place_access(&state, place) {
                    Some(access) => access.locations,
                    None => Targets::from([Location::root(Base::Unknown(place.local))]),
                };
                let id = LoanId {
                    block: block_id,
                    index,
                };
                loans.insert(
                    id,
                    Loan {
                        locations,
                        mutable: *mutable,
                    },
                );
            }
            analyzer.step_statement(&mut state, block_id, index, statement);
        }
    }
    loans
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Read,
    Write,
    Move,
}

struct Access {
    locations: Targets,
    // Loans the access goes through: never in conflict with it.
    through: Targets,
    mode: Mode,
    blame: LocalId,
    // The loan this access creates, when it is a `Ref`.
    creates: Option<LoanId>,
}

struct ConflictChecker<'a, 'ctx> {
    analyzer: &'a Analyzer<'ctx>,
    function: &'a mir::Function,
    liveness: Liveness,
    loans: BTreeMap<LoanId, Loan>,
    unenforced: BTreeSet<LoanId>,
    reported: Reported,
    faults: Vec<MirFault>,
}

impl ConflictChecker<'_, '_> {
    fn check_block(&mut self, block_id: BlockId, mut state: State) {
        let block = &self.function.blocks[block_id];
        for (index, statement) in block.statements.iter().enumerate() {
            let accesses = self.statement_accesses(&state, block_id, index, statement);
            self.check_point(&state, block_id, index, accesses);
            self.analyzer
                .step_statement(&mut state, block_id, index, statement);
        }

        let accesses = self.terminator_accesses(&state, &block.terminator);
        self.check_point(&state, block_id, block.statements.len(), accesses);
    }

    fn check_point(
        &mut self,
        state: &State,
        block_id: BlockId,
        index: usize,
        accesses: Vec<Access>,
    ) {
        if accesses.is_empty() {
            return;
        }
        let live = self.live_loans(state, block_id, index);
        for access in accesses {
            self.check_access(&live, access);
        }
    }

    // Loans still needed at a point: those carried by a local live there, or
    // stored where they outlive the function.
    fn live_loans(&self, state: &State, block_id: BlockId, index: usize) -> BTreeSet<LoanId> {
        let mut carriers = self.analyzer.escaped_loans(state);
        for &local in self.liveness.live_before(block_id, index) {
            carriers.extend(self.analyzer.loans_held_by(state, local));
        }
        carriers.iter().filter_map(loan_id).collect()
    }

    fn check_access(&mut self, live: &BTreeSet<LoanId>, access: Access) {
        let through: BTreeSet<LoanId> = access.through.iter().filter_map(loan_id).collect();
        // Going through a loan already reported as conflicting only repeats
        // that one mistake.
        if through.iter().any(|id| self.unenforced.contains(id)) {
            return;
        }
        let conflict = live.iter().any(|id| {
            if self.unenforced.contains(id) || through.contains(id) {
                return false;
            }
            let Some(loan) = self.loans.get(id) else {
                return false;
            };
            (loan.mutable || access.mode != Mode::Read)
                && overlaps_any(&loan.locations, &access.locations)
        });
        if !conflict {
            return;
        }

        if let Some(created) = access.creates {
            self.unenforced.insert(created);
        }
        let (kind, shown) = match access.mode {
            Mode::Move => (
                MirErrorKind::MoveWhileBorrowed,
                self.reported != Reported::Borrows,
            ),
            Mode::Read | Mode::Write => (
                MirErrorKind::OverlappingBorrows,
                self.reported != Reported::Moves,
            ),
        };
        if shown {
            let span = self.function.locals[access.blame].span;
            self.faults.push(Fault::error_with_kind(kind, Some(span)));
        }
    }

    fn statement_accesses(
        &self,
        state: &State,
        block_id: BlockId,
        index: usize,
        statement: &mir::Statement,
    ) -> Vec<Access> {
        let mir::Statement::Assign(place, rvalue) = statement else {
            return Vec::new();
        };

        let mut accesses = Vec::new();
        match rvalue {
            mir::Rvalue::Ref {
                mutable,
                place: borrowed,
            } => {
                let mode = if *mutable { Mode::Write } else { Mode::Read };
                let creates = Some(LoanId {
                    block: block_id,
                    index,
                });
                accesses.push(self.place(state, borrowed, mode, place.local, creates));
            }
            mir::Rvalue::Len(read) => {
                accesses.push(self.place(state, read, Mode::Read, read.local, None));
            }
            _ => {
                for operand in rvalue_operands(rvalue) {
                    accesses.extend(self.operand(state, operand));
                }
            }
        }
        accesses.push(self.place(state, place, Mode::Write, place.local, None));
        accesses
    }

    fn terminator_accesses(&self, state: &State, terminator: &mir::Terminator) -> Vec<Access> {
        let mut accesses = Vec::new();
        match terminator {
            mir::Terminator::Call {
                arguments,
                destination,
                ..
            } => {
                for argument in arguments.iter() {
                    accesses.extend(self.operand(state, argument));
                }
                if let Some(destination) = destination {
                    let local = destination.local;
                    accesses.push(self.place(state, destination, Mode::Write, local, None));
                }
            }
            mir::Terminator::SwitchInt { discriminant, .. } => {
                accesses.extend(self.operand(state, discriminant));
            }
            mir::Terminator::Assert { cond, msg, .. } => {
                accesses.extend(self.operand(state, cond));
                accesses.extend(self.operand(state, msg));
            }
            mir::Terminator::Goto(_)
            | mir::Terminator::Drop { .. }
            | mir::Terminator::Return
            | mir::Terminator::Unreachable => {}
        }
        accesses
    }

    // Reading or moving an operand, plus — when it holds a reference — using
    // that reference, which accesses what it points at.
    fn operand(&self, state: &State, operand: &mir::Operand) -> Vec<Access> {
        let (mode, place) = match operand {
            mir::Operand::Copy(place) => (Mode::Read, place),
            mir::Operand::Move(place) => (Mode::Move, place),
            mir::Operand::Constant(_) => return Vec::new(),
        };

        let mut accesses = vec![self.place(state, place, mode, place.local, None)];
        if let Some(pointees) = self.analyzer.reference_pointees(state, place) {
            let mode = if self.holds_mutable_reference(place) {
                Mode::Write
            } else {
                Mode::Read
            };
            accesses.push(Access {
                locations: pointees.locations,
                through: pointees.through,
                mode,
                blame: place.local,
                creates: None,
            });
        }
        accesses
    }

    fn place(
        &self,
        state: &State,
        place: &mir::Place,
        mode: Mode,
        blame: LocalId,
        creates: Option<LoanId>,
    ) -> Access {
        let PlaceAccess { locations, through } = self
            .analyzer
            .place_access(state, place)
            .unwrap_or_else(|| PlaceAccess {
                locations: Targets::from([Location::root(Base::Unknown(place.local))]),
                through: Targets::new(),
            });
        Access {
            locations,
            through,
            mode,
            blame,
            creates,
        }
    }

    fn holds_mutable_reference(&self, place: &mir::Place) -> bool {
        use ast_model::{ArrayKind, SolType};

        match self.analyzer.types().place_type(place) {
            Some(SolType::Reference(reference)) => reference.mutable.is_mut(),
            Some(SolType::Array(array)) => array.kind == ArrayKind::MutSlice,
            _ => false,
        }
    }
}

fn rvalue_operands(rvalue: &mir::Rvalue) -> Vec<&mir::Operand> {
    match rvalue {
        mir::Rvalue::Use(operand)
        | mir::Rvalue::UnaryOp(_, operand)
        | mir::Rvalue::Cast(operand, _)
        | mir::Rvalue::HeapAlloc(_, operand, _) => vec![operand],
        mir::Rvalue::BinaryOp(_, left, right) | mir::Rvalue::CheckedBinaryOp(_, left, right) => {
            vec![left, right]
        }
        mir::Rvalue::Aggregate(_, operands) => operands.iter().collect(),
        mir::Rvalue::Ref { .. } | mir::Rvalue::Len(_) => Vec::new(),
    }
}

fn loan_id(target: &Location) -> Option<LoanId> {
    match target.base() {
        Base::Loan(id) => Some(*id),
        _ => None,
    }
}

fn overlaps_any(a: &Targets, b: &Targets) -> bool {
    a.iter().any(|x| b.iter().any(|y| locations_overlap(x, y)))
}

// Whether two locations may share memory. A function's own frame never
// overlaps its caller's memory, and different parameters never overlap each
// other: the caller already had to prove that when it passed them.
fn locations_overlap(a: &Location, b: &Location) -> bool {
    match (a.base(), b.base()) {
        (Base::Unknown(_) | Base::CallResult(_), _)
        | (_, Base::Unknown(_) | Base::CallResult(_)) => true,
        (Base::Frame(x), Base::Frame(y)) => x == y && paths_overlap(a, b),
        (Base::Param(i), Base::Param(j)) => i == j && paths_overlap(a, b),
        (Base::ParamDeep(i), Base::Param(j) | Base::ParamDeep(j))
        | (Base::Param(i), Base::ParamDeep(j)) => i == j,
        _ => false,
    }
}

fn paths_overlap(a: &Location, b: &Location) -> bool {
    a.path().starts_with(b.path()) || b.path().starts_with(a.path())
}

// Which locals may still be read at each point: before statement `index`
// of a block, or before its terminator at `index == statements.len()`.
struct Liveness {
    live_before: VecMap<BlockId, Vec<BTreeSet<LocalId>>>,
}

impl Liveness {
    fn compute(function: &mir::Function) -> Self {
        let predecessors = predecessors(function);
        let mut live_in: VecMap<BlockId, BTreeSet<LocalId>> = VecMap::new();
        let mut queue: VecDeque<BlockId> = function.blocks.keys().collect();
        let mut queued: VecSet<BlockId> = function.blocks.keys().collect();

        while let Some(block_id) = queue.pop_front() {
            queued.remove(block_id);
            let points = block_live_points(function, block_id, &live_in);
            let entry = points.first().cloned().unwrap_or_default();
            if live_in.get(block_id) == Some(&entry) {
                continue;
            }
            live_in.insert(block_id, entry);
            for &pred in predecessors.get(block_id).into_iter().flatten() {
                if queued.insert(pred).is_none() {
                    queue.push_back(pred);
                }
            }
        }

        let mut live_before = VecMap::new();
        for block_id in function.blocks.keys() {
            live_before.insert(block_id, block_live_points(function, block_id, &live_in));
        }
        Self { live_before }
    }

    fn live_before(&self, block_id: BlockId, index: usize) -> impl Iterator<Item = &LocalId> {
        self.live_before
            .get(block_id)
            .and_then(|points| points.get(index))
            .into_iter()
            .flatten()
    }
}

// Live locals before every statement of `block_id` and before its
// terminator, given the live-in sets of its successors.
fn block_live_points(
    function: &mir::Function,
    block_id: BlockId,
    live_in: &VecMap<BlockId, BTreeSet<LocalId>>,
) -> Vec<BTreeSet<LocalId>> {
    let block = &function.blocks[block_id];
    let mut live = BTreeSet::new();
    for successor in super::successors(&block.terminator) {
        if let Some(successor_live) = live_in.get(successor) {
            live.extend(successor_live.iter().copied());
        }
    }

    terminator_liveness(function, &block.terminator, &mut live);
    let mut points = vec![live.clone()];
    for statement in block.statements.iter().rev() {
        if let mir::Statement::Assign(place, rvalue) = statement {
            write_liveness(place, &mut live);
            rvalue_uses(rvalue, &mut live);
        }
        points.push(live.clone());
    }
    points.reverse();
    points
}

fn terminator_liveness(
    function: &mir::Function,
    terminator: &mir::Terminator,
    live: &mut BTreeSet<LocalId>,
) {
    match terminator {
        mir::Terminator::Call {
            arguments,
            destination,
            ..
        } => {
            if let Some(destination) = destination {
                write_liveness(destination, live);
            }
            for argument in arguments.iter() {
                operand_uses(argument, live);
            }
        }
        mir::Terminator::SwitchInt { discriminant, .. } => operand_uses(discriminant, live),
        mir::Terminator::Assert { cond, msg, .. } => {
            operand_uses(cond, live);
            operand_uses(msg, live);
        }
        mir::Terminator::Return => {
            if let Some(return_local) = function.return_local {
                live.insert(return_local);
            }
        }
        mir::Terminator::Goto(_) | mir::Terminator::Drop { .. } | mir::Terminator::Unreachable => {}
    }
}

// A whole-local write ends that local's previous value; a write through a
// reference or into an element reads what it goes through.
fn write_liveness(place: &mir::Place, live: &mut BTreeSet<LocalId>) {
    if place.projection.is_empty() {
        live.remove(&place.local);
        return;
    }
    let goes_through = place
        .projection
        .iter()
        .any(|elem| matches!(elem, mir::PlaceElem::Deref | mir::PlaceElem::Index(_)));
    if goes_through {
        live.insert(place.local);
    }
    index_uses(place, live);
}

fn rvalue_uses(rvalue: &mir::Rvalue, live: &mut BTreeSet<LocalId>) {
    match rvalue {
        mir::Rvalue::Ref { place, .. } | mir::Rvalue::Len(place) => place_uses(place, live),
        _ => {
            for operand in rvalue_operands(rvalue) {
                operand_uses(operand, live);
            }
        }
    }
}

fn operand_uses(operand: &mir::Operand, live: &mut BTreeSet<LocalId>) {
    if let mir::Operand::Copy(place) | mir::Operand::Move(place) = operand {
        place_uses(place, live);
    }
}

fn place_uses(place: &mir::Place, live: &mut BTreeSet<LocalId>) {
    live.insert(place.local);
    index_uses(place, live);
}

fn index_uses(place: &mir::Place, live: &mut BTreeSet<LocalId>) {
    for elem in &place.projection {
        if let mir::PlaceElem::Index(index) = elem {
            live.insert(*index);
        }
    }
}

fn predecessors(function: &mir::Function) -> VecMap<BlockId, Vec<BlockId>> {
    let mut predecessors: VecMap<BlockId, Vec<BlockId>> = VecMap::new();
    for (block_id, block) in function.blocks.entries() {
        for successor in super::successors(&block.terminator) {
            if function.blocks.get(successor).is_some() {
                predecessors.entry(successor).or_default().push(block_id);
            }
        }
    }
    predecessors
}
