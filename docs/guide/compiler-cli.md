# `omgc` compiler CLI

Typical shape:

```text
omgc <entry-dir> -o <output> [--name=<name>] [--import=[<name>:]<dir>]...
     [-O<0-3>] [--target=<triplet>]
     [--emit=<obj|ir|asm>] [-v]
```

`-o` is required. Package arguments are root **directories**, not individual `.omg` files.

## Package identity

`--name=<name>` overrides the local package's declared identity. `--import=<dir>` registers an external package using the directory basename as its identity; `--import=<name>:<dir>` supplies the identity explicitly.

Source-level meaning is specified in [`../language/modules-and-imports.md`](../language/modules-and-imports.md).

## Targets

A target is written as `<arch>-<os>` or `<arch>-<vendor>-<os>`; the vendor segment is accepted but is not currently semantically significant.

The compiler recognizes the target architectures/OS combinations supported by its target layer and LLVM. The default target is `x86_64-unknown-linux`.

An unsupported target is reported as a compiler error before emission.

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
