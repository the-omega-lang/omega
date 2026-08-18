//! Resolves the raw `@name(args)` lists `omega_hir` carries on struct/enum/
//! union/function nodes into typed, validated values. This is the one
//! place that knows which annotation names exist, which item kinds each is
//! allowed on, and what its arguments mean -- everywhere else only ever
//! sees the resolved `Layout`/`InlineMode`/`ManglingMode`/suppress list,
//! never a raw name string.

use crate::analysis::Analyzer;
use crate::error::AnalysisErrorKind;
use crate::error::AnalysisWarningKind;
use crate::resolved_type::ResolvedType;
use omega_hir::{HirAnnotation, HirAnnotationArg, HirAnnotationValue, HirId};
use omega_parser::prelude::{Ident, Span};
use std::fmt;

/// Which of the four item shapes an annotation is attached to -- keyed on
/// this, not the AST/HIR node's own Rust type, since a struct/enum/union
/// member function and a top-level one are already the same
/// `HirFunctionDef`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Struct,
    Enum,
    Union,
    Function,
    /// An `import` statement -- allows only `@suppress` (needed so
    /// `UnusedImport` has any suppress path at all).
    Import,
    /// A `spec`, which may use `@suppress` like every other declaration.
    Spec,
}

impl ItemKind {
    fn article_name(self) -> &'static str {
        match self {
            Self::Struct => "a struct",
            Self::Enum => "an enum",
            Self::Union => "a union",
            Self::Function => "a function",
            Self::Import => "an import",
            Self::Spec => "a spec",
        }
    }

    fn plural(self) -> &'static str {
        match self {
            Self::Struct => "structs",
            Self::Enum => "enums",
            Self::Union => "unions",
            Self::Function => "functions",
            Self::Import => "imports",
            Self::Spec => "specs",
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
/// each defaulting to `1` (fully-packed) when not given: `pack` is
/// C-style internal field-grouping granularity, `align` is the type's own
/// trailing size/outward embedding alignment. `pack` never affects `align`
/// or vice versa.
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
/// hint was given, distinct from either explicit mode), but the
/// annotation itself defaults to `Always` when written bare (`@inline`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineMode {
    Always,
    Never,
}

/// `@mangling(...)`'s resolved mode -- `Enabled` is the default. Unlike
/// `@inline`/`@layout`, there's no sensible default *mode* for a bare
/// `@mangling`, so it still requires an explicit
/// `enabled`/`disabled`/`force = "..."` argument. Not `Copy` because
/// `Forced` owns its exact symbol string.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ManglingMode {
    #[default]
    Enabled,
    Disabled,
    /// `@mangling(force = "...")` -- skips this compiler's own mangling
    /// scheme and uses the given string as the exact, final linker symbol,
    /// verbatim. Unlike `Disabled`, which is rejected on a method (a bare
    /// method name has no owning-type prefix), `Forced` is allowed there:
    /// the caller supplies a complete name, so there's no collision risk.
    /// Still rejected on a generic function -- every instantiation would
    /// otherwise share the one hardcoded name.
    Forced(String),
    /// A `glue` function, carrying just enough (the gap's own module path
    /// + name, and the function name) for codegen to compute *the exact
    /// same* symbol the gap's own synthesized extern declaration uses --
    /// `omega_analyzer` has no dependency on `omega_mangle`/
    /// `omega_codegen`'s mangling scheme, so it's cheaper to derive once,
    /// lazily, where the algorithm already lives.
    Glued { spec_module_path: Vec<Ident>, spec_name: Ident, function_name: Ident },
}

/// Every annotation's resolved value, regardless of which ones actually
/// apply to the item kind being resolved -- callers only read the field(s)
/// relevant to their own item kind, since `resolve` already rejected any
/// annotation that doesn't belong on `kind`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedAnnotations {
    pub layout: Layout,
    pub inline: Option<InlineMode>,
    pub mangling: ManglingMode,
    /// `@suppress`'s warning names, verbatim -- never validated for
    /// existence here: warnings may be renamed/removed, so an
    /// unrecognized name is silently harmless rather than an error.
    pub suppress: Vec<Ident>,
}

/// Validates `annotations` against what `kind` allows, pushing every
/// problem into `analyzer.errors` and returning a resolved, typed result
/// regardless -- the same keep-collecting-errors style every other
/// analysis pass in this crate follows.
///
/// `analyzer` is needed (not just an error sink) because `@layout`'s
/// `pack`/`align` arguments may be `sizeof<Type>`, which needs real type
/// resolution.
///
/// `is_member_function`/`is_generic` only matter for `ItemKind::Function`
/// -- they gate `@mangling(disabled)`'s two hard restrictions.
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
            analyzer.error(node_id, annotation.span, AnalysisErrorKind::DuplicateAnnotation { name: annotation.name.clone() });
        } else {
            seen.push(name);
        }

        match name {
            "layout" => {
                if !matches!(kind, ItemKind::Struct | ItemKind::Enum) {
                    analyzer.error(
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
                    analyzer.error(
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
                    Err(reason) => analyzer.error(
                        node_id,
                        annotation.span,
                        AnalysisErrorKind::InvalidAnnotationArgs { name: annotation.name.clone(), reason },
                    ),
                }
            }
            "mangling" => {
                if kind != ItemKind::Function {
                    analyzer.error(
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
                        analyzer.error(node_id, annotation.span, AnalysisErrorKind::ManglingDisabledOnMethod)
                    }
                    Ok(ManglingMode::Disabled) if is_generic => {
                        analyzer.error(node_id, annotation.span, AnalysisErrorKind::ManglingDisabledOnGeneric)
                    }
                    Ok(ManglingMode::Forced(_)) if is_generic => {
                        analyzer.error(node_id, annotation.span, AnalysisErrorKind::ManglingForcedOnGeneric)
                    }
                    Ok(mode) => result.mangling = mode,
                    Err(reason) => analyzer.error(
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
                            analyzer.error(
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
            _ => analyzer.error(node_id, annotation.span, AnalysisErrorKind::UnknownAnnotation { name: annotation.name.clone() }),
        }
    }

    result
}

/// `@layout(pack = <value>, align = <value>)` -- either key, in any order,
/// each independently optional (an omitted key keeps `Layout::default`'s
/// `1`). Each value is validated as a power of two here, uniformly,
/// regardless of whether it was written as a plain literal or
/// `sizeof<primitive>`.
fn resolve_layout(analyzer: &mut Analyzer, node_id: HirId, annotation: &HirAnnotation) -> Layout {
    let mut layout = Layout::default();
    let mut seen_keys: Vec<&str> = Vec::new();

    for arg in &annotation.args {
        let HirAnnotationArg::KeyValue(key, value) = arg else {
            analyzer.error(
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
            analyzer.error(
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
            analyzer.error(
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
                analyzer.error(
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
                analyzer.error(
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

    // Bare `@layout`/`@layout()` is the sanctioned shorthand for the
    // default -- only warn when a key was explicitly written and it still
    // landed back on the default anyway.
    if !seen_keys.is_empty() && layout == Layout::default() {
        analyzer.warn(node_id, annotation.span, AnalysisWarningKind::RedundantLayoutAnnotation);
    }

    layout
}

/// A `pack =`/`align =` value: a plain integer literal, or `sizeof<Type>`
/// scoped to a primitive `Type`. Returns `None` when type resolution
/// already failed and pushed its own error -- the caller must not push a
/// second one; `Some(Err(reason))` is for problems local to this value
/// (not-a-power-of-two is checked by the caller, since it's the same check
/// for both value shapes).
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
            Some(match resolved.primitive_byte_size(analyzer.pointer_bytes()) {
                Some(n) => Ok(n),
                None => Err(format!(
                    "'sizeof<{resolved}>' is not supported here -- @layout only supports sizeof of a primitive type"
                )),
            })
        }
        HirAnnotationValue::StrLiteral(_) => Some(Err("expected a plain integer or 'sizeof<Type>', found a string literal".to_string())),
    }
}

/// `always`/`never`, or no argument at all, which defaults to `always` --
/// inlining is what most people reach for this annotation to request.
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
        [HirAnnotationArg::KeyValue(key, HirAnnotationValue::StrLiteral(name))] if key.as_ref() == "force" => {
            if name.is_empty() {
                return Err("'force' needs a non-empty symbol name".to_string());
            }
            Ok(ManglingMode::Forced(name.clone()))
        }
        _ => Err("expected 'enabled', 'disabled', or 'force = \"...\"'".to_string()),
    }
}

/// `LargeStructByValue`'s threshold, in bytes -- a round "bigger than a
/// couple cache lines" default. No CLI flag: not requested yet.
pub const LARGE_STRUCT_BY_VALUE_THRESHOLD: u32 = 128;

/// A deliberately approximate, analyzer-only lower bound on a type's
/// in-memory size -- ignores every `@layout` pack/align padding rule, so
/// it can only ever *underestimate* a real type's size, never
/// overestimate it. Good enough to flag "this is clearly a large struct";
/// the only failure mode traded away is a false negative right at the
/// threshold, never a false positive from ignored padding.
pub fn estimate_type_size(r#type: &ResolvedType, pointer_bytes: u32) -> u32 {
    if let Some(n) = r#type.primitive_byte_size(pointer_bytes) {
        return n;
    }
    match r#type {
        ResolvedType::Struct(cell) => cell.borrow().fields.iter().map(|(_, t, _)| estimate_type_size(t, pointer_bytes)).sum(),
        ResolvedType::Union(cell) => {
            cell.borrow().fields.iter().map(|(_, t, _)| estimate_type_size(t, pointer_bytes)).max().unwrap_or(0)
        }
        ResolvedType::Enum { cell, .. } => {
            let cell = cell.borrow();
            let tag = estimate_type_size(&cell.tag_type, pointer_bytes);
            let header: u32 = cell.header.iter().map(|(_, t, _)| estimate_type_size(t, pointer_bytes)).sum();
            let dynamic: u32 = cell.dynamic_fields.iter().map(|(_, t, _)| estimate_type_size(t, pointer_bytes)).sum();
            let body = cell
                .variants
                .iter()
                .map(|v| v.fields.iter().map(|(_, t, _)| estimate_type_size(t, pointer_bytes)).sum::<u32>())
                .max()
                .unwrap_or(0);
            tag + header + dynamic + body
        }
        // `N` copies of the item type's own size, back to back -- an
        // embedded fixed-size array is inline data, not indirection.
        ResolvedType::SizedArray(item, size) => estimate_type_size(item, pointer_bytes) * size,
        // A fat pointer: a data pointer plus an `i32` length.
        ResolvedType::Slice { .. } | ResolvedType::Str { .. } => 12,
        _ => 0,
    }
}
