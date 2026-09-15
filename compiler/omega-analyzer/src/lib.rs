#![allow(clippy::too_many_arguments, clippy::large_enum_variant)]

pub mod aliases;
pub mod analysis;
pub mod annotation_eval;
pub mod annotations;
pub mod checked;
pub mod comp_eval;
pub mod compiler_definitions;
mod context;
pub use context::{DeclarationPolicy, is_reserved_type_name};
pub mod dead_code;
pub mod error;
mod exhaustiveness;
pub mod generics;
pub mod layout;
pub mod resolved_type;
pub mod resolver;
pub mod runtime_checks;
pub mod similarity;
pub mod target;
pub mod type_key;

pub use target::{Arch, Os, Target, TargetParseError};
