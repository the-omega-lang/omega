//! Contextual keywords remain ordinary identifiers except at their grammar-defined positions.

/// Binding mutability, pointer mutability, and mutable `self`.
pub const MUT: &str = "mut";
/// Compile-time evaluation and compile-time bindings.
pub const COMP: &str = "comp";
/// The first parameter of a member function.
pub const SELF: &str = "self";
/// Visibility-bypassing expression and import modifier.
pub const REVEAL: &str = "reveal";
/// Type-size expression.
pub const SIZEOF: &str = "sizeof";
/// The iterator separator in a `for` loop.
pub const IN: &str = "in";
/// Public visibility modifier.
pub const EXPOSED: &str = "exposed";
/// Module-internal visibility modifier.
pub const INTERNAL: &str = "internal";
/// Struct declaration without fields.
pub const MARKER: &str = "marker";
/// Platform capability declaration.
pub const GAP: &str = "gap";
/// Platform capability implementation.
pub const GLUE: &str = "glue";
/// Type-to-spec implementation declaration.
pub const CONFORM: &str = "conform";
/// Separator between a conformance target and spec.
pub const TO: &str = "to";
/// Compiler-provided type implementation declaration.
pub const PRIMITIVE: &str = "primitive";
/// Absolute import root modifier.
pub const ROOT: &str = "root";
/// Macro expression fragment kind.
pub const EXPR: &str = "expr";
/// Macro type fragment kind.
pub const TYPE: &str = "type";
/// Macro identifier fragment kind.
pub const IDENT: &str = "ident";

/// Every contextual word. Tests use this to prove each remains an identifier
/// outside its reserved grammar position.
pub const ALL: &[&str] = &[
    MUT, COMP, SELF, REVEAL, SIZEOF, IN, EXPOSED, INTERNAL, MARKER, GAP, GLUE, CONFORM, TO,
    PRIMITIVE, ROOT, EXPR, TYPE, IDENT,
];
