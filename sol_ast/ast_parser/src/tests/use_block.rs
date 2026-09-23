use ast_model::{FunctionKind, Import, ImportKind, SolType, StatementKind, Struct, Stub};
use sol_utils::{SharedStr, fault::Severity, sol_names::PrimitiveTypes};

use crate::tests::{get_statement, parse, parse_with_declares};

#[test]
fn use_block_empty() {
    let (module, store, context, declares) = parse_with_declares("use Foo {}");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.into_vec()
    );

    let stmt = get_statement(&store, &module, 0);
    let use_block = match &stmt.node {
        StatementKind::UseBlock(b) => b,
        _ => panic!("expected UseBlock"),
    };
    match declares.get_type(use_block.ty) {
        Some(SolType::Stub(stub)) => {
            assert!(stub.matches_ignoring_occurrence(&Stub::new("Foo")))
        }
        other => panic!("expected a Stub named Foo, got {other:?}"),
    }
    assert!(use_block.use_generics.is_empty());
    assert!(use_block.methods.is_empty());
    assert!(use_block.impls.is_empty());
    assert!(use_block.statements.is_empty());
}

#[test]
fn use_block_method() {
    let (module, store, context, declares) = parse_with_declares("use Foo { bar() {} }");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.into_vec()
    );

    let stmt = get_statement(&store, &module, 0);
    let use_block = match &stmt.node {
        StatementKind::UseBlock(b) => b,
        _ => panic!("expected UseBlock"),
    };
    match declares.get_type(use_block.ty) {
        Some(SolType::Stub(stub)) => {
            assert!(stub.matches_ignoring_occurrence(&Stub::new("Foo")))
        }
        other => panic!("expected a Stub named Foo, got {other:?}"),
    }
    assert_eq!(use_block.methods.len(), 1);
    assert!(!use_block.methods[0].is_public);
    let func = &store.functions[use_block.methods[0].id];
    let FunctionKind::Normal(f) = func else {
        panic!("expected Normal function");
    };
    assert_eq!(f.signature.value.name.as_str(), "bar");
    assert!(use_block.impls.is_empty());
    assert!(use_block.statements.is_empty());
}

#[test]
fn use_block_pub_method() {
    let (module, store, context) = parse("use Foo { pub bar() {} }");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.into_vec()
    );

    let stmt = get_statement(&store, &module, 0);
    let use_block = match &stmt.node {
        StatementKind::UseBlock(b) => b,
        _ => panic!("expected UseBlock"),
    };
    assert_eq!(use_block.methods.len(), 1);
    assert!(use_block.methods[0].is_public);
    let func = &store.functions[use_block.methods[0].id];
    let FunctionKind::Normal(f) = func else {
        panic!("expected Normal function");
    };
    assert_eq!(f.signature.value.name.as_str(), "bar");
}

#[test]
fn use_block_multiple_methods() {
    let (module, store, context) = parse("use Foo { bar() {}\nbaz() {} }");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.into_vec()
    );

    let stmt = get_statement(&store, &module, 0);
    let use_block = match &stmt.node {
        StatementKind::UseBlock(b) => b,
        _ => panic!("expected UseBlock"),
    };
    assert_eq!(use_block.methods.len(), 2);
    let func0 = &store.functions[use_block.methods[0].id];
    let FunctionKind::Normal(f0) = func0 else {
        panic!("expected Normal function");
    };
    assert_eq!(f0.signature.value.name.as_str(), "bar");
    let func1 = &store.functions[use_block.methods[1].id];
    let FunctionKind::Normal(f1) = func1 else {
        panic!("expected Normal function");
    };
    assert_eq!(f1.signature.value.name.as_str(), "baz");
}

#[test]
fn use_block_impl_block() {
    let (module, store, context, declares) =
        parse_with_declares("use Foo { impl Bar { baz() {} } }");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.into_vec()
    );

    let stmt = get_statement(&store, &module, 0);
    let use_block = match &stmt.node {
        StatementKind::UseBlock(b) => b,
        _ => panic!("expected UseBlock"),
    };
    assert_eq!(use_block.impls.len(), 1);
    match declares.get_type(use_block.impls[0].impl_trait) {
        Some(SolType::Stub(stub)) => {
            assert!(stub.matches_ignoring_occurrence(&Stub::new("Bar")))
        }
        other => panic!("expected a Stub named Bar, got {other:?}"),
    }
    assert_eq!(use_block.impls[0].methods.len(), 1);
    let func = &store.functions[use_block.impls[0].methods[0]];
    let FunctionKind::Normal(f) = func else {
        panic!("expected Normal function");
    };
    assert_eq!(f.signature.value.name.as_str(), "baz");
    assert!(use_block.methods.is_empty());
    assert!(use_block.statements.is_empty());
}

#[test]
fn use_block_with_generic_type() {
    let (module, store, context, mut declares) = parse_with_declares("use Foo<int> { bar() {} }");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.into_vec()
    );

    let stmt = get_statement(&store, &module, 0);
    let use_block = match &stmt.node {
        StatementKind::UseBlock(b) => b,
        _ => panic!("expected UseBlock"),
    };
    let int_id = declares.intern_type(SolType::Primitive(PrimitiveTypes::Int));
    match declares.get_type(use_block.ty) {
        Some(SolType::Stub(stub)) => assert!(stub.matches_ignoring_occurrence(&Stub {
            name: SharedStr::new("Foo"),
            generics: vec![int_id].into(),
            occurrence: ast_model::NodeId::ERROR,
        })),
        other => panic!("expected a Stub named Foo, got {other:?}"),
    }
    assert!(use_block.use_generics.is_empty());
    assert_eq!(use_block.methods.len(), 1);
}

#[test]
fn use_block_struct_inside() {
    let (module, store, context) = parse("use Foo { struct Inner {} }");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.into_vec()
    );

    let stmt = get_statement(&store, &module, 0);
    let use_block = match &stmt.node {
        StatementKind::UseBlock(b) => b,
        _ => panic!("expected UseBlock"),
    };
    assert_eq!(use_block.statements.len(), 1);
    let inner = &store.statements[use_block.statements[0]];
    match &inner.node {
        StatementKind::Struct(Struct { name, .. }) => {
            assert_eq!(name.as_str(), "Inner");
        }
        _ => panic!("expected Struct inside use block"),
    }
    assert!(use_block.methods.is_empty());
    assert!(use_block.impls.is_empty());
}

#[test]
fn use_block_import_inside() {
    let (module, store, context) = parse("use Foo {\nimport bar.baz\n}");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.into_vec()
    );

    let stmt = get_statement(&store, &module, 0);
    let use_block = match &stmt.node {
        StatementKind::UseBlock(b) => b,
        _ => panic!("expected UseBlock"),
    };
    assert_eq!(use_block.statements.len(), 1);
    let inner = &store.statements[use_block.statements[0]];
    match &inner.node {
        StatementKind::Import(Import { paths, .. }) => {
            assert_eq!(paths.len(), 1);
            assert_eq!(paths[0].kind, ImportKind::Module);
        }
        _ => panic!("expected Import inside use block"),
    }
    assert!(use_block.methods.is_empty());
    assert!(use_block.impls.is_empty());
}

#[test]
fn use_block_type_alias_inside() {
    let (module, store, context) = parse("use Foo { type MyInt = int }");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.into_vec()
    );

    let stmt = get_statement(&store, &module, 0);
    let use_block = match &stmt.node {
        StatementKind::UseBlock(b) => b,
        _ => panic!("expected UseBlock"),
    };
    assert_eq!(use_block.statements.len(), 1);
    let inner = &store.statements[use_block.statements[0]];
    match &inner.node {
        StatementKind::TypeDef(_) => (),
        other => panic!("expected TypeDef inside use block, got {:?}", other),
    }
    assert!(use_block.methods.is_empty());
    assert!(use_block.impls.is_empty());
}

#[test]
fn use_block_inline_method() {
    let (module, store, context, declares) = parse_with_declares("use Foo bar() {}");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.into_vec()
    );

    let stmt = get_statement(&store, &module, 0);
    let use_block = match &stmt.node {
        StatementKind::UseBlock(b) => b,
        _ => panic!("expected UseBlock"),
    };
    match declares.get_type(use_block.ty) {
        Some(SolType::Stub(stub)) => {
            assert!(stub.matches_ignoring_occurrence(&Stub::new("Foo")))
        }
        other => panic!("expected a Stub named Foo, got {other:?}"),
    }
    assert_eq!(use_block.methods.len(), 1);
    assert!(!use_block.methods[0].is_public);
    let func = &store.functions[use_block.methods[0].id];
    let FunctionKind::Normal(f) = func else {
        panic!("expected Normal function");
    };
    assert_eq!(f.signature.value.name.as_str(), "bar");
    assert!(use_block.impls.is_empty());
    assert!(use_block.statements.is_empty());
}

#[test]
fn use_block_mixed_methods_and_impl() {
    let (module, store, context, declares) =
        parse_with_declares("use Foo { bar() {}\nimpl Baz { qux() {} }\nbaz() {} }");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.into_vec()
    );

    let stmt = get_statement(&store, &module, 0);
    let use_block = match &stmt.node {
        StatementKind::UseBlock(b) => b,
        _ => panic!("expected UseBlock"),
    };
    assert_eq!(use_block.methods.len(), 2);
    assert_eq!(use_block.impls.len(), 1);
    match declares.get_type(use_block.impls[0].impl_trait) {
        Some(SolType::Stub(stub)) => {
            assert!(stub.matches_ignoring_occurrence(&Stub::new("Baz")))
        }
        other => panic!("expected a Stub named Baz, got {other:?}"),
    }
    assert_eq!(use_block.impls[0].methods.len(), 1);
    let func = &store.functions[use_block.impls[0].methods[0]];
    let FunctionKind::Normal(f) = func else {
        panic!("expected Normal function");
    };
    assert_eq!(f.signature.value.name.as_str(), "qux");
}

#[test]
fn use_block_method_with_return_type() {
    let (module, store, context, declares) = parse_with_declares("use Foo { bar(): int { 42 } }");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.into_vec()
    );

    let stmt = get_statement(&store, &module, 0);
    let use_block = match &stmt.node {
        StatementKind::UseBlock(b) => b,
        _ => panic!("expected UseBlock"),
    };
    assert_eq!(use_block.methods.len(), 1);
    let func = &store.functions[use_block.methods[0].id];
    let FunctionKind::Normal(f) = func else {
        panic!("expected Normal function");
    };
    assert_eq!(
        declares.get_type(f.signature.value.return_type),
        Some(&SolType::Primitive(PrimitiveTypes::Int))
    );
}

// ----------------------------------------------------------------
//  Bad-path: malformed use blocks
// ----------------------------------------------------------------
#[test]
fn use_block_rejects_variable_statement() {
    let (_, _, context) = parse("use Foo { x := 5 }");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        1,
        "{:#?}",
        context.faults.into_vec()
    );
    assert!(
        context.faults.iter().any(|fault| matches!(
            fault.kind(),
            crate::fault::AstErrorKind::VariableNotAllowedInUseBlock
        )),
        "{:#?}",
        context.faults.into_vec()
    );
}

#[test]
fn use_block_rejects_assignment_statement() {
    let (_, _, context) = parse("use Foo { x = 5 }");
    assert!(
        context.faults.count_severity(Severity::Error) > 0,
        "expected an error for an assignment statement inside a use block"
    );
}

#[test]
fn use_block_recovers_after_rejected_statement_and_still_parses_later_method() {
    let (module, store, context) = parse("use Foo { x := 5\nbar() {} }");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        1,
        "{:#?}",
        context.faults.into_vec()
    );

    let stmt = get_statement(&store, &module, 0);
    let use_block = match &stmt.node {
        StatementKind::UseBlock(b) => b,
        _ => panic!("expected UseBlock"),
    };
    assert_eq!(use_block.methods.len(), 1);
    let func = &store.functions[use_block.methods[0].id];
    let FunctionKind::Normal(f) = func else {
        panic!("expected Normal function");
    };
    assert_eq!(f.signature.value.name.as_str(), "bar");
}

#[test]
fn use_block_missing_type_is_rejected() {
    let (_, _, context) = parse("use { bar() {} }");
    assert!(
        context.faults.count_severity(Severity::Error) > 0,
        "expected an error when a use block has no type"
    );
}

#[test]
fn impl_block_missing_trait_type_is_rejected() {
    let (_, _, context) = parse("use Foo { impl { bar() {} } }");
    assert!(
        context.faults.count_severity(Severity::Error) > 0,
        "expected an error when an impl block has no trait type"
    );
}

#[test]
fn unclosed_impl_block_is_rejected() {
    let (_, _, context) = parse("use Foo { impl Bar { baz()");
    assert!(
        context.faults.count_severity(Severity::Error) > 0,
        "expected an error on an unclosed impl block"
    );
}

#[test]
fn use_block_method_with_params() {
    let (module, store, context, declares) =
        parse_with_declares("use Foo { bar(x: int, y: string) {} }");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.into_vec()
    );

    let stmt = get_statement(&store, &module, 0);
    let use_block = match &stmt.node {
        StatementKind::UseBlock(b) => b,
        _ => panic!("expected UseBlock"),
    };
    assert_eq!(use_block.methods.len(), 1);
    let func = &store.functions[use_block.methods[0].id];
    let FunctionKind::Normal(f) = func else {
        panic!("expected Normal function");
    };
    assert_eq!(f.signature.value.parameters.len(), 2);
    assert_eq!(f.signature.value.parameters[0].name.as_str(), "x");
    assert_eq!(
        declares.get_type(f.signature.value.parameters[0].ty),
        Some(&SolType::Primitive(PrimitiveTypes::Int))
    );
}
