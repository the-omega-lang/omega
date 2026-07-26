//! The mid-level IR: a control-flow-graph-based lowering of
//! `omega_analyzer::checked`'s fully resolved, monomorphized tree, sitting
//! between semantic analysis and `omega-codegen`. See
//! `omega-docs/16-mir-and-codegen.md` for the full rationale (multi-backend
//! support is the driving reason) and `crate::body`'s module doc comment
//! for the CFG shape itself.
//!
//! `lower_program` is the crate's one entry point: it takes every checked
//! module a compilation produced and returns their MIR counterparts,
//! one-to-one, in the same order -- nothing here is whole-program-aware
//! (monomorphization has already fully run by the time a `CheckedModule`
//! exists), so each module lowers independently.

pub mod body;
pub mod ids;
mod lower;
pub mod mir;

pub use body::*;
pub use ids::{BlockId, LocalId};
pub use lower::lower_program;
pub use mir::*;
