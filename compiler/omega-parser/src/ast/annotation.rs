use crate::ast::expression::NumberExpr;
use crate::ast::identifier::{Ident, Origin};
use crate::ast::r#type::Type;
use crate::diagnostics::Span;

#[derive(Debug, Clone)]
pub struct AnnotationNode {
    pub name: Ident,
    pub args: Vec<AnnotationArg>,
    pub span: Span,
    /// Origin of the annotation itself, independent of substituted arguments.
    pub origin: Origin,
}

#[derive(Debug, Clone)]
pub enum AnnotationArg {
    Positional(AnnotationExpr),
    KeyValue(Ident, AnnotationExpr),
}

impl AnnotationArg {
    /// The bare name a positional argument was written as, for the
    /// annotations whose arguments are plain words (`@inline(always)`).
    pub fn bare_name(&self) -> Option<&Ident> {
        match self {
            Self::Positional(expr) => expr.name(),
            Self::KeyValue(..) => None,
        }
    }

    pub fn span(&self) -> Span {
        match self {
            Self::Positional(expr) | Self::KeyValue(_, expr) => expr.span,
        }
    }
}

/// One argument expression of an annotation. The grammar is shared by every
/// annotation, so what a given form *means* -- and which forms are accepted at
/// all -- belongs to the annotation that consumes it.
#[derive(Debug, Clone)]
pub struct AnnotationExpr {
    pub kind: AnnotationExprKind,
    pub span: Span,
    /// Provenance of the token that introduced this expression, so a
    /// diagnostic about macro-authored syntax can name its author.
    pub origin: Origin,
}

#[derive(Debug, Clone)]
pub enum AnnotationExprKind {
    Literal(AnnotationLiteral),
    Name(Ident),
    /// `namespace::name`. Annotation arguments never enter ordinary name
    /// resolution, so exactly two segments are representable.
    Qualified {
        namespace: Ident,
        name: Ident,
    },
    Call {
        name: Ident,
        args: Vec<AnnotationExpr>,
    },
    /// `&[a, b]` -- a membership list. It has no allocation, address, or
    /// slice semantics; the spelling only borrows the familiar shape.
    List(Vec<AnnotationExpr>),
    Sizeof(Type),
}

#[derive(Debug, Clone)]
pub enum AnnotationLiteral {
    Bool(bool),
    /// `negative` records a written leading `-`; the magnitude stays in
    /// `value` so a signed minimum decodes without wrapping.
    Number {
        negative: bool,
        value: NumberExpr,
    },
    Char(char),
    Str(String),
    ByteStr(String),
}

impl AnnotationExpr {
    /// The bare word this expression was written as, for the annotations
    /// whose arguments are plain words rather than values.
    pub fn name(&self) -> Option<&Ident> {
        match &self.kind {
            AnnotationExprKind::Name(name) => Some(name),
            _ => None,
        }
    }

    pub fn describe(&self) -> &'static str {
        match &self.kind {
            AnnotationExprKind::Literal(literal) => literal.describe(),
            AnnotationExprKind::Name(_) => "a name",
            AnnotationExprKind::Qualified { .. } => "a qualified reference",
            AnnotationExprKind::Call { .. } => "a call",
            AnnotationExprKind::List(_) => "a list",
            AnnotationExprKind::Sizeof(_) => "a 'sizeof<Type>'",
        }
    }
}

impl AnnotationLiteral {
    pub fn describe(&self) -> &'static str {
        match self {
            Self::Bool(_) => "a boolean",
            Self::Number { .. } => "a number",
            Self::Char(_) => "a character literal",
            Self::Str(_) => "a string literal",
            Self::ByteStr(_) => "a byte string literal",
        }
    }
}
