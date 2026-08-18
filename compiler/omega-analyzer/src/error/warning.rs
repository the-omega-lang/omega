
use super::*;

#[derive(Debug, Clone)]
pub struct AnalysisWarning {
    pub node_id: HirId,
    pub span: Span,
    pub kind: AnalysisWarningKind,
}

impl AnalysisWarning {
    pub fn new(node_id: HirId, span: Span, kind: AnalysisWarningKind) -> Self {
        Self { node_id, span, kind }
    }

    pub fn to_diagnostic(&self) -> Diagnostic {
        let d = Diagnostic::warning(self.kind.to_string());
        let d = match &self.kind {
            AnalysisWarningKind::UnreachableCode => d
                .with_label(self.span, "this can never run")
                .with_note(
                    "it follows something that always diverges (`return`, `break`, `continue`, \
                     a `loop` with no way out, or a call to a `never`-returning function)",
                ),
            AnalysisWarningKind::PreferLoop => d
                .with_label(self.span, "this condition is always true")
                .with_help("use `loop { }` instead -- it also lets the compiler prove this always diverges"),
            AnalysisWarningKind::InlineNotEnforced => d
                .with_label(self.span, "this hint is recorded but not acted on")
                .with_note("this backend has no function-inlining support yet"),
            AnalysisWarningKind::UnusedVariable { name } => {
                d.with_label(self.span, format!("`{}` is never read", name.as_ref()))
            }
            AnalysisWarningKind::UnusedParameter { name } => {
                d.with_label(self.span, format!("`{}` is never read", name.as_ref()))
            }
            AnalysisWarningKind::UnnecessaryMut { name } => d
                .with_label(self.span, format!("`{}` is declared `mut` but never reassigned", name.as_ref()))
                .with_help("remove `mut` -- it isn't just a hint, dropping it changes nothing about how this compiles"),
            AnalysisWarningKind::UnnecessaryReveal => d
                .with_label(self.span, "this 'reveal' never suppresses anything")
                .with_help("remove 'reveal' -- every check inside it would already pass without it"),
            AnalysisWarningKind::UnusedImport { alias } => {
                d.with_label(self.span, format!("`{}` is never referenced in this module", alias.as_ref()))
            }
            AnalysisWarningKind::UnusedField { owner, field } => d.with_label(
                self.span,
                format!("`{}` is never read anywhere `{}` is used", field.as_ref(), owner.as_ref()),
            ),
            AnalysisWarningKind::NeverConstructedVariant { r#enum, variant } => d.with_label(
                self.span,
                format!("`{}` is never built anywhere `{}` is used", variant.as_ref(), r#enum.as_ref()),
            ),
            AnalysisWarningKind::UnusedReturnValue => d
                .with_label(self.span, "this call's result is discarded")
                .with_note("if that's intentional, bind it to a local instead of leaving it a bare statement"),
            AnalysisWarningKind::NoOpCast { r#type } => {
                d.with_label(self.span, format!("`{type}` cast to itself changes nothing"))
            }
            AnalysisWarningKind::SelfAssignment => {
                d.with_label(self.span, "this assigns a value to itself")
            }
            AnalysisWarningKind::AlwaysTrueFalseComparison { result } => d.with_label(
                self.span,
                format!("this comparison is always {result}, no matter the other operand's value"),
            ),
            AnalysisWarningKind::RedundantLayoutAnnotation => d
                .with_label(self.span, "these values are already the default")
                .with_help("remove the explicit arguments, or bare `@layout`, to mean the same thing"),
            AnalysisWarningKind::LargeStructByValue { r#type, size } => d
                .with_label(self.span, format!("`{type}` is at least {size} bytes, passed by value"))
                .with_note("this backend passes structs as flattened scalars, not by reference -- consider a pointer instead"),
            AnalysisWarningKind::UnfilledGap { functions, .. } => d
                .with_label(self.span, "no glue implements this gap anywhere in this compilation")
                .with_note(format!(
                    "missing: {}",
                    functions.iter().map(|f| format!("'{}'", f.as_ref())).collect::<Vec<_>>().join(", ")
                ))
                .with_help("this only matters if something actually calls this gap -- an unglued, uncalled gap links fine"),
        };
        if self.kind.is_suppressible() {
            d.with_note(format!("suppress this with '@suppress({})'", self.kind.name()))
        } else {
            d
        }
    }
}

impl fmt::Display for AnalysisWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind)
    }
}

#[derive(Debug, Clone)]
pub enum AnalysisWarningKind {
    UnreachableCode,
    PreferLoop,
    InlineNotEnforced,
    UnusedVariable { name: Ident },
    UnusedParameter { name: Ident },
    UnnecessaryMut { name: Ident },
    UnnecessaryReveal,
    UnusedImport { alias: Ident },
    UnusedField { owner: Ident, field: Ident },
    NeverConstructedVariant { r#enum: Ident, variant: Ident },
    UnusedReturnValue,
    NoOpCast { r#type: ResolvedType },
    SelfAssignment,
    AlwaysTrueFalseComparison { result: bool },
    RedundantLayoutAnnotation,
    LargeStructByValue { r#type: ResolvedType, size: u32 },
    UnfilledGap { gap: Ident, functions: Vec<Ident> },
}

impl AnalysisWarningKind {
    pub fn is_suppressible(&self) -> bool {
        !matches!(self, Self::UnfilledGap { .. })
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::UnreachableCode => "unreachable_code",
            Self::PreferLoop => "prefer_loop",
            Self::InlineNotEnforced => "inline_not_enforced",
            Self::UnusedVariable { .. } => "unused_variable",
            Self::UnusedParameter { .. } => "unused_parameter",
            Self::UnnecessaryMut { .. } => "unnecessary_mut",
            Self::UnnecessaryReveal => "unnecessary_reveal",
            Self::UnusedImport { .. } => "unused_import",
            Self::UnusedField { .. } => "unused_field",
            Self::NeverConstructedVariant { .. } => "never_constructed_variant",
            Self::UnusedReturnValue => "unused_return_value",
            Self::NoOpCast { .. } => "no_op_cast",
            Self::SelfAssignment => "self_assignment",
            Self::AlwaysTrueFalseComparison { .. } => "always_true_false_comparison",
            Self::RedundantLayoutAnnotation => "redundant_layout_annotation",
            Self::LargeStructByValue { .. } => "large_struct_by_value",
            Self::UnfilledGap { .. } => "unfilled_gap",
        }
    }
}

impl fmt::Display for AnalysisWarningKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnreachableCode => write!(f, "unreachable code"),
            Self::PreferLoop => write!(f, "this `while` condition is always true"),
            Self::InlineNotEnforced => write!(f, "'@inline' is not enforced by this backend yet"),
            Self::UnusedVariable { name } => write!(f, "unused variable '{}'", name.as_ref()),
            Self::UnusedParameter { name } => write!(f, "unused parameter '{}'", name.as_ref()),
            Self::UnnecessaryMut { name } => write!(f, "unnecessary 'mut' on '{}'", name.as_ref()),
            Self::UnnecessaryReveal => write!(f, "unnecessary 'reveal'"),
            Self::UnusedImport { alias } => write!(f, "unused import '{}'", alias.as_ref()),
            Self::UnusedField { owner, field } => {
                write!(f, "field '{}' of '{}' is never read", field.as_ref(), owner.as_ref())
            }
            Self::NeverConstructedVariant { r#enum, variant } => {
                write!(f, "variant '{}' of '{}' is never constructed", variant.as_ref(), r#enum.as_ref())
            }
            Self::UnusedReturnValue => write!(f, "unused return value"),
            Self::NoOpCast { r#type } => write!(f, "this cast to '{type}' has no effect"),
            Self::SelfAssignment => write!(f, "self-assignment"),
            Self::AlwaysTrueFalseComparison { result } => write!(f, "comparison is always {result}"),
            Self::RedundantLayoutAnnotation => write!(f, "redundant '@layout' arguments"),
            Self::LargeStructByValue { r#type, .. } => write!(f, "large type '{type}' passed by value"),
            Self::UnfilledGap { gap, .. } => write!(f, "gap '{}' has no glue implementation", gap.as_ref()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omega_diagnostics::Footer;

    #[test]
    fn unfilled_gap_does_not_advertise_impossible_suppression() {
        let warning = AnalysisWarning::new(
            HirId { module: omega_hir::SYNTHETIC_MODULE, local: 0 },
            Span::new(0, 0),
            AnalysisWarningKind::UnfilledGap {
                gap: Ident("Platform".into()),
                functions: vec![Ident("write".into())],
            },
        );

        let diagnostic = warning.to_diagnostic();
        assert!(!diagnostic.footers.iter().any(|footer| {
            matches!(footer, Footer::Note(note) if note.contains("@suppress"))
        }));
    }
}
