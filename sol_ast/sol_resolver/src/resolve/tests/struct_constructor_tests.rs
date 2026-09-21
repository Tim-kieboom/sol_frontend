use ast_model::fault::AstErrorKind;

use crate::resolve::tests::{fault_count_matching, resolve_source};

fn is_field_type_mismatch(kind: &AstErrorKind) -> bool {
    matches!(kind, AstErrorKind::FieldTypeMismatch { .. })
}

fn is_struct_has_no_field(kind: &AstErrorKind) -> bool {
    matches!(kind, AstErrorKind::StructHasNoField { .. })
}

fn is_generic_parameter_conflict(kind: &AstErrorKind) -> bool {
    matches!(kind, AstErrorKind::GenericParameterConflict { .. })
}

fn is_undefined_type(kind: &AstErrorKind) -> bool {
    matches!(kind, AstErrorKind::UndefinedType { .. })
}

/// Slice 7 of the `Stub`-redesign: `check_struct_constructor` used to
/// silently accept a struct-constructor naming a type that doesn't exist at
/// all, leaving it to be rejected only much later (if ever) in MIR
/// lowering. This is the same class of bug that motivated the whole
/// redesign — a name that resolves to nothing being caught far too late.
#[test]
fn constructor_naming_an_undefined_type_reports_exactly_one_fault() {
    let ast = resolve_source("main() {\n    NoSuchStruct { x: 1 }\n}\n");
    assert_eq!(fault_count_matching(&ast, is_undefined_type), 1);
}

#[test]
fn constructor_naming_a_real_struct_reports_no_undefined_type_fault() {
    let ast = resolve_source("struct Point { x: i64 }\nmain() {\n    Point { x: 1 }\n}\n");
    assert_eq!(fault_count_matching(&ast, is_undefined_type), 0);
}

fn is_field_type_mismatch_named(field_name: &str) -> impl Fn(&AstErrorKind) -> bool + '_ {
    move |kind| {
        matches!(
            kind,
            AstErrorKind::FieldTypeMismatch { field_name: got, .. } if got.as_ref() == field_name
        )
    }
}

#[test]
fn matching_field_types_report_no_fault() {
    let ast = resolve_source(
        "struct Point { x: i64\n    y: i64 }\nmain() {\n    Point { x: 1, y: 2 }\n}\n",
    );
    assert_eq!(fault_count_matching(&ast, is_field_type_mismatch), 0);
    assert_eq!(fault_count_matching(&ast, is_struct_has_no_field), 0);
}

#[test]
fn mismatched_field_type_reports_exactly_one_fault() {
    let ast = resolve_source(
        "struct Point { x: i64\n    y: i64 }\nmain() {\n    Point { x: 1, y: \"hi\" }\n}\n",
    );
    assert_eq!(
        fault_count_matching(&ast, is_field_type_mismatch_named("y")),
        1
    );
}

#[test]
fn unknown_field_name_reports_exactly_one_fault() {
    let ast = resolve_source(
        "struct Point { x: i64\n    y: i64 }\nmain() {\n    Point { x: 1, z: 2 }\n}\n",
    );
    assert_eq!(
        fault_count_matching(&ast, |kind| matches!(
            kind,
            AstErrorKind::StructHasNoField { struct_name, field_name }
                if struct_name.as_ref() == "Point" && field_name.as_ref() == "z"
        )),
        1
    );
}

#[test]
fn omitted_field_reports_no_fault() {
    let ast =
        resolve_source("struct Point { x: i64\n    y: i64 }\nmain() {\n    Point { x: 1 }\n}\n");
    assert_eq!(fault_count_matching(&ast, is_field_type_mismatch), 0);
    assert_eq!(fault_count_matching(&ast, is_struct_has_no_field), 0);
}

#[test]
fn generic_struct_field_is_skipped_without_fault() {
    let ast = resolve_source("struct Box<T> { value: T }\nmain() {\n    Box { value: 1 }\n}\n");
    assert_eq!(
        fault_count_matching(&ast, is_field_type_mismatch_named("value")),
        0
    );
}

#[test]
fn same_generic_used_on_two_fields_with_incompatible_types_reports_exactly_one_fault() {
    let ast = resolve_source(
        "struct Pair<T> { a: T\n    b: T }\nmain() {\n    Pair { a: 1, b: \"hi\" }\n}\n",
    );
    assert_eq!(fault_count_matching(&ast, is_generic_parameter_conflict), 1);
}

#[test]
fn same_generic_used_on_two_fields_with_matching_types_reports_no_fault() {
    let ast =
        resolve_source("struct Pair<T> { a: T\n    b: T }\nmain() {\n    Pair { a: 1, b: 2 }\n}\n");
    assert_eq!(fault_count_matching(&ast, is_generic_parameter_conflict), 0);
}
