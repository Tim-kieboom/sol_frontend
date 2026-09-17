use ast_model::{
    AnyArray, ArrayKind, ArrayType, Binary, CustomType, ExpressionId, ExpressionKind, Field,
    Literal, ReferenceType, SoulType, Struct, StructConstructor, Stub, TypeId, TypeofKind,
    VarPattern,
    declare_store::DeclareStore,
    operators::{BinaryOperatorKind, UnaryOperatorKind},
};
use ast_parser::fault::{AstErrorKind, AstFault};
use soul_utils::{
    Ident, Mutable, TypeModifier, fault::Fault, soul_names::PrimitiveTypes, span::Span,
};

use super::function_call::{generic_name_of, is_generic_parameter};
use crate::NameResolver;

impl<'a> NameResolver<'a> {
    pub(crate) fn check_struct_constructor(&mut self, struct_constructor: &StructConstructor) {
        let Some(SoulType::Stub(stub)) = self.declares.get_type(struct_constructor.struct_type)
        else {
            return;
        };
        // Cloned so these don't keep borrowing `self.declares` (an `&'a mut
        // DeclareStore`, unlike `mir_parser`'s own `&'a DeclareStore` — a
        // reborrow through it can't outlive a later `&mut self` call) across
        // `check_struct_fields`'s own interning below.
        let stub = stub.clone();

        let Some(entry) = self.lookup_type(&stub.name, self.current.module) else {
            return;
        };

        let custom_type = self.declares.get_custom_type(entry.node_id);
        let Some((CustomType::Struct(struct_), _)) = custom_type else {
            return;
        };
        let struct_ = struct_.clone();

        self.declares.insert_type_resolve(
            struct_constructor.struct_type,
            ast_model::declare_store::TypeResolve::Struct(struct_.id),
        );

        let faults = self.check_struct_fields(&struct_, &stub, struct_constructor);
        for fault in faults {
            self.context.faults.push(fault);
        }
    }

    fn check_struct_fields(
        &mut self,
        struct_: &Struct,
        stub: &Stub,
        struct_constructor: &StructConstructor,
    ) -> Vec<AstFault> {
        fn eq_field_name(field: &Field, field_name: &Ident) -> bool {
            matches!(&field.value.pattern, VarPattern::Simple { binding, .. } if binding.ident.as_str() == field_name.as_str())
        }

        let mut faults = vec![];
        let mut generic_bindings = vec![];
        for (field_name, value_id) in &struct_constructor.values {
            let Some(field) = struct_
                .fields
                .iter()
                .find(|field| eq_field_name(field, field_name))
            else {
                faults.push(Fault::error_with_kind(
                    AstErrorKind::StructHasNoField {
                        struct_name: stub.name.clone(),
                        field_name: field_name.as_shared_str(),
                    },
                    Some(field_name.span()),
                ));
                continue;
            };

            let Some(field_ty) = field
                .value
                .ty
                .and_then(|id| self.declares.get_type(id).cloned())
            else {
                continue;
            };
            let field_ty = &field_ty;

            let span = self.store.expressions.get(*value_id).map(|expr| expr.span);

            if let Some(generic_name) = generic_name_of(self.declares, field_ty, &struct_.generics)
            {
                let Some(value_ty) = self.expression_type(*value_id) else {
                    continue;
                };

                let generic = generic_bindings
                    .iter()
                    .find(|(name, _)| *name == generic_name);

                match generic {
                    Some((_, bound_ty)) => {
                        if self.combine_operand_types(&value_ty, bound_ty).is_none() {
                            faults.push(Fault::error_with_kind(
                                AstErrorKind::GenericParameterConflict {
                                    generic_name: generic_name.into(),
                                    first: self.print_ty(bound_ty).into(),
                                    second: self.print_ty(&value_ty).into(),
                                },
                                span,
                            ));
                        }
                    }
                    None => generic_bindings.push((generic_name, value_ty)),
                }
                continue;
            }

            if is_generic_parameter(self.declares, field_ty, &struct_.generics) {
                continue;
            }

            let Some(value_ty) = self.expression_type(*value_id) else {
                continue;
            };

            if self.combine_operand_types(&value_ty, field_ty).is_some() {
                continue;
            }

            faults.push(Fault::error_with_kind(
                AstErrorKind::FieldTypeMismatch {
                    field_name: field_name.as_shared_str(),
                    expected: self.print_ty(field_ty).into(),
                    got: self.print_ty(&value_ty).into(),
                },
                span,
            ));
        }

        faults
    }

    pub(crate) fn check_binary_expression(
        &mut self,
        expression_id: ExpressionId,
        span: Span,
        binary: &Binary,
    ) {
        let Some(left_ty) = self.expression_type(binary.left) else {
            return;
        };
        let Some(right_ty) = self.expression_type(binary.right) else {
            return;
        };

        let Some(combined) = self.combine_operand_types(&left_ty, &right_ty) else {
            self.log_error(
                AstErrorKind::BinaryExpressionTypeMismatch {
                    left: self.print_ty(&left_ty).into(),
                    right: self.print_ty(&right_ty).into(),
                },
                Some(span),
            );
            return;
        };

        let result_ty = if is_comparison_operator(binary.operator.value) {
            SoulType::Primitive(PrimitiveTypes::Boolean)
        } else {
            combined
        };
        self.declares
            .insert_expression_type(expression_id, result_ty);
    }

    /// Persists `new(expr)`'s own type (`*T`, `T` being `expr`'s own
    /// resolved type) — unlike most of `expression_type`'s own arms, this
    /// can't stay purely on-demand: `expr_id`'s type is what MIR lowering
    /// later reads back via `get_expression_type` (which only ever sees
    /// whatever was explicitly persisted, never recomputes anything), so a
    /// `New` expression needs its result actually written into the
    /// `DeclareStore`, the same way `check_binary_expression` does for a
    /// `Binary` — silently leaves it untyped if `value` itself couldn't be
    /// typed (e.g. an unresolvable inner expression); nothing to log here,
    /// since whatever fault caused that already got logged resolving
    /// `value` itself.
    ///
    /// `default_concrete_type`s `value`'s own type first — `new(1)` has no
    /// declared target type for its bare literal to coerce against (unlike
    /// `x: i64 = 1` or an array literal's own element type), so without this
    /// its untyped literal type (`UntypedInt`/`UntypedUint`) would leak
    /// straight through into `*T`, producing `*UntypedUint` — a type that's
    /// never valid as a value's own resolved type outside of exactly this
    /// still-being-inferred window (mirrors `array_literal_element_type`'s
    /// own reason for calling this on an array literal's inferred element).
    pub(crate) fn check_new_expression(
        &mut self,
        expression_id: ExpressionId,
        value: ExpressionId,
        mutable: Mutable,
    ) {
        let Some(inner) = self.expression_type(value) else {
            return;
        };
        let inner = default_concrete_type(inner);
        let inner = self.declares.intern_type(inner);
        let ptr_ty = SoulType::Pointer(ReferenceType {
            inner,
            lifetime: None,
            mutable,
        });
        self.declares.insert_expression_type(expression_id, ptr_ty);
    }

    pub(crate) fn expression_type(&mut self, expression_id: ExpressionId) -> Option<SoulType> {
        if let Some(ty) = self.declares.get_expression_type(expression_id) {
            return Some(ty.clone());
        }

        let expression = self.store.expressions.get(expression_id)?;
        match &expression.node {
            ExpressionKind::Literal((_, literal)) => Some(literal_type(literal)),
            ExpressionKind::Variable(variable) => {
                let resolved = self.declares.get_variable_resolve(variable.id)?;
                let (_, ty, _) = self.declares.get_variable_type(resolved)?;
                ty.clone()
            }
            ExpressionKind::Lambda(lambda) => {
                let return_type = self
                    .first_lambda_return_type(lambda.body)
                    .unwrap_or(SoulType::None);
                let return_type = self.declares.intern_type(return_type);
                Some(SoulType::Function {
                    arity: lambda.parameters.len(),
                    return_type,
                })
            }
            ExpressionKind::FieldAccess(field_access) => {
                let object_ty = self.expression_type(field_access.object)?;
                self.struct_field_type(&object_ty, field_access.field.as_str())
            }
            // The constructor's own target type is already fully spelled out
            // in the syntax (`Struct{field: value, ..}`) — no inference
            // needed, just read it straight off the AST node.
            ExpressionKind::StructConstructor(ctor) => {
                self.declares.get_type(ctor.struct_type).cloned()
            }
            ExpressionKind::Index(index) => match self.expression_type(index.collection)? {
                SoulType::Array(array_ty) => self.declares.get_type(array_ty.of_type).cloned(),
                _ => None,
            },
            // Only `!` is typed — `-` (`Neg`) isn't lowered by `mir_parser`
            // yet, so a program using it never reaches a point where this
            // type would be observed.
            ExpressionKind::Unary(unary) if unary.operator.value == UnaryOperatorKind::Not => {
                Some(SoulType::Primitive(PrimitiveTypes::Boolean))
            }
            // Mirrors `mir_parser::function::place::lower_ref`'s own
            // bifurcation exactly: referencing a fixed-size array produces a
            // slice (`[&]T`/`[&mut]T`), referencing anything else produces a
            // bare `&T`/`&mut T`.
            ExpressionKind::Ref(ref_) => {
                let value_ty = self.expression_type(ref_.value)?;
                let mutable = if ref_.is_mutable {
                    Mutable::Mut
                } else {
                    Mutable::Immut
                };
                match value_ty {
                    SoulType::Array(array) if matches!(array.kind, ArrayKind::StackArray(_)) => {
                        let kind = if ref_.is_mutable {
                            ArrayKind::MutSlice
                        } else {
                            ArrayKind::ConstSlice
                        };
                        Some(SoulType::Array(ArrayType {
                            of_type: array.of_type,
                            kind,
                        }))
                    }
                    other => {
                        let inner = self.declares.intern_type(other);
                        Some(SoulType::Reference(ReferenceType {
                            inner,
                            lifetime: None,
                            mutable,
                        }))
                    }
                }
            }
            // `NewArray`/`ArrayFiller` are heap arrays — not lowered by
            // `mir_parser` yet, so left unhandled here too.
            ExpressionKind::Array(AnyArray::Array(array)) => match array
                .collection_type
                .and_then(|id| self.declares.get_type(id).cloned())
            {
                Some(ty) => Some(ty),
                None => {
                    // Deliberately *not* `array_literal_element_type` (which
                    // defaults an inferred element type, e.g. `UntypedInt` ->
                    // `Int`, losing its untyped-ness) — `combine_array_types`
                    // needs the raw, still-untyped element type to coerce
                    // `[1, 2]` against a declared `[2]i32` the same way a bare
                    // untyped literal already coerces against `i32`.
                    let of_type = match array
                        .element_type
                        .and_then(|id| self.declares.get_type(id).cloned())
                    {
                        Some(ty) => ty,
                        None => self.expression_type(*array.values.first()?)?,
                    };
                    let of_type = self.declares.intern_type(of_type);
                    Some(SoulType::Array(ArrayType {
                        of_type,
                        kind: ArrayKind::StackArray(array.values.len() as u64),
                    }))
                }
            },

            ExpressionKind::Unary(unary) => self.expression_type(unary.value),

            ExpressionKind::Sizeof(_) => Some(SoulType::Primitive(PrimitiveTypes::Uint)),

            // Only `expr.typeof` (`TypeofKind::Value`) actually reflects a
            // type — `Null`/`NotNull`/`Union{..}` (`expr typeof Type.Variant`
            // /`expr typeof null`, the older syntax — see TODO.md for the
            // planned `expr.typeof == Type`/`if type Variant(binding) = expr`
            // migration) are all boolean checks.
            ExpressionKind::TypeOf(type_of) => Some(match type_of.kind {
                TypeofKind::Value => SoulType::Type,
                TypeofKind::Null | TypeofKind::NotNull | TypeofKind::Union { .. } => {
                    SoulType::Primitive(PrimitiveTypes::Boolean)
                }
            }),

            // `new(expr)`'s own type is `*T`, `T` being `expr`'s own
            // resolved type — the one expression kind whose type depends on
            // an inner expression's type rather than being fixed or `None`.
            // `default_concrete_type`d for the same reason `check_new_
            // expression` does it — see that method's own docs.
            ExpressionKind::New(value, mutable) => {
                let inner = self.expression_type(*value)?;
                let inner = default_concrete_type(inner);
                let inner = self.declares.intern_type(inner);
                Some(SoulType::Pointer(ReferenceType {
                    inner,
                    lifetime: None,
                    mutable: *mutable,
                }))
            }

            // not yet impl
            ExpressionKind::If(_)
            | ExpressionKind::Null(_)
            | ExpressionKind::Copy(_)
            | ExpressionKind::Pass(_)
            | ExpressionKind::Tuple(_)
            | ExpressionKind::Match(_)
            | ExpressionKind::Block(_)
            | ExpressionKind::Binary(_)
            | ExpressionKind::NewArray(_)
            | ExpressionKind::NamedTuple(_)
            | ExpressionKind::MatchMethod(_)
            | ExpressionKind::Constructor(_)
            | ExpressionKind::FunctionCall(_)
            | ExpressionKind::Array(AnyArray::ArrayFiller(_)) => None,

            // `*ptr`'s type is whatever `ptr` (a `&T`/`&mut T`/`*T`) points
            // at — `SoulType::deref_once` is shared with
            // `mir_parser::function::place::resolve_deref_place`.
            ExpressionKind::Deref(deref) => {
                self.expression_type(deref.value)?.deref_once(self.declares)
            }

            // is always none
            ExpressionKind::Break
            | ExpressionKind::For(_)
            | ExpressionKind::None(_)
            | ExpressionKind::Continue
            | ExpressionKind::Return(_)
            | ExpressionKind::Undefined(_)
            | ExpressionKind::StringFormat(_) => None,
        }
    }

    pub(crate) fn foreach_collection_element_type(
        &mut self,
        collection: ExpressionId,
    ) -> Option<SoulType> {
        let expression = self.store.expressions.get(collection)?;
        match &expression.node {
            ExpressionKind::Array(any_array) | ExpressionKind::NewArray(any_array) => {
                self.array_literal_element_type(any_array)
            }
            _ => match self.expression_type(collection)? {
                SoulType::Array(array_ty) => self.declares.get_type(array_ty.of_type).cloned(),
                _ => None,
            },
        }
    }

    fn array_literal_element_type(&mut self, any_array: &AnyArray) -> Option<SoulType> {
        match any_array {
            AnyArray::Array(array) => {
                match array
                    .element_type
                    .and_then(|id| self.declares.get_type(id).cloned())
                {
                    Some(ty) => Some(ty),
                    None => Some(default_concrete_type(
                        self.expression_type(*array.values.first()?)?,
                    )),
                }
            }
            AnyArray::ArrayFiller(filler) => {
                match filler
                    .element_type
                    .and_then(|id| self.declares.get_type(id).cloned())
                {
                    Some(ty) => Some(ty),
                    None => Some(default_concrete_type(self.expression_type(filler.element)?)),
                }
            }
        }
    }

    fn struct_field_type(&mut self, ty: &SoulType, field_name: &str) -> Option<SoulType> {
        // Auto-deref: `object`'s type can be `&Struct`/`&mut Struct` (e.g. a
        // `&this`/`&mut this` receiver) as readily as a bare `Struct` value —
        // `SoulType::deref_once` is shared with
        // `mir_parser::function::place::auto_deref`.
        let resolved;
        let ty = match ty.deref_once(self.declares) {
            Some(inner) => {
                resolved = inner;
                &resolved
            }
            None => ty,
        };
        let SoulType::Stub(stub) = ty else {
            return None;
        };
        let entry = self.lookup_type(&stub.name, self.current.module)?;
        let (CustomType::Struct(struct_), _) = self.declares.get_custom_type(entry.node_id)? else {
            return None;
        };
        let struct_ = struct_.clone();
        let occurrence = self.declares.get_type_id(ty);

        if let Some(occurrence) = occurrence {
            self.declares.insert_type_resolve(
                occurrence,
                ast_model::declare_store::TypeResolve::Struct(struct_.id),
            );
        }

        struct_.fields.iter().find_map(|field| {
            let VarPattern::Simple { binding, .. } = &field.value.pattern else {
                return None;
            };
            if binding.ident.as_str() != field_name {
                return None;
            }
            field
                .value
                .ty
                .and_then(|id| self.declares.get_type(id).cloned())
        })
    }

    pub(crate) fn variable_lvalue(
        &self,
        expression_id: ExpressionId,
    ) -> Option<(TypeModifier, SoulType)> {
        let expression = self.store.expressions.get(expression_id)?;
        let ExpressionKind::Variable(variable) = &expression.node else {
            return None;
        };
        let resolved = self.declares.get_variable_resolve(variable.id)?;
        let (modifier, ty, _) = self.declares.get_variable_type(resolved)?;
        Some((*modifier, ty.clone()?))
    }

    /// Pretty-prints `ty` for an error message — `SoulType`'s own `Debug` is
    /// the plain derived one now (its internal fields are bare `TypeId`s, and
    /// `Debug::fmt` has no way to resolve those), so every fault-message site
    /// that used to do `format!("{ty:?}")` goes through here instead.
    pub(crate) fn print_ty(&self, ty: &SoulType) -> String {
        ast_model::print_type(ty, self.declares).to_string()
    }

    pub(crate) fn combine_operand_types(
        &mut self,
        left: &SoulType,
        right: &SoulType,
    ) -> Option<SoulType> {
        let left = self.resolve_type_alias(left);
        let right = self.resolve_type_alias(right);
        combine_resolved_operand_types(self.declares, &left, &right)
    }

    fn resolve_type_alias(&mut self, ty: &SoulType) -> SoulType {
        let occurrence = self.declares.get_type_id(ty);
        let mut current = ty.clone();
        let mut resolved_any = false;
        for _ in 0..8 {
            let SoulType::Stub(stub) = &current else {
                break;
            };
            let Some(underlying) = self.declares.get_type_alias(stub.name.as_str()) else {
                break;
            };
            current = underlying.clone();
            resolved_any = true;
        }
        // Only cache when this occurrence actually named an alias — an
        // occurrence that never resolved here might still be a struct/enum/
        // trait/generic, which its own call site (`check_struct_constructor`,
        // `check_enum_variant_construction`, ...) is responsible for caching
        // instead; caching a no-op "alias" entry here would shadow that.
        if resolved_any && let Some(occurrence) = occurrence {
            let resolved_id = self.declares.intern_type(current.clone());
            self.declares.insert_type_resolve(
                occurrence,
                ast_model::declare_store::TypeResolve::Alias(resolved_id),
            );
        }
        current
    }
}

fn literal_type(literal: &Literal) -> SoulType {
    match literal {
        Literal::Int(_) => SoulType::Primitive(PrimitiveTypes::UntypedInt),
        Literal::Uint(_) => SoulType::Primitive(PrimitiveTypes::UntypedUint),
        Literal::Float(_) => SoulType::Primitive(PrimitiveTypes::UntypedFloat),
        Literal::Bool(_) => SoulType::Primitive(PrimitiveTypes::Boolean),
        Literal::Char(_) => SoulType::Primitive(PrimitiveTypes::Char),
        Literal::Cstr(_) => SoulType::Primitive(PrimitiveTypes::CStr),
        Literal::Str(_) => SoulType::Reference(ReferenceType::with_lifetime(
            TypeId::STRING,
            Ident::new("static", Span::error()),
            Mutable::Immut,
        )),
    }
}

fn is_comparison_operator(operator: BinaryOperatorKind) -> bool {
    matches!(
        operator,
        BinaryOperatorKind::Eq
            | BinaryOperatorKind::NotEq
            | BinaryOperatorKind::Lt
            | BinaryOperatorKind::Gt
            | BinaryOperatorKind::Le
            | BinaryOperatorKind::Ge
            | BinaryOperatorKind::LogAnd
            | BinaryOperatorKind::LogOr
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NumCategory {
    Int,
    Uint,
    Float,
}

fn concrete_num_category(prim: PrimitiveTypes) -> Option<NumCategory> {
    match prim {
        PrimitiveTypes::CInt
        | PrimitiveTypes::Int
        | PrimitiveTypes::Int8
        | PrimitiveTypes::Int16
        | PrimitiveTypes::Int32
        | PrimitiveTypes::Int64
        | PrimitiveTypes::Int128 => Some(NumCategory::Int),
        PrimitiveTypes::CUint
        | PrimitiveTypes::Uint
        | PrimitiveTypes::Uint8
        | PrimitiveTypes::Uint16
        | PrimitiveTypes::Uint32
        | PrimitiveTypes::Uint64
        | PrimitiveTypes::Uint128 => Some(NumCategory::Uint),
        PrimitiveTypes::Float16 | PrimitiveTypes::Float32 | PrimitiveTypes::Float64 => {
            Some(NumCategory::Float)
        }
        _ => None,
    }
}

fn untyped_targets(kind: PrimitiveTypes) -> &'static [NumCategory] {
    match kind {
        PrimitiveTypes::UntypedInt => &[NumCategory::Int, NumCategory::Float],
        PrimitiveTypes::UntypedUint => &[NumCategory::Int, NumCategory::Uint, NumCategory::Float],
        PrimitiveTypes::UntypedFloat => &[NumCategory::Float],
        _ => &[],
    }
}

fn untyped_kind_of(ty: &SoulType) -> Option<PrimitiveTypes> {
    match ty {
        SoulType::Primitive(
            kind @ (PrimitiveTypes::UntypedInt
            | PrimitiveTypes::UntypedUint
            | PrimitiveTypes::UntypedFloat),
        ) => Some(*kind),
        _ => None,
    }
}

fn combine_untyped_kinds(a: PrimitiveTypes, b: PrimitiveTypes) -> PrimitiveTypes {
    if a == PrimitiveTypes::UntypedFloat || b == PrimitiveTypes::UntypedFloat {
        PrimitiveTypes::UntypedFloat
    } else {
        PrimitiveTypes::UntypedInt
    }
}

pub(crate) fn default_concrete_type(ty: SoulType) -> SoulType {
    match ty {
        SoulType::Primitive(PrimitiveTypes::UntypedInt | PrimitiveTypes::UntypedUint) => {
            SoulType::Primitive(PrimitiveTypes::Int)
        }
        SoulType::Primitive(PrimitiveTypes::UntypedFloat) => {
            SoulType::Primitive(PrimitiveTypes::Float64)
        }
        other => other,
    }
}

fn combine_resolved_operand_types(
    declares: &mut DeclareStore,
    left: &SoulType,
    right: &SoulType,
) -> Option<SoulType> {
    if let (SoulType::Array(left_array), SoulType::Array(right_array)) = (left, right) {
        return combine_array_types(declares, left_array, right_array);
    }

    match (untyped_kind_of(left), untyped_kind_of(right)) {
        (None, None) if declares.soul_types_equal_ignoring_occurrence(left, right) => {
            Some(left.clone())
        }
        (None, None) => None,
        (Some(kind), None) => coerce_untyped_to_concrete(kind, right),
        (None, Some(kind)) => coerce_untyped_to_concrete(kind, left),
        (Some(a), Some(b)) => Some(SoulType::Primitive(combine_untyped_kinds(a, b))),
    }
}

/// Element types combine the same way any other operand pair does — so an
/// untyped array literal's element type (`[1, 2]`'s `UntypedInt`, see the
/// `Array` arm of `expression_type`, which deliberately doesn't default it
/// the way `array_literal_element_type` does) still coerces against a
/// declared `[2]i32`, the same way a bare `1: i32` already does.
///
/// `[&]T`/`[&mut]T` combine freely with each other regardless of kind —
/// slice mutability isn't checked anywhere else in this compiler yet either
/// (M2 borrow checker's job, same as struct field/method-receiver
/// mutability), so `s: [&mut]T = &value` (a plain, non-`mut` `&`) has to
/// type-check the same way it already codegens. Any other `ArrayKind`
/// pairing (`[N]T` vs `[&]T`, mismatched fixed sizes, ...) still needs exact
/// equality.
fn combine_array_types(
    declares: &mut DeclareStore,
    left: &ArrayType,
    right: &ArrayType,
) -> Option<SoulType> {
    let left_of_type = declares.get_type(left.of_type).cloned()?;
    let right_of_type = declares.get_type(right.of_type).cloned()?;
    let of_type = combine_resolved_operand_types(declares, &left_of_type, &right_of_type)?;

    let is_slice = |kind: &ArrayKind| matches!(kind, ArrayKind::MutSlice | ArrayKind::ConstSlice);
    let both_slice = is_slice(&left.kind) && is_slice(&right.kind);
    let is_same = left.kind == right.kind;
    let kind = if both_slice || is_same {
        left.kind
    } else {
        return None;
    };

    let of_type = declares.intern_type(of_type);
    Some(SoulType::Array(ArrayType { of_type, kind }))
}

fn coerce_untyped_to_concrete(kind: PrimitiveTypes, concrete: &SoulType) -> Option<SoulType> {
    let SoulType::Primitive(prim) = concrete else {
        return None;
    };

    let category = concrete_num_category(*prim)?;
    if untyped_targets(kind).contains(&category) {
        Some(concrete.clone())
    } else {
        None
    }
}
