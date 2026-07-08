//! The compiler's diagnostic foundation: source positions ([`Span`]),
//! per-file position translation ([`SourceFile`]), the structured
//! description of one finding ([`Diagnostic`]), and the terminal renderer
//! that turns a finding into a Rust-style annotated snippet ([`Renderer`]).
//!
//! This crate sits at the bottom of the workspace's dependency graph (it
//! depends on nothing, everything else depends on it), so it deliberately
//! knows nothing about tokens, types, or modules -- each compiler stage
//! converts its own error types into [`Diagnostic`]s (see e.g.
//! `omega_parser::diagnostics`), and only the driver/CLI ever renders them.

mod diagnostic;
mod highlight;
mod render;
mod source;
mod span;

pub use diagnostic::{Diagnostic, Label, LabelStyle, Severity};
pub use highlight::{Highlighter, TokenClass};
pub use render::Renderer;
pub use source::SourceFile;
pub use span::Span;
