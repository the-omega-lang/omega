mod kind;
pub(crate) mod render;
mod warning;

pub use kind::AnalysisErrorKind;
pub use render::resolve_error_diagnostic;
pub use warning::{AnalysisWarning, AnalysisWarningKind, WarningPolicy};

use crate::resolved_type::{
    CallingConvention, FunctionNamespace, NumericKind, ResolvedFunctionType, ResolvedType,
};
use crate::resolver::ResolveError;
use crate::target::Target;
use omega_diagnostics::{Diagnostic, LabelStyle, SourceId, SourceSpan};
use omega_hir::HirId;
use omega_parser::prelude::{BinaryOp, Ident, Span};
use std::fmt;

fn join(path: &[Ident]) -> String {
    path.iter()
        .map(|i| i.as_ref())
        .collect::<Vec<_>>()
        .join("::")
}

pub fn raw_type_display(ty: &omega_parser::prelude::Type) -> String {
    use omega_parser::prelude::Type;
    match ty {
        Type::Named(path) => join(&path.segments()),
        Type::Generic(path, args) => {
            let args: Vec<String> = args.iter().map(raw_type_display).collect();
            format!("{}<{}>", join(&path.segments()), args.join(", "))
        }
        Type::Pointer(inner, mutable) => {
            format!(
                "*{}{}",
                if *mutable { "mut " } else { "" },
                raw_type_display(inner)
            )
        }
        Type::UnknownSizeArray(inner) => format!("*[?]{}", raw_type_display(inner)),
        Type::InferredArray(inner) => format!("[]{}", raw_type_display(inner)),
        Type::SizedArray(inner, size) => format!("[{}]{}", size, raw_type_display(inner)),
        Type::Function(f) => {
            let params: Vec<String> = f
                .params
                .iter()
                .map(|p| match &p.name {
                    Some(name) => format!("{name}: {}", raw_type_display(&p.r#type)),
                    None => raw_type_display(&p.r#type),
                })
                .collect();
            format!(
                "({}) => {}",
                params.join(", "),
                raw_type_display(&f.return_type)
            )
        }
        Type::SpecStatic(members) => {
            let members: Vec<String> = members.iter().map(raw_type_display).collect();
            format!("spec {}", members.join(" + "))
        }
        Type::AnonymousEnum(members) => {
            let members: Vec<String> = members.iter().map(raw_type_display).collect();
            format!("enum {}", members.join(" | "))
        }
    }
}

fn generic_name(name: &Ident, type_args: &[ResolvedType]) -> String {
    if type_args.is_empty() {
        return name.as_ref().to_string();
    }
    let args = type_args
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    format!("{}<{}>", name.as_ref(), args)
}

#[derive(Debug, Clone)]
pub enum TypeResolutionError {
    UnrecognizedNamedType {
        name: Ident,
        similar: Option<Ident>,
    },
    ModuleNotImported {
        name: Ident,
        similar: Option<Ident>,
    },
    InvalidArraySize(String),
    ModuleResolution(ResolveError),
    NotAType(Vec<Ident>),
    NoSuchVariantForType {
        r#enum: Ident,
        name: Ident,
        similar: Option<Ident>,
    },
    NotASpec(Ident),
    SpecNotObjectSafe(Ident),
    SpecStaticNotAllowedHere(Ident),
    SpecUsedAsValueType(Ident),
    NeverNotAllowedHere,
    BareUnsizedArray,
    BareUnknownSizeArray,
    UnknownCallingConvention {
        name: Ident,
    },
    CallingConventionNotAvailable {
        name: Ident,
        convention: CallingConvention,
        target: Target,
    },
    VariadicNotSupportedByConvention {
        convention: CallingConvention,
    },
    AnonymousEnumTooManyMembers {
        count: usize,
    },
}

impl fmt::Display for TypeResolutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnrecognizedNamedType { name, .. } => {
                write!(f, "cannot find type '{}' in this scope", name.as_ref())
            }
            Self::ModuleNotImported { name, .. } => {
                write!(f, "module '{}' is not imported", name.as_ref())
            }
            Self::InvalidArraySize(size) => {
                write!(f, "array size '{size}' does not fit a u32")
            }
            Self::ModuleResolution(e) => write!(f, "{e}"),
            Self::NotAType(path) => write!(f, "'{}' is a value, not a type", join(path)),
            Self::NoSuchVariantForType { r#enum, name, .. } => {
                write!(
                    f,
                    "no variant '{}' on enum '{}'",
                    name.as_ref(),
                    r#enum.as_ref()
                )
            }
            Self::NotASpec(name) => write!(f, "'{}' is not a spec", name.as_ref()),
            Self::SpecNotObjectSafe(name) => {
                write!(
                    f,
                    "'{}' can't be used as 'spec *{}' -- it has a 'spec T' (static-dispatch) return requirement, \
                     which no dynamic-dispatch vtable slot can represent",
                    name.as_ref(),
                    name.as_ref()
                )
            }
            Self::SpecStaticNotAllowedHere(name) => {
                write!(
                    f,
                    "'spec {}' is only allowed as a parameter type, or as a return type inside a spec's own function declaration",
                    name.as_ref()
                )
            }
            Self::SpecUsedAsValueType(name) => {
                write!(
                    f,
                    "'{}' is a spec -- it has no size on its own, so it can't be used as a value's type",
                    name.as_ref()
                )
            }
            Self::NeverNotAllowedHere => {
                write!(
                    f,
                    "'never' is only allowed as a function/method's own return type"
                )
            }
            Self::BareUnsizedArray => {
                write!(f, "'[]T' is not valid on its own")
            }
            Self::BareUnknownSizeArray => {
                write!(f, "'[?]T' is not valid on its own")
            }
            Self::UnknownCallingConvention { name } => {
                write!(f, "unknown calling convention '{}'", name.as_ref())
            }
            Self::CallingConventionNotAvailable { name, target, .. } => write!(
                f,
                "calling convention '{}' is not available on target '{target}'",
                name.as_ref()
            ),
            Self::VariadicNotSupportedByConvention { convention } => write!(
                f,
                "the '{convention}' calling convention does not support variadic functions"
            ),
            Self::AnonymousEnumTooManyMembers { count } => write!(
                f,
                "an anonymous enum has {count} distinct members, but its tag is a 'u16' and can \
                 only distinguish {} of them",
                crate::resolved_type::ResolvedAnonymousEnum::MAX_MEMBERS
            ),
        }
    }
}

impl std::error::Error for TypeResolutionError {}

#[derive(Debug, Clone)]
pub struct AnalysisError {
    pub node_id: HirId,
    pub span: Span,
    pub kind: AnalysisErrorKind,
    /// The file `span` -- and any other unqualified span this finding carries
    /// -- indexes.
    pub source: Option<SourceId>,
    /// Set when macro expansion put the actionable syntax somewhere other
    /// than `span` in `source`.
    pub authored: Option<AuthoredSite>,
}

impl AnalysisError {
    pub fn new(node_id: HirId, span: Span, kind: AnalysisErrorKind) -> Self {
        Self {
            node_id,
            span,
            kind,
            source: None,
            authored: None,
        }
    }

    pub fn in_source(mut self, source: Option<SourceId>) -> Self {
        self.source = source;
        self
    }

    pub fn authored_at(mut self, authored: Option<AuthoredSite>) -> Self {
        self.authored = authored;
        self
    }

    pub fn to_diagnostic(&self) -> Diagnostic {
        let diagnostic = self.kind.to_diagnostic(self.span).in_source(self.source);
        relocate(diagnostic, self.authored.as_ref())
    }
}

impl fmt::Display for AnalysisError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind)
    }
}

impl std::error::Error for AnalysisError {}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        word.to_string()
    } else {
        format!("{word}s")
    }
}

fn type_list(types: &[ResolvedType]) -> String {
    let names: Vec<String> = types.iter().map(|t| format!("'{t}'")).collect();
    match names.as_slice() {
        [one] => one.clone(),
        [one, two] => format!("{one} and {two}"),
        [init @ .., last] => format!("{}, and {last}", init.join(", ")),
        [] => String::new(),
    }
}

fn ident_list(names: &[Ident]) -> String {
    let names: Vec<String> = names.iter().map(|f| format!("'{}'", f.as_ref())).collect();
    match names.as_slice() {
        [one] => one.clone(),
        [one, two] => format!("{one} and {two}"),
        [init @ .., last] => format!("{}, and {last}", init.join(", ")),
        [] => String::new(),
    }
}

fn field_list(fields: &[Ident]) -> String {
    format!("{} {}", plural(fields.len(), "field"), ident_list(fields))
}

/// Where a finding is honestly actionable when a macro, not the module being
/// analyzed, authored the syntax it is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthoredSite {
    pub macro_name: Ident,
    /// The macro declaration the syntax was written in.
    pub at: SourceSpan,
    /// The invocation chain that brought it here, innermost first. Entries
    /// whose module has no retained source are dropped rather than guessed.
    pub expansion: Vec<(Ident, SourceSpan)>,
}

/// Moves a finding's primary label to the syntax's authored site and keeps the
/// invocation chain as context. Labels the kind placed elsewhere stay where
/// they are: they describe other declarations, not the reported construct.
pub(crate) fn relocate(mut diagnostic: Diagnostic, authored: Option<&AuthoredSite>) -> Diagnostic {
    let Some(authored) = authored else {
        return diagnostic;
    };
    if let Some(label) = diagnostic
        .labels
        .iter_mut()
        .find(|label| label.style == LabelStyle::Primary)
    {
        label.source = Some(authored.at.source);
        label.span = authored.at.span;
    }
    for (name, at) in &authored.expansion {
        diagnostic = diagnostic
            .with_secondary_label_in(*at, format!("expanded from `{}` here", name.as_ref()));
    }
    diagnostic.with_note(format!(
        "this comes from the `{}` macro, so the fix belongs in its definition",
        authored.macro_name.as_ref()
    ))
}
