use ast_model::fault::AstErrorKind;

use crate::resolve::tests::{fault_count_matching, resolve_source};



fn is_type_mismatch(kind: &AstErrorKind) -> bool {
    matches!(kind, AstErrorKind::BinaryExpressionTypeMismatch { .. })
}

#[test]
fn field_access_value_can_be_used_in_a_binary_expression() {
    let ast = resolve_source(
        "struct Point {\n    x: i64\n}\nmain() {\n    p: Point = Point{x: 5}\n    y := p.x + 1\n}\n",
    );
    assert_eq!(fault_count_matching(&ast, is_type_mismatch), 0);
}

#[test]
fn mismatched_field_access_value_in_a_binary_expression_reports_a_fault() {
    let ast = resolve_source(
        "struct Point {\n    x: i64\n}\nmain() {\n    p: Point = Point{x: 5}\n    y := p.x + \"hi\"\n}\n",
    );
    assert_eq!(fault_count_matching(&ast, is_type_mismatch), 1);
}

#[test]
fn method_call_on_a_nested_field_access_resolves() {
    let ast = resolve_source(
        "struct Inner {\n    n: i64\n}\nuse Inner {\n    get(&this): i64 => this.n\n}\nstruct Outer {\n    inner: Inner\n}\nmain() {\n    o: Outer = Outer{inner: Inner{n: 5}}\n    x := o.inner.get()\n}\n",
    );
    assert_eq!(ast.faults().iter().count(), 0);
}

#[test]
fn field_access_on_an_undeclared_type_is_skipped_without_fault() {
    let ast = resolve_source("main() {\n    y := notAThing.field\n}\n");
    assert_eq!(fault_count_matching(&ast, is_type_mismatch), 0);
}
