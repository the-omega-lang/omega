# The core library

`runtime/core/` is Omega's minimal, permanently available library package. It
contains data representations and allocation-free operations that every target
can support. It does not own comparison, hashing, formatting, buffered I/O, or
console policy; those belong to `std`.

## Layout

```
runtime/core/
  atomic.omg     # ordering types and the AtomicityN capability gaps
  builtins.omg   # file$, line$, column$ -- compiler-implemented
  cmp.omg        # Ordering, Eq, Ord
  iterator.omg   # Iterator<T>, ToIterator<T>
  option.omg     # Option<T>
  panic.omg      # PanicInfo, PanicHandler gap, panic$
  platform.omg   # allocator and console capability gaps
  primitives/    # the built-in type declarations (see below)
  range.omg      # Range<T>, RangeIterator<T>, Successor, Bounded
  result.omg     # Result<T, E>
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
- **`core::result::Result<T, E>`** is the ordinary `Ok { value: T }` /
  `Err { error: E }` enum used when a failure carries a reason. Together with
  `Option<T>` it is what the postfix `?` operator propagates.
- **`core::cmp`** defines `Ordering`, `Eq`, and `Ord`; primitive integer and
  `str` conformances live in core because range iteration depends on them.
- **`core::iterator`** defines `Iterator<T>::next(*mut self) => Option<T>`
  and `ToIterator<T>`, the protocols behind `for`.
- **`core::atomic`** owns the three ordering types and the per-width atomic
  capability gaps. See "Atomic operations" below.
- **`core::panic`** owns `PanicInfo`, the `PanicHandler` gap, and the `panic$`
  macro; **`core::builtins`** owns the compiler-implemented `file$`, `line$`,
  and `column$` source-location macros. See "Panicking" below.
- **`core::primitives`** is where every built-in type is declared. A
  `primitive` block is a type's declaration site, not merely somewhere to hang
  methods on it, so **every** built-in has one — including `void` and `never`,
  whose blocks are empty because neither has a value to call a method on.
  Reading this module answers "which types does this language have" without
  anyone opening the compiler:

  | Module | Declares |
  |---|---|
  | `core::primitives::numerics` | `i8`–`i64`/`isize`, `u8`–`u64`/`usize`, `f32`/`f64` |
  | `core::primitives::strings` | `str` |
  | `core::primitives::slices` | `[]T` |
  | `core::primitives::chars` | `char` |
  | `core::primitives::booleans` | `bool` |
  | `core::primitives::valueless` | `void`, `never` |

  Type *constructors* (`*T`, `[N]T`, `[?]T`) have no block, since they are not
  single types; `[]T`'s generic block is the one exception. Note the deliberate
  mirror with `std::primitives`: `core::primitives` **declares** the built-ins,
  `std::primitives` **conforms** them to the specs `std` owns.
- **`core::primitives::numerics`** supplies only inherent scalar operations: `clamp`,
  `pow`, `abs`, `signum`, `is_even`/`is_odd`, `is_negative`/`is_positive`,
  `is_power_of_two`, and `is_nan`. It does not attach comparison, defaulting,
  hash, or formatting conformances — `min`/`max` in particular are `Ord`
  methods and live in `core::cmp`.
- **`core::primitives::slices`** supplies inherent operations on `*[]T`, including
  `is_empty` and bounds-checked out-parameter access. The out-parameter/
  `bool` form is intentional for this hot, allocation-free API; it is not a
  replacement for `Option<T>` everywhere.
- **`core::primitives::strings`** supplies inherent byte-oriented `str` operations:
  `is_empty`, `as_bytes`, `starts_with`, `ends_with`, and `contains`.
  Equality, hashing, and display of `str` are standard-library
  conformances.
- **`core::primitives::chars`** supplies `from_u32`, ASCII classifiers/case
  mapping, UTF-8 encoded length, and the `Ord`/`Successor`/`Bounded`
  conformances used by generic range iteration. The alphabetic and whitespace
  classifiers are intentionally ASCII-only; Unicode tables do not belong in
  freestanding core.

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

gap StandardOutput { write(bytes: *[]u8) => Option<usize>; }
gap StandardError  { write(bytes: *[]u8) => Option<usize>; }
gap StandardInput  { read(into: *mut []u8) => Option<usize>; }
```

For the console gaps, `None` means failure and `Some(n)` is the exact number
of bytes transferred. In particular, `Some(0)` is valid: it can mean EOF on
input or a zero-progress write. Core declares these names but does not call
them. That keeps a core-only program freestanding and free of console or
allocator glue requirements.

## Atomic operations

`core::atomic` declares atomicity the same way core declares any other
platform capability: one gap per storage width, filled by the platform.

```omega
mut slot: u32 = 0;

Atomicity32::store(&mut slot, 7u32, AtomicStoreOrdering::Release);
previous := Atomicity32::exchange(&mut slot, 9u32, AtomicRmwOrdering::AcquireRelease);
current := Atomicity32::load(&slot, AtomicLoadOrdering::Acquire);
```

`Atomicity8`, `Atomicity16`, `Atomicity32`, and `Atomicity64` each provide
`load`, `store`, `exchange`, `compare_exchange`, `compare_exchange_weak`,
`fetch_add`, `fetch_sub`, `fetch_and`, `fetch_or`, `fetch_xor`, and both an
unsigned and a signed `fetch_min`/`fetch_max`. Signed comparison is a separate
member taking the signed pointer/value type, so it is never inferred from an
unsigned bit pattern.

Three points about using them:

- These are raw operation-level atomics on ordinary storage. A wrapper type is
  a convenience, not the source of atomicity — `std::atomic` is built on
  exactly these calls.
- Only `load` takes `*T`; everything that can modify the location takes
  `*mut T`. Atomicity is not a way to write through an immutable binding, so
  callers supply mutable storage.
- Each operation category has its own ordering type
  (`AtomicLoadOrdering`, `AtomicStoreOrdering`, `AtomicRmwOrdering`), and a
  mismatched ordering is an ordinary compile-time type error rather than a
  runtime check.

Every `fetch_*` and `exchange` returns the value the location held before the
modification; `compare_exchange` returns `Result<T, T>` — `Ok` with the
previous value, or `Err` with the value actually observed. The full contract,
including what concurrent access is excluded, is in
[the language specification](../language/atomics.md).

Nothing here promises lock-freedom. A platform may implement a width with a
lock or an OS primitive, so an atomic call may block.

## Panicking

`core::panic` owns Omega's portable answer to "this must not happen". The
entry point is the `panic$` macro:

```omega
panic$();
panic$("index out of bounds");
```

It builds a `PanicInfo` at the call site and hands it to the `PanicHandler`
gap, which returns `never`, so `panic$(...)` is a diverging expression usable
anywhere one is:

```omega
exposed struct PanicInfo {
    exposed source_file: *str;
    exposed line: u32;
    exposed column: u32;
    exposed message: *str;
}

gap PanicHandler {
    shared panic(info: *PanicInfo) => never;
}
```

The location comes from `core::builtins`' `file$`/`line$`/`column$` macros,
which describe the outermost invocation -- so it names *your* `panic$` call,
not a line in `core`. With no argument the message is `""`.

Like every other gap, the final program or platform package supplies the one
`glue`. Core neither allocates, formats, prints, nor terminates, and declares
no default handler of its own: print-and-abort is right for a hosted program
and wrong for a board that should reset or blink an LED.

A build that registers `plat` gets the hosted answer for free -- its libc
platform ships the panic glue described in
[platform glue](platform-glue.md). Because a gap takes exactly one glue
project-wide, that also means an application linking `plat` cannot add its own
panic policy on top; a program that wants a different one is built without
`plat`'s, and writes it itself:

```omega
import core::panic::PanicInfo;
import std::io::eprintln;

foreign(c) exit(code: i32) => never;

glue core::panic::PanicHandler {
    panic(info: *PanicInfo) => never {
        eprintln$(info.source_file, ":", info.line, ": panic: ", info.message);
        exit(101)
    }
}
```

A program that never reaches a `panic$` emits no reference to the handler and
therefore needs no panic glue, exactly as for the allocator and console gaps.

The message is meant to be optional, but Omega has no optional macro parameter
yet, so `panic$` declares a trailing variadic instead. Pass zero or one message;
passing more currently compiles and keeps the last one rather than reporting an
arity error, which is tracked in
[`../issues/language-limitations.md`](../issues/language-limitations.md).

## Deliberate boundary with `std`

`core::cmp` owns `Ordering`, `Eq`, and `Ord`; `std::default` owns `Default`;
`std::hash` owns `Hash`; `std::fmt` owns `Display` and formatting helpers; and
`std::io` owns `Write`, `Read`, buffering, console marker types, and printing.
The primitive conformances for all of those specs also live in `std`. Core may
still be the only package allowed to add *inherent* primitive methods, while
the ordinary conform orphan rule decides where a spec conformance may be
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

See [the standard library](standard-library.md) for the higher-level
facilities and [gaps and glue](../language/gaps-and-glue.md) for the capability model.
