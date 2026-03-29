use std::collections::HashMap;

use omega_parser::prelude::*;

#[derive(Debug, Clone)]
pub struct ScopeContext {
    pub declared_functions: HashMap<Ident, FunctionType>,
    pub declared_variables: HashMap<Ident, Type>,
}

impl ScopeContext {
    fn new() -> Self {
        Self {
            declared_functions: HashMap::new(),
            declared_variables: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Context {
    scopes: Vec<ScopeContext>,
}

impl Context {
    pub fn new() -> Self {
        Self {
            scopes: vec![ScopeContext::new()],
        }
    }

    // Finder functions
    pub fn find_variable_type(&self, name: &Ident) -> Option<&Type> {
        for scope in self.scopes.iter().rev() {
            if let Some(typ) = scope.declared_variables.get(name) {
                return Some(typ);
            }
        }

        None
    }

    pub fn find_function_type(&self, name: &Ident) -> Option<&FunctionType> {
        for scope in self.scopes.iter().rev() {
            if let Some(typ) = scope.declared_functions.get(name) {
                return Some(typ);
            }
        }

        None
    }

    // Scope helpers
    pub fn current_scope(&mut self) -> &mut ScopeContext {
        self.scopes.last_mut().unwrap()
    }

    pub fn enter_scope(&mut self) -> &mut ScopeContext {
        self.scopes.push(ScopeContext::new());
        self.current_scope()
    }

    pub fn leave_scope(&mut self) -> ScopeContext {
        if self.scopes.len() == 1 {
            // The Context must always
            // have at least one scope
            let scope = self.scopes.remove(0);
            self.scopes.push(ScopeContext::new());
            return scope;
        }

        self.scopes
            .pop()
            .expect("BAD: Context does not have a scope. This should NEVER happen.")
    }
}
