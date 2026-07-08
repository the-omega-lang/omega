use crate::ast::identifier::Ident;

/// Which radix a number literal's integer (and, for `Decimal`, fractional)
/// digits were written in. Kept alongside the digit text rather than eagerly
/// computed into a value here -- the same reason `explicit_type` is kept as
/// `Ident` text -- since only semantic analysis knows which concrete
/// resolved type the literal will end up as, and therefore how to range-check
/// it (`0xFF` might be a `u8`, an `i32`, or anything else numeric).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NumberBase {
    Decimal,
    Hex,
    Octal,
    Binary,
}

impl NumberBase {
    pub fn radix(self) -> u32 {
        match self {
            Self::Decimal => 10,
            Self::Hex => 16,
            Self::Octal => 8,
            Self::Binary => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NumberExpr {
    pub base: NumberBase,
    pub integer_part: String,
    /// Only ever `Some` for `NumberBase::Decimal` -- the grammar has no
    /// hex/octal/binary float notation (e.g. no `0x1.8p0`), so a fraction is
    /// only ever produced alongside a decimal integer part.
    pub fractional_part: Option<String>,
    pub explicit_type: Option<Ident>,
}
