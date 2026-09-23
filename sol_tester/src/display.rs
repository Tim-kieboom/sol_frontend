use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::Path,
};

use anyhow::Result;
use ast_model::{AstStore, AstTree, declare_store::DeclareStore};
use mir_model::MirProgram;
use sol_displayer::{
    ast::display_ast_tree, tokenizer::display_tokens, vecmap_to_json_str, writer::Writer,
};
use sol_tokenizer::TokenStream;
use sol_utils::collections::module_store::ModuleStore;

use crate::config;

pub(crate) fn display_ast(tree: &AstTree) -> Result<()> {
    if !config::CONFIG.print_debug_output() {
        return Ok(());
    }

    let mut output_path = config::CONFIG.output_path().join("ast");
    output_path.push("tree.solc");

    let mut writer = write_create_file(&output_path)?;
    display_ast_tree(tree, config::CONFIG.source_path(), &mut writer)?;

    output_path.pop();
    output_path.push("json");

    let modules = vecmap_to_json_str(tree.crates.modules.as_vecmap())?;
    let externals = serde_json::to_string_pretty(&tree.crates.external)?;
    let scope_info = vecmap_to_json_str(tree.scope_info.scopes.as_vecmap())?;

    let blocks = vecmap_to_json_str(&tree.crates.store.blocks)?;
    let functions = vecmap_to_json_str(&tree.crates.store.functions)?;
    let statements = vecmap_to_json_str(&tree.crates.store.statements)?;
    let expressions = vecmap_to_json_str(&tree.crates.store.expressions)?;

    write_to_file(&output_path.join("modules.json"), &modules)?;
    write_to_file(&output_path.join("externals.json"), &externals)?;
    write_to_file(&output_path.join("scope_info.json"), &scope_info)?;

    output_path.push("store");
    write_to_file(&output_path.join("blocks.json"), &blocks)?;
    write_to_file(&output_path.join("functions.json"), &functions)?;
    write_to_file(&output_path.join("statements.json"), &statements)?;
    write_to_file(&output_path.join("expressions.json"), &expressions)?;

    Ok(())
}

pub(crate) fn display_mir(
    program: &MirProgram,
    ast: &AstStore,
    declares: &DeclareStore,
) -> Result<()> {
    if !config::CONFIG.print_debug_output() {
        return Ok(());
    }

    let mut output_path = config::CONFIG.output_path().join("mir");
    output_path.push("tree.solc");

    let mut writer = write_create_file(&output_path)?;
    let mut dispayer = sol_displayer::mir::Displayer::new(&mut writer, ast, declares);
    for (_, function) in program.functions.entries() {
        dispayer.write_function(function)?;
        dispayer.push_char('\n')?;
    }
    dispayer.writer_flush()?;

    output_path.pop();
    output_path.push("json");
    let functions = vecmap_to_json_str(&program.functions)?;
    write_to_file(&output_path.join("functions.json"), &functions)?;

    Ok(())
}

pub(crate) fn display_tokenizer<'a>(tokens: &TokenStream<'a>, modules: &ModuleStore) -> Result<()> {
    if !config::CONFIG.print_debug_output() {
        return Ok(());
    }

    inner_display_tokenizer(tokens, modules)
        .map_err(|err| anyhow::anyhow!("in display_tokenizer: {err}"))
}

fn inner_display_tokenizer<'a>(tokens: &TokenStream<'a>, modules: &ModuleStore) -> Result<()> {
    let mut output_path = config::CONFIG.output_path().join("tokenizer");
    output_path.push("tokens.solc");

    let mut writer = write_create_file(&output_path)?;
    display_tokens(tokens.clone(), modules, &mut writer, &config::PRINT_CONFIGS)?;
    Ok(())
}

fn write_to_file(path: &Path, str: &str) -> Result<()> {
    let mut file = write_create_file(path)?;
    file.push_str(str)?;
    file.flush()?;
    Ok(())
}

fn write_create_file(path: &Path) -> Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|err| anyhow::anyhow!("Failed to create output file({path:?}): {}", err))
}
