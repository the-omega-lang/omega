//! The parsed syntax tree -- data definitions only. Unlike the old
//! `syntax/` tree this replaces (where every file paired a struct/enum with
//! an inline `impl X { parser!(...) }` chumsky combinator), parsing logic
//! lives entirely in `crate::parser`; these types are just what it builds.
//! One file per grammar tier, rather than one file per node.
pub mod annotation;
pub mod expression;
pub mod generics;
pub mod identifier;
pub mod item;
pub mod range;
pub mod self_mode;
pub mod statement;
pub mod r#type;
pub mod visibility;
