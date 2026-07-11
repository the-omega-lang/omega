/// `b"..."` -- decoded content (escapes already resolved), same shape as
/// `StringExpr`; see `Expression::ByteString`'s doc comment for how the two
/// differ downstream.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ByteStringExpr(pub String);
