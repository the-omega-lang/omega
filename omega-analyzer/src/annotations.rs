//! Resolves the raw `@name(args)` lists `omega_hir` carries on struct/enum/
//! union/function nodes (see `HirAnnotation`'s doc comment) into typed,
//! validated values. This is the one place that knows which annotation
//! names exist, which item kinds each is allowed on, and what its
//! arguments mean -- everywhere else (codegen, the checked tree) only ever
//! sees the resolved `Layout`/`InlineMode`/`ManglingMode`/suppress list,
//! never a raw name string. Adding a future annotation (e.g. `@ufcs`) is a
//! matter of one more `match` arm here, not new plumbing upstream or
//! downstream.

use crate::analysis::Analyzer;
use crate::error::AnalysisError;
use crate::error::AnalysisErrorKind;
use crate::error::AnalysisWarningKind;
use crate::resolved_type::ResolvedType;
use omega_hir::{HirAnnotation, HirAnnotationArg, HirAnnotationValue, HirId};
use omega_parser::prelude::{Ident, Span};
use std::fmt;

/// Which of the four item shapes an annotation is attached to -- the whole
/// applicability table (`"inline" => Function` only, `"layout" =>
/// Struct`/`Enum`, ...) is keyed on this, not on the AST/HIR node's own
/// Rust type, since a struct/enum/union member function and a top-level
/// one are already the same `HirFunctionDef` (see its doc comment).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Struct,
    Enum,
    Union,
    Function,
    /// An `import` statement -- unlike the other three, this one allows
    /// only `@suppress` (needed so `UnusedImport` has any suppress path at
    /// all); every other annotation stays rejected on it via the ordinary
    /// `AnnotationNotApplicable` path, no new error-handling code needed.
    Import,
}

impl ItemKind {
    fn article_name(self) -> &'static str {
        match self {
            Self::Struct => "a struct",
            Self::Enum => "an enum",
            Self::Union => "a union",
            Self::Function => "a function",
            Self::Import => "an import",
        }
    }

    fn plural(self) -> &'static str {
        match self {
            Self::Struct => "structs",
            Self::Enum => "enums",
            Self::Union => "unions",
            Self::Function => "functions",
            Self::Import => "imports",
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

/// `@layout(...)`'s resolved shape -- two independent, orthogonal knobs,
/// each defaulting to `1` (today's implicit fully-packed behavior) when not
/// given: `pack` is C-style internal field-grouping granularity (see
/// `omega_codegen`'s `place_field`), `align` is the type's own trailing
/// size/outward embedding alignment (unchanged from the annotation's
/// original `@layout(align = n)` shape). `pack` never affects `align` or
/// vice versa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub pack: u32,
    pub align: u32,
}

impl Default for Layout {
    fn default() -> Self {
        Self { pack: 1, align: 1 }
    }
}

/// `@inline(...)`'s resolved mode -- no default *field* (`None` means no
/// hint was given at all, distinct from either explicit mode), but the
/// annotation itself defaults to `Always` when written bare (`@inline`) or
/// with empty parens (`@inline()`) -- see `resolve_inline`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineMode {
    Always,
    Never,
}

/// `@mangling(...)`'s resolved mode -- `Enabled` is the default (today's
/// only behavior). Unlike `@inline`/`@layout`, there's no sensible default
/// *mode* for a bare `@mangling` to mean, so it still requires an explicit
/// `enabled`/`disabled` argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ManglingMode {
    #[default]
    Enabled,
    Disabled,
}

/// Every annotation's resolved value, regardless of which ones actually
/// apply to the item kind being resolved -- callers only ever read the
/// field(s) relevant to their own item kind (a struct/enum reads `layout`,
/// a function reads `inline`/`mangling`), since `resolve` already rejected
/// any annotation that doesn't belong on `kind`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedAnnotations {
    pub layout: Layout,
    pub inline: Option<InlineMode>,
    pub mangling: ManglingMode,
    /// `@suppress`'s warning names, verbatim -- never validated for
    /// existence here (see `AnalysisWarningKind::name`'s doc comment):
    /// warnings may be renamed/removed, so an unrecognized name is
    /// silently harmless rather than an error.
    pub suppress: Vec<Ident>,
}

/// Validates `annotations` against what `kind` allows, pushing every
/// problem into `analyzer.errors` (anchored at `node_id` and each
/// annotation's own span) and returning a resolved, typed result
/// regardless -- callers keep going and use whatever came out the other
/// side, the same keep-collecting-errors style every other analysis pass
/// in this crate already follows.
///
/// `analyzer` is needed (not just an error sink) because `@layout`'s
/// `pack`/`align` arguments may be `sizeof<Type>` (see `resolve_size_value`),
/// which needs real type resolution, not just argument-shape parsing.
///
/// `is_member_function`/`is_generic` only matter for `ItemKind::Function`
/// (ignored otherwise) -- they gate `@mangling(disabled)`'s two hard
/// restrictions (see `AnalysisErrorKind::ManglingDisabledOnMethod`/
/// `ManglingDisabledOnGeneric`'s doc comments).
pub fn resolve(
    analyzer: &mut Analyzer,
    node_id: HirId,
    annotations: &[HirAnnotation],
    kind: ItemKind,
    is_member_function: bool,
    is_generic: bool,
) -> ResolvedAnnotations {
    let mut result = ResolvedAnnotations::default();
    let mut seen: Vec<&str> = Vec::new();

    for annotation in annotations {
        let name = annotation.name.as_ref();
        if seen.contains(&name) {
            push_error(analyzer, node_id, annotation.span, AnalysisErrorKind::DuplicateAnnotation { name: annotation.name.clone() });
        } else {
            seen.push(name);
        }

        match name {
            "layout" => {
                if !matches!(kind, ItemKind::Struct | ItemKind::Enum) {
                    push_error(
                        analyzer,
                        node_id,
                        annotation.span,
                        AnalysisErrorKind::AnnotationNotApplicable {
                            name: annotation.name.clone(),
                            found: kind,
                            allowed: vec![ItemKind::Struct, ItemKind::Enum],
                        },
                    );
                    continue;
                }
                result.layout = resolve_layout(analyzer, node_id, annotation);
            }
            "inline" => {
                if kind != ItemKind::Function {
                    push_error(
                        analyzer,
                        node_id,
                        annotation.span,
                        AnalysisErrorKind::AnnotationNotApplicable {
                            name: annotation.name.clone(),
                            found: kind,
                            allowed: vec![ItemKind::Function],
                        },
                    );
                    continue;
                }
                match resolve_inline(annotation) {
                    Ok(mode) => result.inline = Some(mode),
                    Err(reason) => push_error(
                        analyzer,
                        node_id,
                        annotation.span,
                        AnalysisErrorKind::InvalidAnnotationArgs { name: annotation.name.clone(), reason },
                    ),
                }
            }
            "mangling" => {
                if kind != ItemKind::Function {
                    push_error(
                        analyzer,
                        node_id,
                        annotation.span,
                        AnalysisErrorKind::AnnotationNotApplicable {
                            name: annotation.name.clone(),
                            found: kind,
                            allowed: vec![ItemKind::Function],
                        },
                    );
                    continue;
                }
                match resolve_mangling(annotation) {
                    Ok(ManglingMode::Disabled) if is_member_function => {
                        push_error(analyzer, node_id, annotation.span, AnalysisErrorKind::ManglingDisabledOnMethod)
                    }
                    Ok(ManglingMode::Disabled) if is_generic => {
                        push_error(analyzer, node_id, annotation.span, AnalysisErrorKind::ManglingDisabledOnGeneric)
                    }
                    Ok(mode) => result.mangling = mode,
                    Err(reason) => push_error(
                        analyzer,
                        node_id,
                        annotation.span,
                        AnalysisErrorKind::InvalidAnnotationArgs { name: annotation.name.clone(), reason },
                    ),
                }
            }
            "suppress" => {
                result.suppress = annotation
                    .args
                    .iter()
                    .filter_map(|arg| match arg {
                        HirAnnotationArg::Ident(warning) => Some(warning.clone()),
                        HirAnnotationArg::KeyValue(key, _) => {
                            push_error(
                                analyzer,
                                node_id,
                                annotation.span,
                                AnalysisErrorKind::InvalidAnnotationArgs {
                                    name: annotation.name.clone(),
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
            _ => push_error(analyzer, node_id, annotation.span, AnalysisErrorKind::UnknownAnnotation { name: annotation.name.clone() }),
        }
    }

    result
}

fn push_error(analyzer: &mut Analyzer, node_id: HirId, span: Span, kind: AnalysisErrorKind) {
    analyzer.errors.push(AnalysisError::new(node_id, span, kind));
}

/// `@layout(pack = <value>, align = <value>)` -- either key, in any order,
/// each independently optional (an omitted key keeps `Layout::default`'s
/// `1`, and bare `@layout`/`@layout()` -- an empty argument list -- keeps
/// both). Each value is resolved via `resolve_size_value` and validated as
/// a power of two here, uniformly, regardless of whether it was written as
/// a plain literal or `sizeof<primitive>`.
fn resolve_layout(analyzer: &mut Analyzer, node_id: HirId, annotation: &HirAnnotation) -> Layout {
    let mut layout = Layout::default();
    let mut seen_keys: Vec<&str> = Vec::new();

    for arg in &annotation.args {
        let HirAnnotationArg::KeyValue(key, value) = arg else {
            push_error(
                analyzer,
                node_id,
                annotation.span,
                AnalysisErrorKind::InvalidAnnotationArgs {
                    name: annotation.name.clone(),
                    reason: "expected 'pack = <value>' or 'align = <value>'".to_string(),
                },
            );
            continue;
        };
        if !matches!(key.as_ref(), "pack" | "align") {
            push_error(
                analyzer,
                node_id,
                annotation.span,
                AnalysisErrorKind::InvalidAnnotationArgs {
                    name: annotation.name.clone(),
                    reason: format!("unknown @layout argument '{}' -- expected 'pack' or 'align'", key.as_ref()),
                },
            );
            continue;
        }
        if seen_keys.contains(&key.as_ref()) {
            push_error(
                analyzer,
                node_id,
                annotation.span,
                AnalysisErrorKind::InvalidAnnotationArgs {
                    name: annotation.name.clone(),
                    reason: format!("'{}' is already set", key.as_ref()),
                },
            );
            continue;
        }
        seen_keys.push(key.as_ref());

        let Some(resolved) = resolve_size_value(analyzer, node_id, annotation.span, value) else { continue };
        let value = match resolved {
            Ok(n) if n == 0 || !n.is_power_of_two() => {
                push_error(
                    analyzer,
                    node_id,
                    annotation.span,
                    AnalysisErrorKind::InvalidAnnotationArgs {
                        name: annotation.name.clone(),
                        reason: format!("'{}' must be a power of two, found {n}", key.as_ref()),
                    },
                );
                continue;
            }
            Ok(n) => n,
            Err(reason) => {
                push_error(
                    analyzer,
                    node_id,
                    annotation.span,
                    AnalysisErrorKind::InvalidAnnotationArgs { name: annotation.name.clone(), reason },
                );
                continue;
            }
        };
        match key.as_ref() {
            "pack" => layout.pack = value,
            "align" => layout.align = value,
            _ => unreachable!("checked above"),
        }
    }

    // Bare `@layout`/`@layout()` (no keys at all) is the sanctioned
    // shorthand for the default -- only warn when at least one key was
    // explicitly written and it still landed back on the default anyway.
    if !seen_keys.is_empty() && layout == Layout::default() {
        analyzer.warn(node_id, annotation.span, AnalysisWarningKind::RedundantLayoutAnnotation);
    }

    layout
}

/// A `pack =`/`align =` value: a plain integer literal, or `sizeof<Type>`
/// scoped to a primitive `Type` (see `ResolvedType::primitive_byte_size`'s
/// doc comment for why). Returns `None` when type resolution itself already
/// failed and pushed its own error (`Analyzer::resolve_type_or_error`'s own
/// contract) -- the caller must not push a second, redundant error in that
/// case; `Some(Err(reason))` is for problems genuinely local to this value
/// (not a power of two is checked by the caller, not here, since it's the
/// same check for both value shapes).
fn resolve_size_value(
    analyzer: &mut Analyzer,
    node_id: HirId,
    span: Span,
    value: &HirAnnotationValue,
) -> Option<Result<u32, String>> {
    match value {
        HirAnnotationValue::IntLiteral(s) => Some(s.parse::<u32>().map_err(|_| format!("'{s}' does not fit a u32"))),
        HirAnnotationValue::Sizeof(ty) => {
            let resolved = analyzer.resolve_type_or_error(node_id, span, ty, false)?;
            Some(match resolved.primitive_byte_size() {
                Some(n) => Ok(n),
                None => Err(format!(
                    "'sizeof<{resolved}>' is not supported here -- @layout only supports sizeof of a primitive type"
                )),
            })
        }
    }
}

/// `always`/`never`, or no argument at all (`@inline`/`@inline()`), which
/// defaults to `always` -- inlining is what most people reach for this
/// annotation to request in the first place.
fn resolve_inline(annotation: &HirAnnotation) -> Result<InlineMode, String> {
    match annotation.args.as_slice() {
        [] => Ok(InlineMode::Always),
        [HirAnnotationArg::Ident(mode)] if mode.as_ref() == "always" => Ok(InlineMode::Always),
        [HirAnnotationArg::Ident(mode)] if mode.as_ref() == "never" => Ok(InlineMode::Never),
        _ => Err("expected 'always' or 'never'".to_string()),
    }
}

fn resolve_mangling(annotation: &HirAnnotation) -> Result<ManglingMode, String> {
    match annotation.args.as_slice() {
        [HirAnnotationArg::Ident(mode)] if mode.as_ref() == "enabled" => Ok(ManglingMode::Enabled),
        [HirAnnotationArg::Ident(mode)] if mode.as_ref() == "disabled" => Ok(ManglingMode::Disabled),
        _ => Err("expected 'enabled' or 'disabled'".to_string()),
    }
}

/// `LargeStructByValue`'s threshold, in bytes -- a round "bigger than a
/// couple cache lines" default. No CLI flag: not asked for, and one more
/// knob nobody's requested yet isn't worth the surface.
pub const LARGE_STRUCT_BY_VALUE_THRESHOLD: u32 = 128;

/// A deliberately approximate, analyzer-only lower bound on a type's
/// in-memory size -- unlike `omega_codegen`'s real layout algorithm, this
/// ignores every `@layout` pack/align padding rule entirely, so it can only
/// ever *underestimate* a real type's size, never overestimate it: a
/// padded real struct can only be *larger* than this estimate. Good enough
/// to flag "this is clearly a large struct" (`LargeStructByValue`'s only
/// job); the only failure mode this trades away is a false negative right
/// at the threshold, never a false positive from ignored padding.
pub fn estimate_type_size(r#type: &ResolvedType) -> u32 {
    if let Some(n) = r#type.primitive_byte_size() {
        return n;
    }
    match r#type {
        ResolvedType::Struct(cell) => cell.borrow().fields.iter().map(|(_, t)| estimate_type_size(t)).sum(),
        ResolvedType::Union(cell) => {
            cell.borrow().fields.iter().map(|(_, t)| estimate_type_size(t)).max().unwrap_or(0)
        }
        ResolvedType::Enum { cell, .. } => {
            let cell = cell.borrow();
            let tag = estimate_type_size(&cell.tag_type);
            let header: u32 = cell.header.iter().map(|(_, t)| estimate_type_size(t)).sum();
            let dynamic: u32 = cell.dynamic_fields.iter().map(|(_, t)| estimate_type_size(t)).sum();
            let body = cell
                .variants
                .iter()
                .map(|v| v.fields.iter().map(|(_, t)| estimate_type_size(t)).sum::<u32>())
                .max()
                .unwrap_or(0);
            tag + header + dynamic + body
        }
        // `N` copies of the item type's own size, back to back -- an
        // embedded fixed-size array is inline data, not indirection (see
        // `omega_codegen`'s identical `SizedArray` leaf-flattening).
        ResolvedType::SizedArray(item, size) => estimate_type_size(item) * size,
        // A fat pointer: a data pointer plus an `i32` length (see
        // `omega_codegen`'s `IntoIRType` for `Slice`).
        ResolvedType::Slice { .. } => 12,
        _ => 0,
    }
}
