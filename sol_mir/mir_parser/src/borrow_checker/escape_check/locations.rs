//! Abstract memory locations for escape-checking, and the type-directed
//! bookkeeping that keeps their number finite.

use ast_model::{ArrayKind, NodeId, SolType, TupleKind, declare_store::DeclareStore};
use mir_model::{self as mir, BlockId, LocalId};
use sol_utils::{
    collections::{
        array::{Arr, RcArr},
        vec_map::VecMapIndex,
    },
    span::ModuleId,
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Base {
    // A local's own storage, including everything it owns inline or through
    // an owning `*T`.
    Frame(LocalId),
    // Exactly the value reference parameter `.0` (0-based) points at.
    Param(usize),
    // Everything else reachable from parameter `.0`.
    ParamDeep(usize),
    // Whatever the reference returned by the call ending block `.0` points
    // into.
    CallResult(BlockId),
    // Memory the analysis can't name, conservatively blamed on local `.0`.
    Unknown(LocalId),
    // Whatever the caller had stored at a `Param`/`ParamDeep` location
    // before the call. Only ever appears as a location's content: once
    // dereferenced it is just memory reachable from that parameter.
    Incoming(Box<Location>),
}

impl Base {
    pub(super) fn new_incoming(location: &Location) -> Self {
        Self::Incoming(location.clone().into())
    }

    // Bases standing for many concrete places at once; they carry no path.
    fn is_summary(&self) -> bool {
        matches!(
            self,
            Base::ParamDeep(_) | Base::CallResult(_) | Base::Unknown(_) | Base::Incoming(_)
        )
    }

    // The 0-based parameter whose caller-supplied memory this base names.
    pub(super) fn param(&self) -> Option<usize> {
        match self {
            Base::Param(index) | Base::ParamDeep(index) => Some(*index),
            Base::Incoming(location) => location.base.param(),
            Base::Frame(_) | Base::CallResult(_) | Base::Unknown(_) => None,
        }
    }

    // Bases a write can't be applied to at all, only leaked.
    pub(super) fn is_opaque(&self) -> bool {
        matches!(self, Base::CallResult(_) | Base::Unknown(_))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Step {
    Field(usize),
    // Every element of an array at once: indices are runtime values.
    AnyIndex,
    // Into the allocation an owning `*T` holds.
    OwnedDeref,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Location {
    base: Base,
    path: RcArr<Step>,
    // Stands for more than one concrete place, so it only ever gets weak
    // updates.
    summary: bool,
}

impl Location {
    pub(super) fn root(base: Base) -> Self {
        let summary = base.is_summary();
        Self {
            base,
            summary,
            path: RcArr::new(),
        }
    }

    pub(super) fn base(&self) -> &Base {
        &self.base
    }

    pub(super) fn path(&self) -> &[Step] {
        &self.path
    }

    pub(super) fn is_summary(&self) -> bool {
        self.summary
    }
}

pub(super) fn is_pointer_like(ty: &SolType) -> bool {
    match ty {
        SolType::Reference(_) => true,
        SolType::Array(array) => matches!(array.kind, ArrayKind::MutSlice | ArrayKind::ConstSlice),
        _ => false,
    }
}

pub(super) struct Types<'ctx> {
    function: &'ctx mir::Function,
    declares: &'ctx DeclareStore,
    module: Option<ModuleId>,
}

impl<'ctx> Types<'ctx> {
    pub(super) fn new(function: &'ctx mir::Function, declares: &'ctx DeclareStore) -> Self {
        let module = declares
            .get_function(function.id)
            .map(|(_, module)| *module);

        Self {
            function,
            declares,
            module,
        }
    }

    pub(super) fn function(&self) -> &'ctx mir::Function {
        self.function
    }

    // `LocalId` values start at 1 and parameters are allocated first.
    pub(super) fn param_index(&self, local: LocalId) -> Option<usize> {
        let index = local.index();
        (index >= 1 && index <= self.function.arg_count).then(|| index - 1)
    }

    pub(super) fn local_type(&self, local: LocalId) -> Option<SolType> {
        let decl = self.function.locals.get(local)?;
        self.declares.get_type(decl.ty).cloned()
    }

    pub(super) fn place_type(&self, place: &mir::Place) -> Option<SolType> {
        let mut ty = self.local_type(place.local)?;
        for elem in &place.projection {
            ty = self.place_elem_type(&ty, elem)?;
        }
        Some(ty)
    }

    pub(super) fn place_elem_type(&self, ty: &SolType, elem: &mir::PlaceElem) -> Option<SolType> {
        elem.step_type(ty, self.declares, self.module)
    }

    pub(super) fn extend(&self, location: &Location, steps: &[Step]) -> Location {
        let path = location.path.iter().chain(steps).copied();
        self.normalize(location.base.clone(), path)
    }

    pub(super) fn location_type(&self, location: &Location) -> Option<SolType> {
        let mut ty = self.base_type(&location.base)?;
        for step in &location.path {
            ty = self.step_type(&ty, *step)?;
        }
        Some(ty)
    }

    // Relative paths from a value of type `ty` to every reference (or slice)
    // it contains, stopping where a struct type would appear a third time
    // on the path — `normalize` folds anything deeper back onto those.
    pub(super) fn ref_subpaths(&self, ty: &SolType) -> Vec<Arr<Step>> {
        let mut subpaths = Vec::new();
        let mut prefix = Vec::new();
        let mut structs_on_path = Vec::new();
        self.collect_ref_subpaths(ty, &mut prefix, &mut structs_on_path, &mut subpaths);
        subpaths
    }

    pub(super) fn contains_mutable_reference(&self, ty: &SolType) -> bool {
        let mut visited = Vec::new();
        self.contains_mutable_reference_inner(ty, &mut visited)
    }

    fn base_type(&self, base: &Base) -> Option<SolType> {
        match base {
            Base::Frame(local) => self.local_type(*local),
            Base::Param(index) => {
                let ty = self.local_type(LocalId::new_index(index + 1))?;
                match &ty {
                    SolType::Reference(reference) => {
                        self.declares.get_type(reference.inner).cloned()
                    }
                    // A slice's pointee is indexed straight off the slice type.
                    _ if is_pointer_like(&ty) => Some(ty),
                    _ => None,
                }
            }
            Base::ParamDeep(_) | Base::CallResult(_) | Base::Unknown(_) | Base::Incoming(_) => None,
        }
    }

    fn step_type(&self, ty: &SolType, step: Step) -> Option<SolType> {
        match step {
            Step::Field(index) => self.place_elem_type(ty, &mir::PlaceElem::Field(index)),
            Step::AnyIndex => match ty {
                SolType::Array(array) => self.declares.get_type(array.of_type).cloned(),
                _ => None,
            },
            Step::OwnedDeref => match ty {
                SolType::Pointer(_) => ty.deref_once(self.declares),
                _ => None,
            },
        }
    }

    fn struct_id(&self, ty: &SolType) -> Option<NodeId> {
        self.declares
            .resolve_struct(ty, self.module)
            .map(|struct_| struct_.id)
    }

    // Folds a path so every struct type appears on it at most twice: the
    // first occurrence is a distinct place, the second is the summary of
    // every deeper repetition, and a third cycles back onto the second.
    fn normalize(&self, base: Base, steps: impl Iterator<Item = Step>) -> Location {
        if base.is_summary() {
            return Location::root(base);
        }

        let mut path = vec![];
        let mut location = Location::root(base);
        let Some(mut ty) = self.base_type(&location.base) else {
            path.extend(steps);
            location.path = path.into();
            location.summary = true;
            return location;
        };

        let mut struct_positions: Vec<(NodeId, usize)> = Vec::new();
        if let Some(id) = self.struct_id(&ty) {
            struct_positions.push((id, 0));
        }

        for step in steps {
            let Some(next) = self.step_type(&ty, step) else {
                location.summary = true;
                break;
            };
            if step == Step::AnyIndex {
                location.summary = true;
            }
            path.push(step);
            ty = next;

            let Some(id) = self.struct_id(&ty) else {
                continue;
            };
            let earlier: Vec<usize> = struct_positions
                .iter()
                .filter(|(seen, _)| *seen == id)
                .map(|(_, position)| *position)
                .collect();

            match earlier.as_slice() {
                [] => struct_positions.push((id, location.path.len())),
                [_] => {
                    location.summary = true;
                    struct_positions.push((id, location.path.len()));
                }
                [_, second, ..] => {
                    let second = *second;
                    path.truncate(second);
                    struct_positions.retain(|(_, position)| *position <= second);
                }
            }
        }

        location.path = path.into();
        location
    }

    fn collect_ref_subpaths(
        &self,
        ty: &SolType,
        prefix: &mut Vec<Step>,
        structs_on_path: &mut Vec<NodeId>,
        subpaths: &mut Vec<Arr<Step>>,
    ) {
        if is_pointer_like(ty) {
            subpaths.push(prefix.clone().into());
            return;
        }

        match ty {
            SolType::Pointer(_) | SolType::Array(_) => {
                let step = match ty {
                    SolType::Pointer(_) => Step::OwnedDeref,
                    _ => Step::AnyIndex,
                };
                let Some(inner) = self.step_type(ty, step) else {
                    return;
                };
                prefix.push(step);
                self.collect_ref_subpaths(&inner, prefix, structs_on_path, subpaths);
                prefix.pop();
            }
            SolType::TupleKind(TupleKind::Tuple(types)) => {
                for (index, id) in types.iter().enumerate() {
                    let Some(field_ty) = self.declares.get_type(*id) else {
                        continue;
                    };
                    prefix.push(Step::Field(index));
                    self.collect_ref_subpaths(field_ty, prefix, structs_on_path, subpaths);
                    prefix.pop();
                }
            }
            _ => {
                let Some(struct_) = self.declares.resolve_struct(ty, self.module) else {
                    return;
                };
                let occurrences = structs_on_path
                    .iter()
                    .filter(|id| **id == struct_.id)
                    .count();
                if occurrences >= 2 {
                    return;
                }

                structs_on_path.push(struct_.id);
                for (index, field) in struct_.fields.iter().enumerate() {
                    let Some(field_ty) = field.value.ty.and_then(|id| self.declares.get_type(id))
                    else {
                        continue;
                    };
                    prefix.push(Step::Field(index));
                    self.collect_ref_subpaths(field_ty, prefix, structs_on_path, subpaths);
                    prefix.pop();
                }
                structs_on_path.pop();
            }
        }
    }

    fn contains_mutable_reference_inner(&self, ty: &SolType, visited: &mut Vec<NodeId>) -> bool {
        let inner_contains = |id, visited: &mut Vec<NodeId>| {
            self.declares
                .get_type(id)
                .is_some_and(|inner| self.contains_mutable_reference_inner(inner, visited))
        };

        match ty {
            SolType::Reference(reference) => {
                reference.mutable.is_mut() || inner_contains(reference.inner, visited)
            }
            SolType::Pointer(reference) => inner_contains(reference.inner, visited),
            SolType::Array(array) => {
                array.kind == ArrayKind::MutSlice || inner_contains(array.of_type, visited)
            }
            SolType::TupleKind(TupleKind::Tuple(types)) => {
                types.iter().any(|id| inner_contains(*id, visited))
            }
            _ => {
                let Some(struct_) = self.declares.resolve_struct(ty, self.module) else {
                    return false;
                };
                if visited.contains(&struct_.id) {
                    return false;
                }
                visited.push(struct_.id);
                struct_
                    .fields
                    .iter()
                    .filter_map(|field| field.value.ty)
                    .any(|id| inner_contains(id, visited))
            }
        }
    }
}
