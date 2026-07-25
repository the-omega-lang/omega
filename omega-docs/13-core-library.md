# The core library

`omega-core/` — Omega's real, permanent standard-library package (replaces
an earlier `examples/core` throwaway used only to prove the underlying
`for`-spec mechanism worked at all).

## Layout

```
omega-core/
  core/
    core.omg          # real root: imports every submodule, nothing else
    cmp.omg              # core::cmp — Ordering, Eq, Ord
    default.omg              # core::default — Default
    numerics.omg                # core::numerics — all scalar for-blocks (macros)
    slices.omg                     # core::slices — SliceImpl<T> for [T]
    strings.omg                       # core::strings — StrOps for str
```

`core.omg` exists solely to `import` every sibling submodule. This isn't
cosmetic: it's what makes the whole package **reachable-sweepable** as one
unit when compiled standalone (`omgc omega-core/core/core.omg --name=core
-o core.o`), and it's separately what [specs](08-specs.md)'s lazy
`for`-attachment discovery walks to find every extension method in the
package, since that discovery is a transitive import-graph walk, not a
filesystem scan.

`core` is built and linked exactly like any other `--extern` dependency —
its own **ordinary** (non-`for`) items, like `Ordering`'s own methods, are
compiled by `core`'s own `omgc` invocation and must be linked in
(`undefined reference` otherwise). `for`-attached methods are the one
exception: those are compiled by *whoever calls them*, following
`for`-attachment's own by-design "extension methods live in the using
side's TU" model (see [specs](08-specs.md) and
[modules & linkage](10-modules-and-linkage.md)'s weak-linkage section).

## API surface

- **`core::cmp`** — `Ordering` (`Less`/`Equal`/`Greater`, plus `is_lt`/
  `is_eq`/`is_gt`/`reverse`, an ordinary non-generic enum with methods —
  works fine; see [generics](06-generics.md) for why *generic* enum
  methods specifically don't). `Eq { equals; not_equals default }`. `Ord :
  Eq { compare -> Ordering; everything else defaulted off compare alone }`
  — genuinely usable from one required function, as an interface design
  should be.
- **`core::default`** — `Default { default() => Self; }`. Its own tiny
  file deliberately, for reuse beyond just numerics.
- **`core::numerics`** — three macros (`signed_integer`/
  `unsigned_integer`/`float_ops`), invoked once per concrete type (10
  integers + 2 floats), **not** one shared template copy-pasted three
  times: signed types get `abs`/`signum`/`is_negative`/`is_positive`,
  unsigned gets `is_power_of_two`, float gets `is_nan` and skips `Ord`
  entirely (NaN has no correct total order — deliberate). `min`/`max`/
  `clamp`/`pow`/`is_even`/`is_odd` are hand-written directly for speed
  rather than left to `Ord`'s default (they still satisfy `Ord`'s required
  signature). No `min_value`/`max_value` anywhere — `isize`/`usize`'s
  width is target-dependent, so a baked-in bound would silently be wrong
  on some target; cut uniformly across all twelve types rather than
  provided inconsistently.
- **`core::slices`** — `SliceImpl<T> for [T]`: `is_empty`, and `get`/
  `first`/`last` via an `(index, out: *mut T) => bool` pattern (see below
  for why, not `Option<T>`).
- **`core::strings`** — `StrOps : Eq for str`: `equals` (byte-compare
  loop — two different `*str` pointers are never automatically
  structurally equal), `is_empty`, `as_bytes` (a plain reinterpret-cast,
  `str`/`*[u8]` share the identical fat-pointer leaf layout),
  `starts_with`/`ends_with`, `contains` (naive O(n·m) substring search,
  deliberately no skip table, which would need working memory proportional
  to the needle — this layer never does hidden allocation).

## Why no `char` module, no `Option<T>`/`Result<T>`

Both cuts are direct consequences of confirmed language gaps, not missing
effort — see [generics](06-generics.md) and
[control flow](03-control-flow.md):

- **No `core::chars`** — `char` gained comparison and `match`/range
  support (see [primitives](01-primitives.md)), so ASCII
  *classification* (`is_upper`, `is_digit`, and similar range-based
  predicates) is implementable now. It still has no cast or arithmetic
  support at all, so *case conversion* (`to_upper`/`to_lower`, which needs
  to compute a different codepoint) remains blocked — a `core::chars`
  module scoped to classification-only would be straightforward to add
  when wanted, but hasn't been, to avoid shipping a half-finished module.
- **No `Option<T>`/`Result<T>`** — a generic enum with methods fails to
  even pass signature collection (two distinct confirmed bugs). The
  natural implementation (an enum with `is_some()`/`unwrap_or()`) hits this
  immediately, so `core::slices`' `(bool, out: *mut T)` pattern is used
  everywhere a "might not have a value" API is needed instead — arguably
  more embedded-idiomatic anyway (no hidden tag-copy, closer to a C/Zig
  fallible-call convention than a coincidence). Revisit `Option<T>` once
  the generic-enum-methods bug is fixed.
- **No `contains`/generic-bound methods on `core::slices`** — a spec's own
  generics don't support *per-function* bounds (`SliceImpl<T: Eq>` would
  gate the whole spec, including `is_empty`/`get`, behind `Eq` too, wrongly).

## Building it

```
just build-core     # omgc omega-core/core/core.omg --name=core -o target/core.o
just build-exe       # links against target/core.o, alongside mathlib.o
just run-exec           # cc ... -o example && ./example
```

## Caveats

Everything under [generics](06-generics.md)'s and
[control flow](03-control-flow.md)'s caveats sections that shaped a scope
cut here — this file only restates the *consequences* for `core`'s own
API surface, not the underlying compiler gaps themselves.
