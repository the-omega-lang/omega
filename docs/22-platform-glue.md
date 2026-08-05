# `plat`: the default platform `@glue`

`runtime/plat/` — a plain, ordinary `--extern` runtime package, not a
special one. Unlike `core` ([the core library](13-core-library.md)),
`plat` gets no ambient-prelude treatment, no `for`-block privilege, and no
eager-discovery exemption of its own; it's just a package that happens to
ship `@glue` implementations for `core`'s own gaps (see
[gaps and glue](21-gaps-and-glue.md)). Any consumer that registers it via
`--extern` gets its glue discovered automatically, whether or not it ever
`import`s `plat` itself — the same eager, whole-program struct/spec
surface resolution every registered extern now gets (see
[modules & linkage](10-modules-and-linkage.md)'s "Eager local discovery").

## Layout

```
runtime/plat/
  plat/
    plat.omg          # real root: nothing to declare, see below
    libc/
      glue.omg           # plat::libc::glue — @glue LibcAllocator
      sys.omg              # plat::libc::sys — raw extern malloc/free/realloc
```

`plat.omg` has nothing left to declare — it exists only because the
compiler still needs a conventionally-named entry file to find
(`plat.omg`/`plat/plat.omg`) when `plat` is compiled standalone (`just
build-plat`).

`libc` is a real, nested module of its own — one *platform* `plat` ships,
not `plat` itself — deliberately with no entry file of its own
(`libc/libc.omg` would collide with the already-documented directory-
shaped-module-named-like-its-own-entry-file bug, see
[known issues](14-known-issues.md); a namespace-only directory with no
`own_file`, holding `glue.omg`/`sys.omg` as real children, sidesteps it
entirely). A future second platform (`plat::windows`, `plat::wasm`, ...)
would be a sibling directory next to `libc`, not a variant of it — there
is no selection mechanism between platforms today (see "No platform
selection" below), so `libc` isn't a chosen default among several; it's
simply the one platform that exists.

## API surface

- **`plat::libc::glue`** — `LibcAllocator`, the one `@glue` `plat` ships:
  implements `core::glue::GlobalAllocator` by forwarding straight to
  libc's own `malloc`/`free`/`realloc` (`plat::libc::sys`). No page-level
  allocation, no alignment beyond whatever libc's `malloc` already
  guarantees, and no error handling beyond a straight pass-through — the
  gap's own signature has no failure channel (no `Option`/`Result`) to
  put anything else in; a `NULL` from `malloc`/`realloc` is returned
  as-is.
- **`plat::libc::sys`** — raw, unmangled `extern` bindings to libc's
  `malloc`/`free`/`realloc`, `internal` (package-wide, not `exposed`):
  `plat`'s own implementation detail, not part of its public surface.

## No platform selection

There is no OS/target conditional-compilation mechanism anywhere in this
compiler today (no `cfg`-equivalent), so there is nothing for `plat` to
choose *between* yet — `libc` isn't picked by anything, it's just the one
`@glue` that exists. A real multi-platform `plat` would need either a
genuine conditional-compilation feature or a build-level convention for
selecting exactly one platform module per build; neither exists, and
`plat` doesn't speculatively build toward either.

## Building it

```
just build-plat     # omgc runtime/plat/ --extern=core:runtime/core/ -o target/plat.o
```

Built and linked exactly like any other `--extern` dependency — `just
build-exe`/`run-exec` register `--extern=plat:runtime/plat/` and link
`target/plat.o` alongside `core.o`/`mathlib.o`, even though
`examples/dev` never imports `plat`.
