use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

use ast_model::AstTree;
use ast_parser::{ParseInfo, fault::AstErrorKind, parse_module};
use sol_tokenizer::to_token_stream;
use sol_utils::collections::{crate_store::CrateStore, module_store::ModuleStore};

use crate::{collect::tests::fault_count_matching, name_resolve};

static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn make_temp_dir(tag: &str) -> PathBuf {
    let unique = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "sol_import_tests_{tag}_{}_{unique}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("failed to create temp dir");
    dir
}

fn write_module(dir: &Path, name: &str, content: &str) {
    fs::write(dir.join(format!("{name}.sol")), content).expect("failed to write test module");
}

fn resolve_in_dir(dir: &Path, source: &str) -> AstTree {
    let mut module_store = ModuleStore::new();
    module_store.insert_root(PathBuf::from("root.sol"));
    let root = module_store.get_root_id();
    let crate_store = CrateStore::new();

    let tokens = to_token_stream(source, root).expect("test source failed to tokenize");

    let mut ast = AstTree::new(root);
    let info = ParseInfo {
        id: root,
        source_folder: dir.to_path_buf(),
        crate_source_folder: dir.to_path_buf(),
        parent: None,
        modules: &mut module_store,
        context: &mut ast.context,
        forest: &mut ast.crates,
        declares: &mut ast.declares,
        crate_store: &crate_store,
    };
    parse_module(tokens, "crate".to_string(), info);

    name_resolve(&mut module_store, &mut ast, &crate_store);
    ast
}

#[test]
fn importing_a_missing_internal_module_reports_a_fault() {
    let dir = make_temp_dir("missing_module");
    let ast = resolve_in_dir(&dir, "import .missing\n");
    assert!(
        fault_count_matching(&ast, |kind| matches!(
            kind,
            AstErrorKind::ImportedModuleNotFound { .. }
        )) > 0,
        "{:#?}",
        ast.faults()
    );
}

#[test]
fn importing_an_unexported_name_reports_a_fault() {
    let dir = make_temp_dir("unexported_name");
    write_module(&dir, "dep", "pub greet() {}\n");
    let ast = resolve_in_dir(&dir, "import .dep { missingName }\n");
    assert_eq!(
        fault_count_matching(&ast, |kind| matches!(
            kind,
            AstErrorKind::ModuleDoesNotExportItem { .. }
        )),
        1,
        "{:#?}",
        ast.faults()
    );
}

#[test]
fn importing_a_private_function_reports_a_fault() {
    let dir = make_temp_dir("private_function");
    write_module(&dir, "dep", "secret() {}\n");
    let ast = resolve_in_dir(&dir, "import .dep { secret }\n");
    assert_eq!(
        fault_count_matching(&ast, |kind| matches!(
            kind,
            AstErrorKind::ItemIsPrivate { .. }
        )),
        1,
        "{:#?}",
        ast.faults()
    );
}

#[test]
fn importing_a_public_function_reports_no_privacy_fault() {
    let dir = make_temp_dir("public_function");
    write_module(&dir, "dep", "pub greet() {}\n");
    let ast = resolve_in_dir(&dir, "import .dep { greet }\n");
    assert_eq!(
        fault_count_matching(&ast, |kind| matches!(
            kind,
            AstErrorKind::ItemIsPrivate { .. }
        )),
        0,
        "{:#?}",
        ast.faults()
    );
}

#[test]
fn importing_the_same_item_twice_reports_exactly_one_fault() {
    let dir = make_temp_dir("duplicate_item");
    write_module(&dir, "dep", "pub greet() {}\n");
    let ast = resolve_in_dir(&dir, "import .dep { greet, greet }\n");
    assert_eq!(
        fault_count_matching(&ast, |kind| matches!(
            kind,
            AstErrorKind::ItemAliasAlreadyExists { .. }
        )),
        1,
        "{:#?}",
        ast.faults()
    );
}

#[test]
fn calling_a_function_a_module_does_not_have_reports_a_fault() {
    let dir = make_temp_dir("module_missing_function");
    write_module(&dir, "dep", "pub greet() {}\n");
    let ast = resolve_in_dir(
        &dir,
        "import .dep\nmain() {\n    dep.thisFunctionDoesNotExist()\n}\n",
    );
    assert_eq!(
        fault_count_matching(&ast, |kind| matches!(
            kind,
            AstErrorKind::FunctionNotFoundIn { .. }
        )),
        1,
        "{:#?}",
        ast.faults()
    );
}

#[test]
fn calling_a_function_a_module_does_have_reports_no_fault() {
    let dir = make_temp_dir("module_has_function");
    write_module(&dir, "dep", "pub greet() {}\n");
    let ast = resolve_in_dir(&dir, "import .dep\nmain() {\n    dep.greet()\n}\n");
    assert_eq!(
        fault_count_matching(&ast, |kind| matches!(
            kind,
            AstErrorKind::FunctionNotFoundIn { .. }
        )),
        0,
        "{:#?}",
        ast.faults()
    );
}

#[test]
fn importing_from_an_unregistered_external_crate_reports_a_fault() {
    let dir = make_temp_dir("unregistered_crate");
    let ast = resolve_in_dir(&dir, "import missingcrate.thing\n");
    assert!(
        fault_count_matching(&ast, |kind| matches!(
            kind,
            AstErrorKind::ExternalCrateNotFound { .. }
        )) > 0,
        "{:#?}",
        ast.faults()
    );
}
