use crate::{
    context::{Context, ScopeContext},
    error::{AnalysisError, AnalysisErrorKind},
    resolved_type::{ResolvedFunctionType, ResolvedStructType, ResolvedType},
};
use omega_hir::{
    HirExpr, HirExprNode, HirExternDeclaration, HirFunctionDef, HirId, HirItem, HirModule,
    HirPlace, HirPlaceRoot, HirProjection, HirStmt, HirStructDef,
};
use omega_parser::prelude::{Ident, SimpleSpan, Type};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Analysis {
    pub node_types: HashMap<HirId, ResolvedType>,
    pub context: Context,
    pub codeblock_scopes: HashMap<HirId, ScopeContext>,
    pub struct_scopes: HashMap<HirId, ScopeContext>,
}

impl Analysis {
    pub fn get_global_function_type(&self, name: &Ident) -> Option<&ResolvedFunctionType> {
        self.context.find_function_type(name)
    }

    pub fn get_node_type(&self, node_id: &HirId) -> Option<&ResolvedType> {
        self.node_types.get(node_id)
    }

    pub fn get_codeblock_scope(&self, node_id: &HirId) -> Option<&ScopeContext> {
        self.codeblock_scopes.get(node_id)
    }

    pub fn get_struct_scope(&self, node_id: &HirId) -> Option<&ScopeContext> {
        self.struct_scopes.get(node_id)
    }

    pub fn get_struct_function_type(
        &self,
        struct_node_id: HirId,
        name: &Ident,
    ) -> Option<&ResolvedFunctionType> {
        let scope = self.struct_scopes.get(&struct_node_id)?;
        scope.declared_functions.get(name)
    }
}

#[derive(Debug, Clone)]
pub struct Analyzer {
    errors: Vec<AnalysisError>,
    analysis: Analysis,
}

impl Analyzer {
    pub fn new() -> Self {
        Self {
            errors: vec![],
            analysis: Analysis {
                node_types: HashMap::new(),
                context: Context::new(),
                codeblock_scopes: HashMap::new(),
                struct_scopes: HashMap::new(),
            },
        }
    }

    // Utils

    fn node_types(&self) -> &HashMap<HirId, ResolvedType> {
        &self.analysis.node_types
    }

    fn node_types_mut(&mut self) -> &mut HashMap<HirId, ResolvedType> {
        &mut self.analysis.node_types
    }

    fn context(&self) -> &Context {
        &self.analysis.context
    }

    fn context_mut(&mut self) -> &mut Context {
        &mut self.analysis.context
    }

    fn codeblock_scopes_mut(&mut self) -> &mut HashMap<HirId, ScopeContext> {
        &mut self.analysis.codeblock_scopes
    }

    fn struct_scopes_mut(&mut self) -> &mut HashMap<HirId, ScopeContext> {
        &mut self.analysis.struct_scopes
    }

    // Analysis
    fn analyze_declaration(&mut self, node_id: HirId, span: SimpleSpan, ident: &Ident, typ: &Type) {
        let ctx = self.context_mut();
        match typ {
            Type::Function(fntype) => match ctx.resolve_function_type(fntype.to_owned()) {
                Ok(resolved) => {
                    let scope = ctx.current_scope();
                    scope
                        .declared_functions
                        .insert(ident.clone(), resolved.clone());
                    scope
                        .declared_variables
                        .insert(ident.clone(), ResolvedType::Function(resolved));
                }
                Err(err) => {
                    self.errors.push(AnalysisError::new(
                        node_id,
                        span,
                        AnalysisErrorKind::UnresolvedType(err),
                    ));
                }
            },
            typ => {
                let resolved_type = match ctx.resolve_type(typ.to_owned()) {
                    Ok(t) => t,
                    Err(err) => {
                        self.errors.push(AnalysisError::new(
                            node_id,
                            span,
                            AnalysisErrorKind::UnresolvedType(err),
                        ));
                        return;
                    }
                };

                let scope = ctx.current_scope();
                scope.declared_variables.insert(ident.clone(), resolved_type);
            }
        }
    }

    fn analyze_extern_decl(&mut self, extern_decl: &HirExternDeclaration) {
        self.analyze_declaration(
            extern_decl.id,
            extern_decl.span,
            &extern_decl.ident,
            &extern_decl.r#type,
        );
    }

    /// Resolves the type of a place expression by looking up its root, then
    /// folding over its projections in source order. Replaces the old
    /// "hacky mutations" approach: `HirPlace` is already a settled, flat
    /// structure by the time analysis sees it, so this is a plain fold with
    /// no shared mutable side-table involved.
    fn analyze_place(
        &mut self,
        node_id: HirId,
        span: SimpleSpan,
        place: &HirPlace,
    ) -> Option<ResolvedType> {
        let mut current_type = match &place.root {
            HirPlaceRoot::Ident(ident) => {
                let Some(vartype) = self.context().find_variable_type(ident) else {
                    self.errors.push(AnalysisError::new(
                        node_id,
                        span,
                        AnalysisErrorKind::UndefinedVariable(ident.clone()),
                    ));
                    return None;
                };
                vartype.clone()
            }
            HirPlaceRoot::Expr(expr) => {
                self.analyze_expression(expr);
                let Some(typ) = self.node_types().get(&expr.id) else {
                    self.errors.push(AnalysisError::new(
                        node_id,
                        span,
                        AnalysisErrorKind::UnresolvedInnerExpression,
                    ));
                    return None;
                };
                typ.clone()
            }
        };

        for projection in &place.projections {
            match projection {
                HirProjection::FieldAccess(field) => {
                    let ResolvedType::Struct(struct_type) = current_type else {
                        self.errors
                            .push(AnalysisError::new(node_id, span, AnalysisErrorKind::NotAStruct));
                        return None;
                    };
                    let Some(field_type) = struct_type.fields.iter().find(|x| &x.0 == field)
                    else {
                        self.errors.push(AnalysisError::new(
                            node_id,
                            span,
                            AnalysisErrorKind::NoSuchField(field.clone()),
                        ));
                        return None;
                    };
                    current_type = field_type.1.clone();
                }
                HirProjection::Index(index_expr) => {
                    self.analyze_expression(index_expr);
                    let ResolvedType::Array(item_type) = current_type else {
                        self.errors
                            .push(AnalysisError::new(node_id, span, AnalysisErrorKind::NotAnArray));
                        return None;
                    };
                    current_type = *item_type;
                }
            }
        }

        Some(current_type)
    }

    fn analyze_expression(&mut self, node: &HirExprNode) {
        let node_id = node.id;

        match &node.expr {
            HirExpr::String(_) => {
                self.node_types_mut()
                    .insert(node_id, ResolvedType::Pointer(Box::new(ResolvedType::Char)));
            }

            HirExpr::FunctionCall(call_expr) => {
                self.analyze_expression(&call_expr.callee);
                let Some(ResolvedType::Function(function_type)) =
                    self.node_types().get(&call_expr.callee.id)
                else {
                    self.errors.push(AnalysisError::new(
                        node_id,
                        node.span,
                        AnalysisErrorKind::UnresolvedCallee,
                    ));
                    return;
                };

                let function_type = function_type.to_owned();
                self.node_types_mut()
                    .insert(node_id, *function_type.return_type.clone());

                for i in 0..call_expr.args.len() {
                    let arg = &call_expr.args[i];
                    if i >= function_type.params.len() && !function_type.is_variadic {
                        self.errors.push(AnalysisError::new(
                            arg.id,
                            arg.span,
                            AnalysisErrorKind::TooManyArguments {
                                expected: function_type.params.len(),
                            },
                        ));
                        return;
                    }

                    self.analyze_expression(arg);

                    if i >= function_type.params.len() && function_type.is_variadic {
                        continue;
                    }

                    if let Some(typ) = self.node_types().get(&arg.id) {
                        let expected_type = &function_type.params[i].1;
                        if typ != expected_type {
                            self.errors.push(AnalysisError::new(
                                arg.id,
                                arg.span,
                                AnalysisErrorKind::ArgumentTypeMismatch {
                                    expected: expected_type.clone(),
                                    found: typ.clone(),
                                },
                            ));
                        }
                    }
                }
            }

            HirExpr::Number(number_expr) => {
                if let Some(explicit_type) = &number_expr.explicit_type {
                    let Ok(resolved_type) = self
                        .context()
                        .resolve_type(Type::Named(explicit_type.clone()))
                    else {
                        self.errors.push(AnalysisError::new(
                            node_id,
                            node.span,
                            AnalysisErrorKind::InvalidNumberType(explicit_type.clone()),
                        ));
                        return;
                    };
                    self.node_types_mut().insert(node_id, resolved_type);
                    return;
                }

                // TODO: Handle floats and unsigned integers
                self.node_types_mut().insert(node_id, ResolvedType::I32);
            }

            HirExpr::Assignment(assignment) => {
                self.analyze_expression(&assignment.value);
                let Some(typ) = self.node_types().get(&assignment.value.id) else {
                    self.errors.push(AnalysisError::new(
                        node_id,
                        node.span,
                        AnalysisErrorKind::UnresolvedInnerExpression,
                    ));
                    return;
                };
                let typ = typ.clone();

                self.analyze_expression(&assignment.target);

                self.node_types_mut().insert(node_id, typ);
            }

            HirExpr::Place(place) => {
                if let Some(typ) = self.analyze_place(node_id, node.span, place) {
                    self.node_types_mut().insert(node_id, typ);
                }
            }

            HirExpr::Codeblock(_) => {}
        }
    }

    fn analyze_struct_def(&mut self, struct_def: &HirStructDef) {
        let resolved_fields = match struct_def
            .fields
            .iter()
            .map(|field| {
                self.context()
                    .resolve_type(field.r#type.to_owned())
                    .map(|resolved_type| (field.ident.to_owned(), resolved_type))
            })
            .collect::<Result<Vec<(Ident, ResolvedType)>, _>>()
        {
            Ok(r) => r,
            Err(err) => {
                self.errors.push(AnalysisError::new(
                    struct_def.id,
                    struct_def.span,
                    AnalysisErrorKind::UnresolvedType(err),
                ));
                return;
            }
        };

        // TODO: Make sure type does not already exist
        self.context_mut().current_scope().defined_types.insert(
            struct_def.name.to_owned(),
            ResolvedType::Struct(ResolvedStructType {
                fields: resolved_fields,
                functions: vec![],
            }),
        );

        self.context_mut().enter_scope();
        for function_def in &struct_def.functions {
            self.analyze_function_def(function_def);
            let Some(function_type) = self
                .context()
                .current_scope_not_mut()
                .declared_functions
                .get(&function_def.name)
            else {
                self.errors.push(AnalysisError::new(
                    function_def.id,
                    function_def.span,
                    AnalysisErrorKind::FunctionTypeLookupFailed,
                ));
                continue;
            };
            let function_type = function_type.clone();

            let ResolvedType::Struct(resolved_struct_type) = self
                .context_mut()
                .parent_scope()
                .defined_types
                .get_mut(&struct_def.name)
                .unwrap()
            else {
                panic!(
                    "This should never happen. If it did, congrats. No idea how you've done it."
                );
            };

            resolved_struct_type
                .functions
                .push((function_def.name.clone(), function_type));
        }

        let scope = self.context_mut().leave_scope();
        self.struct_scopes_mut().insert(struct_def.id, scope);
    }

    fn analyze_statement(&mut self, stmt: &HirStmt) {
        match stmt {
            HirStmt::Declaration(decl) => {
                self.analyze_declaration(decl.id, decl.span, &decl.ident, &decl.r#type)
            }
            HirStmt::Expression(expr) => self.analyze_expression(expr),
            HirStmt::Return(expr) => self.analyze_expression(expr),
            HirStmt::Struct(struct_def) => self.analyze_struct_def(struct_def),
            HirStmt::ExternDeclaration(_) => {}
        }
    }

    fn analyze_function_def(&mut self, function_def: &HirFunctionDef) {
        // Add function parameters to new scope
        // and analyze its body
        self.context_mut().enter_scope();
        for param in &function_def.params {
            let resolved_type = match self.context().resolve_type(param.r#type.to_owned()) {
                Ok(t) => t,
                Err(err) => {
                    self.errors.push(AnalysisError::new(
                        function_def.id,
                        function_def.span,
                        AnalysisErrorKind::UnresolvedType(err),
                    ));
                    return;
                }
            };
            self.context_mut()
                .current_scope()
                .declared_variables
                .insert(param.ident.to_owned(), resolved_type);
        }
        for stmt in &function_def.body {
            self.analyze_statement(stmt);
        }

        // Save function scope analysis
        let scope = self.context_mut().leave_scope();
        self.codeblock_scopes_mut().insert(function_def.id, scope);

        // Store function return type information
        let fn_type = match self
            .context()
            .resolve_function_type(function_def.function_type())
        {
            Ok(typ) => typ,
            Err(err) => {
                self.errors.push(AnalysisError::new(
                    function_def.id,
                    function_def.span,
                    AnalysisErrorKind::UnresolvedType(err),
                ));
                return;
            }
        };
        self.context_mut()
            .current_scope()
            .declared_functions
            .insert(function_def.name.to_owned(), fn_type);
    }

    fn analyze_item(&mut self, item: &HirItem) {
        match item {
            HirItem::ExternDeclaration(stmt) => self.analyze_extern_decl(stmt),
            HirItem::FunctionDefinition(stmt) => self.analyze_function_def(stmt),
            HirItem::Struct(stmt) => self.analyze_struct_def(stmt),
            HirItem::Declaration(_) => {}
        }
    }

    pub fn analyze(mut self, hir_module: &HirModule) -> Result<Analysis, Vec<AnalysisError>> {
        for item in &hir_module.items {
            self.analyze_item(item);
        }

        if !self.errors.is_empty() {
            return Err(self.errors);
        }

        Ok(self.analysis)
    }
}
