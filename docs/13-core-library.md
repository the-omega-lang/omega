# The core library

`runtime/core/` is Omega's minimal, permanently available library package. It
contains data representations and allocation-free operations that every target
can support. It does not own comparison, hashing, formatting, buffered I/O, or
console policy; those belong to `std`.

## Layout

```
runtime/core/
  iterator.omg   # Iterator<T>, ToIterator<T>
  numerics.omg   # inherent scalar operations
  option.omg     # Option<T>
  platform.omg   # allocator and console capability gaps
  slices.omg     # inherent generic-slice operations
  strings.omg    # inherent str operations
```

`core` has no root source file: its package root is a namespace for these
child modules.

`core` is the one ambient package. Code outside it may name exposed core items
without importing `core`; files *inside* it still use ordinary imports. The
compiler discovers all of core's modules whether core is built locally or
registered as an extern package. It is otherwise an ordinary separately
compiled object: definitions from `core.o` must be linked when used.

## What core owns

- **`core::option::Option<T>`** is the simple `None` / `Some { value: T }`
  result used when a value may be absent.
- **`core::iterator`** defines `Iterator<T>::next(*mut self) => Option<T>`
  and `ToIterator<T>`, the protocols behind `for`.
- **`core::numerics`** supplies only inherent scalar operations: `clamp`,
  `pow`, `abs`, `signum`, `is_even`/`is_odd`, `is_negative`/`is_positive`,
  `is_power_of_two`, and `is_nan`. It does not attach comparison, defaulting,
  hash, or formatting conformances — `min`/`max` in particular are `Ord`
  methods and live in `std::cmp`.
- **`core::slices`** supplies inherent operations on `*[?]T`, including
  `is_empty` and bounds-checked out-parameter access. The out-parameter/
  `bool` form is intentional for this hot, allocation-free API; it is not a
  replacement for `Option<T>` everywhere.
- **`core::strings`** supplies inherent byte-oriented `str` operations:
  `is_empty`, `as_bytes`, `starts_with`, `ends_with`, and `contains`.
  Equality, hashing, and display of `str` are standard-library
  conformances.

## Platform capabilities

`core::platform` only declares capabilities. A final program or platform
package supplies exactly one matching `glue` block when a reachable path needs
one.

```omega
gap GlobalAllocator {
    alloc(size: usize) => *mut u8;
    free(ptr: *u8) => void;
    realloc(ptr: *u8, size: usize) => *mut u8;
}

gap StandardOutput { write(bytes: *[?]u8) => Option<usize>; }
gap StandardError  { write(bytes: *[?]u8) => Option<usize>; }
gap StandardInput  { read(into: *mut [?]u8) => Option<usize>; }
```

For the console gaps, `None` means failure and `Some(n)` is the exact number
of bytes transferred. In particular, `Some(0)` is valid: it can mean EOF on
input or a zero-progress write. Core declares these names but does not call
them. That keeps a core-only program freestanding and free of console or
allocator glue requirements.

## Deliberate boundary with `std`

`std::cmp` owns `Ordering`, `Eq`, and `Ord`; `std::default` owns `Default`;
`std::hash` owns `Hash`; `std::fmt` owns `Display` and formatting helpers; and
`std::io` owns `Write`, `Read`, buffering, console marker types, and printing.
The primitive conformances for all of those specs also live in `std`. Core may
still be the only package allowed to add *inherent* primitive methods, while
the ordinary compose orphan rule decides where a spec conformance may be
declared.

Consequently, a core-only package cannot name `Display`, `Write`, `Hash`, or
`Ord` unless it explicitly links and imports the relevant `std` module. This
is intentional: core remains useful on a target with neither heap nor
console.

## Building and linking

```
just build-core
```

The compiler emits each function into its own object-file section. Link with
`--gc-sections` (the repository's `just` recipes do) so unused sections from a
separately compiled package do not make a capability reachable merely because
another function in the same object uses it. A core-only executable therefore
links with `core.o` and no `plat.o`; it provides no console or allocator glue.

See [the standard library](23-standard-library.md) for the higher-level
facilities and [gaps and glue](21-gaps-and-glue.md) for the capability model.
