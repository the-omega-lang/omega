#![allow(clippy::too_many_arguments, clippy::large_enum_variant)]

pub mod analysis;
pub mod annotations;
pub mod checked;
pub mod comp_eval;
mod context;
pub mod dead_code;
pub mod error;
mod exhaustiveness;
mod generics;
pub mod layout;
pub mod resolved_type;
pub mod resolver;
pub mod similarity;
pub mod target;

pub use target::{Arch, Os, Target, TargetParseError};
