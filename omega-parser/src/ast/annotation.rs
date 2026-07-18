use crate::ast::identifier::Ident;
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

/// One argument inside `@name(...)`: a bare identifier (`packed`, `always`,
/// `enabled`, a `@suppress` warning name, ...) or a `key = value` pair
/// (`align = 4`). The value is kept as raw decimal digit text, matching
/// `parser::type::parse_array_size`'s exact "shape, not value" convention
/// for a bare integer -- no separators/suffix/fraction/base prefix are
/// accepted here at all, so a based/suffixed/fractional literal is rejected
/// at parse time rather than silently misread later.
#[derive(Debug, Clone)]
pub enum AnnotationArg {
    Ident(Ident),
    KeyValue(Ident, String),
}
