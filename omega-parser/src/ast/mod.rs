//! The parsed syntax tree -- data definitions only. Unlike the old
//! `syntax/` tree this replaces (where every file paired a struct/enum with
//! an inline `impl X { parser!(...) }` chumsky combinator), parsing logic
//! lives entirely in `crate::parser`; these types are just what it builds.
//! Kept in the same file-per-construct layout as before for continuity.
pub mod annotation;
pub mod expression;
pub mod generics;
pub mod identifier;
pub mod range;
pub mod statement;
pub mod r#type;
