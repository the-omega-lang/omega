use crate::{
    context::{Context, ScopeContext},
    resolved_type::{ResolvedFunctionType, ResolvedType},
};
use omega_parser::{
    NodeId, SourceModule,
    prelude::{
        CodeblockExpr, Expression, ExpressionNode, ExternDeclarationStmt, FunctionDefinitionStmt,
        FunctionType, Ident, RootStatement, RootStatementNode, Statement, StatementNode, Type,
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
        let scope = self.context_mut().current_scope();
        match &typ {
            Type::Function(fntype) => {
                match ResolvedFunctionType::try_from(fntype.to_owned()) {
                    Ok(resolved) => {
                        scope.declared_functions.insert(ident, resolved);
                    }
                    Err(message) => {
                        self.errors.push(AnalysisError { node_id, message });
                    }
                };
            }
            typ => {
                let resolved_type = match ResolvedType::try_from(typ.to_owned()) {
                    Ok(t) => t,
                    Err(message) => {
                        self.errors.push(AnalysisError { node_id, message });
                        return;
                    }
                };

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
                match ResolvedType::try_from(*function_type.return_type) {
                    Ok(rettype) => {
                        self.expression_types_mut().insert(node_id, rettype);
                    }
                    Err(message) => {
                        self.errors.push(AnalysisError {
                            node_id,
                            message: message.to_string(),
                        });
                        return;
                    }
                }

                // Check types for arguments of the function call
                for i in 0..call_expr.args.len() {
                    let arg = &call_expr.args[i];
                    if i >= function_type.params.len() {
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
                    // if the expression type matches the parameter type
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

            _ => {}
        }
    }

    fn analyze_statement(&mut self, node: &StatementNode) {
        let stmt = &node.statement;
        match stmt {
            Statement::Declaration(decl) => {
                self.analyze_declaration(node.id, decl.ident.clone(), decl.r#type.clone())
            }
            Statement::Expression(expr) => self.analyze_expression(expr),
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
        let scope = self.context_mut().enter_scope();

        // Add function parameters to new scope
        // and analyze its codeblock
        for param in &function_def.params {
            let resolved_type = match ResolvedType::try_from(param.r#type.to_owned()) {
                Ok(t) => t,
                Err(message) => {
                    self.errors.push(AnalysisError { node_id, message });
                    return;
                }
            };
            scope
                .declared_variables
                .insert(param.ident.to_owned(), resolved_type);
        }
        self.analyze_codeblock(&function_def.codeblock);

        // Save function scope analysis
        let scope = self.context_mut().leave_scope();
        self.codeblock_scopes_mut().insert(node_id, scope);

        // Store function return type information
        let fn_type = match ResolvedFunctionType::try_from(function_def.function_type()) {
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
