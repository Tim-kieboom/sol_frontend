use ast_model::{Assignment, ExpressionId, SoulType};
use ast_parser::fault::AstErrorKind;
use soul_utils::TypeModifier;

use super::function_call::is_generic_parameter;
use crate::NameResolver;

impl<'a> NameResolver<'a> {
    /// Checks `x: T = value`'s explicit annotation `declared_ty` against
    /// `value`'s own type — an explicit annotation is never inferred (see
    /// `resolve_variable`), so unlike the no-annotation case, nothing else
    /// ever validates it against the initializer.
    pub(crate) fn check_variable_declaration(
        &mut self,
        declared_ty: &SoulType,
        value: ExpressionId,
    ) {
        let empty = vec![].into();
        let generics = match self.current.function {
            Some(id) => self
                .declares
                .get_function(id)
                .map(|(signature, _)| &signature.generics)
                .unwrap_or(&empty),
            None => &empty,
        };

        if is_generic_parameter(declared_ty, generics) {
            return;
        }

        let Some(value_ty) = self.expression_type(value) else {
            return;
        };
        if self.combine_operand_types(&value_ty, declared_ty).is_some() {
            return;
        }

        let span = self.get_expression(value).map(|expr| expr.span);
        self.log_error(
            AstErrorKind::AssignmentTypeMismatch {
                expected: self.print_ty(declared_ty).into(),
                got: self.print_ty(&value_ty).into(),
            },
            span,
        );
    }

    pub(crate) fn check_assignment(&mut self, assignment: &Assignment) {
        let Some((modifier, left_ty)) = self.variable_lvalue(assignment.left) else {
            return;
        };

        if matches!(modifier, TypeModifier::Immut | TypeModifier::Comptime) {
            let span = self.get_expression(assignment.left).map(|expr| expr.span);

            self.log_error(AstErrorKind::AssignToImmutableVariable, span);
        }

        let empty = vec![].into();
        let generics = match self.current.function {
            Some(id) => self
                .declares
                .get_function(id)
                .map(|(signature, _)| &signature.generics)
                .unwrap_or(&empty),
            None => &empty,
        };

        if is_generic_parameter(&left_ty, generics) {
            return;
        }

        let Some(right_ty) = self.expression_type(assignment.right) else {
            return;
        };
        if self.combine_operand_types(&right_ty, &left_ty).is_some() {
            return;
        }

        let span = self.get_expression(assignment.right).map(|expr| expr.span);
        self.log_error(
            AstErrorKind::AssignmentTypeMismatch {
                expected: self.print_ty(&left_ty).into(),
                got: self.print_ty(&right_ty).into(),
            },
            span,
        );
    }
}
