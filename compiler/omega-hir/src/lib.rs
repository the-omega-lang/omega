//! The high-level intermediate representation: the first tree that exists
//! *after* macro expansion, and the first one whose nodes have stable
//! identities.
//!
//! # Why this is a separate tree from the AST
//!
//! Most of HIR is shaped like `omega_parser`'s AST, which invites deleting
//! it and having the analyzer read the AST directly. It earns its place for
//! one structural reason: **`omega_parser::macros::expand` splices tokens
//! and re-parses**, so any id assigned before expansion would be invalidated
//! by it, and any node present before expansion may not survive it. HIR is
//! the earliest point at which a node can be given an identity that lasts
//! for the rest of the compilation -- which is what [`HirId`] is, and what
//! every later pass (analysis, MIR, codegen, the driver's monomorphization
//! cache) keys on.
//!
//! # What lowering owns
//!
//! Exactly four desugarings, all of which need no type information and so
//! would otherwise be done ad hoc, differently, by whoever needed them
//! first:
//!
//! 1. **`self` insertion** -- a member function's synthetic `self: *Self`
//!    parameter, shaped by its [`SelfMode`](omega_parser::prelude::SelfMode).
//! 2. **`mut self` shadowing** -- a by-value `mut self` becomes an implicit
//!    `mut self := self;` as the body's first statement, so no downstream
//!    pass needs a notion of a mutable parameter.
//! 3. **`spec T` parameters** -- `f(x: spec Foo)` becomes an ordinary bound
//!    generic `f<$Param0: Foo>(x: $Param0)`, so nothing after this point
//!    sees `Type::SpecStatic` in parameter position at all.
//! 4. **Place-chain flattening** -- the parser's nested
//!    `FieldAccess`/`Index`/`Deref` expressions become one [`HirPlace`]: a
//!    root plus a flat projection list. The parser has no notion of an
//!    addressable location; recognizing one is this crate's job.
//!
//! # What lowering deliberately does not do
//!
//! No name resolution, no type checking, no validation of any kind.
//! [`lower_module`] is **infallible**: every rejectable question is
//! `omega_analyzer`'s, which keeps "can this program be rejected here?"
//! answerable per pass rather than per call site. Types, paths and
//! annotations are all carried through raw and unresolved.
//!
//! # Spans
//!
//! A construct that can be the subject of a diagnostic owns its own span.
//! This is load-bearing rather than decorative: spans used to live only on
//! the parser's wrapper nodes (`ItemNode`/`StatementNode`), and anything
//! never wrapped in one -- every method, field, parameter and spec function
//! -- inherited its parent's, so a duplicate struct field underlined the
//! whole struct and a return-type mismatch underlined the whole body. See
//! [`HirParam::name_span`] and [`HirFunctionDef::return_type_span`].

pub mod hir;
pub mod ids;
pub mod lower;

pub use hir::*;
pub use ids::{HirId, ModuleId, SYNTHETIC_MODULE};
pub use lower::lower_module;
