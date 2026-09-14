use ast_model::fault::AstErrorKind;

use crate::resolve::tests::{fault_count_matching, resolve_source};

fn is_type_mismatch(kind: &AstErrorKind) -> bool {
    matches!(kind, AstErrorKind::BinaryExpressionTypeMismatch { .. })
}

#[test]
fn variable_from_a_function_call_can_be_used_in_a_binary_expression() {
    let ast =
        resolve_source("foo(): i64 {\n    1\n}\nmain() {\n    a := foo()\n    b := a + 1\n}\n");
    assert_eq!(fault_count_matching(&ast, is_type_mismatch), 0);
}

#[test]
fn mismatched_use_of_a_function_call_initialized_variable_reports_a_fault() {
    let ast = resolve_source(
        "foo(): i64 {\n    1\n}\nmain() {\n    a := foo()\n    b := a + \"hi\"\n}\n",
    );
    assert_eq!(fault_count_matching(&ast, is_type_mismatch), 1);
}

#[test]
fn variable_from_a_binary_expression_can_be_used_in_another_binary_expression() {
    let ast = resolve_source("main() {\n    a := 1 + 2\n    b := a + 3\n}\n");
    assert_eq!(fault_count_matching(&ast, is_type_mismatch), 0);
}

#[test]
fn variable_from_a_function_call_can_be_used_as_a_method_call_receiver() {
    let ast = resolve_source(
        "struct Counter {\n    n: i64\n}\nuse Counter {\n    get(&this): i64 => this.n\n}\nmakeCounter(): Counter => Counter{n: 5}\nmain() {\n    c := makeCounter()\n    x := c.get()\n}\n",
    );
    assert_eq!(ast.faults().iter().count(), 0);
}

#[test]
fn variable_from_a_struct_constructor_can_have_its_field_used_in_a_binary_expression() {
    let ast = resolve_source(
        "struct Point {\n    x: i64\n}\nmain() {\n    p := Point{x: 1}\n    b := p.x + 1\n}\n",
    );
    assert_eq!(fault_count_matching(&ast, is_type_mismatch), 0);
}

#[test]
fn variable_from_an_array_literal_can_have_an_indexed_element_used_in_a_binary_expression() {
    let ast = resolve_source("main() {\n    a := [1, 2, 3]\n    b := a[0] + 1\n}\n");
    assert_eq!(fault_count_matching(&ast, is_type_mismatch), 0);
}

#[test]
fn mismatched_use_of_an_indexed_array_element_reports_a_fault() {
    let ast = resolve_source("main() {\n    a := [1, 2, 3]\n    b := a[0] + \"hi\"\n}\n");
    assert_eq!(fault_count_matching(&ast, is_type_mismatch), 1);
}

#[test]
fn variable_from_a_not_expression_can_be_used_in_a_logical_expression() {
    let ast = resolve_source("main() {\n    a := !true\n    b := a && false\n}\n");
    assert_eq!(ast.faults().iter().count(), 0);
}

#[test]
fn variable_from_referencing_an_array_can_be_indexed() {
    let ast = resolve_source("main() {\n    a := [1, 2, 3]\n    s := &a\n    b := s[0] + 1\n}\n");
    assert_eq!(fault_count_matching(&ast, is_type_mismatch), 0);
}

#[test]
fn variable_from_a_typeof_null_check_can_be_used_in_a_logical_expression() {
    // `expr typeof null`/`expr typeof !null` (`TypeofKind::Null`/`NotNull`)
    // are boolean checks, unlike `expr.typeof` (`TypeofKind::Value`, which
    // reflects an actual type) — only the latter should type as
    // `SoulType::Type`.
    let ast = resolve_source("main() {\n    a := 1 typeof null\n    b := a && true\n}\n");
    assert_eq!(fault_count_matching(&ast, is_type_mismatch), 0);
}
