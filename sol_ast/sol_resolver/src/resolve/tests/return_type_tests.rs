use ast_model::fault::AstErrorKind;

use crate::resolve::tests::{fault_count_matching, resolve_source};

fn is_return_type_mismatch(kind: &AstErrorKind) -> bool {
    matches!(
        kind,
        AstErrorKind::ReturnTypeMismatch { .. } | AstErrorKind::ReturnTypeMismatchMissing { .. }
    )
}

#[test]
fn matching_tail_expression_reports_no_fault() {
    let ast = resolve_source("foo(): i64 {\n    1\n}\n");
    assert_eq!(fault_count_matching(&ast, is_return_type_mismatch), 0);
}

#[test]
fn mismatched_tail_expression_reports_exactly_one_fault() {
    let ast = resolve_source("foo(): str {\n    1\n}\n");
    assert_eq!(fault_count_matching(&ast, is_return_type_mismatch), 1);
}

#[test]
fn semicolon_terminated_tail_is_not_a_return_value() {
    let ast = resolve_source("foo(): i64 {\n    1;\n}\n");
    assert_eq!(fault_count_matching(&ast, is_return_type_mismatch), 0);
}

#[test]
fn void_function_with_stray_tail_expression_reports_a_fault() {
    let ast = resolve_source("foo() {\n    a: i64 = 1\n    a\n}\n");
    assert_eq!(fault_count_matching(&ast, is_return_type_mismatch), 1);
}

#[test]
fn empty_body_with_non_void_return_type_is_skipped() {
    let ast = resolve_source("foo(): i64 {}\n");
    assert_eq!(fault_count_matching(&ast, is_return_type_mismatch), 0);
}

#[test]
fn exhaustive_if_tail_checks_every_branch() {
    let ast = resolve_source(
        "foo(): i64 {\n    a: bool = true\n    if a {\n        1\n    } else {\n        \"hi\"\n    }\n}\n",
    );
    assert_eq!(fault_count_matching(&ast, is_return_type_mismatch), 1);
}

#[test]
fn exhaustive_if_tail_with_matching_branches_reports_no_fault() {
    let ast = resolve_source(
        "foo(): i64 {\n    a: bool = true\n    if a {\n        1\n    } else {\n        2\n    }\n}\n",
    );
    assert_eq!(fault_count_matching(&ast, is_return_type_mismatch), 0);
}

#[test]
fn non_exhaustive_if_tail_is_skipped() {
    let ast =
        resolve_source("foo(): i64 {\n    a: bool = true\n    if a {\n        \"hi\"\n    }\n}\n");
    assert_eq!(fault_count_matching(&ast, is_return_type_mismatch), 0);
}

#[test]
fn explicit_return_with_matching_type_reports_no_fault() {
    let ast = resolve_source("foo(): i64 {\n    if true {\n        return 1\n    }\n    2\n}\n");
    assert_eq!(fault_count_matching(&ast, is_return_type_mismatch), 0);
}

#[test]
fn explicit_return_with_mismatched_type_reports_exactly_one_fault() {
    let ast =
        resolve_source("foo(): i64 {\n    if true {\n        return \"hi\"\n    }\n    2\n}\n");
    assert_eq!(fault_count_matching(&ast, is_return_type_mismatch), 1);
}

#[test]
fn bare_return_in_void_function_reports_no_fault() {
    let ast = resolve_source("foo() {\n    if true {\n        return\n    }\n}\n");
    assert_eq!(fault_count_matching(&ast, is_return_type_mismatch), 0);
}

#[test]
fn bare_return_in_non_void_function_reports_exactly_one_fault() {
    let ast = resolve_source("foo(): i64 {\n    if true {\n        return\n    }\n    1\n}\n");
    assert_eq!(fault_count_matching(&ast, is_return_type_mismatch), 1);
}

#[test]
fn return_inside_lambda_is_not_checked_against_enclosing_function() {
    let ast =
        resolve_source("foo(): i64 {\n    f := x => {\n        return \"hi\"\n    }\n    1\n}\n");
    assert_eq!(fault_count_matching(&ast, is_return_type_mismatch), 0);
}
