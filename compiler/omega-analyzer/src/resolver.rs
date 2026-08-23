use crate::checked::Storage;
use crate::resolved_type::{
    ConstValue, ResolvedConformance, ResolvedFunctionType, ResolvedGap, ResolvedMethod,
    ResolvedSpecType, ResolvedType,
};
use omega_hir::{HirGenericParam, HirId};
use omega_parser::prelude::{Ident, Origin, Type, Visibility};
use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

#[derive(Debug, Clone)]
pub enum ResolvedItem {
    Type(ResolvedType),
    Value {
        r#type: ResolvedType,
        storage: Storage,
        decl_id: HirId,
        mutable: bool,
    },
    Gap(Rc<ResolvedGap>),
}

/// What a declared `alias` names. An alias has no identity of its own, so
/// every variant is either the target's absolute path or the structural type
/// syntax the target was written as.
#[derive(Debug, Clone)]
pub enum ResolvedAlias {
    Module(Vec<Ident>),
    /// A top-level declaration, named exactly as if its own path were written
    /// at the use site. Covers types, specs, functions and overload sets, and
    /// macros alike -- the use site decides which namespace it wanted.
    Item(Vec<Ident>),
    /// A structural target, expanded before the use site applies contextual
    /// type semantics so that `spec A + B` behaves identically whether it was
    /// written literally or reached through an alias. Every path in `r#type`
    /// and `generics` already carries an origin naming the alias
    /// declaration's module, so expansion needs no module threading.
    Type {
        generics: Vec<HirGenericParam>,
        r#type: Type,
    },
}

#[derive(Debug, Clone)]
pub enum ImportTarget {
    Module(Vec<Ident>),
    Item(Vec<Ident>, ResolvedItem),
    GenericItem(Vec<Ident>),
}

#[derive(Debug, Clone)]
pub enum ResolveError {
    UnknownModule(Vec<Ident>),
    /// An unprefixed import's head is not a known top-level package
    /// identity (the local package or a registered dependency).
    UnknownTopLevelPackage(Ident),
    /// A `super::` chain removed more segments than the importing
    /// module's package-relative depth allows.
    SuperAboveRoot { depth: u32, importer: Vec<Ident> },
    UnknownItem {
        module: Vec<Ident>,
        item: Ident,
    },
    NotVisible {
        module: Vec<Ident>,
        item: Ident,
    },
    Cycle(Vec<Vec<Ident>>),
    AmbiguousModule(Vec<Ident>),
    /// A filesystem module candidate whose `.omg` file stem or directory
    /// segment is not a spelling the parser can tokenize as an identifier.
    /// `path` is the valid ancestor prefix; `invalid` is the raw offending
    /// segment (not wrapped in `Ident`, since it is by definition not one).
    InvalidModuleName { path: Vec<Ident>, invalid: String },
    LoadFailed {
        path: Vec<Ident>,
        message: String,
    },
    RecursiveTypeWithoutIndirection {
        module: Vec<Ident>,
        item: Ident,
    },
    ItemFailed {
        module: Vec<Ident>,
        item: Ident,
    },
    GenericArgCountMismatch {
        module: Vec<Ident>,
        item: Ident,
        expected: usize,
        found: usize,
    },
    SpecNotImplemented {
        type_name: String,
        spec: Ident,
        missing: Vec<Ident>,
    },
    SpecDependencyCycle {
        module: Vec<Ident>,
        spec: Ident,
    },
    AmbiguousAmbientName {
        name: Ident,
        candidates: Vec<Vec<Ident>>,
    },
    /// An `alias` whose target is a declaration kind an alias never names.
    InvalidAliasTarget {
        module: Vec<Ident>,
        declared: Ident,
        target: Vec<Ident>,
        kind: &'static str,
    },
}

fn join(path: &[Ident]) -> String {
    path.iter()
        .map(|i| i.as_ref())
        .collect::<Vec<_>>()
        .join("::")
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownModule(path) => write!(f, "cannot find module '{}'", join(path)),
            Self::UnknownTopLevelPackage(name) => write!(
                f,
                "'{}' is not a known top-level package (unprefixed imports are top-level; use `root::`, `self::`, or `super::` for local navigation, or register a dependency with --import={}:<path>)",
                name.as_ref(),
                name.as_ref()
            ),
            Self::SuperAboveRoot { depth, importer } => write!(
                f,
                "'super' used {} time(s) from '{}' would cross the package root",
                depth,
                join(importer)
            ),
            Self::UnknownItem { module, item } => {
                write!(
                    f,
                    "cannot find '{}' in module '{}'",
                    item.as_ref(),
                    join(module)
                )
            }
            Self::NotVisible { module, item } => {
                write!(
                    f,
                    "'{}::{}' is not visible here",
                    join(module),
                    item.as_ref()
                )
            }
            Self::Cycle(path) => write!(
                f,
                "cyclic resolution dependency: {}",
                path.iter()
                    .map(|p| join(p))
                    .collect::<Vec<_>>()
                    .join(" -> ")
            ),
            Self::AmbiguousModule(path) => write!(
                f,
                "module '{}' is ambiguous (both a file and a directory claim this name)",
                join(path)
            ),
            Self::InvalidModuleName { path, invalid } => {
                if path.is_empty() {
                    write!(f, "'{invalid}' is not a valid Omega module name")
                } else {
                    write!(
                        f,
                        "'{invalid}' under module '{}' is not a valid Omega module name",
                        join(path)
                    )
                }
            }
            Self::LoadFailed { path, message } => {
                write!(f, "failed to load module '{}': {message}", join(path))
            }
            Self::RecursiveTypeWithoutIndirection { module, item } => write!(
                f,
                "recursive type '{}::{}' has infinite size",
                join(module),
                item.as_ref()
            ),
            Self::ItemFailed { module, item } => {
                write!(
                    f,
                    "cannot use '{}::{}' because of its own error",
                    join(module),
                    item.as_ref()
                )
            }
            Self::GenericArgCountMismatch {
                module,
                item,
                expected,
                found,
            } => write!(
                f,
                "'{}::{}' expects {expected} type argument(s), found {found}",
                join(module),
                item.as_ref()
            ),
            Self::SpecNotImplemented {
                type_name,
                spec,
                missing,
            } => write!(
                f,
                "'{type_name}' does not implement spec '{}' (missing: {})",
                spec.as_ref(),
                missing
                    .iter()
                    .map(Ident::as_ref)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::SpecDependencyCycle { module, spec } => {
                write!(
                    f,
                    "spec '{}::{}' depends on itself",
                    join(module),
                    spec.as_ref()
                )
            }
            Self::InvalidAliasTarget {
                module,
                declared,
                target,
                kind,
            } => write!(
                f,
                "alias '{}::{}' cannot name {kind} '{}'",
                join(module),
                declared.as_ref(),
                join(target)
            ),
            Self::AmbiguousAmbientName { name, candidates } => write!(
                f,
                "'{}' is ambiguous: it's exposed by more than one core module ({})",
                name.as_ref(),
                candidates
                    .iter()
                    .map(|c| join(c))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

impl std::error::Error for ResolveError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolveItemOptions {
    indirect: bool,
    bypass_visibility: bool,
}

impl ResolveItemOptions {
    pub const DIRECT: Self = Self {
        indirect: false,
        bypass_visibility: false,
    };

    pub const INDIRECT: Self = Self {
        indirect: true,
        bypass_visibility: false,
    };

    pub const fn with_indirection(indirect: bool) -> Self {
        Self {
            indirect,
            ..Self::DIRECT
        }
    }

    pub const fn through_indirection(mut self) -> Self {
        self.indirect = true;
        self
    }

    pub const fn bypassing_visibility(mut self, bypass: bool) -> Self {
        self.bypass_visibility = bypass;
        self
    }

    pub const fn allows_indirection(self) -> bool {
        self.indirect
    }

    pub const fn bypasses_visibility(self) -> bool {
        self.bypass_visibility
    }
}

pub trait ModuleResolver {
    fn macro_origin_module(&self, origin: Origin) -> Option<Vec<Ident>>;

    fn macro_origin_visibility(&self, origin: Origin) -> Option<Visibility>;

    fn declared_item_visibility(&mut self, absolute_path: &[Ident]) -> Option<Visibility>;

    fn resolve_import_alias(
        &mut self,
        module_path: &[Ident],
        alias: &Ident,
    ) -> Result<Option<ImportTarget>, ResolveError>;

    /// The `alias` declared under `name` in `module_path`, if there is one.
    /// Follows alias chains and reports a cycle rather than recursing.
    fn resolve_declared_alias(
        &mut self,
        module_path: &[Ident],
        name: &Ident,
    ) -> Result<Option<ResolvedAlias>, ResolveError>;

    fn ambient_core_candidates(
        &mut self,
        accessor: &[Ident],
        name: &Ident,
    ) -> Result<Option<Vec<Ident>>, ResolveError>;

    fn import_alias_names(&mut self, module_path: &[Ident]) -> Vec<Ident>;

    fn raw_import_absolute_path(
        &mut self,
        module_path: &[Ident],
        alias: &Ident,
    ) -> Result<Option<(Vec<Ident>, bool)>, ResolveError>;

    fn resolve_item(
        &mut self,
        accessor_module_path: &[Ident],
        absolute_path: &[Ident],
        type_args: &[ResolvedType],
        options: ResolveItemOptions,
    ) -> Result<ResolvedItem, ResolveError>;

    fn is_item_visible(&mut self, accessor_module_path: &[Ident], absolute_path: &[Ident]) -> bool;

    fn generic_function_signature(
        &mut self,
        absolute_path: &[Ident],
    ) -> Result<Option<GenericSignature>, ResolveError>;

    fn generic_literal_signature(
        &mut self,
        absolute_path: &[Ident],
        variant: Option<&Ident>,
    ) -> Result<Option<GenericLiteralSignature>, ResolveError>;

    fn generic_static_function_signature(
        &mut self,
        owner_absolute: &[Ident],
        function_name: &Ident,
    ) -> Result<Option<GenericStaticFunctionSignature>, ResolveError>;

    fn function_overload_signatures(
        &mut self,
        module_path: &[Ident],
        name: &Ident,
    ) -> Result<Option<OverloadCandidates>, ResolveError>;

    fn fresh_synthetic_id(&mut self) -> HirId;

    fn similar_item_name(
        &mut self,
        module_path: &[Ident],
        target: &Ident,
        namespace: ItemNamespace,
    ) -> Option<Ident>;

    fn spec_declaration(
        &mut self,
        absolute_path: &[Ident],
    ) -> Result<Option<Rc<RefCell<ResolvedSpecType>>>, ResolveError>;

    fn primitive_methods(
        &mut self,
        receiver: &ResolvedType,
    ) -> Result<Vec<(Ident, ResolvedMethod)>, ResolveError>;

    fn conformance_for(
        &mut self,
        target: &ResolvedType,
        spec: &Rc<RefCell<ResolvedSpecType>>,
        spec_args: &[ResolvedType],
    ) -> Result<Option<ResolvedConformance>, ResolveError>;

    fn conformances_for_type(
        &mut self,
        target: &ResolvedType,
    ) -> Result<Vec<ResolvedConformance>, ResolveError>;

    fn conformances_for_specs(
        &mut self,
        target: &ResolvedType,
        spec_ids: &[HirId],
    ) -> Result<Vec<ResolvedConformance>, ResolveError>;

    fn resolve_function_body(
        &mut self,
        decl_id: HirId,
    ) -> Result<Option<crate::checked::CheckedFunctionDef>, ResolveError>;

    fn resolve_comp_value(&mut self, decl_id: HirId) -> Option<ConstValue>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemNamespace {
    Value,
    Type,
}

#[derive(Debug, Clone)]
pub struct OverloadCandidate {
    pub decl_id: HirId,
    pub fn_type: ResolvedFunctionType,
    pub visibility: Visibility,
}

pub type OverloadCandidates = Vec<OverloadCandidate>;

#[derive(Debug, Clone)]
pub struct GenericSignature {
    pub generics: Vec<Ident>,
    pub defaults: Vec<Option<Type>>,
    pub params: Vec<Type>,
    pub return_type: Type,
}

#[derive(Debug, Clone)]
pub struct GenericLiteralSignature {
    pub generics: Vec<Ident>,
    pub defaults: Vec<Option<Type>>,
    pub fields: Vec<(Ident, Type)>,
}

#[derive(Debug, Clone)]
pub struct GenericStaticFunctionSignature {
    pub owner_generics: Vec<Ident>,
    pub owner_defaults: Vec<Option<Type>>,
    pub function_generics: Vec<Ident>,
    pub params: Vec<Type>,
    pub return_type: Type,
}
