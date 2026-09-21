use ast_model::{Expression, ExpressionId, TypeId};
use sol_tokenizer::model::TokenKind;
use sol_utils::{
    collections::try_result::{ResultTryErr, TryErr, TryError, TryNotValue, TryOk},
    sol_error_internal,
};

use crate::{
    fault::AstTryResult,
    parser::Parser,
    utils::{ARROW_LEFT, ARROW_RIGHT, ASSIGN, COMMA},
};

pub mod expression;
pub mod function;
pub mod parse_module;
pub mod sol_type;
pub mod statements;

impl<'a, 'f> Parser<'a, 'f> {
    /// Parses a `<T, U, ..>` generic argument list, interning each argument
    /// as it's parsed — every caller only ever embeds these into another
    /// interned type (a `Stub`, `RawPtr`, `Res`) or an AST node's own
    /// `generics` field, never inspects the parsed `SolType` structurally.
    pub(crate) fn parse_generic_define(
        &mut self,
    ) -> AstTryResult<Vec<TypeId>, crate::fault::AstFault> {
        let start_position = self.tokens.current_position();

        self.expect(&ARROW_LEFT).try_err()?;
        let mut types = vec![];
        loop {
            if let TokenKind::Ident(_) = self.token().kind
                && self.peek_is(&ASSIGN)
            {
                self.bump();
                self.bump();
                let value = match self.try_parse_type() {
                    Ok(val) => val,
                    Err(TryError::IsErr(err)) => return TryErr(err),
                    Err(TryError::IsNotValue(err)) => {
                        return TryNotValue(err);
                    }
                };
                types.push(self.intern_type(value));
                if self.current_is(&ARROW_RIGHT) {
                    self.bump();
                    break;
                }
                if !self.current_is(&COMMA) {
                    self.goto(start_position);
                    return TryNotValue(self.get_expect_error(&COMMA));
                }
                self.bump();
                continue;
            }

            let ty = match self.try_parse_type() {
                Ok(val) => val,
                Err(TryError::IsErr(err)) => return TryErr(err),
                Err(TryError::IsNotValue(err)) => {
                    return TryNotValue(err);
                }
            };
            types.push(self.intern_type(ty));

            if self.current_is(&ARROW_RIGHT) {
                self.bump();
                break;
            }

            if !self.current_is(&COMMA) {
                self.goto(start_position);
                return TryNotValue(self.get_expect_error(&COMMA));
            }
            self.bump();
        }
        TryOk(types)
    }

    pub(crate) fn get_forest_expression(
        &self,
        id: ExpressionId,
    ) -> Result<&Expression, crate::fault::AstFault> {
        self.forest
            .store
            .expressions
            .get(id)
            .ok_or_else(|| sol_error_internal!(format!("{id:?} not found"), None).into_kind())
    }
}
