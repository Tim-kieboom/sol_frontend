use std::path::PathBuf;

use ast_model::{
    ArrayKind, ArrayType, AstStore, AstTree, FunctionKind, ReferenceType, SolType, TypeId,
    declare_store::DeclareStore,
};
use ast_parser::{ParseInfo, parse_module};
use mir_model::{ConstValue, Operand, Rvalue};
use sol_resolver::name_resolve;
use sol_tokenizer::to_token_stream;
use sol_utils::{
    FunctionId, Mutable,
    collections::{
        crate_store::CrateStore, module_store::ModuleStore, vec_map::VecMap, vec_set::VecSet,
    },
    compiler_options::{CompilerOptions, MirOptions},
    sol_names::PrimitiveTypes,
};

use crate::{
    MirLowerer,
    fault::{MirErrorKind, MirResult},
    function::FunctionLowerer,
};

// Tests exercise the checked-arithmetic/bounds-check MIR shapes, so run with
// every `MirOptions` flag on — matching `mir_run`'s own test fixtures, and
// unlike `sol_tester`'s production config, which currently defaults both
// off (see `sol_tester::config::COMPILER_OPTIONS`).
const OPTIONS: CompilerOptions = CompilerOptions {
    mir: MirOptions::all(),
    ..CompilerOptions::const_default()
};
fn create_lowerer(ast: &mut AstTree) -> MirLowerer<'_> {
    MirLowerer::new(&ast.crates.store, &mut ast.declares, &OPTIONS)
}

fn resolve_source(source: &str) -> AstTree {
    let mut module_store = ModuleStore::new();
    module_store.insert_root(PathBuf::from("test.sol"));
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
        declares: &mut ast.declares,
        crate_store: &crate_store,
    };
    parse_module(tokens, "crate".to_string(), info);

    name_resolve(&mut module_store, &mut ast, &crate_store);
    assert_eq!(
        ast.faults().iter().count(),
        0,
        "test source failed to resolve: {:#?}",
        ast.faults()
    );
    ast
}

fn find_function(store: &AstStore, name: &str) -> FunctionId {
    store
        .functions
        .entries()
        .find_map(|(id, kind)| match kind {
            FunctionKind::Normal(function) if function.signature.value.name.as_str() == name => {
                Some(id)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("no function named `{name}` found"))
}

fn lower_function(
    store: &AstStore,
    declares: &mut DeclareStore,
    id: FunctionId,
) -> MirResult<mir_model::Function> {
    let mut lowerer = MirLowerer::new(store, declares, &OPTIONS);
    lowerer.lower_function(id)?;
    Ok(lowerer.functions.into_values().next().unwrap())
}

fn lower_source(source: &str, function_name: &str) -> MirResult<mir_model::Function> {
    let mut ast = resolve_source(source);
    let function_id = find_function(&ast.crates.store, function_name);
    lower_function(&ast.crates.store, &mut ast.declares, function_id)
}

/// Like `lower_source`, but also hands back the `DeclareStore` — needed by
/// anything (like `escape_check`) that has to resolve a `TypeId` back to a
/// `SolType` after lowering.
fn lower_source_with_declares(
    source: &str,
    function_name: &str,
) -> (mir_model::Function, DeclareStore) {
    let mut ast = resolve_source(source);
    let function_id = find_function(&ast.crates.store, function_name);
    let mir = lower_function(&ast.crates.store, &mut ast.declares, function_id)
        .expect("expected successful lowering");
    (mir, ast.declares)
}

/// `check_escapes` is now whole-program (interprocedural — see that
/// module's own docs), so every call site needs a `VecMap` even for a
/// single function's own test.
fn function_map(function: mir_model::Function) -> VecMap<FunctionId, mir_model::Function> {
    std::iter::once((function.id, function)).collect()
}

/// Lowers every function declared in `source` (not just one by name) — for
/// interprocedural `check_escapes` tests, which need more than one function
/// in the same `VecMap` to exercise a real call graph.
fn lower_all_with_declares(
    source: &str,
) -> (VecMap<FunctionId, mir_model::Function>, DeclareStore) {
    let mut ast = resolve_source(source);
    let ids: Vec<FunctionId> = ast
        .crates
        .store
        .functions
        .entries()
        .map(|(id, _)| id)
        .collect();
    let functions = ids
        .into_iter()
        .map(|id| {
            let function = lower_function(&ast.crates.store, &mut ast.declares, id)
                .expect("expected successful lowering");
            (function.id, function)
        })
        .collect();
    (functions, ast.declares)
}

fn assert_rejected_matching(
    result: &MirResult<mir_model::Function>,
    predicate: impl Fn(&MirErrorKind) -> bool,
) {
    let Err(fault) = result else {
        panic!("expected lowering to fail, got {:#?}", result.as_ref().ok());
    };
    assert!(
        predicate(fault.kind()),
        "unexpected fault kind: {:?}",
        fault.kind()
    );
    assert!(
        fault.span().is_some(),
        "expected the fault to carry a span, got {fault:#?}"
    );
}

fn assert_rejected_with(result: &MirResult<mir_model::Function>, expected: MirErrorKind) {
    assert_rejected_matching(result, |kind| *kind == expected);
}

#[test]
fn lowers_arithmetic_with_a_variable_and_a_return() {
    let mir = lower_source(
        "add(a: int, b: int): int {\n    c := a + b\n    return c\n}\n",
        "add",
    )
    .expect("expected successful lowering");

    assert_eq!(mir.arg_count, 2);
    // `a + b` now traps on overflow (see `lower_checked_binary_op`), which
    // splits the function into an arithmetic block and a continuation; `c`
    // is a body-declared local, so falling off the end of the continuation
    // now also goes through `seal_return`'s scope-exit `Drop(c)` chain
    // (mirrors `docs/mir-design.md`'s own `add(a, b)` worked example) —
    // three blocks total, not two.
    assert_eq!(mir.blocks.entries().count(), 3, "{:#?}", mir.blocks);

    let mut blocks = mir.blocks.entries();
    let (_, entry) = blocks.next().expect("expected an entry block");
    assert_eq!(entry.statements.len(), 1, "{:#?}", entry.statements);

    let mir_model::Statement::Assign(tuple_place, Rvalue::CheckedBinaryOp(op, left, right)) =
        &entry.statements[0]
    else {
        panic!("expected first statement to assign a CheckedBinaryOp");
    };
    assert_eq!(*op, ast_model::operators::BinaryOperatorKind::Add);
    assert!(matches!(left, Operand::Copy(_)));
    assert!(matches!(right, Operand::Copy(_)));

    let mir_model::Terminator::Assert {
        cond: Operand::Copy(overflow_place),
        expected: false,
        target,
        ..
    } = &entry.terminator
    else {
        panic!("expected the entry block to end in an overflow Assert");
    };
    assert_eq!(overflow_place.local, tuple_place.local);
    assert!(matches!(
        overflow_place.projection.as_slice(),
        [mir_model::PlaceElem::Field(1)]
    ));

    let (ok_id, ok_block) = blocks.next().expect("expected a continuation block");
    assert_eq!(*target, ok_id);
    // `c`'s own declaration (`Assign` + `SetDropFlag`, since `c` is tracked),
    // then `return c`'s copy into the return local (no `SetDropFlag`: the
    // return local is never tracked — see `push_assign`'s docs).
    assert_eq!(ok_block.statements.len(), 3, "{:#?}", ok_block.statements);
    let mir_model::Statement::Assign(c_place, Rvalue::Use(Operand::Copy(_))) =
        &ok_block.statements[0]
    else {
        panic!(
            "expected c's own declaration assign, got {:#?}",
            ok_block.statements[0]
        );
    };
    assert!(matches!(
        &ok_block.statements[1],
        mir_model::Statement::SetDropFlag(local, true) if *local == c_place.local
    ));
    assert!(matches!(
        &ok_block.statements[2],
        mir_model::Statement::Assign(place, Rvalue::Use(Operand::Copy(_)))
        if Some(place.local) == mir.return_local
    ));

    let mir_model::Terminator::Drop {
        place: dropped,
        target: drop_target,
        ..
    } = &ok_block.terminator
    else {
        panic!(
            "expected the continuation to end in Drop(c), got {:#?}",
            ok_block.terminator
        );
    };
    assert_eq!(dropped.local, c_place.local);

    let (final_id, final_block) = blocks.next().expect("expected a final block");
    assert_eq!(*drop_target, final_id);
    assert!(final_block.statements.is_empty());
    assert!(matches!(
        final_block.terminator,
        mir_model::Terminator::Return
    ));
}

#[test]
fn lowers_a_bare_literal_return() {
    let mir =
        lower_source("answer(): int {\n    return 42\n}\n", "answer").expect("expected success");

    let (_, block) = mir.blocks.entries().next().unwrap();
    assert_eq!(block.statements.len(), 1);
    let mir_model::Statement::Assign(_, Rvalue::Use(Operand::Constant(ConstValue::Uint(42)))) =
        &block.statements[0]
    else {
        panic!(
            "expected a constant assignment, got {:#?}",
            block.statements[0]
        );
    };
}

#[test]
fn missing_return_is_rejected() {
    let result = lower_source("f(): int {\n    x := 1\n}\n", "f");
    assert_rejected_with(&result, MirErrorKind::MissingReturnStatement);
}

#[test]
fn non_primitive_return_type_is_rejected() {
    // `f() { .. }` with no declared return type is a `none`-returning
    // function, which is now valid (see `none_returning_function_...` tests
    // below) — a heap-array return type isolates a genuine non-primitive
    // type still out of scope (structs and fixed-size arrays/slices are now
    // accepted, see the struct and array tests below).
    let result = lower_source("f(a: []int): []int {\n    return a\n}\n", "f");
    assert_rejected_matching(&result, |kind| {
        matches!(kind, MirErrorKind::NonPrimitiveType { .. })
    });
}

#[test]
fn destructuring_variable_pattern_is_rejected() {
    let result = lower_source(
        "get_pair(): (int, int) {\n    return .(1, 2)\n}\nf(): int {\n    (a, b) := get_pair()\n    return a\n}\n",
        "f",
    );
    assert_rejected_with(&result, MirErrorKind::NonSimpleVariablePatternUnsupported);
}

#[test]
fn struct_field_read_lowers_to_a_field_projection() {
    let mir = lower_source(
        "struct Point { x: int }\nf(p: Point): int {\n    return p.x\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    // param `p` + the `int` return local — reading `p.x` reuses `p`'s own
    // storage via a `Field` projection, no extra temp local.
    assert_eq!(mir.locals.entries().count(), 2);

    let (_, block) = mir.blocks.entries().next().expect("expected one block");
    let mir_model::Statement::Assign(_, Rvalue::Use(Operand::Copy(place))) = &block.statements[0]
    else {
        panic!(
            "expected a bare Use(Copy(..)) assignment, got {:#?}",
            block.statements[0]
        );
    };
    assert!(
        matches!(
            place.projection.as_slice(),
            [mir_model::PlaceElem::Field(0)]
        ),
        "expected a single Field(0) projection, got {:#?}",
        place.projection
    );
}

#[test]
fn nested_struct_field_read_lowers_to_a_single_place_with_two_projections() {
    let mir = lower_source(
        "struct Inner { x: int }\nstruct Outer { inner: Inner }\nf(o: Outer): int {\n    return o.inner.x\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    // param `o` + the `int` return local — `o.inner.x` reuses `o`'s own
    // storage via a two-element `Field` projection, no temp for `o.inner`.
    assert_eq!(mir.locals.entries().count(), 2);

    let (_, block) = mir.blocks.entries().next().expect("expected one block");
    let mir_model::Statement::Assign(_, Rvalue::Use(Operand::Copy(place))) = &block.statements[0]
    else {
        panic!(
            "expected a bare Use(Copy(..)) assignment, got {:#?}",
            block.statements[0]
        );
    };
    assert!(
        matches!(
            place.projection.as_slice(),
            [
                mir_model::PlaceElem::Field(0),
                mir_model::PlaceElem::Field(0)
            ]
        ),
        "expected a two-element Field(0), Field(0) projection, got {:#?}",
        place.projection
    );
}

#[test]
fn nested_struct_field_write_lowers_to_an_assign_through_two_projections() {
    let mir = lower_source(
        "struct Inner { x: int }\nstruct Outer { inner: Inner }\nf(mut o: Outer): int {\n    o.inner.x = 5\n    return o.inner.x\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let (_, block) = mir.blocks.entries().next().expect("expected one block");
    let mir_model::Statement::Assign(place, Rvalue::Use(Operand::Constant(ConstValue::Uint(5)))) =
        &block.statements[0]
    else {
        panic!(
            "expected the first statement to assign a constant, got {:#?}",
            block.statements[0]
        );
    };
    assert!(
        matches!(
            place.projection.as_slice(),
            [
                mir_model::PlaceElem::Field(0),
                mir_model::PlaceElem::Field(0)
            ]
        ),
        "expected a two-element Field(0), Field(0) projection, got {:#?}",
        place.projection
    );
}

#[test]
fn struct_constructor_lowers_to_an_aggregate_in_declared_field_order() {
    let mir = lower_source(
        "struct Point {\n    x: int\n    y: int\n}\nf(): int {\n    p: Point = Point{y: 2, x: 1}\n    return p.x\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let (_, block) = mir.blocks.entries().next().expect("expected one block");
    let mir_model::Statement::Assign(_, Rvalue::Aggregate(kind, operands)) = &block.statements[0]
    else {
        panic!(
            "expected the first statement to construct an Aggregate, got {:#?}",
            block.statements[0]
        );
    };
    assert!(matches!(kind, mir_model::AggregateKind::Struct));
    // Field-literal order was `y, x`; the aggregate's operands must follow
    // the struct's *declared* order (`x, y`) instead, since that's what the
    // `Field(usize)` projections used to read it back index into.
    assert_eq!(operands.len(), 2);
    assert!(matches!(
        &operands[0],
        Operand::Constant(ConstValue::Uint(1))
    ));
    assert!(matches!(
        &operands[1],
        Operand::Constant(ConstValue::Uint(2))
    ));
}

#[test]
fn untyped_struct_constructor_declaration_infers_its_type_and_lowers_successfully() {
    // Unlike the sibling test above (`p: Point = ..`, an explicit
    // annotation), this is `:=` with no annotation at all — the type has to
    // come from `expression_type`'s `StructConstructor` arm backfilling it,
    // not from a declared type. Previously `expression_type` had no
    // `StructConstructor` arm at all, so this panicked mid-lowering with
    // "variable has no resolved type" instead of erroring cleanly, let alone
    // succeeding.
    let mir = lower_source(
        "struct Point {\n    x: int\n    y: int\n}\nf(): int {\n    p := Point{x: 1, y: 2}\n    return p.x\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let (_, block) = mir.blocks.entries().next().expect("expected one block");
    assert!(
        matches!(
            &block.statements[0],
            mir_model::Statement::Assign(_, Rvalue::Aggregate(mir_model::AggregateKind::Struct, _))
        ),
        "{:#?}",
        block.statements[0]
    );
}

#[test]
fn array_literal_lowers_to_an_aggregate_in_literal_order() {
    // `a` is only constructed here, not indexed — a fixed-size array can't
    // be indexed directly in this slice (see
    // `indexing_a_fixed_size_array_directly_is_rejected` below); it has to
    // go through a slice first.
    let mir = lower_source("f(): int {\n    a: [2]int = [1, 2]\n    return 0\n}\n", "f")
        .expect("expected successful lowering");

    let (_, block) = mir.blocks.entries().next().expect("expected one block");
    let mir_model::Statement::Assign(_, Rvalue::Aggregate(kind, operands)) = &block.statements[0]
    else {
        panic!(
            "expected the first statement to construct an Aggregate, got {:#?}",
            block.statements[0]
        );
    };
    assert!(matches!(kind, mir_model::AggregateKind::Array));
    assert_eq!(operands.len(), 2);
    assert!(matches!(
        &operands[0],
        Operand::Constant(ConstValue::Uint(1))
    ));
    assert!(matches!(
        &operands[1],
        Operand::Constant(ConstValue::Uint(2))
    ));
}

#[test]
fn array_reference_lowers_to_a_ref_plus_fat_pointer_aggregate() {
    let mir = lower_source(
        "f(): int {\n    a: [2]int = [1, 2]\n    s: [&]int = &a\n    return s[0]\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let (_, block) = mir.blocks.entries().next().expect("expected one block");
    // statements[0]: `a`'s Aggregate construction (checked above); [1]: its
    // `SetDropFlag` (`a` is a body-declared local); [2]: the Ref-producing
    // temp `&a` builds (a compiler-internal temp, never tracked, so no
    // `SetDropFlag` of its own); [3]: the `{ptr, len}` Aggregate for `s`.
    let mir_model::Statement::Assign(_, Rvalue::Ref { place, .. }) = &block.statements[2] else {
        panic!(
            "expected the third statement to build a bare Ref, got {:#?}",
            block.statements[2]
        );
    };
    assert!(
        place.projection.is_empty(),
        "expected a Ref straight at `a`'s own local, got {:#?}",
        place.projection
    );

    let mir_model::Statement::Assign(_, Rvalue::Aggregate(kind, operands)) = &block.statements[3]
    else {
        panic!(
            "expected the fourth statement to construct the slice Aggregate, got {:#?}",
            block.statements[3]
        );
    };
    assert!(matches!(kind, mir_model::AggregateKind::Slice));
    assert_eq!(operands.len(), 2);
    assert!(matches!(&operands[0], Operand::Copy(_)));
    assert!(matches!(
        &operands[1],
        Operand::Constant(ConstValue::Uint(2))
    ));
}

#[test]
fn deref_read_lowers_to_a_place_with_a_deref_projection() {
    let mir = lower_source(
        "f(): int {\n    x: int = 5\n    p := &x\n    return *p\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let (_, block) = mir.blocks.entries().next().expect("expected one block");
    let mir_model::Statement::Assign(return_place, Rvalue::Use(Operand::Copy(deref_place))) = block
        .statements
        .last()
        .expect("expected at least one statement")
    else {
        panic!(
            "expected the last statement to Copy through a Deref place, got {:#?}",
            block.statements
        );
    };
    assert_eq!(Some(return_place.local), mir.return_local);
    assert!(
        matches!(
            deref_place.projection.as_slice(),
            [mir_model::PlaceElem::Deref]
        ),
        "expected a single Deref projection, got {:#?}",
        deref_place.projection
    );
}

#[test]
fn deref_write_lowers_to_an_assign_through_a_deref_projection() {
    let mir = lower_source(
        "f(): int {\n    mut x: int = 5\n    p := &mut x\n    *p = 9\n    return x\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let all_statements: Vec<&mir_model::Statement> = mir
        .blocks
        .entries()
        .flat_map(|(_, block)| &block.statements)
        .collect();
    let deref_write = all_statements.iter().find_map(|statement| {
        let mir_model::Statement::Assign(
            place,
            Rvalue::Use(Operand::Constant(ConstValue::Uint(9))),
        ) = statement
        else {
            return None;
        };
        matches!(place.projection.as_slice(), [mir_model::PlaceElem::Deref]).then_some(place)
    });
    assert!(
        deref_write.is_some(),
        "expected a `*p = 9` write through a Deref place, got {:#?}",
        all_statements
    );
}

#[test]
fn dereferencing_a_non_reference_is_rejected() {
    let result = lower_source("f(): int {\n    x: int = 5\n    return *x\n}\n", "f");
    assert_rejected_matching(&result, |kind| {
        matches!(kind, MirErrorKind::DerefTargetNotAReference { .. })
    });
}

#[test]
fn mut_this_receiver_is_passed_as_a_mutable_reference() {
    let mir = lower_source(
        "struct Counter {\n    n: int\n    increment(&mut this) {\n        this.n = this.n + 1\n    }\n}\nf(): int {\n    mut c: Counter = Counter{n: 0}\n    c.increment()\n    return c.n\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let arguments = mir
        .blocks
        .entries()
        .find_map(|(_, block)| match &block.terminator {
            mir_model::Terminator::Call { arguments, .. } => Some(arguments.clone()),
            _ => None,
        })
        .expect("expected a Call terminator");
    let Operand::Copy(ref_place) = &arguments[0] else {
        panic!(
            "expected the receiver argument to be a Copy of a reference temp, got {:#?}",
            arguments[0]
        );
    };

    // The temp holding the reference (a fresh local distinct from `c`'s own
    // local) must be populated via a *mutable* `Ref` rvalue ahead of the
    // call — not a by-value copy of `c` itself.
    let built_via_mutable_ref = mir.blocks.entries().any(|(_, block)| {
        block.statements.iter().any(|statement| {
            matches!(
                statement,
                mir_model::Statement::Assign(place, Rvalue::Ref { mutable: true, .. })
                if place.local == ref_place.local
            )
        })
    });
    assert!(
        built_via_mutable_ref,
        "expected the receiver arg to be built via a mutable Ref rvalue, got {:#?}",
        mir.blocks
    );
}

#[test]
fn const_this_receiver_is_passed_as_an_immutable_reference() {
    let mir = lower_source(
        "struct Number {\n    n: int\n    get(&this): int {\n        return this.n\n    }\n}\nf(): int {\n    c: Number = Number{n: 5}\n    return c.get()\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let arguments = mir
        .blocks
        .entries()
        .find_map(|(_, block)| match &block.terminator {
            mir_model::Terminator::Call { arguments, .. } => Some(arguments.clone()),
            _ => None,
        })
        .expect("expected a Call terminator");
    let Operand::Copy(ref_place) = &arguments[0] else {
        panic!(
            "expected the receiver argument to be a Copy of a reference temp, got {:#?}",
            arguments[0]
        );
    };
    let built_via_immutable_ref = mir.blocks.entries().any(|(_, block)| {
        block.statements.iter().any(|statement| {
            matches!(
                statement,
                mir_model::Statement::Assign(place, Rvalue::Ref { mutable: false, .. })
                if place.local == ref_place.local
            )
        })
    });
    assert!(
        built_via_immutable_ref,
        "expected the receiver arg to be built via an immutable Ref rvalue, got {:#?}",
        mir.blocks
    );
}

#[test]
fn mut_this_body_accesses_fields_through_a_deref_projection() {
    let mut ast = resolve_source(
        "struct Counter {\n    n: int\n    increment(&mut this) {\n        this.n = this.n + 1\n    }\n}\nf(): int {\n    mut c: Counter = Counter{n: 0}\n    c.increment()\n    return c.n\n}\n",
    );
    let method_id = find_function(&ast.crates.store, "increment");
    let mir = lower_function(&ast.crates.store, &mut ast.declares, method_id)
        .expect("expected successful lowering");

    // `this` itself is argument 0 and is `Reference`-typed now, so
    // `this.n = ..` must project through a `Deref` before the `Field`, same
    // as any other reference-typed place.
    let found = mir.blocks.entries().any(|(_, block)| {
        block.statements.iter().any(|statement| {
            matches!(
                statement,
                mir_model::Statement::Assign(place, _)
                if matches!(
                    place.projection.as_slice(),
                    [mir_model::PlaceElem::Deref, mir_model::PlaceElem::Field(0)]
                )
            )
        })
    });
    assert!(
        found,
        "expected an assign through [Deref, Field(0)], got {:#?}",
        mir.blocks
    );
}

#[test]
fn consuming_this_receiver_is_still_passed_directly_with_no_ref_rvalue() {
    let mir = lower_source(
        "struct Number {\n    n: int\n    intoN(this): int {\n        return this.n\n    }\n}\nf(): int {\n    c: Number = Number{n: 5}\n    return c.intoN()\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let arguments = mir
        .blocks
        .entries()
        .find_map(|(_, block)| match &block.terminator {
            mir_model::Terminator::Call { arguments, .. } => Some(arguments.clone()),
            _ => None,
        })
        .expect("expected a Call terminator");
    // `Number` is a struct — move-only per `is_auto_copy` — so a consuming
    // `this` receiver now moves the caller's own local directly (no `Ref`
    // rvalue/temp involved, unlike `&this`/`&mut this` above).
    assert!(matches!(&arguments[0], Operand::Move(place) if place.projection.is_empty()));
}

#[test]
fn slice_index_read_lowers_to_a_place_with_an_index_projection() {
    let mir = lower_source(
        "f(s: [&]int): int {\n    i: uint = 0\n    return s[i]\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    // `s[i]` now emits a MIR-level bounds check (`Len` + comparison +
    // `Assert`) ahead of the actual indexed read, splitting the function
    // into several blocks — search all of them for the read itself instead
    // of assuming which block/offset it lands at.
    let found = mir.blocks.entries().any(|(_, block)| {
        block.statements.iter().any(|statement| {
            matches!(
                statement,
                mir_model::Statement::Assign(_, Rvalue::Use(Operand::Copy(read_place)))
                if matches!(read_place.projection.as_slice(), [mir_model::PlaceElem::Index(_)])
            )
        })
    });
    assert!(
        found,
        "expected a Use(Copy(..)) read through a single Index(..) projection somewhere in {:#?}",
        mir.blocks
    );
}

#[test]
fn slice_index_write_lowers_to_an_assign_through_an_index_projection() {
    let mir = lower_source(
        "f(mut s: [&mut]int): int {\n    s[0] = 5\n    return s[0]\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    // `s[0] = 5` now emits a MIR-level bounds check ahead of the write too —
    // search all blocks for the write itself rather than assuming which
    // block/offset it lands at.
    let found = mir.blocks.entries().any(|(_, block)| {
        block.statements.iter().any(|statement| {
            matches!(
                statement,
                mir_model::Statement::Assign(place, Rvalue::Use(Operand::Constant(ConstValue::Uint(5))))
                if matches!(place.projection.as_slice(), [mir_model::PlaceElem::Index(_)])
            )
        })
    });
    assert!(
        found,
        "expected an Assign through a single Index(..) projection with a constant 5, somewhere in {:#?}",
        mir.blocks
    );
}

#[test]
fn indexing_a_fixed_size_array_directly_is_rejected() {
    let result = lower_source("f(a: [2]int): int {\n    return a[0]\n}\n", "f");
    assert_rejected_matching(&result, |kind| {
        matches!(kind, MirErrorKind::IndexTargetNotASlice { .. })
    });
}

#[test]
fn struct_field_write_lowers_to_an_assign_through_a_field_projection() {
    let mir = lower_source(
        "struct Point { x: int }\nf(mut p: Point): int {\n    p.x = 5\n    return p.x\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let (_, block) = mir.blocks.entries().next().expect("expected one block");
    let mir_model::Statement::Assign(place, Rvalue::Use(Operand::Constant(ConstValue::Uint(5)))) =
        &block.statements[0]
    else {
        panic!(
            "expected the first statement to assign a constant, got {:#?}",
            block.statements[0]
        );
    };
    assert!(
        matches!(
            place.projection.as_slice(),
            [mir_model::PlaceElem::Field(0)]
        ),
        "expected a single Field(0) projection, got {:#?}",
        place.projection
    );
}

#[test]
fn field_write_on_an_immutable_object_is_still_lowered_the_same_way() {
    // Mutability enforcement is the borrow checker's job (M2, not started
    // yet per mir-design.md) — this lowering slice doesn't gate on it.
    let mir = lower_source(
        "struct Point { x: int }\nf(p: Point): int {\n    p.x = 5\n    return p.x\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let (_, block) = mir.blocks.entries().next().expect("expected one block");
    assert_eq!(block.statements.len(), 2, "{:#?}", block.statements);
}

#[test]
fn nested_compound_expression_is_lowered_via_a_temporary() {
    let mir = lower_source(
        "f(a: int, b: int, c: int): int {\n    return a + b * c\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    // Both `+` and `*` now trap on overflow (`lower_checked_binary_op`),
    // splitting this into several blocks instead of two statements in one —
    // search across all of them rather than assuming exact block/statement
    // indices.
    let all_statements: Vec<&mir_model::Statement> = mir
        .blocks
        .entries()
        .flat_map(|(_, block)| &block.statements)
        .collect();

    let mul_tuple = all_statements
        .iter()
        .find_map(|statement| match statement {
            mir_model::Statement::Assign(place, Rvalue::CheckedBinaryOp(op, ..))
                if *op == ast_model::operators::BinaryOperatorKind::Mul =>
            {
                Some(place)
            }
            _ => None,
        })
        .expect("expected a CheckedBinaryOp(Mul, ..) assignment somewhere");

    // The `b * c` tuple's result field (0) must get copied into its own
    // temp before the outer `a + ..` reads it.
    let temp = all_statements
        .iter()
        .find_map(|statement| match statement {
            mir_model::Statement::Assign(place, Rvalue::Use(Operand::Copy(read_place)))
                if read_place.local == mul_tuple.local
                    && matches!(
                        read_place.projection.as_slice(),
                        [mir_model::PlaceElem::Field(0)]
                    ) =>
            {
                Some(place.local)
            }
            _ => None,
        })
        .expect("expected the Mul tuple's result field to be copied into a temp");

    let temp_decl = mir
        .locals
        .get(temp)
        .expect("expected the temp to have a local declaration");
    assert_eq!(
        temp_decl.mutability,
        sol_utils::TypeModifier::Immut,
        "a temp holding a runtime-computed sub-expression must not be marked `Comptime`"
    );

    let add_reads_temp = all_statements.iter().any(|statement| {
        matches!(
            statement,
            mir_model::Statement::Assign(
                _,
                Rvalue::CheckedBinaryOp(
                    ast_model::operators::BinaryOperatorKind::Add,
                    _,
                    Operand::Copy(right),
                ),
            ) if right.local == temp
        )
    });
    assert!(
        add_reads_temp,
        "expected the outer `a + ..` to read back the `b * c` temp"
    );
}

#[test]
fn return_of_a_call_result_lowers_via_the_call_terminator() {
    let mir = lower_source(
        "g(): int { return 1 }\nf(): int {\n    return g()\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    // entry (ends in the Call) + continuation (reads the result, returns).
    assert_eq!(mir.blocks.entries().count(), 2, "{:#?}", mir.blocks);

    let (_, entry) = mir.blocks.entries().next().unwrap();
    assert!(entry.statements.is_empty(), "{:#?}", entry.statements);
    let mir_model::Terminator::Call {
        arguments: args,
        destination,
        target,
        ..
    } = &entry.terminator
    else {
        panic!(
            "expected the entry block to end in a Call, got {:#?}",
            entry.terminator
        );
    };
    assert!(args.is_empty());
    let destination_local = destination
        .as_ref()
        .expect("expected a destination since the call's result is used")
        .local;
    let continuation_id = target.expect("expected a continuation block");

    let continuation = mir
        .blocks
        .get(continuation_id)
        .expect("expected the continuation block to exist");
    let mir_model::Statement::Assign(ret_place, Rvalue::Use(Operand::Copy(read_place))) =
        &continuation.statements[0]
    else {
        panic!(
            "expected the continuation to read the call's result back, got {:#?}",
            continuation.statements
        );
    };
    assert_eq!(read_place.local, destination_local);
    assert_eq!(Some(ret_place.local), mir.return_local);
    assert!(matches!(
        continuation.terminator,
        mir_model::Terminator::Return
    ));
}

#[test]
fn call_embedded_in_a_larger_expression_splits_the_block() {
    let mir = lower_source(
        "g(): int { return 1 }\nf(a: int): int {\n    return a + g()\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    // entry (ends in the Call) + a checked-add block (`a + <result>` now
    // traps on overflow too, so it splits again for its own Assert) + a
    // final block that returns.
    assert_eq!(mir.blocks.entries().count(), 3, "{:#?}", mir.blocks);

    let (_, entry) = mir.blocks.entries().next().unwrap();
    assert!(
        matches!(entry.terminator, mir_model::Terminator::Call { .. }),
        "{:#?}",
        entry.terminator
    );

    let (_, add_block) = mir.blocks.entries().nth(1).unwrap();
    let mir_model::Statement::Assign(_, Rvalue::CheckedBinaryOp(op, ..)) = &add_block.statements[0]
    else {
        panic!(
            "expected the continuation to compute `a + <call result>`, got {:#?}",
            add_block.statements
        );
    };
    assert_eq!(*op, ast_model::operators::BinaryOperatorKind::Add);
    assert!(matches!(
        add_block.terminator,
        mir_model::Terminator::Assert { .. }
    ));

    let (_, final_block) = mir.blocks.entries().nth(2).unwrap();
    assert!(matches!(
        final_block.terminator,
        mir_model::Terminator::Return
    ));
}

#[test]
fn call_with_multiple_arguments_lowers_each_before_the_call() {
    let mir = lower_source(
        "g(x: int, y: int): int { return x }\nf(a: int, b: int): int {\n    return g(a, b + 1)\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    // `b + 1` must be flattened into its own temp before the call, exactly
    // like any other nested compound expression — and, now that `+` traps
    // on overflow, that temp's own computation splits its own block ahead
    // of the call rather than landing as a single statement in the entry
    // block, so search for whichever block actually ends in the Call.
    let (_, call_block) = mir
        .blocks
        .entries()
        .find(|(_, block)| matches!(block.terminator, mir_model::Terminator::Call { .. }))
        .expect("expected some block to end in a Call");

    let mir_model::Terminator::Call {
        arguments: args, ..
    } = &call_block.terminator
    else {
        unreachable!("just matched on Terminator::Call");
    };
    assert_eq!(args.len(), 2, "{:#?}", args);
}

#[test]
fn bare_statement_call_discards_the_result_even_when_non_none() {
    // The trailing `;` matters: a semicolon-less call in tail position gets
    // implicit-return treatment (checked against `f`'s own return type by the
    // resolver), which isn't what this test is isolating.
    let mir = lower_source("g(): int { return 1 }\nf(): none {\n    g();\n}\n", "f")
        .expect("expected successful lowering");

    let (_, entry) = mir.blocks.entries().next().unwrap();
    let mir_model::Terminator::Call { destination, .. } = &entry.terminator else {
        panic!(
            "expected the entry block to end in a Call, got {:#?}",
            entry.terminator
        );
    };
    assert!(
        destination.is_none(),
        "a bare statement call's result must be discarded even though `g` returns `int`, got {:#?}",
        destination
    );
}

#[test]
fn none_returning_function_can_fall_off_the_end() {
    let mir =
        lower_source("f(): none {\n    x := 1\n}\n", "f").expect("expected successful lowering");

    assert_eq!(mir.return_local, None);
    // `x` is a body-declared local, so falling off the end now goes through
    // `seal_return`'s scope-exit `Drop(x)` chain before the final `Return`
    // (see `lowers_arithmetic_with_a_variable_and_a_return` for the fully
    // worked-out shape) — the entry block ends in that `Drop`, not `Return`
    // directly anymore.
    let (_, entry) = mir.blocks.entries().next().unwrap();
    assert!(
        matches!(entry.terminator, mir_model::Terminator::Drop { .. }),
        "expected the entry block to end in a Drop(x) scope-exit, got {:#?}",
        entry.terminator
    );
    let (_, last) = mir.blocks.entries().last().unwrap();
    assert!(matches!(last.terminator, mir_model::Terminator::Return));
}

#[test]
fn none_returning_function_with_a_bare_return() {
    let mir = lower_source(
        "f(): none {\n    if true {\n        return\n    }\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    assert_eq!(mir.return_local, None);
}

#[test]
fn calling_a_none_returning_function_as_a_statement() {
    let mir = lower_source("g(): none {\n}\nf(): none {\n    g()\n}\n", "f")
        .expect("expected successful lowering");

    let (_, entry) = mir.blocks.entries().next().unwrap();
    let mir_model::Terminator::Call { destination, .. } = &entry.terminator else {
        panic!(
            "expected the entry block to end in a Call, got {:#?}",
            entry.terminator
        );
    };
    assert!(destination.is_none());
}

#[test]
fn non_extern_signature_only_function_is_rejected() {
    // A trait method declaration is signature-only (no body) but isn't
    // `extern "C"` either — unlike an extern declaration, there's no FFI
    // target to lower it to, so it's correctly still rejected (see
    // `extern_c_signature_lowers_into_an_extern_function_with_no_type_restriction`
    // for the case that *is* now supported).
    let mut ast = resolve_source("trait Greeter {\n    greet(): none\n}\n");
    let function_id = ast
        .crates
        .store
        .functions
        .entries()
        .next()
        .map(|(id, _)| id)
        .expect("expected one function entry");

    let result = lower_function(&ast.crates.store, &mut ast.declares, function_id);
    assert_rejected_with(&result, MirErrorKind::SignatureOnlyFunctionHasNoBody);
}

#[test]
fn if_without_else_joins_after_the_then_branch() {
    let mir = lower_source(
        "f(): int {\n    if true {\n        return 1\n    }\n    return 2\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    assert_eq!(mir.blocks.entries().count(), 3, "{:#?}", mir.blocks);

    let switch_blocks = mir
        .blocks
        .entries()
        .filter(|(_, block)| matches!(block.terminator, mir_model::Terminator::SwitchInt { .. }))
        .count();
    assert_eq!(switch_blocks, 1, "expected exactly one switchInt block");

    let return_blocks = mir
        .blocks
        .entries()
        .filter(|(_, block)| matches!(block.terminator, mir_model::Terminator::Return))
        .count();
    assert_eq!(
        return_blocks, 2,
        "expected both the then-branch and the join block to return"
    );
}

#[test]
fn if_else_where_both_branches_return_has_no_join_block() {
    let mir = lower_source(
        "f(): int {\n    if true {\n        return 1\n    } else {\n        return 2\n    }\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    assert_eq!(mir.blocks.entries().count(), 3, "{:#?}", mir.blocks);
}

#[test]
fn statement_after_an_if_else_that_always_returns_is_unreachable() {
    let result = lower_source(
        "f(): int {\n    if true {\n        return 1\n    } else {\n        return 2\n    }\n    return 3\n}\n",
        "f",
    );
    assert_rejected_with(&result, MirErrorKind::UnreachableStatement);
}

#[test]
fn statement_after_a_return_is_unreachable() {
    let result = lower_source("f(): int {\n    return 1\n    return 2\n}\n", "f");
    assert_rejected_with(&result, MirErrorKind::UnreachableStatement);
}

#[test]
fn non_bool_if_condition_is_rejected() {
    let result = lower_source(
        "f(): int {\n    if 1 {\n        return 1\n    }\n    return 2\n}\n",
        "f",
    );
    assert_rejected_with(&result, MirErrorKind::UnsupportedConditionExpression);
}

#[test]
fn while_loop_with_break_reaches_the_exit_block() {
    let mir = lower_source(
        "f(): int {\n    for true {\n        break\n    }\n    return 1\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    assert_eq!(mir.blocks.entries().count(), 4, "{:#?}", mir.blocks);
}

#[test]
fn continue_in_while_body_jumps_back_to_the_header() {
    let mir = lower_source(
        "f(): int {\n    for true {\n        continue\n    }\n    return 1\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let (header_id, _) = mir
        .blocks
        .entries()
        .find(|(_, block)| matches!(block.terminator, mir_model::Terminator::SwitchInt { .. }))
        .expect("expected a header block with a switchInt terminator");

    let continues_to_header = mir.blocks.entries().any(|(_, block)| {
        matches!(block.terminator, mir_model::Terminator::Goto(target) if target == header_id)
    });
    assert!(
        continues_to_header,
        "expected `continue` to jump back to the loop header, {:#?}",
        mir.blocks
    );
}

#[test]
fn break_nested_inside_an_if_targets_the_enclosing_loops_exit_block() {
    let mir = lower_source(
        "f(): int {\n    for true {\n        if true {\n            break\n        }\n    }\n    return 1\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let (_, header) = mir
        .blocks
        .entries()
        .find(|(_, block)| matches!(block.terminator, mir_model::Terminator::SwitchInt { .. }))
        .expect("expected a loop header block");
    let mir_model::Terminator::SwitchInt {
        otherwise: exit_id, ..
    } = header.terminator
    else {
        unreachable!("just matched on SwitchInt above")
    };

    let break_targets_exit = mir.blocks.entries().any(|(_, block)| {
        matches!(block.terminator, mir_model::Terminator::Goto(target) if target == exit_id)
    });
    assert!(
        break_targets_exit,
        "expected the nested `break` to jump to the loop's exit block, {:#?}",
        mir.blocks
    );
}

#[test]
fn break_outside_a_loop_is_rejected() {
    let result = lower_source("f(): int {\n    break\n    return 1\n}\n", "f");
    assert_rejected_with(&result, MirErrorKind::BreakOutsideLoop);
}

#[test]
fn continue_outside_a_loop_is_rejected() {
    let result = lower_source("f(): int {\n    continue\n    return 1\n}\n", "f");
    assert_rejected_with(&result, MirErrorKind::ContinueOutsideLoop);
}

#[test]
fn bare_for_loop_without_a_condition_is_rejected() {
    let result = lower_source(
        "f(): int {\n    for {\n        break\n    }\n    return 1\n}\n",
        "f",
    );
    assert_rejected_with(&result, MirErrorKind::UnsupportedLoopCondition);
}

#[test]
fn comparison_condition_lowers_via_a_temp_into_switch_int() {
    let mir = lower_source(
        "f(a: int, b: int): int {\n    if a > b {\n        return 1\n    }\n    return 2\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let (_, entry) = mir
        .blocks
        .entries()
        .next()
        .expect("expected an entry block");
    assert_eq!(
        entry.statements.len(),
        1,
        "expected the comparison to be lowered into one temp, {:#?}",
        entry.statements
    );

    let mir_model::Statement::Assign(cond_place, Rvalue::BinaryOp(op, _, _)) = &entry.statements[0]
    else {
        panic!(
            "expected the comparison to be assigned to a temp, got {:#?}",
            entry.statements[0]
        );
    };
    assert_eq!(*op, ast_model::operators::BinaryOperatorKind::Gt);

    let mir_model::Terminator::SwitchInt { discriminant, .. } = &entry.terminator else {
        panic!(
            "expected the entry block to end in a switchInt, got {:#?}",
            entry.terminator
        );
    };
    assert!(
        matches!(discriminant, Operand::Copy(place) if place.local == cond_place.local),
        "expected switchInt to read back the comparison's temp"
    );
}

#[test]
fn logical_and_of_two_comparisons_is_lowered_as_nested_temps() {
    let mir = lower_source(
        "f(a: int, b: int, c: int, d: int): int {\n    if a > b && c < d {\n        return 1\n    }\n    return 2\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let (_, entry) = mir
        .blocks
        .entries()
        .next()
        .expect("expected an entry block");

    assert_eq!(entry.statements.len(), 3, "{:#?}", entry.statements);

    let mir_model::Statement::Assign(_, Rvalue::BinaryOp(op, ..)) = &entry.statements[2] else {
        panic!(
            "expected the third statement to combine the two comparisons, got {:#?}",
            entry.statements[2]
        );
    };
    assert_eq!(*op, ast_model::operators::BinaryOperatorKind::LogAnd);
}

#[test]
fn bitwise_and_condition_is_rejected_as_non_bool() {
    let result = lower_source(
        "f(a: int, b: int): int {\n    if a & b {\n        return 1\n    }\n    return 2\n}\n",
        "f",
    );
    assert_rejected_with(&result, MirErrorKind::UnsupportedConditionExpression);
}

#[test]
fn bool_variable_condition_lowers_directly_with_no_temp() {
    let mir = lower_source(
        "f(flag: bool): int {\n    if flag {\n        return 1\n    }\n    return 2\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    assert_eq!(mir.locals.entries().count(), 2, "{:#?}", mir.locals);

    let (_, entry) = mir
        .blocks
        .entries()
        .next()
        .expect("expected an entry block");
    assert_eq!(
        entry.statements.len(),
        0,
        "a bare bool variable condition needs no temp, {:#?}",
        entry.statements
    );
    let mir_model::Terminator::SwitchInt { discriminant, .. } = &entry.terminator else {
        panic!(
            "expected the entry block to end in a switchInt, got {:#?}",
            entry.terminator
        );
    };
    assert!(
        matches!(discriminant, Operand::Copy(_)),
        "expected switchInt to read the `flag` parameter directly, got {discriminant:#?}"
    );
}

#[test]
fn non_bool_variable_condition_is_rejected() {
    let result = lower_source(
        "f(a: int): int {\n    if a {\n        return 1\n    }\n    return 2\n}\n",
        "f",
    );
    assert_rejected_with(&result, MirErrorKind::UnsupportedConditionExpression);
}

#[test]
fn unary_not_condition_lowers_via_a_temp() {
    let mir = lower_source(
        "f(flag: bool): int {\n    if !flag {\n        return 1\n    }\n    return 2\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let (_, entry) = mir
        .blocks
        .entries()
        .next()
        .expect("expected an entry block");
    assert_eq!(entry.statements.len(), 1, "{:#?}", entry.statements);

    let mir_model::Statement::Assign(cond_place, Rvalue::UnaryOp(op, _)) = &entry.statements[0]
    else {
        panic!(
            "expected the `!` to be assigned to a temp, got {:#?}",
            entry.statements[0]
        );
    };
    assert_eq!(*op, ast_model::operators::UnaryOperatorKind::Not);

    let mir_model::Terminator::SwitchInt { discriminant, .. } = &entry.terminator else {
        panic!(
            "expected the entry block to end in a switchInt, got {:#?}",
            entry.terminator
        );
    };
    assert!(
        matches!(discriminant, Operand::Copy(place) if place.local == cond_place.local),
        "expected switchInt to read back the `!flag` temp"
    );
}

#[test]
fn not_of_a_comparison_composes_with_the_existing_temp_flattening() {
    let mir = lower_source(
        "f(a: int, b: int): int {\n    if !(a > b) {\n        return 1\n    }\n    return 2\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let (_, entry) = mir
        .blocks
        .entries()
        .next()
        .expect("expected an entry block");

    assert_eq!(entry.statements.len(), 2, "{:#?}", entry.statements);
}

#[test]
fn unary_negation_is_rejected() {
    let result = lower_source("f(a: int): int {\n    return -a\n}\n", "f");
    assert_rejected_with(&result, MirErrorKind::UnsupportedUnaryOperator);
}

#[test]
fn assignment_to_a_mutable_parameter_reuses_its_existing_local() {
    let mir = lower_source("f(mut a: int): int {\n    a = 5\n    return a\n}\n", "f")
        .expect("expected successful lowering");

    assert_eq!(mir.locals.entries().count(), 2, "{:#?}", mir.locals);

    let (_, block) = mir.blocks.entries().next().unwrap();
    assert_eq!(block.statements.len(), 2, "{:#?}", block.statements);

    let mir_model::Statement::Assign(assign_place, Rvalue::Use(Operand::Constant(_))) =
        &block.statements[0]
    else {
        panic!(
            "expected the first statement to assign the constant into `a`'s local, got {:#?}",
            block.statements[0]
        );
    };

    let mir_model::Statement::Assign(_, Rvalue::Use(Operand::Copy(read_place))) =
        &block.statements[1]
    else {
        panic!(
            "expected the second statement to read `a` back for the return, got {:#?}",
            block.statements[1]
        );
    };
    assert_eq!(
        assign_place.local, read_place.local,
        "expected the assignment and the later read to target the same local"
    );
}

#[test]
fn compound_assignment_is_desugared_into_a_binary_read_of_the_same_local() {
    let mir = lower_source("f(mut n: int): int {\n    n -= 1\n    return n\n}\n", "f")
        .expect("expected successful lowering");

    // `n -= 1` desugars to `n = n - 1`, and `-` now traps on overflow, so
    // the arithmetic lands in a fresh tuple temp (not `n`'s own place)
    // with `n` reassigned from its result field afterward — search across
    // all blocks instead of assuming a single block/statement shape.
    let all_statements: Vec<&mir_model::Statement> = mir
        .blocks
        .entries()
        .flat_map(|(_, block)| &block.statements)
        .collect();

    let (tuple_local, n_local) = all_statements
        .iter()
        .find_map(|statement| match statement {
            mir_model::Statement::Assign(
                place,
                Rvalue::CheckedBinaryOp(
                    ast_model::operators::BinaryOperatorKind::Sub,
                    Operand::Copy(left),
                    _,
                ),
            ) => Some((place.local, left.local)),
            _ => None,
        })
        .expect("expected `n -= 1` to lower to a CheckedBinaryOp(Sub, ..) reading `n`'s own local");

    let reassigns_n = all_statements.iter().any(|statement| {
        matches!(
            statement,
            mir_model::Statement::Assign(place, Rvalue::Use(Operand::Copy(read_place)))
            if place.local == n_local
                && read_place.local == tuple_local
                && matches!(read_place.projection.as_slice(), [mir_model::PlaceElem::Field(0)])
        )
    });
    assert!(
        reassigns_n,
        "expected `n` to be reassigned from the Sub tuple's result field"
    );
}

#[test]
fn assignment_to_a_let_declared_local_reuses_its_local() {
    let mir = lower_source(
        "f(): int {\n    mut x := 1\n    x = 2\n    return x\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    assert_eq!(mir.locals.entries().count(), 2, "{:#?}", mir.locals);
}

#[test]
fn assert_intrinsic_lowers_to_an_assert_terminator() {
    let mir = lower_source("f(): none {\n    assert(true)\n}\n", "f")
        .expect("expected successful lowering");

    // entry (ends in Assert) + continuation (falls off the end -> Return).
    assert_eq!(mir.blocks.entries().count(), 2, "{:#?}", mir.blocks);

    let (_, entry) = mir.blocks.entries().next().unwrap();
    assert!(entry.statements.is_empty(), "{:#?}", entry.statements);
    let mir_model::Terminator::Assert {
        cond,
        expected,
        target,
        ..
    } = &entry.terminator
    else {
        panic!(
            "expected the entry block to end in an Assert, got {:#?}",
            entry.terminator
        );
    };
    assert!(matches!(cond, Operand::Constant(ConstValue::Bool(true))));
    assert!(*expected);

    let continuation = mir
        .blocks
        .get(*target)
        .expect("expected the success continuation block to exist");
    assert!(matches!(
        continuation.terminator,
        mir_model::Terminator::Return
    ));
}

#[test]
fn assert_condition_reuses_the_bool_condition_machinery() {
    let mir = lower_source("f(a: int, b: int): none {\n    assert(a > b)\n}\n", "f")
        .expect("expected successful lowering");

    let (_, entry) = mir.blocks.entries().next().unwrap();
    // `a > b` must be flattened into a temp first, exactly like an `if`
    // condition would, since `lower_assert_intrinsic` reuses
    // `lower_bool_condition`.
    assert_eq!(entry.statements.len(), 1, "{:#?}", entry.statements);
    let mir_model::Statement::Assign(cond_place, Rvalue::BinaryOp(op, ..)) = &entry.statements[0]
    else {
        panic!(
            "expected the comparison to be assigned to a temp, got {:#?}",
            entry.statements[0]
        );
    };
    assert_eq!(*op, ast_model::operators::BinaryOperatorKind::Gt);

    let mir_model::Terminator::Assert { cond, .. } = &entry.terminator else {
        panic!(
            "expected the entry block to end in an Assert, got {:#?}",
            entry.terminator
        );
    };
    assert!(matches!(cond, Operand::Copy(place) if place.local == cond_place.local));
}

#[test]
fn panic_intrinsic_diverges_with_no_reachable_continuation() {
    let mir = lower_source("f(): none {\n    panic(\"oops\")\n}\n", "f")
        .expect("expected successful lowering");

    // No continuation block is ever inserted for `panic`'s dead `target` —
    // same discipline as an `if`/`else` where both arms terminate.
    assert_eq!(mir.blocks.entries().count(), 1, "{:#?}", mir.blocks);

    let (_, entry) = mir.blocks.entries().next().unwrap();
    let mir_model::Terminator::Assert {
        cond,
        expected,
        msg,
        ..
    } = &entry.terminator
    else {
        panic!(
            "expected the entry block to end in an Assert, got {:#?}",
            entry.terminator
        );
    };
    assert!(matches!(cond, Operand::Constant(ConstValue::Bool(false))));
    assert!(*expected);
    assert!(matches!(msg, Operand::Constant(ConstValue::Str(s)) if s == "oops"));
}

#[test]
fn assert_used_as_a_value_is_rejected() {
    // `x := assert(true)` fails earlier, at the variable's own
    // `VariableHasNoResolvedType` check — the resolver never assigns a type
    // to an intrinsic call at all. Routing through `return` instead reaches
    // `lower_operand` directly (via `lower_rvalue`'s catch-all), isolating
    // the check this test is actually after.
    let result = lower_source("f(): int {\n    return assert(true)\n}\n", "f");
    assert_rejected_with(&result, MirErrorKind::CannotUseNoneValueAsOperand);
}

#[test]
fn unsupported_intrinsic_is_rejected_with_a_clear_fault() {
    // `t: int` (not the intrinsic's real expected `typeid` parameter type) is
    // deliberate — MIR doesn't type-check intrinsic arguments, dispatch on
    // `kind` happens before any argument is even lowered, so a bogus but
    // primitive-typed argument is enough to isolate this check without
    // needing `typeid` (non-primitive) support to exist first.
    let result = lower_source("f(t: int): none {\n    intrinsic.typeinfo(t)\n}\n", "f");
    assert_rejected_with(
        &result,
        MirErrorKind::UnsupportedIntrinsic {
            name: "typeinfo".into(),
        },
    );
}

#[test]
fn extern_c_signature_lowers_into_an_extern_function_with_no_type_restriction() {
    let mut ast = resolve_source(r#"extern "C" printCStr(message: cstr): none"#);
    let (id, _) = ast
        .crates
        .store
        .functions
        .entries()
        .find(|(_, kind)| matches!(kind, FunctionKind::Signature(_)))
        .expect("expected one signature-only function");

    let mut lowerer = create_lowerer(&mut ast);
    lowerer
        .lower_function(id)
        .expect("expected extern lowering to succeed");
    let (functions, externs) = lowerer.into_functions_and_externs();

    assert_eq!(
        functions.entries().count(),
        0,
        "an extern declaration has no body — it must not end up as a `Function`"
    );
    let (_, extern_fn) = externs
        .entries()
        .next()
        .expect("expected the extern declaration in `externs`");
    assert_eq!(extern_fn.parameters.len(), 1);
    assert!(
        matches!(
            ast.declares.get_type(extern_fn.parameters[0]),
            Some(ast_model::SolType::Primitive(
                sol_utils::sol_names::PrimitiveTypes::CStr
            ))
        ),
        "{:?}",
        extern_fn.parameters[0]
    );
    assert_eq!(
        extern_fn.return_type, None,
        "a `none`-returning extern function should have no return type, same as `Function::return_local`"
    );
}

#[test]
fn lambda_arrow_body_implicitly_returns_its_tail_expression() {
    let mir = lower_source("id(x: int): int => x\n", "id").expect("expected successful lowering");

    assert_eq!(mir.blocks.entries().count(), 1);
    let (_, block) = mir.blocks.entries().next().unwrap();
    let mir_model::Statement::Assign(place, Rvalue::Use(Operand::Copy(_))) = &block.statements[0]
    else {
        panic!(
            "expected the `=>` body's bare `x` to be assigned into the return local, got {:#?}",
            block.statements
        );
    };
    assert_eq!(Some(place.local), mir.return_local);
    assert!(matches!(block.terminator, mir_model::Terminator::Return));
}

#[test]
fn block_tail_expression_with_no_return_or_semicolon_is_an_implicit_return() {
    let mir = lower_source("f(): int {\n    x := 42\n    x\n}\n", "f")
        .expect("expected successful lowering");

    // `x` is a body-declared local, so falling off the end now goes through
    // `seal_return`'s scope-exit `Drop(x)` chain before the final `Return` —
    // two blocks, not one (see `lowers_arithmetic_with_a_variable_and_a_return`
    // for the fully worked-out shape).
    assert_eq!(mir.blocks.entries().count(), 2, "{:#?}", mir.blocks);
    let (_, block) = mir.blocks.entries().next().unwrap();
    // statements[0] is `x := 42`'s own assignment; [1] its `SetDropFlag`
    // (`x` is tracked); the tail `x` becomes the third statement, assigning
    // into the return local (no `SetDropFlag` — the return local is never
    // tracked).
    assert_eq!(block.statements.len(), 3, "{:#?}", block.statements);
    let mir_model::Statement::Assign(x_place, Rvalue::Use(Operand::Constant(_))) =
        &block.statements[0]
    else {
        panic!(
            "expected x's own declaration assign, got {:#?}",
            block.statements[0]
        );
    };
    assert!(matches!(
        &block.statements[1],
        mir_model::Statement::SetDropFlag(local, true) if *local == x_place.local
    ));
    let mir_model::Statement::Assign(place, Rvalue::Use(Operand::Copy(_))) = &block.statements[2]
    else {
        panic!(
            "expected the tail `x` to be assigned into the return local, got {:#?}",
            block.statements[2]
        );
    };
    assert_eq!(Some(place.local), mir.return_local);

    let mir_model::Terminator::Drop {
        place: dropped,
        target,
        ..
    } = &block.terminator
    else {
        panic!("expected Drop(x), got {:#?}", block.terminator);
    };
    assert_eq!(dropped.local, x_place.local);

    let (final_id, final_block) = mir.blocks.entries().nth(1).unwrap();
    assert_eq!(*target, final_id);
    assert!(matches!(
        final_block.terminator,
        mir_model::Terminator::Return
    ));
}

#[test]
fn exhaustive_if_tail_branches_are_each_an_implicit_return() {
    let mir = lower_source(
        "pick(cond: bool, a: int, b: int): int {\n    if cond {\n        a\n    } else {\n        b\n    }\n}\n",
        "pick",
    )
    .expect("expected successful lowering");

    // No join block: both branches return directly, so the join block
    // `lower_if` allocates is never actually reached/sealed.
    assert_eq!(mir.blocks.entries().count(), 3, "{:#?}", mir.blocks);
    for (_, block) in mir.blocks.entries().skip(1) {
        assert!(
            matches!(block.terminator, mir_model::Terminator::Return),
            "expected every non-entry block (both if-branches) to end in Return, got {:#?}",
            block.terminator
        );
        let mir_model::Statement::Assign(place, Rvalue::Use(Operand::Copy(_))) =
            &block.statements[0]
        else {
            panic!(
                "expected each branch's bare tail to be assigned into the return local, got {:#?}",
                block.statements
            );
        };
        assert_eq!(Some(place.local), mir.return_local);
    }
}

#[test]
fn non_exhaustive_if_tail_is_not_an_implicit_return() {
    // No `else`, so `branch_is_tail` is `false` for the `then` branch (see
    // `lower_if`) — its bare `1` is never treated as this function's return
    // value, so it hits the same `NonReturnTerminalStatementUnsupported` a
    // bare non-`return` expression always has, exactly as before this
    // feature existed (mirrors the resolver's own `non_exhaustive_if_tail_
    // is_skipped`: a non-exhaustive `if`'s branch is never tail-checked
    // either).
    let result = lower_source(
        "f(cond: bool): int {\n    if cond {\n        1\n    }\n}\n",
        "f",
    );
    assert_rejected_with(&result, MirErrorKind::NonReturnTerminalStatementUnsupported);
}

#[test]
fn tail_position_branch_ending_in_a_variable_declaration_still_requires_a_return() {
    // Exhaustive `if`, so `branch_is_tail` is `true` for the `then` branch —
    // but its last statement is a `Variable` declaration, not an
    // `Expression`, so `lower_statement`'s `is_tail` flag has nothing to
    // apply to. Falling off the end of that branch still needs an explicit
    // `return`, same as before this feature existed.
    let result = lower_source(
        "f(cond: bool): int {\n    if cond {\n        x := 1\n    } else {\n        return 2\n    }\n}\n",
        "f",
    );
    assert_rejected_with(&result, MirErrorKind::MissingReturnStatement);
}

#[test]
fn for_loop_tail_is_not_an_implicit_return() {
    // A loop body is never in tail position (see `lower_for`), regardless of
    // being the function's own last statement — a bare trailing expression
    // there is rejected exactly as it was before this feature existed.
    let result = lower_source("f(): int {\n    for true {\n        1\n    }\n}\n", "f");
    assert_rejected_with(&result, MirErrorKind::NonReturnTerminalStatementUnsupported);
}

#[test]
fn multiple_body_locals_are_dropped_in_reverse_declaration_order() {
    let mir = lower_source(
        "f(): int {\n    x := 1\n    y := 2\n    return x + y\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    // Walk the Drop chain in execution order (each Drop's target is the next
    // block) rather than trusting `VecMap`'s own (allocation-order) entries
    // iteration, which isn't necessarily chain order.
    let mut drops = Vec::new();
    let mut current = mir.blocks.entries().find_map(|(id, block)| {
        matches!(block.terminator, mir_model::Terminator::Drop { .. }).then_some(id)
    });
    while let Some(id) = current {
        let block = &mir.blocks[id];
        let mir_model::Terminator::Drop { place, target, .. } = &block.terminator else {
            break;
        };
        drops.push(place.local);
        current = mir
            .blocks
            .get(*target)
            .is_some_and(|b| matches!(b.terminator, mir_model::Terminator::Drop { .. }))
            .then_some(*target);
    }

    assert_eq!(
        drops.len(),
        2,
        "expected exactly 2 Drops (x and y), got {:#?}",
        mir.blocks
    );
    assert_ne!(
        drops[0], drops[1],
        "expected two distinct locals to be dropped"
    );
    // `y` was declared after `x` (higher LocalId, since allocation is
    // strictly increasing) and must be dropped first.
    assert!(
        drops[0] > drops[1],
        "expected the later-declared local (y) to be dropped first, got order {:#?}",
        drops
    );
}

#[test]
fn reassigning_a_body_local_sets_its_drop_flag_again() {
    let mir = lower_source(
        "f(): int {\n    mut x := 1\n    x = 2\n    return x\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let set_true_count = mir
        .blocks
        .entries()
        .flat_map(|(_, block)| &block.statements)
        .filter(|statement| matches!(statement, mir_model::Statement::SetDropFlag(_, true)))
        .count();
    // Once for `x`'s own declaration, once more for `x = 2`'s reassignment.
    assert_eq!(
        set_true_count, 2,
        "expected a SetDropFlag(true) after both x's declaration and its reassignment, got {:#?}",
        mir.blocks
    );
}

#[test]
fn reassigning_a_parameter_gets_no_drop_flag_and_is_never_dropped() {
    // Parameters are never tracked in `body_locals` (mirrors
    // `docs/mir-design.md`'s own `add(a, b)` example, which drops only its
    // body-declared `c`, never `a`/`b`) — a parameter reassignment gets no
    // `SetDropFlag`, and no `Drop` terminator is emitted for it at all.
    let mir = lower_source("f(mut a: int): int {\n    a = 2\n    return a\n}\n", "f")
        .expect("expected successful lowering");

    let has_set_drop_flag = mir
        .blocks
        .entries()
        .flat_map(|(_, block)| &block.statements)
        .any(|statement| matches!(statement, mir_model::Statement::SetDropFlag(..)));
    assert!(
        !has_set_drop_flag,
        "expected no SetDropFlag at all, got {:#?}",
        mir.blocks
    );
    let has_drop = mir
        .blocks
        .entries()
        .any(|(_, block)| matches!(block.terminator, mir_model::Terminator::Drop { .. }));
    assert!(
        !has_drop,
        "expected no Drop terminator, got {:#?}",
        mir.blocks
    );
}

#[test]
fn a_return_nested_inside_an_if_now_drops_enclosing_scope_locals() {
    // Once per-branch scope tracking landed, a `return` from inside an
    // `if`/`else` branch is no longer a hard "no drops at all" case — that's
    // now only true for a `return` reached through an enclosing `for` (see
    // `for_loop_is_a_hard_stop_for_return_unwinding` below). It unwinds the
    // *entire* `scopes` stack, including locals declared in enclosing,
    // non-loop scopes.
    let mir = lower_source(
        "f(cond: bool): int {\n    x := 1\n    if cond {\n        return x\n    }\n    return x\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let drop_count = mir
        .blocks
        .entries()
        .filter(|(_, block)| matches!(block.terminator, mir_model::Terminator::Drop { .. }))
        .count();
    // Both the `return x` inside the if-branch and the top-level `return x`
    // each drop `x` — two separate Drop(x) chains, one per return site.
    assert_eq!(
        drop_count, 2,
        "expected a Drop(x) chain at both return sites, got {:#?}",
        mir.blocks
    );
}

#[test]
fn an_if_branch_local_is_dropped_at_its_own_join_point_not_the_functions_end() {
    let mir = lower_source(
        "struct Thing {\n    n: int\n}\nf(cond: bool): int {\n    if cond {\n        y := Thing{n: 1}\n    }\n    return 0\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    // Exactly one Drop(y) — at the if-branch's own fallthrough to the join
    // block — not a second one bundled into the function's final return
    // (that return's own frame never held `y` in the first place: `y` was
    // popped off `scopes` the moment the branch's own join-exit ran).
    let drop_count = mir
        .blocks
        .entries()
        .filter(|(_, block)| matches!(block.terminator, mir_model::Terminator::Drop { .. }))
        .count();
    assert_eq!(
        drop_count, 1,
        "expected exactly one Drop(y), at the branch's own join point, got {:#?}",
        mir.blocks
    );
}

#[test]
fn return_inside_an_if_branch_drops_branch_then_enclosing_locals_in_order() {
    let mir = lower_source(
        "struct A {\n    n: int\n}\nstruct B {\n    n: int\n}\nf(cond: bool): int {\n    x := A{n: 1}\n    if cond {\n        y := B{n: 2}\n        return 0\n    }\n    return 1\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    // Walk the Drop chain reachable from the `if`-branch's own early
    // `return` in execution order (each Drop's target is the next block) —
    // `y` (innermost, declared inside the branch) must be dropped before
    // `x` (the enclosing, function-level local).
    let mut drops = Vec::new();
    let mut current = mir.blocks.entries().find_map(|(id, block)| {
        matches!(block.terminator, mir_model::Terminator::Drop { .. }).then_some(id)
    });
    while let Some(id) = current {
        let block = &mir.blocks[id];
        let mir_model::Terminator::Drop { place, target, .. } = &block.terminator else {
            break;
        };
        drops.push(place.local);
        current = mir
            .blocks
            .get(*target)
            .is_some_and(|b| matches!(b.terminator, mir_model::Terminator::Drop { .. }))
            .then_some(*target);
    }
    assert_eq!(
        drops.len(),
        2,
        "expected exactly 2 Drops (y then x) on the branch's own return path, got {:#?}",
        mir.blocks
    );
    assert!(
        drops[0] > drops[1],
        "expected the innermost local (y, higher LocalId) to be dropped before the enclosing one (x), got order {:#?}",
        drops
    );
}

#[test]
fn a_return_through_an_enclosing_for_now_drops_it_too() {
    // Once `for` bodies got real scope frames (same shape as `if`-branches),
    // `return`'s old hard-stop-on-any-enclosing-`for` rule was lifted: a
    // `return` diverges straight out of the function, so a `for`'s frame is
    // walked by `seal_return` exactly like an `if`-branch's — no per-
    // iteration concern applies to a path that never loops back at all.
    let mir = lower_source(
        "struct Thing {\n    n: int\n}\nf(cond: bool): int {\n    x := Thing{n: 1}\n    for cond {\n        if cond {\n            return 0\n        }\n    }\n    return 1\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let drop_count = mir
        .blocks
        .entries()
        .filter(|(_, block)| matches!(block.terminator, mir_model::Terminator::Drop { .. }))
        .count();
    // Both the `return 0` inside the `for`+`if` and the top-level `return 1`
    // drop `x` — two separate Drop(x) chains, one per return site.
    assert_eq!(
        drop_count, 2,
        "expected a Drop(x) chain at both return sites, got {:#?}",
        mir.blocks
    );
}

#[test]
fn a_struct_typed_body_local_is_dropped_the_same_as_a_primitive_one() {
    // No `AutoCopy`/move-only classification exists yet (that's the next M2
    // slice) — `Drop`/`SetDropFlag` emission for a body local doesn't
    // distinguish struct types from primitives at all right now.
    let mir = lower_source(
        "struct Point { x: int }\nf(): int {\n    p := Point{x: 5}\n    return p.x\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let has_drop = mir
        .blocks
        .entries()
        .any(|(_, block)| matches!(block.terminator, mir_model::Terminator::Drop { .. }));
    assert!(has_drop, "expected p to be dropped, got {:#?}", mir.blocks);
    let has_set_drop_flag = mir
        .blocks
        .entries()
        .flat_map(|(_, block)| &block.statements)
        .any(|statement| matches!(statement, mir_model::Statement::SetDropFlag(_, true)));
    assert!(
        has_set_drop_flag,
        "expected p's declaration to set its drop flag, got {:#?}",
        mir.blocks
    );
}

/// Builds a `FunctionLowerer` and drives a real `.lower()` over `f` in
/// `source` (populating `self.module`, needed for the struct/`Stub` case
/// below), returning the lowerer so its `is_auto_copy` can be probed
/// directly afterward — there's no call site yet to exercise it through
/// (see M2's TODO.md entry), so this drives it straight, the same way
/// `require_lowerable` itself was tested before it had callers.
fn lowerer_for_is_auto_copy(ast: &mut AstTree) -> FunctionLowerer<'_> {
    let function_id = find_function(&ast.crates.store, "f");
    let FunctionKind::Normal(function) = &ast.crates.store.functions[function_id] else {
        panic!("expected a normal function");
    };
    let mut lowerer = FunctionLowerer::new(&ast.crates.store, &mut ast.declares, &OPTIONS);
    lowerer
        .lower(function)
        .expect("expected successful lowering");
    lowerer
}

fn function_span(ast: &AstTree, name: &str) -> sol_utils::span::Span {
    let function_id = find_function(&ast.crates.store, name);
    let FunctionKind::Normal(function) = &ast.crates.store.functions[function_id] else {
        panic!("expected a normal function");
    };
    function.signature.span
}

#[test]
fn is_auto_copy_treats_primitives_and_references_as_autocopy() {
    let mut ast = resolve_source("f(): none {}\n");
    let span = function_span(&ast, "f");
    let int_ty = ast
        .declares
        .intern_type(SolType::Primitive(PrimitiveTypes::Int));
    let ref_ty = ast.declares.intern_type(SolType::Reference(ReferenceType {
        inner: TypeId::NONE,
        lifetime: None,
        mutable: Mutable::Immut,
    }));
    let lowerer = lowerer_for_is_auto_copy(&mut ast);

    assert!(lowerer.is_auto_copy(int_ty, span).expect("lowerable"));
    assert!(lowerer.is_auto_copy(ref_ty, span).expect("lowerable"));
}

#[test]
fn is_auto_copy_treats_an_owning_pointer_as_move_only() {
    // Unlike `&T`/`RawPtr<T>`, `*T` (`SolType::Pointer`) is an *owning*
    // heap pointer — `new(expr)`'s own result type, freed by its `Drop` —
    // so it's move-only, same as a struct: copying it would produce two
    // "owners" of the same allocation.
    let mut ast = resolve_source("f(): none {}\n");
    let span = function_span(&ast, "f");
    let ptr_ty = ast.declares.intern_type(SolType::Pointer(ReferenceType {
        inner: TypeId::NONE,
        lifetime: None,
        mutable: Mutable::Mut,
    }));
    let lowerer = lowerer_for_is_auto_copy(&mut ast);

    assert!(
        !lowerer.is_auto_copy(ptr_ty, span).expect("lowerable"),
        "an owning *T pointer should be move-only"
    );
}

#[test]
fn is_auto_copy_treats_a_slice_as_autocopy_but_an_owning_array_as_move_only() {
    let mut ast = resolve_source("f(): none {}\n");
    let span = function_span(&ast, "f");
    let mut_slice_ty = ast.declares.intern_type(SolType::Array(ArrayType {
        of_type: TypeId::NONE,
        kind: ArrayKind::MutSlice,
    }));
    let const_slice_ty = ast.declares.intern_type(SolType::Array(ArrayType {
        of_type: TypeId::NONE,
        kind: ArrayKind::ConstSlice,
    }));
    let stack_array_ty = ast.declares.intern_type(SolType::Array(ArrayType {
        of_type: TypeId::NONE,
        kind: ArrayKind::StackArray(4),
    }));
    let lowerer = lowerer_for_is_auto_copy(&mut ast);

    assert!(
        lowerer.is_auto_copy(mut_slice_ty, span).expect("lowerable"),
        "a slice is a non-owning fat pointer, so it should be AutoCopy"
    );
    assert!(
        lowerer
            .is_auto_copy(const_slice_ty, span)
            .expect("lowerable"),
        "a slice is a non-owning fat pointer, so it should be AutoCopy"
    );
    assert!(
        !lowerer
            .is_auto_copy(stack_array_ty, span)
            .expect("lowerable"),
        "an owning fixed-size array should be move-only"
    );
}

#[test]
fn is_auto_copy_treats_a_resolved_struct_as_move_only() {
    let mut ast = resolve_source("struct Point { x: int }\nf(p: Point): none {}\n");
    // Extracted *before* `lowerer_for_is_auto_copy` takes its `&mut
    // ast.declares` borrow (held for the lowerer's whole lifetime) — a
    // second borrow of `ast.declares` afterward wouldn't compile.
    let (param_ty, span) = {
        let function_id = find_function(&ast.crates.store, "f");
        let FunctionKind::Normal(function) = &ast.crates.store.functions[function_id] else {
            panic!("expected a normal function");
        };
        (
            function.signature.value.parameters[0].ty,
            function.signature.span,
        )
    };
    let lowerer = lowerer_for_is_auto_copy(&mut ast);

    assert!(
        !lowerer.is_auto_copy(param_ty, span).expect("lowerable"),
        "a resolved struct should be move-only"
    );
}

#[test]
fn is_auto_copy_rejects_a_type_require_lowerable_would_also_reject() {
    let mut ast = resolve_source("f(): none {}\n");
    let span = function_span(&ast, "f");
    let any_ty = ast.declares.intern_type(SolType::Any);
    let lowerer = lowerer_for_is_auto_copy(&mut ast);

    let result = lowerer.is_auto_copy(any_ty, span);
    let Err(fault) = result else {
        panic!("expected is_auto_copy to reject a non-lowerable type, got {result:#?}");
    };
    assert!(matches!(
        fault.kind(),
        MirErrorKind::NonPrimitiveType { .. }
    ));
}

#[test]
fn a_move_only_struct_argument_is_moved_not_copied() {
    let mir = lower_source(
        "struct Session {\n    n: int\n}\nconsume(s: Session) {}\nf() {\n    s := Session{n: 1}\n    consume(s)\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let arguments = mir
        .blocks
        .entries()
        .find_map(|(_, block)| match &block.terminator {
            mir_model::Terminator::Call { arguments, .. } => Some(arguments.clone()),
            _ => None,
        })
        .expect("expected a Call terminator");
    let Operand::Move(place) = &arguments[0] else {
        panic!(
            "expected the Session argument to be Moved, got {:#?}",
            arguments[0]
        );
    };
    assert!(place.projection.is_empty());

    // `MarkMoved(s)` + `SetDropFlag(s, false)` must land in the same block,
    // ahead of the `Call` terminator that actually consumes `s`.
    let entry_block = mir
        .blocks
        .entries()
        .find(|(_, block)| matches!(block.terminator, mir_model::Terminator::Call { .. }))
        .map(|(_, block)| block)
        .expect("expected the entry block to end in the Call");
    assert!(
        entry_block
            .statements
            .iter()
            .any(|s| matches!(s, mir_model::Statement::MarkMoved(local) if *local == place.local)),
        "expected a MarkMoved(s), got {:#?}",
        entry_block.statements
    );
    assert!(
        entry_block.statements.iter().any(|s| matches!(
            s,
            mir_model::Statement::SetDropFlag(local, false) if *local == place.local
        )),
        "expected a SetDropFlag(s, false), got {:#?}",
        entry_block.statements
    );
}

#[test]
fn lowering_still_emits_an_unconditional_drop_for_a_moved_out_local() {
    // Lowering itself is now deliberately move-unaware (see `drop_chain`'s
    // own docs) — every tracked local gets a `Drop`, moved or not. It's
    // `move_check::elaborate_drops`, a separate pass over the finished MIR,
    // that's responsible for pruning one down to a `Goto`; see the test
    // right below for that half.
    let mir = lower_source(
        "struct Session {\n    n: int\n}\nconsume(s: Session) {}\nf() {\n    s := Session{n: 1}\n    consume(s)\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let has_drop = mir
        .blocks
        .entries()
        .any(|(_, block)| matches!(block.terminator, mir_model::Terminator::Drop { .. }));
    assert!(
        has_drop,
        "expected lowering to still emit an unconditional Drop for s, got {:#?}",
        mir.blocks
    );
}

#[test]
fn elaborate_drops_excludes_a_moved_out_local_from_its_own_scopes_drop_chain() {
    // `s` is moved into `consume(s)` — its own scope's `Drop` (the
    // function's own top-level frame, unwound at the implicit end-of-body
    // `return`) must be pruned to a no-op `Goto` by `elaborate_drops`: once
    // `Terminator::Drop` actually frees an owning `*T` (see M2's TODO.md
    // entry), dropping an already-moved local would double-free it.
    let mut mir = lower_source(
        "struct Session {\n    n: int\n}\nconsume(s: Session) {}\nf() {\n    s := Session{n: 1}\n    consume(s)\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    crate::borrow_checker::elaborate_drops(&mut mir);

    let has_drop = mir
        .blocks
        .entries()
        .any(|(_, block)| matches!(block.terminator, mir_model::Terminator::Drop { .. }));
    assert!(
        !has_drop,
        "expected the moved-out s to have no Drop terminator left after elaborate_drops, got {:#?}",
        mir.blocks
    );
}

#[test]
fn an_owning_pointer_parameter_is_dropped_at_its_own_functions_end() {
    // `consume`'s own body never moves `v` anywhere else (no reassignment,
    // no return, no further consuming call) — it's still owned by `v` when
    // `consume` itself returns, so `consume`'s own top-level frame must drop
    // it there. Before parameters were tracked in `scopes` at all, `v` got
    // **no** `Drop` ever, meaning `consume`'s own `free()` never ran — a
    // real leak on every call, not just the documented conditional-move one.
    let mir =
        lower_source("consume(v: *int) {}\n", "consume").expect("expected successful lowering");

    let drop_target = mir.blocks.entries().find_map(|(_, block)| {
        let mir_model::Terminator::Drop { place, target, .. } = &block.terminator else {
            return None;
        };
        Some((place.local, *target))
    });
    let Some((dropped_local, target)) = drop_target else {
        panic!(
            "expected v to be Dropped at consume's own end, got {:#?}",
            mir.blocks
        );
    };
    let (v_local, _) = mir
        .locals
        .entries()
        .next()
        .expect("expected v, consume's sole parameter, as its first local");
    assert_eq!(
        dropped_local, v_local,
        "expected v (consume's sole parameter) to be the one dropped"
    );
    assert!(
        matches!(
            mir.blocks.get(target).map(|b| &b.terminator),
            Some(mir_model::Terminator::Return)
        ),
        "expected the Drop to chain straight into Return, got {:#?}",
        mir.blocks
    );
}

#[test]
fn a_conditional_move_in_only_one_if_branch_no_longer_leaks_on_the_untaken_path() {
    // `p` is declared *outside* the `if`, and moved into `consume(p)` in
    // only the `then` branch — the `else` branch (here, simply absent)
    // never touches it. Both the `then` and `otherwise` edges converge on
    // the *same* single `Drop` site (the function's own top-level frame,
    // unwound once at the end-of-body return): `p` is "maybe" but not
    // "definitely" moved reaching that shared site (moved on the
    // `cond == true` path, not on `cond == false`) — exactly the case the
    // definite/maybe split exists for. Before that split, `elaborate_drops`
    // only had "maybe" to go on and pruned the `Drop` outright, leaking `p`
    // on the untaken path (this test used to assert exactly that, as the
    // then-accepted imprecision). Now it keeps the `Drop`, marked `guarded`
    // instead — codegen gates the actual `free()` on `p`'s own runtime drop
    // flag, correct on both paths without needing a second `Drop` site.
    let mut mir = lower_source(
        "consume(p: *int) {}\nf(cond: bool) {\n    p := new(1)\n    if cond {\n        consume(p)\n    }\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    crate::borrow_checker::elaborate_drops(&mut mir);

    let guarded_drop = mir.blocks.entries().find_map(|(_, block)| {
        let mir_model::Terminator::Drop { guarded, .. } = &block.terminator else {
            return None;
        };
        Some(*guarded)
    });
    assert_eq!(
        guarded_drop,
        Some(true),
        "expected p's Drop to survive, marked guarded (moved on some but not \
         all paths reaching it), got {:#?}",
        mir.blocks
    );
}

#[test]
fn elaborate_drops_fixes_a_moved_taint_leaking_past_an_early_return() {
    // The bug the old lowering-order-only `moved` flag actually had, that a
    // real per-path dataflow fixes: the `then` branch moves `p` *and*
    // returns early, so it never reaches any code after the `if` at all —
    // but the crude flag was a single value threaded through the *entire*
    // lowering pass in AST-visitation order, so once set it stayed set for
    // every statement lowered afterward, including code reachable *only*
    // through the `cond == false` path (where `p` was never moved). That
    // code's own `Drop` of `p` (at its own `return`) used to be silently
    // skipped — a real leak on the only path that ever reaches it. The real
    // dataflow's join correctly sees this block's only live predecessor (the
    // `otherwise` edge, since `then` diverged via `return` and never reaches
    // here) as never having moved `p`, so `elaborate_drops` must leave this
    // `Drop` in place.
    let mut mir = lower_source(
        "consume(p: *int): int {\n    return 1\n}\nf(cond: bool): int {\n    p := new(1)\n    if cond {\n        consume(p)\n        return 5\n    }\n    x := *p\n    return x\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    crate::borrow_checker::elaborate_drops(&mut mir);

    let has_drop = mir
        .blocks
        .entries()
        .any(|(_, block)| matches!(block.terminator, mir_model::Terminator::Drop { .. }));
    assert!(
        has_drop,
        "expected p to still be Dropped on the cond == false path, got {:#?}",
        mir.blocks
    );
}

#[test]
fn an_autocopy_primitive_argument_is_still_copied() {
    let mir = lower_source(
        "consume(n: int) {}\nf() {\n    n := 1\n    consume(n)\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let arguments = mir
        .blocks
        .entries()
        .find_map(|(_, block)| match &block.terminator {
            mir_model::Terminator::Call { arguments, .. } => Some(arguments.clone()),
            _ => None,
        })
        .expect("expected a Call terminator");
    assert!(matches!(&arguments[0], Operand::Copy(place) if place.projection.is_empty()));

    let has_mark_moved = mir
        .blocks
        .entries()
        .flat_map(|(_, block)| &block.statements)
        .any(|s| matches!(s, mir_model::Statement::MarkMoved(_)));
    assert!(
        !has_mark_moved,
        "an AutoCopy primitive should never be MarkMoved"
    );
}

#[test]
fn a_move_only_argument_reached_through_a_field_projection_is_moved() {
    // A partial move: `c.item` itself is moved, with no whole-local
    // `MarkMoved`/`SetDropFlag` bookkeeping for `c`.
    let mir = lower_source(
        "struct Session {\n    n: int\n}\nstruct Container {\n    item: Session\n}\nconsume(s: Session) {}\nf(c: Container) {\n    consume(c.item)\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let arguments = mir
        .blocks
        .entries()
        .find_map(|(_, block)| match &block.terminator {
            mir_model::Terminator::Call { arguments, .. } => Some(arguments.clone()),
            _ => None,
        })
        .expect("expected a Call terminator");
    assert!(matches!(
        &arguments[0],
        Operand::Move(place) if matches!(place.projection.as_slice(), [mir_model::PlaceElem::Field(0)])
    ));
}

#[test]
fn a_move_only_parameter_argument_is_moved_the_same_as_a_body_local() {
    // Move-eligibility is per-local (via its own type), not gated on
    // `body_locals` (which only tracks Drop-chain scope, a separate
    // concern) — a parameter qualifies exactly the same as a body-declared
    // variable.
    let mir = lower_source(
        "struct Session {\n    n: int\n}\nconsume(s: Session) {}\nf(s: Session) {\n    consume(s)\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let arguments = mir
        .blocks
        .entries()
        .find_map(|(_, block)| match &block.terminator {
            mir_model::Terminator::Call { arguments, .. } => Some(arguments.clone()),
            _ => None,
        })
        .expect("expected a Call terminator");
    assert!(matches!(&arguments[0], Operand::Move(place) if place.projection.is_empty()));
}

#[test]
fn a_move_only_declaration_initializer_moves_the_source() {
    // `t := s` is, structurally, the same "read s, write into a fresh
    // place" as a call argument — same Move-eligibility treatment (via the
    // shared `move_eligible_operand`, see `lower_movable_rvalue`'s docs).
    let mir = lower_source(
        "struct Session {\n    n: int\n}\nf() {\n    s := Session{n: 1}\n    t := s\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let all_statements: Vec<&mir_model::Statement> = mir
        .blocks
        .entries()
        .flat_map(|(_, block)| &block.statements)
        .collect();

    let t_decl = all_statements.iter().find_map(|statement| {
        let mir_model::Statement::Assign(place, Rvalue::Use(Operand::Move(source))) = statement
        else {
            return None;
        };
        Some((place.local, source.local))
    });
    let Some((t_local, s_local)) = t_decl else {
        panic!(
            "expected t's declaration to Move s, got {:#?}",
            all_statements
        );
    };
    assert_ne!(t_local, s_local);

    assert!(
        all_statements
            .iter()
            .any(|s| matches!(s, mir_model::Statement::MarkMoved(local) if *local == s_local)),
        "expected a MarkMoved(s), got {:#?}",
        all_statements
    );
    assert!(
        all_statements.iter().any(|s| matches!(
            s,
            mir_model::Statement::SetDropFlag(local, false) if *local == s_local
        )),
        "expected a SetDropFlag(s, false), got {:#?}",
        all_statements
    );
}

#[test]
fn a_move_only_reassignment_moves_the_source() {
    let mir = lower_source(
        "struct Session {\n    n: int\n}\nf() {\n    mut t := Session{n: 1}\n    s := Session{n: 2}\n    t = s\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let all_statements: Vec<&mir_model::Statement> = mir
        .blocks
        .entries()
        .flat_map(|(_, block)| &block.statements)
        .collect();

    // `t = s`'s own Assign — a Move of a place distinct from `t`'s own local
    // and whose write side re-sets `t`'s own drop flag true right after
    // (`push_assign`, unaffected by this feature).
    let reassignment = all_statements.iter().find_map(|statement| {
        let mir_model::Statement::Assign(place, Rvalue::Use(Operand::Move(source))) = statement
        else {
            return None;
        };
        (source.local != place.local).then_some((place.local, source.local))
    });
    let Some((_, s_local)) = reassignment else {
        panic!("expected t = s to Move s, got {:#?}", all_statements);
    };
    assert!(
        all_statements
            .iter()
            .any(|s| matches!(s, mir_model::Statement::MarkMoved(local) if *local == s_local)),
        "expected a MarkMoved(s), got {:#?}",
        all_statements
    );
}

#[test]
fn returning_a_move_only_variable_moves_it_into_the_return_local() {
    // `return p` is, structurally, the same "read p, write into a fresh
    // place" (the return local) as `t := p` — same Move-eligibility
    // treatment (via `lower_movable_rvalue`, now shared by control_flow.rs).
    let mir = lower_source(
        "struct Session {\n    n: int\n}\nf(): Session {\n    p := Session{n: 1}\n    return p\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let all_statements: Vec<&mir_model::Statement> = mir
        .blocks
        .entries()
        .flat_map(|(_, block)| &block.statements)
        .collect();

    let return_local = mir.return_local.expect("f returns Session, not none");
    let moved_into_return = all_statements.iter().any(|statement| {
        matches!(
            statement,
            mir_model::Statement::Assign(place, Rvalue::Use(Operand::Move(_)))
                if place.local == return_local
        )
    });
    assert!(
        moved_into_return,
        "expected `return p` to Move p into the return local, got {:#?}",
        all_statements
    );

    assert!(
        all_statements
            .iter()
            .any(|s| matches!(s, mir_model::Statement::MarkMoved(local) if *local != return_local)),
        "expected a MarkMoved(p), got {:#?}",
        all_statements
    );
}

#[test]
fn an_implicit_tail_return_of_a_move_only_variable_moves_it() {
    // Same as above, but via the implicit-tail-return fallback (no explicit
    // `return`), which goes through the same `lower_movable_rvalue` call.
    let mir = lower_source(
        "struct Session {\n    n: int\n}\nf(): Session {\n    p := Session{n: 1}\n    p\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let all_statements: Vec<&mir_model::Statement> = mir
        .blocks
        .entries()
        .flat_map(|(_, block)| &block.statements)
        .collect();

    let return_local = mir.return_local.expect("f returns Session, not none");
    let moved_into_return = all_statements.iter().any(|statement| {
        matches!(
            statement,
            mir_model::Statement::Assign(place, Rvalue::Use(Operand::Move(_)))
                if place.local == return_local
        )
    });
    assert!(
        moved_into_return,
        "expected the implicit tail return of p to Move p into the return local, got {:#?}",
        all_statements
    );
}

#[test]
fn an_autocopy_declaration_initializer_is_still_copied() {
    let mir = lower_source("f(): int {\n    x := 1\n    y := x\n    return y\n}\n", "f")
        .expect("expected successful lowering");

    let has_move = mir
        .blocks
        .entries()
        .flat_map(|(_, block)| &block.statements)
        .any(|s| matches!(s, mir_model::Statement::MarkMoved(_)));
    assert!(
        !has_move,
        "an AutoCopy primitive declaration initializer should never be MarkMoved"
    );
}

#[test]
fn a_move_only_declaration_initializer_reached_through_a_field_projection_is_still_copied() {
    // Same scoping as the call-argument case: `SetDropFlag` is per-`LocalId`,
    // not per-place, so `t := c.item` still falls through to a plain Copy.
    let mir = lower_source(
        "struct Session {\n    n: int\n}\nstruct Container {\n    item: Session\n}\nf(c: Container) {\n    t := c.item\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let has_move = mir
        .blocks
        .entries()
        .flat_map(|(_, block)| &block.statements)
        .any(|s| matches!(s, mir_model::Statement::MarkMoved(_)));
    assert!(
        !has_move,
        "a field-projection source should still be Copy, not MarkMoved"
    );
}

#[test]
fn a_move_only_struct_constructor_field_moves_its_source() {
    let mir = lower_source(
        "struct Session {\n    n: int\n}\nstruct Wrapper {\n    s: Session\n}\nf() {\n    s := Session{n: 1}\n    w := Wrapper{s: s}\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let all_statements: Vec<&mir_model::Statement> = mir
        .blocks
        .entries()
        .flat_map(|(_, block)| &block.statements)
        .collect();

    let aggregate_moved_local = all_statements.iter().find_map(|statement| {
        let mir_model::Statement::Assign(
            _,
            Rvalue::Aggregate(mir_model::AggregateKind::Struct, operands),
        ) = statement
        else {
            return None;
        };
        operands.iter().find_map(|operand| match operand {
            Operand::Move(place) => Some(place.local),
            _ => None,
        })
    });
    let Some(s_local) = aggregate_moved_local else {
        panic!(
            "expected Wrapper's field to Move s, got {:#?}",
            all_statements
        );
    };
    assert!(
        all_statements
            .iter()
            .any(|s| matches!(s, mir_model::Statement::MarkMoved(local) if *local == s_local)),
        "expected a MarkMoved(s), got {:#?}",
        all_statements
    );
    assert!(
        all_statements.iter().any(|s| matches!(
            s,
            mir_model::Statement::SetDropFlag(local, false) if *local == s_local
        )),
        "expected a SetDropFlag(s, false), got {:#?}",
        all_statements
    );
}

#[test]
fn an_autocopy_struct_constructor_field_is_still_copied() {
    let mir = lower_source(
        "struct Point {\n    x: int\n}\nf() {\n    n := 1\n    p := Point{x: n}\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let has_move = mir
        .blocks
        .entries()
        .flat_map(|(_, block)| &block.statements)
        .any(|s| matches!(s, mir_model::Statement::MarkMoved(_)));
    assert!(
        !has_move,
        "an AutoCopy primitive field should never be MarkMoved"
    );
}

#[test]
fn move_only_array_literal_elements_move_their_sources() {
    let mir = lower_source(
        "struct Session {\n    n: int\n}\nf() {\n    s := Session{n: 1}\n    t := Session{n: 2}\n    arr: [2]Session = [s, t]\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let all_statements: Vec<&mir_model::Statement> = mir
        .blocks
        .entries()
        .flat_map(|(_, block)| &block.statements)
        .collect();

    let moved_locals: Vec<mir_model::LocalId> = all_statements
        .iter()
        .find_map(|statement| {
            let mir_model::Statement::Assign(
                _,
                Rvalue::Aggregate(mir_model::AggregateKind::Array, operands),
            ) = statement
            else {
                return None;
            };
            Some(
                operands
                    .iter()
                    .filter_map(|operand| match operand {
                        Operand::Move(place) => Some(place.local),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .unwrap_or_default();
    assert_eq!(
        moved_locals.len(),
        2,
        "expected both array elements to Move, got {:#?}",
        all_statements
    );

    for local in &moved_locals {
        assert!(
            all_statements
                .iter()
                .any(|s| matches!(s, mir_model::Statement::MarkMoved(l) if l == local)),
            "expected a MarkMoved for {local:?}, got {:#?}",
            all_statements
        );
    }
}

#[test]
fn an_autocopy_array_literal_is_still_copied() {
    let mir = lower_source(
        "f() {\n    a := 1\n    b := 2\n    arr: [2]int = [a, b]\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let has_move = mir
        .blocks
        .entries()
        .flat_map(|(_, block)| &block.statements)
        .any(|s| matches!(s, mir_model::Statement::MarkMoved(_)));
    assert!(
        !has_move,
        "AutoCopy primitive array elements should never be MarkMoved"
    );
}

/// Follows a chain of `Drop` terminators starting at `start`, returning the
/// `BlockId` the chain ultimately `Goto`s to (or `None` if it ends in
/// something other than a plain `Goto`, e.g. `Return`) — a `Drop`'s own
/// `target` is always a fresh intermediate block (from `new_block()`), never
/// the chain's real final destination directly, so tests can't just check a
/// `Drop`'s immediate `target` field against the destination they expect.
fn drop_chain_target(
    mir: &mir_model::Function,
    start: mir_model::BlockId,
) -> Option<mir_model::BlockId> {
    let mut current = start;
    loop {
        let block = mir.blocks.get(current)?;
        match &block.terminator {
            mir_model::Terminator::Drop { target, .. } => current = *target,
            mir_model::Terminator::Goto(target) => return Some(*target),
            _ => return None,
        }
    }
}

#[test]
fn a_for_loop_body_local_is_dropped_on_the_normal_back_edge_to_the_header() {
    let mir = lower_source(
        "struct Thing {\n    n: int\n}\nf(): int {\n    for true {\n        y := Thing{n: 1}\n    }\n    return 1\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    // Every normal (non-`break`/`continue`) iteration falls off the end of
    // the body and loops back via the *same* scope-exit machinery an
    // `if`-branch's join uses (`seal_scope_exit`): drop the body's own
    // frame, then `Goto(header)`.
    let (header_id, _) = mir
        .blocks
        .entries()
        .find(|(_, block)| matches!(block.terminator, mir_model::Terminator::SwitchInt { .. }))
        .expect("expected a loop header block");

    let reaches_header = mir.blocks.entries().any(|(id, block)| {
        matches!(block.terminator, mir_model::Terminator::Drop { .. })
            && drop_chain_target(&mir, id) == Some(header_id)
    });
    assert!(
        reaches_header,
        "expected a Drop chain ending in Goto(header), got {:#?}",
        mir.blocks
    );
}

#[test]
fn break_drops_the_loop_bodys_own_local_before_exiting() {
    let mir = lower_source(
        "struct Thing {\n    n: int\n}\nf(): int {\n    for true {\n        y := Thing{n: 1}\n        break\n    }\n    return 1\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let (_, header) = mir
        .blocks
        .entries()
        .find(|(_, block)| matches!(block.terminator, mir_model::Terminator::SwitchInt { .. }))
        .expect("expected a loop header block");
    let mir_model::Terminator::SwitchInt {
        otherwise: exit_id, ..
    } = header.terminator
    else {
        unreachable!("just matched on SwitchInt above")
    };

    let drop_targets_exit = mir.blocks.entries().any(|(id, block)| {
        matches!(block.terminator, mir_model::Terminator::Drop { .. })
            && drop_chain_target(&mir, id) == Some(exit_id)
    });
    assert!(
        drop_targets_exit,
        "expected break's own Drop(y) chain to end in Goto(exit), got {:#?}",
        mir.blocks
    );
}

#[test]
fn continue_drops_the_loop_bodys_own_local_before_looping_back() {
    let mir = lower_source(
        "struct Thing {\n    n: int\n}\nf(): int {\n    for true {\n        y := Thing{n: 1}\n        continue\n    }\n    return 1\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let (header_id, _) = mir
        .blocks
        .entries()
        .find(|(_, block)| matches!(block.terminator, mir_model::Terminator::SwitchInt { .. }))
        .expect("expected a loop header block");

    let drop_targets_header = mir.blocks.entries().any(|(id, block)| {
        matches!(block.terminator, mir_model::Terminator::Drop { .. })
            && drop_chain_target(&mir, id) == Some(header_id)
    });
    assert!(
        drop_targets_header,
        "expected continue's own Drop(y) chain to end in Goto(header), got {:#?}",
        mir.blocks
    );
}

#[test]
fn break_inside_a_nested_if_drops_the_ifs_and_loops_frames_but_not_an_outer_one() {
    // `x` is declared outside the loop, `y` inside the loop body, `z` inside
    // an `if`-branch nested in the loop body — `break` from inside that `if`
    // must drop `z` then `y` (the loop's own frame boundary,
    // `LoopTargets::loop_frame_index`) but never touch `x`, which stays
    // alive after the loop.
    let mir = lower_source(
        "struct Thing {\n    n: int\n}\nf(cond: bool): int {\n    x := Thing{n: 0}\n    for cond {\n        y := Thing{n: 1}\n        if cond {\n            z := Thing{n: 2}\n            break\n        }\n    }\n    return 1\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let (_, header) = mir
        .blocks
        .entries()
        .find(|(_, block)| matches!(block.terminator, mir_model::Terminator::SwitchInt { .. }))
        .expect("expected a loop header block");
    let mir_model::Terminator::SwitchInt {
        otherwise: exit_id, ..
    } = header.terminator
    else {
        unreachable!("just matched on SwitchInt above")
    };

    // Every block whose own Drop chain (transitively) reaches `exit` is part
    // of break's chain — there may be other, unrelated Drop chains too (e.g.
    // the loop's own normal back-edge to the header). Their `target`s are
    // each other's *next* step, so the one entry point is whichever of them
    // is never itself somebody else's target.
    let exit_chain_drops: Vec<mir_model::BlockId> = mir
        .blocks
        .entries()
        .filter(|(id, block)| {
            matches!(block.terminator, mir_model::Terminator::Drop { .. })
                && drop_chain_target(&mir, *id) == Some(exit_id)
        })
        .map(|(id, _)| id)
        .collect();
    let later_steps: VecSet<mir_model::BlockId> = exit_chain_drops
        .iter()
        .filter_map(|id| match &mir.blocks[*id].terminator {
            mir_model::Terminator::Drop { target, .. } => Some(*target),
            _ => None,
        })
        .collect();
    let entry = exit_chain_drops
        .iter()
        .copied()
        .find(|id| !later_steps.contains(*id));

    // Walk forward from the entry point, collecting exactly which locals
    // break's own chain drops, in order.
    let mut drops = Vec::new();
    let mut current = entry;
    while let Some(id) = current {
        let block = &mir.blocks[id];
        let mir_model::Terminator::Drop { place, target, .. } = &block.terminator else {
            break;
        };
        drops.push(place.local);
        current = mir
            .blocks
            .get(*target)
            .is_some_and(|b| matches!(b.terminator, mir_model::Terminator::Drop { .. }))
            .then_some(*target);
    }

    assert_eq!(
        drops.len(),
        2,
        "expected exactly 2 Drops (z then y) on break's own path, got {:#?}",
        mir.blocks
    );
    assert!(
        drops[0] > drops[1],
        "expected the innermost local (z, higher LocalId) dropped before the loop's own (y), got order {:#?}",
        drops
    );
    // `drops.len() == 2` above already proves the boundary: break's own
    // chain drops exactly `z` and `y`, never a third local — `x` (declared
    // outside the loop) is dropped separately, at the function's own final
    // `return`, not as part of this chain.
}

#[test]
fn check_moves_flags_a_use_after_move_in_straight_line_code() {
    let mir = lower_source(
        "struct Session {\n    n: int\n}\nconsume(s: Session) {}\nf() {\n    s := Session{n: 1}\n    consume(s)\n    consume(s)\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let faults = crate::borrow_checker::check_moves(&mir);
    assert_eq!(
        faults.len(),
        1,
        "expected exactly one use-after-move fault, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::UseAfterMove));
}

#[test]
fn check_moves_allows_a_reassigned_local_to_be_used_again() {
    let mir = lower_source(
        "struct Session {\n    n: int\n}\nconsume(s: Session) {}\nf() {\n    mut s := Session{n: 1}\n    consume(s)\n    s = Session{n: 2}\n    consume(s)\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let faults = crate::borrow_checker::check_moves(&mir);
    assert!(
        faults.is_empty(),
        "expected no faults — s was reassigned before its second use, got {:#?}",
        faults
    );
}

#[test]
fn check_moves_allows_moving_the_same_local_in_each_arm_of_an_if_else() {
    // The exact case flow-insensitive "moved anywhere ⇒ reject" scans get
    // wrong: `s` is moved once on every path (both arms), never reused after
    // the join — a real dataflow join (union of predecessor states, but each
    // arm's own state independent of its sibling) must accept this.
    let mir = lower_source(
        "struct Session {\n    n: int\n}\nconsume(s: Session) {}\nf(cond: bool) {\n    s := Session{n: 1}\n    if cond {\n        consume(s)\n    } else {\n        consume(s)\n    }\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let faults = crate::borrow_checker::check_moves(&mir);
    assert!(
        faults.is_empty(),
        "moving s once on every path with no reuse after the join should be accepted, got {:#?}",
        faults
    );
}

#[test]
fn check_moves_catches_a_move_then_reuse_reachable_only_through_a_branch() {
    // Before the CFG-dataflow slice, any function containing a `SwitchInt`
    // was skipped *entirely* — so this straight-line-shaped bug (nothing to
    // do with branch precision at all) was silently missed just because an
    // unrelated `if` existed somewhere in the same function. Proves that
    // regression is fixed: `s` is moved, then moved again unconditionally
    // inside the `if`, on every call.
    let mir = lower_source(
        "struct Session {\n    n: int\n}\nconsume(s: Session) {}\nf(cond: bool) {\n    s := Session{n: 1}\n    consume(s)\n    if cond {\n        consume(s)\n    }\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let faults = crate::borrow_checker::check_moves(&mir);
    assert_eq!(
        faults.len(),
        1,
        "expected the straight-line move-then-reuse inside the if to be caught, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::UseAfterMove));
}

#[test]
fn check_moves_flags_a_use_after_a_move_on_only_one_branch() {
    // `s` is moved only on the `cond == true` path; the unconditional use
    // after the join is unsafe on exactly that path — the "maybe moved"
    // join (union of predecessor states) must reject it, even though a
    // "definitely moved on every path" check would wrongly accept it.
    let mir = lower_source(
        "struct Session {\n    n: int\n}\nconsume(s: Session) {}\nf(cond: bool) {\n    s := Session{n: 1}\n    if cond {\n        consume(s)\n    }\n    consume(s)\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let faults = crate::borrow_checker::check_moves(&mir);
    assert_eq!(
        faults.len(),
        1,
        "expected the post-join use to be flagged as maybe-moved, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::UseAfterMove));
}

#[test]
fn check_moves_keeps_sibling_branch_state_independent() {
    // The `if` arm double-moves `s` — a real, self-contained bug in that arm
    // alone. The `else` arm reads `s` once, never moved on its own path. A
    // single shared "moved" flag (instead of per-branch state joined only at
    // the join point) would wrongly poison the `else` arm's legitimate read
    // too — exactly one fault (the `if` arm's own double-move) proves the
    // two arms are tracked independently.
    let mir = lower_source(
        "struct Session {\n    n: int\n}\nconsume(s: Session) {}\nf(cond: bool) {\n    s := Session{n: 1}\n    if cond {\n        consume(s)\n        consume(s)\n    } else {\n        consume(s)\n    }\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let faults = crate::borrow_checker::check_moves(&mir);
    assert_eq!(
        faults.len(),
        1,
        "expected exactly the if arm's own double-move, not the else arm's independent read, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::UseAfterMove));
}

#[test]
fn check_moves_flags_a_loop_body_reusing_a_value_it_moved_last_iteration() {
    // A real fixed-point-over-a-cyclic-CFG case: `s` is moved by `consume`
    // inside the loop body, never reinitialized, and the body reuses it
    // unconditionally — safe (or rather, un-checked at all) on a single
    // reverse-postorder pass that never sees the back edge's own effect
    // propagate back to the header, but a genuine violation on the second
    // and every later iteration. Proves the worklist actually reprocesses
    // the loop header/body until the back edge's state is folded in.
    let mir = lower_source(
        "struct Session {\n    n: int\n}\nconsume(s: Session) {}\nf(cond: bool) {\n    s := Session{n: 1}\n    for cond {\n        consume(s)\n    }\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let faults = crate::borrow_checker::check_moves(&mir);
    assert_eq!(
        faults.len(),
        1,
        "expected the loop body's own repeated move to be caught, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::UseAfterMove));
}

#[test]
fn check_moves_allows_a_loop_body_that_reinitializes_before_every_use() {
    // The fixed point must not just conservatively reject anything touching
    // a loop: `s` is reassigned at the top of the body before `consume`
    // reads it, every single iteration — the reassignment's kill clears
    // whatever the back edge fed in, so this is genuinely safe.
    let mir = lower_source(
        "struct Session {\n    n: int\n}\nconsume(s: Session) {}\nf(cond: bool) {\n    mut s := Session{n: 1}\n    for cond {\n        s = Session{n: 2}\n        consume(s)\n    }\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let faults = crate::borrow_checker::check_moves(&mir);
    assert!(
        faults.is_empty(),
        "reinitializing s before every use inside the loop should be accepted, got {:#?}",
        faults
    );
}

#[test]
fn check_moves_allows_autocopy_values_to_be_used_repeatedly() {
    let mir = lower_source(
        "consume(n: int) {}\nf() {\n    n := 1\n    consume(n)\n    consume(n)\n    consume(n)\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let faults = crate::borrow_checker::check_moves(&mir);
    assert!(
        faults.is_empty(),
        "an AutoCopy primitive should never be flagged as moved, got {:#?}",
        faults
    );
}

#[test]
fn check_moves_flags_a_borrow_of_an_already_moved_value() {
    let mir = lower_source(
        "struct Session {\n    n: int\n}\nconsume(s: Session) {}\nf() {\n    s := Session{n: 1}\n    consume(s)\n    r := &s\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let faults = crate::borrow_checker::check_moves(&mir);
    assert_eq!(
        faults.len(),
        1,
        "expected borrowing an already-moved value to be flagged, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::UseAfterMove));
}

// Partial-move tests: moving a move-only field out of an owned local leaves
// that field (and anything overlapping it) unusable, while its disjoint
// siblings stay usable; moving out from behind a reference, pointer, or
// index is rejected outright.
const PARTIAL_MOVE_TYPES: &str = "struct Box2 {\n    p: *int\n}\nstruct Two {\n    a: *int\n    b: *int\n}\nstruct Outer {\n    inner: Box2\n}\nstruct Mixed {\n    p: *int\n    n: int\n}\nconsume(p: *int) {}\ntakeBox(b: Box2) {}\n";

fn partial_move_faults(function: &str) -> Vec<crate::fault::MirFault> {
    let mir = lower_source(&format!("{PARTIAL_MOVE_TYPES}{function}"), "f")
        .expect("expected successful lowering");
    crate::borrow_checker::check_moves(&mir)
}

fn assert_single_fault(faults: &[crate::fault::MirFault], expected: MirErrorKind, what: &str) {
    assert_eq!(faults.len(), 1, "expected {what}, got {faults:#?}");
    assert_eq!(
        std::mem::discriminant(faults[0].kind()),
        std::mem::discriminant(&expected),
        "expected {what}, got {faults:#?}"
    );
}

#[test]
fn check_moves_flags_a_field_moved_out_twice() {
    let faults = partial_move_faults(
        "f() {\n    n := 1\n    p := new(n)\n    b := Box2{p: p}\n    consume(b.p)\n    consume(b.p)\n}\n",
    );
    assert_single_fault(
        &faults,
        MirErrorKind::UseAfterMove,
        "a second move of b.p to be flagged",
    );
}

#[test]
fn check_moves_flags_a_field_copied_into_two_owners() {
    // Two locals owning one allocation would both free it.
    let faults = partial_move_faults(
        "f() {\n    n := 1\n    p := new(n)\n    b := Box2{p: p}\n    q := b.p\n    r := b.p\n}\n",
    );
    assert_single_fault(
        &faults,
        MirErrorKind::UseAfterMove,
        "the second owner of b.p to be flagged",
    );
}

#[test]
fn check_moves_flags_a_whole_struct_used_after_a_partial_move() {
    let faults = partial_move_faults(
        "f() {\n    n := 1\n    p := new(n)\n    b := Box2{p: p}\n    consume(b.p)\n    takeBox(b)\n}\n",
    );
    assert_single_fault(
        &faults,
        MirErrorKind::UseAfterMove,
        "moving a partially-moved b to be flagged",
    );
}

#[test]
fn check_moves_flags_an_enclosing_field_used_after_a_nested_partial_move() {
    let faults = partial_move_faults(
        "f() {\n    n := 1\n    p := new(n)\n    bx := Box2{p: p}\n    o := Outer{inner: bx}\n    consume(o.inner.p)\n    takeBox(o.inner)\n}\n",
    );
    assert_single_fault(
        &faults,
        MirErrorKind::UseAfterMove,
        "moving a partially-moved o.inner to be flagged",
    );
}

#[test]
fn check_moves_flags_a_field_moved_on_only_one_branch_then_used() {
    let faults = partial_move_faults(
        "f(c: bool) {\n    n := 1\n    p := new(n)\n    b := Box2{p: p}\n    if c {\n        consume(b.p)\n    }\n    consume(b.p)\n}\n",
    );
    assert_single_fault(
        &faults,
        MirErrorKind::UseAfterMove,
        "a maybe-moved b.p to be flagged",
    );
}

#[test]
fn check_moves_flags_a_field_used_after_its_whole_struct_moved() {
    let faults = partial_move_faults(
        "f() {\n    n := 1\n    p := new(n)\n    b := Box2{p: p}\n    takeBox(b)\n    consume(b.p)\n}\n",
    );
    assert_single_fault(
        &faults,
        MirErrorKind::UseAfterMove,
        "a field of a moved b to be flagged",
    );
}

#[test]
fn check_moves_flags_a_borrow_of_a_partially_moved_struct() {
    let faults = partial_move_faults(
        "useBox(r: &Box2) {}\nf() {\n    n := 1\n    p := new(n)\n    b := Box2{p: p}\n    consume(b.p)\n    r := &b\n    useBox(r)\n}\n",
    );
    assert_single_fault(
        &faults,
        MirErrorKind::UseAfterMove,
        "borrowing a partially-moved b to be flagged",
    );
}

#[test]
fn check_moves_allows_moving_disjoint_fields() {
    let faults = partial_move_faults(
        "f() {\n    n := 1\n    m := 2\n    pa := new(n)\n    pb := new(m)\n    t := Two{a: pa, b: pb}\n    consume(t.a)\n    consume(t.b)\n}\n",
    );
    assert!(
        faults.is_empty(),
        "moving two different fields should be accepted, got {faults:#?}"
    );
}

#[test]
fn check_moves_allows_reading_an_unmoved_field_of_a_partially_moved_struct() {
    let faults = partial_move_faults(
        "f(): int {\n    n := 1\n    p := new(n)\n    m := Mixed{p: p, n: 3}\n    consume(m.p)\n    return m.n\n}\n",
    );
    assert!(
        faults.is_empty(),
        "reading an unmoved sibling field should be accepted, got {faults:#?}"
    );
}

#[test]
fn check_moves_allows_a_field_reinitialized_after_a_partial_move() {
    let faults = partial_move_faults(
        "f() {\n    n := 1\n    p := new(n)\n    mut b := Box2{p: p}\n    consume(b.p)\n    b.p = new(n)\n    consume(b.p)\n}\n",
    );
    assert!(
        faults.is_empty(),
        "a reinitialized field should be movable again, got {faults:#?}"
    );
}

#[test]
fn check_moves_rejects_moving_a_field_out_through_a_reference() {
    let faults = partial_move_faults("f(r: &Box2) {\n    consume(r.p)\n}\n");
    assert_single_fault(
        &faults,
        MirErrorKind::MoveOutOfBorrow,
        "a move out through &Box2 to be rejected",
    );
}

#[test]
fn check_moves_rejects_moving_a_field_out_through_an_owning_pointer() {
    let faults = partial_move_faults("f(p: *Box2) {\n    consume(p.p)\n}\n");
    assert_single_fault(
        &faults,
        MirErrorKind::MoveOutOfBorrow,
        "a move out through *Box2 to be rejected",
    );
}

#[test]
fn check_moves_rejects_moving_an_element_out_through_a_slice() {
    let faults = partial_move_faults(
        "f() {\n    n := 1\n    m := 2\n    p1 := new(n)\n    p2 := new(m)\n    mut a: [2]*int = [p1, p2]\n    s: [&mut]*int = &a\n    consume(s[0])\n}\n",
    );
    assert_single_fault(
        &faults,
        MirErrorKind::MoveOutOfBorrow,
        "a move out of a slice element to be rejected",
    );
}

#[test]
fn new_expression_lowers_to_a_heap_alloc_rvalue() {
    let mir = lower_source("f(): int {\n    p := new(5)\n    return *p\n}\n", "f")
        .expect("expected successful lowering");

    let heap_alloc = mir.blocks.entries().find_map(|(_, block)| {
        block.statements.iter().find_map(|statement| {
            let mir_model::Statement::Assign(place, Rvalue::HeapAlloc(_, operand, _)) = statement
            else {
                return None;
            };
            Some((place.local, operand.clone()))
        })
    });
    let Some((p_local, operand)) = heap_alloc else {
        panic!(
            "expected a HeapAlloc rvalue assigned into p, got {:#?}",
            mir.blocks
        );
    };
    assert!(
        matches!(operand, Operand::Constant(ConstValue::Uint(5))),
        "expected the allocated value to be the constant 5, got {operand:#?}"
    );

    // `return *p` reads through a Deref projection on p's own place, same
    // as any other reference/pointer dereference.
    let return_place = mir.blocks.entries().find_map(|(_, block)| {
        block.statements.iter().find_map(|statement| {
            let mir_model::Statement::Assign(place, Rvalue::Use(Operand::Copy(deref_place))) =
                statement
            else {
                return None;
            };
            (Some(place.local) == mir.return_local).then(|| deref_place.clone())
        })
    });
    let Some(deref_place) = return_place else {
        panic!(
            "expected the return value to read through a Deref place, got {:#?}",
            mir.blocks
        );
    };
    assert_eq!(deref_place.local, p_local);
    assert!(matches!(
        deref_place.projection.as_slice(),
        [mir_model::PlaceElem::Deref]
    ));
}

#[test]
fn new_expressions_bare_literal_argument_defaults_to_a_concrete_type() {
    // `new(1)` has no declared target type for its bare literal argument to
    // coerce against (unlike `x: i64 = 1`) — without `default_concrete_type`
    // (see `check_new_expression`'s own docs), `1`'s untyped literal type
    // (`UntypedUint`) would leak straight through into `new(1)`'s own `*T`,
    // producing `*UntypedUint` instead of `*int`.
    let mut ast = resolve_source("f(): none {\n    p := new(1)\n}\n");
    let function_id = find_function(&ast.crates.store, "f");
    let mir = lower_function(&ast.crates.store, &mut ast.declares, function_id)
        .expect("expected successful lowering");

    let heap_alloc_ty = mir.blocks.entries().find_map(|(_, block)| {
        block.statements.iter().find_map(|statement| {
            let mir_model::Statement::Assign(_, Rvalue::HeapAlloc(ty, _, _)) = statement else {
                return None;
            };
            Some(*ty)
        })
    });
    let Some(heap_alloc_ty) = heap_alloc_ty else {
        panic!(
            "expected a HeapAlloc rvalue assigned into p, got {:#?}",
            mir.blocks
        );
    };
    assert_eq!(
        ast.declares.get_type(heap_alloc_ty),
        Some(&SolType::Primitive(PrimitiveTypes::Int)),
        "expected new(1)'s allocated element type to default to int, not stay untyped"
    );
}

#[test]
fn check_escapes_flags_a_direct_reference_to_a_body_local() {
    // The most basic case: `&x` where `x` is a body-declared local — `x`'s
    // own storage dies at this function's own end, so returning a reference
    // to it is dangling on every call, unconditionally.
    let (mir, declares) =
        lower_source_with_declares("f(): &int {\n    x := 1\n    return &x\n}\n", "f");

    let faults = crate::borrow_checker::check_escapes(&function_map(mir), &declares);
    assert_eq!(
        faults.len(),
        1,
        "expected exactly one dangling-reference fault, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::DanglingReference));
}

#[test]
fn check_escapes_flags_a_reference_routed_through_an_intermediate_local() {
    // Same bug as the direct case, but `&x` is stored into `p` first and
    // `p` is what's actually returned — the trace has to follow `p`'s own
    // backward alias chain to find the original `Ref` before it can tell.
    let (mir, declares) = lower_source_with_declares(
        "f(): &int {\n    x := 1\n    p := &x\n    return p\n}\n",
        "f",
    );

    let faults = crate::borrow_checker::check_escapes(&function_map(mir), &declares);
    assert_eq!(
        faults.len(),
        1,
        "expected the reference routed through p to still be caught, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::DanglingReference));
}

#[test]
fn check_escapes_allows_returning_a_received_reference_parameter_unchanged() {
    // `p` is already a reference the caller handed in — this function never
    // constructs a fresh one, it just passes the same value back. Whether
    // that's actually sound depends on what the *caller* passed, which this
    // slice deliberately doesn't check yet (see the module's own docs) — for
    // this function's own body in isolation, it's accepted.
    let (mir, declares) = lower_source_with_declares("f(p: &int): &int {\n    return p\n}\n", "f");

    let faults = crate::borrow_checker::check_escapes(&function_map(mir), &declares);
    assert!(
        faults.is_empty(),
        "returning an already-received reference parameter should be accepted, got {:#?}",
        faults
    );
}

#[test]
fn check_escapes_allows_reborrowing_through_a_reference_parameter() {
    // `&*p` explicitly dereferences `p` (itself a reference) before
    // re-referencing it — the resulting place's projection contains a
    // `Deref`, so this points at whatever `p` points at (external to this
    // function's own frame), not at `p`'s own local storage slot.
    let (mir, declares) =
        lower_source_with_declares("f(p: &int): &int {\n    return &*p\n}\n", "f");

    let faults = crate::borrow_checker::check_escapes(&function_map(mir), &declares);
    assert!(
        faults.is_empty(),
        "reborrowing through an existing reference should be accepted, got {:#?}",
        faults
    );
}

#[test]
fn check_escapes_flags_a_reference_into_an_owning_heap_pointer_parameter() {
    // `p` is passed by value and owned by this function, so it is freed at
    // function exit — a reference into its allocation outlives it.
    let (mir, declares) =
        lower_source_with_declares("f(p: *int): &int {\n    return &*p\n}\n", "f");

    let faults = crate::borrow_checker::check_escapes(&function_map(mir), &declares);
    assert_eq!(
        faults.len(),
        1,
        "expected a reference into a dropped owning parameter to be flagged, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::DanglingReference));
}

#[test]
fn check_escapes_flags_a_reference_into_a_body_local_heap_pointer() {
    // `p` is dropped (freed) at function exit, like any other body local.
    let (mir, declares) = lower_source_with_declares(
        "f(): &int {\n    n := 5\n    p := new(n)\n    return &*p\n}\n",
        "f",
    );

    let faults = crate::borrow_checker::check_escapes(&function_map(mir), &declares);
    assert_eq!(
        faults.len(),
        1,
        "expected a reference into a freed heap local to be flagged, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::DanglingReference));
}

#[test]
fn check_escapes_allows_a_heap_pointer_reached_through_a_reference_parameter() {
    // The pointer is owned by the caller's `Holder`, not by this function,
    // so its allocation outlives this call.
    let (mir, declares) = lower_source_with_declares(
        "struct Holder { p: *int }\nf(h: &Holder): &int {\n    return &*h.p\n}\n",
        "f",
    );

    let faults = crate::borrow_checker::check_escapes(&function_map(mir), &declares);
    assert!(
        faults.is_empty(),
        "a heap pointer owned behind a reference parameter should be accepted, got {:#?}",
        faults
    );
}

#[test]
fn check_escapes_now_flags_a_dangling_return_reached_through_a_bodyless_if_branch() {
    // `if`/`else` is now analyzed (previously the whole function would have
    // been skipped unanalyzed the moment it contained any branch at all) —
    // `x` is never touched by the `if`, so the value reaching `return &x`
    // is dangling on every call, `cond` notwithstanding.
    let (mir, declares) = lower_source_with_declares(
        "f(cond: bool): &int {\n    x := 1\n    if cond {\n        y := 2\n    }\n    return &x\n}\n",
        "f",
    );

    let faults = crate::borrow_checker::check_escapes(&function_map(mir), &declares);
    assert_eq!(
        faults.len(),
        1,
        "expected the branch to no longer hide this dangling return, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::DanglingReference));
}

#[test]
fn check_escapes_flags_a_dangling_return_reachable_only_through_one_of_two_branches() {
    // The falsifying case for this slice: the *old* "skip the whole function
    // on any branch" gate would have silently let this through, even though
    // the `else` arm's own `return &x` is unconditionally dangling every
    // time `cond` is false. Two independent `Terminator::Return`s (one per
    // arm) — proves multi-return handling, not just the single-return case
    // every earlier test here used.
    let (mir, declares) = lower_source_with_declares(
        "f(cond: bool, p: &int): &int {\n    x := 1\n    if cond {\n        return &*p\n    } else {\n        return &x\n    }\n}\n",
        "f",
    );

    let faults = crate::borrow_checker::check_escapes(&function_map(mir), &declares);
    assert_eq!(
        faults.len(),
        1,
        "expected exactly the else-arm's dangling return to be flagged, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::DanglingReference));
}

#[test]
fn check_escapes_flags_a_value_only_made_safe_on_one_incoming_path() {
    // A single `return r` whose value comes from a real join: `r` starts
    // dangling (`&x`), then is *conditionally* reassigned to a safe value
    // before the return. The bodyless `else` means one incoming path to the
    // join still carries the original dangling assignment — the join has to
    // require *every* incoming path safe, not just the one that reassigned.
    let (mir, declares) = lower_source_with_declares(
        "f(cond: bool, p: &int): &int {\n    x := 1\n    mut r := &x\n    if cond {\n        r = &*p\n    }\n    return r\n}\n",
        "f",
    );

    let faults = crate::borrow_checker::check_escapes(&function_map(mir), &declares);
    assert_eq!(
        faults.len(),
        1,
        "expected the still-dangling untaken path to be flagged, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::DanglingReference));
}

#[test]
fn check_escapes_allows_a_value_made_safe_on_every_incoming_path() {
    // Same join shape as above, but *both* incoming values are safe — proves
    // the require-all-safe join isn't just conservatively rejecting anything
    // that touches a branch.
    let (mir, declares) = lower_source_with_declares(
        "f(cond: bool, p: &int, q: &int): &int {\n    mut r := p\n    if cond {\n        r = q\n    }\n    return r\n}\n",
        "f",
    );

    let faults = crate::borrow_checker::check_escapes(&function_map(mir), &declares);
    assert!(
        faults.is_empty(),
        "expected a value safe on every incoming path to be accepted, got {:#?}",
        faults
    );
}

#[test]
fn check_escapes_now_flags_a_dangling_return_reached_through_a_for_loop() {
    // `for`'s back edge is now analyzed (previously the whole function would
    // have been skipped unanalyzed the moment it contained any branch at
    // all, `for` included) — `x` is never touched by the loop, so the value
    // reaching `return &x` is dangling regardless of how many iterations run.
    let (mir, declares) = lower_source_with_declares(
        "f(cond: bool): &int {\n    x := 1\n    for cond {\n        y := 1\n    }\n    return &x\n}\n",
        "f",
    );

    let faults = crate::borrow_checker::check_escapes(&function_map(mir), &declares);
    assert_eq!(
        faults.len(),
        1,
        "expected the for loop to no longer hide this dangling return, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::DanglingReference));
}

#[test]
fn check_escapes_allows_a_value_reassigned_safely_inside_a_for_loop() {
    // A loop is now genuinely analyzed, not just "any loop ⇒ reject" — `r`
    // is reassigned every iteration, but always to another safe, already-
    // received reference, so the converged verdict must still be safe.
    let (mir, declares) = lower_source_with_declares(
        "f(cond: bool, p: &int, q: &int): &int {\n    mut r := p\n    for cond {\n        r = q\n    }\n    return r\n}\n",
        "f",
    );

    let faults = crate::borrow_checker::check_escapes(&function_map(mir), &declares);
    assert!(
        faults.is_empty(),
        "expected a value reassigned safely on every iteration to be accepted, got {:#?}",
        faults
    );
}

#[test]
fn check_escapes_flags_a_value_that_only_dangles_via_the_loop_back_edge() {
    // The falsifying case for real fixed-point convergence: `r` starts safe
    // (an already-received parameter), and the loop body only *sometimes*
    // reassigns it to a dangling loop-local `x`. Resolving `r`'s own value
    // reaching the loop header requires tracing through the loop body's own
    // bodyless-`if` join, whose "untaken" path loops back to the header's
    // own not-yet-settled value — a genuine self-referential dependency a
    // one-shot, non-iterating trace can't answer, and a naive "any cycle ⇒
    // give up" gate would have skipped entirely, missing a real bug: since
    // the loop can run zero or more times, there's a reachable execution
    // (`branch` true on some iteration) where `r` is dangling by the time
    // the function returns.
    let (mir, declares) = lower_source_with_declares(
        "f(cond: bool, branch: bool, p: &int): &int {\n    mut r := p\n    for cond {\n        if branch {\n            x := 1\n            r = &x\n        }\n    }\n    return r\n}\n",
        "f",
    );

    let faults = crate::borrow_checker::check_escapes(&function_map(mir), &declares);
    assert_eq!(
        faults.len(),
        1,
        "expected the loop-back-edge-reachable dangling value to be flagged, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::DanglingReference));
}

#[test]
fn check_escapes_ignores_functions_not_returning_a_reference() {
    let (mir, declares) =
        lower_source_with_declares("f(): int {\n    x := 1\n    return x\n}\n", "f");

    let faults = crate::borrow_checker::check_escapes(&function_map(mir), &declares);
    assert!(
        faults.is_empty(),
        "a function not returning a reference has nothing to check, got {:#?}",
        faults
    );
}

#[test]
fn check_escapes_flags_a_dangling_argument_passed_through_a_tied_parameter() {
    // The core motivating example for interprocedural tracing: `lifetime`'s
    // own body is safe in isolation (it just passes its parameter through),
    // so its own summary is "tied to parameter 0" rather than dangling —
    // but `wrapper` calls it with a reference to a temporary struct that
    // dies before `wrapper` ever returns, so `wrapper`'s own return value
    // is genuinely dangling. No prior slice could catch this: a
    // reference-typed parameter used to be trusted as unconditionally safe.
    let (functions, declares) = lower_all_with_declares(
        "struct Obj {}\nlifetime(obj: &Obj): &Obj { return obj }\nwrapper(): &Obj {\n    t := Obj{}\n    p := &t\n    return lifetime(p)\n}\n",
    );

    let faults = crate::borrow_checker::check_escapes(&functions, &declares);
    assert_eq!(
        faults.len(),
        1,
        "expected wrapper's own dangling return (via lifetime's tied parameter) to be flagged, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::DanglingReference));
}

#[test]
fn check_escapes_allows_a_safe_argument_passed_through_a_tied_parameter() {
    // Same call shape, but `wrapper` passes its own reference parameter
    // through instead of a dangling temporary — `wrapper`'s own summary
    // becomes tied to *its* parameter 0 in turn, but since nothing in this
    // test ever calls `wrapper` with something dangling, no fault fires.
    let (functions, declares) = lower_all_with_declares(
        "struct Obj {}\nlifetime(obj: &Obj): &Obj { return obj }\nwrapper(o: &Obj): &Obj {\n    return lifetime(o)\n}\n",
    );

    let faults = crate::borrow_checker::check_escapes(&functions, &declares);
    assert!(
        faults.is_empty(),
        "passing a genuinely safe argument through a tied parameter should be accepted, got {:#?}",
        faults
    );
}

#[test]
fn check_escapes_flags_a_dangling_argument_propagated_through_a_two_hop_call_chain() {
    // Proves real transitive propagation, not just a single call hop:
    // `middle`'s own summary has to be derived from `lifetime`'s (tied to
    // `middle`'s own parameter 0 in turn) before `outer`'s own call to
    // `middle` with a dangling temporary can be recognized as unsafe. The
    // flat, round-based call-graph fixed point takes roughly one extra
    // round per hop to fully propagate — this is the case that actually
    // exercises that.
    let (functions, declares) = lower_all_with_declares(
        "struct Obj {}\nlifetime(obj: &Obj): &Obj { return obj }\nmiddle(x: &Obj): &Obj {\n    return lifetime(x)\n}\nouter(): &Obj {\n    t := Obj{}\n    p := &t\n    return middle(p)\n}\n",
    );

    let faults = crate::borrow_checker::check_escapes(&functions, &declares);
    assert_eq!(
        faults.len(),
        1,
        "expected outer's own dangling return, propagated through two call hops, to be flagged, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::DanglingReference));
}

#[test]
fn check_escapes_flags_a_dangling_argument_through_self_recursion() {
    // Proves the call-graph fixed point actually handles a cyclic call
    // graph (self-recursion) rather than hanging or silently giving a
    // wrong answer — `recurse`'s own summary has to settle to "tied to
    // parameter 0" despite depending on itself; `wrapper` then calls it
    // with a dangling temporary.
    let (functions, declares) = lower_all_with_declares(
        "struct Obj {}\nrecurse(obj: &Obj, n: int): &Obj {\n    if n > 0 {\n        return recurse(obj, n - 1)\n    }\n    return obj\n}\nwrapper(): &Obj {\n    t := Obj{}\n    p := &t\n    return recurse(p, 3)\n}\n",
    );

    let faults = crate::borrow_checker::check_escapes(&functions, &declares);
    assert_eq!(
        faults.len(),
        1,
        "expected wrapper's own dangling return, through self-recursive `recurse`, to be flagged, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::DanglingReference));
}

#[test]
fn check_escapes_flags_a_dangling_reference_stored_through_a_struct_field() {
    // The core motivating fix for the field-tracing slice: `h.r` is a
    // reference-typed field, set (via the struct constructor) to `p`, a
    // dangling reference to a body local — `&h.r.value` reborrows through
    // that field. Previously any `Deref` reached through a field first was
    // unconditionally `Safe`; this must now be flagged.
    let (mir, declares) = lower_source_with_declares(
        "struct Obj2 { value: int }\nstruct Holder { r: &Obj2 }\nf(): &int {\n    x := Obj2{value: 1}\n    p := &x\n    h := Holder{r: p}\n    return &h.r.value\n}\n",
        "f",
    );

    let faults = crate::borrow_checker::check_escapes(&function_map(mir), &declares);
    assert_eq!(
        faults.len(),
        1,
        "expected the dangling reference reached through h's own field to be flagged, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::DanglingReference));
}

#[test]
fn check_escapes_allows_a_safe_reference_stored_through_a_struct_field() {
    // Same shape as the core fix, but `h.r` is built from a received
    // reference parameter instead of a dangling local — proves the fix
    // isn't over-broad: a struct field tied to a trusted parameter must
    // still resolve to `TiedToParams`, not get flagged.
    let (mir, declares) = lower_source_with_declares(
        "struct Obj2 { value: int }\nstruct Holder { r: &Obj2 }\nf(safe: &Obj2): &int {\n    h := Holder{r: safe}\n    return &h.r.value\n}\n",
        "f",
    );

    let faults = crate::borrow_checker::check_escapes(&function_map(mir), &declares);
    assert!(
        faults.is_empty(),
        "a struct field sourced from a trusted parameter should be accepted, got {:#?}",
        faults
    );
}

#[test]
fn check_escapes_flags_a_dangling_reference_written_directly_into_a_struct_field() {
    // Same bug, but `h.r` is set via a direct field-store assignment after
    // construction, not via the constructor — exercises
    // `trace_field_within_block`'s "exact field-store" branch specifically
    // (distinct from the whole-place-`Aggregate` branch the test above
    // exercises), and proves the backward scan correctly prefers this
    // *later* write over `h`'s own earlier, now-stale safe construction.
    let (mir, declares) = lower_source_with_declares(
        "struct Obj2 { value: int }\nstruct Holder { r: &Obj2 }\nf(safe: &Obj2): &int {\n    mut h := Holder{r: safe}\n    x := Obj2{value: 1}\n    h.r = &x\n    return &h.r.value\n}\n",
        "f",
    );

    let faults = crate::borrow_checker::check_escapes(&function_map(mir), &declares);
    assert_eq!(
        faults.len(),
        1,
        "expected the direct field-store's dangling reference to be flagged, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::DanglingReference));
}

#[test]
fn check_escapes_flags_a_struct_field_assigned_in_an_earlier_block() {
    // The call to `noop()` splits the function, so `h`'s defining write
    // lives in a different block than the `return`.
    let (functions, declares) = lower_all_with_declares(
        "struct Obj2 { value: int }\nstruct Holder { r: &Obj2 }\nnoop() {}\nf(): &int {\n    x := Obj2{value: 1}\n    p := &x\n    mut h := Holder{r: p}\n    noop()\n    return &h.r.value\n}\n",
    );

    let faults = crate::borrow_checker::check_escapes(&functions, &declares);
    assert_eq!(
        faults.len(),
        1,
        "expected a field write from an earlier block to be traced, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::DanglingReference));
}

#[test]
fn check_escapes_flags_a_dangling_reference_through_two_struct_fields() {
    let (mir, declares) = lower_source_with_declares(
        "struct Obj2 { value: int }\nstruct Holder { r: &Obj2 }\nstruct Outer { h: Holder }\nf(): &int {\n    x := Obj2{value: 1}\n    p := &x\n    h := Holder{r: p}\n    o := Outer{h: h}\n    return &o.h.r.value\n}\n",
        "f",
    );

    let faults = crate::borrow_checker::check_escapes(&function_map(mir), &declares);
    assert_eq!(
        faults.len(),
        1,
        "expected a dangling reference two fields deep to be flagged, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::DanglingReference));
}

#[test]
fn check_escapes_flags_a_dangling_reference_copied_out_of_a_struct_field() {
    let (mir, declares) = lower_source_with_declares(
        "struct Obj2 { value: int }\nstruct Holder { r: &Obj2 }\nf(): &Obj2 {\n    x := Obj2{value: 1}\n    p := &x\n    h := Holder{r: p}\n    q := h.r\n    return q\n}\n",
        "f",
    );

    let faults = crate::borrow_checker::check_escapes(&function_map(mir), &declares);
    assert_eq!(
        faults.len(),
        1,
        "expected a dangling reference copied out of a field to be flagged, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::DanglingReference));
}

#[test]
fn check_escapes_flags_a_dangling_field_passed_through_a_tied_parameter() {
    let (functions, declares) = lower_all_with_declares(
        "struct Obj2 { value: int }\nstruct Holder { r: &Obj2 }\nid(r: &Obj2): &Obj2 {\n    return r\n}\nf(): &Obj2 {\n    x := Obj2{value: 1}\n    p := &x\n    h := Holder{r: p}\n    return id(h.r)\n}\n",
    );

    let faults = crate::borrow_checker::check_escapes(&functions, &declares);
    assert_eq!(
        faults.len(),
        1,
        "expected a dangling field passed through a tied parameter to be flagged, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::DanglingReference));
}

#[test]
fn check_escapes_flags_a_dangling_reference_read_through_a_slice_element() {
    let (mir, declares) = lower_source_with_declares(
        "struct Obj2 { value: int }\nf(): &int {\n    x := Obj2{value: 1}\n    p := &x\n    a: [1]&Obj2 = [p]\n    s: [&]&Obj2 = &a\n    return &s[0].value\n}\n",
        "f",
    );

    let faults = crate::borrow_checker::check_escapes(&function_map(mir), &declares);
    assert_eq!(
        faults.len(),
        1,
        "expected a dangling reference reached through a slice element to be flagged, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::DanglingReference));
}

#[test]
fn check_escapes_allows_a_struct_field_assigned_safely_in_an_earlier_block() {
    let (functions, declares) = lower_all_with_declares(
        "struct Obj2 { value: int }\nstruct Holder { r: &Obj2 }\nnoop() {}\nf(safe: &Obj2): &int {\n    mut h := Holder{r: safe}\n    noop()\n    return &h.r.value\n}\n",
    );

    let faults = crate::borrow_checker::check_escapes(&functions, &declares);
    assert!(
        faults.is_empty(),
        "a field set from a parameter in an earlier block should be accepted, got {:#?}",
        faults
    );
}

#[test]
fn check_escapes_allows_a_safe_reference_through_two_struct_fields() {
    let (mir, declares) = lower_source_with_declares(
        "struct Obj2 { value: int }\nstruct Holder { r: &Obj2 }\nstruct Outer { h: Holder }\nf(safe: &Obj2): &int {\n    h := Holder{r: safe}\n    o := Outer{h: h}\n    return &o.h.r.value\n}\n",
        "f",
    );

    let faults = crate::borrow_checker::check_escapes(&function_map(mir), &declares);
    assert!(
        faults.is_empty(),
        "a parameter reached two fields deep should be accepted, got {:#?}",
        faults
    );
}

#[test]
fn check_escapes_allows_a_safe_reference_copied_out_of_a_struct_field() {
    let (mir, declares) = lower_source_with_declares(
        "struct Obj2 { value: int }\nstruct Holder { r: &Obj2 }\nf(safe: &Obj2): &Obj2 {\n    h := Holder{r: safe}\n    q := h.r\n    return q\n}\n",
        "f",
    );

    let faults = crate::borrow_checker::check_escapes(&function_map(mir), &declares);
    assert!(
        faults.is_empty(),
        "a parameter copied out of a field should be accepted, got {:#?}",
        faults
    );
}

#[test]
fn check_escapes_allows_a_safe_field_passed_through_a_tied_parameter() {
    let (functions, declares) = lower_all_with_declares(
        "struct Obj2 { value: int }\nstruct Holder { r: &Obj2 }\nid(r: &Obj2): &Obj2 {\n    return r\n}\nf(safe: &Obj2): &Obj2 {\n    h := Holder{r: safe}\n    return id(h.r)\n}\n",
    );

    let faults = crate::borrow_checker::check_escapes(&functions, &declares);
    assert!(
        faults.is_empty(),
        "a parameter passed through a field into a tied parameter should be accepted, got {:#?}",
        faults
    );
}

#[test]
fn check_escapes_allows_a_safe_reference_read_through_a_slice_element() {
    let (mir, declares) = lower_source_with_declares(
        "struct Obj2 { value: int }\nf(safe: &Obj2): &int {\n    a: [1]&Obj2 = [safe]\n    s: [&]&Obj2 = &a\n    return &s[0].value\n}\n",
        "f",
    );

    let faults = crate::borrow_checker::check_escapes(&function_map(mir), &declares);
    assert!(
        faults.is_empty(),
        "a parameter reached through a slice element should be accepted, got {:#?}",
        faults
    );
}

#[test]
fn check_escapes_flags_a_dangling_reference_written_through_a_mutable_alias() {
    // `m.r = &x` writes `h.r` through `m`, so the scan rooted at `h` must
    // not skip it and fall back to `h`'s earlier safe construction.
    let (mir, declares) = lower_source_with_declares(
        "struct Obj2 { value: int }\nstruct Holder { r: &Obj2 }\nf(safe: &Obj2): &int {\n    x := Obj2{value: 1}\n    mut h := Holder{r: safe}\n    m := &mut h\n    m.r = &x\n    return &h.r.value\n}\n",
        "f",
    );

    let faults = crate::borrow_checker::check_escapes(&function_map(mir), &declares);
    assert_eq!(
        faults.len(),
        1,
        "expected a dangling write through a mutable alias to be flagged, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::DanglingReference));
}

#[test]
fn check_escapes_allows_a_safe_reference_written_through_a_mutable_alias() {
    let (mir, declares) = lower_source_with_declares(
        "struct Obj2 { value: int }\nstruct Holder { r: &Obj2 }\nf(safe: &Obj2, other: &Obj2): &int {\n    mut h := Holder{r: safe}\n    m := &mut h\n    m.r = other\n    return &h.r.value\n}\n",
        "f",
    );

    let faults = crate::borrow_checker::check_escapes(&function_map(mir), &declares);
    assert!(
        faults.is_empty(),
        "a parameter written through a mutable alias should be accepted, got {:#?}",
        faults
    );
}

#[test]
fn check_escapes_flags_a_dangling_reference_written_through_a_mutable_slice() {
    let (mir, declares) = lower_source_with_declares(
        "struct Obj2 { value: int }\nf(safe: &Obj2): &int {\n    x := Obj2{value: 1}\n    p := &x\n    mut a: [1]&Obj2 = [safe]\n    w: [&mut]&Obj2 = &a\n    w[0] = p\n    r: [&]&Obj2 = &a\n    return &r[0].value\n}\n",
        "f",
    );

    let faults = crate::borrow_checker::check_escapes(&function_map(mir), &declares);
    assert_eq!(
        faults.len(),
        1,
        "expected a dangling write through a mutable slice to be flagged, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::DanglingReference));
}

#[test]
fn check_escapes_allows_a_safe_reference_written_through_a_mutable_slice() {
    let (mir, declares) = lower_source_with_declares(
        "struct Obj2 { value: int }\nf(safe: &Obj2, other: &Obj2): &int {\n    mut a: [1]&Obj2 = [safe]\n    w: [&mut]&Obj2 = &a\n    w[0] = other\n    r: [&]&Obj2 = &a\n    return &r[0].value\n}\n",
        "f",
    );

    let faults = crate::borrow_checker::check_escapes(&function_map(mir), &declares);
    assert!(
        faults.is_empty(),
        "a parameter written through a mutable slice should be accepted, got {:#?}",
        faults
    );
}

#[test]
fn check_escapes_flags_a_dangling_reference_written_by_a_callee_through_a_mutable_parameter() {
    // `set` stores its `r` argument into the caller's `h`, so `h.r` is only
    // as safe as what the caller passed for `r`.
    let (functions, declares) = lower_all_with_declares(
        "struct Obj2 { value: int }\nstruct Holder { r: &Obj2 }\nset(h: &mut Holder, r: &Obj2) {\n    h.r = r\n}\nf(safe: &Obj2): &int {\n    x := Obj2{value: 1}\n    mut h := Holder{r: safe}\n    xr := &x\n    hm := &mut h\n    set(hm, xr)\n    return &h.r.value\n}\n",
    );

    let faults = crate::borrow_checker::check_escapes(&functions, &declares);
    assert_eq!(
        faults.len(),
        1,
        "expected a dangling reference stored by a callee to be flagged, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::DanglingReference));
}

#[test]
fn check_escapes_allows_a_safe_reference_written_by_a_callee_through_a_mutable_parameter() {
    let (functions, declares) = lower_all_with_declares(
        "struct Obj2 { value: int }\nstruct Holder { r: &Obj2 }\nset(h: &mut Holder, r: &Obj2) {\n    h.r = r\n}\nf(safe: &Obj2, other: &Obj2): &int {\n    mut h := Holder{r: safe}\n    hm := &mut h\n    set(hm, other)\n    return &h.r.value\n}\n",
    );

    let faults = crate::borrow_checker::check_escapes(&functions, &declares);
    assert!(
        faults.is_empty(),
        "a parameter stored by a callee should be accepted, got {:#?}",
        faults
    );
}

#[test]
fn check_escapes_flags_a_callee_storing_its_own_local_into_a_mutable_parameter() {
    // No reference is returned at all: the dangling reference escapes
    // through the caller's memory instead.
    let (functions, declares) = lower_all_with_declares(
        "struct Obj2 { value: int }\nstruct Holder { r: &Obj2 }\nset(h: &mut Holder) {\n    x := Obj2{value: 1}\n    h.r = &x\n}\n",
    );

    let faults = crate::borrow_checker::check_escapes(&functions, &declares);
    assert_eq!(
        faults.len(),
        1,
        "expected a local stored into a mutable parameter to be flagged, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::DanglingReference));
}

#[test]
fn check_escapes_allows_a_dangling_field_overwritten_by_a_callee_that_always_writes() {
    // `set` writes `h.r` on every path, so the caller's earlier dangling
    // value is replaced, not joined.
    let (functions, declares) = lower_all_with_declares(
        "struct Obj2 { value: int }\nstruct Holder { r: &Obj2 }\nset(h: &mut Holder, r: &Obj2) {\n    h.r = r\n}\nf(safe: &Obj2): &int {\n    x := Obj2{value: 1}\n    p := &x\n    mut h := Holder{r: p}\n    hm := &mut h\n    set(hm, safe)\n    return &h.r.value\n}\n",
    );

    let faults = crate::borrow_checker::check_escapes(&functions, &declares);
    assert!(
        faults.is_empty(),
        "a callee that always overwrites the field should replace its old value, got {:#?}",
        faults
    );
}

#[test]
fn check_escapes_flags_a_dangling_field_only_conditionally_overwritten_by_a_callee() {
    // `setIf` may skip the write, so the earlier dangling value survives.
    let (functions, declares) = lower_all_with_declares(
        "struct Obj2 { value: int }\nstruct Holder { r: &Obj2 }\nsetIf(h: &mut Holder, r: &Obj2, c: bool) {\n    if c {\n        h.r = r\n    }\n}\nf(safe: &Obj2, c: bool): &int {\n    x := Obj2{value: 1}\n    p := &x\n    mut h := Holder{r: p}\n    hm := &mut h\n    setIf(hm, safe, c)\n    return &h.r.value\n}\n",
    );

    let faults = crate::borrow_checker::check_escapes(&functions, &declares);
    assert_eq!(
        faults.len(),
        1,
        "expected a conditionally-overwritten dangling field to be flagged, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::DanglingReference));
}

#[test]
fn check_escapes_flags_a_dangling_reference_conditionally_written_by_a_callee() {
    let (functions, declares) = lower_all_with_declares(
        "struct Obj2 { value: int }\nstruct Holder { r: &Obj2 }\nsetIf(h: &mut Holder, r: &Obj2, c: bool) {\n    if c {\n        h.r = r\n    }\n}\nf(safe: &Obj2, c: bool): &int {\n    x := Obj2{value: 1}\n    mut h := Holder{r: safe}\n    xr := &x\n    hm := &mut h\n    setIf(hm, xr, c)\n    return &h.r.value\n}\n",
    );

    let faults = crate::borrow_checker::check_escapes(&functions, &declares);
    assert_eq!(
        faults.len(),
        1,
        "expected a conditionally-written dangling reference to be flagged, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::DanglingReference));
}

#[test]
fn check_escapes_allows_a_dangling_field_overwritten_through_a_unique_mutable_alias() {
    let (mir, declares) = lower_source_with_declares(
        "struct Obj2 { value: int }\nstruct Holder { r: &Obj2 }\nf(safe: &Obj2): &int {\n    x := Obj2{value: 1}\n    p := &x\n    mut h := Holder{r: p}\n    m := &mut h\n    m.r = safe\n    return &h.r.value\n}\n",
        "f",
    );

    let faults = crate::borrow_checker::check_escapes(&function_map(mir), &declares);
    assert!(
        faults.is_empty(),
        "a write through an alias that always targets h should replace its old value, got {:#?}",
        faults
    );
}

#[test]
fn check_escapes_flags_a_dangling_field_overwritten_through_an_alias_that_may_target_another_place()
{
    // `m` points at `h` or `g` depending on `c`, so the write through it
    // can't replace `h.r`'s earlier dangling value.
    let (mir, declares) = lower_source_with_declares(
        "struct Obj2 { value: int }\nstruct Holder { r: &Obj2 }\nf(safe: &Obj2, c: bool): &int {\n    x := Obj2{value: 1}\n    p := &x\n    mut h := Holder{r: p}\n    mut g := Holder{r: safe}\n    mut m := &mut h\n    if c {\n        m = &mut g\n    }\n    m.r = safe\n    return &h.r.value\n}\n",
        "f",
    );

    let faults = crate::borrow_checker::check_escapes(&function_map(mir), &declares);
    assert_eq!(
        faults.len(),
        1,
        "expected a write through an ambiguous alias to leave h.r's dangling value in place, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::DanglingReference));
}

// `copyR` stores whatever the caller already had in `src.r` into `dst.r`,
// so the caller's `dst.r` ends up exactly as safe as its own `src.r`.
const COPY_FIELD_CALLEE: &str = "struct Obj2 { value: int }\nstruct Holder { r: &Obj2 }\ncopyR(dst: &mut Holder, src: &Holder) {\n    dst.r = src.r\n}\n";

#[test]
fn check_escapes_flags_a_dangling_field_copied_between_arguments_by_a_callee() {
    let (functions, declares) = lower_all_with_declares(&format!(
        "{COPY_FIELD_CALLEE}f(safe: &Obj2): &int {{\n    x := Obj2{{value: 1}}\n    p := &x\n    mut dst := Holder{{r: safe}}\n    src := Holder{{r: p}}\n    dm := &mut dst\n    sr := &src\n    copyR(dm, sr)\n    return &dst.r.value\n}}\n"
    ));

    let faults = crate::borrow_checker::check_escapes(&functions, &declares);
    assert_eq!(
        faults.len(),
        1,
        "expected the callee's copy of a dangling field to be flagged, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::DanglingReference));
}

#[test]
fn check_escapes_allows_a_safe_field_copied_between_arguments_by_a_callee() {
    let (functions, declares) = lower_all_with_declares(&format!(
        "{COPY_FIELD_CALLEE}f(safe: &Obj2, other: &Obj2): &int {{\n    mut dst := Holder{{r: safe}}\n    src := Holder{{r: other}}\n    dm := &mut dst\n    sr := &src\n    copyR(dm, sr)\n    return &dst.r.value\n}}\n"
    ));

    let faults = crate::borrow_checker::check_escapes(&functions, &declares);
    assert!(
        faults.is_empty(),
        "a parameter copied between fields by a callee should be accepted, got {:#?}",
        faults
    );
}

#[test]
fn check_escapes_converges_on_a_recursive_callee_writing_through_its_mutable_parameter() {
    // `set` writes `h.r` either directly or through its own recursive call,
    // so every return has written it and the caller's dangling value is
    // replaced.
    let (functions, declares) = lower_all_with_declares(
        "struct Obj2 { value: int }\nstruct Holder { r: &Obj2 }\nset(h: &mut Holder, r: &Obj2, c: bool) {\n    if c {\n        set(h, r, false)\n        return\n    }\n    h.r = r\n}\nf(safe: &Obj2, c: bool): &int {\n    x := Obj2{value: 1}\n    p := &x\n    mut h := Holder{r: p}\n    hm := &mut h\n    set(hm, safe, c)\n    return &h.r.value\n}\n",
    );

    let faults = crate::borrow_checker::check_escapes(&functions, &declares);
    assert!(
        faults.is_empty(),
        "every path through the recursive callee writes h.r, so the call should replace it, got {:#?}",
        faults
    );
}

#[test]
fn check_escapes_flags_a_dangling_field_a_recursive_callee_only_sometimes_overwrites() {
    // With `d` false neither branch writes, so the optimistic start of the
    // summary rounds must still settle on "sometimes".
    let (functions, declares) = lower_all_with_declares(
        "struct Obj2 { value: int }\nstruct Holder { r: &Obj2 }\nset(h: &mut Holder, r: &Obj2, c: bool, d: bool) {\n    if c {\n        set(h, r, false, d)\n        return\n    }\n    if d {\n        h.r = r\n    }\n}\nf(safe: &Obj2, c: bool, d: bool): &int {\n    x := Obj2{value: 1}\n    p := &x\n    mut h := Holder{r: p}\n    hm := &mut h\n    set(hm, safe, c, d)\n    return &h.r.value\n}\n",
    );

    let faults = crate::borrow_checker::check_escapes(&functions, &declares);
    assert_eq!(
        faults.len(),
        1,
        "expected a recursive callee's conditional write to leave the dangling value in place, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::DanglingReference));
}

// Recursive-type tests: `n.r` is a distinct (strongly updated) place, while
// every path at or below `n.next` folds onto one summary place that is only
// ever weakly updated. The signal is the dangling store left in the
// caller's memory at function exit.
const RECURSIVE_NODE: &str =
    "struct Obj2 { value: int }\nstruct Node {\n    r: &Obj2\n    next: *Node\n}\n";

fn recursive_node_faults(body: &str) -> Vec<crate::fault::MirFault> {
    let source = format!(
        "{RECURSIVE_NODE}f(n: &mut Node, safe: &Obj2) {{\n    x := Obj2{{value: 1}}\n{body}}}\n"
    );
    let (mir, declares) = lower_source_with_declares(&source, "f");
    crate::borrow_checker::check_escapes(&function_map(mir), &declares)
}

#[test]
fn check_escapes_allows_a_dangling_head_field_overwritten_on_a_recursive_type() {
    let faults = recursive_node_faults("    n.r = &x\n    n.r = safe\n");
    assert!(
        faults.is_empty(),
        "the head's own field should be strongly updated, got {:#?}",
        faults
    );
}

#[test]
fn check_escapes_flags_a_dangling_tail_field_not_cleared_by_a_head_write() {
    let faults = recursive_node_faults("    n.next.r = &x\n    n.r = safe\n");
    assert_eq!(
        faults.len(),
        1,
        "expected a head write to leave the tail's dangling value in place, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::DanglingReference));
}

#[test]
fn check_escapes_flags_a_dangling_deep_tail_field_not_cleared_by_a_shallower_tail_write() {
    // `n.next.r` and `n.next.next.r` share one summary place, so the later
    // write can't replace the earlier one.
    let faults = recursive_node_faults("    n.next.next.r = &x\n    n.next.r = safe\n");
    assert_eq!(
        faults.len(),
        1,
        "expected the summary place to be weakly updated, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::DanglingReference));
}

#[test]
fn check_escapes_flags_a_dangling_tail_field_overwritten_on_a_recursive_type() {
    // An accepted false positive of the summary rule: even the same tail
    // path, written twice, is only ever weakly updated.
    let faults = recursive_node_faults("    n.next.r = &x\n    n.next.r = safe\n");
    assert_eq!(
        faults.len(),
        1,
        "expected a tail overwrite to be a weak update, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::DanglingReference));
}

#[test]
fn check_escapes_converges_when_a_loop_walks_a_recursive_type() {
    // `cur` aliases `n`, then the summary place, on later iterations; the
    // fixed point must terminate with every write treated as weak.
    let (mir, declares) = lower_source_with_declares(
        &format!(
            "{RECURSIVE_NODE}f(n: &mut Node, safe: &Obj2, c: bool) {{\n    mut cur := n\n    for c {{\n        cur.r = safe\n        cur = &mut *cur.next\n    }}\n}}\n"
        ),
        "f",
    );

    let faults = crate::borrow_checker::check_escapes(&function_map(mir), &declares);
    assert!(
        faults.is_empty(),
        "writing a parameter through a list-walking alias should be accepted, got {:#?}",
        faults
    );
}

// Borrow conflicts are checked across the whole program (a call result
// carries the loans of the arguments it derives from), so every function in
// `source` is lowered and checked.
fn overlaps_in(source: &str) -> Vec<crate::fault::MirFault> {
    let (functions, declares) = lower_all_with_declares(source);
    crate::borrow_checker::check_borrow_overlaps(&functions, &declares)
}

fn moves_while_borrowed_in(source: &str) -> Vec<crate::fault::MirFault> {
    let (functions, declares) = lower_all_with_declares(source);
    crate::borrow_checker::check_move_while_borrowed(&functions, &declares)
}

#[test]
fn check_borrow_overlaps_flags_a_mutable_borrow_overlapping_a_shared_borrow() {
    // The exact motivating example from scoping this slice: `mutRef` is
    // created, `ref` is created and used while `mutRef` is still pending
    // its own later use — their live ranges overlap, and one side is
    // mutable.
    let faults = overlaps_in(
        "struct Obj {}\nuseRef(r: &Obj) {}\nuseMut(r: &mut Obj) {}\nf() {\n    mut var := Obj{}\n    mutRef := &mut var\n    ref := &var\n    useMut(mutRef)\n    useRef(ref)\n}\n",
    );
    assert_eq!(
        faults.len(),
        1,
        "expected exactly one overlapping-borrow fault, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::OverlappingBorrows));
}

#[test]
fn check_borrow_overlaps_allows_two_overlapping_shared_borrows() {
    // Unlimited simultaneous shared borrows are always fine — neither side
    // is mutable, so this must never be flagged regardless of overlap.
    let faults = overlaps_in(
        "struct Obj {}\nuseRef(r: &Obj) {}\nf() {\n    var := Obj{}\n    r1 := &var\n    r2 := &var\n    useRef(r1)\n    useRef(r2)\n}\n",
    );
    assert!(
        faults.is_empty(),
        "two overlapping shared borrows should never be flagged, got {:#?}",
        faults
    );
}

#[test]
fn check_borrow_overlaps_traces_a_reborrow_through_an_intermediate_local() {
    // `r2 := r1` is just an alias of `r1`'s own value, not a fresh `&var` —
    // the trace has to follow it back to `r1`'s own `Ref` to realize `r2`
    // and `mutRef` actually conflict. `r1` itself (last used only by the
    // `r2 := r1` read, before `mutRef` even exists) must NOT be flagged.
    let faults = overlaps_in(
        "struct Obj {}\nuseRef(r: &Obj) {}\nuseMut(r: &mut Obj) {}\nf() {\n    mut var := Obj{}\n    r1 := &var\n    r2 := r1\n    mutRef := &mut var\n    useRef(r2)\n    useMut(mutRef)\n}\n",
    );
    assert_eq!(
        faults.len(),
        1,
        "expected exactly the r2-vs-mutRef conflict (not r1, whose only use predates mutRef), got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::OverlappingBorrows));
}

#[test]
fn check_borrow_overlaps_allows_sequential_non_overlapping_mutable_borrows() {
    // Real (NLL-style) liveness, not a lexical-scope heuristic: `r1`'s last
    // use is strictly before `r2` is even created, so their live ranges
    // don't actually overlap even though both are `&mut` borrows of the
    // same local within the same function.
    let faults = overlaps_in(
        "struct Obj {}\nuseMut(r: &mut Obj) {}\nf() {\n    mut var := Obj{}\n    r1 := &mut var\n    useMut(r1)\n    r2 := &mut var\n    useMut(r2)\n}\n",
    );
    assert!(
        faults.is_empty(),
        "sequential, non-overlapping mutable borrows should be accepted, got {:#?}",
        faults
    );
}

#[test]
fn check_borrow_overlaps_now_flags_an_overlap_reached_through_a_bodyless_if_branch() {
    // `if`/`else` is now analyzed (previously the whole function would have
    // been skipped unanalyzed the moment it contained any branch at all) —
    // `mutRef` is read inside the `if`, `ref` is read unconditionally right
    // after it: on the path where `cond` is true, both are genuinely live
    // at the same time, a real conflict every earlier straight-line-only
    // slice would have missed.
    let faults = overlaps_in(
        "struct Obj {}\nuseRef(r: &Obj) {}\nuseMut(r: &mut Obj) {}\nf(cond: bool) {\n    mut var := Obj{}\n    mutRef := &mut var\n    ref := &var\n    if cond {\n        useMut(mutRef)\n    }\n    useRef(ref)\n}\n",
    );
    assert_eq!(
        faults.len(),
        1,
        "expected the branch to no longer hide this overlap, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::OverlappingBorrows));
}

#[test]
fn check_borrow_overlaps_allows_two_mutable_borrows_in_mutually_exclusive_branches() {
    // The falsifying case for this slice: a *naive* extension that just
    // concatenated every block into one flat sequential list (rather than
    // real per-point CFG liveness) would see these two `&mut` borrows as
    // "sequential" and wrongly flag them — they're actually in sibling
    // branches, so at most one of them ever exists at runtime. Proves the
    // CFG-aware liveness respects branch exclusivity, not just that it
    // catches more cases than straight-line code did.
    let faults = overlaps_in(
        "struct Obj {}\nuseMut(r: &mut Obj) {}\nf(cond: bool) {\n    mut var := Obj{}\n    if cond {\n        r1 := &mut var\n        useMut(r1)\n    } else {\n        r2 := &mut var\n        useMut(r2)\n    }\n}\n",
    );
    assert!(
        faults.is_empty(),
        "two mutable borrows confined to sibling branches should never be flagged, got {:#?}",
        faults
    );
}

#[test]
fn check_borrow_overlaps_now_flags_an_overlap_reached_through_a_for_loop() {
    // `for`'s back edge is now analyzed (previously the whole function would
    // have been skipped unanalyzed the moment it contained a loop at all) —
    // `mutRef` remains live throughout the loop body, `ref` is read right
    // after: a real conflict every earlier slice would have missed.
    let faults = overlaps_in(
        "struct Obj {}\nuseRef(r: &Obj) {}\nuseMut(r: &mut Obj) {}\nf(cond: bool) {\n    mut var := Obj{}\n    mutRef := &mut var\n    ref := &var\n    for cond {\n        useMut(mutRef)\n    }\n    useRef(ref)\n}\n",
    );
    assert_eq!(
        faults.len(),
        1,
        "expected the for loop to no longer hide this overlap, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::OverlappingBorrows));
}

#[test]
fn check_borrow_overlaps_allows_a_borrow_confined_to_one_loop_iteration() {
    // A loop is now genuinely analyzed, not just "any loop ⇒ reject" — `r`
    // is created and used entirely within the loop body's own single pass
    // (dies before the back edge), so it never conflicts with anything.
    let faults = overlaps_in(
        "struct Obj {}\nuseMut(r: &mut Obj) {}\nf(cond: bool) {\n    mut var := Obj{}\n    for cond {\n        r := &mut var\n        useMut(r)\n    }\n}\n",
    );
    assert!(
        faults.is_empty(),
        "a borrow confined to a single pass through the loop body should never be flagged, got {:#?}",
        faults
    );
}

#[test]
fn check_borrow_overlaps_flags_two_mutable_borrows_nested_inside_a_loop_body() {
    // A pre-loop `&mut` borrow (`r1`) stays live across every statement of
    // the loop body, including a conditionally-taken inner `if` that itself
    // creates a second `&mut` borrow (`r2`) of the same place — a real,
    // nested-live-mutable-borrows conflict, only reachable through a
    // for loop's own body, that the old "any for loop ⇒ skip" gate would
    // have missed entirely (`check_borrow_overlaps` never analyzed a single
    // statement of any function containing a `for` before this slice).
    let faults = overlaps_in(
        "struct Obj {}\nuseMut(r: &mut Obj) {}\nf(cond: bool, branch: bool) {\n    mut var := Obj{}\n    r1 := &mut var\n    for cond {\n        if branch {\n            r2 := &mut var\n            useMut(r2)\n        }\n        useMut(r1)\n    }\n}\n",
    );
    assert_eq!(
        faults.len(),
        1,
        "expected the nested mutable borrows inside the loop body to be flagged, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::OverlappingBorrows));
}

#[test]
fn check_borrow_overlaps_allows_two_mutable_borrows_of_disjoint_fields() {
    // The core case for per-field disjointness: `ra`/`rb` are both `&mut`,
    // both live at the same time, but of two different fields of the same
    // struct — genuinely disjoint memory, so this must be accepted even
    // though a whole-locals-only check would (wrongly) flag it.
    let faults = overlaps_in(
        "struct Pair {\n    a: int\n    b: int\n}\nuseMut(r: &mut int) {}\nf() {\n    mut var := Pair{a: 1, b: 2}\n    ra := &mut var.a\n    rb := &mut var.b\n    useMut(ra)\n    useMut(rb)\n}\n",
    );
    assert!(
        faults.is_empty(),
        "two mutable borrows of disjoint fields should never be flagged, got {:#?}",
        faults
    );
}

#[test]
fn check_borrow_overlaps_flags_two_mutable_borrows_of_the_same_field() {
    // Same shape as the disjoint-fields case, but both borrows target the
    // exact same field — proves field-disjointness isn't over-broad; a
    // real conflict on the same field must still be caught.
    let faults = overlaps_in(
        "struct Pair {\n    a: int\n    b: int\n}\nuseMut(r: &mut int) {}\nf() {\n    mut var := Pair{a: 1, b: 2}\n    r1 := &mut var.a\n    r2 := &mut var.a\n    useMut(r1)\n    useMut(r2)\n}\n",
    );
    assert_eq!(
        faults.len(),
        1,
        "expected two overlapping mutable borrows of the same field to be flagged, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::OverlappingBorrows));
}

#[test]
fn check_borrow_overlaps_flags_a_whole_struct_borrow_overlapping_a_field_borrow() {
    // A whole-struct borrow is a prefix of every one of its own fields'
    // places, so it must still conflict with a live field borrow — the
    // "one place is a prefix of the other ⇒ overlapping" half of this
    // slice's own scoping, not just the "genuinely disjoint fields" half.
    let faults = overlaps_in(
        "struct Pair {\n    a: int\n    b: int\n}\nuseMutPair(r: &mut Pair) {}\nuseMutField(r: &mut int) {}\nf() {\n    mut var := Pair{a: 1, b: 2}\n    rWhole := &mut var\n    rField := &mut var.a\n    useMutPair(rWhole)\n    useMutField(rField)\n}\n",
    );
    assert_eq!(
        faults.len(),
        1,
        "expected the whole-struct borrow to still conflict with the field borrow, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::OverlappingBorrows));
}

// Loan-based conflict tests: a `Ref` creates a loan of a place, every
// reference derived from it (copies, aliases joined across branches, fields
// it is stored in, call results, reborrows) carries that loan, and an access
// to an overlapping place while a conflicting loan is still needed is an
// error unless the access goes through the loan itself.
const LOAN_TYPES: &str = "struct Obj {\n    n: int\n}\nstruct Holder {\n    r: &mut Obj\n}\nstruct Inner {\n    n: int\n    grow(&mut this) {\n        this.n = this.n + 1\n    }\n}\nstruct Outer {\n    inner: Inner\n    count: int\n}\nuseRef(r: &Obj) {}\nuseMut(r: &mut Obj) {}\nid(r: &mut Obj): &mut Obj {\n    return r\n}\n";

fn loan_faults(function: &str) -> Vec<crate::fault::MirFault> {
    overlaps_in(&format!("{LOAN_TYPES}{function}"))
}

#[test]
fn check_borrow_overlaps_flags_a_loan_carried_by_an_alias_joined_across_branches() {
    let faults = loan_faults(
        "f(c: bool) {\n    mut a := Obj{n: 1}\n    mut b := Obj{n: 2}\n    mut r := &mut a\n    if c {\n        r = &mut b\n    }\n    s := r\n    t := &a\n    useMut(s)\n    useRef(t)\n}\n",
    );
    assert_single_fault(
        &faults,
        MirErrorKind::OverlappingBorrows,
        "&a while s may still carry &mut a to be flagged",
    );
}

#[test]
fn check_borrow_overlaps_flags_two_mutable_reborrows_through_a_parameter() {
    let faults = loan_faults(
        "f(p: &mut Obj) {\n    x := &mut *p\n    y := &mut *p\n    useMut(x)\n    useMut(y)\n}\n",
    );
    assert_single_fault(
        &faults,
        MirErrorKind::OverlappingBorrows,
        "a second &mut *p while the first is live to be flagged",
    );
}

#[test]
fn check_borrow_overlaps_flags_a_parent_reference_used_while_its_reborrow_is_live() {
    let faults =
        loan_faults("f(p: &mut Obj) {\n    x := &mut *p\n    useMut(p)\n    useMut(x)\n}\n");
    assert_single_fault(
        &faults,
        MirErrorKind::OverlappingBorrows,
        "using p while &mut *p is still needed to be flagged",
    );
}

#[test]
fn check_borrow_overlaps_flags_a_loan_carried_through_a_struct_field() {
    let faults = loan_faults(
        "f() {\n    mut a := Obj{n: 1}\n    ra := &mut a\n    h := Holder{r: ra}\n    q := h.r\n    t := &a\n    useMut(q)\n    useRef(t)\n}\n",
    );
    assert_single_fault(
        &faults,
        MirErrorKind::OverlappingBorrows,
        "&a while q still carries &mut a to be flagged",
    );
}

#[test]
fn check_borrow_overlaps_flags_a_loan_carried_by_a_call_result() {
    let faults = loan_faults(
        "f() {\n    mut a := Obj{n: 1}\n    ra := &mut a\n    r := id(ra)\n    t := &a\n    useMut(r)\n    useRef(t)\n}\n",
    );
    assert_single_fault(
        &faults,
        MirErrorKind::OverlappingBorrows,
        "&a while id's result still carries &mut a to be flagged",
    );
}

#[test]
fn check_borrow_overlaps_flags_a_read_of_a_place_while_it_is_mutably_borrowed() {
    let faults = loan_faults(
        "f(): int {\n    mut a := Obj{n: 1}\n    r := &mut a\n    x := a.n\n    useMut(r)\n    return x\n}\n",
    );
    assert_single_fault(
        &faults,
        MirErrorKind::OverlappingBorrows,
        "reading a.n under a live &mut a to be flagged",
    );
}

#[test]
fn check_borrow_overlaps_flags_a_write_to_a_place_while_it_is_borrowed() {
    let faults =
        loan_faults("f() {\n    mut a := Obj{n: 1}\n    r := &a\n    a.n = 2\n    useRef(r)\n}\n");
    assert_single_fault(
        &faults,
        MirErrorKind::OverlappingBorrows,
        "writing a.n under a live &a to be flagged",
    );
}

#[test]
fn check_borrow_overlaps_allows_a_parent_reference_used_after_its_reborrows_last_use() {
    let faults =
        loan_faults("f(p: &mut Obj) {\n    x := &mut *p\n    useMut(x)\n    useMut(p)\n}\n");
    assert!(
        faults.is_empty(),
        "using p after &mut *p is done should be accepted, got {faults:#?}"
    );
}

#[test]
fn check_borrow_overlaps_allows_two_shared_reborrows_through_a_parameter() {
    let faults =
        loan_faults("f(p: &Obj) {\n    x := &*p\n    y := &*p\n    useRef(x)\n    useRef(y)\n}\n");
    assert!(
        faults.is_empty(),
        "two shared reborrows should be accepted, got {faults:#?}"
    );
}

#[test]
fn check_borrow_overlaps_allows_a_mutable_method_call_on_a_field_of_a_mutable_parameter() {
    // The motivating reborrow: `o.inner.grow()` borrows `(*o).inner` for
    // the call only, so writing `o.count` afterwards is fine.
    let faults = loan_faults("f(o: &mut Outer) {\n    o.inner.grow()\n    o.count = 1\n}\n");
    assert!(
        faults.is_empty(),
        "a field method call then a sibling write should be accepted, got {faults:#?}"
    );
}

#[test]
fn check_borrow_overlaps_allows_writing_through_a_mutable_borrow_while_it_is_live() {
    // An access *through* the loan is never a conflict with that loan.
    let faults = loan_faults(
        "f() {\n    mut a := Obj{n: 1}\n    r := &mut a\n    r.n = 2\n    useMut(r)\n}\n",
    );
    assert!(
        faults.is_empty(),
        "writing through the live borrow itself should be accepted, got {faults:#?}"
    );
}

#[test]
fn check_borrow_overlaps_allows_reading_a_place_after_its_mutable_borrow_ends() {
    let faults = loan_faults(
        "f(): int {\n    mut a := Obj{n: 1}\n    r := &mut a\n    useMut(r)\n    return a.n\n}\n",
    );
    assert!(
        faults.is_empty(),
        "reading a after r's last use should be accepted, got {faults:#?}"
    );
}

// `c.set(c.get())`: the `&mut` receiver of `set` must only be created once
// its arguments are evaluated, so `get`'s shared receiver never overlaps it.
const COUNTER: &str = "struct Counter {\n    n: int\n    get(&this): int {\n        return this.n\n    }\n    set(&mut this, v: int) {\n        this.n = v\n    }\n}\n";

#[test]
fn mutable_receiver_ref_is_created_after_the_call_arguments_are_evaluated() {
    let mir = lower_source(
        &format!("{COUNTER}f() {{\n    mut c := Counter{{n: 1}}\n    c.set(c.get())\n}}\n"),
        "f",
    )
    .expect("expected successful lowering");

    let set_block = mir
        .blocks
        .values()
        .find(|block| {
            matches!(&block.terminator, mir_model::Terminator::Call { arguments, .. } if arguments.len() == 2)
        })
        .expect("expected the two-argument call to `set`");
    let creates_mutable_receiver = set_block.statements.iter().any(|statement| {
        matches!(
            statement,
            mir_model::Statement::Assign(_, Rvalue::Ref { mutable: true, .. })
        )
    });
    assert!(
        creates_mutable_receiver,
        "expected the &mut receiver to be created in the block that calls `set`, after `get` returns"
    );
}

#[test]
fn check_borrow_overlaps_allows_a_mutable_method_call_whose_argument_reads_the_receiver() {
    let faults = overlaps_in(&format!(
        "{COUNTER}f() {{\n    mut c := Counter{{n: 1}}\n    c.set(c.get())\n}}\n"
    ));
    assert!(
        faults.is_empty(),
        "c.set(c.get()) should be accepted, got {faults:#?}"
    );
}

#[test]
fn check_move_while_borrowed_flags_a_move_while_a_shared_borrow_is_still_needed() {
    // The core case, and proof the conflict rule really is mutability-
    // independent (unlike `check_borrow_overlaps`'s own matrix): `r` is only
    // ever a *shared* borrow of `var`, but `var` is moved into `consume`
    // while `r` is still needed for the `useRef(r)` right after — a real
    // conflict, since `r` would dereference storage that's already gone.
    let faults = moves_while_borrowed_in(
        "struct Obj {}\nconsume(o: Obj) {}\nuseRef(r: &Obj) {}\nf() {\n    var := Obj{}\n    r := &var\n    consume(var)\n    useRef(r)\n}\n",
    );
    assert_eq!(
        faults.len(),
        1,
        "expected the move-while-still-borrowed conflict to be flagged, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::MoveWhileBorrowed));
}

#[test]
fn check_move_while_borrowed_allows_a_move_after_the_borrows_last_use() {
    // Same shape, but `r`'s own last (and only) read happens strictly
    // before the move — `r` is no longer live by the time `var` is moved,
    // so this is legal: real NLL-style liveness, not "the borrow's lexical
    // scope hasn't ended yet."
    let faults = moves_while_borrowed_in(
        "struct Obj {}\nconsume(o: Obj) {}\nuseRef(r: &Obj) {}\nf() {\n    var := Obj{}\n    r := &var\n    useRef(r)\n    consume(var)\n}\n",
    );
    assert!(
        faults.is_empty(),
        "a move after the borrow's own last use should be accepted, got {:#?}",
        faults
    );
}

// Projected moves against live borrows: moving `b.p` conflicts with any
// live borrow overlapping it (`&b`, `&b.p`, or an alias of either), but not
// with a borrow of a disjoint sibling field.
fn field_move_while_borrowed_faults(function: &str) -> Vec<crate::fault::MirFault> {
    let source = format!(
        "{PARTIAL_MOVE_TYPES}useBox(r: &Box2) {{}}\nuseTwo(r: &Two) {{}}\nuseP(r: &*int) {{}}\n{function}"
    );
    moves_while_borrowed_in(&source)
}

#[test]
fn check_move_while_borrowed_flags_a_field_moved_while_its_struct_is_borrowed() {
    let faults = field_move_while_borrowed_faults(
        "f() {\n    n := 1\n    p := new(n)\n    b := Box2{p: p}\n    r := &b\n    consume(b.p)\n    useBox(r)\n}\n",
    );
    assert_single_fault(
        &faults,
        MirErrorKind::MoveWhileBorrowed,
        "moving b.p while &b is live to be flagged",
    );
}

#[test]
fn check_move_while_borrowed_flags_a_field_moved_while_that_field_is_borrowed() {
    let faults = field_move_while_borrowed_faults(
        "f() {\n    n := 1\n    p := new(n)\n    b := Box2{p: p}\n    r := &b.p\n    consume(b.p)\n    useP(r)\n}\n",
    );
    assert_single_fault(
        &faults,
        MirErrorKind::MoveWhileBorrowed,
        "moving b.p while &b.p is live to be flagged",
    );
}

#[test]
fn check_move_while_borrowed_flags_a_field_moved_while_an_alias_of_its_struct_is_live() {
    let faults = field_move_while_borrowed_faults(
        "f() {\n    n := 1\n    p := new(n)\n    b := Box2{p: p}\n    r := &b\n    alias := r\n    consume(b.p)\n    useBox(alias)\n}\n",
    );
    assert_single_fault(
        &faults,
        MirErrorKind::MoveWhileBorrowed,
        "moving b.p while an alias of &b is live to be flagged",
    );
}

#[test]
fn check_move_while_borrowed_allows_a_field_moved_while_a_disjoint_field_is_borrowed() {
    let faults = field_move_while_borrowed_faults(
        "f() {\n    n := 1\n    m := 2\n    pa := new(n)\n    pb := new(m)\n    t := Two{a: pa, b: pb}\n    r := &t.b\n    consume(t.a)\n    useP(r)\n}\n",
    );
    assert!(
        faults.is_empty(),
        "a borrow of a disjoint field shouldn't block the move, got {faults:#?}"
    );
}

#[test]
fn check_move_while_borrowed_allows_a_field_moved_after_the_borrows_last_use() {
    let faults = field_move_while_borrowed_faults(
        "f() {\n    n := 1\n    p := new(n)\n    b := Box2{p: p}\n    r := &b\n    useBox(r)\n    consume(b.p)\n}\n",
    );
    assert!(
        faults.is_empty(),
        "a move after the borrow's last use should be accepted, got {faults:#?}"
    );
}
