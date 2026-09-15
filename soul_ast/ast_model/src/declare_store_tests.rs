use crate::{ArrayKind, ArrayType, SoulType, Stub, TypeId, declare_store::DeclareStore};
use soul_utils::soul_names::PrimitiveTypes;

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
