# The standard library

`runtime/std/` — Omega's first real collection of data structures, built
entirely on top of `core::glue::GlobalAllocator`. Unlike `plat`, `std`'s
code is the same on every platform; it only ever needs a *glue* to be
provided, never a per-platform reimplementation of its own.

## Layout

```
runtime/std/
  std/
    std.omg            # real root: nothing to declare, same reason as core.omg
    alloc.omg              # std::alloc — non-generic GlobalAllocator wrappers
    list.omg                  # std::list — List<T>, ListIterator<T>
    linked_list.omg              # std::linked_list — LinkedList<T>, LinkedListIterator<T>
    string.omg                      # std::string — String
    hash_map.omg                       # std::hash_map — HashMap<K,V>, KeyValue<K,V>, HashMapIterator<K,V>
    hash_set.omg                          # std::hash_set — HashSet<T>, HashSetIterator<T>
```

`std` is an **ordinary `--extern` package** — no ambient-prelude/eager-
discovery privilege (that treatment is hardcoded to `core` specifically;
`std` gets none of it, exactly like `plat` — see
[the core library](13-core-library.md) and
[modules & linkage](10-modules-and-linkage.md)). Every consumer writes
explicit imports: `import std::list::List;`, `import std::hash_map::HashMap;`,
and so on.

```
./target/debug/omgc runtime/std/ --name=std --extern=core:runtime/core/ -o target/std.o
```

No `just build-std` recipe exists yet (the `justfile`'s own recipes only
cover `core`/`plat`/the single `examples/dev` demo today) — this is the
same `omgc` invocation shape `build-plat` already uses for `plat`,
adapted for `std`, not yet wired into the `justfile` itself.

## No RAII — every owning type has an explicit `.free()`

Omega has no `Drop`-equivalent; `defer` is its only structured cleanup
mechanism (see [control flow](03-control-flow.md)). Every owning type in
`std` follows the same idiom, with no exceptions:

```
list := List<i32>::new();
defer list.free();
```

Forgetting the `defer` silently leaks — there is no leak detector
anywhere in this toolchain. This is a foundational decision for the
*whole* standard library going forward, not something re-litigated per
type.

## `std::alloc`: why generic collections don't call `GlobalAllocator` directly

Every generic collection routes its allocator calls through
`std::alloc::{alloc, free, realloc}` — plain, non-generic wrapper
functions — rather than calling `core::glue::GlobalAllocator`'s `@gap`
methods straight from a generic method body. See that file's own header
comment for the full story: originally a workaround for a real codegen
bug (generic instantiations whose template lived in an extern package
were silently dropped — now fixed, see
[modules & linkage](10-modules-and-linkage.md)'s "Fixed: two bugs found
building `std`..." section), the indirection is kept regardless because
removing it empirically made gap-reachability *worse*, not better —
`std.o` is compiled as one whole-package translation unit ahead of any
specific consumer, so an internal dependency like `String` → `List<u8>`
bakes a `GlobalAllocator` reference into `std.o` itself either way; the
wrapper functions keep that reference to exactly three symbols instead of
one per generic collection × instantiation.

## API surface

- **`std::list::List<T>`** — a growable, heap-backed contiguous array
  (Rust's `Vec<T>`, under a name that doesn't read as "this is actually a
  vector" the way `Vec` does outside a math/graphics context).
  `new`/`with_capacity`/`len`/`capacity`/`is_empty`/`push`/`pop`/`get`/
  `set`/`clear`/`as_slice`/`free`. Doubling growth (starting at 4),
  explicitly branching `alloc` (first allocation) vs. `realloc` (every
  later growth) rather than relying on `realloc(NULL, size) ==
  malloc(size)` — a real property of libc's own `realloc` that
  `core::glue::GlobalAllocator` itself never promises, and this library
  is meant to work with glues nobody's written yet. Fallible access
  (`get`/`set`/`pop`) mirrors `core::slices::SliceImpl<T>`'s own
  out-pointer/`bool` convention exactly, rather than `Option<T>` — it
  *is* the growable version of that same hot-path, no-allocation-on-read
  concept, and should feel like a drop-in extension of methods a caller
  already knows from a plain `*[T]`. `as_slice` is the first real
  consumer of `raw_slice<T>` (see
  [strings, casting & slices](11-strings-casting-and-slices.md)).
- **`std::linked_list::LinkedList<T>`** — doubly linked, each element its
  own individual heap allocation (`Node<T>`, `prev`/`next`) — the classic
  space/locality-for-O(1)-splice tradeoff against `List<T>`'s one
  contiguous buffer. `new`/`len`/`is_empty`/`push_back`/`push_front`/
  `pop_back`/`pop_front`/`clear`/`free`. `pop_back`/`pop_front` return
  `Option<T>`, not an out-pointer — no existing Omega precedent to match
  the way `List<T>` had `core::slices`, so this follows Rust's own
  `VecDeque`/`LinkedList` shape instead.
- **`std::string::String`** — an owned, growable UTF-8 byte buffer,
  wrapping `std::list::List<u8>` directly rather than duplicating
  growth/allocation logic (mirrors Rust's own `struct String { vec:
  Vec<u8> }`). `new`/`with_capacity`/`from_str`/`len`/`is_empty`/
  `push_str`/`push(c: char)`/`as_str`/`clear`/`free`. `push` hand-encodes
  a `char` (a full 4-byte Unicode scalar) into 1-4 UTF-8 bytes — the
  standard shift/mask encoding, one `self.bytes.push(byte)` per output
  byte. `as_str` is `List<u8>::as_slice()` reinterpret-cast to `*str` —
  the same already-existing fat→fat cast `core::strings` itself relies
  on, so no second `raw_slice<T>` call is needed. **Deliberately out of
  scope**: decoding — a `chars()`-style iterator walking variable-width
  UTF-8 back into `char`s. `push`/`push_str`/`as_str`/`len` is enough
  surface for real use today; `core` itself shipped `Option<T>` with zero
  convenience methods in its own first pass for the identical reason (see
  [the core library](13-core-library.md)).
- **`std::hash_map::HashMap<K: Hash, V>`** — separate-chaining hash map.
  `new`/`len`/`is_empty`/`insert`/`get`/`contains_key`/`remove`/`clear`/
  `free`, plus `KeyValue<K, V> { exposed key; exposed value; }` as
  iteration's own output shape (deliberately decoupled from `Entry<K,
  V>`'s internal chaining representation, so the storage strategy could
  change later without changing what iterating produces). Bucket index is
  plain `key.hash() % self.bucket_count` (modulo, not power-of-two
  masking — no invariant to maintain across a resize). Resizes (doubling
  `bucket_count`, relinking every entry in place with no entry itself
  reallocated) once `(count + 1) * 4 > bucket_count * 3` — a 75% load
  factor, the same figure Rust's own `std::collections::HashMap` targets.
  `insert`/`get`/`remove` all return `Option<V>` — again no existing
  precedent to match, so this follows Rust's own `HashMap` shape.
  Iterating copies each entry's key/value into a fresh `KeyValue<K, V>`
  per step (Rust's own `iter()` yields references instead; accepted here
  as a known, deliberate simplification — Omega has more copy-happy value
  semantics and no borrow checker to make a reference-yielding iterator
  sound).
- **`std::hash_set::HashSet<T: Hash>`** — a thin wrapper around
  `HashMap<T, Unit>` (`Unit` a real, zero-sized `marker`-declared
  placeholder, see [marker types](20-marker-types.md)) — the identical
  trick Rust's own real `HashSet<T> = HashMap<T, ()>` uses internally,
  and genuinely free here too. `new`/`len`/`is_empty`/`insert`/
  `contains`/`remove`/`clear`/`free`. `insert`/`remove`/`contains` return
  plain `bool`, not `Option<T>` — unlike `HashMap<K, V>`, there's no
  payload to hand back beyond presence itself (the caller already holds
  the value), matching Rust's own real `HashSet` API exactly.

Every `ToIterator<T>` implementation above (`List`, `LinkedList`,
`HashMap` via `KeyValue<K, V>`, `HashSet`) uses the same pointer/cursor
shape `core`'s own iterator examples already establish — `for x in
my_list { }` needs no new mechanism at all. See
[`for` .. `in` loops](18-for-in-loops.md).

## `core::hash::Hash` — the one new `core` dependency

`HashMap<K: Hash, V>`/`HashSet<T: Hash>` are the first real consumers of
`core::hash::Hash`. It has to live in `core`, not here, because
`for`-attached specs (the only way to give a primitive a method at all)
are hardcoded to `core`'s own module tree — see
[the core library](13-core-library.md)'s own `core::hash` section for the
full mechanism and its primitive implementations.

## Gap reachability: an honest characteristic, not a broken guarantee

The rule that a glue is only required when the code path needing it is
actually reached (see [gaps and glue](21-gaps-and-glue.md)) still holds
exactly as before — nothing in `std` introduces a new gap or changes how
linking decides `UnfilledGap` vs. a hard error. The one thing worth
being explicit about: **every current `std` type happens to need
allocation** (`List`, `LinkedList`, `String`, `HashMap`, `HashSet` all
allocate on construction or first growth), so no program that actually
constructs any of them today can avoid requiring
`core::glue::GlobalAllocator`'s glue. That's a property of the current
five types, not a violated guarantee — a hypothetical future
allocation-free `std` type (a fixed-capacity ring buffer over caller-
supplied storage, say) would still link with no glue required at all if
never reached, exactly per the rule.

## Building it

```
just build-core                                                                  # std depends on core
./target/debug/omgc runtime/std/ --name=std --extern=core:runtime/core/ -o target/std.o
```

A consumer needs both `--extern=core:...` and `--extern=std:...`, plus,
if it ever actually reaches an allocation, a `@glue` implementing
`GlobalAllocator` (`plat`'s `LibcAllocator`, today — see
[`plat`: the default platform `@glue`](22-platform-glue.md) — or any
other).

## Caveats

- **No random seeding on the default hasher** (`core::hash::Hash`'s
  SplitMix64-style integer mixer, FNV-1a for `str`) — deterministic, not
  DoS-resistant, unlike Rust's own SipHash default. Fine for a first
  pass; a real gap if `HashMap`/`HashSet` ever see untrusted keys.
- **No char-decoding iterator on `String`** — `push`/`as_str` only, no
  `chars()`. A natural, separable follow-up, deliberately not bundled in
  here.
- Two real, narrow parser bugs were found (not fixed) while writing this
  package — see [known issues](14-known-issues.md)'s `## Parser` section,
  [control flow](03-control-flow.md), and
  [strings, casting & slices](11-strings-casting-and-slices.md) for the
  full writeups and the workarounds used throughout these files.
- `Result<T, E>` still doesn't exist anywhere in this language. Nothing
  here needed it — every fallible operation above is `Option<T>`, `bool`,
  or an out-pointer/`bool` pair — so it isn't added speculatively; a
  likely near-future addition once something genuinely needs a
  payload-bearing error, not before.
