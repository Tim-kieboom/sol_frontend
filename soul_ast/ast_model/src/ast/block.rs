use soul_utils::{collections::array::RcArr, impl_soul_ids, span::Span};

use crate::StatementId;

impl_soul_ids!(BlockId);

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Block {
    pub statements: RcArr<StatementId>,
    pub is_const: bool,
    pub span: Span,
}
