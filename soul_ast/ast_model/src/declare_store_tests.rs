use crate::{ArrayKind, ArrayType, SoulType, declare_store::DeclareStore};
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
    let build_array_of_int = || {
        SoulType::Array(ArrayType {
            of_type: Box::new(SoulType::Primitive(PrimitiveTypes::Int)),
            kind: ArrayKind::HeapArray,
        })
    };

    let first = declares.intern_type(build_array_of_int());
    let second = declares.intern_type(build_array_of_int());

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
    let int_ty = SoulType::Primitive(PrimitiveTypes::Int);

    assert_eq!(declares.get_type_id(&int_ty), None);
}
