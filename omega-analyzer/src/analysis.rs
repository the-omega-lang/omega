use crate::{
    context::{Context, ScopeContext},
    place::{Place, PlaceRoot},
    resolved_type::{self, ResolvedFunctionType, ResolvedStructType, ResolvedType},
};
use omega_parser::{
    NodeId, SourceModule,
    prelude::{
        CodeblockExpr, DeclarationStmt, Expression, ExpressionNode, ExternDeclarationStmt,
        FunctionDefinitionStmt, FunctionType, Ident, PlaceExpr, ReturnStmt, RootStatement,
        RootStatementNode, Statement, StatementNode, StructStmt, Type,
    },
    syntax::place::PlaceModifierPostfix,
};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct AnalysisError {
    pub node_id: NodeId,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct Analysis {
    pub node_types: HashMap<NodeId, ResolvedType>,
    pub places: HashMap<NodeId, Place>,
    pub context: Context,
    pub codeblock_scopes: HashMap<NodeId, ScopeContext>,
    pub struct_scopes: HashMap<NodeId, ScopeContext>,
}

impl Analysis {
    pub fn get_global_function_type(&self, name: &Ident) -> Option<&ResolvedFunctionType> {
        self.context.find_function_type(name)
    }

    pub fn get_node_type(&self, node_id: &NodeId) -> Option<&ResolvedType> {
        self.node_types.get(&node_id)
    }

    pub fn get_codeblock_scope(&self, codeblock_node_id: &NodeId) -> Option<&ScopeContext> {
        self.codeblock_scopes.get(&codeblock_node_id)
    }

    pub fn get_struct_scope(&self, codeblock_node_id: &NodeId) -> Option<&ScopeContext> {
        self.struct_scopes.get(&codeblock_node_id)
    }

    pub fn get_place(&self, node_id: &NodeId) -> Option<&Place> {
        self.places.get(&node_id)
    }

    pub fn get_struct_function_type(
        &self,
        struct_node_id: NodeId,
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
        let errors = vec![];
        let expression_types = HashMap::new();
        let context = Context::new();
        let codeblock_scopes = HashMap::new();
        let struct_scopes = HashMap::new();
        let places = HashMap::new();

        Self {
            errors,
            analysis: Analysis {
                node_types: expression_types,
                context,
                codeblock_scopes,
                struct_scopes,
                places,
            },
        }
    }

    // Utils

    fn node_types(&self) -> &HashMap<NodeId, ResolvedType> {
        &self.analysis.node_types
    }

    fn node_types_mut(&mut self) -> &mut HashMap<NodeId, ResolvedType> {
        &mut self.analysis.node_types
    }

    fn places(&self) -> &HashMap<NodeId, Place> {
        &self.analysis.places
    }

    fn places_mut(&mut self) -> &mut HashMap<NodeId, Place> {
        &mut self.analysis.places
    }

    fn context(&self) -> &Context {
        &self.analysis.context
    }

    fn context_mut(&mut self) -> &mut Context {
        &mut self.analysis.context
    }

    fn codeblock_scopes(&self) -> &HashMap<NodeId, ScopeContext> {
        &self.analysis.codeblock_scopes
    }

    fn codeblock_scopes_mut(&mut self) -> &mut HashMap<NodeId, ScopeContext> {
        &mut self.analysis.codeblock_scopes
    }

    fn struct_scopes(&self) -> &HashMap<NodeId, ScopeContext> {
        &self.analysis.struct_scopes
    }

    fn struct_scopes_mut(&mut self) -> &mut HashMap<NodeId, ScopeContext> {
        &mut self.analysis.struct_scopes
    }

    // Analysis
    fn analyze_declaration(&mut self, node_id: NodeId, ident: Ident, typ: Type) {
        let ctx = self.context_mut();
        match &typ {
            Type::Function(fntype) => {
                match ctx.resolve_function_type(fntype.to_owned()) {
                    Ok(resolved) => {
                        let scope = ctx.current_scope();
                        scope
                            .declared_functions
                            .insert(ident.clone(), resolved.clone());
                        scope
                            .declared_variables
                            .insert(ident, ResolvedType::Function(resolved));
                    }
                    Err(message) => {
                        self.errors.push(AnalysisError { node_id, message });
                    }
                };
            }
            typ => {
                let resolved_type = match ctx.resolve_type(typ.to_owned()) {
                    Ok(t) => t,
                    Err(message) => {
                        self.errors.push(AnalysisError { node_id, message });
                        return;
                    }
                };

                let scope = ctx.current_scope();
                scope
                    .declared_variables
                    .insert(ident, resolved_type.clone());
            }
        }
    }

    fn analyze_extern_decl(&mut self, node_id: NodeId, extern_decl: &ExternDeclarationStmt) {
        self.analyze_declaration(
            node_id,
            extern_decl.ident.to_owned(),
            extern_decl.r#type.to_owned(),
        );
    }

    fn analyze_place(&mut self, node_id: NodeId, place: &PlaceExpr) {
        self.analyze_expression(&place.base);

        let root = match self.places_mut().get_mut(&place.base.id) {
            Some(r) => r,
            None =>
            // TODO: Properly handle derefs
            {
                let new_place = Place {
                    root: PlaceRoot::Deref(place.base.clone()),
                    modifiers: vec![],
                };

                self.places_mut().insert(place.base.id, new_place);
                self.places_mut().get_mut(&place.base.id).unwrap()
            }
        };
        let modifier = &place.modifier;
        root.modifiers.push(modifier.clone());

        let Some(typ) = self.analysis.get_node_type(&place.base.id) else {
            self.errors.push(AnalysisError {
                node_id,
                message: "Unknown variable type".to_string(),
            });
            return;
        };
        let typ = typ.to_owned();

        let mut last_type = typ;
        match modifier {
            PlaceModifierPostfix::FieldAccess(ident) => {
                let ResolvedType::Struct(struct_type) = last_type else {
                    self.errors.push(AnalysisError {
                        node_id: node_id,
                        message: "Not a struct".to_string(),
                    });
                    return;
                };
                let Some(field_type) = struct_type.fields.iter().find(|x| &x.0 == ident) else {
                    self.errors.push(AnalysisError {
                        node_id: node_id,
                        message: "No such field in the struct".to_string(),
                    });
                    return;
                };

                last_type = field_type.1.clone();
            }
            PlaceModifierPostfix::Index(_expr) => {
                let ResolvedType::Array(array_item_type) = last_type else {
                    self.errors.push(AnalysisError {
                        node_id: node_id,
                        message: "Not an array".to_string(),
                    });
                    return;
                };
                last_type = *array_item_type;
            }
        }

        self.node_types_mut().insert(node_id, last_type);
    }

    fn analyze_expression(&mut self, node: &ExpressionNode) {
        let expr = &node.expression;
        let node_id = node.id;

        match expr {
            Expression::String(_) => {
                self.node_types_mut()
                    .insert(node.id, ResolvedType::Pointer(Box::new(ResolvedType::Char)));
            }
            Expression::FunctionCall(call_expr) => {
                self.analyze_expression(call_expr.callee.as_ref());
                let Some(ResolvedType::Function(function_type)) =
                    self.node_types().get(&call_expr.callee.id)
                else {
                    self.errors.push(AnalysisError {
                        node_id,
                        message: format!(
                            "Unknown or invalid type for callee: '{:?}'",
                            call_expr.callee
                        ),
                    });
                    return;
                };

                // Store function call expression type
                let function_type = function_type.to_owned();
                self.node_types_mut()
                    .insert(node_id, *function_type.return_type);

                // Check types for arguments of the function call
                for i in 0..call_expr.args.len() {
                    let arg = &call_expr.args[i];
                    if i >= function_type.params.len() && !function_type.is_variadic {
                        self.errors.push(AnalysisError {
                            node_id: arg.id,
                            message: format!(
                                "Too many arguments for function. Expected: {}",
                                function_type.params.len()
                            ),
                        });
                        return;
                    }

                    self.analyze_expression(arg);

                    // If the arg was successfully analyzed, lets check
                    // if the expression type matches the parameter type.
                    // ONLY if the function is not variadic.
                    if i >= function_type.params.len() && function_type.is_variadic {
                        continue;
                    }

                    if let Some(typ) = self.node_types().get(&arg.id) {
                        let expected_type = &function_type.params[i].1;
                        if typ != expected_type {
                            self.errors.push(AnalysisError {
                                node_id: arg.id,
                                message: format!(
                                    "Expected type '{:?}' for argument, found: {:?}",
                                    expected_type, typ
                                ),
                            });
                        }
                    }
                }
            }

            Expression::Number(number_expr) => {
                if let Some(explicit_type) = &number_expr.explicit_type {
                    let Ok(resolved_type) = self
                        .context()
                        .resolve_type(Type::Named(explicit_type.clone()))
                    else {
                        self.errors.push(AnalysisError {
                            node_id,
                            message: format!(
                                "Invalid type for number expression: {:?}",
                                explicit_type
                            ),
                        });
                        return;
                    };
                    self.node_types_mut().insert(node.id, resolved_type);
                    return;
                }

                // TODO: Handle floats and unsigned integers
                self.node_types_mut().insert(node.id, ResolvedType::I32);
            }

            Expression::Assignment(assignment) => {
                self.analyze_expression(&assignment.value);
                let Some(typ) = self.node_types().get(&assignment.value.id) else {
                    self.errors.push(AnalysisError {
                        node_id,
                        message: "Inner expression not resolved".to_string(),
                    });
                    return;
                };
                let typ = typ.clone();

                self.analyze_expression(&assignment.place);

                self.node_types_mut().insert(node.id, typ);
            }

            Expression::Place(place) => {
                self.analyze_place(node_id, &place);
                let Some(typ) = self.node_types().get(&node_id) else {
                    self.errors.push(AnalysisError {
                        node_id,
                        message: format!("Failed to analyze Place ({})", node_id),
                    });
                    return;
                };
                let typ = typ.clone();
                self.node_types_mut().insert(node.id, typ);
            }

            Expression::Ident(ident) => {
                let Some(vartype) = self.context().find_variable_type(&ident) else {
                    self.errors.push(AnalysisError {
                        node_id,
                        message: format!("Undefined variable '{}'", ident.as_ref()),
                    });
                    return;
                };
                let vartype = vartype.clone();
                self.node_types_mut().insert(node.id, vartype);
                self.places_mut().insert(
                    node_id,
                    Place {
                        root: PlaceRoot::Ident(ident.clone()),
                        modifiers: vec![],
                    },
                );
            }

            Expression::Codeblock(_) => {}
        }
    }

    fn analyze_return(&mut self, return_stmt: &ReturnStmt) {
        self.analyze_expression(&return_stmt.return_value);
    }

    fn analyze_struct_def(&mut self, node_id: NodeId, struct_stmt: &StructStmt) {
        let resolved_fields = match struct_stmt
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
            Err(e) => {
                self.errors.push(AnalysisError {
                    node_id,
                    message: e,
                });
                return;
            }
        };

        // TODO: Make sure type does not already exist
        self.context_mut().current_scope().defined_types.insert(
            struct_stmt.ident.to_owned(),
            ResolvedType::Struct(ResolvedStructType {
                fields: resolved_fields,
                functions: vec![],
            }),
        );

        self.context_mut().enter_scope();
        for (node_id, mut function_def) in struct_stmt.functions.clone() {
            // Add "self" pointer to member functions
            if function_def.is_member_function {
                function_def.params.insert(
                    0,
                    DeclarationStmt {
                        ident: Ident("self".to_string()),
                        r#type: Type::Pointer(Box::new(Type::Named(struct_stmt.ident.clone()))),
                    },
                )
            }

            self.analyze_function_def(node_id.clone(), &function_def);
            let Some(function_type) = self
                .context()
                .current_scope_not_mut()
                .declared_functions
                .get(&function_def.function_name)
            else {
                self.errors.push(AnalysisError {
                    node_id,
                    message: "Failed to get function type".to_owned(),
                });
                continue;
            };
            let function_type = function_type.clone();

            let ResolvedType::Struct(resolved_struct_type) = self
                .context_mut()
                .parent_scope()
                .defined_types
                .get_mut(&struct_stmt.ident)
                .unwrap()
            else {
                panic!(
                    "This should never happen. If it did, congrats. No idea how you've done it."
                );
            };

            resolved_struct_type
                .functions
                .push((function_def.function_name, function_type));
        }

        let scope = self.context_mut().leave_scope();
        self.struct_scopes_mut().insert(node_id, scope);
    }

    fn analyze_statement(&mut self, node: &StatementNode) {
        let stmt = &node.statement;
        match stmt {
            Statement::Declaration(decl) => {
                self.analyze_declaration(node.id, decl.ident.clone(), decl.r#type.clone())
            }
            Statement::Expression(expr) => self.analyze_expression(expr),
            Statement::Return(ret) => self.analyze_return(ret),
            Statement::Struct(stmt) => self.analyze_struct_def(node.id, stmt),
            _ => {}
        }
    }

    // NOTE: This function assumes the scope context has already been set up
    fn analyze_codeblock(&mut self, codeblock: &CodeblockExpr) {
        for stmt in &codeblock.0 {
            self.analyze_statement(stmt);
        }
    }

    fn analyze_function_def(&mut self, node_id: NodeId, function_def: &FunctionDefinitionStmt) {
        // Add function parameters to new scope
        // and analyze its codeblock
        self.context_mut().enter_scope();
        for param in &function_def.params {
            let resolved_type = match self.context().resolve_type(param.r#type.to_owned()) {
                Ok(t) => t,
                Err(message) => {
                    self.errors.push(AnalysisError { node_id, message });
                    return;
                }
            };
            self.context_mut()
                .current_scope()
                .declared_variables
                .insert(param.ident.to_owned(), resolved_type);
        }
        self.analyze_codeblock(&function_def.codeblock);

        // Save function scope analysis
        let scope = self.context_mut().leave_scope();
        self.codeblock_scopes_mut().insert(node_id, scope);

        // Store function return type information
        let fn_type = match self
            .context()
            .resolve_function_type(function_def.function_type())
        {
            Ok(typ) => typ,
            Err(message) => {
                self.errors.push(AnalysisError { node_id, message });
                return;
            }
        };
        self.context_mut()
            .current_scope()
            .declared_functions
            .insert(function_def.function_name.to_owned(), fn_type);
    }

    fn analyze_root_stmt(&mut self, node: &RootStatementNode) {
        let root_stmt = &node.root_stmt;
        let node_id = node.id;

        match root_stmt {
            RootStatement::ExternDeclaration(stmt) => self.analyze_extern_decl(node_id, stmt),
            RootStatement::FunctionDefinition(stmt) => self.analyze_function_def(node_id, stmt),
            RootStatement::Struct(stmt) => self.analyze_struct_def(node_id, stmt),
            _ => {}
        }
    }

    pub fn analyze(mut self, source_module: &SourceModule) -> Result<Analysis, Vec<AnalysisError>> {
        for root_stmt in &source_module.nodes {
            self.analyze_root_stmt(root_stmt);
        }

        if !self.errors.is_empty() {
            return Err(self.errors);
        }

        Ok(self.analysis)
    }
}
