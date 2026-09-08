use crate::analysis::Analyzer;
use crate::error::AnalysisErrorKind;
use crate::error::AnalysisWarningKind;
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
    ForeignFunction,
    ForeignBinding,
    Global,
}

/// The item kinds `@suppress` scopes a warning over. A global binding has no
/// body or members to scope one across, so it is deliberately absent.
const SUPPRESS_TARGETS: [ItemKind; 8] = [
    ItemKind::Struct,
    ItemKind::Enum,
    ItemKind::Union,
    ItemKind::Function,
    ItemKind::Import,
    ItemKind::Spec,
    ItemKind::ForeignFunction,
    ItemKind::ForeignBinding,
];

impl ItemKind {
    fn article_name(self) -> &'static str {
        match self {
            Self::Struct => "a struct",
            Self::Enum => "an enum",
            Self::Union => "a union",
            Self::Function => "a function",
            Self::Import => "an import",
            Self::Spec => "a spec",
            Self::ForeignFunction => "a foreign function",
            Self::ForeignBinding => "a foreign binding",
            Self::Global => "a global binding",
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
            Self::ForeignFunction => "foreign functions",
            Self::ForeignBinding => "foreign bindings",
            Self::Global => "global bindings",
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
    Glued {
        spec_module_path: Vec<Ident>,
        spec_name: Ident,
        function_name: Ident,
    },
}

/// Binary visibility of a symbol in the linked image. This is independent of
/// Omega's source visibility: `Hidden` still links across source files and
/// separately compiled objects of one image, it only stops the symbol from
/// being exported out of a shared image. An Omega-declared item is `Hidden`
/// unless it asks otherwise; a `foreign` item is not, because naming an
/// external symbol is the whole point of declaring one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SymbolVisibility {
    #[default]
    Hidden,
    Default,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SymbolPolicy {
    pub mangling: ManglingMode,
    pub visibility: SymbolVisibility,
}

impl SymbolPolicy {
    /// The policy an ordinary Omega item has without `@symbol(...)`.
    pub fn ordinary() -> Self {
        Self {
            mangling: ManglingMode::Enabled,
            visibility: SymbolVisibility::Hidden,
        }
    }

    /// The policy a foreign item has without `@symbol(...)`. `foreign` is
    /// already the declaration that a symbol is looked up in, or defined for,
    /// something outside this compilation, so it forks both defaults: the
    /// exact name written in source, and a symbol that crosses images.
    pub fn foreign() -> Self {
        Self {
            mangling: ManglingMode::Disabled,
            visibility: SymbolVisibility::Default,
        }
    }

    pub fn mangled(mangling: ManglingMode) -> Self {
        Self {
            mangling,
            visibility: SymbolVisibility::Hidden,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedAnnotations {
    pub layout: Layout,
    pub inline: Option<InlineMode>,
    pub symbol: SymbolPolicy,
    pub suppress: Vec<Ident>,
    pub naked: bool,
}

/// Resolves an item's annotations. `default_symbol` supplies the policy that
/// applies when no `@symbol(...)` annotation is written -- `SymbolPolicy::ordinary`
/// for ordinary items, `SymbolPolicy::foreign` for foreign ones -- so the caller
/// decides the default deliberately instead of this function silently picking
/// `SymbolPolicy`'s own `#[default]`.
pub fn resolve(
    analyzer: &mut Analyzer,
    node_id: HirId,
    annotations: &[HirAnnotation],
    kind: ItemKind,
    is_member_function: bool,
    is_generic: bool,
    default_symbol: SymbolPolicy,
) -> ResolvedAnnotations {
    let mut result = ResolvedAnnotations {
        symbol: default_symbol.clone(),
        ..ResolvedAnnotations::default()
    };
    let mut seen: Vec<&str> = Vec::new();
    let mut inline_span: Option<Span> = None;
    let mut naked_span: Option<Span> = None;

    for annotation in annotations {
        let name = annotation.name.as_ref();
        if seen.contains(&name) {
            analyzer.error(
                node_id,
                annotation.span,
                AnalysisErrorKind::DuplicateAnnotation {
                    name: annotation.name.clone(),
                },
            );
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
                    Ok(mode) => {
                        result.inline = Some(mode);
                        inline_span = Some(annotation.span);
                    }
                    Err(reason) => analyzer.error(
                        node_id,
                        annotation.span,
                        AnalysisErrorKind::InvalidAnnotationArgs {
                            name: annotation.name.clone(),
                            reason,
                        },
                    ),
                }
            }
            "naked" => {
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
                if !annotation.args.is_empty() {
                    analyzer.error(
                        node_id,
                        annotation.span,
                        AnalysisErrorKind::InvalidAnnotationArgs {
                            name: annotation.name.clone(),
                            reason: "'@naked' takes no arguments".to_string(),
                        },
                    );
                    continue;
                }
                result.naked = true;
                naked_span = Some(annotation.span);
            }
            "symbol" => {
                if !matches!(
                    kind,
                    ItemKind::Function
                        | ItemKind::ForeignFunction
                        | ItemKind::ForeignBinding
                        | ItemKind::Global
                ) {
                    analyzer.error(
                        node_id,
                        annotation.span,
                        AnalysisErrorKind::AnnotationNotApplicable {
                            name: annotation.name.clone(),
                            found: kind,
                            allowed: vec![
                                ItemKind::Function,
                                ItemKind::ForeignFunction,
                                ItemKind::ForeignBinding,
                                ItemKind::Global,
                            ],
                        },
                    );
                    continue;
                }
                match resolve_symbol(annotation, &default_symbol) {
                    Ok(policy) => match policy.mangling {
                        ManglingMode::Disabled if is_member_function => analyzer.error(
                            node_id,
                            annotation.span,
                            AnalysisErrorKind::ManglingDisabledOnMethod,
                        ),
                        ManglingMode::Disabled if is_generic => analyzer.error(
                            node_id,
                            annotation.span,
                            AnalysisErrorKind::ManglingDisabledOnGeneric,
                        ),
                        ManglingMode::Forced(_) if is_generic => analyzer.error(
                            node_id,
                            annotation.span,
                            AnalysisErrorKind::ManglingForcedOnGeneric,
                        ),
                        _ => result.symbol = policy,
                    },
                    Err(reason) => analyzer.error(
                        node_id,
                        annotation.span,
                        AnalysisErrorKind::InvalidAnnotationArgs {
                            name: annotation.name.clone(),
                            reason,
                        },
                    ),
                }
            }
            "suppress" => {
                if !SUPPRESS_TARGETS.contains(&kind) {
                    analyzer.error(
                        node_id,
                        annotation.span,
                        AnalysisErrorKind::AnnotationNotApplicable {
                            name: annotation.name.clone(),
                            found: kind,
                            allowed: SUPPRESS_TARGETS.to_vec(),
                        },
                    );
                    continue;
                }
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
            _ => analyzer.error(
                node_id,
                annotation.span,
                AnalysisErrorKind::UnknownAnnotation {
                    name: annotation.name.clone(),
                },
            ),
        }
    }

    if let (Some(_), Some(naked_span)) = (inline_span, naked_span) {
        analyzer.error(node_id, naked_span, AnalysisErrorKind::NakedInlineConflict);
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
                    reason: format!(
                        "unknown @layout argument '{}' -- expected 'pack' or 'align'",
                        key.as_ref()
                    ),
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

        let Some(resolved) = resolve_size_value(analyzer, node_id, annotation.span, value) else {
            continue;
        };
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
                    AnalysisErrorKind::InvalidAnnotationArgs {
                        name: annotation.name.clone(),
                        reason,
                    },
                );
                continue;
            }
        };
        match key.as_ref() {
            "pack" => layout.pack = value,
            "align" => {
                // An alignment is an address requirement, so `alignof` and
                // every rounding step must be able to hold it in the
                // target's `usize`. A 16-bit target therefore caps it well
                // below the u32 the annotation grammar accepts.
                let pointer_bytes = analyzer.pointer_bytes();
                if !fits_target_usize(value, pointer_bytes) {
                    analyzer.error(
                        node_id,
                        annotation.span,
                        AnalysisErrorKind::InvalidAnnotationArgs {
                            name: annotation.name.clone(),
                            reason: format!(
                                "'align' must fit this target's usize ({pointer_bytes} bytes), found {value}"
                            ),
                        },
                    );
                    continue;
                }
                layout.align = value;
            }
            _ => unreachable!("checked above"),
        }
    }

    if !seen_keys.is_empty() && layout == Layout::default() {
        analyzer.warn(
            node_id,
            annotation.span,
            AnalysisWarningKind::RedundantLayoutAnnotation,
        );
    }

    layout
}

fn fits_target_usize(value: u32, pointer_bytes: u32) -> bool {
    pointer_bytes >= 4 || u64::from(value) < (1u64 << (pointer_bytes * 8))
}

fn resolve_size_value(
    analyzer: &mut Analyzer,
    node_id: HirId,
    span: Span,
    value: &HirAnnotationValue,
) -> Option<Result<u32, String>> {
    match value {
        HirAnnotationValue::IntLiteral(s) => Some(
            s.parse::<u32>()
                .map_err(|_| format!("'{s}' does not fit a u32")),
        ),
        HirAnnotationValue::Sizeof(ty) => {
            let resolved = analyzer.resolve_type_or_error(node_id, span, ty, false)?;
            Some(
                match resolved.primitive_byte_size(analyzer.pointer_bytes()) {
                    Some(n) => Ok(n),
                    None => Err(format!(
                        "'sizeof<{resolved}>' is not supported here -- @layout only supports sizeof of a primitive type"
                    )),
                },
            )
        }
        HirAnnotationValue::StrLiteral(_) => Some(Err(
            "expected a plain integer or 'sizeof<Type>', found a string literal".to_string(),
        )),
        HirAnnotationValue::Ident(_) => Some(Err(
            "expected a plain integer or 'sizeof<Type>', found an identifier".to_string(),
        )),
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

fn resolve_symbol(
    annotation: &HirAnnotation,
    default: &SymbolPolicy,
) -> Result<SymbolPolicy, String> {
    if annotation.args.is_empty() {
        return Err(
            "expected at least one of 'mangle = enabled|disabled', 'name = \"...\"', or 'export'"
                .to_string(),
        );
    }

    let mut mangle: Option<ManglingMode> = None;
    let mut name: Option<String> = None;
    let mut visibility: Option<SymbolVisibility> = None;
    let mut seen_keys: Vec<&str> = Vec::new();

    for arg in &annotation.args {
        let key = match arg {
            HirAnnotationArg::Ident(key) => key,
            HirAnnotationArg::KeyValue(key, _) => key,
        };
        if seen_keys.contains(&key.as_ref()) {
            return Err(format!("'{}' is already set", key.as_ref()));
        }
        seen_keys.push(key.as_ref());

        match arg {
            // Only `export` carries meaning on its own; a bare `mangle` or a
            // bare mode identifier would silently pick a policy the source
            // never states.
            HirAnnotationArg::Ident(key) if key.as_ref() == "export" => {
                visibility = Some(SymbolVisibility::Default);
            }
            HirAnnotationArg::Ident(key) => {
                return Err(format!(
                    "'{}' needs a value -- only 'export' can be written on its own",
                    key.as_ref()
                ));
            }
            HirAnnotationArg::KeyValue(key, value) => match key.as_ref() {
                "mangle" => mangle = Some(mangling_mode(value)?),
                "name" => name = Some(symbol_name(value)?),
                "export" => visibility = Some(export_mode(value)?),
                other => {
                    return Err(format!(
                        "unknown @symbol argument '{other}' -- expected 'mangle', 'name', or 'export'"
                    ));
                }
            },
        }
    }

    let mangling = match (name, mangle) {
        (Some(_), Some(_)) => {
            return Err(
                "'name' already decides the exact symbol, so it cannot be combined with 'mangle'"
                    .to_string(),
            );
        }
        (Some(name), None) => ManglingMode::Forced(name),
        (None, Some(mode)) => mode,
        (None, None) => default.mangling.clone(),
    };
    Ok(SymbolPolicy {
        mangling,
        visibility: visibility.unwrap_or(default.visibility),
    })
}

fn mangling_mode(value: &HirAnnotationValue) -> Result<ManglingMode, String> {
    match value {
        HirAnnotationValue::Ident(mode) if mode.as_ref() == "enabled" => Ok(ManglingMode::Enabled),
        HirAnnotationValue::Ident(mode) if mode.as_ref() == "disabled" => {
            Ok(ManglingMode::Disabled)
        }
        _ => Err("'mangle' expects 'enabled' or 'disabled'".to_string()),
    }
}

fn export_mode(value: &HirAnnotationValue) -> Result<SymbolVisibility, String> {
    match value {
        HirAnnotationValue::Ident(mode) if mode.as_ref() == "enabled" => {
            Ok(SymbolVisibility::Default)
        }
        HirAnnotationValue::Ident(mode) if mode.as_ref() == "disabled" => {
            Ok(SymbolVisibility::Hidden)
        }
        _ => Err("'export' expects 'enabled' or 'disabled'".to_string()),
    }
}

fn symbol_name(value: &HirAnnotationValue) -> Result<String, String> {
    let HirAnnotationValue::StrLiteral(name) = value else {
        return Err("'name' expects a string literal".to_string());
    };
    if name.is_empty() {
        return Err("'name' needs a non-empty symbol name".to_string());
    }
    if name.contains('\0') {
        return Err("'name' cannot contain a NUL byte".to_string());
    }
    Ok(name.clone())
}

pub const LARGE_STRUCT_BY_VALUE_THRESHOLD: u32 = 128;

#[cfg(test)]
mod tests;
