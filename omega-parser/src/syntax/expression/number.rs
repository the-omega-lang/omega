use crate::{parser, prelude::Ident, syntax::ParseError};
use chumsky::prelude::*;

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

/// One or more base-`radix` digits, with `_` allowed anywhere after the
/// first digit as a visual separator (`1_000_000`, `0xDE_AD_BE_EF`, and even
/// `1_2_3` or a trailing `100_` immediately before a type suffix) -- matching
/// how mainstream languages with this feature (e.g. Rust) actually specify
/// it: the only hard requirement is that the literal not *start* with `_`
/// (that would instead be an identifier). Returns the digit text with
/// separators already stripped, ready to hand straight to
/// `<int>::from_str_radix`.
fn radix_digits<'a>(radix: u32) -> impl Parser<'a, &'a str, String, ParseError<'a>> + Clone {
    let digit = any().filter(move |c: &char| c.is_digit(radix));
    digit
        .then(choice((digit, just('_'))).repeated())
        .to_slice()
        .map(|s: &str| s.replace('_', ""))
}

impl NumberExpr {
    parser!(() => Self {
        // A type suffix is `i`/`u`/`f` followed by plain decimal digits
        // (`i64`, `u8`, `f32`, ...), or the literal keyword `usize`/`isize`
        // (pointer-sized, no digit width of their own) -- kept as arbitrary
        // `Ident` text rather than validated against a fixed list here, the
        // same as before: only semantic analysis knows which names actually
        // resolve to a numeric type (see `Analyzer::analyze_expr`'s
        // `HirExpr::Number` arm). The keyword forms are tried first so
        // `5isize` isn't instead parsed as `5i` followed by a dangling
        // `size` (chumsky's `choice` would still backtrack correctly either
        // way, since `suffix_digits` requires at least one digit and `size`
        // has none, but trying the complete keyword first is clearer).
        let suffix_digits = text::digits(10).at_least(1).to_slice().map(ToString::to_string);
        let explicit_type_parser = choice((
            text::keyword("usize").to(Ident("usize".to_string())),
            text::keyword("isize").to(Ident("isize".to_string())),
            choice((just('i'), just('u'), just('f')))
                .then(suffix_digits)
                .map(|(prefix, digits)| Ident(format!("{prefix}{digits}"))),
        ));

        let based_prefix = |prefix: &'static str, base: NumberBase| {
            just(prefix)
                .ignore_then(radix_digits(base.radix()))
                .map(move |digits| (base, digits))
        };

        let based_int = choice((
            based_prefix("0x", NumberBase::Hex),
            based_prefix("0o", NumberBase::Octal),
            based_prefix("0b", NumberBase::Binary),
        ))
        .then(explicit_type_parser.clone().or_not())
        .map(|((base, integer_part), explicit_type)| Self {
            base,
            integer_part,
            fractional_part: None,
            explicit_type,
        });

        // Tried second: `0x`/`0o`/`0b` above already claims anything with
        // that prefix, so an ordinary decimal literal (including one that
        // just starts with `0`, like `0` or `007`) falls through to here.
        let decimal = radix_digits(10)
            .then(just('.').ignore_then(radix_digits(10)).or_not())
            .then(explicit_type_parser.or_not())
            .map(|((integer_part, fractional_part), explicit_type)| Self {
                base: NumberBase::Decimal,
                integer_part,
                fractional_part,
                explicit_type,
            });

        choice((based_int, decimal))
    });
}
