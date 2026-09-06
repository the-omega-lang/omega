# Runtime, core, standard library, and platform architecture

Omega intentionally minimizes hidden runtime machinery. Most facilities normally called a “runtime” are ordinary Omega packages compiled separately and linked like user packages.

```text
                 compiler-generated/user object
                         |
          +--------------+--------------+
          |                             |
     core objects                   std objects
          |                             |
          +---------- capability -------+
                         |
                    plat objects

optional target-specific assembly: runtime/shims/
```

Each package compiles to a directory of per-source objects rather than a single file, so `core`, `std`, and `plat` each contribute the objects of their own source files. The exact objects linked are a build/application choice; there is no mandatory compiler-injected libc runtime object.

## Package layering

### `runtime/core`

`core` is the minimal portable foundation. It owns:

- built-in primitive declaration blocks and inherent primitive methods;
- allocation-free core data/protocols such as `Option`, comparison, iterators, ranges;
- platform capability **gaps** (allocator/console/panic/atomic contracts), not implementations;
- the compiler-implemented source-location macros in `core::builtins`.

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

Today `runtime/plat/libc/` is compiled as an ordinary package but presented to source under declared identity `plat` using `plat:<dir>` / `--import=plat:<dir>`.

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

### Panic as a gap

Unrecoverable failure uses the same seam. `core::panic` declares `PanicHandler`, and `core::panic::panic$` is the source-level entry point: the macro builds a stack-local `PanicInfo` from `core::builtins`' source-location macros at the call site and tail-calls the handler, which returns `never`.

Nothing in the compiler or `core` picks a panic policy. Panic policy is a platform decision like any other capability, so it lives with the platform package: `runtime/plat/libc` reports the site on descriptor 2 and calls `abort`, which is correct for a hosted program and wrong for a freestanding target that wants a trap, a reset, or a status LED -- such a target supplies its own glue in a build that does not register `plat`, since one gap still takes exactly one glue. Deliberately keeping the construction inside the macro rather than behind a core helper function is what keeps `core`'s objects free of any reference to the handler symbol, so a program that never panics needs no panic glue and no extra linkage.

No allocation, formatting, unwinding, backtrace machinery, runtime registry, or backend intrinsic is involved: the location macros become ordinary literals during macro expansion, and the handler call is an ordinary gap call.

### Atomics as a width capability

`core::atomic` declares one gap per storage width -- `Atomicity8`,
`Atomicity16`, `Atomicity32`, `Atomicity64` -- each carrying the full
load/store/exchange/compare-exchange/`fetch_*` set for that width. `std::atomic`
wraps them in fixed-width types. The layering rule is the same as every other
capability, and it is deliberate that atomicity enters here rather than in the
backend:

- no compiler crate has an atomic semantic type, checked node, MIR
  instruction, LLVM atomic instruction, or target atomic-capability table;
- an atomic call is an ordinary direct call to a gap symbol, resolved at link
  time like `GlobalAllocator::alloc`;
- the platform that fills a width gap chooses the mechanism -- native atomic
  instructions, an LL/SC or CAS retry loop, interrupt masking, an OS service,
  or a lock -- and owes the complete contract in
  [`../language/atomics.md`](../language/atomics.md) for that width;
- `runtime/plat/libc` fills none of them, because no honest libc-only
  implementation exists without an architecture-specific body or a new runtime
  dependency. That is a platform gap left open on purpose, not an oversight.

One consequence is worth recording. Codegen treats an ordinary external call
conservatively with respect to memory, and platform inline assembly is emitted
with a mandatory memory clobber, so an atomic gap call already acts as a
compiler memory barrier whatever ordering it requested. Requested `Relaxed`
therefore optimizes less than an intrinsic-based design would. That is
stronger behavior than promised, which the language's strengthening rule
permits, and recovering the lost freedom would mean giving the backend
knowledge of atomic semantics -- exactly what this seam exists to avoid.

## Platform independence

`core` declares allocator/console/panic/atomic capabilities but does not automatically invoke them merely by being linked. Higher-level `std` facilities reference the gaps only from functions that need them.

Native ownership is per source file, so unused capabilities can be dropped at two independent granularities. Whole objects: a build that archives a package's objects and lets the linker extract only what it references never pulls in a source file whose capabilities the program does not use. Within an object: the repository compiles functions into independently collectible sections and links integration binaries with `--gc-sections`, which is what the repository's own links rely on, because they pass every emitted object directly and therefore retain each one. That still matters even with per-source objects -- a single source file mixes used and unused functions, `std::atomic`'s wrappers among them -- so every link line in the repository, the `just` recipes and `bin/test-runner` alike, must pass `--gc-sections`.

This is important to Omega's “no hidden runtime cost” and freestanding goals.

## Separate compilation of runtime packages

Typical repository flow:

```text
omgc runtime/core/                    -> target/core/**.o
omgc runtime/std/ --import=core:...   -> target/std/**.o
omgc plat:runtime/plat/libc/ \
     --import=core:...                -> target/plat/**.o
omgc app/ --import=core:... \
          --import=std:... \
          --import=plat:...           -> target/app/**.o

system linker -> app executable
```

Each `omgc` invocation resolves signatures from registered extern roots but emits only the bodies its compilation owns (plus concrete template instantiations it is responsible for), spread across one object per source file of that package. `omgc` itself never links or archives; assembling those objects is a build-system decision.

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

## The link configuration as an architecture test

`core`, `plat` and `std` are three separate `omgc` invocations, and
`bin/test-runner` links the per-source objects of all three into every
conformance case with `-Wl,--gc-sections`. That single uniform configuration is
what exercises the architectural promises:

- a case that never touches std I/O still links, with console glue
  dead-stripped rather than kept off the link line;
- symbols and ABI agree across separately compiled packages;
- generic/conform instantiations that several packages produce independently
  coalesce at link time instead of colliding.

The root `justfile` only builds those packages (`build-core`, `build-plat`,
`build-std`) and links the hosted `playground` target; it does not itself
encode runtime combinations.

See [`testing-and-validation.md`](testing-and-validation.md).

## Relevant programmer docs

For API/use rather than implementation architecture:

- [`../guide/core-library.md`](../guide/core-library.md)
- [`../guide/standard-library.md`](../guide/standard-library.md)
- [`../guide/platform-glue.md`](../guide/platform-glue.md)
- [`../guide/console-io.md`](../guide/console-io.md)

For normative capability semantics, see [`../language/gaps-and-glue.md`](../language/gaps-and-glue.md).
