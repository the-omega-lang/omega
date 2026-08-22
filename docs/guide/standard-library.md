# The standard library

`runtime/std/` is Omega's ordinary, portable `std` package. It builds on core
and may *declare a use* of a platform capability through core, but it does not
provide platform glue itself. Unlike core, it is never ambient: consumers
import every name they use.

## Layout

```
runtime/std/
  alloc.omg       # non-generic allocator wrappers
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
just build-core
just build-std
```

A consuming package registers both roots, imports the required names, and
links `target/core.o` and `target/std.o`. It additionally links a platform
object only for the capabilities its reachable code needs. The standard
library's object can contain allocation or console-using functions without
forcing those glues into every final executable: per-function sections plus
the linker's `--gc-sections` discard unused functions.

## Specs and primitive conformances

`core::cmp` provides `Ordering`, `Eq`, and `Ord`; `std::default` provides
`Default`; `std::hash` provides `Hash`; and `std::fmt` provides `Display`.
`std::primitives` declares these conformances for numeric scalars, `str`, `char`, and
`bool`. The corresponding inherent primitive operations remain in core.

`HashMap<K: Hash, V>` and `HashSet<T: Hash>` therefore import `std::hash`,
not core. The default hashing is deterministic: integers use a SplitMix64
style finalizer and strings use FNV-1a. It is not randomly seeded and is not
intended as a DoS-resistant hash-table default.

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
`std::alloc` wrappers. A program that constructs one needs a
`GlobalAllocator` glue implementation, but merely linking `std.o` does not.

## Formatting and I/O

`std::fmt::Display` formats into a dynamic `*mut spec std::io::Write`.
`std::io` provides the byte I/O contracts, console marker implementations,
caller-owned buffering, `read_line`, `String` formatting, and the print
macros. No old broad `Writer` or `Reader` type exists. The complete API and
its exact short-transfer semantics are documented in
[console I/O](console-io.md).
