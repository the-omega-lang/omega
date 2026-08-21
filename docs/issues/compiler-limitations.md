# Compiler implementation limitations

Implementation caveats migrated out of architecture chapters. These are non-normative and should be removed when resolved.


## Parsing, macro expansion, and the HIR

- **Recovery granularity is coarse.** `synchronize_to_item_boundary` and
  `synchronize_to_statement_boundary` both treat any identifier as a
  plausible boundary, so recovery often stops almost immediately after the
  error. This is sufficient (one mistake yields one error) but it is not
  precise, and a badly-malformed member can still swallow its enclosing
  block's closing brace.
- **Macro expansion traverses the AST by hand.** `macros/expander.rs` reconstructs
  expression/statement/item nodes field-by-field in order to recurse. Exhaustive
  matching keeps the traversal honest, but the boilerplate grows with the AST; a
  future visitor/fold abstraction could centralize that traversal if it can do so
  without hiding the expansion rules.
- **HIR still mirrors the AST closely.** That is the cost of the identity
  boundary described above, not an accident, but it does mean two node sets
  to keep in step.
- See [known-issues.md](known-issues.md) for the language-level
  questions this area raises that are *not* bugs and were deliberately not
  decided during refactoring.


## Diagnostics display width

- **Terminal-width handling is scalar-based, not grapheme-aware.** `omega-diagnostics`
  now uses one shared display-column calculation for headers and underlines and expands tabs
  consistently, but non-tab Unicode scalar values still count as one terminal column. Combining
  marks, full-width CJK characters, emoji sequences, and similar text can therefore make a
  diagnostic underline visually drift in terminals. Correct handling should eventually use a
  dedicated Unicode terminal-width/grapheme policy rather than growing ad-hoc cases in the
  renderer.


## The MIR, and how it reaches codegen

- **No three-address form yet.** `MirExpr` stays tree-shaped on purpose
  (see "What's still a tree" above); this is the natural next step for
  whenever `omega-codegen` gets its own dedicated refactor, and would open
  the door to real local optimizations (CSE, constant propagation across
  statements) this MIR doesn't attempt today.
- **Block-arguments were tried and rejected as the general mechanism for
  threading an `if`/`match`'s value across its join** — a Cranelift-native
  phi-equivalent, and the more "purely Rust-MIR" choice would be a mutable
  temp local either way (Rust's own MIR has no block-argument mechanism at
  all). The block-argument version broke the moment a *sibling*
  expression built more blocks before the value was actually consumed — a
  real, reproduced bug (a stale value read back from a since-abandoned
  block), not a theoretical one — so every cross-block value in this MIR
  (an `if`/`match` join's result, the function's own return value threaded
  through its `defer` exit chain) is an ordinary local instead, with the
  fast path above recovering the common case's cost back.
- **`MirItem::Declaration`/`MirPlaceRoot::Global` are fully implemented**
  (an ordinary top-level global, `mut` included, with or without a
  compile-time-known initial value — see
  [compile-time-evaluation.md](../language/compile-time-evaluation.md)). Extern
  *data* (a non-function `extern`) is the one storage gap left, rejected by
  shared codegen preflight before backend selection — its storage lives in
  another translation unit, a genuinely separate question.

## Compile-time evaluation fuel limit

A single `comp` evaluation currently has a shared fuel budget of **1,000,000** steps across loop progress and nested calls. Exhaustion is diagnosed as runaway compile-time evaluation. This is an implementation safety limit, not a normative promise that programs below or above a particular step count must be accepted by every Omega implementation.
