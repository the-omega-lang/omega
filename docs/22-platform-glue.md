# `plat`: the default platform glue

`runtime/plat/` — a plain directory, not a package itself. Each
subdirectory under it is its own independent, standalone-compilable
package, honestly named after what it physically is (`libc/` today), that
*presents* as the same declared identity `plat` purely via a compiler-
level alias (`--name=`/`--extern=plat:...`) — the project's own files
never lie about what they are; only the compiler's view of a root's
identity can differ from its on-disk name. Unlike `core`
([the core library](13-core-library.md)), `plat` gets no ambient-prelude
treatment, no `primitive`-block privilege, and no eager-discovery exemption of
its own; it's just an ordinary `--extern` package that happens to ship
`glue` implementations for `core`'s own gaps (see
[gaps and glue](21-gaps-and-glue.md)). Any consumer that registers it gets
its glue discovered automatically, whether or not it ever `import`s `plat`
itself — the same eager, whole-program struct/spec surface resolution
every registered extern gets (see [modules & linkage](10-modules-and-linkage.md)'s
"Eager local discovery").

## Layout

```
runtime/plat/
  libc/
    libc.omg      # honestly named — compiles/registers as "plat" via alias
```

`libc.omg` declares everything this platform needs directly: three
`internal extern` bindings to libc's own `malloc`/`free`/`realloc`, and
the `glue` declaration that adapts them to `core::platform::GlobalAllocator`.
A deliberate, real name collision lives here — the marker's own
`free`/`realloc` methods share their literal names with the raw externs
they call — confirmed safe rather than assumed: a bare call inside a
method's own body always resolves through ordinary module-level
resolution, never implicitly back to the enclosing method.

A future second platform (`runtime/plat/windows/`, say) would be a
sibling directory, its own independent package, aliased to `plat` the
same way — not a submodule of `libc`, and not a variant of it. There is
no selection mechanism *between* platforms (see "No platform selection"
below); each is simply available at its own honest path, and a build
picks one by choosing which directory its `--name=`/`--extern=plat:...`
flags point at.

## The `plat` alias

`--name=<name>` (standalone compilation) and `--extern=<name>:<dir>`
(consumption) have always let a package's *declared* identity differ from
its root directory's own basename in principle (`examples/extern_lib/`
compiles as `mathlib` this same way) — what's new is that the alias now
applies to *everything* discovered beneath the root, not just the root
segment itself, so a directory honestly named `libc`, with real content
of its own, can still present as `plat` in full. Two real,
previously-latent problems had to be fixed for this to actually work
end-to-end, not just at the entry point:

- **`fs_resolve::discover_into` double-counted a directory-shaped module
  named the same as its own entry file** (`X/X.omg`) — the exact shape
  real nesting under an aliased root would otherwise need. Now fixed
  (was tracked in [known issues](14-known-issues.md); `core`'s own
  `core/core.omg` had this bug too, latently, harmless only because
  `core.omg` itself declares nothing).
- **`ModuleRoots::locate`** used to reconstruct a filesystem path
  directly from a path's *declared* segments for every `--extern`
  lookup — which would search for a literal `plat.omg` on disk no matter
  what. It now reads the already-discovered, already-aliased inventory
  (`ModuleRoots::extern_trees`) instead, a plain map lookup with no live
  filesystem access at all — both simpler than the old live-lookup path
  and a prerequisite for the alias to work.

## No platform selection

There is no OS/target conditional-compilation mechanism anywhere in this
compiler today (no `cfg`-equivalent), so there is nothing for the
*compiler* to choose between platforms with — picking one is entirely a
build-script decision: which directory `--extern=plat:...` points at.
`libc` isn't a chosen default among several in any deeper sense; it's
just the one platform that exists right now, at its own honest path.

## API surface

- **Allocator glue** — the `glue` `plat`'s `libc` platform ships implements
  `core::platform::GlobalAllocator` by forwarding straight to
  libc's own `malloc`/`free`/`realloc`. No page-level allocation, no
  alignment beyond whatever libc's `malloc` already guarantees, and no
  error handling beyond a straight pass-through — the gap's own signature
  has no failure channel (no `Option`/`Result`) to put anything else in;
  a `NULL` from `malloc`/`realloc` is returned as-is.
- The three `extern` bindings themselves are `internal` (package-wide,
  not `exposed`): implementation detail, not part of any public surface.

## Building it

```
just build-plat     # omgc runtime/plat/libc/ --name=plat --extern=core:runtime/core/ -o target/plat.o
```

Built and linked exactly like any other `--extern` dependency — `just
build-exe`/`run-exec` register `--extern=plat:runtime/plat/libc/` and
link `target/plat.o` alongside `core.o`/`mathlib.o`, even though
`examples/dev` never imports `plat` (see `examples/dev/main.omg`'s own
`GlobalAllocator::alloc`/`free` demo, resolved through the ambient `core`
prelude with no `plat` reference of any kind).

The libc platform adds console glue using `write(2)` for stdout/stderr and
`read(2)` for stdin. Each target gap has its own glue declaration.
