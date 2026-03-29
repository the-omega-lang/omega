use crate::context::{Context, ScopeContext};
use omega_parser::{
    NodeId, SourceModule,
    prelude::{
        CodeblockExpr, ExpressionNode, ExternDeclarationStmt, FunctionDefinitionStmt, FunctionType,
        Ident, RootStatement, RootStatementNode, Statement, StatementNode, Type,
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
    pub expression_types: HashMap<NodeId, Type>,
    pub context: Context,
    pub codeblock_scopes: HashMap<NodeId, ScopeContext>,
}

#[derive(Debug, Clone)]
pub struct Analyzer {
    errors: Vec<AnalysisError>,
    analysis: Analysis,
}

impl Analyzer {
    pub fn new() -> Self {
        let mut errors = vec![];
        let mut expression_types = HashMap::new();
        let mut context = Context::new();
        let mut codeblock_scopes = HashMap::new();

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

    fn expression_types(&mut self) -> &mut HashMap<NodeId, Type> {
        &mut self.analysis.expression_types
    }

    fn context(&mut self) -> &mut Context {
        &mut self.analysis.context
    }

    fn codeblock_scopes(&mut self) -> &mut HashMap<NodeId, ScopeContext> {
        &mut self.analysis.codeblock_scopes
    }

    // Analysis
    fn analyze_declaration(&mut self, ident: Ident, typ: Type) {
        let scope = self.context().current_scope();
        match typ {
            Type::Function(fntype) => {
                scope
                    .declared_functions
                    .insert(ident.to_owned(), fntype.to_owned());
            }
            typ => {
                scope
                    .declared_variables
                    .insert(ident.to_owned(), typ.to_owned());
            }
        }
    }

    fn analyze_extern_decl(&mut self, extern_decl: &ExternDeclarationStmt) {
        self.analyze_declaration(extern_decl.ident.to_owned(), extern_decl.r#type.to_owned());
    }

    fn analyze_expression(&mut self, node: &ExpressionNode) {}

    fn analyze_statement(&mut self, node: &StatementNode) {
        let stmt = &node.statement;
        match stmt {
            Statement::Declaration(decl) => {
                self.analyze_declaration(decl.ident.clone(), decl.r#type.clone())
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
        let scope = self.context().enter_scope();

        // Add function parameters to new scope
        // and analyze its codeblock
        for param in &function_def.params {
            scope
                .declared_variables
                .insert(param.ident.to_owned(), param.r#type.to_owned());
        }
        self.analyze_codeblock(&function_def.codeblock);

        // Save function scope analysis
        let scope = self.context().leave_scope();
        self.codeblock_scopes().insert(node_id, scope);

        // Store function return type information
        self.context().current_scope().declared_functions.insert(
            function_def.function_name.to_owned(),
            function_def.function_type(),
        );
    }

    fn analyze_root_stmt(&mut self, node: &RootStatementNode) {
        let root_stmt = &node.root_stmt;
        let node_id = node.id;

        match root_stmt {
            RootStatement::ExternDeclaration(stmt) => self.analyze_extern_decl(stmt),
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
