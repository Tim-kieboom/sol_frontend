//! Driver that runs the MIR-lowering stage over an already-resolved `AstTree`,
//! mirroring `ast_run::to_ast` — that crate wires tokenizer+parser+resolver
//! together, this one wires the (much narrower, see `mir_parser`) MIR-lowering
//! pass on top of its output. Kept as a separate stage rather than folded into
//! `to_ast` itself: MIR lowering is a genuinely separate pipeline step (see
//! `docs/mir-design.md`), and today's lowering pass only covers a small subset
//! of the language, so its failures shouldn't be conflated with "the program
//! failed to parse/resolve."
//!
//! Failures use the same fault-reporting mechanism as every other stage: a
//! function this slice can't lower doesn't return an error to the caller here,
//! it pushes a `Fault` into `context` (see `mir_parser`'s docs for why that's
//! the expected, non-fatal outcome for most of the language today) — so a
//! caller collects/prints MIR faults exactly the way it already collects parse
//! and resolve faults, rather than a separate ad hoc error list.

#[cfg(test)]
mod tests;

use std::time::Instant;

use ast_model::AstTree;
use mir_model::MirProgram;
use mir_parser::{MirLowerer, borrow_checker, fault::MirErrorKind};
use sol_utils::{
    CrateContext, collections::benchmark::Benchmark, compiler_options::CompilerOptions,
};

pub fn to_mir(
    ast: &mut AstTree,
    benchmark: &mut Benchmark,
    context: &mut CrateContext<MirErrorKind>,
    options: &CompilerOptions,
) -> MirProgram {
    let time = Instant::now();

    // No pre-filter by `FunctionKind` here: `lower_function` itself already
    // handles every case (a normal body, an `extern "C"` declaration, or a
    // non-extern signature-only stub it correctly faults on).
    let mut lowerer = MirLowerer::new(&ast.crates.store, &mut ast.declares, options);
    for (id, _) in ast.crates.store.functions.entries() {
        if let Err(fault) = lowerer.lower_function(id) {
            context.faults.push(fault);
        }
    }

    benchmark.add_benchmark("mir", time.elapsed());
    let (mut functions, externs) = lowerer.into_functions_and_externs();

    // Move checking and drop-elaboration (see `mir_parser::move_check`) are
    // their own passes over already-lowered MIR, not part of lowering
    // itself — a function that fails to lower never reaches here at all, so
    // these only ever run on functions that already lowered successfully.
    // `elaborate_drops` runs first: `mir_parser::function`'s own lowering
    // always emits an unconditional `Drop` for every tracked owning local,
    // move-unaware by design (see that module's own docs) — this is what
    // turns some of those into a no-op once the real, whole-function
    // dataflow proves the value might already be gone. Order relative to
    // `check_moves` doesn't matter: `check_moves` never reads a `Drop`
    // terminator's own shape, only `Operand`/`Assign` occurrences, which
    // `elaborate_drops` never touches.
    for function in functions.values_mut() {
        borrow_checker::elaborate_drops(function);
    }
    for function in functions.values() {
        for fault in borrow_checker::check_moves(function) {
            context.faults.push(fault);
        }
    }

    // Escape checking (see `mir_parser::escape_check`) — does a function
    // return a reference to storage that doesn't outlive it. A separate
    // concern from move-checking (aliasing vs. lifetime), so its own pass;
    // `lowerer` has already been consumed above, freeing its `&mut
    // DeclareStore` borrow, so `ast.declares` can be read here.
    for function in functions.values() {
        for fault in borrow_checker::check_escapes(function, &ast.declares) {
            context.faults.push(fault);
        }
    }

    // Overlap checking (see `mir_parser::borrow_checker::overlap_check`) —
    // do two live borrows of the same local conflict (`&mut` vs anything
    // else). A third, separate concern from move-checking and
    // escape-checking; doesn't need `declares` at all.
    for function in functions.values() {
        for fault in borrow_checker::check_borrow_overlaps(function) {
            context.faults.push(fault);
        }
    }

    MirProgram { functions, externs }
}
