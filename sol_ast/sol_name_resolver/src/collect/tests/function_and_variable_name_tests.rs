use crate::collect::tests::{fault_count_matching, resolve_source};
use ast_parser::fault::AstErrorKind;

#[test]
fn a_nested_function_sharing_its_enclosing_functions_name_reports_exactly_one_fault() {
    let ast = resolve_source("foo() {\n    foo() {}\n}\n");
    assert_eq!(
        fault_count_matching(&ast, |kind| matches!(
            kind,
            AstErrorKind::ParentChildFunctionSameName
        )),
        1,
        "{:#?}",
        ast.faults()
    );
}

#[test]
fn a_nested_function_with_a_distinct_name_reports_no_fault() {
    let ast = resolve_source("foo() {\n    bar() {}\n}\n");
    assert_eq!(
        fault_count_matching(&ast, |kind| matches!(
            kind,
            AstErrorKind::ParentChildFunctionSameName
        )),
        0,
        "{:#?}",
        ast.faults()
    );
}

#[test]
fn a_function_name_with_a_triple_underscore_reports_exactly_one_fault() {
    let ast = resolve_source("foo___bar() {}\n");
    assert_eq!(
        fault_count_matching(&ast, |kind| matches!(
            kind,
            AstErrorKind::FunctionNameTripleUnderscore
        )),
        1,
        "{:#?}",
        ast.faults()
    );
}

#[test]
fn a_function_name_with_a_double_underscore_reports_no_fault() {
    let ast = resolve_source("foo__bar() {}\n");
    assert_eq!(
        fault_count_matching(&ast, |kind| matches!(
            kind,
            AstErrorKind::FunctionNameTripleUnderscore
        )),
        0,
        "{:#?}",
        ast.faults()
    );
}
