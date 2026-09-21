use ast_model::{AstTree, SolType, StatementKind, VarPattern};
use sol_utils::sol_names::PrimitiveTypes;

use crate::collect::tests::resolve_source;

/// Finds the inferred/declared type of the first `let`/`mut` binding named `name`.
fn variable_type(ast: &AstTree, name: &str) -> Option<SolType> {
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
            .get_variable_type(binding.id)
            .and_then(|(_, ty, _)| ty.clone())
    })
}

#[test]
fn bare_uint_literal_binding_infers_int() {
    let ast = resolve_source("main() {\n    a := 5\n}\n");
    assert_eq!(
        variable_type(&ast, "a"),
        Some(SolType::Primitive(PrimitiveTypes::Int))
    );
}

#[test]
fn negative_int_literal_binding_infers_int() {
    let ast = resolve_source("main() {\n    a := -5\n}\n");
    assert_eq!(
        variable_type(&ast, "a"),
        Some(SolType::Primitive(PrimitiveTypes::Int))
    );
}

#[test]
fn bool_literal_binding_infers_bool() {
    let ast = resolve_source("main() {\n    a := true\n}\n");
    assert_eq!(
        variable_type(&ast, "a"),
        Some(SolType::Primitive(PrimitiveTypes::Boolean))
    );
}

#[test]
fn str_literal_binding_infers_string() {
    let ast = resolve_source("main() {\n    a := \"hi\"\n}\n");
    assert_eq!(variable_type(&ast, "a"), Some(SolType::String));
}

#[test]
fn explicit_annotation_is_not_overridden_by_literal_inference() {
    let ast = resolve_source("main() {\n    a: i64 = 5\n}\n");
    assert_eq!(
        variable_type(&ast, "a"),
        Some(SolType::Primitive(PrimitiveTypes::Int64))
    );
}

#[test]
fn non_literal_initializer_is_backfilled_once_resolve_runs() {
    let ast = resolve_source("main() {\n    b := 1\n    a := b\n}\n");
    assert_eq!(
        variable_type(&ast, "a"),
        Some(SolType::Primitive(PrimitiveTypes::Int))
    );
}
