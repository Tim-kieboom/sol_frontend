use std::path::PathBuf;

use ast_model::AstTree;
use ast_run::{AstRequest, to_ast};
use mir_parser::fault::MirErrorKind;
use soul_tokenizer::to_token_stream;
use soul_utils::{
    CrateContext,
    collections::{benchmark::Benchmark, crate_store::CrateStore, module_store::ModuleStore},
    compiler_options::{CompilerOptions, MirOptions},
    fault::Severity,
};

use crate::MirProgram;

fn create_mir(ast: &mut AstTree) -> (MirProgram, CrateContext<MirErrorKind>) {
    let mut benchmark = Benchmark::new();
    create_mir_with_benchmark(ast, &mut benchmark)
}

fn create_mir_with_benchmark(
    ast: &mut AstTree,
    benchmark: &mut Benchmark,
) -> (MirProgram, CrateContext<MirErrorKind>) {
    let mut context = CrateContext::default();
    let mir = crate::to_mir(
        ast,
        benchmark,
        &mut context,
        &CompilerOptions::const_default(),
    );
    (mir, context)
}

fn build_ast(source: &str) -> AstTree {
    let mut module_store = ModuleStore::new();
    module_store.insert_root(PathBuf::from("test.soul"));
    let root = module_store.get_root_id();
    let crate_store = CrateStore::new();

    let tokens = to_token_stream(source, root).expect("test source failed to tokenize");
    let mut benchmark = Benchmark::new();

    let mir = MirOptions::empty()
        .add(MirOptions::CHECK_ALGORITHMIC_OVERFLOW)
        .add(MirOptions::CHECK_INDEX_OUT_OF_BOUNDS);

    let options = CompilerOptions {
        mir,
        ..CompilerOptions::const_default()
    };

    let ast = to_ast(
        tokens,
        AstRequest {
            source_folder: PathBuf::from("."),
            benchmark: &mut benchmark,
            module_store: &mut module_store,
            crate_store: &crate_store,
        },
        &options,
    );
    assert_eq!(
        ast.faults().iter().count(),
        0,
        "test source failed to resolve: {:#?}",
        ast.faults()
    );
    ast
}

#[test]
fn lowers_every_lowerable_function_with_no_errors() {
    let mut ast = build_ast("add(a: int, b: int): int {\n    c := a + b\n    return c\n}\n");
    let mut benchmark = Benchmark::new();

    let (program, context) = create_mir_with_benchmark(&mut ast, &mut benchmark);

    assert_eq!(program.functions.entries().count(), 1);
    assert_eq!(
        context.faults.iter().count(),
        0,
        "expected no lowering faults: {:#?}",
        context.faults.iter().collect::<Vec<_>>()
    );
    assert!(
        benchmark.iter().any(|(name, _)| name == "mir"),
        "expected a `mir` benchmark entry to be recorded"
    );
}

#[test]
fn out_of_scope_function_pushes_a_fault_into_the_context_not_a_panic() {
    // Struct-typed and fixed-size-array/slice-typed params/locals are
    // supported now (see `mir_parser`'s own struct and array tests) — a
    // heap-array type isolates a genuine non-primitive type this lowering
    // slice still doesn't support.
    let mut ast = build_ast("f(a: []int): []int {\n    return a\n}\n");

    let (program, context) = create_mir(&mut ast);

    assert_eq!(program.functions.entries().count(), 0);
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        1,
        "{:#?}",
        context.faults.iter().collect::<Vec<_>>()
    );
    assert!(
        context
            .faults
            .iter()
            .any(|fault| matches!(fault.kind(), MirErrorKind::NonPrimitiveType { .. })),
        "{:#?}",
        context.faults.iter().collect::<Vec<_>>()
    );
}

#[test]
fn extern_c_function_lowers_into_externs_not_functions() {
    let mut ast = build_ast(r#"extern "C" printf(fmt: &char): int"#);

    let (program, context) = create_mir(&mut ast);

    assert_eq!(
        program.functions.entries().count(),
        0,
        "an extern declaration has no body — it must not end up in `functions`"
    );
    assert_eq!(
        program.externs.entries().count(),
        1,
        "expected the extern declaration to be lowered into `externs`"
    );
    assert_eq!(
        context.faults.iter().count(),
        0,
        "a supported extern \"C\" declaration shouldn't fault: {:#?}",
        context.faults.iter().collect::<Vec<_>>()
    );
}

#[test]
fn use_after_move_in_straight_line_code_faults_through_the_full_pipeline() {
    // Proves the wiring in `crate::to_mir` itself — not just
    // `mir_parser::move_check::check_moves` in isolation — actually turns a
    // real use-after-move `.soul` program into a hard error.
    let mut ast = build_ast(
        "struct Session {\n    n: int\n}\nconsume(s: Session) {}\nf() {\n    s := Session{n: 1}\n    consume(s)\n    consume(s)\n}\n",
    );

    let (_, context) = create_mir(&mut ast);

    assert_eq!(
        context.faults.count_severity(Severity::Error),
        1,
        "{:#?}",
        context.faults.iter().collect::<Vec<_>>()
    );
    assert!(
        context
            .faults
            .iter()
            .any(|fault| matches!(fault.kind(), MirErrorKind::UseAfterMove)),
        "{:#?}",
        context.faults.iter().collect::<Vec<_>>()
    );
}

#[test]
fn an_if_branch_with_a_move_violation_is_flagged_through_the_full_pipeline() {
    // Proves the wiring, not just `move_check::check_moves` in isolation: an
    // `if`/`else` is now real CFG dataflow, not skipped — `s` is
    // double-moved inside the `if` arm alone.
    let mut ast = build_ast(
        "struct Session {\n    n: int\n}\nconsume(s: Session) {}\nf(cond: bool) {\n    s := Session{n: 1}\n    if cond {\n        consume(s)\n        consume(s)\n    }\n}\n",
    );

    let (_, context) = create_mir(&mut ast);

    assert_eq!(
        context.faults.count_severity(Severity::Error),
        1,
        "{:#?}",
        context.faults.iter().collect::<Vec<_>>()
    );
    assert!(
        context
            .faults
            .iter()
            .any(|fault| matches!(fault.kind(), MirErrorKind::UseAfterMove)),
        "{:#?}",
        context.faults.iter().collect::<Vec<_>>()
    );
}

#[test]
fn a_looping_function_reusing_a_moved_value_is_flagged_through_the_full_pipeline() {
    // Proves the wiring for the fixed-point-over-a-cyclic-CFG slice, not
    // just `move_check::check_moves` in isolation: `s` is moved inside the
    // loop body and reused on every later iteration.
    let mut ast = build_ast(
        "struct Session {\n    n: int\n}\nconsume(s: Session) {}\nf(cond: bool) {\n    s := Session{n: 1}\n    for cond {\n        consume(s)\n    }\n}\n",
    );

    let (_, context) = create_mir(&mut ast);

    assert_eq!(
        context.faults.count_severity(Severity::Error),
        1,
        "{:#?}",
        context.faults.iter().collect::<Vec<_>>()
    );
    assert!(
        context
            .faults
            .iter()
            .any(|fault| matches!(fault.kind(), MirErrorKind::UseAfterMove)),
        "{:#?}",
        context.faults.iter().collect::<Vec<_>>()
    );
}

#[test]
fn a_dangling_reference_return_is_flagged_through_the_full_pipeline() {
    // Proves the wiring for `mir_parser::escape_check`, not just
    // `check_escapes` in isolation: `f` returns a reference to its own
    // body-local `x`, routed through an intermediate local `p`.
    let mut ast = build_ast("f(): &int {\n    x := 1\n    p := &x\n    return p\n}\n");

    let (_, context) = create_mir(&mut ast);

    assert_eq!(
        context.faults.count_severity(Severity::Error),
        1,
        "{:#?}",
        context.faults.iter().collect::<Vec<_>>()
    );
    assert!(
        context
            .faults
            .iter()
            .any(|fault| matches!(fault.kind(), MirErrorKind::DanglingReference)),
        "{:#?}",
        context.faults.iter().collect::<Vec<_>>()
    );
}

#[test]
fn an_overlapping_mutable_borrow_is_flagged_through_the_full_pipeline() {
    // Proves the wiring for `mir_parser::borrow_checker::overlap_check`, not
    // just `check_borrow_overlaps` in isolation: `mutRef` and `ref` both
    // borrow `var`, and their live ranges overlap.
    let mut ast = build_ast(
        "struct Obj {}\nuseRef(r: &Obj) {}\nuseMut(r: &mut Obj) {}\nf() {\n    mut var := Obj{}\n    mutRef := &mut var\n    ref := &var\n    useMut(mutRef)\n    useRef(ref)\n}\n",
    );

    let (_, context) = create_mir(&mut ast);

    assert_eq!(
        context.faults.count_severity(Severity::Error),
        1,
        "{:#?}",
        context.faults.iter().collect::<Vec<_>>()
    );
    assert!(
        context
            .faults
            .iter()
            .any(|fault| matches!(fault.kind(), MirErrorKind::OverlappingBorrows)),
        "{:#?}",
        context.faults.iter().collect::<Vec<_>>()
    );
}

#[test]
fn mixed_program_lowers_what_it_can_and_faults_on_the_rest() {
    let mut ast = build_ast(
        "okFn(a: int, b: int): int {\n    return a + b\n}\nbadFn(a: []int): []int {\n    return a\n}\n",
    );

    let (program, context) = create_mir(&mut ast);

    assert_eq!(program.functions.entries().count(), 1);
    assert_eq!(
        context.faults.count_severity(Severity::Error),
        1,
        "{:#?}",
        context.faults.iter().collect::<Vec<_>>()
    );
    assert!(
        context
            .faults
            .iter()
            .any(|fault| matches!(fault.kind(), MirErrorKind::NonPrimitiveType { .. })),
        "{:#?}",
        context.faults.iter().collect::<Vec<_>>()
    );
}
