use crate::resolved_type::{ResolvedFunctionType, ResolvedType};
use omega_parser::prelude::*;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct ScopeContext {
    pub declared_functions: HashMap<Ident, ResolvedFunctionType>,
    pub declared_variables: HashMap<Ident, ResolvedType>,
    pub defined_types: HashMap<Ident, ResolvedType>,
}

impl ScopeContext {
    fn new() -> Self {
        Self {
            declared_functions: HashMap::new(),
            declared_variables: HashMap::new(),
            defined_types: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Context {
    scopes: Vec<ScopeContext>,
}

impl Context {
    pub fn new() -> Self {
        let mut global_scope = ScopeContext::new();
        global_scope.defined_types.extend([
            // Standard types
            (Ident("void".into()), ResolvedType::Void),
            (Ident("char".into()), ResolvedType::Char),
            (Ident("i32".into()), ResolvedType::I32),
        ]);
        Self {
            scopes: vec![global_scope],
        }
    }

    // Finder functions
    pub fn find_variable_type(&self, name: &Ident) -> Option<&ResolvedType> {
        for scope in self.scopes.iter().rev() {
            if let Some(typ) = scope.declared_variables.get(name) {
                return Some(typ);
            }
        }

        None
    }

    pub fn find_function_type(&self, name: &Ident) -> Option<&ResolvedFunctionType> {
        for scope in self.scopes.iter().rev() {
            if let Some(typ) = scope.declared_functions.get(name) {
                return Some(typ);
            }
        }

        None
    }

    pub fn find_defined_type(&self, name: &Ident) -> Option<&ResolvedType> {
        for scope in self.scopes.iter().rev() {
            if let Some(typ) = scope.defined_types.get(name) {
                return Some(typ);
            }
        }

        None
    }

    pub fn resolve_function_type(
        &self,
        fntype: FunctionType,
    ) -> Result<ResolvedFunctionType, String> {
        Ok(ResolvedFunctionType {
            params: fntype
                .params
                .into_iter()
                .map(|(ident, typ)| self.resolve_type(typ).map(|resolved| (ident, resolved)))
                .collect::<Result<Vec<(Ident, ResolvedType)>, String>>()?,
            return_type: Box::new(self.resolve_type(*fntype.return_type)?),
            is_variadic: fntype.is_variadic,
        })
    }

    pub fn resolve_type(&self, typ: Type) -> Result<ResolvedType, String> {
        let resolved = match typ {
            Type::Named(name) => self
                .find_defined_type(&name)
                .ok_or_else(|| format!("Unrecognized named type: {}", name.0))?
                .to_owned(),
            Type::Pointer(pointee_type) => {
                ResolvedType::Pointer(Box::new(self.resolve_type(*pointee_type)?))
            }
            Type::Function(fntyp) => ResolvedType::Function(self.resolve_function_type(fntyp)?),
            Type::Array(item_type) => ResolvedType::Array(Box::new(self.resolve_type(*item_type)?)),
        };

        Ok(resolved)
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
