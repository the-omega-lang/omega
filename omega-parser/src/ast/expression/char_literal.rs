/// `'c'` -- a single Unicode scalar value, single-quote delimited. Shares
/// its escape grammar with `StringExpr`; unlike a string, exactly one
/// character or escape is allowed between the quotes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CharExpr(pub char);
