use ast_model::{
    ExpressionId, ExpressionKind, FieldAccess, FunctionCall, FunctionCallee, FunctionCalleeKind,
    ImportItem, ImportKind, SolType, Stub, VariableExpression,
    declare_store::{FunctionLookup, FunctionResolve, IntrinsicResolve},
    scope::{ScopeModuleEntry, ScopeTypeEntryKind, ScopeValue},
};
use ast_parser::fault::AstErrorKind;
use sol_utils::sol_names::PrimitiveTypes;
use sol_utils::{
    FunctionId,
    intrinsics::IntrinsicFunction,
    sol_error_internal,
    span::{ModuleId, Span},
};
use std::str::FromStr;

use crate::NameResolver;

impl<'a> NameResolver<'a> {
    pub(super) fn resolve_function_call(
        &mut self,
        expression_id: ExpressionId,
        call: &FunctionCall,
    ) {
        for argument in &call.arguments {
            self.resolve_expression(argument.value);
        }

        if let Some(path) = self.try_get_intrinsic_path(call) {
            self.resolve_intrinsic_call(&path, call);
            return;
        }

        // `assert`/`panic` are the only intrinsics callable bare (no
        // `intrinsic.` prefix) — checked only when there's no callee at all,
        // so a qualified call (e.g. a method named `assert`) is unaffected.
        if call.callee.is_none()
            && let Ok(kind) = IntrinsicFunction::from_str(call.name.as_str())
            && kind.callable_bare()
        {
            self.check_and_insert_intrinsic(kind, call.name.as_str(), call);
            return;
        }

        self.resolve_call(expression_id, call);
    }

    fn resolve_variable_callable(
        &mut self,
        expression_id: ExpressionId,
        call: &FunctionCall,
    ) -> bool {
        let name = call.name.as_str();
        let Some(var_id) =
            self.scope_info
                .scopes
                .lookup_value(name, ScopeValue::Variable, self.current.module)
        else {
            return false;
        };

        if var_id < self.synthetic_id_boundary && call.id < var_id {
            self.log_error(
                AstErrorKind::VariableUsedBeforeDeclaration {
                    name: call.name.as_shared_str(),
                },
                Some(call.name.span()),
            );
            return true;
        }

        let Some((_, Some(SolType::Function { return_type, .. }), _)) =
            self.declares.get_variable_type(var_id)
        else {
            return false;
        };
        let Some(return_type) = self.declares.get_type(*return_type).cloned() else {
            return false;
        };

        self.declares
            .insert_expression_type(expression_id, return_type);
        true
    }

    fn try_get_intrinsic_path(&mut self, call: &FunctionCall) -> Option<String> {
        let callee = call.callee.as_ref()?;
        let value = match &callee.kind {
            FunctionCalleeKind::Type(_) => return None,
            FunctionCalleeKind::Expression(id) => *id,
        };

        let mut segments = self.collect_intrinsic_path_segments(value)?;
        segments.push(call.name.to_string());
        Some(segments.join("."))
    }

    fn collect_intrinsic_path_segments(&mut self, id: ExpressionId) -> Option<Vec<String>> {
        let expr = self.store.expressions.get(id)?;
        match &expr.node {
            ExpressionKind::Variable(VariableExpression { name, .. })
                if name.as_str() == "intrinsic" =>
            {
                Some(Vec::new())
            }
            ExpressionKind::FieldAccess(field_access) => {
                let mut segments = self.collect_intrinsic_path_segments(field_access.object)?;
                segments.push(field_access.field.to_string());
                Some(segments)
            }
            _ => None,
        }
    }

    fn resolve_intrinsic_call(&mut self, path: &str, call: &FunctionCall) {
        let Ok(kind) = IntrinsicFunction::from_str(path) else {
            self.log_error(
                AstErrorKind::UnknownIntrinsic { path: path.into() },
                Some(call.name.span()),
            );
            return;
        };

        self.check_and_insert_intrinsic(kind, path, call);
    }

    fn check_and_insert_intrinsic(
        &mut self,
        kind: IntrinsicFunction,
        path: &str,
        call: &FunctionCall,
    ) {
        if call.arguments.len() != kind.arity() {
            self.log_error(
                AstErrorKind::IntrinsicArityMismatch {
                    path: path.into(),
                    expected: kind.arity(),
                    got: call.arguments.len(),
                },
                Some(call.name.span()),
            );
        }

        self.declares
            .insert_intrinsic_resolve(call.id, IntrinsicResolve { kind });
    }

    fn resolve_call(&mut self, expression_id: ExpressionId, call: &FunctionCall) {
        let module_entry = match &self.try_get_callee_string(call) {
            Some(string) => self.lookup_module(string),
            None => None,
        };

        if let Some(module) = module_entry {
            self.resolve_module_call(expression_id, module, call);
            return;
        }

        if !self.is_function_imported(call) {
            let name = call.name.as_str();
            match self.lookup_function(name) {
                Some(id) => {
                    self.declares.insert_function_resolve(
                        call.id,
                        FunctionResolve {
                            id,
                            is_defer: false,
                            ignore_callee: false,
                        },
                    );
                    self.finish_call_resolution(expression_id, call, id);
                }
                None if self.resolve_variable_callable(expression_id, call) => {}
                None => {
                    self.log_error(
                        AstErrorKind::UndefinedFunction {
                            name: call.name.as_shared_str(),
                        },
                        Some(call.name.span()),
                    );
                    self.declares.insert_function_resolve(
                        call.id,
                        FunctionResolve {
                            id: FunctionId::ERROR,
                            is_defer: false,
                            ignore_callee: false,
                        },
                    );
                }
            }
            return;
        }

        let type_qualifier = self.parse_owner_type(call.callee.as_ref());
        let ignore_callee = type_qualifier.is_some();
        if let Some(callee) = &call.callee {
            let is_union = self.is_callee_union(callee).unwrap_or(false);
            if type_qualifier.is_some() && is_union {
                self.declares.insert_function_resolve(
                    call.id,
                    FunctionResolve {
                        id: FunctionId::ERROR,
                        is_defer: false,
                        ignore_callee,
                    },
                );
                return;
            }

            match &callee.kind {
                FunctionCalleeKind::Type(_) => (),
                FunctionCalleeKind::Expression(id) => {
                    if !ignore_callee {
                        self.resolve_expression(*id);
                    }
                }
            }
        }

        let name = call.name.as_str();
        let owner_type = self.get_owner_kind(type_qualifier.as_ref(), call);
        let has_owner_type = owner_type.is_some();
        let resolved = if has_owner_type {
            match self.declares.find_function(name, owner_type.as_ref()) {
                FunctionLookup::Found(id) => Some(id),
                FunctionLookup::NotFound => None,
                FunctionLookup::Ambiguous => {
                    self.log_error(
                        AstErrorKind::AmbiguousMethodCall {
                            name: call.name.as_shared_str(),
                        },
                        Some(call.name.span()),
                    );
                    self.declares.insert_function_resolve(
                        call.id,
                        FunctionResolve {
                            id: FunctionId::ERROR,
                            is_defer: false,
                            ignore_callee,
                        },
                    );
                    return;
                }
            }
        } else {
            self.lookup_function(name)
        };

        let Some(id) = resolved else {
            if !has_owner_type {
                if !self.resolve_variable_callable(expression_id, call) {
                    self.log_error(
                        AstErrorKind::UndefinedFunction {
                            name: call.name.as_shared_str(),
                        },
                        Some(call.name.span()),
                    );
                }
            } else if let Some(owner_ty) = &owner_type {
                self.check_enum_variant_construction(owner_ty, call);
            }
            return;
        };
        self.declares.insert_function_resolve(
            call.id,
            FunctionResolve {
                id,
                is_defer: false,
                ignore_callee: false,
            },
        );
        self.finish_call_resolution(expression_id, call, id);
    }

    fn get_owner_kind(
        &mut self,
        type_qualifier: Option<&SolType>,
        call: &FunctionCall,
    ) -> Option<SolType> {
        if let Some(ty) = type_qualifier {
            return Some(ty.clone());
        }

        let callee = call.callee.as_ref()?;
        match &callee.kind {
            FunctionCalleeKind::Type(id) => self.declares.get_type(*id).cloned(),
            FunctionCalleeKind::Expression(id) => self.expression_type(*id),
        }
    }

    fn is_callee_union(&self, callee: &FunctionCallee) -> Option<bool> {
        let ident = match &callee.kind {
            FunctionCalleeKind::Expression(id) => match &self.store.expressions.get(*id)?.node {
                ExpressionKind::Variable(VariableExpression { name, .. }) => name.as_str(),
                _ => return None,
            },
            FunctionCalleeKind::Type(id) => match self.declares.get_type(*id)? {
                SolType::Stub(stub) => &stub.name,
                SolType::Res { .. } => return Some(true),
                _ => return None,
            },
        };

        let entry = self.lookup_type(ident, self.current.module)?;
        Some(matches!(entry.kind, ScopeTypeEntryKind::Union))
    }

    fn is_function_imported(&mut self, call: &FunctionCall) -> bool {
        let function_name = call.name.as_str();
        let mut has_module_with_this = false;
        let mut matches_imported_item = false;

        let Some(modules) = self.scope_info.scopes.iter_modules(self.current.module) else {
            self.log_fault(sol_error_internal!(
                format!("{:?} not found", self.current.module),
                Some(call.name.span())
            ));
            return false;
        };

        for (_name, entry) in modules {
            match &entry.import_kind {
                ImportKind::Module => continue,
                ImportKind::This => has_module_with_this = true,
                ImportKind::Items { has_this, .. } if *has_this => {
                    has_module_with_this = true;
                }
                _ => (),
            }

            for item in &entry.imported_items {
                match item {
                    ImportItem::Normal(ident) => {
                        if ident.as_str() == function_name {
                            matches_imported_item = true;
                        }
                    }
                    ImportItem::Alias { alias, .. } => {
                        if alias.as_str() == function_name {
                            matches_imported_item = true;
                        }
                    }
                }
            }
        }

        !has_module_with_this || matches_imported_item
    }

    fn parse_owner_type(&mut self, callee: Option<&FunctionCallee>) -> Option<SolType> {
        let callee = callee?;
        let value = match &callee.kind {
            FunctionCalleeKind::Type(_) => return None,
            FunctionCalleeKind::Expression(val) => *val,
        };

        let expr_node = &self.store.expressions.get(value)?.node;
        match expr_node {
            ExpressionKind::Variable(VariableExpression { name, .. }) => {
                if self.contains_type(name.as_str()) {
                    let occurrence = self.node_generator.alloc();
                    return Some(SolType::Stub(Stub::new_at(name.as_str(), occurrence)));
                }
                if let Ok(prim) = PrimitiveTypes::from_str(name.as_str()) {
                    return Some(SolType::Primitive(prim));
                }
            }
            ExpressionKind::FieldAccess(field_access) => {
                return self.parse_owner_from_field_access(field_access);
            }
            _ => {}
        }

        None
    }

    fn parse_owner_from_field_access(&mut self, field_access: &FieldAccess) -> Option<SolType> {
        let (module_entry, field_name) = self.follow_field_access_to_module(field_access)?;
        let ast_module = self.ast_modules.get(module_entry.module_id)?;
        let header_entry = ast_module.header.get(&field_name)?;
        let custom_type = header_entry.custom_type.as_ref()?;
        let name = custom_type.value.name().as_str().to_string();
        let occurrence = self.node_generator.alloc();
        Some(SolType::Stub(Stub::new_at(name, occurrence)))
    }

    /// Walk a FieldAccess chain like `Std.Io.Stdout` to find the innermost
    /// module entry and the final field name.
    fn follow_field_access_to_module(
        &mut self,
        field_access: &FieldAccess,
    ) -> Option<(ScopeModuleEntry, String)> {
        let obj_expr = self.store.expressions.get(field_access.object)?;
        match &obj_expr.node {
            ExpressionKind::Variable(VariableExpression { name, .. }) => {
                let name_str = name.as_str();
                if let Some(module_entry) = self.lookup_module(name_str) {
                    return Some((module_entry, field_access.field.to_string()));
                }
                let module_entry = self.find_module_by_crate_name(name_str)?;
                Some((module_entry, field_access.field.to_string()))
            }
            ExpressionKind::FieldAccess(inner_fa) => {
                let (module_entry, _) = self.follow_field_access_to_module(inner_fa)?;
                let module_id = module_entry.module_id;
                let field_name = field_access.field.to_string();
                let inner_module = self.ast_modules.get(module_id)?;
                let header_entry = inner_module.header.get(&field_name)?;
                header_entry.custom_type.as_ref()?;
                Some((module_entry, field_name))
            }
            _ => None,
        }
    }

    fn find_module_by_crate_name(&mut self, name: &str) -> Option<ScopeModuleEntry> {
        if let Some(modules) = self.scope_info.scopes.iter_modules(self.current.module) {
            for (_, entry) in modules {
                if entry.crate_name.as_deref() == Some(name) {
                    return Some(entry.clone());
                }
            }
        }
        None
    }

    fn resolve_module_call(
        &mut self,
        expression_id: ExpressionId,
        module_entry: ScopeModuleEntry,
        call: &FunctionCall,
    ) {
        let resolve =
            self.lookup_module_function(&module_entry, call.name.as_str(), call.name.span());
        let id = match resolve {
            Some(id) => id,
            None => self.resolve_external_function(&module_entry, call),
        };

        let ignore_callee = id != FunctionId::ERROR;
        self.declares.insert_function_resolve(
            call.id,
            FunctionResolve {
                id,
                ignore_callee,
                is_defer: false,
            },
        );
        self.finish_call_resolution(expression_id, call, id);
    }

    fn resolve_external_function(
        &mut self,
        module_entry: &ScopeModuleEntry,
        call: &FunctionCall,
    ) -> FunctionId {
        let location = match &module_entry.crate_name {
            Some(crate_name) => format!("crate '{crate_name}'"),
            None => format!("module '{}'", module_entry.module_name),
        };
        self.log_error(
            AstErrorKind::FunctionNotFoundIn {
                function_name: call.name.as_shared_str(),
                location: location.into(),
            },
            Some(call.name.span()),
        );

        FunctionId::ERROR
    }

    fn lookup_module_function(
        &mut self,
        module_entry: &ScopeModuleEntry,
        function_name: &str,
        span: Span,
    ) -> Option<FunctionId> {
        if let Some(function_name) = self.lookup_function_import(module_entry, function_name) {
            return self.lookup_function(&function_name);
        }

        let module_id = module_entry.module_id;
        debug_assert!(module_id != ModuleId::ERROR);
        debug_assert!(self.ast_modules.contains(module_id));

        let header = &self.ast_modules.get(module_id)?.header;
        let entry = header.get(function_name)?.function?;
        if !entry.is_public {
            self.log_error(
                AstErrorKind::ItemIsPrivate {
                    kind: "function".into(),
                    name: function_name.into(),
                },
                Some(span),
            );
        }

        Some(entry.value)
    }

    fn lookup_function_import(
        &mut self,
        module_entry: &ScopeModuleEntry,
        function_name: &str,
    ) -> Option<String> {
        for item in &module_entry.imported_items {
            match item {
                ImportItem::Normal(name) => {
                    if name.as_str() == function_name {
                        return Some(name.to_string());
                    }
                }
                ImportItem::Alias { alias, name } => {
                    if alias.as_str() == function_name {
                        return Some(name.to_string());
                    }
                }
            }
        }

        None
    }

    fn try_get_callee_string(&mut self, call: &FunctionCall) -> Option<String> {
        let callee = call.callee.as_ref()?;
        let value = match &callee.kind {
            FunctionCalleeKind::Type(_) => return None,
            FunctionCalleeKind::Expression(val) => *val,
        };

        let Some(value) = self.store.expressions.get(value) else {
            self.log_fault(sol_error_internal!(
                format!("{value:?} not found"),
                Some(call.name.span())
            ));
            return None;
        };

        match &value.node {
            ExpressionKind::Variable(var) => Some(var.name.to_string()),
            _ => None,
        }
    }

    pub(super) fn contains_type(&mut self, ident: &str) -> bool {
        self.lookup_type(ident, self.current.module).is_some()
    }

    pub(super) fn lookup_module(&mut self, string: &str) -> Option<ScopeModuleEntry> {
        self.scope_info
            .scopes
            .lookup_module(string, self.current.module)
    }

    fn lookup_function(&mut self, string: &str) -> Option<FunctionId> {
        self.scope_info
            .scopes
            .lookup_function(string, self.current.module)
    }
}
