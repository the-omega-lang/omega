use crate::checked::Storage;
use crate::resolved_type::{
    ConstValue, ResolvedConformance, ResolvedFunctionType, ResolvedGap, ResolvedMethod,
    ResolvedSpecType, ResolvedType,
};
use omega_hir::{HirGenericParam, HirId};
use omega_parser::prelude::{Ident, Origin, Path, Type, Visibility};
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
    /// An overload set, whose candidates are frozen to exactly those the
    /// alias declaration site could name. Candidates are identified by their
    /// own `decl_id`s: an alias re-exports the functions that already exist,
    /// so it must not invent a wrapper, a new signature, or a new identity
    /// for them -- and it must not widen the set later either, which is what
    /// re-deriving visibility at each caller would do.
    Overloads {
        absolute: Vec<Ident>,
        candidates: Vec<HirId>,
    },
}

/// The overload candidates a written name offers one caller, together with
/// the group's own absolute path -- never an alias's path, since an alias
/// forwards its target's identity.
#[derive(Debug, Clone)]
pub struct ResolvedOverloadSet {
    pub absolute: Vec<Ident>,
    pub candidates: OverloadCandidates,
}

/// An absolute item path together with whether the eventual `resolve_item`
/// is already authorized to reach it without re-checking the accessor's own
/// visibility.
///
/// The bypass bit is a capability that was granted once, at the binding that
/// produced this path, and must survive every hop between that binding and
/// the item query: a validated declared-alias chain (whose every link was
/// gated at its own declaration site) and an `import reveal` both grant it.
/// Losing it silently turns an authorized reference back into an
/// unauthorized one.
#[derive(Debug, Clone)]
pub struct ItemAccess {
    pub absolute: Vec<Ident>,
    pub bypass_visibility: bool,
}

impl ItemAccess {
    /// A path the accessor must still be allowed to name for itself.
    pub fn gated(absolute: Vec<Ident>) -> Self {
        Self {
            absolute,
            bypass_visibility: false,
        }
    }

    /// A path whose target the accessor has already been authorized to reach.
    pub fn authorized(absolute: Vec<Ident>) -> Self {
        Self {
            absolute,
            bypass_visibility: true,
        }
    }

    /// `options` with this access's authorization folded in. An already-set
    /// bypass is never cleared: authorization only accumulates.
    pub fn options(&self, options: ResolveItemOptions) -> ResolveItemOptions {
        options.bypassing_visibility(self.bypass_visibility || options.bypasses_visibility())
    }
}

#[derive(Debug, Clone)]
pub enum ImportTarget {
    Module(Vec<Ident>),
    Item(Vec<Ident>, ResolvedItem),
    /// A name bound to a path whose item is resolved later: an ordinary
    /// (non-alias) generic template, whose arity and contents are unknown
    /// until a use site supplies arguments, or a declared alias, which never
    /// flattens into its target. The binding's own authorization travels
    /// with the path (see [`ItemAccess`]) so the deferred item query applies
    /// exactly the rights this binding established.
    ItemPath(ItemAccess),
}

#[derive(Debug, Clone)]
pub enum ResolveError {
    UnknownModule(Vec<Ident>),
    /// An unprefixed import's head is not a known top-level package
    /// identity (the local package or a registered dependency).
    UnknownTopLevelPackage(Ident),
    /// A `super::` chain removed more segments than the importing
    /// module's package-relative depth allows.
    SuperAboveRoot {
        depth: u32,
        importer: Vec<Ident>,
    },
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
    InvalidModuleName {
        path: Vec<Ident>,
        invalid: String,
    },
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
    /// An `alias`'s own generic parameter list binds a name it may not:
    /// a language type spelling, or one it already bound. Alias placeholders
    /// are validated symbolically here rather than through the analyzer's
    /// generic registration, so the rule has to be restated at this site.
    InvalidAliasGenericParam {
        module: Vec<Ident>,
        declared: Ident,
        param: Ident,
        /// Predicate completing "generic parameter '<param>' ...".
        reason: &'static str,
    },
    /// An `alias` whose target is a declaration kind an alias never names.
    InvalidAliasTarget {
        module: Vec<Ident>,
        declared: Ident,
        target: Vec<Ident>,
        kind: &'static str,
        /// The target was written inside type syntax, where the legal alias
        /// namespace is narrower: only a type or spec belongs there, even
        /// though a bare alias may forward a function, macro or module.
        type_position: bool,
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
            Self::InvalidAliasGenericParam {
                module,
                declared,
                param,
                reason,
            } => write!(
                f,
                "generic parameter '{}' of alias '{}::{}' {reason}",
                param.as_ref(),
                join(module),
                declared.as_ref()
            ),
            Self::InvalidAliasTarget {
                module,
                declared,
                target,
                kind,
                ..
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

    /// Resolves a `Path`'s explicit anchor (`root::`/`self::`/`super::`)
    /// relative to `origin_module`. Returns `None` when the path carries no
    /// explicit anchor, so the caller falls back to its own unanchored
    /// lookup rules instead of treating the path as navigation.
    fn resolve_explicit_anchor(
        &self,
        origin_module: &[Ident],
        path: &Path,
    ) -> Option<Result<Vec<Ident>, ResolveError>>;

    /// Resolves an absolute module path through physical modules and declared
    /// module aliases. Every alias segment is checked from `accessor`; the
    /// returned path is the canonical physical module the binding denotes.
    /// `None` means the path is not a module binding, so callers may try a
    /// type/item-qualified interpretation instead.
    fn resolve_module_path(
        &mut self,
        accessor: &[Ident],
        absolute_path: &[Ident],
    ) -> Result<Option<Vec<Ident>>, ResolveError>;

    fn declared_item_visibility(&mut self, absolute_path: &[Ident]) -> Option<Visibility>;

    fn resolve_import_alias(
        &mut self,
        module_path: &[Ident],
        alias: &Ident,
    ) -> Result<Option<ImportTarget>, ResolveError>;

    /// The `alias` declared under `name` in `alias_module`, if there is one,
    /// gated on `accessor` being allowed to name the alias itself.
    ///
    /// This is the only alias query semantic resolution uses: an alias is
    /// always its own visibility gate, and deciding that from the outside --
    /// by asking whether an alias merely exists -- is what let equivalent
    /// spellings disagree. `bypass_visibility` says the gate was already
    /// passed elsewhere (see [`ItemAccess`]); it never means "no gate".
    /// Alias chains are followed and a cycle is reported rather than
    /// recursed.
    fn resolve_visible_alias(
        &mut self,
        accessor: &[Ident],
        alias_module: &[Ident],
        name: &Ident,
        bypass_visibility: bool,
    ) -> Result<Option<ResolvedAlias>, ResolveError>;

    fn ambient_core_candidates(
        &mut self,
        accessor: &[Ident],
        name: &Ident,
    ) -> Result<Option<Vec<Ident>>, ResolveError>;

    fn import_alias_names(&mut self, module_path: &[Ident]) -> Vec<Ident>;

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

    /// The one function `owner_absolute` declares under `function_name` in
    /// `namespace`, when the owner is generic. A member's signature includes
    /// its receiver as parameter 0, with `Self` already rewritten to the
    /// owner applied to its own generics, so ordinary argument inference can
    /// solve the owner's type arguments from the explicit receiver.
    fn generic_owner_function_signature(
        &mut self,
        owner_absolute: &[Ident],
        function_name: &Ident,
        namespace: crate::resolved_type::FunctionNamespace,
    ) -> Result<Option<GenericOwnerFunctionSignature>, ResolveError>;

    /// The overload candidates `accessor` may choose between when it writes
    /// the name bound by `access`. `Ok(None)` means the name is not an
    /// overload set at all.
    ///
    /// A declared overload alias is gated once, here, and then forwards the
    /// candidate set frozen at its own declaration site unchanged. A direct
    /// (non-alias) name is filtered against `accessor`, since nothing gated
    /// it earlier. Either way the caller receives an already-authorized set
    /// and never re-filters it.
    fn resolve_overload_set(
        &mut self,
        accessor: &[Ident],
        access: &ItemAccess,
    ) -> Result<Option<ResolvedOverloadSet>, ResolveError>;

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
pub struct GenericOwnerFunctionSignature {
    pub owner_generics: Vec<Ident>,
    pub owner_defaults: Vec<Option<Type>>,
    pub function_generics: Vec<Ident>,
    pub params: Vec<Type>,
    pub return_type: Type,
}
