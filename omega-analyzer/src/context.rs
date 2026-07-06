use crate::checked::Storage;
use crate::error::TypeResolutionError;
use crate::resolved_type::{ResolvedFunctionType, ResolvedType};
use crate::resolver::{ModuleResolver, ResolvedItem};
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
    /// Whole-module `import` aliases (`import mymodule;`, or `import
    /// mymodule::thing;` where `thing` turns out to be a submodule rather
    /// than an item) -- flat, not scope-stacked, since imports are root-level
    /// only (see `omega_parser::syntax::statement::import`). Maps the bound
    /// alias (always the import path's *last* segment) to the absolute
    /// module path it names. An *item*-level import (`import
    /// mymodule::foo;`) never goes here -- it binds `foo` directly into the
    /// current scope's `declared_variables`/`defined_types` instead, via
    /// `bind_imported_item`, exactly like `Context::new` already seeds
    /// builtin primitives.
    imported_modules: HashMap<Ident, Vec<Ident>>,
}

impl Context {
    pub fn new() -> Self {
        let mut global_scope = ScopeContext::new();
        global_scope.defined_types.extend([
            // Standard types
            (Ident("void".into()), ResolvedType::Void),
            (Ident("bool".into()), ResolvedType::Bool),
            (Ident("char".into()), ResolvedType::Char),
            (Ident("i8".into()), ResolvedType::I8),
            (Ident("i16".into()), ResolvedType::I16),
            (Ident("i32".into()), ResolvedType::I32),
            (Ident("i64".into()), ResolvedType::I64),
            (Ident("u8".into()), ResolvedType::U8),
            (Ident("u16".into()), ResolvedType::U16),
            (Ident("u32".into()), ResolvedType::U32),
            (Ident("u64".into()), ResolvedType::U64),
            (Ident("f32".into()), ResolvedType::F32),
            (Ident("f64".into()), ResolvedType::F64),
        ]);
        Self {
            scopes: vec![global_scope],
            imported_modules: HashMap::new(),
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

    /// Binds a whole-module import alias (`import mymodule;`, or the
    /// submodule case of `import mymodule::thing;`) -- `alias` is always the
    /// import path's last segment (see `omega_analyzer::analysis::Analyzer::
    /// process_import`).
    pub fn import_module(&mut self, alias: Ident, absolute_path: Vec<Ident>) {
        self.imported_modules.insert(alias, absolute_path);
    }

    /// Binds an imported *item* directly by name (`import mymodule::foo;`
    /// then bare `foo()`) -- one mechanism, reused from `Context::new`'s own
    /// builtin-primitive seeding: an imported item just becomes an ordinary
    /// local binding in the current (module-level) scope.
    pub fn bind_imported_item(&mut self, name: Ident, item: ResolvedItem) -> Result<(), Ident> {
        match item {
            ResolvedItem::Type(resolved_type) => {
                if self.current_scope().defined_types.contains_key(&name) {
                    return Err(name);
                }
                self.current_scope().defined_types.insert(name, resolved_type);
                Ok(())
            }
            ResolvedItem::Value { r#type, storage, decl_id } => {
                self.current_scope().declare(name, VarBinding { decl_id, storage, r#type })
            }
        }
    }

    /// Substitutes `path`'s head for whatever absolute module path it's
    /// aliased to (via a whole-module `import`), producing a full absolute
    /// path (e.g. after `import mymodule;`, `mymodule::thing::foo` becomes
    /// `["mymodule", "thing", "foo"]`). `None` means `path`'s head isn't an
    /// imported module alias at all -- per requirement 7 ("whatever is not
    /// imported is not visible"), that's a hard error the caller reports,
    /// not a fallback to some other lookup.
    pub fn absolute_path(&self, path: &Path) -> Option<Vec<Ident>> {
        let target = self.imported_modules.get(&path.head)?;
        Some(target.iter().cloned().chain(path.tail.iter().cloned()).collect())
    }

    pub fn resolve_function_type(
        &self,
        fntype: FunctionType,
        resolver: &mut dyn ModuleResolver,
    ) -> Result<ResolvedFunctionType, TypeResolutionError> {
        let params = fntype
            .params
            .into_iter()
            .map(|(ident, typ)| self.resolve_type(typ, resolver).map(|resolved| (ident, resolved)))
            .collect::<Result<Vec<(Ident, ResolvedType)>, TypeResolutionError>>()?;
        Ok(ResolvedFunctionType {
            params,
            return_type: Box::new(self.resolve_type(*fntype.return_type, resolver)?),
            is_variadic: fntype.is_variadic,
            is_member_function: fntype.is_member_function,
        })
    }

    pub fn resolve_type(
        &self,
        typ: Type,
        resolver: &mut dyn ModuleResolver,
    ) -> Result<ResolvedType, TypeResolutionError> {
        let resolved = match typ {
            Type::Named(path) if path.is_unqualified() => self
                .find_defined_type(&path.head)
                .ok_or_else(|| TypeResolutionError::UnrecognizedNamedType(path.head.clone()))?
                .to_owned(),
            // A qualified type reference (`mymodule::Foo`) -- `path`'s head
            // must already be an imported module alias (see `absolute_path`);
            // the rest is resolved across modules by `resolver`, never
            // locally.
            Type::Named(path) => {
                let absolute = self
                    .absolute_path(&path)
                    .ok_or_else(|| TypeResolutionError::UnrecognizedNamedType(path.head.clone()))?;
                match resolver.resolve_item(&absolute).map_err(TypeResolutionError::ModuleResolution)? {
                    ResolvedItem::Type(t) => t,
                    ResolvedItem::Value { .. } => return Err(TypeResolutionError::NotAType(absolute)),
                }
            }
            // `*[T]` is a slice (a fat pointer), not a thin `Pointer` to an
            // `Array` -- `[T]` alone is unsized, so a pointer to it is
            // necessarily a different, wider representation (data pointer +
            // length), the same reasoning Rust's `&[T]` follows. Any other
            // pointee resolves to an ordinary thin `Pointer`, unchanged.
            Type::Pointer(pointee_type) => match self.resolve_type(*pointee_type, resolver)? {
                ResolvedType::Array(item_type) => ResolvedType::Slice(item_type),
                other => ResolvedType::Pointer(Box::new(other)),
            },
            Type::Function(fntyp) => ResolvedType::Function(self.resolve_function_type(fntyp, resolver)?),
            Type::Array(item_type) => ResolvedType::Array(Box::new(self.resolve_type(*item_type, resolver)?)),
            Type::SizedArray(item_type, size) => {
                let size = size
                    .parse::<u32>()
                    .map_err(|_| TypeResolutionError::InvalidArraySize(size.clone()))?;
                ResolvedType::SizedArray(Box::new(self.resolve_type(*item_type, resolver)?), size)
            }
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
