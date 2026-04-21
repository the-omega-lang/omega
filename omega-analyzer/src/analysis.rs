use crate::{
    context::{Context, ScopeContext},
    resolved_type::{self, ResolvedFunctionType, ResolvedStructType, ResolvedType},
};
use omega_parser::{
    NodeId, SourceModule,
    prelude::{
        CodeblockExpr, Expression, ExpressionNode, ExternDeclarationStmt, FunctionDefinitionStmt,
        FunctionType, Ident, ReturnStmt, RootStatement, RootStatementNode, Statement,
        StatementNode, StructStmt, Type,
    },
};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct AnalysisError {
    pub node_id: NodeId,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct Analysis {
    pub expression_types: HashMap<NodeId, ResolvedType>,
    pub context: Context,
    pub codeblock_scopes: HashMap<NodeId, ScopeContext>,
}

impl Analysis {
    pub fn get_global_function_type(&self, name: &Ident) -> Option<&ResolvedFunctionType> {
        self.context.find_function_type(name)
    }

    pub fn get_expression_type(&self, node_id: &NodeId) -> Option<&ResolvedType> {
        self.expression_types.get(&node_id)
    }

    pub fn get_codeblock_scope(&self, codeblock_node_id: &NodeId) -> Option<&ScopeContext> {
        self.codeblock_scopes.get(&codeblock_node_id)
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

        Self {
            errors,
            analysis: Analysis {
                expression_types,
                context,
                codeblock_scopes,
            },
        }
    }

    // Utils

    fn expression_types(&self) -> &HashMap<NodeId, ResolvedType> {
        &self.analysis.expression_types
    }

    fn expression_types_mut(&mut self) -> &mut HashMap<NodeId, ResolvedType> {
        &mut self.analysis.expression_types
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

    // Analysis
    fn analyze_declaration(&mut self, node_id: NodeId, ident: Ident, typ: Type) {
        let ctx = self.context_mut();
        match &typ {
            Type::Function(fntype) => {
                match ctx.resolve_function_type(fntype.to_owned()) {
                    Ok(resolved) => {
                        let scope = ctx.current_scope();
                        scope.declared_functions.insert(ident, resolved);
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
                    .insert(ident.to_owned(), resolved_type);
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

    fn analyze_expression(&mut self, node: &ExpressionNode) {
        let expr = &node.expression;
        let node_id = node.id;

        match expr {
            Expression::String(_) => {
                self.expression_types_mut()
                    .insert(node.id, ResolvedType::Pointer(Box::new(ResolvedType::Char)));
            }
            Expression::FunctionCall(call_expr) => {
                let Some(function_type) =
                    self.context().find_function_type(&call_expr.function_name)
                else {
                    self.errors.push(AnalysisError {
                        node_id,
                        message: format!("Undefined function '{}'", call_expr.function_name.0),
                    });
                    return;
                };

                // Store function call expression type
                let function_type = function_type.to_owned();
                self.expression_types_mut()
                    .insert(node_id, *function_type.return_type);

                // Check types for arguments of the function call
                for i in 0..call_expr.args.len() {
                    let arg = &call_expr.args[i];
                    if i >= function_type.params.len() && !function_type.is_variadic {
                        self.errors.push(AnalysisError {
                            node_id: arg.id,
                            message: format!(
                                "Too many arguments for function '{}'. Expected: {}",
                                call_expr.function_name.0,
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

                    if let Some(typ) = self.expression_types().get(&arg.id) {
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
                    self.expression_types_mut().insert(node.id, resolved_type);
                    return;
                }

                // TODO: Handle floats and unsigned integers
                self.expression_types_mut()
                    .insert(node.id, ResolvedType::I32);
            }

            Expression::Assignment(assignment) => {
                self.analyze_expression(&assignment.value);
                let Some(typ) = self.expression_types().get(&assignment.value.id) else {
                    self.errors.push(AnalysisError {
                        node_id,
                        message: "Inner expression not resolved".to_string(),
                    });
                    return;
                };
                let typ = typ.clone();

                self.expression_types_mut().insert(node.id, typ);
            }

            Expression::Index(index) => {
                self.analyze_expression(&index.indexed);
                self.analyze_expression(&index.index);

                let Some(array_type) = self.expression_types().get(&index.indexed.id) else {
                    self.errors.push(AnalysisError {
                        node_id,
                        message: "Inner expression not resolved".to_string(),
                    });
                    return;
                };

                let ResolvedType::Array(item_type) = array_type else {
                    self.errors.push(AnalysisError {
                        node_id,
                        message: "Indexed expression is not an array".to_string(),
                    });
                    return;
                };

                let typ = *item_type.to_owned();
                self.expression_types_mut().insert(node.id, typ);
            }

            Expression::Ident(ident) => {
                let Some(typ) = self.context().find_variable_type(&ident) else {
                    self.errors.push(AnalysisError {
                        node_id,
                        message: "Unknown variable type".to_string(),
                    });
                    return;
                };
                let typ = typ.to_owned();

                self.expression_types_mut().insert(node_id, typ);
            }

            _ => {}
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

        self.context_mut().current_scope().defined_types.insert(
            struct_stmt.ident.to_owned(),
            ResolvedType::Struct(ResolvedStructType {
                fields: resolved_fields,
            }),
        );
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
