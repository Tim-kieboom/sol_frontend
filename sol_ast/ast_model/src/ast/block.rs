use sol_utils::{collections::array::RcArr, impl_sol_ids, span::Span};

use crate::StatementId;

impl_sol_ids!(BlockId);

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Block {
    pub statements: RcArr<StatementId>,
    pub is_const: bool,
    pub span: Span,
}
