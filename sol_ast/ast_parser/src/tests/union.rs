use ast_model::{EnumVariant, SolType, StatementKind, UnionKind};
use sol_utils::{fault::Severity, sol_names::PrimitiveTypes};

use crate::tests::{get_statement, parse, parse_with_declares};

#[test]
fn union_mixed_variants() {
    let (module, store, context, declares) = parse_with_declares(
        r#"
union Literal {
    None,
    Int(int),
    Str{tag: str, value: str},
}
"#,
    );
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    let union_ = match &stmt.node {
        StatementKind::Union(u) => u,
        _ => panic!("expected Union, got {:?}", stmt.node.variant_name()),
    };
    assert_eq!(union_.name.as_str(), "Literal");
    assert!(union_.impl_type.is_none());
    assert_eq!(union_.variants.len(), 3);

    match &union_.variants[0] {
        EnumVariant::Normal(name) => assert_eq!(name.as_str(), "None"),
        _ => panic!("expected Normal variant for None"),
    }

    match &union_.variants[1] {
        EnumVariant::Union(UnionKind::Tuple { name, parameters }) => {
            assert_eq!(name.as_str(), "Int");
            assert_eq!(parameters.len(), 1);
            assert_eq!(
                declares.get_type(parameters[0]),
                Some(&SolType::Primitive(PrimitiveTypes::Int))
            );
        }
        other => panic!("expected Tuple union variant for Int, got {:?}", other),
    }

    match &union_.variants[2] {
        EnumVariant::Union(UnionKind::NamedTuple { name, parameters }) => {
            assert_eq!(name.as_str(), "Str");
            assert_eq!(parameters.len(), 2);
            assert_eq!(parameters[0].0.as_str(), "tag");
            assert_eq!(declares.get_type(parameters[0].1), Some(&SolType::String));
            assert_eq!(parameters[1].0.as_str(), "value");
            assert_eq!(declares.get_type(parameters[1].1), Some(&SolType::String));
        }
        other => panic!("expected NamedTuple union variant for Str, got {:?}", other),
    }
}

// ----------------------------------------------------------------
//  Bad-path: malformed union declarations
// ----------------------------------------------------------------
#[test]
fn union_missing_name_is_rejected() {
    let (_, _, context) = parse("union {}");
    assert!(
        context.faults.count_severity(Severity::Error) > 0,
        "expected an error when a union declaration has no name"
    );
}

#[test]
fn union_variants_missing_comma_is_rejected() {
    let (_, _, context) = parse("union Foo { A B }");
    assert!(
        context.faults.count_severity(Severity::Error) > 0,
        "expected an error when variants aren't comma-separated"
    );
}

#[test]
fn union_tuple_variant_unclosed_paren_is_rejected() {
    let (_, _, context) = parse("union Foo { Bar(int }");
    assert!(
        context.faults.count_severity(Severity::Error) > 0,
        "expected an error on an unclosed tuple-union parameter list"
    );
}

#[test]
fn union_empty() {
    let (module, store, context) = parse("union Foo {}");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    let union_ = match &stmt.node {
        StatementKind::Union(u) => u,
        _ => panic!("expected Union"),
    };
    assert_eq!(union_.name.as_str(), "Foo");
    assert!(union_.variants.is_empty());
    assert!(union_.impl_type.is_none());
}
