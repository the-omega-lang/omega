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
    /// `import`ed *generic* item aliases (`import mymodule::List;` where
    /// `List` turns out to be a generic struct or function) -- maps the
    /// bound alias to the item's absolute path. Kept separate from
    /// `imported_modules`/the ordinary local-binding tables `bind_imported_item`
    /// uses, since a generic item has no concrete `ResolvedType`/`VarBinding`
    /// to bind yet (there's nothing concrete until a use site supplies type
    /// arguments) -- this just records "this name means that absolute path,"
    /// substituted in wherever an unqualified reference would otherwise fall
    /// through to an implicit own-module-prefixed one (see
    /// `resolve_absolute_item_path`).
    generic_aliases: HashMap<Ident, Vec<Ident>>,
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
            (Ident("isize".into()), ResolvedType::ISize),
            (Ident("u8".into()), ResolvedType::U8),
            (Ident("u16".into()), ResolvedType::U16),
            (Ident("u32".into()), ResolvedType::U32),
            (Ident("u64".into()), ResolvedType::U64),
            (Ident("usize".into()), ResolvedType::USize),
            (Ident("f32".into()), ResolvedType::F32),
            (Ident("f64".into()), ResolvedType::F64),
        ]);
        Self {
            scopes: vec![global_scope],
            imported_modules: HashMap::new(),
            generic_aliases: HashMap::new(),
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

    /// Binds an imported *generic* item alias (`import mymodule::List;`
    /// where `List` is a generic struct/function) -- see `generic_aliases`'s
    /// doc comment.
    pub fn bind_generic_alias(&mut self, alias: Ident, absolute_path: Vec<Ident>) {
        self.generic_aliases.insert(alias, absolute_path);
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

    /// An imported *generic* item alias's absolute path, if `alias` is one
    /// (see `generic_aliases`'s doc comment) -- the `Analyzer::
    /// resolve_generic_call` counterpart to `absolute_path`, for the
    /// unqualified-only case a generic item import always is.
    pub fn generic_alias(&self, alias: &Ident) -> Option<&Vec<Ident>> {
        self.generic_aliases.get(alias)
    }

    /// A function/method signature's param and return types are never
    /// embedded inline into anything's layout (a function is called, not
    /// laid out inline) -- always `indirect = true`, regardless of what the
    /// caller itself was.
    pub fn resolve_function_type(
        &self,
        fntype: FunctionType,
        resolver: &mut dyn ModuleResolver,
        module_path: &[Ident],
    ) -> Result<ResolvedFunctionType, TypeResolutionError> {
        let params = fntype
            .params
            .into_iter()
            .map(|(ident, typ)| {
                self.resolve_type(typ, resolver, module_path, true)
                    .map(|resolved| (ident, resolved))
            })
            .collect::<Result<Vec<(Ident, ResolvedType)>, TypeResolutionError>>()?;
        Ok(ResolvedFunctionType {
            params,
            return_type: Box::new(self.resolve_type(*fntype.return_type, resolver, module_path, true)?),
            is_variadic: fntype.is_variadic,
            is_member_function: fntype.is_member_function,
        })
    }

    /// Resolves `path` to an absolute `[module_path.., name]`, the shared
    /// logic behind `Type::Named`'s and `Type::Generic`'s unqualified/
    /// qualified branches (kept as one method so this priority order is only
    /// written once): for an unqualified `path`, a `generic_aliases` hit
    /// (an imported generic item, see its doc comment) wins over the
    /// implicit own-module-prefixed fallback -- a generic item is never
    /// itself a `find_defined_type` entry, so callers still check that
    /// first, separately, for ordinary local shadowing. For a qualified
    /// `path`, this is exactly `absolute_path` (an imported module alias).
    fn resolve_absolute_item_path(
        &self,
        path: &Path,
        module_path: &[Ident],
    ) -> Result<Vec<Ident>, TypeResolutionError> {
        if path.is_unqualified() {
            if let Some(absolute) = self.generic_aliases.get(&path.head) {
                return Ok(absolute.clone());
            }
            Ok(module_path.iter().cloned().chain(std::iter::once(path.head.clone())).collect())
        } else {
            self.absolute_path(path)
                .ok_or_else(|| TypeResolutionError::UnrecognizedNamedType(path.head.clone()))
        }
    }

    /// `module_path` is the *caller's own* absolute module path -- used to
    /// build an implicit absolute path for an unqualified reference that
    /// isn't a builtin or a local (function-body-level) binding, so it can
    /// be resolved the exact same way a qualified cross-module one is (see
    /// `ModuleResolver::resolve_item`'s doc comment: there's no longer an
    /// architectural difference between the two).
    ///
    /// `indirect` is true whenever `typ` itself sits somewhere that never
    /// embeds its referent inline into another type's layout. It starts out
    /// as whatever the caller passed and only ever *turns on* as the walk
    /// descends: `Pointer`/`Array` (a thin pointer) and a `Function`'s own
    /// param/return types are never embedded inline into anything, so
    /// everything beneath them is indirect regardless of what it started as;
    /// `SizedArray` carries its element inline, so it just passes the
    /// current value through unchanged. See `ModuleResolver::resolve_item`
    /// for what this distinction ultimately protects.
    pub fn resolve_type(
        &self,
        typ: Type,
        resolver: &mut dyn ModuleResolver,
        module_path: &[Ident],
        indirect: bool,
    ) -> Result<ResolvedType, TypeResolutionError> {
        let resolved = match typ {
            Type::Named(path) if path.is_unqualified() => {
                if let Some(local) = self.find_defined_type(&path.head) {
                    local.to_owned()
                } else {
                    let absolute = self.resolve_absolute_item_path(&path, module_path)?;
                    match resolver
                        .resolve_item(&absolute, &[], indirect)
                        .map_err(TypeResolutionError::ModuleResolution)?
                    {
                        ResolvedItem::Type(t) => t,
                        ResolvedItem::Value { .. } => return Err(TypeResolutionError::NotAType(absolute)),
                    }
                }
            }
            // A qualified type reference (`mymodule::Foo`) -- `path`'s head
            // must already be an imported module alias (see `absolute_path`);
            // the rest is resolved across modules by `resolver`, never
            // locally.
            Type::Named(path) => {
                let absolute = self.resolve_absolute_item_path(&path, module_path)?;
                match resolver
                    .resolve_item(&absolute, &[], indirect)
                    .map_err(TypeResolutionError::ModuleResolution)?
                {
                    ResolvedItem::Type(t) => t,
                    ResolvedItem::Value { .. } => return Err(TypeResolutionError::NotAType(absolute)),
                }
            }
            // `Path<Type, ...>` -- a generic item referenced with explicit
            // type arguments (e.g. `List<u32>`). Args are resolved first
            // (always `indirect = true`: naming a type as a generic argument
            // never itself embeds it inline -- the *usage* inside the
            // template body decides that, at instantiation time), then the
            // whole reference is resolved exactly like `Type::Named`'s
            // (minus the local-shadowing check -- a generic item is never a
            // `find_defined_type` entry).
            Type::Generic(path, args) => {
                let resolved_args = args
                    .into_iter()
                    .map(|arg| self.resolve_type(arg, resolver, module_path, true))
                    .collect::<Result<Vec<_>, _>>()?;
                let absolute = self.resolve_absolute_item_path(&path, module_path)?;
                match resolver
                    .resolve_item(&absolute, &resolved_args, indirect)
                    .map_err(TypeResolutionError::ModuleResolution)?
                {
                    ResolvedItem::Type(t) => t,
                    ResolvedItem::Value { .. } => return Err(TypeResolutionError::NotAType(absolute)),
                }
            }
            // `*[T]` is a slice (a fat pointer), not a thin `Pointer` to an
            // `Array` -- `[T]` alone is unsized, so a pointer to it is
            // necessarily a different, wider representation (data pointer +
            // length), the same reasoning Rust's `&[T]` follows. Any other
            // pointee resolves to an ordinary thin `Pointer`, unchanged.
            Type::Pointer(pointee_type) => {
                match self.resolve_type(*pointee_type, resolver, module_path, true)? {
                    ResolvedType::Array(item_type) => ResolvedType::Slice(item_type),
                    other => ResolvedType::Pointer(Box::new(other)),
                }
            }
            Type::Function(fntyp) => ResolvedType::Function(self.resolve_function_type(fntyp, resolver, module_path)?),
            Type::Array(item_type) => {
                ResolvedType::Array(Box::new(self.resolve_type(*item_type, resolver, module_path, true)?))
            }
            Type::SizedArray(item_type, size) => {
                let size = size
                    .parse::<u32>()
                    .map_err(|_| TypeResolutionError::InvalidArraySize(size.clone()))?;
                ResolvedType::SizedArray(
                    Box::new(self.resolve_type(*item_type, resolver, module_path, indirect)?),
                    size,
                )
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
