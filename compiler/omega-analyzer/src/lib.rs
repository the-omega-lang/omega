//! Semantic analysis: HIR in, a fully typed checked tree out.
//!
//! One [`analysis::Analyzer`] checks exactly one top-level item -- a
//! signature or a body -- and is then thrown away. Anything module-shaped it
//! needs (what a path names, what an import means, another module's items)
//! it asks a [`resolver::ModuleResolver`] for, so nothing in this crate ever
//! touches a filesystem or a cross-module cache.
//!
//! Findings are kept structured all the way out ([`error`]): a variant with
//! typed fields, never a pre-rendered string, so the CLI can anchor real
//! spans and suggest real names.
//!
//! Two conventions run through the whole crate:
//!
//! - **A node is identified by its `(HirId, Span)` pair**, threaded
//!   explicitly rather than carried in the analyzer's state -- which is why
//!   so many functions here take both. (Collapsing the pair into one type is
//!   worth doing, but it reaches into `omega-hir` and every call site; see
//!   the design review.)
//! - **Resolve once, at signature time; read back everywhere.** Annotations,
//!   self-mode, method identities and spec visibility are all decided when a
//!   signature is collected, and every later phase reads those decisions
//!   back instead of re-deriving them.

// Both of these follow from the conventions above: the argument count from
// the explicit `(HirId, Span)` pair, and the enum size from one short-lived
// local enum whose larger variant is never stored. Boxing either would cost
// clarity for no real benefit.
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
