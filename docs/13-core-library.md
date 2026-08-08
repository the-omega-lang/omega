# The core library

`runtime/core/` — Omega's real, permanent standard-library package (replaces
an earlier `examples/core` throwaway used only to prove the underlying
`for`-spec mechanism worked at all).

## Layout

```
runtime/core/
  core/
    core.omg          # real root: nothing to declare, see below
    cmp.omg              # core::cmp — Ordering, Eq, Ord
    default.omg              # core::default — Default
    glue.omg                   # core::glue — @gap GlobalAllocator
    hash.omg                     # core::hash — Hash
    iterator.omg                 # core::iterator — Iterator<T>, ToIterator<T>
    numerics.omg                   # core::numerics — all scalar for-blocks (macros)
    option.omg                       # core::option — Option<T>
    slices.omg                          # core::slices — SliceImpl<T> for [?]T
    strings.omg                            # core::strings — StrOps for str
```

`core.omg` has nothing left to declare — it exists only because the
compiler still needs a conventionally-named entry file to find
(`core.omg`/`core/core.omg`) when `core` is compiled standalone (`just
build-core`). It used to need an `import` of every sibling submodule, for
two reasons, both now gone:

- Making the whole package reachable when compiled standalone — the
  filesystem alone already guarantees that, unconditionally, for
  whichever package is being compiled locally (see
  [modules & linkage](10-modules-and-linkage.md)'s "Eager local
  discovery").
- Feeding [specs](08-specs.md)'s `for`-attachment discovery, which used
  to walk `core.omg`'s own import graph to find every extension method in
  the package. `core` now gets the same eager, filesystem-driven
  treatment as the local package being compiled, *regardless* of whether
  it's local or `--extern`-referenced (`ModuleRoots::core_modules`, see
  [modules & linkage](10-modules-and-linkage.md)'s "Eager local
  discovery" and "`core` as an ambient prelude") — no import graph is
  walked for this anymore, so a submodule missing from `core.omg` can no
  longer hide a `for`-block from anyone.

That same eager treatment is also what makes every name `core` exposes
resolvable with no `import core;` at all, anywhere else in a program —
see [modules & linkage](10-modules-and-linkage.md)'s "`core` as an
ambient prelude" for the full mechanism. `core`'s own files still need
ordinary imports among themselves, same as any other module — the
prelude treatment is specifically for code *outside* `core`.

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
- **`core::hash`** — `exposed spec Hash { hash(*self) => u64; }`. Lives in
  `core`, not `std`, because it isn't optional: `for`-attached specs (the
  only way to give a primitive a method at all) are hardcoded to `core`'s
  own module tree, and a target type gets exactly one `for` block,
  globally — so giving `i32`/`str`/etc. a `hash()` method means extending
  the *existing* `for`-blocks in `numerics.omg`/`strings.omg`, not adding
  a competing one. Numeric types mix their bits through a SplitMix64-style
  finalizer (`mix64`, reading `<u64>*self`); floats bit-reinterpret to
  `u64` first, normalizing `-0.0` to `0.0`'s own bit pattern before mixing
  (required for "equal values hash equal" — `-0.0 == 0.0` but they don't
  share a bit pattern). `str` uses a plain FNV-1a byte loop over
  `as_bytes()`, matching `core::strings`' own existing byte-loop style.
  `std::hash_map::HashMap<K: Hash, V>`/`std::hash_set::HashSet<T: Hash>`
  are `Hash`'s only consumers so far — see
  [the standard library](23-standard-library.md). No random seeding: the
  default hasher is deterministic, not DoS-resistant, unlike Rust's own
  SipHash default — fine for a first pass, a real gap if either type ever
  sees untrusted keys.
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
- **`core::slices`** — `SliceImpl<T> for [?]T`: `is_empty`, and `get`/
  `first`/`last` via an `(index, out: *mut T) => bool` pattern (see below
  for why, not `Option<T>`).
- **`core::strings`** — `StrOps : Eq for str`: `equals` (byte-compare
  loop — two different `*str` pointers are never automatically
  structurally equal), `is_empty`, `as_bytes` (a plain reinterpret-cast,
  `str`/`*[?]u8` share the identical fat-pointer leaf layout),
  `starts_with`/`ends_with`, `contains` (naive O(n·m) substring search,
  deliberately no skip table, which would need working memory proportional
  to the needle — this layer never does hidden allocation).
- **`core::glue`** — `@gap exposed spec GlobalAllocator { alloc; free;
  realloc; }`, the one platform capability the library needs but can't
  itself provide — see [gaps and glue](21-gaps-and-glue.md). No default
  implementation ships here; a final application supplies exactly one
  `@glue` marker implementing it, or leaves it unglued if nothing ever
  calls it.

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

## `Eq`/`Ord`/`Ordering`/`Default`/`Hash` are `exposed`, not just declared

A spec function carries no visibility modifier of its own — it always
inherits its *declaring spec's* own visibility, unlike an ordinary
struct/enum method, which does carry one. `Eq`, `Ord`, `Ordering`, and
`Default` all shipped with no modifier (hidden by default), which meant
every method `core::numerics`/`core::strings` attaches to a primitive via
these specs — `equals`, `compare`, `min`, `hash`, all of it — was
unreachable from any package other than `core` itself, cascading into
"not visible here" errors the moment, say, `42i32.hash()` was called from
a consumer. Nothing had exercised this path before (`examples/dev/main.omg`
never called any of these methods), so it was a real, previously-untested
gap, not a regression — fixed by marking all four specs, plus `Hash`
itself, `exposed`.

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
