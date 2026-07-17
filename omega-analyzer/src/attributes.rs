//! Resolves the raw `@name(args)` lists `omega_hir` carries on struct/enum/
//! union/function nodes (see `HirAttribute`'s doc comment) into typed,
//! validated values. This is the one place that knows which annotation
//! names exist, which item kinds each is allowed on, and what its
//! arguments mean -- everywhere else (codegen, the checked tree) only ever
//! sees the resolved `Packing`/`InlineMode`/`ManglingMode`/suppress list,
//! never a raw name string. Adding a future annotation (e.g. `@ufcs`) is a
//! matter of one more `match` arm here, not new plumbing upstream or
//! downstream.

use crate::error::AnalysisErrorKind;
use omega_hir::{HirAttribute, HirAttributeArg};
use omega_parser::prelude::{Ident, Span};
use std::fmt;

/// Which of the four item shapes an annotation is attached to -- the whole
/// applicability table (`"inline" => Function` only, `"packing" =>
/// Struct`/`Enum`, ...) is keyed on this, not on the AST/HIR node's own
/// Rust type, since a struct/enum/union member function and a top-level
/// one are already the same `HirFunctionDef` (see its doc comment).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Struct,
    Enum,
    Union,
    Function,
}

impl ItemKind {
    fn article_name(self) -> &'static str {
        match self {
            Self::Struct => "a struct",
            Self::Enum => "an enum",
            Self::Union => "a union",
            Self::Function => "a function",
        }
    }

    fn plural(self) -> &'static str {
        match self {
            Self::Struct => "structs",
            Self::Enum => "enums",
            Self::Union => "unions",
            Self::Function => "functions",
        }
    }
}

impl fmt::Display for ItemKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.article_name())
    }
}

/// `'structs'` / `'structs and enums'` / `'structs, enums, and functions'` --
/// for `AnalysisErrorKind::AnnotationNotApplicable`'s note.
pub fn item_kind_list(kinds: &[ItemKind]) -> String {
    let names: Vec<&str> = kinds.iter().map(|k| k.plural()).collect();
    match names.as_slice() {
        [one] => one.to_string(),
        [one, two] => format!("{one} and {two}"),
        [init @ .., last] => format!("{}, and {last}", init.join(", ")),
        [] => String::new(),
    }
}

/// `@packing(...)`'s resolved mode -- see the annotation's own doc comment
/// in the language design. `Packed` is the default (today's implicit,
/// zero-padding layout) whether or not `@packing(packed)` is written
/// explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Packing {
    #[default]
    Packed,
    /// A validated power-of-two byte alignment -- the type's own size is
    /// rounded up to a multiple of this, and this is the alignment a field
    /// of this type imposes on whatever it's embedded in (see
    /// `omega_codegen`'s layout functions).
    Align(u32),
}

/// `@inline(...)`'s resolved mode -- no default: `None` (absence) means no
/// hint was given at all, distinct from either explicit mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineMode {
    Always,
    Never,
}

/// `@mangling(...)`'s resolved mode -- `Enabled` is the default (today's
/// only behavior).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ManglingMode {
    #[default]
    Enabled,
    Disabled,
}

/// Every annotation's resolved value, regardless of which ones actually
/// apply to the item kind being resolved -- callers only ever read the
/// field(s) relevant to their own item kind (a struct/enum reads
/// `packing`, a function reads `inline`/`mangling`), since `resolve`
/// already rejected any annotation that doesn't belong on `kind`.
#[derive(Debug, Clone, Default)]
pub struct ResolvedAttributes {
    pub packing: Packing,
    pub inline: Option<InlineMode>,
    pub mangling: ManglingMode,
    /// `@suppress`'s warning names, verbatim -- never validated for
    /// existence here (see `AnalysisWarningKind::name`'s doc comment):
    /// warnings may be renamed/removed, so an unrecognized name is
    /// silently harmless rather than an error.
    pub suppress: Vec<Ident>,
}

/// Validates `attrs` against what `kind` allows, reporting every problem
/// through `on_error` (span first, matching `AnalysisError::new`'s own
/// `(node_id, span, kind)` order once the caller wraps this with its own
/// `node_id`) and returning a resolved, typed result regardless -- callers
/// keep going and use whatever came out the other side, the same
/// keep-collecting-errors style every other analysis pass in this crate
/// already follows.
///
/// `is_member_function`/`is_generic` only matter for `ItemKind::Function`
/// (ignored otherwise) -- they gate `@mangling(disabled)`'s two hard
/// restrictions (see `AnalysisErrorKind::ManglingDisabledOnMethod`/
/// `ManglingDisabledOnGeneric`'s doc comments).
pub fn resolve(
    attrs: &[HirAttribute],
    kind: ItemKind,
    is_member_function: bool,
    is_generic: bool,
    mut on_error: impl FnMut(Span, AnalysisErrorKind),
) -> ResolvedAttributes {
    let mut result = ResolvedAttributes::default();
    let mut seen: Vec<&str> = Vec::new();

    for attr in attrs {
        let name = attr.name.as_ref();
        if seen.contains(&name) {
            on_error(attr.span, AnalysisErrorKind::DuplicateAnnotation { name: attr.name.clone() });
        } else {
            seen.push(name);
        }

        match name {
            "packing" => {
                if !matches!(kind, ItemKind::Struct | ItemKind::Enum) {
                    on_error(
                        attr.span,
                        AnalysisErrorKind::AnnotationNotApplicable {
                            name: attr.name.clone(),
                            found: kind,
                            allowed: vec![ItemKind::Struct, ItemKind::Enum],
                        },
                    );
                    continue;
                }
                match resolve_packing(attr) {
                    Ok(packing) => result.packing = packing,
                    Err(reason) => {
                        on_error(attr.span, AnalysisErrorKind::InvalidAnnotationArgs { name: attr.name.clone(), reason })
                    }
                }
            }
            "inline" => {
                if kind != ItemKind::Function {
                    on_error(
                        attr.span,
                        AnalysisErrorKind::AnnotationNotApplicable {
                            name: attr.name.clone(),
                            found: kind,
                            allowed: vec![ItemKind::Function],
                        },
                    );
                    continue;
                }
                match resolve_inline(attr) {
                    Ok(mode) => result.inline = Some(mode),
                    Err(reason) => {
                        on_error(attr.span, AnalysisErrorKind::InvalidAnnotationArgs { name: attr.name.clone(), reason })
                    }
                }
            }
            "mangling" => {
                if kind != ItemKind::Function {
                    on_error(
                        attr.span,
                        AnalysisErrorKind::AnnotationNotApplicable {
                            name: attr.name.clone(),
                            found: kind,
                            allowed: vec![ItemKind::Function],
                        },
                    );
                    continue;
                }
                match resolve_mangling(attr) {
                    Ok(ManglingMode::Disabled) if is_member_function => {
                        on_error(attr.span, AnalysisErrorKind::ManglingDisabledOnMethod)
                    }
                    Ok(ManglingMode::Disabled) if is_generic => {
                        on_error(attr.span, AnalysisErrorKind::ManglingDisabledOnGeneric)
                    }
                    Ok(mode) => result.mangling = mode,
                    Err(reason) => {
                        on_error(attr.span, AnalysisErrorKind::InvalidAnnotationArgs { name: attr.name.clone(), reason })
                    }
                }
            }
            "suppress" => {
                result.suppress = attr
                    .args
                    .iter()
                    .filter_map(|arg| match arg {
                        HirAttributeArg::Ident(warning) => Some(warning.clone()),
                        HirAttributeArg::KeyValue(key, _) => {
                            on_error(
                                attr.span,
                                AnalysisErrorKind::InvalidAnnotationArgs {
                                    name: attr.name.clone(),
                                    reason: format!(
                                        "'{}' should be a bare warning name, not a key = value pair",
                                        key.as_ref()
                                    ),
                                },
                            );
                            None
                        }
                    })
                    .collect();
            }
            _ => on_error(attr.span, AnalysisErrorKind::UnknownAnnotation { name: attr.name.clone() }),
        }
    }

    result
}

/// `packed` or `align = N` (`N` a power-of-two decimal literal) -- anything
/// else is a malformed argument, reported through the shared
/// `InvalidAnnotationArgs` error rather than a dedicated variant per shape.
fn resolve_packing(attr: &HirAttribute) -> Result<Packing, String> {
    match attr.args.as_slice() {
        [HirAttributeArg::Ident(mode)] if mode.as_ref() == "packed" => Ok(Packing::Packed),
        [HirAttributeArg::KeyValue(key, value)] if key.as_ref() == "align" => {
            let n: u32 = value.parse().map_err(|_| format!("'{value}' does not fit a u32"))?;
            if n == 0 || !n.is_power_of_two() {
                return Err(format!("alignment must be a power of two, found {n}"));
            }
            Ok(Packing::Align(n))
        }
        _ => Err("expected 'packed' or 'align = N'".to_string()),
    }
}

fn resolve_inline(attr: &HirAttribute) -> Result<InlineMode, String> {
    match attr.args.as_slice() {
        [HirAttributeArg::Ident(mode)] if mode.as_ref() == "always" => Ok(InlineMode::Always),
        [HirAttributeArg::Ident(mode)] if mode.as_ref() == "never" => Ok(InlineMode::Never),
        _ => Err("expected 'always' or 'never'".to_string()),
    }
}

fn resolve_mangling(attr: &HirAttribute) -> Result<ManglingMode, String> {
    match attr.args.as_slice() {
        [HirAttributeArg::Ident(mode)] if mode.as_ref() == "enabled" => Ok(ManglingMode::Enabled),
        [HirAttributeArg::Ident(mode)] if mode.as_ref() == "disabled" => Ok(ManglingMode::Disabled),
        _ => Err("expected 'enabled' or 'disabled'".to_string()),
    }
}
