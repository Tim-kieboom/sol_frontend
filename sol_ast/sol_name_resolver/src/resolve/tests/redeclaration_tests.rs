use ast_model::fault::AstErrorKind;

use crate::resolve::tests::{fault_count_matching, resolve_source};

fn is_already_exists_in_scope(kind: &AstErrorKind) -> bool {
    matches!(
        kind,
        AstErrorKind::TypeAlreadyExistsInScope { .. }
            | AstErrorKind::ValueAlreadyExistsInScope { .. }
    )
}

fn is_type_already_exists_named(name: &str) -> impl Fn(&AstErrorKind) -> bool + '_ {
    move |kind| {
        matches!(
            kind,
            AstErrorKind::TypeAlreadyExistsInScope { name: got } if got.as_ref() == name
        )
    }
}

#[test]
fn redeclaring_a_variable_in_the_same_scope_reports_exactly_one_fault() {
    let ast = resolve_source("main() {\n    a := 1\n    a := 2\n}\n");
    assert_eq!(fault_count_matching(&ast, is_already_exists_in_scope), 1);
}

#[test]
fn distinct_variable_names_in_the_same_scope_report_no_fault() {
    let ast = resolve_source("main() {\n    a := 1\n    b := 2\n}\n");
    assert_eq!(fault_count_matching(&ast, is_already_exists_in_scope), 0);
}

#[test]
fn same_variable_name_in_separate_function_scopes_reports_no_fault() {
    let ast = resolve_source("foo() {\n    a := 1\n}\nbar() {\n    a := 2\n}\n");
    assert_eq!(fault_count_matching(&ast, is_already_exists_in_scope), 0);
}

#[test]
fn redeclaring_a_struct_in_the_same_scope_reports_exactly_one_fault() {
    let ast = resolve_source("struct Foo {}\nstruct Foo {}\n");
    assert_eq!(
        fault_count_matching(&ast, is_type_already_exists_named("Foo")),
        1
    );
}

#[test]
fn distinct_struct_names_report_no_fault() {
    let ast = resolve_source("struct Foo {}\nstruct Bar {}\n");
    assert_eq!(fault_count_matching(&ast, is_already_exists_in_scope), 0);
}

#[test]
fn redeclaring_an_enum_in_the_same_scope_reports_exactly_one_fault() {
    let ast = resolve_source("enum Foo {\n    A\n}\nenum Foo {\n    B\n}\n");
    assert_eq!(
        fault_count_matching(&ast, is_type_already_exists_named("Foo")),
        1
    );
}

#[test]
fn a_struct_and_an_enum_sharing_a_name_reports_exactly_one_fault() {
    let ast = resolve_source("struct Foo {}\nenum Foo {\n    A\n}\n");
    assert_eq!(
        fault_count_matching(&ast, is_type_already_exists_named("Foo")),
        1
    );
}
