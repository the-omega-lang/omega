# Known issues tracker

A single consolidated list of every confirmed, currently-unfixed gap
described in these docs, for tracking at a glance. Each entry links to its
full writeup. Update this file whenever a gap here is fixed (move it to a
"Fixed" note in the relevant topic file, don't just delete the line) or a
new one is found.

## Codegen

- **Variadic `f64` from a function parameter prints `0.0`.** Any `-O`
  level. [primitives.md](01-primitives.md)
- **Variadic `f64` read via an enum body-field projection prints garbage.**
  Only under `-O1` and above. [primitives.md](01-primitives.md)
- **No real C-ABI aggregate-passing convention** — structs/enums pass as
  flattened positional scalars, fine Omega-to-Omega, not safely callable
  from hand-written C expecting real struct-passing rules.
  [primitives.md](01-primitives.md)
- **Object files for byte-identical source aren't reproducible
  build-to-build** — spec-default-method `$$N` suffixes depend on
  `HashMap` iteration order. Harmless within one compilation.
  [modules-and-linkage.md](10-modules-and-linkage.md)

## Generics

- **A spec's own generics can't forward into a dependency's type args**
  (`spec Foo<T> : Bar<T>`). A narrower, cleanly-scoped, understood gap
  (fix direction documented) — not a bug in the sense the two items below
  were. [generics.md](06-generics.md), [specs.md](08-specs.md)
- **Untyped literals don't reliably narrow across integer widths** outside
  a function's own tail-return position. A separate subsystem from
  generics (reproduces with no generics involved at all) — only reads as
  a generics issue because `Self`-typed code is where it's most visible.
  [generics.md](06-generics.md)

## Modules & imports

- **A cross-module, mutually-by-value struct cycle through a bare import
  alias silently compiles** instead of being rejected as infinite-size.
  Confirmed pre-existing. [modules-and-linkage.md](10-modules-and-linkage.md)

## Types

- **`*str` is not actually guaranteed valid UTF-8** — casting between
  `*str` and `*[u8]`/`*[i8]` is unsound in both directions, no validation.
  Deliberately deferred pending a `core`-provided validating conversion.
  [strings-casting-and-slices.md](11-strings-casting-and-slices.md)
- **`char` still has no arithmetic, bitwise, or cast support** —
  deliberately: nothing validates that a computed codepoint is still a
  legal Unicode scalar value, so allowing e.g. `'a' + 1` could silently
  produce an invalid `char`. Comparison and `match`/range support were
  added (fixed — see [primitives.md](01-primitives.md)); a real,
  validating path for arithmetic (e.g. a fallible `char::from_u32`-style
  constructor) is left as deliberate future work, not solved narrowly
  here. [primitives.md](01-primitives.md), [core-library.md](13-core-library.md)
- **`bool` has no `== != & | ^`, and there is no `!` operator** — by
  design, not a gap, but worth knowing before reaching for it.
  [control-flow.md](03-control-flow.md)

## Specs

- **Coercion into `spec *T` isn't wired into every expression position**
  (struct-literal fields, array-literal elements, bare tail-return without
  `return` are missing). [specs.md](08-specs.md)
- **No `is_variadic` support on spec functions.** [specs.md](08-specs.md)

## Visibility

- **No re-export / `pub use`-equivalent.** Matches the language having no
  re-export concept at all today. [visibility.md](07-visibility.md)

## Design debt worth watching

- Every new `Expression`/`HirExpr`/`CheckedExpr` variant needs updates
  across up to five separate exhaustive matches spread over multiple
  crates (macro expansion, prelude re-exports, HIR lowering, defer-id
  collection, codegen emission) — the compiler catches every miss as a
  hard exhaustiveness error, so nothing is silently skipped, but budget
  for it when adding new expression forms.
