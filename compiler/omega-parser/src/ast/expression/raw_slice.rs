use crate::ast::expression::ExpressionNode;
use crate::ast::r#type::Type;

/// `raw_slice<Type>(ptr, len)` -- constructs a `*[Type]` (or `*mut [Type]`,
/// inherited from `ptr`'s own mutability) directly from a raw data pointer
/// and a runtime element count, with no existing array/slice/`str` to
/// re-slice. There is otherwise no way to build a fat pointer from scratch
/// in this language: ordinary slicing (`base[range]`) only ever re-slices
/// an already-sliceable base (`SizedArray`/`Slice`/`Str`), and casting is
/// one-directional (fat -> thin, never thin -> fat). This is the one
/// escape hatch, needed by anything managing its own heap storage (a
/// growable collection, say) that wants to hand back a real slice view of
/// it. Parsed the same contextual-keyword way `sizeof<Type>` is (see
/// `SizeofExpr`'s doc comment) -- `raw_slice` is recognized only when
/// immediately followed by `<`; used any other way it's still a plain
/// identifier.
#[derive(Debug, Clone)]
pub struct RawSliceExpr {
    pub item_type: Type,
    pub ptr: Box<ExpressionNode>,
    pub len: Box<ExpressionNode>,
}
