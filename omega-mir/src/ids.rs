/// Indexes into a single [`crate::body::MirBody`]'s `blocks` -- minted
/// sequentially by `FunctionLowerer` (see [`crate::lower::function`]) as it
/// builds a function's control-flow graph. Block `0` is always the entry
/// block (see `MirBody::blocks`'s own doc comment).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockId(pub u32);

/// Indexes into a single [`crate::body::MirBody`]'s `locals`. `0..arg_count`
/// are the function's own parameters, in declaration order; everything
/// after that is either a user-declared local or a lowering-synthesized
/// temporary (a `defer`'s own flag) -- see `MirBody::locals`'s doc comment
/// for why both share one uniform index space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LocalId(pub u32);
