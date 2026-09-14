use ast_model::fault::AstErrorKind;

use crate::resolve::tests::{fault_count_matching, resolve_source};

fn is_type_mismatch(kind: &AstErrorKind) -> bool {
    matches!(kind, AstErrorKind::BinaryExpressionTypeMismatch { .. })
}

#[test]
fn foreach_element_from_array_literal_can_be_used_in_binary_expression() {
    let ast = resolve_source("main() {\n    for x in [1, 2, 3] {\n        y := x + 1\n    }\n}\n");
    assert_eq!(
        fault_count_matching(&ast, is_type_mismatch),
        0,
        "{:#?}",
        ast.faults()
    );
}

#[test]
fn foreach_element_type_mismatch_in_body_reports_a_fault() {
    let ast =
        resolve_source("main() {\n    for x in [1, 2, 3] {\n        y := x + \"hi\"\n    }\n}\n");
    assert_eq!(
        fault_count_matching(&ast, is_type_mismatch),
        1,
        "{:#?}",
        ast.faults()
    );
}

#[test]
fn foreach_element_from_typed_variable_collection_is_usable() {
    let ast = resolve_source(
        "main() {\n    xs: []int = [1, 2, 3]\n    for x in xs {\n        y := x + 1\n    }\n}\n",
    );
    assert_eq!(
        fault_count_matching(&ast, is_type_mismatch),
        0,
        "{:#?}",
        ast.faults()
    );
}

#[test]
fn foreach_over_undeclared_collection_is_skipped_without_fault() {
    let ast =
        resolve_source("main() {\n    for x in notAThing {\n        y := x + \"hi\"\n    }\n}\n");
    assert_eq!(
        fault_count_matching(&ast, is_type_mismatch),
        0,
        "{:#?}",
        ast.faults()
    );
}
