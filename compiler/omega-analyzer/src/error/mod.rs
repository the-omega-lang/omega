mod kind;
pub(crate) mod render;
mod warning;

pub use kind::AnalysisErrorKind;
pub use render::resolve_error_diagnostic;
pub use warning::{AnalysisWarning, AnalysisWarningKind, WarningPolicy};

use crate::resolved_type::{
    CallingConvention, FunctionNamespace, NumericKind, ResolvedFunctionType, ResolvedGenericArg,
    ResolvedType,
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

/// The written spelling of one generic argument, for diagnostics that echo
/// source syntax back to the reader.
pub fn raw_generic_arg_display(arg: &omega_parser::prelude::GenericArg) -> String {
    use omega_parser::prelude::GenericArg;
    match arg {
        GenericArg::Type(r#type) => raw_type_display(r#type),
        GenericArg::Value(literal) => raw_comp_literal_display(literal),
    }
}

pub fn raw_comp_literal_display(literal: &omega_parser::prelude::CompLiteral) -> String {
    use omega_parser::prelude::CompLiteral;
    match literal {
        CompLiteral::Int { negative, number } => {
            let sign = if *negative { "-" } else { "" };
            format!("{sign}{}", number.integer_part)
        }
        CompLiteral::Bool(value) => value.to_string(),
        CompLiteral::Char(value) => format!("'{value}'"),
    }
}

pub(crate) fn raw_array_length_display(length: &omega_parser::prelude::ArrayLength) -> String {
    use omega_parser::prelude::ArrayLength;
    match length {
        ArrayLength::Literal(literal) => raw_comp_literal_display(literal),
        ArrayLength::Path(path) => join(&path.segments()),
    }
}

pub fn raw_type_display(ty: &omega_parser::prelude::Type) -> String {
    use omega_parser::prelude::Type;
    match ty {
        Type::Named(path) => join(&path.segments()),
        Type::Generic(path, args) => {
            let args: Vec<String> = args.iter().map(raw_generic_arg_display).collect();
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
        Type::SizedArray(inner, length) => format!(
            "[{}]{}",
            raw_array_length_display(length),
            raw_type_display(inner)
        ),
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

fn generic_name(name: &Ident, generic_args: &[ResolvedGenericArg]) -> String {
    if generic_args.is_empty() {
        return name.as_ref().to_string();
    }
    let args = generic_args
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
    /// A written array length that does not resolve to a nonnegative
    /// compile-time integer inside the fixed-array size domain.
    InvalidArrayLength {
        written: String,
        /// The compile-time value the length resolved to, when it resolved at
        /// all. A symbolic length is much harder to read without it.
        value: Option<String>,
        reason: ArrayLengthProblem,
    },
    /// A path used where a compile-time value is required, but the binding
    /// it names is an ordinary runtime binding.
    NotACompValue(Ident),
    /// A type named where a compile-time value is required.
    CompValueIsAType(Ident),
    /// A `comp` binding whose value is not available at this point.
    CompValueUnavailable(Ident),
    /// A `comp` generic parameter declared with a type that has no canonical
    /// compile-time identity yet.
    UnsupportedCompParamType {
        param: Ident,
        r#type: ResolvedType,
    },
    /// A generic argument written as the wrong kind for its parameter.
    GenericArgKindMismatch {
        param: Ident,
        expected_value: bool,
    },
    /// A written scalar literal whose magnitude no compile-time integer can
    /// hold.
    CompLiteralOutOfRange(omega_parser::prelude::CompLiteral),
    /// A compile-time value that cannot be represented exactly in the type
    /// its `comp` parameter declares.
    CompArgNotRepresentable {
        param: Ident,
        value: String,
        declared: ResolvedType,
    },
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
            Self::InvalidArrayLength {
                written,
                value,
                reason,
            } => match value {
                Some(value) if value != written => {
                    write!(f, "array length '{written}' ({value}) {reason}")
                }
                _ => write!(f, "array length '{written}' {reason}"),
            },
            Self::NotACompValue(name) => write!(
                f,
                "'{}' is a runtime binding, but a compile-time value is required here \
                 (declare it with 'comp')",
                name.as_ref()
            ),
            Self::CompValueIsAType(name) => write!(
                f,
                "'{}' is a type, but a compile-time value is required here",
                name.as_ref()
            ),
            Self::CompValueUnavailable(name) => write!(
                f,
                "the compile-time value of '{}' is not available here",
                name.as_ref()
            ),
            Self::UnsupportedCompParamType { param, r#type } => write!(
                f,
                "'comp {}: {type}' is not supported -- a 'comp' generic parameter must currently be \
                 an integer, 'bool', or 'char'",
                param.as_ref()
            ),
            Self::GenericArgKindMismatch {
                param,
                expected_value: true,
            } => write!(
                f,
                "generic parameter '{}' is a 'comp' parameter, so it takes a compile-time value, \
                 not a type",
                param.as_ref()
            ),
            Self::GenericArgKindMismatch {
                param,
                expected_value: false,
            } => write!(
                f,
                "generic parameter '{}' is a type parameter, so it takes a type, not a value",
                param.as_ref()
            ),
            Self::CompLiteralOutOfRange(literal) => write!(
                f,
                "'{}' is outside the range of every compile-time integer type",
                raw_comp_literal_display(literal)
            ),
            Self::CompArgNotRepresentable {
                param,
                value,
                declared,
            } => write!(
                f,
                "'{value}' cannot be represented as '{declared}', the declared type of 'comp {}'",
                param.as_ref()
            ),
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

/// Why a written fixed-array length is not usable as one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrayLengthProblem {
    NotAnInteger,
    Negative,
    TooLarge,
}

impl fmt::Display for ArrayLengthProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAnInteger => write!(f, "is not a compile-time integer"),
            Self::Negative => write!(f, "is negative"),
            Self::TooLarge => write!(f, "does not fit a u32"),
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
