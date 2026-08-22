mod kind;
mod render;
mod warning;

pub use kind::AnalysisErrorKind;
pub use render::resolve_error_diagnostic;
pub use warning::{AnalysisWarning, AnalysisWarningKind};

use crate::resolved_type::{CallingConvention, NumericKind, ResolvedFunctionType, ResolvedType};
use crate::target::Target;
use crate::resolver::ResolveError;
use omega_diagnostics::Diagnostic;
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
                .map(|p| format!("{}: {}", p.ident.as_ref(), raw_type_display(&p.r#type)))
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
        }
    }
}

impl std::error::Error for TypeResolutionError {}

#[derive(Debug, Clone)]
pub struct AnalysisError {
    pub node_id: HirId,
    pub span: Span,
    pub kind: AnalysisErrorKind,
}

impl AnalysisError {
    pub fn new(node_id: HirId, span: Span, kind: AnalysisErrorKind) -> Self {
        Self {
            node_id,
            span,
            kind,
        }
    }

    pub fn to_diagnostic(&self) -> Diagnostic {
        self.kind.to_diagnostic(self.span)
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
