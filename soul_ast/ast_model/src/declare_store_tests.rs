use crate::{
    ArrayKind, ArrayType, NodeId, SoulType, Struct, Stub, TypeId,
    declare_store::{DeclareStore, TypeResolve},
};
use soul_utils::{
    Ident,
    collections::{array::RcArr, vec_map::VecMapIndex},
    soul_names::PrimitiveTypes,
    span::{ModuleId, Span},
};

fn empty_struct(id: NodeId, name: &str) -> Struct {
    Struct {
        id,
        name: Ident::new(name, Span::error()),
        fields: RcArr::new(),
        generics: RcArr::new(),
        statements: RcArr::new(),
    }
}

#[test]
fn interning_the_same_type_twice_returns_the_same_id() {
    let mut declares = DeclareStore::new();
    let int_ty = SoulType::Primitive(PrimitiveTypes::Int);

    let first = declares.intern_type(int_ty.clone());
    let second = declares.intern_type(int_ty);

    assert_eq!(first, second);
}

#[test]
fn structurally_equal_nested_types_built_separately_canonicalize_to_the_same_id() {
    let mut declares = DeclareStore::new();
    let build_array_of_int = |declares: &mut DeclareStore| {
        SoulType::Array(ArrayType {
            of_type: declares.intern_type(SoulType::Primitive(PrimitiveTypes::Int)),
            kind: ArrayKind::HeapArray,
        })
    };

    let first_ty = build_array_of_int(&mut declares);
    let second_ty = build_array_of_int(&mut declares);
    let first = declares.intern_type(first_ty);
    let second = declares.intern_type(second_ty);

    assert_eq!(first, second);
}

#[test]
fn different_types_get_different_ids() {
    let mut declares = DeclareStore::new();

    let int_id = declares.intern_type(SoulType::Primitive(PrimitiveTypes::Int));
    let bool_id = declares.intern_type(SoulType::Primitive(PrimitiveTypes::Boolean));

    assert_ne!(int_id, bool_id);
}

#[test]
fn get_type_resolves_an_interned_id_back_to_its_canonical_soul_type() {
    let mut declares = DeclareStore::new();
    let int_ty = SoulType::Primitive(PrimitiveTypes::Int);

    let id = declares.intern_type(int_ty.clone());

    assert_eq!(declares.get_type(id), Some(&int_ty));
}

#[test]
fn get_type_id_does_not_intern_an_unseen_type() {
    let declares = DeclareStore::new();
    let unseen_ty = SoulType::Stub(Stub::new("SomeStructNeverInterned"));

    assert_eq!(declares.get_type_id(&unseen_ty), None);
}

#[test]
fn well_known_type_ids_are_preinterned_by_new() {
    let declares = DeclareStore::new();

    assert_eq!(declares.get_type(TypeId::NONE), Some(&SoulType::None));
    assert_eq!(
        declares.get_type(TypeId::ERROR_TYPE),
        Some(&SoulType::Error)
    );
    assert_eq!(
        declares.get_type(TypeId::PRIM_INT),
        Some(&SoulType::Primitive(PrimitiveTypes::Int))
    );
    assert_eq!(
        declares.get_type(TypeId::PRIM_BOOLEAN),
        Some(&SoulType::Primitive(PrimitiveTypes::Boolean))
    );
}

#[test]
fn interning_a_well_known_type_again_returns_its_comptime_constant() {
    let mut declares = DeclareStore::new();

    // Everything else in the compiler builds these types by constructing a
    // fresh SoulType and interning it — never by naming the TypeId constant
    // directly. This is the guarantee that makes `some_id == TypeId::PRIM_INT`
    // valid: the constant and a freshly-interned equivalent must agree.
    let id = declares.intern_type(SoulType::Primitive(PrimitiveTypes::Int));
    assert_eq!(id, TypeId::PRIM_INT);

    let id = declares.intern_type(SoulType::None);
    assert_eq!(id, TypeId::NONE);
}

#[test]
fn type_resolve_round_trips_through_insert_and_get() {
    let mut declares = DeclareStore::new();
    let occurrence = declares.intern_type(SoulType::Stub(Stub::new("Point")));
    let struct_id = NodeId::new_index(1);

    assert_eq!(declares.get_type_resolve(occurrence), None);
    declares.insert_type_resolve(occurrence, TypeResolve::Struct(struct_id));
    assert_eq!(
        declares.get_type_resolve(occurrence),
        Some(TypeResolve::Struct(struct_id))
    );
}

/// The whole point of `type_resolves`: once an occurrence is resolved and
/// cached, `resolve_struct` must prefer that cached answer over the
/// module-scoped name lookup — this is what will let two different
/// modules' same-named-but-different structs resolve correctly through the
/// same API, once every occurrence is cached (later slices), instead of
/// silently returning whichever module's struct the name lookup happens to
/// find first.
#[test]
fn resolve_struct_prefers_the_cached_resolution_over_the_name_lookup() {
    let mut declares = DeclareStore::new();
    let module = ModuleId::new_index(0);

    let name_lookup_struct = empty_struct(NodeId::new_index(1), "Point");
    declares.try_insert_struct(name_lookup_struct.id, &name_lookup_struct, module);

    let cached_struct = empty_struct(NodeId::new_index(2), "Point");
    declares.try_insert_struct(cached_struct.id, &cached_struct, ModuleId::new_index(1));

    let occurrence = declares.intern_type(SoulType::Stub(Stub::new("Point")));
    let ty = declares.get_type(occurrence).cloned().unwrap();

    // No cache entry yet: falls back to the module-scoped name lookup.
    assert_eq!(
        declares.resolve_struct(&ty, Some(module)).map(|s| s.id),
        Some(name_lookup_struct.id)
    );

    declares.insert_type_resolve(occurrence, TypeResolve::Struct(cached_struct.id));

    // Cached now: wins even though the name lookup would still find the
    // other struct.
    assert_eq!(
        declares.resolve_struct(&ty, Some(module)).map(|s| s.id),
        Some(cached_struct.id)
    );
}
