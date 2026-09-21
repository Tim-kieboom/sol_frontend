use ast_parser::fault::AstErrorKind;

use crate::resolve::tests::{fault_count_matching, resolve_source};

fn is_assignment_type_mismatch(kind: &AstErrorKind) -> bool {
    matches!(kind, AstErrorKind::AssignmentTypeMismatch { .. })
}

fn is_assign_to_immutable(kind: &AstErrorKind) -> bool {
    matches!(kind, AstErrorKind::AssignToImmutableVariable)
}

#[test]
fn matching_assignment_reports_no_fault() {
    let ast = resolve_source("main() {\n    mut a: i64 = 1\n    a = 2\n}\n");
    assert_eq!(fault_count_matching(&ast, is_assignment_type_mismatch), 0);
}

#[test]
fn mismatched_assignment_reports_exactly_one_fault() {
    let ast = resolve_source("main() {\n    mut a: i64 = 1\n    a = \"hi\"\n}\n");
    assert_eq!(fault_count_matching(&ast, is_assignment_type_mismatch), 1);
}

#[test]
fn compound_assignment_reuses_binary_type_checking() {
    let ast = resolve_source("main() {\n    mut a: i64 = 1\n    b: str = \"hi\"\n    a += b\n}\n");
    assert_eq!(
        fault_count_matching(&ast, |kind| matches!(
            kind,
            AstErrorKind::BinaryExpressionTypeMismatch { .. }
        )),
        1
    );
}

#[test]
fn matching_compound_assignment_reports_no_fault() {
    let ast = resolve_source("main() {\n    mut a: i64 = 1\n    a += 2\n}\n");
    assert_eq!(
        fault_count_matching(&ast, |kind| matches!(
            kind,
            AstErrorKind::BinaryExpressionTypeMismatch { .. }
        )),
        0
    );
}

#[test]
fn assigning_to_immutable_variable_reports_exactly_one_fault() {
    let ast = resolve_source("main() {\n    a: i64 = 1\n    a = 2\n}\n");
    assert_eq!(fault_count_matching(&ast, is_assign_to_immutable), 1);
}

#[test]
fn assigning_to_mutable_variable_reports_no_mutability_fault() {
    let ast = resolve_source("main() {\n    mut a: i64 = 1\n    a = 2\n}\n");
    assert_eq!(fault_count_matching(&ast, is_assign_to_immutable), 0);
}

#[test]
fn assigning_to_immutable_parameter_reports_exactly_one_fault() {
    let ast = resolve_source("foo(a: i64) {\n    a = 2\n}\n");
    assert_eq!(fault_count_matching(&ast, is_assign_to_immutable), 1);
}

#[test]
fn assigning_mismatched_type_to_mutable_parameter_reports_exactly_one_fault() {
    let ast = resolve_source("foo(mut a: i64) {\n    a = \"hi\"\n}\n");
    assert_eq!(fault_count_matching(&ast, is_assignment_type_mismatch), 1);
}

#[test]
fn assigning_matching_type_to_mutable_parameter_reports_no_fault() {
    let ast = resolve_source("foo(mut a: i64) {\n    a = 2\n}\n");
    assert_eq!(fault_count_matching(&ast, is_assignment_type_mismatch), 0);
    assert_eq!(fault_count_matching(&ast, is_assign_to_immutable), 0);
}

#[test]
fn assignment_to_generic_parameter_is_skipped_without_fault() {
    let ast = resolve_source(
        "swap<T>(mut a: T, b: T) {\n    a = b\n}\nmain() {\n    x: i64 = 1\n    y: i64 = 2\n    swap(x, y)\n}\n",
    );
    assert_eq!(fault_count_matching(&ast, is_assignment_type_mismatch), 0);
}

#[test]
fn matching_explicit_declaration_type_reports_no_fault() {
    let ast = resolve_source("main() {\n    a: bool = true\n}\n");
    assert_eq!(fault_count_matching(&ast, is_assignment_type_mismatch), 0);
}

#[test]
fn mismatched_explicit_declaration_type_reports_exactly_one_fault() {
    // An explicit annotation (`a: bool = 1`) is never inferred — unlike a
    // bare `a := 1`, nothing previously checked it against its initializer
    // at all, so `value: bool = 1` silently compiled.
    let ast = resolve_source("main() {\n    a: bool = 1\n}\n");
    assert_eq!(fault_count_matching(&ast, is_assignment_type_mismatch), 1);
}

#[test]
fn explicit_declaration_type_for_a_generic_parameter_is_skipped_without_fault() {
    let ast = resolve_source("box<T>(value: T): T {\n    a: T = value\n    return a\n}\n");
    assert_eq!(fault_count_matching(&ast, is_assignment_type_mismatch), 0);
}

#[test]
fn untyped_array_literal_declaration_coerces_its_element_type() {
    // `[1, 2]`'s inferred element type is `UntypedInt` (not yet defaulted to
    // `int`) until it's combined against the declared `i32` — same coercion
    // a bare `a: i32 = 1` already gets.
    let ast = resolve_source("main() {\n    a: [2]i32 = [1, 2]\n}\n");
    assert_eq!(fault_count_matching(&ast, is_assignment_type_mismatch), 0);
}

#[test]
fn a_plain_reference_satisfies_a_mut_slice_declaration() {
    // `&a` (no `mut` keyword) parses as a const reference, but slice
    // mutability isn't checked anywhere else in this compiler yet (M2
    // borrow checker's job) — `[&]T`/`[&mut]T` must combine freely.
    let ast = resolve_source("main() {\n    a: [2]i32 = [1, 2]\n    mut s: [&mut]i32 = &a\n}\n");
    assert_eq!(fault_count_matching(&ast, is_assignment_type_mismatch), 0);
}

#[test]
fn mismatched_array_element_type_reports_exactly_one_fault() {
    let ast = resolve_source("main() {\n    a: [2]bool = [1, 2]\n}\n");
    assert_eq!(fault_count_matching(&ast, is_assignment_type_mismatch), 1);
}
