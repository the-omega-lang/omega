# The standard library

`runtime/std/` is Omega's ordinary, portable `std` package. It builds on core
and may *declare a use* of a platform capability through core, but it does not
provide platform glue itself. Unlike core, it is never ambient: consumers
import every name they use.

## Layout

```
runtime/std/
  alloc.omg       # non-generic aligned allocation over the raw byte gap
  atomic.omg      # fixed-width atomic types over core::atomic
  default.omg     # Default
  fmt.omg         # Display and formatting helpers
  hash.omg        # Hash
  io.omg          # Read/Write, console markers, buffering, print macros
  primitives.omg  # primitive conformances for std-owned specs
  list.omg        # List<T>
  linked_list.omg # LinkedList<T>
  string.omg      # String
  hash_map.omg    # HashMap<K, V>
  hash_set.omg    # HashSet<T>
```

`std` likewise has a namespace-only root module.

Build it as a separately compiled extern package:

```sh
just build-runtime          # core, the host platform and std together
```

A consuming package registers both roots, imports the required names, and
links the objects under `target/<target>/core/` and `target/<target>/std/`. It additionally
links platform objects only for the capabilities its reachable code needs. The
standard library's objects can contain allocation or console-using functions
without forcing those glues into every final executable: per-source objects
plus per-function sections and the linker's `--gc-sections` discard what is
unused.

## Specs and primitive conformances

`core::cmp` provides `Ordering`, `Eq`, and `Ord`; `std::default` provides
`Default`; `std::hash` provides `Hash`; and `std::fmt` provides `Display`.
`std::primitives` declares these conformances for numeric scalars, `str`, `char`, and
`bool`. The corresponding inherent primitive operations remain in core.

`HashMap<K: Hash, V>` and `HashSet<T: Hash>` therefore import `std::hash`,
not core. The default hashing is deterministic: integers use a SplitMix64
style finalizer and strings use FNV-1a. It is not randomly seeded and is not
intended as a DoS-resistant hash-table default.

## Atomics

`std::atomic` provides `AtomicU8`–`AtomicU64`, `AtomicI8`–`AtomicI64`, and
`AtomicBool`, plus the three ordering types re-exported from `core::atomic` so
one import path covers both:

```omega
import std::atomic::AtomicU32;
import std::atomic::AtomicRmwOrdering;
import std::atomic::AtomicLoadOrdering;

mut counter := AtomicU32::new(0u32);
previous := counter.fetch_add(1u32, AtomicRmwOrdering::AcquireRelease);
current := counter.load(AtomicLoadOrdering::Acquire);
```

The wrappers own storage and naming only. Their methods call the matching
`core::atomic` width gap directly, so the atomicity — and whether it is
lock-free, or a lock, or an OS call — is the selected platform's, exactly as
it is for code calling the gaps itself. `AtomicI*` reaches the same width gap
through the bit-preserving unsigned operations and uses the gap's signed
`fetch_min`/`fetch_max` for comparisons; `AtomicBool` stores one byte and
exposes only the boolean-meaningful operations.

Each wrapper carries `@layout(align = sizeof<T>)`, which is what makes its
storage naturally aligned -- the alignment every atomic location is required to
have. Omega declarations are packed by default, so code calling a
`core::atomic` width gap on storage of its own must state that alignment
itself; see [the language specification](../language/atomics.md).

Mutating methods take `*mut self` and `load` takes `*self`, matching the
pointer mutability the gaps require. The backing field is not part of the
public surface: this first surface has no `get_mut` or non-atomic accessor.

`std` supplies no glue, so a program using these types links a platform that
fills the corresponding `AtomicityN` gap. Every `runtime/plat/target/`
platform fills all four widths from its architecture's own instructions; the
`runtime/plat/libc` compatibility platform fills none, because no honest
libc-only implementation of the contract exists — see
[platform glue](platform-glue.md). Semantics are specified in
[the language specification](../language/atomics.md).

## Collections and ownership

Omega has no implicit destruction. Every owning standard-library value has an
explicit `.free()`, normally paired with `defer`:

```omega
values := List<i32>::new();
defer values.free();
```

- `List<T>` is a growable contiguous allocation. Its fallible element APIs
  use an out-parameter and `bool`, matching core slices.
- `LinkedList<T>` owns independently allocated doubly linked nodes; its pops
  return `Option<T>`.
- `String` owns a growable UTF-8 byte buffer. It supports construction,
  appending `str` and `char`, inspection as `str`, clearing, and explicit
  freeing; it deliberately has no character-decoding iterator yet.
- `HashMap<K: Hash, V>` is a separate-chaining map and `HashSet<T: Hash>` is
  its `Unit`-valued wrapper. They expose the usual insertion, lookup,
  removal, iteration, and explicit-free operations.

The generic collections route heap operations through the non-generic
`std::alloc` functions, passing the alignment of the type they actually
allocate: `T` for `List<T>`, the node for `LinkedList<T>`, and the entry or
bucket element for `HashMap`. A program that constructs one needs a
`GlobalAllocator` glue implementation, but merely linking `std`'s objects does
not.

## Aligned allocation

`std::alloc` is the standard allocation family, and the only one the
collections use:

```omega
alloc(size: usize, align: usize) => *mut u8
realloc(ptr: *u8, size: usize, align: usize) => *mut u8
free(ptr: *u8) => void
```

`align` must be a nonzero power of two; the returned address is a multiple of
it, so heap storage can satisfy a type's `@layout(align)` requirement. Pass
`alignof<T>` rather than a guess.

- Invalid alignment, arithmetic overflow, and a failed underlying allocation
  all return null. There is no diagnostic for a runtime allocation failure;
  null is the API result.
- A zero-size request still returns a distinct, aligned, freeable address.
- `free(null)` does nothing.
- `realloc(null, size, align)` allocates. Otherwise it allocates the
  replacement, copies `min(old requested size, size)` bytes, and frees the old
  block, so a successful request may change the alignment and always moves the
  storage. On failure it returns null and leaves the old block untouched.
  Moving storage another execution context is accessing concurrently remains
  the caller's error: alignment does not make relocating a live atomic safe.

**Pairing.** These results must be freed with `std::alloc::free`, and a raw
`core::platform::GlobalAllocator::alloc` result must be freed with
`GlobalAllocator::free`. The two families are not interchangeable.

**Cost.** The adapter over-allocates: each block carries a small private
header immediately before the returned address plus up to `align - 1` bytes of
slack. Reallocation is always allocate-copy-free rather than an in-place
resize.

## Formatting and I/O

`std::fmt::Display` formats into a dynamic `*mut spec std::io::Write`.
`std::io` provides the byte I/O contracts, console marker implementations,
caller-owned buffering, `read_line`, `String` formatting, and the print
macros. No old broad `Writer` or `Reader` type exists. The complete API and
its exact short-transfer semantics are documented in
[console I/O](console-io.md).
