use crate::{
    fault::{MirErrorKind, MirResult},
    function::FunctionLowerer,
};
use ast_model::{self as ast, SoulType, declare_store::DeclareStore};
use mir_model as mir;
use soul_utils::{fault::Fault, span::Span};

impl<'a> FunctionLowerer<'a> {
    /// The Soul type held at `place`, walking its projection the same way
    /// `resolve_field_place`/`resolve_index_place`/`resolve_deref_place`
    /// computed it in the first place — a pure, side-effect-free
    /// re-derivation (no statements emitted, unlike those), used by
    /// `operand_type` to type an already-lowered operand without re-lowering
    /// it. `None` for anything this walk can't resolve (an unresolvable
    /// struct/field, or a `Deref` of something that isn't a reference).
    pub(super) fn place_type(&self, place: &mir::Place) -> Option<SoulType> {
        let mut ty = self
            .declares
            .get_type(self.locals.get(place.local)?.ty)?
            .clone();
        for elem in &place.projection {
            ty = match elem {
                mir::PlaceElem::Field(index) => {
                    if let SoulType::TupleKind(ast::TupleKind::Tuple(types)) = &ty {
                        let id = *types.get(*index)?;
                        self.declares.get_type(id)?.clone()
                    } else {
                        let struct_ = self.resolve_struct(&ty)?;
                        let field_ty_id = struct_.fields.get(*index)?.value.ty?;
                        self.declares.get_type(field_ty_id)?.clone()
                    }
                }
                mir::PlaceElem::Index(_) => {
                    let SoulType::Array(array) = &ty else {
                        return None;
                    };
                    self.declares.get_type(array.of_type)?.clone()
                }
                mir::PlaceElem::Deref => match ty {
                    SoulType::Reference(reference) | SoulType::Pointer(reference) => {
                        self.declares.get_type(reference.inner)?.clone()
                    }
                    _ => return None,
                },
            };
        }
        Some(ty)
    }

    /// The Soul type an already-lowered operand carries, if any — a bare
    /// constant carries no type of its own (mirrors `mir_codegen::rvalue`'s
    /// `operand_type`, one layer up: Soul types here, LLVM types there).
    /// Needed because the resolver never assigns a type to a `FieldAccess`/
    /// `Index` *expression* (see `resolve_place_expression`'s docs), so a
    /// checked binary op between two such operands can't be typed by
    /// looking the original AST expression up in `self.declares` — it has
    /// to walk the already-built `Place` instead.
    pub(super) fn operand_type(&self, operand: &mir::Operand) -> Option<SoulType> {
        match operand {
            mir::Operand::Copy(place) | mir::Operand::Move(place) => self.place_type(place),
            mir::Operand::Constant(_) => None,
        }
    }

    /// Whether a binary expression's operands are float-typed — same
    /// operand-then-whole-expression fallback `lower_checked_binary_op`/
    /// `lower_checked_div` use to type themselves, reused here so
    /// `lower_rvalue` can decide *before* routing into either of those
    /// whether this is even an integer operation to begin with.
    pub(super) fn is_float_operand(
        &self,
        left: &mir::Operand,
        right: &mir::Operand,
        expr_id: ast::ExpressionId,
    ) -> bool {
        let ty = self
            .operand_type(left)
            .or_else(|| self.operand_type(right))
            .or_else(|| self.declares.get_expression_type(expr_id).cloned());
        matches!(ty, Some(SoulType::Primitive(p)) if p.is_float())
    }

    pub(super) fn expression_is_bool(&self, expr_id: ast::ExpressionId) -> bool {
        expr_id.is_boolean(self.store, self.declares)
    }

    /// Resolves a struct-typed `SoulType::Stub`'s bare name back to its
    /// `Struct` declaration in this function's module — see
    /// `DeclareStore::resolve_struct`, shared with `mir_codegen`.
    pub(super) fn resolve_struct(&self, ty: &SoulType) -> Option<&ast::Struct> {
        self.declares.resolve_struct(ty, self.module)
    }
}

pub(super) fn require_primitive(
    declares: &DeclareStore,
    ty: &SoulType,
    span: Span,
) -> MirResult<()> {
    if ty.is_primitive() {
        Ok(())
    } else {
        Err(Fault::error_with_kind(
            MirErrorKind::NonPrimitiveType {
                ty: ast::print_type(ty, declares).to_string().into(),
            },
            Some(span),
        ))
    }
}
