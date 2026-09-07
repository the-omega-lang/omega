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

## Inline `asm` cannot pin a multi-byte AVR operand

An unpinned `reg(ptr)` is fine on AVR: pointer-like leaves use LLVM's pointer
class `e`, so the backend allocates a whole `X`/`Y`/`Z` pair and `$ptr` prints
as that pair's name, which is what `ld`/`st` accept. Naming a register for a
value wider than one byte does not work:

- `reg(ptr, "Z")` and `reg(ptr, "r31:r30")` are rejected. `reg`'s second
  argument always lowers to a named-register constraint `{...}`, and LLVM's AVR
  backend has no register of either name; only the bare constraint letters
  `x`/`y`/`z` select a specific pair, and `reg` cannot spell those.
- `reg(ptr, "r30")` is *accepted* and silently truncates: the backend emits
  `mov r30, r24` and never writes `r31`, so the body addresses through a
  half-initialized pointer. This is the dangerous form, because it compiles and
  only corrupts memory at run time.

So a body needing one particular pair cannot be written: `icall`/`ijmp` and
`lpm` require `Z` specifically. The same truncation applies to a 16-bit
non-pointer operand, which takes the generic `r` class and therefore prints
only the low half of the pair it was given, so `u16`/`i16` cannot be used as
16-bit asm operands at all.

Until `reg` can express these, do not name a register for an AVR operand wider
than a byte, and do not pass a `u16`/`i16` into an AVR body.

## A libc-free `x86_64-windows` link cannot resolve `_fltused`

LLVM emits an undefined reference to `_fltused` from any MSVC-target module
that uses floating point; the definition normally comes from the C runtime,
which this platform deliberately does not link. Three objects in a normal build
carry the reference:

```
target/x86_64-windows/std/fmt.obj
target/x86_64-windows/std/primitives.obj
target/x86_64-windows/core/primitives/numerics.obj
```

Nothing in the repository defines the symbol, so the link command documented in
[`../guide/platform-glue.md`](../guide/platform-glue.md) fails for any program
reaching one of those objects — which `std::fmt` means is most of them, not
only programs that use floats themselves. `aarch64-windows` is unaffected;
`_fltused` is an x86 MSVC convention.

It cannot currently be fixed in Omega source. `@mangling` is rejected on a
`global-declaration` (*"this item can't carry annotations"*), and a `foreign`
binding is a declaration that takes no initializer, so neither of the two ways
to name an exact linker symbol can also *define* one holding data. Resolving it
means picking one of: allowing `@mangling(force = "...")` on globals, defining
`_fltused` in the backend for MSVC x86 targets the way the CRT would, or
shipping a hand-written object in the Windows platform.

`bin/check-platform` does not catch it because its Windows checks scan only
`<target>/plat` objects, never `core` or `std`.

