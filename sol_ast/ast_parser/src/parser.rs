use ast_model::{
    AstModuleStore, Block, BlockId, CrateForest, ExternLanguage, Module, SolType,
    declare_store::DeclareStore,
};
use sol_tokenizer::TokenStream;
#[cfg(debug_assertions)]
use sol_tokenizer::model::Token;
use sol_utils::{CrateContext, collections::vec_set::VecSet, sol_error_internal};
use sol_utils::{
    collections::{crate_store::CrateStore, module_store::ModuleStore},
    span::ModuleId,
};
use std::{collections::HashMap, path::PathBuf};

use crate::ParseInfo;

/// struct used to easily see debug info about current state of Parser can be ignored outside of debug
#[cfg(debug_assertions)]
#[derive(Debug, Clone)]
pub(crate) struct DebugViewer {
    pub(crate) current_index: usize,
    pub(crate) current: Token,
}

#[derive(Debug, Default)]
pub(crate) struct Current {
    pub(crate) this_type: Option<SolType>,
    /// `Some(lang)` while parsing inside an `extern "C" ( ... )` block, so that
    /// the next statement is forced to parse as another extern signature
    /// instead of a normal statement. See [`crate::parse::function`]'s
    /// `parse_extern_signature_statement`.
    pub(crate) extern_block: Option<ExternLanguage>,
}

/// Recursive descent parser that builds AST from token stream.
///
/// Manages token consumption, error recovery, scope tracking, and debug
/// information (debug builds only). Supports position save/restore for
/// backtracking during parsing.
#[derive(Debug)]
pub(crate) struct Parser<'a, 'f> {
    #[cfg(debug_assertions)]
    pub(crate) debug: DebugViewer,

    pub(crate) id: ModuleId,
    pub(crate) current: Current,
    pub(crate) source_path: PathBuf,
    pub(crate) tokens: TokenStream<'a>,
    pub(crate) crate_source_path: PathBuf,
    pub(crate) modules: &'f mut ModuleStore,
    pub(crate) context: &'f mut CrateContext<crate::fault::AstErrorKind>,
    pub(crate) forest: &'f mut CrateForest,
    pub(crate) crate_store: &'f CrateStore,
    pub(crate) declares: &'f mut DeclareStore,
}
impl<'a, 'f> Parser<'a, 'f> {
    pub fn parse(tokens: TokenStream<'a>, name: String, info: ParseInfo<'f>) {
        let id = info.id;
        let parent = info.parent;

        let module = Module {
            id,
            name,
            parent,
            modules: VecSet::new(),
            global: BlockId::ERROR,
            header: HashMap::default(),
        };
        info.forest.modules_mut().insert(id, module);

        let mut this = Self::new(tokens, info);

        #[cfg(debug_assertions)]
        {
            this.debug.current = this.token().clone();
            this.debug.current_index = this.tokens.index();
        }

        let statements = this.parse_global_statements().into();
        let global = this.forest.store.insert_block(Block {
            statements,
            is_const: false,
            span: this.token().span,
        });

        match this.forest.modules_mut().get_mut(id) {
            Some(module) => module.global = global,
            None => {
                this.log_fault(sol_error_internal!(format!("{id:?} not found"), None).into_kind())
            }
        }
    }

    pub(crate) fn modules_mut(&mut self) -> &mut AstModuleStore {
        self.forest.modules_mut()
    }

    pub(crate) fn modules(&self) -> &AstModuleStore {
        self.forest.modules()
    }

    fn new(tokens: TokenStream<'a>, info: ParseInfo<'f>) -> Self {
        #[cfg(debug_assertions)]
        let debug = {
            use sol_tokenizer::model::TokenKind;
            use sol_utils::span::Span;

            DebugViewer {
                current: Token::new(TokenKind::EndLine, Span::error()),
                current_index: 0,
            }
        };

        Self {
            #[cfg(debug_assertions)]
            debug,

            tokens,
            id: info.id,
            context: info.context,
            modules: info.modules,
            forest: info.forest,
            source_path: info.source_folder,
            crate_source_path: info.crate_source_folder,
            crate_store: info.crate_store,
            declares: info.declares,
            current: Current::default(),
        }
    }

    /// Interns `ty`, returning its canonical [`ast_model::TypeId`] — the one
    /// place a `SolType` an AST node stores actually gets built into that
    /// node, so every such site goes through here instead of storing the
    /// `SolType` value directly.
    pub(crate) fn intern_type(&mut self, ty: SolType) -> ast_model::TypeId {
        self.declares.intern_type(ty)
    }
}
