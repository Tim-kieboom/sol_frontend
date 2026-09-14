
use ast_model::{AstTree, SoulType, StatementKind, VarPattern};
use ast_parser::{fault::AstErrorKind};
use soul_utils::{
    soul_names::PrimitiveTypes,
};

use crate::{resolve::tests::resolve_source};

fn expression_type_of_binding(ast: &AstTree, name: &str) -> Option<SoulType> {
    ast.crates.store.statements.values().find_map(|statement| {
        let StatementKind::Variable(variable) = &statement.node else {
            return None;
        };
        let VarPattern::Simple { binding, .. } = &variable.pattern else {
            return None;
        };
        if binding.ident.as_str() != name {
            return None;
        }
        ast.declares
            .get_expression_type(variable.initialize_value?)
            .cloned()
    })
}

fn type_mismatch_fault_count(ast: &AstTree) -> usize {
    ast.faults()
        .iter()
        .filter(|fault| {
            matches!(
                fault.kind(),
                AstErrorKind::BinaryExpressionTypeMismatch { .. }
            )
        })
        .count()
}

#[test]
fn same_type_arithmetic_binary_infers_operand_type() {
    let ast = resolve_source("main() {\n    a: i64 = 1\n    b: i64 = 2\n    c := a + b\n}\n");
    assert_eq!(
        expression_type_of_binding(&ast, "c"),
        Some(SoulType::Primitive(PrimitiveTypes::Int64))
    );
    assert_eq!(type_mismatch_fault_count(&ast), 0);
}

#[test]
fn same_type_comparison_binary_infers_bool() {
    let ast = resolve_source("main() {\n    a: i64 = 1\n    b: i64 = 2\n    c := a == b\n}\n");
    assert_eq!(
        expression_type_of_binding(&ast, "c"),
        Some(SoulType::Primitive(PrimitiveTypes::Boolean))
    );
}

#[test]
fn mismatched_operand_types_report_exactly_one_fault() {
    let ast = resolve_source("main() {\n    a: i64 = 1\n    b: u64 = 2\n    c := a + b\n}\n");
    assert_eq!(type_mismatch_fault_count(&ast), 1);
    assert_eq!(expression_type_of_binding(&ast, "c"), None);
}

#[test]
fn untyped_literal_operand_coerces_to_concrete_operand_type() {
    let ast = resolve_source("main() {\n    a: i64 = 1\n    c := a + 5\n}\n");
    assert_eq!(type_mismatch_fault_count(&ast), 0);
    assert_eq!(
        expression_type_of_binding(&ast, "c"),
        Some(SoulType::Primitive(PrimitiveTypes::Int64))
    );
}

#[test]
fn incompatible_untyped_literal_operand_reports_exactly_one_fault() {
    let ast = resolve_source("main() {\n    a: str = \"hi\"\n    c := a + 5\n}\n");
    assert_eq!(type_mismatch_fault_count(&ast), 1);
    assert_eq!(expression_type_of_binding(&ast, "c"), None);
}

#[test]
fn two_untyped_int_literals_combine_and_stay_untyped() {
    let ast = resolve_source("main() {\n    c := 5 + -3\n}\n");
    assert_eq!(type_mismatch_fault_count(&ast), 0);
    assert_eq!(
        expression_type_of_binding(&ast, "c"),
        Some(SoulType::Primitive(PrimitiveTypes::UntypedInt))
    );
}

#[test]
fn untyped_chain_with_float_stays_untyped_float() {
    let ast = resolve_source("main() {\n    c := 5 + -3 + 2.0\n}\n");
    assert_eq!(type_mismatch_fault_count(&ast), 0);
    assert_eq!(
        expression_type_of_binding(&ast, "c"),
        Some(SoulType::Primitive(PrimitiveTypes::UntypedFloat))
    );
}

#[test]
fn operand_from_function_call_uses_its_return_type() {
    let ast = resolve_source(
        "foo(): i64 {\n    return 1\n}\nmain() {\n    a: i64 = 1\n    c := a + foo()\n}\n",
    );
    assert_eq!(type_mismatch_fault_count(&ast), 0);
    assert_eq!(
        expression_type_of_binding(&ast, "c"),
        Some(SoulType::Primitive(PrimitiveTypes::Int64))
    );
}
