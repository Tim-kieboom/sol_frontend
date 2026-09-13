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
fn conforming_impl_resolves_without_faults() {
    let ast = resolve_source(
        "trait Greeter {\n    greet(&this): i64\n}\nstruct Bar {\n    n: i64\n}\nuse Bar {\n    impl Greeter {\n        greet(&this): i64 => this.n\n    }\n}\nmain() {\n    b: Bar = Bar{n: 5}\n    x := b.greet()\n}\n",
    );
    assert_eq!(ast.faults().iter().count(), 0);
}

#[test]
fn impl_missing_a_trait_method_reports_exactly_one_fault() {
    let ast = resolve_source(
        "trait Greeter {\n    greet(&this): i64\n    farewell(&this): i64\n}\nstruct Bar {\n    n: i64\n}\nuse Bar {\n    impl Greeter {\n        greet(&this): i64 => this.n\n    }\n}\n",
    );
    assert_eq!(
        fault_count_matching(&ast, |k| matches!(
            k,
            AstErrorKind::ImplMissingTraitMethod { .. }
        )),
        1
    );
}

#[test]
fn impl_with_a_method_the_trait_does_not_declare_reports_exactly_one_fault() {
    let ast = resolve_source(
        "trait Greeter {\n    greet(&this): i64\n}\nstruct Bar {\n    n: i64\n}\nuse Bar {\n    impl Greeter {\n        greet(&this): i64 => this.n\n        farewell(&this): i64 => this.n\n    }\n}\n",
    );
    assert_eq!(
        fault_count_matching(&ast, |k| matches!(
            k,
            AstErrorKind::ImplHasExtraTraitMethod { .. }
        )),
        1
    );
}

#[test]
fn impl_method_with_mismatched_return_type_reports_exactly_one_fault() {
    let ast = resolve_source(
        "trait Greeter {\n    greet(&this): i64\n}\nstruct Bar {\n    n: i64\n}\nuse Bar {\n    impl Greeter {\n        greet(&this): none => {}\n    }\n}\n",
    );
    assert_eq!(
        fault_count_matching(&ast, |k| matches!(
            k,
            AstErrorKind::ImplTraitMethodSignatureMismatch { .. }
        )),
        1
    );
}

#[test]
fn impl_of_an_undeclared_trait_name_reports_exactly_one_fault() {
    let ast = resolve_source(
        "struct Bar {\n    n: i64\n}\nuse Bar {\n    impl NotATrait {\n        greet(&this): i64 => this.n\n    }\n}\n",
    );
    assert_eq!(
        fault_count_matching(&ast, |k| matches!(
            k,
            AstErrorKind::ImplTargetIsNotATrait { .. }
        )),
        1
    );
}

#[test]
fn calling_a_same_named_method_from_two_trait_impls_is_ambiguous() {
    let ast = resolve_source(
        "trait A {\n    foo(&this): i64\n}\ntrait B {\n    foo(&this): i64\n}\nstruct Bar {\n    n: i64\n}\nuse Bar {\n    impl A {\n        foo(&this): i64 => this.n\n    }\n}\nuse Bar {\n    impl B {\n        foo(&this): i64 => this.n\n    }\n}\nmain() {\n    b: Bar = Bar{n: 1}\n    x := b.foo()\n}\n",
    );
    assert_eq!(
        fault_count_matching(&ast, |k| matches!(
            k,
            AstErrorKind::AmbiguousMethodCall { .. }
        )),
        1
    );
}
