#![allow(clippy::too_many_arguments, clippy::large_enum_variant)]

pub mod aliases;
pub mod analysis;
pub mod annotations;
pub mod checked;
pub mod comp_eval;
mod context;
pub use context::BUILTIN_TYPE_NAMES;
pub mod dead_code;
pub mod error;
mod exhaustiveness;
pub mod generics;
pub mod layout;
pub mod resolved_type;
pub mod resolver;
pub mod similarity;
pub mod target;

pub use target::{Arch, Os, Target, TargetParseError};
