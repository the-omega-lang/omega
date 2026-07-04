use crate::checked::Storage;
use crate::error::TypeResolutionError;
use crate::resolved_type::{ResolvedFunctionType, ResolvedType};
use omega_hir::HirId;
use omega_parser::prelude::*;
use std::collections::HashMap;

/// What a name resolves to within a scope: the declaring node's own id (so
/// codegen can key its storage maps by declaration identity instead of by
/// name), where its value physically lives, and its resolved type. Anything
/// callable by name -- extern function decls, local function defs, struct
/// methods within their own struct scope -- is bound here too, with
/// `storage: Storage::Function`; there is no separate function-only table.
#[derive(Debug, Clone)]
pub struct VarBinding {
    pub decl_id: HirId,
    pub storage: Storage,
    pub r#type: ResolvedType,
}

#[derive(Debug, Clone)]
pub struct ScopeContext {
    pub declared_variables: HashMap<Ident, VarBinding>,
    pub defined_types: HashMap<Ident, ResolvedType>,
}

impl ScopeContext {
    fn new() -> Self {
        Self {
            declared_variables: HashMap::new(),
            defined_types: HashMap::new(),
        }
    }

    /// Binds `ident` in this scope, or returns it back as `Err` if it's
    /// already declared *in this scope* -- shadowing an outer scope is
    /// ordinary lexical scoping and stays allowed. Centralizes a check that
    /// used to live, wrongly, in codegen (a name-keyed stack-slot map, which
    /// only coincidentally caught same-function redeclaration and never
    /// caught it for parameters at all).
    pub fn declare(&mut self, ident: Ident, binding: VarBinding) -> Result<(), Ident> {
        if self.declared_variables.contains_key(&ident) {
            return Err(ident);
        }
        self.declared_variables.insert(ident, binding);
        Ok(())
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
    pub fn find_variable(&self, ident: &Ident) -> Option<&VarBinding> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.declared_variables.get(ident))
    }

    pub fn find_defined_type(&self, name: &Ident) -> Option<&ResolvedType> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.defined_types.get(name))
    }

    pub fn resolve_function_type(
        &self,
        fntype: FunctionType,
    ) -> Result<ResolvedFunctionType, TypeResolutionError> {
        let params = fntype
            .params
            .into_iter()
            .map(|(ident, typ)| self.resolve_type(typ).map(|resolved| (ident, resolved)))
            .collect::<Result<Vec<(Ident, ResolvedType)>, TypeResolutionError>>()?;
        Ok(ResolvedFunctionType {
            params,
            return_type: Box::new(self.resolve_type(*fntype.return_type)?),
            is_variadic: fntype.is_variadic,
            is_member_function: fntype.is_member_function,
        })
    }

    pub fn resolve_type(&self, typ: Type) -> Result<ResolvedType, TypeResolutionError> {
        let resolved = match typ {
            Type::Named(name) => self
                .find_defined_type(&name)
                .ok_or_else(|| TypeResolutionError::UnrecognizedNamedType(name.clone()))?
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
