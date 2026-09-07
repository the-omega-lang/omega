# Console I/O and formatting

Console and byte I/O live in `std`, not `core`. The model separates an
operation contract from a concrete capability and from caller-owned buffering.
It has no global stream object and no hidden allocation.

## Byte contracts

```omega
exposed spec Write {
    write(*mut self, bytes: *[]u8) => Option<usize>;
}

exposed spec Read {
    read(*mut self, into: *mut []u8) => Option<usize>;
}
```

`None` is failure. `Some(n)` reports exactly how many bytes were consumed or
produced; `Some(0)` is a legitimate result. A caller must treat a short write
as partial progress and resume from the unwritten suffix when appropriate.
This is equally useful for a console, a bounded memory sink, a file-like
adapter, or a test double.

## Console capabilities

`Stdout`, `Stderr`, and `Stdin` are zero-sized marker values that conform to
`Write` or `Read`. They are the only `std` types that name the core platform
gaps:

```omega
mut sink := Stdout {};
Write::write(&mut sink, b"hello\n");
```

A hosted `plat` target forwards these to whatever the operating system's own
byte I/O is -- the `read`/`write` syscalls on descriptors 0, 1 and 2 on Linux,
`ReadFile`/`WriteFile` on the standard handles on Windows -- with no libc in
between. An OS error is `None`; every successful transfer, including a zero
or short one, is `Some(count)`. A platform can implement the three gaps
independently, and `avr-none` implements none of them: a generic AVR target
identifies no serial port. Linking a program that uses a marker requires the
corresponding glue; linking a program that does not reach it does not.

## Caller-owned adapters

`SliceWriter` writes into a supplied `*mut []u8`. It returns a partial
`Some(n)` when the final write reaches its capacity and returns `None` for a
later non-empty write once full. Its `len()` and `as_slice()` expose the
written prefix.

`BufWriter<W: Write>` wraps a caller-supplied `W` and caller-supplied byte
storage. Its `write` buffers bytes, flushing as necessary; `flush()` resumes
short inner writes and preserves any unflushed suffix when the inner writer
returns `None` or zero progress. It is itself a `Write`, so buffering can be
nested without a special case.

```omega
mut sink := Stdout {};
mut storage: [128]u8;
mut out := BufWriter<Stdout>::new(&mut sink, &mut storage[0..]);
out.write(b"buffered text\n");
out.flush();
```

`BufReader<R: Read>` is the symmetric caller-buffered reader. It returns
already buffered bytes first and makes at most one inner read per call, so it
does not hide a source's short-read behavior. A zero-length buffer simply
delegates to its inner reader or writer.

`read_line(reader: *mut spec Read, into: *mut String) => bool` is a free
function rather than a required method on every reader. It appends a line to
the caller's `String`, consumes but does not append a newline, and returns
false only when it read no byte at all. `StringWriter` and
`string_writer(*mut String)` are `Write` adapters for allocation-backed text;
`to_string<T: Display>` formats through one.

## Formatting and macros

```omega
exposed spec Display {
    fmt(*self, out: *mut spec Write) => void;
}
```

`std::fmt` owns `Display` and its allocation-free helpers. Integer helpers
support bases 2 through 36. Floating-point output has six fractional digits,
uses scientific notation outside its fixed range, and handles `nan`, `inf`,
and `-inf`; it is deliberately not shortest-round-trip formatting.

Primitive `Display` conformances live in `std::primitives`. A package-owned
type can conform to it in the usual way:

```omega
import std::fmt::Display;
import std::io::Write;

meet Display for Pair {
    fmt(*self, out: *mut spec Write) => void {
        out.write(b"pair");
    }
}
```

`print`, `println`, `eprint`, and `eprintln` are exposed `std::io` macros.
They build a caller-visible 256-byte `BufWriter` around the relevant console
marker, format each argument through `Display`, and flush before the expansion
ends. They are concatenative (`println$("count=", count)`), not format-string
based. There is no implicit flush at program exit.

Print macros resolve their own `Display`, `Write`, `BufWriter`, and
`Stdout`/`Stderr` references in `std::io`; callers need import only the macro
itself. Their `buf`, `sink`, and `out` locals are hygienic, while each supplied
argument remains in the caller's scope. A conforming method is reached with
the spec-qualified `Display::fmt`, which lets literals and temporaries use the
normal receiver adaptation rules.

Build and run the hosted example with:

```sh
just playground
```

That recipe builds `core`, `plat` and `std` first, then links
`playground/playground.omg` against their objects and runs it. The playground
ends on a deliberate `panic!` demonstration, so a non-zero exit at the end is
the expected result, not a build failure.
