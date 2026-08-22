# Runtime, core, standard library, and platform architecture

Omega intentionally minimizes hidden runtime machinery. Most facilities normally called a “runtime” are ordinary Omega packages compiled separately and linked like user packages.

```text
                 compiler-generated/user object
                         |
          +--------------+--------------+
          |                             |
       core.o                         std.o
          |                             |
          +---------- capability -------+
                         |
                      plat.o

optional target-specific assembly: runtime/shims/
```

The exact objects linked are a build/application choice; there is no mandatory compiler-injected libc runtime object.

## Package layering

### `runtime/core`

`core` is the minimal portable foundation. It owns:

- built-in primitive declaration blocks and inherent primitive methods;
- allocation-free core data/protocols such as `Option`, comparison, iterators, ranges;
- platform capability **gaps** (allocator/console contracts), not implementations.

It is designed to remain useful in freestanding/embedded contexts.

Compiler privilege is narrow and explicit:

- exposed core items/macros can participate in ambient fallback lookup;
- primitive declarations/inherent primitive methods belong to core.

`core` is still compiled to an ordinary separate object and linked when its definitions are used.

### `runtime/std`

`std` is an ordinary portable external package above core. It owns higher-level facilities such as:

- allocation wrappers;
- `Default`, hashing, display/formatting;
- I/O traits/helpers;
- collections and owned string.

It is **not ambient**. Consumers import the names they use.

Owning library values use explicit lifetime/free APIs; the compiler does not inject destructor/GC behavior.

### `runtime/plat/*`

`runtime/plat/` is a container of platform implementations, not one magical compiler-known package.

Today `runtime/plat/libc/` is compiled as an ordinary package but presented to source under declared identity `plat` using `--name=plat` / `--import=plat:<dir>`.

Selecting a platform implementation is therefore selecting which root/object is built and registered, not an implicit compiler target hook.

### `runtime/shims`

`runtime/shims/` contains target-specific assembly for functionality that cannot/should not be expressed as ordinary portable Omega code. It is outside the normal compiler package graph and is linked explicitly when a build recipe needs it.

## Gaps and glue as the platform seam

Portable code declares a capability with `gap`. A platform/final-program package provides it with `glue`.

Architecturally:

```text
core declares gap function identity
        |
        | calls compile as ordinary external function symbol
        v
same mangled symbol
        ^
        | glue body compiled in platform/final package
platform provides implementation
```

There is no runtime service locator, registration table, or implicit dynamic dispatch for gaps.

The driver checks declaration/implementation relationship and uniqueness at compilation scope. MIR mangling ensures both sides use the same symbol.

## Platform independence

`core` declares allocator/console capabilities but does not automatically invoke them merely by being linked. Higher-level `std` facilities reference the gaps only from functions that need them.

The repository compiles functions into independently collectible object sections and links integration binaries with `--gc-sections`, so unused library functions should not force unrelated platform capabilities into the final executable.

This is important to Omega's “no hidden runtime cost” and freestanding goals.

## Separate compilation of runtime packages

Typical repository flow:

```text
omgc runtime/core/                    -> core.o
omgc runtime/std/ --import=core:...   -> std.o
omgc runtime/plat/libc/ --name=plat \
     --import=core:...                -> plat.o
omgc app/ --import=core:... \
          --import=std:... \
          --import=plat:...           -> app.o

system linker -> app executable
```

Each `omgc` invocation resolves signatures from registered extern roots but emits only the bodies its compilation owns (plus concrete template instantiations it is responsible for).

## `core` ambient lookup

Outside `core`, exposed core declarations can be found as an ambient fallback after normal local/import resolution fails. This is implemented by the driver/resolver, not by copying all core declarations into every lexical scope.

Inside core itself, ordinary explicit module/import rules apply rather than recursively using the ambient fallback.

Macros have analogous exposed-core environment behavior during expansion.

## Primitive ownership

Built-in scalar/slice/string semantic types are compiler-known, but their inherent method declaration surface is represented through `primitive` blocks in core.

The driver discovers/registers primitive declarations/templates; analyzer method resolution asks the resolver for applicable primitive methods.

This keeps much of the primitive API in ordinary Omega source instead of hardcoding every operation in Rust.

## Standard-library conformances

Core/std can attach ordinary spec conformances through the same conformance mechanism used by user packages, subject to language orphan/ownership rules. `std` uses this for protocols it owns while core retains primitive inherent methods/minimal protocols required by core features.

The compiler should not special-case a standard-library type/spec relationship when the ordinary conformance system can express it.

## ABI boundary

Runtime objects are normal separately compiled Omega objects, so they share the same:

- mangling;
- internal Omega call ABI;
- aggregate layout;
- generic/conformance weak-linkage rules.

Platform calls crossing into C via `extern` additionally depend on the FFI contract and current platform-C-ABI limitations documented under language/issues docs.

## Build recipes as architecture tests

The root `justfile` deliberately constructs several runtime combinations:

- core-only binaries;
- std + allocator but no console platform object;
- std + platform console;
- multiple packages independently producing generic conform instantiations;
- mixed backend objects.

These recipes are not just demos; they exercise architectural promises such as “unused std I/O does not force console glue” and “same symbols/ABI work across separate compiler invocations”.

See [`testing-and-validation.md`](testing-and-validation.md).

## Relevant programmer docs

For API/use rather than implementation architecture:

- [`../guide/core-library.md`](../guide/core-library.md)
- [`../guide/standard-library.md`](../guide/standard-library.md)
- [`../guide/platform-glue.md`](../guide/platform-glue.md)
- [`../guide/console-io.md`](../guide/console-io.md)

For normative capability semantics, see [`../language/gaps-and-glue.md`](../language/gaps-and-glue.md).
