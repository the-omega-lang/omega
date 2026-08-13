# Gaps and glue

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
gap StandardOutput { write(bytes: *[?]u8) => Option<usize>; }
gap StandardError  { write(bytes: *[?]u8) => Option<usize>; }
gap StandardInput  { read(into: *mut [?]u8) => Option<usize>; }

# a platform package
glue core::platform::GlobalAllocator {
    alloc(size: usize) => *mut u8 { libc_alloc(size) }
    free(ptr: *u8) => void { libc_free(ptr); }
    realloc(ptr: *u8, size: usize) => *mut u8 { libc_realloc(ptr, size) }
}
```

Both declarations are contextual keywords, not annotations. `gap` has no
visibility modifier, generics, bodies, or `self` parameters. A `glue` has no
name, visibility, or generics of its own; it names one qualified gap path and
contains ordinary static function definitions.

For console I/O, `None` represents failure and `Some(n)` is the exact transfer
count. `Some(0)` is valid, including EOF on input. The three gaps are separate
so a target may provide only the capabilities it possesses; `std::io` turns
them into its `Stdout`, `Stderr`, and `Stdin` marker implementations.

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

For every gap function, the declaring package emits an extern declaration.
The corresponding glue function is compiled with the exact same mangled
symbol, so calls have the normal direct-function ABI and need no runtime
registry, object, or dynamic dispatch.

`runtime/core/platform.omg` supplies the core gaps. `runtime/plat/libc/`
is an ordinary external package that supplies the libc-backed glue for them.
See [`plat`](22-platform-glue.md) for that implementation and
[the core library](13-core-library.md) for the public core layout.
