use ast_model::fault::AstErrorKind;

use crate::resolve::tests::{fault_count_matching, resolve_source};



fn is_undefined_variable_named(name: &str) -> impl Fn(&AstErrorKind) -> bool + '_ {
    move |kind| matches!(kind, AstErrorKind::UndefinedVariable { name: got } if got.as_ref() == name)
}

fn is_undefined_variable(kind: &AstErrorKind) -> bool {
    matches!(kind, AstErrorKind::UndefinedVariable { .. })
}

fn is_undefined_function_named(name: &str) -> impl Fn(&AstErrorKind) -> bool + '_ {
    move |kind| matches!(kind, AstErrorKind::UndefinedFunction { name: got } if got.as_ref() == name)
}

#[test]
fn bare_undefined_variable_reports_exactly_one_fault() {
    let ast = resolve_source("main() {\n    b := a\n}\n");
    assert_eq!(
        fault_count_matching(&ast, is_undefined_variable_named("a")),
        1
    );
}

#[test]
fn defined_variable_reports_no_undefined_fault() {
    let ast = resolve_source("main() {\n    a := 1\n    b := a\n}\n");
    assert_eq!(fault_count_matching(&ast, is_undefined_variable), 0);
}

#[test]
fn calling_an_undefined_function_reports_exactly_one_fault() {
    let ast = resolve_source("main() {\n    thisFunctionDoesNotExist()\n}\n");
    assert_eq!(
        fault_count_matching(
            &ast,
            is_undefined_function_named("thisFunctionDoesNotExist")
        ),
        1
    );
}

#[test]
fn calling_a_defined_function_reports_no_fault() {
    let ast = resolve_source("foo() {}\nmain() {\n    foo()\n}\n");
    assert_eq!(ast.faults().iter().count(), 0);
}
