use std::collections::HashMap;

use crate::{
    CustomType, Enum, ExpressionId, InnerFunctionSignature, NodeId, SoulType, Struct, Trait, TypeId,
};
use soul_utils::{
    FunctionId, SharedStr, TypeModifier,
    collections::{bimap::BiMap, vec_map::VecMap},
    ids::IdGenerator,
    intrinsics::IntrinsicFunction,
    span::ModuleId,
};

/// A store of all declarations in a module.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DeclareStore {
    /// The main function (entry point), if defined.
    pub main_function: Option<FunctionId>,
    /// All variable resolutions, indexed by their ID.
    variable_resolves: VecMap<NodeId, NodeId>,
    /// All functionCall resolutions, indexed by their ID.
    function_resolves: VecMap<NodeId, FunctionResolve>,
    /// All `intrinsic.*` call resolutions, indexed by their ID.
    intrinsic_resolves: VecMap<NodeId, IntrinsicResolve>,
    /// All structs declarations, indexed by their ID.
    custom_types: VecMap<NodeId, (CustomType, ModuleId)>,
    /// Struct name -> declaration `NodeId`, per module. Lets a later pass
    /// (MIR lowering) resolve a struct-typed `SoulType::Stub`'s bare name
    /// back to its `Struct` declaration without re-walking scopes the way
    /// name resolution itself does — struct declarations only ever live at
    /// module scope, so a flat per-module map is enough (no nested-scope
    /// shadowing to account for, unlike local variables). Keyed by `ModuleId`
    /// first (rather than a single `HashMap<(SharedStr, ModuleId), _>`) so a
    /// lookup can hash a bare `&str` against the inner map instead of having
    /// to allocate an owned `SharedStr` just to build a lookup key.
    struct_names: VecMap<ModuleId, HashMap<SharedStr, NodeId>>,
    /// All function declarations, indexed by their ID.
    functions: VecMap<FunctionId, (InnerFunctionSignature, ModuleId)>,
    /// All function declarations, indexed by their ID.
    function_names: HashMap<SharedStr, Vec<FunctionId>>,
    /// The synthetic `NodeId` name resolution bound `this`/`self` to inside a
    /// non-static method's body (`this` isn't a real `Parameter` in the AST —
    /// see `collect_function`'s `insert_value("this", ..)` — so a later pass
    /// like MIR lowering has no other way to find the id it needs to map a
    /// `this` local onto).
    receiver_bindings: VecMap<FunctionId, NodeId>,
    /// Variable type information, indexed by node ID.
    variable_type: VecMap<NodeId, (TypeModifier, Option<SoulType>, ModuleId)>,
    /// Resolved type of an expression, indexed by its ID.
    expression_types: VecMap<ExpressionId, SoulType>,
    /// Non-`distinct` `type X := Y` aliases, mapping `X`'s name to its
    /// underlying type `Y`. A `distinct` alias is deliberately not
    /// interchangeable with its underlying type, so it's never registered here.
    type_aliases: HashMap<SharedStr, SoulType>,
    /// Canonical `TypeId <-> SoulType` interning table — see [`Self::intern_type`].
    type_ids: BiMap<TypeId, SoulType>,
    type_id_alloc: IdGenerator<TypeId>,
    /// A `Stub` occurrence's resolved meaning, keyed on that occurrence's own
    /// (now per-occurrence-unique, see `Stub::occurrence`) `TypeId` — mirrors
    /// `variable_resolves`/`function_resolves`'s "referencing id -> resolved
    /// declaration id" shape, for types instead of variables/calls. Populated
    /// lazily wherever a `Stub` is actually resolved during the resolve/
    /// typecheck phase (`struct_field_type`, `check_struct_constructor`, ...)
    /// — deliberately *not* eagerly during collection (`NameResolver::
    /// collect_type`'s call sites run in file order within a single linear
    /// pass, so a struct declared *after* the code referencing it wouldn't be
    /// registered yet; typecheck-phase resolution runs only after collection
    /// fully finishes, so it doesn't have that ordering hazard). Coverage is
    /// therefore partial by design at this slice — see `resolve_struct`'s own
    /// docs for the fallback this implies. See `TODO.md`'s `Stub`-redesign
    /// entry for the full multi-slice plan this is part of.
    type_resolves: VecMap<TypeId, TypeResolve>,
}

/// What a `Stub` occurrence turned out to name, once resolved. Only
/// `Struct` is ever produced today (Slice 2 of the `Stub`-redesign); the
/// other variants exist now so downstream matches are exhaustive against
/// the shape the later slices will actually populate, not because anything
/// constructs them yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TypeResolve {
    /// The struct declaration's own `NodeId` (a `custom_types` key).
    Struct(NodeId),
    /// The enum declaration's own `NodeId`.
    Enum(NodeId),
    /// The trait declaration's own `NodeId`.
    Trait(NodeId),
    /// The already-interned underlying type of a non-`distinct`
    /// `type X := Y` alias.
    Alias(TypeId),
    /// Confirmed to name an in-scope generic parameter — not an error, and
    /// not a declaration; stays symbolic until monomorphization (M3).
    Generic,
}
impl Default for DeclareStore {
    fn default() -> Self {
        Self::new()
    }
}
impl DeclareStore {
    /// Creates a new declaration store, pre-populated with the well-known
    /// `TypeId`s (see `TypeId::NONE`/`TypeId::PRIM_INT`/etc.) — never construct
    /// a `DeclareStore` any other way, or those constants stop matching what
    /// `intern_type` actually returns for those types.
    pub fn new() -> Self {
        let mut this = Self {
            main_function: None,
            variable_resolves: VecMap::new(),
            functions: VecMap::new(),
            custom_types: VecMap::new(),
            struct_names: VecMap::new(),
            variable_type: VecMap::new(),
            function_names: HashMap::new(),
            function_resolves: VecMap::new(),
            intrinsic_resolves: VecMap::new(),
            expression_types: VecMap::new(),
            type_aliases: HashMap::new(),
            receiver_bindings: VecMap::new(),
            type_ids: BiMap::new(),
            type_id_alloc: IdGenerator::new(),
            type_resolves: VecMap::new(),
        };
        this.register_well_known_types();
        this
    }

    /// Interns every context-free `SoulType` in the exact order `TypeId`'s
    /// well-known constants (`TypeId::NONE`, `TypeId::PRIM_INT`, ...) assume —
    /// see the comment on that `impl TypeId` block. Each `debug_assert_eq!`
    /// is the only thing standing between "the constants are right" and "the
    /// constants silently drifted" — this function's own ordering *is* the
    /// spec, so treat any failure here as this function being wrong, not the
    /// constant.
    fn register_well_known_types(&mut self) {
        use soul_utils::soul_names::PrimitiveTypes as Prim;
        macro_rules! register {
            () => {};
            ($($name:expr => $ty:expr),* $(,)?) => {
                $(
                    {
                        let id = self.intern_type($ty);
                        debug_assert_eq!(
                            id,
                            $name,
                            concat!(
                                "well-known TypeId order drifted: ",
                                stringify!($name)
                            )
                        );
                    }
                )*
            };
        }

        register!(
            TypeId::NONE =>                 SoulType::None,
            TypeId::NEVER =>                SoulType::Never,
            TypeId::STRING =>               SoulType::String,
            TypeId::FORMAT_STRING =>        SoulType::FormatString,

            TypeId::ANY =>                  SoulType::Any,
            TypeId::TYPE =>                 SoulType::Type,
            TypeId::ERROR_TYPE =>           SoulType::Error,

            TypeId::PRIM_CHAR =>            SoulType::Primitive(Prim::Char),
            TypeId::PRIM_CHAR8 =>           SoulType::Primitive(Prim::Char8),
            TypeId::PRIM_CHAR16 =>          SoulType::Primitive(Prim::Char16),
            TypeId::PRIM_CHAR32 =>          SoulType::Primitive(Prim::Char32),
            TypeId::PRIM_CHAR64 =>          SoulType::Primitive(Prim::Char64),

            TypeId::PRIM_CSTR =>            SoulType::Primitive(Prim::CStr),
            TypeId::PRIM_NONE =>            SoulType::Primitive(Prim::None),
            TypeId::PRIM_BOOLEAN =>         SoulType::Primitive(Prim::Boolean),
            TypeId::PRIM_CINT =>            SoulType::Primitive(Prim::CInt),

            TypeId::PRIM_UNTYPED_INT =>     SoulType::Primitive(Prim::UntypedInt),
            TypeId::PRIM_INT =>             SoulType::Primitive(Prim::Int),
            TypeId::PRIM_INT8 =>            SoulType::Primitive(Prim::Int8),
            TypeId::PRIM_INT16 =>           SoulType::Primitive(Prim::Int16),
            TypeId::PRIM_INT32 =>           SoulType::Primitive(Prim::Int32),
            TypeId::PRIM_INT64 =>           SoulType::Primitive(Prim::Int64),
            TypeId::PRIM_INT128 =>          SoulType::Primitive(Prim::Int128),
            TypeId::PRIM_CUINT =>           SoulType::Primitive(Prim::CUint),

            TypeId::PRIM_UNTYPED_UINT =>    SoulType::Primitive(Prim::UntypedUint),
            TypeId::PRIM_UINT =>            SoulType::Primitive(Prim::Uint),
            TypeId::PRIM_UINT8 =>           SoulType::Primitive(Prim::Uint8),
            TypeId::PRIM_UINT16 =>          SoulType::Primitive(Prim::Uint16),
            TypeId::PRIM_UINT32 =>          SoulType::Primitive(Prim::Uint32),
            TypeId::PRIM_UINT64 =>          SoulType::Primitive(Prim::Uint64),
            TypeId::PRIM_UINT128 =>         SoulType::Primitive(Prim::Uint128),

            TypeId::PRIM_UNTYPED_FLOAT =>   SoulType::Primitive(Prim::UntypedFloat),
            TypeId::PRIM_FLOAT16 =>         SoulType::Primitive(Prim::Float16),
            TypeId::PRIM_FLOAT32 =>         SoulType::Primitive(Prim::Float32),
            TypeId::PRIM_FLOAT64 =>         SoulType::Primitive(Prim::Float64),
        );
    }

    /// Inserts a function into the store.
    pub fn insert_functions(
        &mut self,
        index: FunctionId,
        function: InnerFunctionSignature,
        module: ModuleId,
    ) {
        if let Some(entries) = self.function_names.get_mut(function.name.as_str()) {
            entries.push(index);
        } else {
            self.function_names
                .insert(function.name.as_shared_str(), vec![index]);
        }
        self.functions.insert(index, (function, module));
    }

    /// Retrieves a function by its ID.
    pub fn get_function(&self, index: FunctionId) -> Option<&(InnerFunctionSignature, ModuleId)> {
        self.functions.get(index)
    }

    /// Records the synthetic `NodeId` a non-static method's `this` resolves
    /// to inside its own body.
    pub fn insert_receiver_binding(&mut self, function: FunctionId, this_node: NodeId) {
        self.receiver_bindings.insert(function, this_node);
    }

    /// Retrieves the synthetic `this` `NodeId` for a non-static method,
    /// if any.
    pub fn get_receiver_binding(&self, function: FunctionId) -> Option<NodeId> {
        self.receiver_bindings.get(function).copied()
    }

    /// Retrieves the resolved function-call info for a call-expression node.
    pub fn get_call_resolve(&self, id: NodeId) -> Option<&FunctionResolve> {
        self.function_resolves.get(id)
    }

    /// Retrieves the node ID a variable reference resolves to.
    pub fn get_variable_resolve(&self, id: NodeId) -> Option<NodeId> {
        self.variable_resolves.get(id).copied()
    }

    /// Finds a function by name and optional owner type (for method
    /// resolution) — e.g. two different trait impls on the same type each
    /// defining a same-named method are `Ambiguous`, not "pick the first".
    ///
    /// Each function is registered twice (once from `collect_function`, once
    /// from `resolve_function`), so a matching `FunctionId` seen more than
    /// once is not itself a collision — only a match against a *different*
    /// `FunctionId` is.
    pub fn find_function(&self, name: &str, owner_type: Option<&SoulType>) -> FunctionLookup {
        let Some(functions) = self.function_names.get(name) else {
            return FunctionLookup::NotFound;
        };

        let mut found: Option<FunctionId> = None;
        for id in functions {
            let (signature, _) = &self.functions[*id];
            let is_match = match owner_type {
                Some(owner) => self
                    .get_type(signature.method_type)
                    .is_some_and(|method_type| {
                        self.soul_types_equal_ignoring_occurrence(method_type, owner)
                    }),
                None => signature.method_type == TypeId::NONE,
            };
            if !is_match {
                continue;
            }
            match found {
                None => found = Some(*id),
                Some(prev) if prev == *id => {}
                Some(_) => return FunctionLookup::Ambiguous,
            }
        }

        match found {
            Some(id) => FunctionLookup::Found(id),
            None => FunctionLookup::NotFound,
        }
    }

    /// try Inserts a enum into the store.
    pub fn try_insert_enum(&mut self, index: NodeId, obj: &Enum, module: ModuleId) {
        if self.custom_types.contains(index) {
            return;
        }

        self.custom_types
            .insert(index, (CustomType::Enum(obj.clone()), module));
    }

    /// try Inserts a trait into the store.
    pub fn try_insert_trait(&mut self, index: NodeId, obj: &Trait, module: ModuleId) {
        if self.custom_types.contains(index) {
            return;
        }

        self.custom_types
            .insert(index, (CustomType::Trait(obj.clone()), module));
    }

    /// try Inserts a struct into the store.
    pub fn try_insert_struct(&mut self, index: NodeId, obj: &Struct, module: ModuleId) {
        if self.custom_types.contains(index) {
            return;
        }

        self.struct_names
            .get_mut_or_default(module)
            .insert(obj.name.as_shared_str(), index);
        self.custom_types
            .insert(index, (CustomType::Struct(obj.clone()), module));
    }

    /// Retrieves a struct/enum/trait declaration by its own declaration NodeId.
    pub fn get_custom_type(&self, index: NodeId) -> Option<&(CustomType, ModuleId)> {
        self.custom_types.get(index)
    }

    /// Resolves a struct's bare name (e.g. the name inside a
    /// `SoulType::Stub` that a struct-typed value carries) back to its
    /// `Struct` declaration, scoped to the module it was declared in.
    pub fn get_struct_by_name(&self, name: &str, module: ModuleId) -> Option<&Struct> {
        let index = *self.struct_names.get(module)?.get(name)?;
        match self.custom_types.get(index) {
            Some((CustomType::Struct(struct_), _)) => Some(struct_),
            _ => None,
        }
    }

    /// Records what a `Stub` occurrence (identified by its own, now
    /// per-occurrence-unique `TypeId` — see `Stub::occurrence`) resolved to.
    /// Returns the previous entry, if any (there normally shouldn't be one —
    /// each occurrence is resolved at most once).
    pub fn insert_type_resolve(&mut self, occurrence: TypeId, resolve: TypeResolve) -> Option<TypeResolve> {
        self.type_resolves.insert(occurrence, resolve)
    }

    /// The previously-recorded resolution for a `Stub` occurrence, if any —
    /// see `insert_type_resolve`. Coverage is partial (Slice 2 of the
    /// `Stub`-redesign only populates this for occurrences the resolve/
    /// typecheck phase actually visits, e.g. via `struct_field_type`/
    /// `check_struct_constructor` — a bare parameter/return-type annotation
    /// never used in a field access or constructor expression anywhere in
    /// its function isn't covered yet), so callers should always fall back
    /// to a name-based lookup when this returns `None` — `resolve_struct`
    /// already does this.
    pub fn get_type_resolve(&self, occurrence: TypeId) -> Option<TypeResolve> {
        self.type_resolves.get(occurrence).copied()
    }

    /// Resolves a struct-typed `SoulType::Stub`'s bare name back to its
    /// `Struct` declaration, scoped to `module`. `None` for anything that
    /// isn't a `Stub`, or a `Stub` that doesn't name a struct visible from
    /// `module` (an enum/trait, a generic, or an unresolved name). Shared by
    /// `mir_parser` and `mir_codegen` — both used to carry an identical
    /// private copy of this same lookup.
    ///
    /// Checks `type_resolves` first (a `TypeId`-keyed cache, sound now that
    /// `Stub` carries a per-occurrence identity — see `Stub::occurrence`),
    /// falling back to the `module`-scoped name lookup when this occurrence
    /// hasn't been resolved that way yet (`type_resolves`' coverage is
    /// partial by design at this slice, see its own docs) — `module` stays
    /// a required parameter for exactly that fallback, not because caching
    /// is unsound any more.
    pub fn resolve_struct(&self, ty: &SoulType, module: Option<ModuleId>) -> Option<&Struct> {
        let SoulType::Stub(stub) = ty else {
            return None;
        };
        if let Some(TypeResolve::Struct(node_id)) =
            self.get_type_id(ty).and_then(|id| self.get_type_resolve(id))
        {
            return match self.custom_types.get(node_id) {
                Some((CustomType::Struct(struct_), _)) => Some(struct_),
                _ => None,
            };
        }
        self.get_struct_by_name(&stub.name, module?)
    }

    /// Whether `ty` is one this compiler's MIR lowering can turn into a local
    /// at all: primitives, structs whose name resolves to a declaration
    /// visible from `module`, fixed-size-array/slice-typed arrays (`[N]T`,
    /// `[&]T`, `[&mut]T`), and bare `&T`/`&mut T`/`*T` references. A
    /// reference is just an opaque pointer-sized value here (no recursive
    /// check on its inner type). Everything else (wildcard/heap arrays,
    /// generics, an undeclared/unresolvable name) isn't. Pure classification
    /// — callers that need a diagnostic on failure (`mir_parser`'s own
    /// `require_lowerable`) wrap this with their own `Fault`.
    pub fn is_lowerable(&self, ty: &SoulType, module: Option<ModuleId>) -> bool {
        let is_lowerable_array =
            matches!(ty, SoulType::Array(array) if array.kind.is_lowerable());
        matches!(ty, SoulType::Primitive(_) | SoulType::Reference(_) | SoulType::Pointer(_))
            || self.resolve_struct(ty, module).is_some()
            || is_lowerable_array
    }

    /// Classifies `ty` as `AutoCopy` (`Operand::Copy` is safe — reading it
    /// doesn't invalidate the source) or move-only (`Operand::Move` needed).
    /// Only meaningful for a type `is_lowerable` already accepts — callers
    /// that need to reject an unlowerable type first should check that
    /// themselves (this never panics on one, it just falls through to the
    /// `false`/move-only default below, same as a resolved struct would).
    ///
    /// Primitives and references are `AutoCopy` — a `&T`/`&mut T` reference
    /// never owns what it points at, so copying it is always sound
    /// regardless of what's on the other end. A slice (`[&]T`/`[&mut]T`) is
    /// the same fat-pointer case: non-owning, so `AutoCopy` too, even though
    /// it's a `SoulType::Array`.
    ///
    /// `SoulType::Pointer` (`*T`) is deliberately **not** `AutoCopy` — unlike
    /// `&T`, a `*T` is an *owning* heap pointer (`new(expr)`'s own result
    /// type; its `Drop` frees the allocation), so it's move-only, same as a
    /// struct: copying it would produce two "owners" of the same allocation,
    /// both trying to free it. A resolved struct and an *owning* array
    /// (`StackArray`/`HeapArray`) are move-only for the same reason.
    pub fn is_auto_copy(&self, ty: &SoulType) -> bool {
        match ty {
            SoulType::Primitive(_) | SoulType::Reference(_) => true,
            SoulType::Array(array) => {
                matches!(
                    array.kind,
                    crate::ArrayKind::MutSlice | crate::ArrayKind::ConstSlice
                )
            }
            _ => false,
        }
    }

    /// Structural equality between two types, treating a `Stub` as equal to
    /// another `Stub` whenever their `name`/`generics` match — ignoring
    /// `occurrence` at every level of nesting.
    ///
    /// Needed because `Stub.occurrence` (a fresh `NodeId` per syntactic
    /// occurrence) participates in `Stub`'s own derived `Eq`, so two
    /// *independently written* occurrences of the same name (e.g. a
    /// parameter's declared type vs. an argument expression's inferred type)
    /// are never `==` any more, even when they name the same struct/enum/
    /// trait/alias/generic. Every type-checking comparison that means "is
    /// this the same type" (argument-vs-parameter, declared-vs-initializer,
    /// trait-method-vs-impl-method signatures, ...) needs this instead of a
    /// bare `==`/`PartialEq` — see `TODO.md`'s `Stub`-redesign entry.
    ///
    /// This restores today's pre-`occurrence` "same name = same type"
    /// semantics without needing full resolved-identity (`type_resolves`,
    /// a later slice of that redesign) — a stopgap, not the end state: once
    /// `type_resolves` exists, this should compare *resolved declarations*
    /// instead of raw names, which would also correctly distinguish two
    /// different modules' same-named-but-different structs (this function
    /// cannot do that yet, same limitation `resolve_struct` already has).
    pub fn types_equal_ignoring_occurrence(&self, a: TypeId, b: TypeId) -> bool {
        if a == b {
            return true;
        }
        match (self.get_type(a), self.get_type(b)) {
            (Some(ta), Some(tb)) => self.soul_types_equal_ignoring_occurrence(ta, tb),
            _ => false,
        }
    }

    pub fn soul_types_equal_ignoring_occurrence(&self, a: &SoulType, b: &SoulType) -> bool {
        let ids_eq = |x: TypeId, y: TypeId| self.types_equal_ignoring_occurrence(x, y);
        let opt_ids_eq = |x: Option<TypeId>, y: Option<TypeId>| match (x, y) {
            (Some(x), Some(y)) => ids_eq(x, y),
            (None, None) => true,
            _ => false,
        };
        match (a, b) {
            (SoulType::Stub(sa), SoulType::Stub(sb)) => {
                sa.name == sb.name
                    && sa.generics.len() == sb.generics.len()
                    && sa
                        .generics
                        .iter()
                        .zip(sb.generics.iter())
                        .all(|(&x, &y)| ids_eq(x, y))
            }
            (SoulType::Array(aa), SoulType::Array(ab)) => {
                aa.kind == ab.kind && ids_eq(aa.of_type, ab.of_type)
            }
            (SoulType::Reference(ra), SoulType::Reference(rb))
            | (SoulType::Pointer(ra), SoulType::Pointer(rb)) => {
                ra.mutable == rb.mutable && ids_eq(ra.inner, rb.inner)
            }
            (SoulType::RawPtr(pa), SoulType::RawPtr(pb)) => opt_ids_eq(*pa, *pb),
            (SoulType::Res { ok: oa, err: ea }, SoulType::Res { ok: ob, err: eb }) => {
                opt_ids_eq(*oa, *ob) && opt_ids_eq(*ea, *eb)
            }
            (SoulType::Optional(pa), SoulType::Optional(pb))
            | (SoulType::ImplTrait(pa), SoulType::ImplTrait(pb)) => ids_eq(*pa, *pb),
            (
                SoulType::NamedVariant {
                    base: ba,
                    variant: va,
                },
                SoulType::NamedVariant {
                    base: bb,
                    variant: vb,
                },
            ) => va == vb && ids_eq(*ba, *bb),
            (
                SoulType::Function {
                    arity: aa,
                    return_type: ra,
                },
                SoulType::Function {
                    arity: ab,
                    return_type: rb,
                },
            ) => aa == ab && ids_eq(*ra, *rb),
            (SoulType::TupleKind(ta), SoulType::TupleKind(tb)) => match (ta, tb) {
                (crate::TupleKind::Tuple(xs), crate::TupleKind::Tuple(ys)) => {
                    xs.len() == ys.len() && xs.iter().zip(ys.iter()).all(|(&x, &y)| ids_eq(x, y))
                }
                (crate::TupleKind::NamedTuple(xs), crate::TupleKind::NamedTuple(ys)) => {
                    xs.len() == ys.len()
                        && xs
                            .iter()
                            .zip(ys.iter())
                            .all(|((nx, x), (ny, y))| nx == ny && ids_eq(*x, *y))
                }
                _ => false,
            },
            // Everything else (None/Never/String/FormatString/Any/Type/
            // Primitive/Error) never nests a Stub, so plain structural
            // equality is exactly right and already covers it.
            _ => a == b,
        }
    }

    /// Records that a variable reference node resolves to the declaration
    /// node `resolved`. Returns the previously stored resolution, if any.
    pub fn insert_variable_resolve(&mut self, node_id: NodeId, resolved: NodeId) -> Option<NodeId> {
        self.variable_resolves.insert(node_id, resolved)
    }

    /// Records the resolved function-call info for a call-expression node.
    /// Returns the previously stored resolution, if any.
    pub fn insert_function_resolve(
        &mut self,
        node_id: NodeId,
        function: FunctionResolve,
    ) -> Option<FunctionResolve> {
        self.function_resolves.insert(node_id, function)
    }

    /// Retrieves the resolved function-call info for a call-expression node.
    pub fn get_function_resolve(&self, node_id: NodeId) -> Option<FunctionResolve> {
        self.function_resolves.get(node_id).copied()
    }

    /// Records the resolved intrinsic-call info for a call-expression node.
    /// Returns the previously stored resolution, if any.
    pub fn insert_intrinsic_resolve(
        &mut self,
        node_id: NodeId,
        intrinsic: IntrinsicResolve,
    ) -> Option<IntrinsicResolve> {
        self.intrinsic_resolves.insert(node_id, intrinsic)
    }

    /// Retrieves the resolved intrinsic-call info for a call-expression node.
    pub fn get_intrinsic_resolve(&self, node_id: NodeId) -> Option<IntrinsicResolve> {
        self.intrinsic_resolves.get(node_id).copied()
    }

    /// Finds a function declared by `name` within a specific module.
    pub fn find_function_in_module(&self, name: &str, module: ModuleId) -> Option<FunctionId> {
        let functions = self.function_names.get(name)?;
        for id in functions {
            let (_, module_id) = self.functions[*id];
            if module_id == module {
                return Some(*id);
            }
        }

        None
    }

    /// Returns all function declarations in the store, indexed by their ID.
    pub fn functions(&self) -> &VecMap<FunctionId, (InnerFunctionSignature, ModuleId)> {
        &self.functions
    }

    /// Gets the type of a variable by its node ID.
    pub fn get_variable_type(
        &self,
        index: NodeId,
    ) -> Option<&(TypeModifier, Option<SoulType>, ModuleId)> {
        self.variable_type.get(index)
    }

    /// Sets the type of a variable.
    pub fn insert_variable_type(
        &mut self,
        index: NodeId,
        modifier: TypeModifier,
        ty: Option<SoulType>,
        module: ModuleId,
    ) {
        self.variable_type.insert(index, (modifier, ty, module));
    }

    /// Gets the resolved type of an expression by its ID.
    pub fn get_expression_type(&self, index: ExpressionId) -> Option<&SoulType> {
        self.expression_types.get(index)
    }

    /// Sets the resolved type of an expression.
    pub fn insert_expression_type(&mut self, index: ExpressionId, ty: SoulType) {
        self.expression_types.insert(index, ty);
    }

    /// Registers a non-`distinct` `type X := Y` alias's underlying type.
    pub fn insert_type_alias(&mut self, name: impl Into<SharedStr>, underlying: SoulType) {
        self.type_aliases.insert(name.into(), underlying);
    }

    /// The underlying type of a non-`distinct` alias by name, if `name` is one.
    pub fn get_type_alias(&self, name: &str) -> Option<&SoulType> {
        self.type_aliases.get(name)
    }

    /// Interns `ty`, returning its canonical `TypeId`. Interning the same
    /// `SoulType` value twice (by `PartialEq`) always returns the same id —
    /// this is the single entry point that keeps the `TypeId <-> SoulType`
    /// mapping 1:1, so callers should never construct a `TypeId` any other way.
    pub fn intern_type(&mut self, ty: SoulType) -> TypeId {
        self.type_ids.insert(&mut self.type_id_alloc, ty)
    }

    /// Looks up the canonical `SoulType` a `TypeId` was interned from.
    pub fn get_type(&self, id: TypeId) -> Option<&SoulType> {
        self.type_ids.get_value(id)
    }

    /// Looks up the `TypeId` `ty` was interned as, without interning it if
    /// it hasn't been seen before.
    pub fn get_type_id(&self, ty: &SoulType) -> Option<TypeId> {
        self.type_ids.get_key(ty)
    }
}

/// Outcome of a name+owner-type function/method lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionLookup {
    Found(FunctionId),
    NotFound,
    /// More than one function named `name` matches this owner type — e.g.
    /// two different trait impls on the same type each defining a
    /// same-named method. Ambiguous, not resolvable by "pick the first".
    Ambiguous,
}

/// The resolved target of a function call.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct FunctionResolve {
    /// The ID of the resolved function.
    pub id: FunctionId,
    /// Whether the call is deferred (executed at scope exit).
    pub is_defer: bool,
    /// Whether the callee expression should be ignored when generating code
    /// for the call (e.g. because it was only used for method resolution).
    pub ignore_callee: bool,
}

/// The resolved target of an `intrinsic.*` call.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct IntrinsicResolve {
    /// Which intrinsic function the call resolves to.
    pub kind: IntrinsicFunction,
}
