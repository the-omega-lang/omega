use crate::ast::identifier::Ident;
use crate::ast::r#type::Type;
use crate::diagnostics::Span;

/// `@name(arg, arg, ...)` -- one annotation, attached above a struct/enum/
/// union/function declaration (see `parser::item::parse_annotations`). The
/// parser only records shape; which names are recognized, which item kinds
/// they're allowed on, and whether their arguments make sense is entirely
/// `omega_analyzer::annotations`'s concern, same division of labor as every
/// other semantic check in this compiler.
#[derive(Debug, Clone)]
pub struct AnnotationNode {
    pub name: Ident,
    pub args: Vec<AnnotationArg>,
    pub span: Span,
}

/// One argument inside `@name(...)`: a bare identifier (`always`, `enabled`,
/// a `@suppress` warning name, ...) or a `key = value` pair (`align = 4`,
/// `pack = sizeof<usize>`).
#[derive(Debug, Clone)]
pub enum AnnotationArg {
    Ident(Ident),
    KeyValue(Ident, AnnotationValue),
}

/// A `key = value` annotation argument's value -- either a plain integer
/// literal or a `sizeof<Type>` query (see `SizeofExpr`'s doc comment; the
/// same construct, just parsed directly in argument-value position rather
/// than as a general expression). An integer literal is kept as raw decimal
/// digit text, matching `parser::type::parse_array_size`'s exact "shape,
/// not value" convention -- no separators/suffix/fraction/base prefix are
/// accepted here at all, so a based/suffixed/fractional literal is rejected
/// at parse time rather than silently misread later.
#[derive(Debug, Clone)]
pub enum AnnotationValue {
    IntLiteral(String),
    Sizeof(Type),
}
