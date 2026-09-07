# Compiler implementation limitations

Implementation caveats migrated out of architecture chapters. These are non-normative and should be removed when resolved.


## Parsing, macro expansion, and the HIR

- **Recovery is grammar-aware but still line-agnostic.** `parser/recovery.rs`
  resynchronizes using the same lookahead `parse_item`/`parse_statement_content`
  dispatch on, never consumes an enclosing block's closing brace, and always
  makes forward progress. It still has no notion of indentation or line
  structure, so an error deep inside a malformed nested expression can skip
  further than a human would.
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
  [compile-time-evaluation.md](../language/compile-time-evaluation.md)).
  `MirItem::ForeignBinding` for non-function data is also implemented: a
  real linker-visible external global with no initializer/local section, its
  storage genuinely living in another translation unit.

## Compile-time evaluation fuel limit

A single `comp` evaluation currently has a shared fuel budget of **1,000,000** steps across loop progress and nested calls. Exhaustion is diagnosed as runaway compile-time evaluation. This is an implementation safety limit, not a normative promise that programs below or above a particular step count must be accepted by every Omega implementation.

## AVR object emission needs an optimized build

`avr-none` cannot emit `runtime/core` or `runtime/std` at `-O0`: LLVM's AVR
backend reports *"ran out of registers during register allocation"* for
functions whose unoptimized lowering keeps too many 32- and 64-bit values
live at once (`i64::abs`, `std::fmt`'s byte writer). The same sources emit
cleanly at `-O1` and above, so the repository's AVR matrix build compiles that
target optimized while every other target still builds at the default `-O0`.

This is a backend register-allocation limitation on a 32-register 8-bit
machine, not an Omega semantic difference: nothing in the language says a
target's object emission may depend on the optimization level, and the
restriction should disappear rather than become a documented rule.

## Inline `asm` cannot name an AVR pointer register pair

An AVR data-space access from inline assembly (`ld`/`st`) requires one of the
pointer register pairs `X`/`Y`/`Z`, but `reg` cannot deliver one:

- `reg(ptr)` allocates a pair and prints its **low** half (`r24`), so
  `st $ptr, r18` is not valid assembly;
- `reg(ptr, "X")`, `"Z"` and `"r31:r30"` are rejected — LLVM's AVR backend has
  no such named register for an inline-asm constraint;
- `reg(ptr, "r26")` is *accepted* and silently truncates: the backend emits
  `mov r26, r24` and never writes `r27`, so `st X, r18` stores through a
  half-initialized address.

The last form is the dangerous one, because it compiles and only corrupts
memory at run time. Until `reg` can express a pointer-register-pair class
(the LLVM `e`/`x`/`y`/`z` constraints, or an operand modifier that prints a
pair as `X`/`Y`/`Z`), an AVR `asm` body must not be handed a pointer, and no
AVR platform code may pass one.
