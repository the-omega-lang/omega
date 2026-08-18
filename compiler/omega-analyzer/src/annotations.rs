
use crate::analysis::Analyzer;
use crate::error::AnalysisErrorKind;
use crate::error::AnalysisWarningKind;
use crate::resolved_type::ResolvedType;
use omega_hir::{HirAnnotation, HirAnnotationArg, HirAnnotationValue, HirId};
use omega_parser::prelude::{Ident, Span};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Struct,
    Enum,
    Union,
    Function,
    Import,
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

pub fn item_kind_list(kinds: &[ItemKind]) -> String {
    let names: Vec<&str> = kinds.iter().map(|k| k.plural()).collect();
    match names.as_slice() {
        [one] => one.to_string(),
        [one, two] => format!("{one} and {two}"),
        [init @ .., last] => format!("{}, and {last}", init.join(", ")),
        [] => String::new(),
    }
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineMode {
    Always,
    Never,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ManglingMode {
    #[default]
    Enabled,
    Disabled,
    Forced(String),
    Glued { spec_module_path: Vec<Ident>, spec_name: Ident, function_name: Ident },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedAnnotations {
    pub layout: Layout,
    pub inline: Option<InlineMode>,
    pub mangling: ManglingMode,
    pub suppress: Vec<Ident>,
}

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

    if !seen_keys.is_empty() && layout == Layout::default() {
        analyzer.warn(node_id, annotation.span, AnalysisWarningKind::RedundantLayoutAnnotation);
    }

    layout
}

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

pub const LARGE_STRUCT_BY_VALUE_THRESHOLD: u32 = 128;

pub fn estimate_type_size(r#type: &ResolvedType, pointer_bytes: u32) -> u32 {
    if let Some(n) = r#type.primitive_byte_size(pointer_bytes) {
        return n;
    }
    match r#type {
        ResolvedType::Struct(cell) => cell.borrow().fields.iter().map(|field| estimate_type_size(&field.r#type, pointer_bytes)).sum(),
        ResolvedType::Union(cell) => {
            cell.borrow().fields.iter().map(|field| estimate_type_size(&field.r#type, pointer_bytes)).max().unwrap_or(0)
        }
        ResolvedType::Enum { cell, .. } => {
            let cell = cell.borrow();
            let tag = estimate_type_size(&cell.tag_type, pointer_bytes);
            let header: u32 = cell.header.iter().map(|field| estimate_type_size(&field.r#type, pointer_bytes)).sum();
            let dynamic: u32 = cell.dynamic_fields.iter().map(|field| estimate_type_size(&field.r#type, pointer_bytes)).sum();
            let body = cell
                .variants
                .iter()
                .map(|v| v.fields.iter().map(|field| estimate_type_size(&field.r#type, pointer_bytes)).sum::<u32>())
                .max()
                .unwrap_or(0);
            tag + header + dynamic + body
        }
        ResolvedType::SizedArray(item, size) => estimate_type_size(item, pointer_bytes) * size,
        ResolvedType::Slice { .. } | ResolvedType::Str { .. } => 12,
        _ => 0,
    }
}
