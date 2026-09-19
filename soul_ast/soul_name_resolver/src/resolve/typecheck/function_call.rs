use ast_model::{
    AnyArray, CustomType, EnumVariant, ExpressionId, ExpressionKind, FunctionCall, Generic,
    SoulType, UnionKind, declare_store::DeclareStore,
};
use ast_parser::fault::{AstErrorKind, EnumVariantArgumentTypeMismatch};
use soul_utils::{FunctionId, soul_names::PrimitiveTypes};

use crate::NameResolver;

impl<'a> NameResolver<'a> {
    pub(crate) fn finish_call_resolution(
        &mut self,
        expression_id: ExpressionId,
        call: &FunctionCall,
        function_id: FunctionId,
    ) {
        if function_id == FunctionId::ERROR {
            return;
        }
        let Some((signature, _)) = self.declares.get_function(function_id) else {
            return;
        };
        let Some(return_type) = self.declares.get_type(signature.return_type).cloned() else {
            return;
        };
        let parameters = signature.parameters.clone();
        let generics = signature.generics.clone();

        // Only checkable when arity matches positionally and no argument is named.
        let checkable = call.arguments.len() == parameters.len()
            && call.arguments.iter().all(|arg| arg.name.is_none());

        if checkable {
            let mut generic_bindings: Vec<(&str, SoulType)> = Vec::new();

            for (argument, parameter) in call.arguments.iter().zip(&parameters) {
                // The `varargs` marker parameter has no real declared type
                // (`parameter.ty` is a meaningless placeholder — see
                // `ast_parser::parse::function`) and its argument is never
                // typechecked against it positionally; it's checked
                // element-by-element instead (rule 2/3/4/5/6 of the
                // `varargs` design).
                if parameter.is_variadic {
                    self.check_varargs_argument(argument.value);
                    continue;
                }

                let Some(parameter_ty) = self.declares.get_type(parameter.ty).cloned() else {
                    continue;
                };

                if matches!(parameter_ty, SoulType::ImplTrait(_)) {
                    continue;
                }

                let Some(arg_ty) = self.expression_type(argument.value) else {
                    continue;
                };

                let span = self
                    .store
                    .expressions
                    .get(argument.value)
                    .map(|expr| expr.span)
                    .unwrap_or(call.name.span());

                if let Some(generic_name) = generic_name_of(self.declares, &parameter_ty, &generics)
                {
                    match generic_bindings
                        .iter()
                        .find(|(name, _)| *name == generic_name)
                    {
                        Some((_, bound_ty)) => {
                            if self.combine_operand_types(&arg_ty, bound_ty).is_none() {
                                self.log_error(
                                    AstErrorKind::GenericParameterConflict {
                                        generic_name: generic_name.into(),
                                        first: self.print_ty(bound_ty).into(),
                                        second: self.print_ty(&arg_ty).into(),
                                    },
                                    Some(span),
                                );
                            }
                        }
                        None => generic_bindings.push((generic_name, arg_ty)),
                    }
                    continue;
                }

                if self.combine_operand_types(&arg_ty, &parameter_ty).is_some() {
                    continue;
                }

                self.log_error(
                    AstErrorKind::ArgumentTypeMismatch {
                        expected: self.print_ty(&parameter_ty).into(),
                        got: self.print_ty(&arg_ty).into(),
                    },
                    Some(span),
                );
            }
        }

        self.declares
            .insert_expression_type(expression_id, return_type);
    }

    /// Checks a `varargs` parameter's argument — which the grammar requires
    /// to always be written explicitly as `varargs.[elem, ...]` (rule 2 of
    /// the `varargs` design; parses as a plain array literal whose
    /// `collection_type` names the `varargs` marker, same shape as any other
    /// `Type.[...]` collection literal — see `ast_parser`'s expression
    /// access parsing). Each element is checked against the fixed primitive
    /// whitelist (rule 4); C default-argument promotion (`f32` -> `f64`,
    /// narrower-than-`int` -> `int`) and untyped-literal defaulting (rule 6)
    /// are purely a lowering/codegen concern (`mir_parser`/`mir_codegen`),
    /// not something this check needs to rewrite the AST for.
    fn check_varargs_argument(&mut self, argument: ExpressionId) {
        let span = self
            .store
            .expressions
            .get(argument)
            .map(|expr| expr.span);

        let Some(expr) = self.store.expressions.get(argument) else {
            return;
        };
        let ExpressionKind::Array(AnyArray::Array(array)) = &expr.node else {
            self.log_error(AstErrorKind::VarargsArgumentRequired, span);
            return;
        };
        let is_varargs_literal = array
            .collection_type
            .and_then(|id| self.declares.get_type(id))
            .is_some_and(|ty| matches!(ty, SoulType::Stub(stub) if stub.name.as_ref() == "varargs"));
        if !is_varargs_literal {
            self.log_error(AstErrorKind::VarargsArgumentRequired, span);
            return;
        }

        let values = array.values.clone();
        for value in values.iter() {
            let elem_span = self.store.expressions.get(*value).map(|e| e.span);
            let Some(elem_ty) = self.expression_type(*value) else {
                continue;
            };
            if !is_varargs_allowed_type(&elem_ty) {
                self.log_error(
                    AstErrorKind::VarargsElementTypeNotAllowed {
                        found: self.print_ty(&elem_ty).into(),
                    },
                    elem_span,
                );
            }
        }
    }

    pub(crate) fn check_enum_variant_construction(
        &mut self,
        owner_type: &SoulType,
        call: &FunctionCall,
    ) {
        let SoulType::Stub(stub) = owner_type else {
            return;
        };

        let Some(entry) = self.lookup_type(&stub.name, self.current.module) else {
            // Slice 7 of the `Stub`-redesign: a name resolving to nothing at
            // all — as opposed to resolving to something that just isn't an
            // enum (a separate, pre-existing gap, out of scope here) —
            // reaches this function only as a last resort, after ordinary
            // function/method resolution already failed; without this, a
            // call shaped like `X.Y(..)` where `X` isn't any known name at
            // all was silently accepted with no diagnostic at all.
            self.log_error(
                AstErrorKind::UndefinedType {
                    name: stub.name.clone(),
                },
                Some(call.name.span()),
            );
            return;
        };

        let node_id = entry.node_id;
        let Some((CustomType::Enum(enum_), _)) = self.declares.get_custom_type(node_id) else {
            return;
        };
        let enum_ = enum_.clone();

        let occurrence = self.declares.intern_type(owner_type.clone());
        self.declares.insert_type_resolve(
            occurrence,
            ast_model::declare_store::TypeResolve::Enum(node_id),
        );

        let variant_name = call.name.as_str();
        let Some(EnumVariant::Union(UnionKind::Tuple { parameters, .. })) = enum_
            .variants
            .iter()
            .find(|variant| enum_variant_name(variant) == variant_name)
        else {
            return;
        };
        let parameters = parameters.clone();

        if call.arguments.len() != parameters.len() {
            self.log_error(
                AstErrorKind::EnumVariantArityMismatch {
                    enum_name: stub.name.clone(),
                    variant_name: call.name.as_shared_str(),
                    expected: parameters.len(),
                    got: call.arguments.len(),
                },
                Some(call.name.span()),
            );
            return;
        }

        for (argument, param_ty) in call.arguments.iter().zip(&parameters) {
            let Some(param_ty) = self.declares.get_type(*param_ty).cloned() else {
                continue;
            };
            let Some(arg_ty) = self.expression_type(argument.value) else {
                continue;
            };
            if self.combine_operand_types(&arg_ty, &param_ty).is_some() {
                continue;
            }

            let span = self.store.expressions.get(argument.value).map(|e| e.span);
            let expected = self.print_ty(&param_ty).into();
            let got = self.print_ty(&arg_ty).into();
            self.log_error(
                EnumVariantArgumentTypeMismatch {
                    enum_name: stub.name.as_str().into(),
                    variant_name: variant_name.into(),
                    expected,
                    got,
                }
                .into(),
                span,
            );
        }
    }
}

pub(crate) fn is_generic_parameter(
    declares: &mut DeclareStore,
    ty: &SoulType,
    generics: &[Generic],
) -> bool {
    match ty {
        SoulType::ImplTrait(_) => true,
        SoulType::Stub(stub) => {
            let is_generic = generics
                .iter()
                .any(|generic| generic.name.as_str() == stub.name.as_str());
            if is_generic && let Some(occurrence) = declares.get_type_id(ty) {
                declares.insert_type_resolve(
                    occurrence,
                    ast_model::declare_store::TypeResolve::Generic,
                );
            }
            is_generic
        }
        _ => false,
    }
}

/// The declared generic's name if `ty` is a bare reference to it (e.g. `T`
/// in `foo<T>(a: T)`), so repeated uses of the same generic within one call
/// can be checked against each other.
pub(crate) fn generic_name_of<'g>(
    declares: &mut DeclareStore,
    ty: &SoulType,
    generics: &'g [Generic],
) -> Option<&'g str> {
    let SoulType::Stub(stub) = ty else {
        return None;
    };
    let found = generics
        .iter()
        .find(|generic| generic.name.as_str() == stub.name.as_str())?;
    if let Some(occurrence) = declares.get_type_id(ty) {
        declares.insert_type_resolve(occurrence, ast_model::declare_store::TypeResolve::Generic);
    }
    Some(found.name.as_str())
}

/// The fixed whitelist of types allowed as a `varargs.[...]` element (rule 4
/// of the `varargs` design): `cstr`, `bool`, the built-in integer/float
/// types, and raw pointers — no structs, no nested `varargs`, no other
/// aggregates. Untyped int/float literals are included since they still
/// default to an allowed concrete type (rule 6) before ever reaching
/// codegen. Raw pointers need no promotion (C's default argument
/// promotions never touch pointers — they're already word-sized) and are
/// emitted as-is by `codegen_variadic_argument`.
fn is_varargs_allowed_type(ty: &SoulType) -> bool {
    use PrimitiveTypes::*;
    matches!(
        ty,
        SoulType::RawPtr(_)
            | SoulType::Primitive(
                CStr | Boolean
                    | CInt
                    | CUint
                    | UntypedInt
                    | UntypedUint
                    | UntypedFloat
                    | Int
                    | Int8
                    | Int16
                    | Int32
                    | Int64
                    | Int128
                    | Uint
                    | Uint8
                    | Uint16
                    | Uint32
                    | Uint64
                    | Uint128
                    | Float16
                    | Float32
                    | Float64
            )
    )
}

fn enum_variant_name(variant: &EnumVariant) -> &str {
    match variant {
        EnumVariant::Normal(name) => name.as_str(),
        EnumVariant::Assigned { name, .. } => name.as_str(),
        EnumVariant::Union(UnionKind::Tuple { name, .. } | UnionKind::NamedTuple { name, .. }) => {
            name.as_str()
        }
    }
}
