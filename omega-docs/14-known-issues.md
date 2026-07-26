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

## Enums

- **`e.tag`/header fields are write-protected only against plain `=`** — a
  compound assignment (`e.tag += 1`) or a write through `&mut e.tag` both
  silently bypass the check that correctly rejects `e.tag = 5`, corrupting
  a live enum's tag with no cast needed. [design-review.md](15-design-review.md)

## Visibility

- **`&hidden base[range]`/`&hidden [array literal]` silently drop the
  `hidden` bypass** — the plain, non-sliced form (`&hidden base`) works; a
  third, unfixed occurrence of the same "position-dependent activation"
  bug class already described in [visibility.md](07-visibility.md).
  [design-review.md](15-design-review.md)

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

## Compiler internals

Shape problems in `omega-driver` that work today but each need a breaking
change to fix — full writeups in
[design-review.md](15-design-review.md#compiler-architecture-omega-driver).

- **Overloading needs a whole parallel item pipeline** (two extra caches,
  two extra sweeps, two extra resolver methods) purely because the item
  query key can't name one candidate of an overload group — which also
  makes generic overloads structurally impossible.
- **Two independent pending-spec-method queues** that differ only in
  whether the owner has a declared item to key on.
- **`core` is hardcoded as the only place a `for` block may be declared**,
  so no third-party package can ship extension methods.
- **`ResolveError::Cycle` carries a chain it never populates** — it always
  prints one module, so the rendered message implies a cycle it never
  shows.
- **Module paths and item paths are the same untyped `Vec<Ident>`**, so
  nothing prevents confusing the two.
- **Diagnostic scoping for scanned (extern/`core`) modules is three ad-hoc
  lists** with four different outcomes and no stated policy.

## Design debt worth watching

- Every new `Expression`/`HirExpr`/`CheckedExpr` variant needs updates
  across up to five separate exhaustive matches spread over multiple
  crates (macro expansion, prelude re-exports, HIR lowering, defer-id
  collection, codegen emission) — the compiler catches every miss as a
  hard exhaustiveness error, so nothing is silently skipped, but budget
  for it when adding new expression forms.
