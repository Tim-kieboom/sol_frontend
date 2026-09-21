use ast_model::fault::AstErrorKind;

use crate::resolve::tests::{fault_count_matching, resolve_source};

fn is_variable_used_before_declaration(kind: &AstErrorKind) -> bool {
    matches!(kind, AstErrorKind::VariableUsedBeforeDeclaration { .. })
}

#[test]
fn variable_used_before_its_declaration_reports_exactly_one_fault() {
    // The exact reported repro.
    let ast = resolve_source(
        "lambda() {\n    num := fn()\n    fn := () => return 2\n    assertEq(fn(), \"\")\n    assertEq(fn(), 2)\n}\n",
    );
    assert_eq!(
        fault_count_matching(&ast, is_variable_used_before_declaration),
        1
    );
}

#[test]
fn variable_used_after_its_declaration_reports_no_fault() {
    let ast = resolve_source("main() {\n    a: i64 = 1\n    b := a\n}\n");
    assert_eq!(
        fault_count_matching(&ast, is_variable_used_before_declaration),
        0
    );
}

#[test]
fn variable_used_before_declaration_inside_an_if_branch_still_faults() {
    let ast = resolve_source("main() {\n    if true {\n        b := a\n    }\n    a: i64 = 1\n}\n");
    assert_eq!(
        fault_count_matching(&ast, is_variable_used_before_declaration),
        1
    );
}

#[test]
fn function_parameter_used_in_body_reports_no_fault() {
    let ast = resolve_source("foo(a: i64) {\n    b := a\n}\n");
    assert_eq!(
        fault_count_matching(&ast, is_variable_used_before_declaration),
        0
    );
}

#[test]
fn array_constructor_operator_overload_parameter_reports_no_fault() {
    // Regression test: `This.[Type](param) => body` used to allocate the
    // parameter's NodeId *after* parsing `body`, so every use of `param`
    // inside `body` looked like it came before the parameter's own
    // declaration.
    let ast = resolve_source(
        "struct IntArray {\n    array: []int\n    len: uint\n\n    This.[int](array) => This{ len: array.len(), array }\n}\n",
    );
    assert_eq!(
        fault_count_matching(&ast, is_variable_used_before_declaration),
        0
    );
}

#[test]
fn implicit_this_receiver_reports_no_fault() {
    let ast = resolve_source(
        "struct Point {\n    x: i64\n}\nuse Point {\n    getX(): i64 {\n        this.x\n    }\n}\n",
    );
    assert_eq!(
        fault_count_matching(&ast, is_variable_used_before_declaration),
        0
    );
}

#[test]
fn forward_reference_between_functions_reports_no_fault() {
    let ast = resolve_source("foo() {\n    bar()\n}\nbar() {}\n");
    assert_eq!(
        fault_count_matching(&ast, is_variable_used_before_declaration),
        0
    );
}
