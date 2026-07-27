# The core library

`runtime/core/` — Omega's real, permanent standard-library package (replaces
an earlier `examples/core` throwaway used only to prove the underlying
`for`-spec mechanism worked at all).

## Layout

```
runtime/core/
  core/
    core.omg          # real root: imports every submodule, nothing else
    cmp.omg              # core::cmp — Ordering, Eq, Ord
    default.omg              # core::default — Default
    iterator.omg               # core::iterator — Iterator<T>, ToIterator<T>
    numerics.omg                # core::numerics — all scalar for-blocks (macros)
    option.omg                     # core::option — Option<T>
    slices.omg                        # core::slices — SliceImpl<T> for [T]
    strings.omg                          # core::strings — StrOps for str
```

`core.omg` exists solely to `import` every sibling submodule. This isn't
cosmetic: it's what makes the whole package **reachable-sweepable** as one
unit when compiled standalone (`omgc runtime/core/core/core.omg --name=core
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
- **`core::option`** — `Option<T> { None, Some { exposed value: T; }; }`.
  Real, ordinary generic enum — see "`Option<T>` finally exists" below for
  why it's here now, and why its variant order (`None` = 0, `Some` = 1) is
  load-bearing, not incidental.
- **`core::iterator`** — `Iterator<T> { next(*mut self) => Option<T>; }`
  and `ToIterator<T> { to_iterator(*self) => spec *mut Iterator<T>; }` —
  the protocol `for <binding> in <iterator> { }` is built on. See
  [for-in loops](18-for-in-loops.md).
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
  provided inconsistently. Its macro bodies still explicitly cast every
  bare literal compared or combined with `*self` (`<Self>0`, not `0`) —
  written before [binary-op literal narrowing](03-control-flow.md) was
  fixed, when this was strictly required rather than just harmless/
  explicit; left as-is (still correct, just no longer the only way to
  write it) rather than churned for its own sake.
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

## `Option<T>` finally exists — but `core::slices` still doesn't use it

`Option<T>`'s blockers (a generic enum with methods failing signature
collection; `T` not deducible from a generic-enum-typed argument) were
fixed independently of any real need for `Option<T>` itself — it stayed
out of `core` on scope grounds alone until `for <binding> in <iterator>
{ }` (see [for-in loops](18-for-in-loops.md)) needed a real "maybe a
value" return type for `Iterator<T>::next`, at which point adding it
stopped being optional. It's the plainest possible shape (`None`, `Some {
value: T }`, no methods) — deliberately not extended with `is_some`/
`unwrap_or`-style conveniences in the same pass that added it, to avoid
piling unrelated scope onto a change driven by one specific need.

`core::slices`' own `(index, out: *mut T) => bool` pattern (`get`/`first`/
`last`) is **not** being migrated to return `Option<T>` — both shapes now
coexist deliberately: the out-pointer form avoids a hidden tag-copy and
reads as a closer match to a C/Zig fallible-call convention, which is
still the better fit for a hot, no-allocation slice-indexing path;
`Option<T>` is the better fit for a one-shot "maybe produced a value"
result like an iterator step, which was never going to be `#[inline]`-hot
the same way `get` is. Picking one uniformly across `core` would have
been consistency for its own sake at a real ergonomics/performance cost
somewhere.

## No `char` module yet

`char` gained comparison and `match`/range support (see
[primitives](01-primitives.md)), so ASCII *classification* (`is_upper`,
`is_digit`, and similar range-based predicates) is implementable now.
`char` also now has arithmetic (see [primitives](01-primitives.md)'s
"`char`, `bool`, and pointer arithmetic"), which unblocks ASCII *case
conversion* too: `to_upper`/`to_lower` can compute the shifted codepoint
as a `u32`, truncate to `u8` (every ASCII letter fits), then cast the
`u8` back to `char` (the one direction guaranteed valid). Full Unicode
case conversion is still blocked — a shifted codepoint outside ASCII
doesn't fit in a `u8`, and there is still no general integer-to-`char`
cast (see [known-issues.md](14-known-issues.md)). A `core::chars` module
scoped to ASCII (classification and case conversion both) would be
straightforward to add when wanted, but hasn't been, to avoid shipping a
half-finished module.

## No `contains`/generic-bound methods on `core::slices`

A spec's own generics don't support *per-function* bounds (`SliceImpl<T:
Eq>` would gate the whole spec, including `is_empty`/`get`, behind `Eq`
too, wrongly).

## Building it

```
just build-core     # omgc runtime/core/core/core.omg --name=core -o target/core.o
just build-exe       # links against target/core.o, alongside mathlib.o
just run-exec           # cc ... -o example && ./example
```

## Caveats

Everything under [generics](06-generics.md)'s and
[control flow](03-control-flow.md)'s caveats sections that shaped a scope
cut here — this file only restates the *consequences* for `core`'s own
API surface, not the underlying compiler gaps themselves.
