# The target the repository builds and runs natively. Every package of one
# build must be compiled for the same target: it fixes `usize`, layout and the
# selected platform package together.
HOST_TARGET := "x86_64-linux"

# Per-target artifact roots keep one target's `.o` from overwriting another's
# `.obj` at the same relative path.
ARTIFACTS := "target"

test-all: build-omgc build-runtime
    @echo "[*] Starting test-runner..."
    ./bin/test-runner

playground: build-omgc build-runtime
    @echo "[*] Running playground..."
    rm -rf target/playground target/playground-objects
    ./bin/omgc-debug -v playground/ --target={{HOST_TARGET}} \
        --import=core:runtime/core/ \
        --import=std:runtime/std/ \
        --import=plat:runtime/plat/target/{{HOST_TARGET}}/ \
        -o target/playground-objects
    just link-native target/playground-objects target/playground
    ./target/playground

# The link every native x86-64 Linux program in this repository uses. No CRT
# startup object and no libc: `plat`'s own `_start` is the ELF entry point, so
# `-nostdlib` must not be relaxed here. `-no-pie` because that entry does not
# self-relocate, and `--gc-sections` because a runtime object that mentions a
# capability the program never uses must not retain its glue.
link-native OBJECTS OUTPUT:
    cc -nostdlib -static -no-pie -Wl,--gc-sections \
        $(find {{OBJECTS}} {{ARTIFACTS}}/{{HOST_TARGET}}/core {{ARTIFACTS}}/{{HOST_TARGET}}/plat {{ARTIFACTS}}/{{HOST_TARGET}}/std -name '*.o' | sort) \
        -o {{OUTPUT}}

build-runtime: (build-runtime-for HOST_TARGET)
    @echo "[*] Runtime built successfully"

build-omgc:
    @echo "[*] Building omgc..."
    cargo build

# The composed target roots are committed as relative symlinks; a checkout
# that turned them into plain files would compile a platform with no
# allocator, console or entry point instead of failing.
check-plat-links:
    @./bin/check-plat-links

# Compiles `core`, the selected platform root and `std` for one target. Each
# package owns an output directory of per-source artifacts. The build is not
# incremental, so the directory is cleared first: a source that was deleted
# must not leave its artifact behind for the next link.
build-runtime-for TARGET OPT="-O0": build-omgc check-plat-links
    @echo "[*] Building runtime for {{TARGET}} ({{OPT}})..."
    rm -rf {{ARTIFACTS}}/{{TARGET}}/core {{ARTIFACTS}}/{{TARGET}}/plat {{ARTIFACTS}}/{{TARGET}}/std
    ./bin/omgc-debug -v runtime/core/ --target={{TARGET}} {{OPT}} -o {{ARTIFACTS}}/{{TARGET}}/core
    ./bin/omgc-debug -v plat:runtime/plat/target/{{TARGET}}/ --target={{TARGET}} {{OPT}} \
        --import=core:runtime/core/ -o {{ARTIFACTS}}/{{TARGET}}/plat
    ./bin/omgc-debug -v runtime/std/ --target={{TARGET}} {{OPT}} \
        --import=core:runtime/core/ -o {{ARTIFACTS}}/{{TARGET}}/std

# Every platform root in the tree, compiled the way its own target compiles
# it. `omgc` emits objects only, so this proves the packages build and what
# they depend on; producing an executable for a cross target is the job of
# that target's linker and import libraries (docs/guide/platform-glue.md).
build-runtime-all:
    just build-runtime-for x86_64-linux
    just build-runtime-for aarch64-linux
    just build-runtime-for x86_64-windows
    just build-runtime-for aarch64-windows
    # `-O1` because LLVM's AVR backend cannot register-allocate core's and
    # std's unoptimized 64-bit code; see docs/issues/compiler-limitations.md.
    just build-runtime-for avr-none -O1

# `runtime/plat/libc` is kept as an explicit compatibility platform for builds
# that want to sit on a C runtime; nothing in the normal build or test path
# selects it.
build-plat-libc: build-omgc
    @echo "[*] Building the libc compatibility platform..."
    rm -rf {{ARTIFACTS}}/{{HOST_TARGET}}/plat-libc
    ./bin/omgc-debug -v plat:runtime/plat/libc/ --target={{HOST_TARGET}} \
        --import=core:runtime/core/ -o {{ARTIFACTS}}/{{HOST_TARGET}}/plat-libc

# The platform matrix: every target root compiled for its own target, the
# hosted link proven free of a C runtime, the atomic glue put under real
# concurrency, and `avr-none` proven partial rather than stubbed.
check-platform: build-runtime-all
    @./bin/check-platform
