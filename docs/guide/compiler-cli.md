# `omgc` compiler CLI

Typical shape:

```text
omgc [<name>:]<entry-dir> -o <output> [--import=[<name>:]<dir>]...
     [-O<0-3>] [--target=<arch>-<os>]
     [--emit=<obj|ir|asm>] [-v]
```

`-o` is required. Package arguments are root **directories**, not individual `.omg` files.

## Package identity

The compiled package and its dependencies use the same `[<name>:]<dir>` spelling. A bare directory takes its identity from the directory basename; `<name>:<dir>` supplies the identity explicitly:

```sh
omgc mathlib:examples/extern_lib/ -o target/mathlib.o
omgc app/ --import=mathlib:examples/extern_lib/ -o target/app.o
```

Source-level meaning is specified in [`../language/modules-and-imports.md`](../language/modules-and-imports.md).

## Targets

A target is written as `<arch>-<os>` or `<arch>-<vendor>-<os>`; the vendor segment is accepted but is not currently semantically significant. The default target is `x86_64-unknown-linux`; there is no host autodetection, so an omitted `--target` always means that default rather than the machine running `omgc`.

Only real architecture/OS pairs are accepted. Every architecture supports freestanding use; a hosted OS is offered where that OS runs on the architecture:

| Architecture | Operating systems |
|---|---|
| `x86_64` | `none`, `linux`, `macos`, `windows` |
| `aarch64` | `none`, `linux`, `macos`, `windows` |
| `x86` (`i386`/`i686`) | `none`, `linux`, `windows` |
| `armv7` (`arm`) | `none`, `linux` |
| `riscv32`, `riscv64` | `none`, `linux` |
| `thumbv7em` (`thumbv7`) | `none` |
| `avr` | `none` |

`none` may also be spelled `freestanding`, and `macos` may be spelled `darwin`. A pair outside this table — `avr-macos`, `riscv64-windows` — is rejected while parsing arguments rather than passed to the backend as an invented triple.

```sh
omgc src/ --target=aarch64-linux   -o target/main-aarch64.o
omgc src/ --target=x86_64-windows  -o target/main-windows.obj
omgc src/ --target=avr-none        -o target/main-avr.o
```

The selected target is one decision shared by semantic analysis and code generation: it fixes `usize`/`isize` width, `sizeof` results, and layout as well as the LLVM triple and data layout. `avr-none` is a 16-bit target, so `sizeof<usize>` is `2` there. Cross-compilation is host-independent — a target is never rejected merely for differing from the machine running `omgc`.

`avr-none` selects generic AVR; no MCU (`atmega328p`, …) or CPU feature selection exists yet. AVR is a Harvard architecture, so function values, vtable slots and indirect calls use LLVM's program address space while ordinary data pointers stay in address space 0. This is a representation detail: a function value is still exactly one address and still casts to and from a thin raw pointer as [`../language/strings-casts-arrays-and-slices.md`](../language/strings-casts-arrays-and-slices.md) specifies.

A malformed, unknown, or unsupported target is rejected while parsing arguments. A supported target whose backend this build of LLVM cannot construct is reported as a compiler error before emission.

Selecting a target never introduces a libc, CRT, sysroot, linker or runtime object, and `omgc` emits objects only — it does not invoke a cross linker.

## Emit modes

```text
--emit=obj     object file (default)
--emit=ir      LLVM IR
--emit=asm     assembly
```

Optimization levels are `-O0` through `-O3`, defaulting to `-O0`.

## Separate compilation

A normal multi-package build compiles each package in a separate `omgc` process and links the produced objects afterward:

```sh
omgc runtime/core/ -o target/core.o
omgc examples/mathlib/ -o target/mathlib.o
omgc examples/dev/ \
    --import=mathlib:examples/mathlib/ \
    --import=core:runtime/core/ \
    -o target/main.o
cc -Wl,--gc-sections target/main.o target/mathlib.o target/core.o -o example
```

Generated functions use independent object-file sections; repository build recipes link with section garbage collection so unused functions do not retain unrelated dependencies.

There is no implicit libc/runtime requirement in this model. The selected package/glue objects determine what the final link requires.
