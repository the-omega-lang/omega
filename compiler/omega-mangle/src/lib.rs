//! Omega's symbol mangling scheme, adapted from Rust's RFC 2603 (v0
//! mangling) rather than ported verbatim -- see this crate's design plan
//! for the full rationale. The one deliberate, load-bearing deviation:
//! RFC 2603 doesn't encode function parameter types, because Rust has no
//! overloading; Omega does, so a `Symbol`'s `signature` is load-bearing
//! for uniqueness here in a way it never needs to be for Rust.
//!
//! This crate is intentionally standalone -- it knows nothing about
//! `omega_analyzer`'s `ResolvedType`/`CheckedFunctionDef` or any other
//! compiler-internal representation. Callers (`omega_codegen`) build a
//! `Symbol` from whatever they have on hand and call `encode`; tools
//! (`omg-demangle`) call `demangle` on raw linker symbols.
//!
//! Grammar (EBNF; terminals in quotes):
//!
//! ```text
//! <symbol>    = "_omg_" <path> [<signature>] [<vendor-suffix>]
//! <path>      = "C" <ident>                    // module root
//!             | "N" <namespace> <path> <ident> // nested (module::sub, Type::method, ...)
//!             | "I" <path> {<type>} "E"        // generic args
//!             | <backref>
//! <namespace> = "t" | "v"                      // type / value namespace
//! <ident>     = <decimal-len> ["_"] <bytes>    // length-prefixed; "_" iff <bytes> starts with a digit or "_"
//! <signature> = {<type>} "E" <type>            // params (self included when present) + return
//! <type>      = <basic> | <path>
//!             | "R" <path> <base62>            // MyEnum::Variant (refined enum type)
//!             | "P" <type> | "Q" <type>        // *T / *mut T
//!             | "S" <type> | "W" <type>        // *[T] / *mut [T]
//!             | "G" <type>                     // [T] (decayed array param)
//!             | "A" <type> <base62>            // [T; N]
//!             | "D" <type> | "K" <type>        // spec *T / spec *mut T
//!             | "T" | "U"                       // *str / *mut str
//!             | "F" ["V"] {<type>} "E" <type>  // fn(...) -> ...; "V" marks variadic
//!             | <backref>
//! <basic>     = one letter per primitive (void/bool/char/i8/i16/i32/i64/isize/u8/u16/u32/u64/usize/f32/f64)
//! <base62>    = {0-9a-zA-Z} "_"                // 0-based, offset-by-one; "_" alone is 0
//! <backref>   = "B" <base62>                   // byte offset into the mangled string so far
//! <vendor-suffix> = "." <bytes>
//! ```
//!
//! `E` is reserved exclusively as a list terminator -- never reused as a
//! leading tag -- and every optional element sits at the *start* of a
//! self-terminating production, never as a trailing suffix on one, so
//! there is no positional ambiguity anywhere in the grammar.
//!
//! No Rust-style `M`/`X`/`Y`/`<impl-path>` (Omega methods nest directly
//! under their owner type's own path -- there's no separate, possibly
//! anonymous "impl block" to root a path at). No `<lifetime>`/`<binder>`
//! (no borrow checker). No disambiguator-index or Punycode (Omega has no
//! closures, no macro hygiene collisions once the full signature is part
//! of the symbol, and no non-ASCII identifiers) -- see the design plan's
//! Context section for why each of these is safe to omit, not merely
//! unimplemented.

mod base62;
mod encode;
mod grammar;
mod demangle;
pub mod symbol;

pub use encode::encode;
pub use demangle::{decode, demangle};
pub use symbol::{ManglePath, MangleType, Namespace, Symbol};
