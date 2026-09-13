use std::path::PathBuf;

use ast_model::AstTree;
use ast_parser::{ParseInfo, fault::AstErrorKind, parse_module};
use soul_tokenizer::to_token_stream;
use soul_utils::collections::{crate_store::CrateStore, module_store::ModuleStore};

use crate::name_resolve;

fn resolve_source(source: &str) -> AstTree {
    let mut module_store = ModuleStore::new();
    module_store.insert_root(PathBuf::from("test.soul"));
    let root = module_store.get_root_id();
    let crate_store = CrateStore::new();

    let tokens = to_token_stream(source, root).expect("test source failed to tokenize");

    let mut ast = AstTree::new(root);
    let info = ParseInfo {
        id: root,
        source_folder: PathBuf::from("."),
        crate_source_folder: PathBuf::from("."),
        parent: None,
        modules: &mut module_store,
        context: &mut ast.context,
        forest: &mut ast.crates,
        crate_store: &crate_store,
    };
    parse_module(tokens, "crate".to_string(), info);

    name_resolve(&mut module_store, &mut ast, &crate_store);
    ast
}

fn fault_count_matching(ast: &AstTree, predicate: impl Fn(&AstErrorKind) -> bool) -> usize {
    ast.faults()
        .iter()
        .filter(|fault| predicate(fault.kind()))
        .count()
}

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
