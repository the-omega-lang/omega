//! Every finding analysis can produce, and how each one renders.
//!
//! Findings are kept fully structured all the way to the CLI -- a variant
//! with typed fields, never a pre-rendered string -- so a diagnostic can
//! anchor real spans, suggest real names, and be reformatted freely. That
//! split is why this module has three layers:
//!
//! - the finding itself ([`AnalysisErrorKind`], [`AnalysisWarningKind`]) --
//!   data only;
//! - its one-line headline (`Display`, in [`kind`]/[`warning`]);
//! - its full annotated form (`to_diagnostic`, in [`render`]).

mod kind;
mod render;
mod warning;

pub use kind::AnalysisErrorKind;
pub use render::resolve_error_diagnostic;
pub use warning::{AnalysisWarning, AnalysisWarningKind};

use crate::resolved_type::{NumericKind, ResolvedFunctionType, ResolvedType};
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

/// A raw (unresolved) `Type` spelled back for a diagnostic -- the shapes a
/// reader wrote, not a resolved-type `Display` (the type never resolved, by
/// definition). Kept here rather than on `Type` itself since nothing else
/// needs it.
pub fn raw_type_display(ty: &omega_parser::prelude::Type) -> String {
    use omega_parser::prelude::Type;
    match ty {
        Type::Named(path) => join(&path.segments()),
        Type::Generic(path, args) => {
            let args: Vec<String> = args.iter().map(raw_type_display).collect();
            format!("{}<{}>", join(&path.segments()), args.join(", "))
        }
        Type::Pointer(inner, mutable) => {
            format!("*{}{}", if *mutable { "mut " } else { "" }, raw_type_display(inner))
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
            format!("({}) => {}", params.join(", "), raw_type_display(&f.return_type))
        }
        Type::SpecObject(inner, mutable) => {
            format!("spec *{}{}", if *mutable { "mut " } else { "" }, raw_type_display(inner))
        }
        Type::SpecStatic(inner) => format!("spec {}", raw_type_display(inner)),
    }
}

/// Renders a possibly-generic name for a diagnostic -- `"Name"` when
/// `type_args` is empty, `"Name<Arg1, Arg2>"` otherwise. Used where
/// `ResolvedType::Struct`/`Spec`'s own `Display` deliberately stays bare
/// but the diagnostic needs to show *which* instantiation it's about.
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
    /// A bare type name that doesn't exist in scope. `similar` is a
    /// close-enough visible type name, when one exists -- the "did you
    /// mean" candidate (computed at error time, while the scope still
    /// exists to search).
    UnrecognizedNamedType { name: Ident, similar: Option<Ident> },
    /// A qualified reference (`mymodule::Foo`) whose head was never bound
    /// by an `import` -- nothing is visible across modules without one.
    ModuleNotImported { name: Ident, similar: Option<Ident> },
    /// `[T; N]`'s `N` doesn't fit `u32` -- kept as raw text by the parser
    /// (same as `NumberExpr`'s integer literals) and only parsed/range-checked
    /// here, during type resolution.
    InvalidArraySize(String),
    /// A qualified type path (`mymodule::Foo`) failed to resolve across
    /// modules -- unknown module/item, not visible, or a cycle. See
    /// `crate::resolver::ModuleResolver`.
    ModuleResolution(ResolveError),
    /// A qualified path resolved to a value (a function/extern/global), not
    /// a type, in a position that requires a type.
    NotAType(Vec<Ident>),
    /// `Enum::Name` in *type* position (e.g. `x: *Entity::Name`) where
    /// `Name` isn't one of `Enum`'s variants -- the type-position mirror of
    /// `AnalysisErrorKind::NoSuchEnumMember`.
    NoSuchVariantForType {
        r#enum: Ident,
        name: Ident,
        similar: Option<Ident>,
    },
    /// `spec *Foo`/`spec *mut Foo` where `Foo` resolved to something other
    /// than a spec (a struct, a primitive, ...) -- a dynamic-dispatch
    /// pointer's pointee must always be a spec.
    NotASpec(Ident),
    /// `spec *Foo`/`spec *mut Foo` where `Foo` has at least one `spec T`
    /// (static-dispatch, associated-type-like) return requirement, directly
    /// or through a dependency -- see `ResolvedSpecType::is_object_safe`'s
    /// doc comment for why a spec shaped this way can never back a
    /// dynamic-dispatch trait object.
    SpecNotObjectSafe(Ident),
    /// `spec Foo` (no `*`, static dispatch) written somewhere other than a
    /// parameter type or a return type inside a spec's own function
    /// declaration -- the only two positions this sugar is defined for
    /// (see `Type::SpecStatic`'s doc comment).
    SpecStaticNotAllowedHere(Ident),
    /// A bare spec name (`Animal`, no `spec *`/`spec` prefix) resolved
    /// somewhere a value's actual type is required -- a variable's
    /// declaration, a field, a parameter, a return type, a cast/`sizeof`
    /// target, a generic argument, and so on. A spec definition alone has
    /// no size or representation; only `spec *Foo` (dynamic dispatch) or a
    /// generic bound (`T: Foo`) give it one.
    SpecUsedAsValueType(Ident),
    /// `never` resolved somewhere other than a function/method/extern/gap's
    /// own declared return type. There is no `never`-typed value to store
    /// anywhere, only a proof a return position is unreachable; a `(...) =>
    /// never` function type used elsewhere is unaffected.
    NeverNotAllowedHere,
    /// `[]T` reached ordinary type resolution directly -- inferred-size,
    /// nothing here to give it a length. Only legal behind a leading `*`
    /// (`*[]T`) or paired with an array-literal initializer that infers the
    /// length (see `Analyzer::resolve_typed_decl_init`).
    BareUnsizedArray,
    /// `[?]T` reached ordinary type resolution directly -- unlike `[]T`,
    /// there is no standalone-legal case: a slice's length is only known
    /// at runtime. Only legal behind a leading `*` (`*[?]T`).
    BareUnknownSizeArray,
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

    /// The renderable form of this error: a headline stating the problem, a
    /// caret label localizing it, and -- where a language rule or a likely
    /// fix genuinely helps -- a `note:`/`help:` footer. Advice is only
    /// attached where it's always true; a wrong hint is worse than none.
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

/// `'a'` / `'a' and 'b'` / `'a', 'b', and 'c'` -- the bare listing
/// `field_list`/`NonExhaustiveMatchEnum`'s diagnostic build their own
/// noun-prefixed message around.
fn ident_list(names: &[Ident]) -> String {
    let names: Vec<String> = names.iter().map(|f| format!("'{}'", f.as_ref())).collect();
    match names.as_slice() {
        [one] => one.clone(),
        [one, two] => format!("{one} and {two}"),
        [init @ .., last] => format!("{}, and {last}", init.join(", ")),
        [] => String::new(),
    }
}

/// `field 'a'` / `fields 'a' and 'b'` / `fields 'a', 'b', and 'c'` -- for
/// `MissingFieldInitializers`' headline and label.
fn field_list(fields: &[Ident]) -> String {
    format!("{} {}", plural(fields.len(), "field"), ident_list(fields))
}
