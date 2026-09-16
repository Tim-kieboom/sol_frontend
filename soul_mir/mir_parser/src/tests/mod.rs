use std::path::PathBuf;

use ast_model::{
    ArrayKind, ArrayType, AstStore, AstTree, FunctionKind, ReferenceType, SoulType, TypeId,
    declare_store::DeclareStore,
};
use ast_parser::{ParseInfo, parse_module};
use mir_model::{ConstValue, Operand, Rvalue};
use soul_name_resolver::name_resolve;
use soul_tokenizer::to_token_stream;
use soul_utils::{
    FunctionId, Mutable,
    collections::{crate_store::CrateStore, module_store::ModuleStore},
    compiler_options::{CompilerOptions, MirOptions},
    soul_names::PrimitiveTypes,
};

use crate::{
    MirLowerer,
    fault::{MirErrorKind, MirResult},
    function::FunctionLowerer,
};

// Tests exercise the checked-arithmetic/bounds-check MIR shapes, so run with
// every `MirOptions` flag on — matching `mir_run`'s own test fixtures, and
// unlike `soul_tester`'s production config, which currently defaults both
// off (see `soul_tester::config::COMPILER_OPTIONS`).
const OPTIONS: CompilerOptions = CompilerOptions {
    mir: MirOptions::all(),
    ..CompilerOptions::const_default()
};
fn create_lowerer(ast: &mut AstTree) -> MirLowerer<'_> {
    MirLowerer::new(&ast.crates.store, &mut ast.declares, &OPTIONS)
}

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
        soul_utils::TypeModifier::Immut,
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
            Some(ast_model::SoulType::Primitive(
                soul_utils::soul_names::PrimitiveTypes::CStr
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
        let mir_model::Terminator::Drop { place, target } = &block.terminator else {
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
        let mir_model::Terminator::Drop { place, target } = &block.terminator else {
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

fn function_span(ast: &AstTree, name: &str) -> soul_utils::span::Span {
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
        .intern_type(SoulType::Primitive(PrimitiveTypes::Int));
    let ref_ty = ast.declares.intern_type(SoulType::Reference(ReferenceType {
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
    // Unlike `&T`/`RawPtr<T>`, `*T` (`SoulType::Pointer`) is an *owning*
    // heap pointer — `new(expr)`'s own result type, freed by its `Drop` —
    // so it's move-only, same as a struct: copying it would produce two
    // "owners" of the same allocation.
    let mut ast = resolve_source("f(): none {}\n");
    let span = function_span(&ast, "f");
    let ptr_ty = ast.declares.intern_type(SoulType::Pointer(ReferenceType {
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
    let mut_slice_ty = ast.declares.intern_type(SoulType::Array(ArrayType {
        of_type: TypeId::NONE,
        kind: ArrayKind::MutSlice,
    }));
    let const_slice_ty = ast.declares.intern_type(SoulType::Array(ArrayType {
        of_type: TypeId::NONE,
        kind: ArrayKind::ConstSlice,
    }));
    let stack_array_ty = ast.declares.intern_type(SoulType::Array(ArrayType {
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
    let any_ty = ast.declares.intern_type(SoulType::Any);
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
fn a_moved_body_local_is_excluded_from_its_own_scopes_drop_chain() {
    // `s` is moved into `consume(s)` — its own scope's `Drop` chain (the
    // function's own top-level frame, unwound at the implicit end-of-body
    // `return`) must skip it: once `Terminator::Drop` actually frees an
    // owning `*T` (see M2's TODO.md entry), dropping an already-moved local
    // would double-free it.
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
        !has_drop,
        "expected the moved-out s to have no Drop terminator at all, got {:#?}",
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
        let mir_model::Terminator::Drop { place, target } = &block.terminator else {
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
fn a_conditional_move_in_only_one_if_branch_leaks_on_the_untaken_path_but_never_double_frees() {
    // `p` is declared *outside* the `if`, and moved into `consume(p)` in
    // only the `then` branch — the `else` branch (here, simply absent)
    // never touches it. `moved` isn't saved/restored per branch (see its
    // own docs on `FunctionLowerer`): lowering the `then` branch marks `p`
    // moved, and that state is never reset before `f`'s own top-level frame
    // (which is what actually owns `p`) unwinds at the end-of-body return.
    // So `p` gets **no** `Drop` at all, regardless of which branch actually
    // ran at runtime — correct (no double free) on the `cond == true` path,
    // where `consume` already owns `p`, but a real leak on the `cond ==
    // false` path, where `p` was never moved and nothing ever frees it.
    // This is the documented, accepted imprecision of Slice 3b (see
    // `TODO.md`'s M2 entry) — full per-path precision needs real dataflow,
    // not implemented yet.
    let mir = lower_source(
        "consume(p: *int) {}\nf(cond: bool) {\n    p := new(1)\n    if cond {\n        consume(p)\n    }\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let has_drop = mir
        .blocks
        .entries()
        .any(|(_, block)| matches!(block.terminator, mir_model::Terminator::Drop { .. }));
    assert!(
        !has_drop,
        "expected p to have no Drop at all (the documented leak-not-crash \
         imprecision for a conditionally-moved outer-scope local), got {:#?}",
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
fn a_move_only_argument_reached_through_a_field_projection_is_still_copied() {
    // Scoped deliberately: `SetDropFlag` is per-`LocalId`, not per-place, so
    // a field-projection move (`consume(container.item)`) has no sound way
    // to express "only this one field moved" yet — see `lower_call_operand`'s
    // docs. Falls through to a plain `Copy`, same as before Move existed.
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
        Operand::Copy(place) if matches!(place.projection.as_slice(), [mir_model::PlaceElem::Field(0)])
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
    let later_steps: std::collections::HashSet<mir_model::BlockId> = exit_chain_drops
        .iter()
        .filter_map(|id| match &mir.blocks[*id].terminator {
            mir_model::Terminator::Drop { target, .. } => Some(*target),
            _ => None,
        })
        .collect();
    let entry = exit_chain_drops
        .iter()
        .copied()
        .find(|id| !later_steps.contains(id));

    // Walk forward from the entry point, collecting exactly which locals
    // break's own chain drops, in order.
    let mut drops = Vec::new();
    let mut current = entry;
    while let Some(id) = current {
        let block = &mir.blocks[id];
        let mir_model::Terminator::Drop { place, target } = &block.terminator else {
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

    let faults = crate::move_check::check_moves(&mir);
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

    let faults = crate::move_check::check_moves(&mir);
    assert!(
        faults.is_empty(),
        "expected no faults — s was reassigned before its second use, got {:#?}",
        faults
    );
}

#[test]
fn check_moves_skips_functions_containing_a_branch() {
    // Straight-line only for now — a branching function isn't analyzed at
    // all yet (see `move_check`'s own docs), so even a pattern that would be
    // a real violation on some individual path is silently unchecked here,
    // rather than incorrectly flagged or crashed on.
    let mir = lower_source(
        "struct Session {\n    n: int\n}\nconsume(s: Session) {}\nf(cond: bool) {\n    s := Session{n: 1}\n    if cond {\n        consume(s)\n    } else {\n        consume(s)\n    }\n}\n",
        "f",
    )
    .expect("expected successful lowering");

    let faults = crate::move_check::check_moves(&mir);
    assert!(
        faults.is_empty(),
        "expected a branching function to be skipped entirely, got {:#?}",
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

    let faults = crate::move_check::check_moves(&mir);
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

    let faults = crate::move_check::check_moves(&mir);
    assert_eq!(
        faults.len(),
        1,
        "expected borrowing an already-moved value to be flagged, got {:#?}",
        faults
    );
    assert!(matches!(faults[0].kind(), MirErrorKind::UseAfterMove));
}

#[test]
fn new_expression_lowers_to_a_heap_alloc_rvalue() {
    let mir = lower_source("f(): int {\n    p := new(5)\n    return *p\n}\n", "f")
        .expect("expected successful lowering");

    let heap_alloc = mir.blocks.entries().find_map(|(_, block)| {
        block.statements.iter().find_map(|statement| {
            let mir_model::Statement::Assign(place, Rvalue::HeapAlloc(_, operand)) = statement
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
            let mir_model::Statement::Assign(_, Rvalue::HeapAlloc(ty, _)) = statement else {
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
        Some(&SoulType::Primitive(PrimitiveTypes::Int)),
        "expected new(1)'s allocated element type to default to int, not stay untyped"
    );
}
