use soul_utils::{collections::array::Arr, impl_soul_ids, span::Span};

use crate::StatementId;

impl_soul_ids!(BlockId);

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Block {
    pub statements: Arr<StatementId>,
    pub is_const: bool,
    pub span: Span,
}
