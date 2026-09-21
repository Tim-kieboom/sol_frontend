use ast_model::fault::AstErrorKind;

use crate::resolve::tests::{fault_count_matching, resolve_source};

fn is_undefined_variable(kind: &AstErrorKind) -> bool {
    matches!(kind, AstErrorKind::UndefinedVariable { .. })
}

fn is_argument_type_mismatch(kind: &AstErrorKind) -> bool {
    matches!(kind, AstErrorKind::ArgumentTypeMismatch { .. })
}

fn is_generic_parameter_conflict(kind: &AstErrorKind) -> bool {
    matches!(kind, AstErrorKind::GenericParameterConflict { .. })
}

#[test]
fn unresolved_variable_inside_call_argument_now_reports_a_fault() {
    let ast = resolve_source("foo(a: i64) {}\nmain() {\n    foo(x)\n}\n");
    assert_eq!(fault_count_matching(&ast, is_undefined_variable), 1);
}

#[test]
fn matching_positional_argument_type_reports_no_fault() {
    let ast = resolve_source("foo(a: i64) {}\nmain() {\n    foo(1)\n}\n");
    assert_eq!(fault_count_matching(&ast, is_argument_type_mismatch), 0);
}

#[test]
fn mismatched_positional_argument_type_reports_exactly_one_fault() {
    let ast = resolve_source("foo(a: i64) {}\nmain() {\n    foo(\"hi\")\n}\n");
    assert_eq!(fault_count_matching(&ast, is_argument_type_mismatch), 1);
}

#[test]
fn named_argument_call_is_skipped_without_fault() {
    let ast = resolve_source("foo(a: i64) {}\nmain() {\n    foo(a: \"hi\")\n}\n");
    assert_eq!(fault_count_matching(&ast, is_argument_type_mismatch), 0);
}

#[test]
fn arity_mismatched_call_is_skipped_without_type_fault() {
    let ast = resolve_source("foo(a: i64, b: i64) {}\nmain() {\n    foo(1)\n}\n");
    assert_eq!(fault_count_matching(&ast, is_argument_type_mismatch), 0);
}

#[test]
fn same_generic_used_with_incompatible_argument_types_reports_exactly_one_fault() {
    let ast = resolve_source("assertEq<T>(a: T, b: T) {}\nmain() {\n    assertEq(1, \"\")\n}\n");
    assert_eq!(fault_count_matching(&ast, is_generic_parameter_conflict), 1);
}

#[test]
fn same_generic_used_with_matching_argument_types_reports_no_fault() {
    let ast = resolve_source("assertEq<T>(a: T, b: T) {}\nmain() {\n    assertEq(1, 2)\n}\n");
    assert_eq!(fault_count_matching(&ast, is_generic_parameter_conflict), 0);
}

#[test]
fn generic_used_once_is_skipped_without_fault() {
    let ast = resolve_source("identity<T>(a: T): T => a\nmain() {\n    identity(1)\n}\n");
    assert_eq!(fault_count_matching(&ast, is_generic_parameter_conflict), 0);
}

/// Regression test for the `Stub`-occurrence-identity change: `Point` is
/// written three separate times here (the struct's own field type is
/// irrelevant; what matters is the parameter annotation, the variable's
/// declared type, and the struct-constructor's target type below) — three
/// independent syntactic occurrences of the same name, each interning to
/// its own distinct `TypeId` now that `Stub` carries a per-occurrence
/// identity. Argument-type checking must still recognize them as the same
/// type via `DeclareStore::sol_types_equal_ignoring_occurrence`, not raw
/// `TypeId`/`SolType` equality.
#[test]
fn struct_argument_matches_parameter_across_independently_parsed_occurrences() {
    let ast = resolve_source(
        "struct Point { x: i64 }\nconsume(p: Point) {}\nmain() {\n    a: Point = Point{x: 1}\n    consume(a)\n}\n",
    );
    assert_eq!(fault_count_matching(&ast, is_argument_type_mismatch), 0);
}

#[test]
fn non_distinct_type_alias_is_interchangeable_with_its_underlying_type() {
    let ast = resolve_source(
        "type Byte := u8\nassertEq<T>(a: T, b: T) {}\nmain() {\n    b: Byte := 200\n    assertEq(b, 200_u8)\n}\n",
    );
    assert_eq!(fault_count_matching(&ast, is_generic_parameter_conflict), 0);
}

#[test]
fn distinct_type_alias_is_not_interchangeable_with_its_underlying_type() {
    let ast = resolve_source(
        "type Meters := distinct f64\nassertEq<T>(a: T, b: T) {}\nmain() {\n    m: Meters := 1.0\n    assertEq(m, 1.0_f64)\n}\n",
    );
    assert_eq!(fault_count_matching(&ast, is_generic_parameter_conflict), 1);
}
