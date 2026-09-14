use ast_model::{
    Assignment, CustomType, Enum, EnumVariant, ExpressionId, FunctionKind, ImplBlock,
    InnerFunctionSignature, SoulType, StatementId, StatementKind, Struct, Trait, UseBlock,
    VarPattern, Variable, scope::ScopeTypeEntryKind,
};
use ast_parser::fault::AstErrorKind;
use soul_utils::{FunctionId, soul_error_internal, span::Span};

use crate::NameResolver;

impl<'a> NameResolver<'a> {
    pub(super) fn resolve_statement(&mut self, id: StatementId) {
        let Some(statement) = self.store.statements.get(id) else {
            self.log_fault(soul_error_internal!(format!("{id:?} not found"), None));
            return;
        };

        match &statement.node {
            StatementKind::Import(_) | StatementKind::TypeDef(_) => (),
            StatementKind::Enum(enum_) => self.resolve_enum(enum_),
            StatementKind::Union(union_) => self.resolve_enum(union_),
            StatementKind::Function(id) => self.resolve_function(*id),
            StatementKind::Trait(trait_) => self.resolve_trait(trait_),
            StatementKind::Struct(struct_) => self.resolve_struct(struct_),
            StatementKind::Variable(variable) => self.resolve_variable(variable),
            StatementKind::UseBlock(use_block) => self.resolve_use_block(use_block, statement.span),
            StatementKind::ExternalFunction(id) => self.resolve_function(*id),
            StatementKind::Assignment(assignment) => self.resolve_assignment(assignment),
            StatementKind::Expression {
                expression,
                ends_semicolon: _,
            } => self.resolve_expression(*expression),
        }
    }

    fn resolve_assignment(&mut self, assignment: &Assignment) {
        self.resolve_expression(assignment.left);
        self.resolve_expression(assignment.right);
        self.check_assignment(assignment);
    }

    fn resolve_use_block(&mut self, use_block: &UseBlock, span: Span) {
        for method in &use_block.methods {
            self.resolve_function(method.id);
        }

        for impl_block in &use_block.impls {
            for method in &impl_block.methods {
                self.resolve_function(*method);
            }
            self.check_impl_conformance(impl_block, span);
        }

        for statement in &use_block.statements {
            self.resolve_statement(*statement);
        }
    }

    /// Verifies `impl Trait for X` supplies *exactly* `Trait`'s declared
    /// method set (M1 has no default trait method bodies yet — see
    /// TODO.md — so nothing can be omitted), each with a matching signature
    /// (parameter types positionally, plus return type; the receiver itself
    /// is never compared — see below).
    ///
    /// The receiver is excluded deliberately, not by oversight: a trait
    /// method's `method_type` is parsed as a `Stub` carrying the *trait's*
    /// own name (`from_keyword.rs`'s trait-body parser has no real `Self`
    /// placeholder yet — see TODO.md's planned `This` type), while the impl
    /// method's `method_type` is the concrete implementing type — the two
    /// are never expected to be equal.
    fn check_impl_conformance(&mut self, impl_block: &ImplBlock, span: Span) {
        let SoulType::Stub(stub) = &impl_block.impl_trait else {
            self.log_error(
                AstErrorKind::ImplTargetIsNotATrait {
                    name: format!("{:?}", impl_block.impl_trait).into(),
                },
                Some(span),
            );
            return;
        };
        let trait_name = stub.name.clone();

        let entry = self.lookup_type(trait_name.as_str(), self.current.module);
        let is_trait_entry =
            entry.is_some_and(|entry| matches!(entry.kind, ScopeTypeEntryKind::Trait));
        if !is_trait_entry {
            self.log_error(
                AstErrorKind::ImplTargetIsNotATrait {
                    name: trait_name.clone(),
                },
                Some(span),
            );
            return;
        }
        let Some((CustomType::Trait(trait_), _)) =
            entry.and_then(|entry| self.declares.get_custom_type(entry.node_id))
        else {
            return;
        };

        let trait_methods: Vec<&InnerFunctionSignature> = trait_
            .methods
            .iter()
            .filter_map(|id| self.store.functions.get(*id))
            .map(FunctionKind::signature)
            .collect();
        let impl_methods: Vec<&InnerFunctionSignature> = impl_block
            .methods
            .iter()
            .filter_map(|id| self.store.functions.get(*id))
            .map(FunctionKind::signature)
            .collect();

        for trait_sig in &trait_methods {
            let method_name = trait_sig.name.as_str();
            let Some(impl_sig) = impl_methods
                .iter()
                .find(|sig| sig.name.as_str() == method_name)
            else {
                self.log_error(
                    AstErrorKind::ImplMissingTraitMethod {
                        trait_name: trait_name.clone(),
                        method_name: method_name.into(),
                    },
                    Some(span),
                );
                continue;
            };

            if !signatures_match(trait_sig, impl_sig) {
                self.log_error(
                    AstErrorKind::ImplTraitMethodSignatureMismatch {
                        trait_name: trait_name.clone(),
                        method_name: method_name.into(),
                    },
                    Some(impl_sig.name.span()),
                );
            }
        }

        for impl_sig in &impl_methods {
            let method_name = impl_sig.name.as_str();
            let declared_by_trait = trait_methods
                .iter()
                .any(|sig| sig.name.as_str() == method_name);
            if !declared_by_trait {
                self.log_error(
                    AstErrorKind::ImplHasExtraTraitMethod {
                        trait_name: trait_name.clone(),
                        method_name: impl_sig.name.as_shared_str(),
                    },
                    Some(impl_sig.name.span()),
                );
            }
        }
    }

    fn resolve_variable(&mut self, variable: &Variable) {
        if let Some(value) = variable.initialize_value {
            self.resolve_expression(value);
            match variable.ty.and_then(|id| self.declares.get_type(id).cloned()) {
                // An explicit annotation (`x: T = value`) is never inferred —
                // it must instead be checked against the initializer, the
                // same way a plain `x = value` reassignment already is (see
                // `check_assignment`).
                Some(declared_ty) => self.check_variable_declaration(&declared_ty, value),
                None => self.backfill_variable_type(variable, value),
            }
        }
    }

    fn backfill_variable_type(&mut self, variable: &Variable, value: ExpressionId) {
        let type_key = match &variable.pattern {
            VarPattern::Simple { binding, .. } => binding.id,
            _ => variable.id,
        };
        if self
            .declares
            .get_variable_type(type_key)
            .is_some_and(|(_, ty, _)| ty.is_some())
        {
            return;
        }

        let Some(ty) = self.expression_type(value) else {
            return;
        };
        self.declares.insert_variable_type(
            type_key,
            variable.modifier,
            Some(ty),
            self.current.module,
        );
    }

    fn resolve_struct(&mut self, struct_: &Struct) {
        for field in &struct_.fields {
            if let Some(value) = &field.value.initialize_value {
                self.resolve_expression(*value);
            }
        }
        for statement in &struct_.statements {
            self.resolve_statement(*statement);
        }
    }

    fn resolve_trait(&mut self, trait_: &Trait) {
        for method in &trait_.methods {
            self.resolve_function(*method);
        }
    }

    fn resolve_function(&mut self, function_id: FunctionId) {
        let Some(function_kind) = self.store.functions.get(function_id) else {
            self.log_fault(soul_error_internal!(
                format!("{function_id:?} not found"),
                None
            ));
            return;
        };

        let prev = self.current.function;
        let signature = &function_kind.signature();
        self.current.function = Some(signature.id);
        for parameter in &signature.parameters {
            if let Some(default) = &parameter.default {
                self.resolve_expression(*default);
            }
        }

        self.declares
            .insert_functions(signature.id, (*signature).clone(), self.current.module);

        match function_kind {
            FunctionKind::Signature(_) => (),
            FunctionKind::Normal(function) => {
                self.resolve_block(function.block);
                self.check_tail_return_type(
                    function.block,
                    &signature.return_type,
                    &signature.generics,
                );
            }
        };

        self.current.function = prev;
    }

    fn resolve_enum(&mut self, enum_: &Enum) {
        for variant in &enum_.variants {
            match variant {
                EnumVariant::Union(_) | EnumVariant::Normal(_) => (),
                EnumVariant::Assigned { name: _, value } => self.resolve_expression(*value),
            }
        }
    }
}

/// Compares an impl method's signature against its trait method's, ignoring
/// the receiver (`method_type` never matches — see `check_impl_conformance`)
/// and parameter names (M1 conformance is positional-type-only, not named).
fn signatures_match(trait_sig: &InnerFunctionSignature, impl_sig: &InnerFunctionSignature) -> bool {
    trait_sig.function_kind == impl_sig.function_kind
        && trait_sig.return_type == impl_sig.return_type
        && trait_sig.parameters.len() == impl_sig.parameters.len()
        && trait_sig
            .parameters
            .iter()
            .zip(&impl_sig.parameters)
            .all(|(a, b)| a.ty == b.ty)
}
