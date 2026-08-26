# Gaps and glue

This chapter is normative for current Omega language behavior. Known implementation limitations are tracked separately under [`../issues/`](../issues/).

Some capabilities are requested by portable code but can only be supplied by
the final program or its platform layer. A `gap` declares that capability as a
named, static function set; exactly one `glue` declaration provides its
implementation across the compilation.

```omega
# runtime/core/platform.omg
gap GlobalAllocator {
    alloc(size: usize) => *mut u8;
    free(ptr: *u8) => void;
    realloc(ptr: *u8, size: usize) => *mut u8;
}

# Independent console capabilities use Option for an exact transfer result.
gap StandardOutput { write(bytes: *[]u8) => Option<usize>; }
gap StandardError  { write(bytes: *[]u8) => Option<usize>; }
gap StandardInput  { read(into: *mut []u8) => Option<usize>; }

# a platform package
glue core::platform::GlobalAllocator {
    alloc(size: usize) => *mut u8 { libc_alloc(size) }
    free(ptr: *u8) => void { libc_free(ptr); }
    realloc(ptr: *u8, size: usize) => *mut u8 { libc_realloc(ptr, size) }
}
```

Both declarations are contextual keywords, not annotations. `gap` has no
visibility modifier of its own, and no generics, bodies, or `self` parameters.
A `glue` has no name, visibility, or generics of its own; it names one
qualified gap path and contains ordinary static function definitions.

For console I/O, `None` represents failure and `Some(n)` is the exact transfer
count. `Some(0)` is valid, including EOF on input. The three gaps are separate
so a target may provide only the capabilities it possesses; `std::io` turns
them into its `Stdout`, `Stderr`, and `Stdin` marker implementations.

## Gap-function visibility

A gap *function* may carry an ordinary `exposed`/`shared`/`hidden` modifier.
With no modifier it is `exposed`, so an existing gap keeps its meaning:

```omega
gap PanicHandler {
    # Callable only from within the declaring package: `core::panic::panic$`
    # stays the ordinary way to reach it.
    shared panic(info: *PanicInfo) => never;
}
```

The modifier gates who may **call** the function through a path, using the
ordinary visibility rules (see [`visibility.md`](visibility.md)); `hidden`
means the exact declaring module. Like any other visibility, it is a source
boundary, not an unforgeable one: an explicit `reveal` at the use site still
bypasses it.

Matching a `glue` is not a call, so this visibility is deliberately no part of
the gap's ABI, symbol identity, or conformance identity. A platform or
application package must still implement every function of a gap it fills,
including ones it may not call itself.

## Resolution and conformance

A gap is a first-class item in the function namespace, not a type or a spec.
Its functions are called with qualified paths, for example
`core::platform::GlobalAllocator::alloc(64)`. An imported gap can be used in
the same way:

```omega
import platform::GlobalAllocator;

buffer := GlobalAllocator::alloc(64);
```

Each glue must implement the gap's function set exactly: every required
function appears once, no extra function is allowed, and parameter and return
types must match. The compiler reports a targeted diagnostic for a non-gap
target, a missing function, an extra function, or a mismatched signature.

At whole-program scope, a gap may have one glue declaration. No glue produces
an `unfilled_gap` warning; it remains legal when no emitted code needs the
symbol. Two glues are a compile error. This keeps the declaration usable in
libraries without making reachability analysis part of the language model.

## Linkage

For every gap function, the declaring package emits a foreign-style declaration.
The corresponding glue function is compiled with the exact same mangled
symbol, so calls have the normal direct-function ABI and need no runtime
registry, object, or dynamic dispatch.

`runtime/core/platform.omg` supplies the allocator and console gaps and
`runtime/core/panic.omg` supplies `PanicHandler`. `runtime/plat/libc/` is an
ordinary external package that supplies the libc-backed glue for the platform
gaps; panic policy is left to the final program.
See [`plat`](../guide/platform-glue.md) for that implementation and
[the core library](../guide/core-library.md) for the public core layout.
