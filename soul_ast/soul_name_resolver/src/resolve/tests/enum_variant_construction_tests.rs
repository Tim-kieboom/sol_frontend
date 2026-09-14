use ast_parser::{fault::AstErrorKind};

use crate::resolve::tests::{fault_count_matching, resolve_source};


fn is_enum_variant_fault(kind: &AstErrorKind) -> bool {
    matches!(
        kind,
        AstErrorKind::EnumVariantArityMismatch { .. }
            | AstErrorKind::EnumVariantArgumentTypeMismatch { .. }
    )
}

#[test]
fn correct_arity_and_type_reports_no_fault() {
    let ast = resolve_source(
        "union Literal {\n    None,\n    Int(int)\n}\nmain() {\n    x := Literal.Int(1)\n}\n",
    );
    assert_eq!(
        fault_count_matching(&ast, is_enum_variant_fault),
        0,
        "{:#?}",
        ast.faults()
    );
}

#[test]
fn wrong_arity_reports_exactly_one_fault() {
    let ast = resolve_source(
        "union Literal {\n    None,\n    Int(int)\n}\nmain() {\n    x := Literal.Int(1, 2)\n}\n",
    );
    assert_eq!(
        fault_count_matching(&ast, |kind| matches!(
            kind,
            AstErrorKind::EnumVariantArityMismatch {
                expected: 1,
                got: 2,
                ..
            }
        )),
        1,
        "{:#?}",
        ast.faults()
    );
}

#[test]
fn wrong_argument_type_reports_exactly_one_fault() {
    let ast = resolve_source(
        "union Literal {\n    None,\n    Int(int)\n}\nmain() {\n    x := Literal.Int(\"hi\")\n}\n",
    );
    assert_eq!(
        fault_count_matching(&ast, |kind| matches!(
            kind,
            AstErrorKind::EnumVariantArgumentTypeMismatch { .. }
        )),
        1,
        "{:#?}",
        ast.faults()
    );
}

#[test]
fn unrelated_call_on_undeclared_type_is_left_unresolved_without_fault() {
    let ast = resolve_source("main() {\n    x := NotAType.Whatever(1)\n}\n");
    assert_eq!(
        fault_count_matching(&ast, is_enum_variant_fault),
        0,
        "{:#?}",
        ast.faults()
    );
}

#[test]
fn method_call_on_a_variable_is_not_treated_as_variant_construction() {
    let ast = resolve_source(
        "union Literal {\n    None,\n    Int(int)\n}\nuse Literal {\n    Int(&this): int => 1\n}\nmain() {\n    x := Literal.None\n    y := x.Int()\n}\n",
    );
    assert_eq!(
        fault_count_matching(&ast, is_enum_variant_fault),
        0,
        "{:#?}",
        ast.faults()
    );
}
