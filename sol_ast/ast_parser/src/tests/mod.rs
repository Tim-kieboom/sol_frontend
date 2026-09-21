use std::{fs, path::PathBuf, sync::LazyLock};

use ast_model::{
    AnyArray, ArrayKind, ArrayType, Assignment, AstStore, AstTree, Constructor, ExpressionKind,
    FunctionCall, FunctionCalleeKind, Import, ImportKind, Literal, MatchMethod, Module, NodeId,
    ReferenceType, SolType, Statement, StatementKind, StructConstructor, Stub, TypeId, TypeOf,
    TypeofKind, Variable, operators::BinaryOperatorKind,
};
use sol_tokenizer::to_token_stream;
use sol_utils::{
    CrateContext, Mutable, SharedStr,
    collections::{
        crate_store::{CrateEntry, CrateStore},
        module_store::ModuleStore,
    },
    fault::Severity,
    ids::IdAlloc,
    linkage::Linkage,
    sol_names::PrimitiveTypes,
    span::ModuleId,
};

use crate::{ParseInfo, fault::AstErrorKind, parse_module};

mod associated_constants;
mod attributes;
mod big_test;
mod conditional;
mod enums;
mod functions;
mod lambda;
mod literals;
mod regressions;
mod structs;
mod union;
mod use_block;
mod variables;

static TEST_ENV: LazyLock<TestEnv> = LazyLock::new(|| {
    let base = std::env::temp_dir().join(format!("sol_test_modules_{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).expect("failed to create test dir");

    for file in &["bar.sol", "core.sol", "io.sol", "fmt.sol", "baz.sol"] {
        fs::write(base.join(file), "").ok();
    }

    TestEnv { base }
});

struct TestEnv {
    base: PathBuf,
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn module_id() -> ModuleId {
    ModuleId::begin()
}

fn create_test_crate_store(test_env: &TestEnv) -> CrateStore {
    let mut store = CrateStore::new();
    for name in &["foo", "sol", "bar"] {
        store.insert(
            name.to_string(),
            CrateEntry::new(name.to_string(), test_env.base.clone()).apply_linkage(Linkage::Static),
        );
    }
    store
}

fn parse(source: &str) -> (Module, AstStore, CrateContext<AstErrorKind>) {
    let (module, store, context, _declares) = parse_with_declares(source);
    (module, store, context)
}

/// Like [`parse`], but also returns the `DeclareStore` populated during
/// parsing — needed by tests that inspect an interned `TypeId` (e.g. a
/// `Parameter.ty`) back to its `SolType`.
fn parse_with_declares(
    source: &str,
) -> (
    Module,
    AstStore,
    CrateContext<AstErrorKind>,
    ast_model::declare_store::DeclareStore,
) {
    let test_env = &*TEST_ENV;
    let module_id = module_id();
    let stream = to_token_stream(source, module_id).unwrap();
    let mut modules = ModuleStore::new();
    let mut ast = AstTree::new(module_id);
    let crate_store = create_test_crate_store(test_env);
    let info = ParseInfo {
        id: module_id,
        parent: None,
        source_folder: test_env.base.clone(),
        crate_source_folder: test_env.base.clone(),
        context: &mut ast.context,
        modules: &mut modules,
        forest: &mut ast.crates,
        declares: &mut ast.declares,
        crate_store: &crate_store,
    };
    parse_module(stream, "test".to_string(), info);
    let module = ast
        .crates
        .modules
        .as_vecmap_mut()
        .remove(module_id)
        .expect("should have module");

    (module, ast.crates.store, ast.context, ast.declares)
}

fn get_statement<'a>(store: &'a AstStore, module: &Module, index: usize) -> &'a Statement {
    let block = &store.blocks[module.global];
    &store.statements[block.statements[index]]
}

// ----------------------------------------------------------------
//  Empty / minimal
// ----------------------------------------------------------------
#[test]
fn empty_module() {
    let (module, store, context) = parse("");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let block = &store.blocks[module.global];
    assert!(block.statements.is_empty(), "expected no statements");
}

#[test]
fn only_newlines() {
    let (module, store, context) = parse("\n\n\n\n");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let block = &store.blocks[module.global];
    assert!(block.statements.is_empty());
}

// ----------------------------------------------------------------
//  Binary expressions
// ----------------------------------------------------------------
#[test]
fn binary_power() {
    let (module, store, context) = parse("2 ** 3");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    match &stmt.node {
        StatementKind::Expression { expression, .. } => {
            let expr = &store.expressions[*expression];
            match &expr.node {
                ExpressionKind::Binary(bin) => {
                    assert_eq!(bin.operator.value, BinaryOperatorKind::Pow);
                    let left = &store.expressions[bin.left];
                    let right = &store.expressions[bin.right];
                    assert!(matches!(
                        left.node,
                        ExpressionKind::Literal((_, Literal::Uint(2)))
                    ));
                    assert!(matches!(
                        right.node,
                        ExpressionKind::Literal((_, Literal::Uint(3)))
                    ));
                }
                _ => panic!("expected Binary expression"),
            }
        }
        _ => panic!("expected Expression statement"),
    }
}

#[test]
fn binary_addition() {
    let (module, store, context) = parse("1 + 2");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    match &stmt.node {
        StatementKind::Expression { expression, .. } => {
            let expr = &store.expressions[*expression];
            match &expr.node {
                ExpressionKind::Binary(bin) => {
                    assert_eq!(bin.operator.value, BinaryOperatorKind::Add);
                    let left = &store.expressions[bin.left];
                    let right = &store.expressions[bin.right];
                    assert!(matches!(
                        left.node,
                        ExpressionKind::Literal((_, Literal::Uint(1)))
                    ));
                    assert!(matches!(
                        right.node,
                        ExpressionKind::Literal((_, Literal::Uint(2)))
                    ));
                }
                _ => panic!("expected Binary expression"),
            }
        }
        _ => panic!("expected Expression statement"),
    }
}

// ----------------------------------------------------------------
//  Parenthesized expressions
// ----------------------------------------------------------------
#[test]
fn parenthesized_expression() {
    let (module, store, context) = parse("(1 + 2) * 3");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    match &stmt.node {
        StatementKind::Expression { expression, .. } => {
            let expr = &store.expressions[*expression];
            match &expr.node {
                ExpressionKind::Binary(outer) => {
                    assert_eq!(outer.operator.value, BinaryOperatorKind::Mul);
                    let left = &store.expressions[outer.left];
                    match &left.node {
                        ExpressionKind::Binary(inner) => {
                            assert_eq!(inner.operator.value, BinaryOperatorKind::Add);
                            let a = &store.expressions[inner.left];
                            let b = &store.expressions[inner.right];
                            assert!(matches!(
                                a.node,
                                ExpressionKind::Literal((_, Literal::Uint(1)))
                            ));
                            assert!(matches!(
                                b.node,
                                ExpressionKind::Literal((_, Literal::Uint(2)))
                            ));
                        }
                        other => panic!("expected inner Binary(Add), got {:?}", other),
                    }
                    let right = &store.expressions[outer.right];
                    assert!(matches!(
                        right.node,
                        ExpressionKind::Literal((_, Literal::Uint(3)))
                    ));
                }
                other => panic!("expected outer Binary(Mul), got {:?}", other),
            }
        }
        other => panic!("expected Expression statement, got {:?}", other),
    }
}

#[test]
fn simple_parens() {
    let (module, store, context) = parse("(42)");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    match &stmt.node {
        StatementKind::Expression { expression, .. } => {
            let expr = &store.expressions[*expression];
            assert!(matches!(
                expr.node,
                ExpressionKind::Literal((_, Literal::Uint(42)))
            ));
        }
        other => panic!("expected Expression statement, got {:?}", other),
    }
}

#[test]
fn nested_parens() {
    let (module, store, context) = parse("((1 + 2))");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    match &stmt.node {
        StatementKind::Expression { expression, .. } => {
            let expr = &store.expressions[*expression];
            match &expr.node {
                ExpressionKind::Binary(bin) => {
                    assert_eq!(bin.operator.value, BinaryOperatorKind::Add);
                }
                other => panic!("expected Binary(Add), got {:?}", other),
            }
        }
        other => panic!("expected Expression statement, got {:?}", other),
    }
}

// ----------------------------------------------------------------
//  Assignments
// ----------------------------------------------------------------
#[test]
fn simple_assignment() {
    let (module, store, context) = parse("x = 5");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    match &stmt.node {
        StatementKind::Assignment(Assignment { left, right, .. }) => {
            let l = &store.expressions[*left];
            match &l.node {
                ExpressionKind::Variable(v) => assert_eq!(v.name.as_str(), "x"),
                _ => panic!("expected Variable on LHS"),
            }
            let r = &store.expressions[*right];
            assert!(matches!(
                r.node,
                ExpressionKind::Literal((_, Literal::Uint(5)))
            ));
        }
        _ => panic!("expected Assignment statement"),
    }
}

// ----------------------------------------------------------------
//  Struct constructor
// ----------------------------------------------------------------
#[test]
fn struct_constructor() {
    let (module, store, context, declares) = parse_with_declares("Point { x: 1, y: 2 }");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    match &stmt.node {
        StatementKind::Expression { expression, .. } => {
            let expr = &store.expressions[*expression];
            match &expr.node {
                ExpressionKind::StructConstructor(StructConstructor {
                    struct_type,
                    values,
                    ..
                }) => {
                    match declares.get_type(*struct_type) {
                        Some(SolType::Stub(stub)) => {
                            assert!(stub.matches_ignoring_occurrence(&Stub {
                                name: "Point".into(),
                                generics: vec![].into(),
                                occurrence: NodeId::ERROR,
                            }))
                        }
                        other => panic!("expected a Stub named Point, got {other:?}"),
                    }
                    assert_eq!(values.len(), 2);
                    assert_eq!(values[0].0.as_str(), "x");
                    assert_eq!(values[1].0.as_str(), "y");
                }
                _ => panic!("expected StructConstructor expression"),
            }
        }
        _ => panic!("expected Expression statement"),
    }
}

#[test]
fn struct_constructor_shorthand() {
    let (module, store, context) = parse("Point { x, y }");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    match &stmt.node {
        StatementKind::Expression { expression, .. } => {
            let expr = &store.expressions[*expression];
            match &expr.node {
                ExpressionKind::StructConstructor(StructConstructor { values, .. }) => {
                    assert_eq!(values.len(), 2);
                }
                _ => panic!("expected StructConstructor expression"),
            }
        }
        _ => panic!("expected Expression statement"),
    }
}

// ----------------------------------------------------------------
//  Imports
// ----------------------------------------------------------------
#[test]
fn simple_import() {
    let (module, store, context) = parse("import foo.bar");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    match &stmt.node {
        StatementKind::Import(Import { paths, .. }) => {
            assert_eq!(paths.len(), 1);
            assert_eq!(paths[0].kind, ImportKind::Module);
        }
        _ => panic!("expected Import statement"),
    }
}

#[test]
fn import_glob() {
    let (module, store, context) = parse("import foo.bar.*");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    match &stmt.node {
        StatementKind::Import(Import { paths, .. }) => {
            assert_eq!(paths.len(), 1);
            assert_eq!(paths[0].kind, ImportKind::Glob);
        }
        _ => panic!("expected Import statement"),
    }
}

// ----------------------------------------------------------------
//  Blocks
// ----------------------------------------------------------------
#[test]
fn block_expression() {
    let (module, store, context) = parse("{ x := 1 }");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    match &stmt.node {
        StatementKind::Expression { expression, .. } => {
            let expr = &store.expressions[*expression];
            match &expr.node {
                ExpressionKind::Block(block_id) => {
                    let block = &store.blocks[*block_id];
                    assert_eq!(block.statements.len(), 1);
                }
                _ => panic!("expected Block expression"),
            }
        }
        _ => panic!("expected Expression statement"),
    }
}

// ----------------------------------------------------------------
//  New expressions
// ----------------------------------------------------------------
#[test]
fn new_ptr() {
    let (module, store, context) = parse("new(42)");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    match &stmt.node {
        StatementKind::Expression { expression, .. } => {
            let expr = &store.expressions[*expression];
            match &expr.node {
                ExpressionKind::New(inner, mutable) => {
                    let inner_expr = &store.expressions[*inner];
                    assert!(matches!(
                        inner_expr.node,
                        ExpressionKind::Literal((_, Literal::Uint(42)))
                    ));
                    assert_eq!(*mutable, sol_utils::Mutable::Immut);
                }
                _ => panic!("expected New expression"),
            }
        }
        _ => panic!("expected Expression statement"),
    }
}

#[test]
fn new_mut_ptr() {
    let (module, store, context) = parse("new mut(42)");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    match &stmt.node {
        StatementKind::Expression { expression, .. } => {
            let expr = &store.expressions[*expression];
            match &expr.node {
                ExpressionKind::New(inner, mutable) => {
                    let inner_expr = &store.expressions[*inner];
                    assert!(matches!(
                        inner_expr.node,
                        ExpressionKind::Literal((_, Literal::Uint(42)))
                    ));
                    assert_eq!(*mutable, sol_utils::Mutable::Mut);
                }
                _ => panic!("expected New expression"),
            }
        }
        _ => panic!("expected Expression statement"),
    }
}

// ----------------------------------------------------------------
//  Type alias
// ----------------------------------------------------------------
#[test]
fn type_alias() {
    let (module, store, context, declares) = parse_with_declares("type MyInt = int");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    match &stmt.node {
        StatementKind::TypeDef(def) => {
            match declares.get_type(def.new_type) {
                Some(SolType::Stub(stub)) => {
                    assert!(stub.matches_ignoring_occurrence(&Stub {
                        name: "MyInt".into(),
                        generics: vec![].into(),
                        occurrence: NodeId::ERROR,
                    }))
                }
                other => panic!("expected a Stub named MyInt, got {other:?}"),
            }
            assert_eq!(
                declares.get_type(def.old_type),
                Some(&SolType::Primitive(PrimitiveTypes::Int))
            );
            assert!(!def.is_distinct);
        }
        _ => panic!("expected TypeDef statement"),
    }
}

// ----------------------------------------------------------------
//  Multi-statement module
// ----------------------------------------------------------------
#[test]
fn multiple_statements() {
    let source = "x := 1\ny := 2\nz := 3";

    let (module, store, context) = parse(source);
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let block = &store.blocks[module.global];
    assert_eq!(block.statements.len(), 3);
}

// ----------------------------------------------------------------
//  Error recovery — parser does not panic on bad input
// ----------------------------------------------------------------
fn has_fault_matching(
    context: &CrateContext<crate::fault::AstErrorKind>,
    predicate: impl Fn(&crate::fault::AstErrorKind) -> bool,
) -> bool {
    context
        .faults
        .faults
        .iter()
        .any(|fault| predicate(fault.kind()))
}

#[test]
fn error_on_bad_token() {
    let (_, _, context) = parse("???");
    assert!(
        has_fault_matching(&context, |kind| {
            matches!(
                kind,
                crate::fault::AstErrorKind::InvalidExpressionStart { .. }
            )
        }),
        "expected an 'invalid start of expression' fault: {:#?}",
        context.faults.faults
    );
}

#[test]
fn error_partial_expression() {
    let (_, _, context) = parse("1 +\n");
    assert!(
        has_fault_matching(&context, |kind| {
            matches!(
                kind,
                crate::fault::AstErrorKind::InvalidExpressionStart { .. }
            )
        }),
        "expected an 'invalid start of expression' fault: {:#?}",
        context.faults.faults
    );
}

#[test]
fn unclosed_struct_brace_is_rejected() {
    let (_, _, context) = parse("struct Foo {");
    assert!(
        context.faults.count_severity(Severity::Error) > 0,
        "expected an error on an unclosed struct body"
    );
}

#[test]
fn unclosed_function_paren_is_rejected() {
    let (_, _, context) = parse("foo(");
    assert!(
        context.faults.count_severity(Severity::Error) > 0,
        "expected an error on an unclosed parameter list"
    );
}

#[test]
fn unclosed_enum_brace_is_rejected() {
    let (_, _, context) = parse("enum Foo {");
    assert!(
        context.faults.count_severity(Severity::Error) > 0,
        "expected an error on an unclosed enum body"
    );
}

#[test]
fn unclosed_use_block_is_rejected() {
    let (_, _, context) = parse("use Foo {");
    assert!(
        context.faults.count_severity(Severity::Error) > 0,
        "expected an error on an unclosed use block"
    );
}

#[test]
fn truncated_function_signature_is_rejected() {
    let (_, _, context) = parse("foo(a:");
    assert!(
        context.faults.count_severity(Severity::Error) > 0,
        "expected an error on a truncated parameter type"
    );
}

#[test]
fn well_formed_struct_after_malformed_examples_reports_no_error() {
    let (_, _, context) = parse("struct Foo {}");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );
}

// ----------------------------------------------------------------
//  Field access
// ----------------------------------------------------------------
#[test]
fn field_access() {
    let (module, store, context) = parse("obj.field");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    match &stmt.node {
        StatementKind::Expression { expression, .. } => {
            let expr = &store.expressions[*expression];
            match &expr.node {
                ExpressionKind::FieldAccess(fa) => {
                    let obj = &store.expressions[fa.object];
                    match &obj.node {
                        ExpressionKind::Variable(v) => assert_eq!(v.name.as_str(), "obj"),
                        _ => panic!("expected Variable on LHS of field access"),
                    }
                    assert_eq!(fa.field.as_str(), "field");
                }
                _ => panic!("expected FieldAccess expression"),
            }
        }
        _ => panic!("expected Expression statement"),
    }
}

// ----------------------------------------------------------------
//  Nested function call chain
// ----------------------------------------------------------------
#[test]
fn chained_function_calls() {
    let (module, store, context) = parse("foo().bar()");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    match &stmt.node {
        StatementKind::Expression { expression, .. } => {
            let expr = &store.expressions[*expression];
            match &expr.node {
                ExpressionKind::FunctionCall(FunctionCall { name, callee, .. }) => {
                    assert_eq!(name.as_str(), "bar");
                    assert!(callee.is_some());
                    let value = match callee.as_ref().unwrap().kind {
                        FunctionCalleeKind::Type(_) => {
                            panic!("should be FunctionCalleeKind::Expression")
                        }
                        FunctionCalleeKind::Expression(val) => val,
                    };
                    let inner = &store.expressions[value];
                    match &inner.node {
                        ExpressionKind::FunctionCall(FunctionCall {
                            name: inner_name, ..
                        }) => {
                            assert_eq!(inner_name.as_str(), "foo");
                        }
                        _ => panic!("expected inner FunctionCall"),
                    }
                }
                _ => panic!("expected outer FunctionCall"),
            }
        }
        _ => panic!("expected Expression statement"),
    }
}

// ----------------------------------------------------------------
//  Option type wrapping
// ----------------------------------------------------------------
#[test]
fn optional_type_variable() {
    let (module, store, context, mut declares) = parse_with_declares("x: ?int = null");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    let Variable { ty, .. } = match &stmt.node {
        StatementKind::Variable(v) => v,
        _ => panic!("expected Variable"),
    };
    let int_id = declares.intern_type(SolType::Primitive(
        sol_utils::sol_names::PrimitiveTypes::Int,
    ));
    assert_eq!(
        ty.map(|id| declares.get_type(id).unwrap().clone()),
        Some(SolType::Optional(int_id))
    );
}

// ----------------------------------------------------------------
//  Reference type variable
// ----------------------------------------------------------------
#[test]
fn reference_type_variable() {
    let (module, store, context, mut declares) = parse_with_declares("x: &int = &1");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    let Variable { ty, .. } = match &stmt.node {
        StatementKind::Variable(v) => v,
        _ => panic!("expected Variable"),
    };
    let int_id = declares.intern_type(SolType::Primitive(PrimitiveTypes::Int));
    assert_eq!(
        ty.map(|id| declares.get_type(id).unwrap().clone()),
        Some(SolType::Reference(ReferenceType::new(
            int_id,
            Mutable::Immut
        )))
    );
}

#[test]
fn mut_reference_type_variable() {
    let (module, store, context, mut declares) = parse_with_declares("x: &mut int = &mut 1");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    let Variable { ty, .. } = match &stmt.node {
        StatementKind::Variable(v) => v,
        _ => panic!("expected Variable"),
    };
    let int_id = declares.intern_type(SolType::Primitive(PrimitiveTypes::Int));
    assert_eq!(
        ty.map(|id| declares.get_type(id).unwrap().clone()),
        Some(SolType::Reference(ReferenceType::new(
            int_id,
            Mutable::Mut
        )))
    );
}

// ----------------------------------------------------------------
//  Struct constructor — empty and with defaults
// ----------------------------------------------------------------
#[test]
fn struct_constructor_empty() {
    let (module, store, context) = parse("Point {}");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    match &stmt.node {
        StatementKind::Expression { expression, .. } => {
            let expr = &store.expressions[*expression];
            match &expr.node {
                ExpressionKind::StructConstructor(StructConstructor {
                    values, defaults, ..
                }) => {
                    assert!(values.is_empty());
                    assert!(!defaults);
                }
                _ => panic!("expected StructConstructor"),
            }
        }
        _ => panic!("expected Expression statement"),
    }
}

#[test]
fn struct_constructor_defaults() {
    let (module, store, context) = parse("Point { x: 1, .. }");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    match &stmt.node {
        StatementKind::Expression { expression, .. } => {
            let expr = &store.expressions[*expression];
            match &expr.node {
                ExpressionKind::StructConstructor(StructConstructor {
                    values, defaults, ..
                }) => {
                    assert_eq!(values.len(), 1);
                    assert!(defaults);
                }
                _ => panic!("expected StructConstructor"),
            }
        }
        _ => panic!("expected Expression statement"),
    }
}

// ----------------------------------------------------------------
//  Array literal / filler
// ----------------------------------------------------------------
#[test]
fn array_literal() {
    let (module, store, context) = parse("[1, 2, 3]");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    match &stmt.node {
        StatementKind::Expression { expression, .. } => {
            let expr = &store.expressions[*expression];
            match &expr.node {
                ExpressionKind::Array(AnyArray::Array(arr)) => {
                    assert_eq!(arr.values.len(), 3);
                }
                other => panic!("expected Array, got {:?}", other),
            }
        }
        _ => panic!("expected Expression statement"),
    }
}

#[test]
fn array_filler() {
    let (module, store, context) = parse("[for 3 => 0]");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    match &stmt.node {
        StatementKind::Expression { expression, .. } => {
            let expr = &store.expressions[*expression];
            match &expr.node {
                ExpressionKind::Array(AnyArray::ArrayFiller(filler)) => {
                    let amount = &store.expressions[filler.amount];
                    let element = &store.expressions[filler.element];
                    assert!(matches!(
                        amount.node,
                        ExpressionKind::Literal((_, Literal::Uint(3)))
                    ));
                    assert!(matches!(
                        element.node,
                        ExpressionKind::Literal((_, Literal::Uint(0)))
                    ));
                }
                other => panic!("expected ArrayFiller, got {:?}", other),
            }
        }
        _ => panic!("expected Expression statement"),
    }
}

#[test]
fn array_filler_with_index() {
    let (module, store, context) = parse("[for i in 3 => i]");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    match &stmt.node {
        StatementKind::Expression { expression, .. } => {
            let expr = &store.expressions[*expression];
            match &expr.node {
                ExpressionKind::Array(AnyArray::ArrayFiller(filler)) => {
                    let index = filler.for_index.as_ref().expect("expected for_index");
                    assert_eq!(index.ident.as_str(), "i");
                    assert!(filler.for_index.is_some());
                    let amount = &store.expressions[filler.amount];
                    assert!(matches!(
                        amount.node,
                        ExpressionKind::Literal((_, Literal::Uint(3)))
                    ));
                    assert!(filler.element != filler.amount);
                }
                other => panic!("expected ArrayFiller, got {:?}", other),
            }
        }
        _ => panic!("expected Expression statement"),
    }
}

#[test]
fn array_contructor_literal() {
    let (module, store, context, declares) = parse_with_declares("List.[1, 2, 3]");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    match &stmt.node {
        StatementKind::Expression { expression, .. } => {
            let expr = &store.expressions[*expression];
            match &expr.node {
                ExpressionKind::Array(AnyArray::Array(arr)) => {
                    match arr.collection_type.and_then(|id| declares.get_type(id)) {
                        Some(SolType::Stub(stub)) => {
                            assert!(stub.matches_ignoring_occurrence(&Stub::new("List")))
                        }
                        other => panic!("expected a Stub named List, got {other:?}"),
                    }
                    assert_eq!(arr.element_type, None);
                    assert_eq!(arr.values.len(), 3);
                }
                other => panic!("expected Array, got {:?}", other),
            }
        }
        _ => panic!("expected Expression statement"),
    }
}

#[test]
fn array_contructor_element_type_literal() {
    let (module, store, context, declares) = parse_with_declares("List.[int: 1, 2, 3]");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    match &stmt.node {
        StatementKind::Expression { expression, .. } => {
            let expr = &store.expressions[*expression];
            match &expr.node {
                ExpressionKind::Array(AnyArray::Array(arr)) => {
                    match arr.collection_type.and_then(|id| declares.get_type(id)) {
                        Some(SolType::Stub(stub)) => {
                            assert!(stub.matches_ignoring_occurrence(&Stub::new("List")))
                        }
                        other => panic!("expected a Stub named List, got {other:?}"),
                    }
                    assert_eq!(
                        arr.element_type.and_then(|id| declares.get_type(id)),
                        Some(&SolType::Primitive(PrimitiveTypes::Int))
                    );
                    assert_eq!(arr.values.len(), 3);
                }
                other => panic!("expected Array, got {:?}", other),
            }
        }
        _ => panic!("expected Expression statement"),
    }
}

#[test]
fn array_contructor_empty_literal() {
    let (module, store, context, declares) = parse_with_declares("List.[]");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    match &stmt.node {
        StatementKind::Expression { expression, .. } => {
            let expr = &store.expressions[*expression];
            match &expr.node {
                ExpressionKind::Array(AnyArray::Array(arr)) => {
                    match arr.collection_type.and_then(|id| declares.get_type(id)) {
                        Some(SolType::Stub(stub)) => {
                            assert!(stub.matches_ignoring_occurrence(&Stub::new("List")))
                        }
                        other => panic!("expected a Stub named List, got {other:?}"),
                    }
                    assert_eq!(arr.element_type, None);
                    assert_eq!(arr.values.len(), 0);
                }
                other => panic!("expected Array, got {:?}", other),
            }
        }
        _ => panic!("expected Expression statement"),
    }
}

#[test]
fn array_contructor_generic_literal() {
    let (module, store, context, mut declares) = parse_with_declares("List<int>.[1, 2, 3]");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults,
    );

    let stmt = get_statement(&store, &module, 0);
    match &stmt.node {
        StatementKind::Expression { expression, .. } => {
            let expr = &store.expressions[*expression];
            match &expr.node {
                ExpressionKind::Array(AnyArray::Array(arr)) => {
                    let int_id = declares.intern_type(SolType::Primitive(PrimitiveTypes::Int));
                    match arr.collection_type.and_then(|id| declares.get_type(id)) {
                        Some(SolType::Stub(stub)) => {
                            assert!(stub.matches_ignoring_occurrence(&Stub {
                                name: SharedStr::new("List"),
                                generics: vec![int_id].into(),
                                occurrence: NodeId::ERROR,
                            }))
                        }
                        other => panic!("expected a Stub named List, got {other:?}"),
                    }
                    assert_eq!(arr.element_type, None);
                    assert_eq!(arr.values.len(), 3);
                }
                other => panic!("expected Array, got {:?}", other),
            }
        }
        _ => panic!("expected Expression statement"),
    }
}

// ----------------------------------------------------------------
//  New array
// ----------------------------------------------------------------
#[test]
fn new_array() {
    let (module, store, context) = parse("new[1, 2]");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    match &stmt.node {
        StatementKind::Expression { expression, .. } => {
            let expr = &store.expressions[*expression];
            match &expr.node {
                ExpressionKind::NewArray(AnyArray::Array(arr)) => {
                    assert_eq!(arr.values.len(), 2);
                }
                other => panic!("expected NewArray, got {:?}", other),
            }
        }
        _ => panic!("expected Expression statement"),
    }
}

// ----------------------------------------------------------------
//  Deref expression
// ----------------------------------------------------------------
#[test]
fn deref_expression() {
    let (module, store, context) = parse("*ptr");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    match &stmt.node {
        StatementKind::Expression { expression, .. } => {
            let expr = &store.expressions[*expression];
            match &expr.node {
                ExpressionKind::Deref(d) => {
                    let inner = &store.expressions[d.value];
                    match &inner.node {
                        ExpressionKind::Variable(v) => assert_eq!(v.name.as_str(), "ptr"),
                        _ => panic!("expected Variable inside Deref"),
                    }
                }
                other => panic!("expected Deref, got {:?}", other),
            }
        }
        _ => panic!("expected Expression statement"),
    }
}

// ----------------------------------------------------------------
//  TypeOf expression
// ----------------------------------------------------------------
#[test]
fn typeof_expression() {
    let (module, store, context) = parse("x typeof Foo.Bar");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    match &stmt.node {
        StatementKind::Expression { expression, .. } => {
            let expr = &store.expressions[*expression];
            match &expr.node {
                ExpressionKind::TypeOf(TypeOf {
                    kind:
                        TypeofKind::Union {
                            type_name,
                            variant_name,
                        },
                    ..
                }) => {
                    assert_eq!(type_name.as_str(), "Foo");
                    assert_eq!(variant_name.as_str(), "Bar");
                }
                other => panic!("expected TypeOf, got {:?}", other),
            }
        }
        _ => panic!("expected Expression statement"),
    }
}

// ----------------------------------------------------------------
//  Match-method expression
// ----------------------------------------------------------------
#[test]
fn match_method_expression() {
    let (module, store, context) = parse("x.Variant { true }");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    match &stmt.node {
        StatementKind::Expression { expression, .. } => {
            let expr = &store.expressions[*expression];
            match &expr.node {
                ExpressionKind::MatchMethod(MatchMethod {
                    scrutinee, arms, ..
                }) => {
                    let scrut = &store.expressions[*scrutinee];
                    match &scrut.node {
                        ExpressionKind::Variable(v) => assert_eq!(v.name.as_str(), "x"),
                        _ => panic!("expected Variable scrutinee"),
                    }
                    assert_eq!(arms.len(), 1);
                    assert_eq!(arms[0].variant.as_str(), "Variant");
                }
                other => panic!("expected MatchMethod, got {:?}", other),
            }
        }
        _ => panic!("expected Expression statement"),
    }
}

// ----------------------------------------------------------------
//  Constructor expression (Type.(args))
// ----------------------------------------------------------------
#[test]
fn constructor_expression() {
    let (module, store, context) = parse("Foo.(1, 2)");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    match &stmt.node {
        StatementKind::Expression { expression, .. } => {
            let expr = &store.expressions[*expression];
            match &expr.node {
                ExpressionKind::Constructor(Constructor { ty, arguments, .. }) => {
                    match ty {
                        SolType::Stub(stub) => {
                            assert!(stub.matches_ignoring_occurrence(&Stub {
                                name: "Foo".into(),
                                generics: vec![].into(),
                                occurrence: NodeId::ERROR,
                            }))
                        }
                        other => panic!("expected a Stub named Foo, got {other:?}"),
                    }
                    assert_eq!(arguments.len(), 2);
                }
                other => panic!("expected Constructor, got {:?}", other),
            }
        }
        _ => panic!("expected Expression statement"),
    }
}

// ----------------------------------------------------------------
//  Compound assignments (+=, -=, etc.)
// ----------------------------------------------------------------
#[test]
fn compound_add_assign() {
    let (module, store, context) = parse("x += 5");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    match &stmt.node {
        StatementKind::Assignment(Assignment { left, right, .. }) => {
            let l = &store.expressions[*left];
            match &l.node {
                ExpressionKind::Variable(v) => assert_eq!(v.name.as_str(), "x"),
                _ => panic!("expected Variable on LHS"),
            }
            let r = &store.expressions[*right];
            match &r.node {
                ExpressionKind::Binary(bin) => {
                    assert_eq!(bin.operator.value, BinaryOperatorKind::Add);
                }
                other => panic!("expected Binary(Add) in RHS, got {:?}", other),
            }
        }
        _ => panic!("expected Assignment statement"),
    }
}

#[test]
fn compound_sub_assign() {
    let (module, store, context) = parse("x -= 3");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    match &stmt.node {
        StatementKind::Assignment(Assignment { left, right, .. }) => {
            let l = &store.expressions[*left];
            match &l.node {
                ExpressionKind::Variable(v) => assert_eq!(v.name.as_str(), "x"),
                _ => panic!("expected Variable on LHS"),
            }
            let r = &store.expressions[*right];
            match &r.node {
                ExpressionKind::Binary(bin) => {
                    assert_eq!(bin.operator.value, BinaryOperatorKind::Sub);
                }
                other => panic!("expected Binary(Sub) in RHS, got {:?}", other),
            }
        }
        _ => panic!("expected Assignment statement"),
    }
}

// ----------------------------------------------------------------
//  Array type annotations
// ----------------------------------------------------------------
#[test]
fn array_type_wildcard_variable() {
    let (module, store, context, mut declares) = parse_with_declares("mut x: [_]int");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    let Variable { ty, .. } = match &stmt.node {
        StatementKind::Variable(v) => v,
        _ => panic!("expected Variable"),
    };
    assert_eq!(
        ty.map(|id| declares.get_type(id).unwrap().clone()),
        Some(SolType::Array(ArrayType {
            of_type: declares.intern_type(SolType::Primitive(PrimitiveTypes::Int)),
            kind: ArrayKind::StackArrayWildcard,
        }))
    );
}

#[test]
fn array_type_const_slice_variable() {
    let (module, store, context, mut declares) = parse_with_declares("mut x: [&]int");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    let Variable { ty, .. } = match &stmt.node {
        StatementKind::Variable(v) => v,
        _ => panic!("expected Variable"),
    };
    assert_eq!(
        ty.map(|id| declares.get_type(id).unwrap().clone()),
        Some(SolType::Array(ArrayType {
            of_type: declares.intern_type(SolType::Primitive(PrimitiveTypes::Int)),
            kind: ArrayKind::ConstSlice,
        }))
    );
}

#[test]
fn array_type_mut_slice_variable() {
    let (module, store, context, mut declares) = parse_with_declares("mut x: [&mut]int");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    let Variable { ty, .. } = match &stmt.node {
        StatementKind::Variable(v) => v,
        _ => panic!("expected Variable"),
    };
    assert_eq!(
        ty.map(|id| declares.get_type(id).unwrap().clone()),
        Some(SolType::Array(ArrayType {
            of_type: declares.intern_type(SolType::Primitive(PrimitiveTypes::Int)),
            kind: ArrayKind::MutSlice,
        }))
    );
}

#[test]
fn array_type_heap_variable() {
    let (module, store, context, mut declares) = parse_with_declares("mut x: []int");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    let Variable { ty, .. } = match &stmt.node {
        StatementKind::Variable(v) => v,
        _ => panic!("expected Variable"),
    };
    assert_eq!(
        ty.map(|id| declares.get_type(id).unwrap().clone()),
        Some(SolType::Array(ArrayType {
            of_type: declares.intern_type(SolType::Primitive(PrimitiveTypes::Int)),
            kind: ArrayKind::HeapArray,
        }))
    );
}

#[test]
fn array_type_sized_variable() {
    let (module, store, context, mut declares) = parse_with_declares("mut x: [5]int");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    let Variable { ty, .. } = match &stmt.node {
        StatementKind::Variable(v) => v,
        _ => panic!("expected Variable"),
    };
    assert_eq!(
        ty.map(|id| declares.get_type(id).unwrap().clone()),
        Some(SolType::Array(ArrayType {
            of_type: declares.intern_type(SolType::Primitive(PrimitiveTypes::Int)),
            kind: ArrayKind::StackArray(5),
        }))
    );
}

// ----------------------------------------------------------------
//  Pointer type
// ----------------------------------------------------------------
#[test]
fn pointer_type_variable() {
    let (module, store, context, mut declares) = parse_with_declares("mut x: *int");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    let Variable { ty, .. } = match &stmt.node {
        StatementKind::Variable(v) => v,
        _ => panic!("expected Variable"),
    };

    let inner = declares.intern_type(SolType::Primitive(PrimitiveTypes::Int));
    assert_eq!(
        ty.map(|id| declares.get_type(id).unwrap().clone()),
        Some(SolType::Pointer(ReferenceType {
            inner,
            lifetime: None,
            mutable: Mutable::Immut
        }))
    );
}

#[test]
fn pointer_mut_type_variable() {
    let (module, store, context, mut declares) = parse_with_declares("mut x: *mut int");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    let Variable { ty, .. } = match &stmt.node {
        StatementKind::Variable(v) => v,
        _ => panic!("expected Variable"),
    };

    let inner = declares.intern_type(SolType::Primitive(PrimitiveTypes::Int));
    assert_eq!(
        ty.map(|id| declares.get_type(id).unwrap().clone()),
        Some(SolType::Pointer(ReferenceType {
            inner,
            lifetime: None,
            mutable: Mutable::Mut
        }))
    );
}

// ----------------------------------------------------------------
//  Named variant type
// ----------------------------------------------------------------
// ----------------------------------------------------------------
//  RawPtr type
// ----------------------------------------------------------------
#[test]
fn raw_ptr_type_void() {
    let (module, store, context, declares) = parse_with_declares("mut x: RawPtr");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    let Variable { ty, .. } = match &stmt.node {
        StatementKind::Variable(v) => v,
        _ => panic!("expected Variable"),
    };
    assert_eq!(
        ty.map(|id| declares.get_type(id).unwrap().clone()),
        Some(SolType::RawPtr(None))
    );
}

#[test]
fn raw_ptr_type_with_generic() {
    let (module, store, context, declares) = parse_with_declares("mut x: RawPtr<int>");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    let Variable { ty, .. } = match &stmt.node {
        StatementKind::Variable(v) => v,
        _ => panic!("expected Variable"),
    };
    assert_eq!(
        ty.map(|id| declares.get_type(id).unwrap().clone()),
        Some(SolType::RawPtr(Some(TypeId::PRIM_INT)))
    );
}

#[test]
fn raw_ptr_type_explicit_none() {
    let (module, store, context, declares) = parse_with_declares("mut x: RawPtr<none>");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    let Variable { ty, .. } = match &stmt.node {
        StatementKind::Variable(v) => v,
        _ => panic!("expected Variable"),
    };
    assert_eq!(
        ty.map(|id| declares.get_type(id).unwrap().clone()),
        Some(SolType::RawPtr(Some(TypeId::NONE)))
    );
}

// ----------------------------------------------------------------
//  Res type
// ----------------------------------------------------------------
#[test]
fn res_type_void() {
    let (module, store, context, declares) = parse_with_declares("mut x: Res");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    let Variable { ty, .. } = match &stmt.node {
        StatementKind::Variable(v) => v,
        _ => panic!("expected Variable"),
    };
    assert_eq!(
        ty.map(|id| declares.get_type(id).unwrap().clone()),
        Some(SolType::Res {
            ok: None,
            err: None
        })
    );
}

#[test]
fn res_type_one_generic() {
    let (module, store, context, declares) = parse_with_declares("mut x: Res<int>");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    let Variable { ty, .. } = match &stmt.node {
        StatementKind::Variable(v) => v,
        _ => panic!("expected Variable"),
    };
    assert_eq!(
        ty.map(|id| declares.get_type(id).unwrap().clone()),
        Some(SolType::Res {
            ok: Some(TypeId::PRIM_INT),
            err: None,
        })
    );
}

#[test]
fn res_type_two_generics() {
    let (module, store, context, declares) = parse_with_declares("mut x: Res<int, str>");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    let Variable { ty, .. } = match &stmt.node {
        StatementKind::Variable(v) => v,
        _ => panic!("expected Variable"),
    };
    assert_eq!(
        ty.map(|id| declares.get_type(id).unwrap().clone()),
        Some(SolType::Res {
            ok: Some(TypeId::PRIM_INT),
            err: Some(TypeId::STRING),
        })
    );
}

#[test]
fn error_type() {
    let (module, store, context, declares) = parse_with_declares("mut x: Error");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    let Variable { ty, .. } = match &stmt.node {
        StatementKind::Variable(v) => v,
        _ => panic!("expected Variable"),
    };
    assert_eq!(
        ty.map(|id| declares.get_type(id).unwrap().clone()),
        Some(SolType::Error)
    );
}

#[test]
fn named_variant_type_variable() {
    let (module, store, context, declares) = parse_with_declares("mut x: Foo.Bar");
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        0,
        "{:#?}",
        context.faults.faults
    );

    let stmt = get_statement(&store, &module, 0);
    let Variable { ty, .. } = match &stmt.node {
        StatementKind::Variable(v) => v,
        _ => panic!("expected Variable"),
    };
    let resolved_ty = ty.and_then(|id| declares.get_type(id));
    match resolved_ty {
        Some(SolType::NamedVariant { base, variant }) => {
            assert_eq!(variant.as_str(), "Bar");
            match declares.get_type(*base) {
                Some(SolType::Stub(stub)) => {
                    assert!(stub.matches_ignoring_occurrence(&Stub {
                        name: "Foo".into(),
                        generics: vec![].into(),
                        occurrence: NodeId::ERROR,
                    }))
                }
                other => panic!("expected a Stub named Foo, got {other:?}"),
            }
        }
        other => panic!("expected NamedVariant, got {:?}", other),
    }
}
