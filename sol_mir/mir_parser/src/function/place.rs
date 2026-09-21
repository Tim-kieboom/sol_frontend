use crate::{
    fault::{MirErrorKind, MirResult},
    function::FunctionLowerer,
};
use ast_model::{self as ast, SolType, operators::BinaryOperatorKind};
use mir_model as mir;
use sol_utils::{
    TypeModifier, compiler_options::MirOptions, fault::Fault, sol_names::PrimitiveTypes,
    span::Span,
};

impl<'a> FunctionLowerer<'a> {
    /// Lowers `object.field` as a read — see `resolve_field_place`.
    pub(super) fn lower_field_access(
        &mut self,
        field_access: &ast::FieldAccess,
        span: Span,
    ) -> MirResult<mir::Operand> {
        Ok(mir::Operand::Copy(
            self.resolve_field_place(field_access, span)?.0,
        ))
    }

    /// Lowers `collection[index]` as a read — see `resolve_index_place`.
    pub(super) fn lower_index_access(
        &mut self,
        index: &ast::Index,
        span: Span,
    ) -> MirResult<mir::Operand> {
        Ok(mir::Operand::Copy(self.resolve_index_place(index, span)?.0))
    }

    /// Lowers `*ptr` as a read — see `resolve_deref_place`.
    pub(super) fn lower_deref_access(
        &mut self,
        deref: &ast::Deref,
        span: Span,
    ) -> MirResult<mir::Operand> {
        Ok(mir::Operand::Copy(self.resolve_deref_place(deref, span)?.0))
    }

    /// Resolves an arbitrary "place expression" — a variable, a field access
    /// (`object.field`), an index (`collection[i]`), or any nesting of those
    /// — into a `Place` plus its resolved type. The shared entry point
    /// `resolve_field_place`'s object, `resolve_index_place`'s collection,
    /// and `lower_ref`'s referenced value all recurse through, so
    /// `o.items[i].x` ends up as one `Place` with a three-element
    /// projection, not a chain of temporaries. Anything else (a
    /// call/constructor result, ...) still faults — those aren't places
    /// this slice can project through.
    pub(super) fn resolve_place_expression(
        &mut self,
        expr_id: ast::ExpressionId,
        span: Span,
    ) -> MirResult<(mir::Place, SolType)> {
        let expr = &self.store.expressions[expr_id];
        match &expr.node {
            ast::ExpressionKind::Variable(var) => {
                let local = self.resolve_local(var, expr.span)?;
                let ty = self
                    .declares
                    .get_type(self.locals[local].ty)
                    .cloned()
                    .expect("mir::Type is always an interned TypeId");
                Ok((mir::Place::local(local), ty))
            }
            ast::ExpressionKind::FieldAccess(field_access) => {
                self.resolve_field_place(field_access, expr.span)
            }
            ast::ExpressionKind::Index(index) => self.resolve_index_place(index, expr.span),
            ast::ExpressionKind::Deref(deref) => self.resolve_deref_place(deref, expr.span),
            _ => Err(Fault::error_with_kind(
                MirErrorKind::UnsupportedPlaceExpression,
                Some(span),
            )),
        }
    }

    /// Lowers `&value`/`@value`. For a fixed-size array (`[N]T`) place, this
    /// produces a slice instead of a plain pointer: a bare pointer to an
    /// array carries no length, so — decided up front so a later bounds
    /// check can land without an ABI break — referencing an array always
    /// builds the `{ptr, len}` fat pointer (`AggregateKind::Array`, `len` a
    /// compile-time constant since only fixed-size arrays are supported
    /// here) rather than a plain `Rvalue::Ref`. Anything else (an ordinary
    /// `&x`) still lowers to a plain `Rvalue::Ref` — the bifurcation is
    /// entirely at the MIR-lowering level, `Ref` itself stays bare-pointer
    /// only.
    pub(super) fn lower_ref(&mut self, ref_: &ast::Ref, span: Span) -> MirResult<mir::Rvalue> {
        let (place, value_ty) = self.resolve_place_expression(ref_.value, span)?;
        let mutable = ref_.is_mutable;

        let SolType::Array(array) = &value_ty else {
            return Ok(mir::Rvalue::Ref { mutable, place });
        };
        let ast::ArrayKind::StackArray(len) = array.kind else {
            return Err(Fault::error_with_kind(
                MirErrorKind::ArrayReferenceUnsupported {
                    ty: self.print_ty(&value_ty).into(),
                },
                Some(span),
            ));
        };
        let ptr_ty = self
            .declares
            .intern_type(SolType::Reference(ast::ReferenceType {
                inner: array.of_type,
                lifetime: None,
                mutable: if mutable {
                    sol_utils::Mutable::Mut
                } else {
                    sol_utils::Mutable::Immut
                },
            }));
        let ptr_temp = self.alloc_local(ptr_ty, TypeModifier::Immut, span);
        self.push_assign(
            mir::Place::local(ptr_temp),
            mir::Rvalue::Ref { mutable, place },
        );

        Ok(mir::Rvalue::Aggregate(
            mir::AggregateKind::Slice,
            vec![
                mir::Operand::Copy(mir::Place::local(ptr_temp)),
                mir::Operand::Constant(mir::ConstValue::Uint(len as u128)),
            ],
        ))
    }

    /// Borrows a `&this`/`&mut this` call-site receiver into a temp
    /// `Reference` local, mirroring `lower_ref`'s non-array path (a receiver
    /// is always a struct instance, never array-typed, so the array-to-slice
    /// bifurcation there doesn't apply here).
    pub(super) fn lower_receiver_ref(
        &mut self,
        expr_id: ast::ExpressionId,
        mutable: bool,
        span: Span,
    ) -> MirResult<mir::Operand> {
        let (place, value_ty) = self.resolve_place_expression(expr_id, span)?;
        let inner = self.declares.intern_type(value_ty);
        let ref_ty = self
            .declares
            .intern_type(SolType::Reference(ast::ReferenceType {
                inner,
                lifetime: None,
                mutable: if mutable {
                    sol_utils::Mutable::Mut
                } else {
                    sol_utils::Mutable::Immut
                },
            }));
        let ref_temp = self.alloc_local(ref_ty, TypeModifier::Immut, span);
        self.push_assign(
            mir::Place::local(ref_temp),
            mir::Rvalue::Ref { mutable, place },
        );
        Ok(mir::Operand::Copy(mir::Place::local(ref_temp)))
    }

    /// Lowers `*ptr` into a `Place` with a `Deref` projection appended onto
    /// the pointer/reference's own place. `ptr` must itself resolve to a
    /// `Reference`/`Pointer` type; the resulting place's own type is
    /// whatever it points at.
    fn resolve_deref_place(
        &mut self,
        deref: &ast::Deref,
        span: Span,
    ) -> MirResult<(mir::Place, SolType)> {
        let value_span = self.store.expressions[deref.value].span;
        let (mut place, value_ty) = self.resolve_place_expression(deref.value, value_span)?;

        let Some(inner) = value_ty.deref_once(self.declares) else {
            return Err(Fault::error_with_kind(
                MirErrorKind::DerefTargetNotAReference {
                    ty: self.print_ty(&value_ty).into(),
                },
                Some(span),
            ));
        };

        place.projection.push(mir::PlaceElem::Deref);
        Ok((place, inner))
    }

    /// Lowers `object.field` into a `Place` with a `Field` projection
    /// appended onto the object's own place — read or write, straight off
    /// whatever storage the struct value already lives in (no temp/copy).
    /// Also returns the resolved place's own type (the innermost field's
    /// declared type), since a recursive caller needs it to resolve the
    /// *next* struct.
    fn resolve_field_place(
        &mut self,
        field_access: &ast::FieldAccess,
        span: Span,
    ) -> MirResult<(mir::Place, SolType)> {
        let object_span = self.store.expressions[field_access.object].span;
        let (mut place, object_ty) =
            self.resolve_place_expression(field_access.object, object_span)?;
        let object_ty = auto_deref(self.declares, &mut place, object_ty);

        let struct_ = self.resolve_struct(&object_ty).ok_or_else(|| {
            Fault::error_with_kind(
                MirErrorKind::NonPrimitiveType {
                    ty: self.print_ty(&object_ty).into(),
                },
                Some(span),
            )
        })?;

        let field_name = field_access.field.as_str();
        let (index, field_ty) = struct_
            .fields
            .iter()
            .enumerate()
            .find_map(|(index, field)| {
                let is_match = matches!(&field.value.pattern, ast::VarPattern::Simple { binding, .. } if binding.ident.as_str() == field_name);
                is_match.then_some((index, field.value.ty))
            })
            .ok_or_else(|| {
                Fault::error_with_kind(
                    MirErrorKind::StructFieldNotFound {
                        struct_name: struct_.name.as_str().into(),
                        field: field_name.into(),
                    },
                    Some(span),
                )
            })?;

        let field_ty = field_ty
            .and_then(|id| self.declares.get_type(id).cloned())
            .ok_or_else(|| {
                Fault::error_with_kind(MirErrorKind::VariableHasNoResolvedType, Some(span))
            })?;

        place.projection.push(mir::PlaceElem::Field(index));
        Ok((place, field_ty))
    }

    /// Lowers `collection[index]` into a `Place` with an `Index` projection
    /// appended onto the collection's own place. Only a slice
    /// (`[&]T`/`[&mut]T`) collection is supported — indexing directly into a
    /// fixed-size array/wildcard/heap array isn't (per the M1 scope: those
    /// only ever get *referenced* into a slice first, see `lower_ref`).
    fn resolve_index_place(
        &mut self,
        index: &ast::Index,
        span: Span,
    ) -> MirResult<(mir::Place, SolType)> {
        let collection_span = self.store.expressions[index.collection].span;
        let (mut place, collection_ty) =
            self.resolve_place_expression(index.collection, collection_span)?;
        let collection_ty = auto_deref(self.declares, &mut place, collection_ty);

        let SolType::Array(array) = &collection_ty else {
            return Err(Fault::error_with_kind(
                MirErrorKind::IndexTargetNotASlice {
                    ty: self.print_ty(&collection_ty).into(),
                },
                Some(span),
            ));
        };
        if !matches!(
            array.kind,
            ast::ArrayKind::MutSlice | ast::ArrayKind::ConstSlice
        ) {
            return Err(Fault::error_with_kind(
                MirErrorKind::IndexTargetNotASlice {
                    ty: self.print_ty(&collection_ty).into(),
                },
                Some(span),
            ));
        }
        let element_ty = self
            .declares
            .get_type(array.of_type)
            .cloned()
            .expect("ArrayType.of_type is always an interned TypeId");

        // Indices are always non-negative offsets in this slice — always
        // materialize into a `uint` temp rather than trying to preserve
        // whatever concrete int type the index expression happened to have
        // (matching the existing "untyped int literal defaults to `uint`"
        // convention elsewhere in this lowerer).
        let index_local =
            self.operand_local(index.index, SolType::Primitive(PrimitiveTypes::Uint), span)?;

        if self
            .options
            .mir
            .contains(MirOptions::CHECK_INDEX_OUT_OF_BOUNDS)
        {
            self.emit_bounds_check(&place, index_local, span);
        }

        place.projection.push(mir::PlaceElem::Index(index_local));
        Ok((place, element_ty))
    }

    /// `assert(index < collection.len())` — mirrors rustc's own `Len` +
    /// comparison + `Assert` shape: bounds checking is an ordinary MIR
    /// terminator here, not a codegen-level "insert a panicking branch
    /// here" mechanism (`mir_codegen` only has to implement `Rvalue::Len`
    /// and the already-generic `Assert` terminator). `index_local` is cast
    /// to `uint` first if it isn't already one — `operand_local` reuses a
    /// bare-`Variable` index's own declared type as-is (see its docs), but
    /// `Len` is always `uint`-typed and the comparison needs matching
    /// widths, so a non-`uint` index goes through `Rvalue::Cast` first.
    fn emit_bounds_check(
        &mut self,
        collection: &mir::Place,
        index_local: mir::LocalId,
        span: Span,
    ) {
        const UINT: SolType = SolType::Primitive(PrimitiveTypes::Uint);
        let uint_id = self.declares.intern_type(UINT);

        let len_local = self.alloc_local(uint_id, TypeModifier::Immut, span);
        self.push_assign(
            mir::Place::local(len_local),
            mir::Rvalue::Len(collection.clone()),
        );

        let is_uint = self
            .declares
            .get_type(self.locals[index_local].ty)
            .is_some_and(|ty| ty.is_primitive_kind(PrimitiveTypes::Uint));
        let index_local = if is_uint {
            index_local
        } else {
            let cast = self.alloc_local(uint_id, TypeModifier::Immut, span);
            self.push_assign(
                mir::Place::local(cast),
                mir::Rvalue::Cast(mir::Operand::Copy(mir::Place::local(index_local)), uint_id),
            );
            cast
        };

        let bool_id = self
            .declares
            .intern_type(SolType::Primitive(PrimitiveTypes::Boolean));
        let cond_local = self.alloc_local(bool_id, TypeModifier::Immut, span);
        self.push_assign(
            mir::Place::local(cond_local),
            mir::Rvalue::BinaryOp(
                BinaryOperatorKind::Lt,
                mir::Operand::Copy(mir::Place::local(index_local)),
                mir::Operand::Copy(mir::Place::local(len_local)),
            ),
        );

        let msg = mir::Operand::Constant(mir::ConstValue::Str("index out of bounds".to_string()));
        let next = self.new_block();
        self.seal(
            mir::Terminator::Assert {
                cond: mir::Operand::Copy(mir::Place::local(cond_local)),
                expected: true,
                msg,
                target: next,
                span,
            },
            Some(next),
        );
    }

    /// Materializes an expression into a `LocalId` holding its value —
    /// `PlaceElem::Index` needs an actual local to reference, not an
    /// arbitrary `Operand`. Reuses an already-existing local as-is when the
    /// expression is just a bare variable (no extra temp/copy, and no risk
    /// of a width mismatch from re-typing it as `ty`); otherwise allocates a
    /// fresh temp of type `ty` and assigns into it.
    fn operand_local(
        &mut self,
        expr_id: ast::ExpressionId,
        ty: SolType,
        span: Span,
    ) -> MirResult<mir::LocalId> {
        let expr = &self.store.expressions[expr_id];
        if let ast::ExpressionKind::Variable(var) = &expr.node {
            return self.resolve_local(var, expr.span);
        }

        let rvalue = self.lower_rvalue(expr_id)?;
        let ty = self.declares.intern_type(ty);
        let temp = self.alloc_local(ty, TypeModifier::Immut, span);
        self.push_assign(mir::Place::local(temp), rvalue);
        Ok(temp)
    }
}

/// If `ty` is a `&T`/`&mut T`/`*T`, appends a `Deref` step onto `place` and
/// returns `T`; otherwise `place`/`ty` pass through unchanged. Called before
/// resolving a field/index access's object/collection, so `object.field`/
/// `collection[i]` work the same whether `object`/`collection` is a value or
/// a reference to one — this is what keeps `this.field` working once a
/// `&this`/`&mut this` receiver becomes a real reference-typed place instead
/// of today's by-value copy. The type decision itself (`SolType::deref_once`)
/// is shared with the resolver — this only adds the MIR-specific side effect.
fn auto_deref(
    declares: &ast_model::declare_store::DeclareStore,
    place: &mut mir::Place,
    ty: SolType,
) -> SolType {
    match ty.deref_once(declares) {
        Some(inner) => {
            place.projection.push(mir::PlaceElem::Deref);
            inner
        }
        None => ty,
    }
}
