run-exec DEBUGGER="": build-asm build-exe
    # ld target/hello.o target/shims.o -o target/example # no libc
    cc -Wl,--gc-sections target/main.o target/mathlib.o target/core.o target/std.o target/plat.o -o target/example   # with libc
    {{DEBUGGER}} ./target/example firstarg secondarg; echo -e "\nexit code: $?"

build-exe: build-core build-plat build-std
    rm target/example || true
    RUST_BACKTRACE=1 cargo build
    ./target/debug/omgc -v examples/mathlib/ -o target/mathlib.o
    ./target/debug/omgc -v examples/dev/ --extern=mathlib:examples/mathlib/ --extern=core:runtime/core/ --extern=std:runtime/std/ --extern=plat:runtime/plat/libc/ -o target/main.o

# `runtime/plat/` is a plain directory, not a package -- each subdirectory
# under it (just `libc/` today) is its own independent, honestly-named
# package (`runtime/plat/libc/libc.omg`) that presents as the *same*
# declared identity `plat` purely via `--name=`/`--extern=plat:...`, never
# by renaming its own files (see docs/22-platform-glue.md). Picking a
# platform is exactly choosing which directory these two flags point at --
# there is no compiler-level selection mechanism, this is it. `plat` gets
# no other privilege `core` has (no eager-discovery/ambient-prelude
# exemption); registering it is enough for `examples/dev`'s reference to
# `core::platform::GlobalAllocator` to find its glue, even though nothing in
# `examples/dev` ever imports `plat` itself.
build-plat: build-core
    ./target/debug/omgc -v runtime/plat/libc/ --name=plat --extern=core:runtime/core/ -o target/plat.o

# `omgc` takes a package root directory. That directory is its root module:
# `<dir>/<basename>.omg`, when present, owns the root and the root's other
# entries are its children. Without that file the root is a namespace-only
# module. `main` in the root module, not a special filename, receives the C
# entry symbol.
build-core:
    mkdir -p target
    RUST_BACKTRACE=1 cargo build
    ./target/debug/omgc -v runtime/core/ -o target/core.o

build-std: build-core
    ./target/debug/omgc -v runtime/std/ --extern=core:runtime/core/ -o target/std.o

build-io-demo: build-std build-plat
    ./target/debug/omgc -v examples/io_demo/ --extern=core:runtime/core/ --extern=std:runtime/std/ --extern=plat:runtime/plat/libc/ -o target/io_demo.o
    cc -Wl,--gc-sections target/io_demo.o target/core.o target/std.o target/plat.o -o target/io_demo

test-io: build-io-demo
    ./target/io_demo < tests/io_demo.stdin > target/io_demo.stdout
    diff tests/io_demo.expected target/io_demo.stdout

build-stdio-contract: build-std build-plat
    ./target/debug/omgc -v examples/stdio_contract/ --extern=core:runtime/core/ --extern=std:runtime/std/ --extern=plat:runtime/plat/libc/ -o target/stdio_contract.o
    cc -Wl,--gc-sections target/stdio_contract.o target/core.o target/std.o target/plat.o -o target/stdio_contract

test-stdio-contract: build-stdio-contract
    ./target/stdio_contract > target/stdio_contract.stdout
    diff tests/stdio_contract.expected target/stdio_contract.stdout

build-core-only: build-core
    ./target/debug/omgc -v examples/core_only/ --extern=core:runtime/core/ -o target/core_only.o
    cc -Wl,--gc-sections target/core_only.o target/core.o -o target/core_only

test-core-only: build-core-only
    ./target/core_only
    ! readelf -rW target/core.o | rg 'Standard(Output|Error|Input)|GlobalAllocator'

# Ranges are ordinary `core` values, so this needs nothing but `core` -- which
# is itself the assertion that range iteration implies no allocator and no
# platform glue. `range_demo` returns a distinct exit code per failed case.
build-range: build-core
    ./target/debug/omgc -v examples/range_demo/ --extern=core:runtime/core/ -o target/range_demo.o
    cc -Wl,--gc-sections target/range_demo.o target/core.o -o target/range_demo

test-range: build-range
    ./target/range_demo

# `char`'s semantics need execution to mean anything -- whether `from_u32`
# actually rejects a surrogate, whether `Successor` skips the hole. Needs only
# `core`, which is also the assertion that none of it pulls in an allocator.
build-char: build-core
    ./target/debug/omgc -v examples/char_demo/ --extern=core:runtime/core/ -o target/char_demo.o
    cc -Wl,--gc-sections target/char_demo.o target/core.o -o target/char_demo

test-char: build-char
    ./target/char_demo

# Only `main` in the root module may receive the bare C entry symbol; a child
# module's identically named function remains normally mangled. Compiled with
# no `--extern` at all, so this also covers a package that never registers
# `core`.
build-root-layout:
    mkdir -p target
    RUST_BACKTRACE=1 cargo build
    ./target/debug/omgc -v examples/root_layout/ -o target/root_layout.o

# The second assertion spells the child's *whole* mangled path out on
# purpose: a bare `4main` match would still pass if discovery regressed to
# treating the root directory as a container, which would make `nested` a
# top-level module (`C6nested`) instead of `root_layout::nested`
# (`NtC11root_layout6nested`).
test-root-layout: build-root-layout
    test "$(nm --defined-only target/root_layout.o | rg -c ' main$')" = 1
    nm --defined-only target/root_layout.o | rg '_omg_NvNtC11root_layout6nested4main'

# `std` allocation works with only allocator glue; this target deliberately
# omits `plat.o`, so any retained console path is a link failure.
build-allocator-only: build-std
    ./target/debug/omgc -v examples/allocator_only/ --extern=core:runtime/core/ --extern=std:runtime/std/ -o target/allocator_only.o
    cc -Wl,--gc-sections target/allocator_only.o target/core.o target/std.o -o target/allocator_only

# The link above is the primary assertion: it omits `plat.o`, so a retained
# console path is an undefined-reference failure before this recipe runs at
# all. The check below is the secondary one -- that `--gc-sections` actually
# *dropped* the console code rather than the link having succeeded for some
# unrelated reason. Deliberately over *defined* symbols: `nm -u` on a linked
# executable can never report anything, so asserting on it proves nothing.
test-allocator-only: build-allocator-only
    ./target/allocator_only
    ! nm --defined-only target/allocator_only | rg 'Std(out|err|in).*(Write5write|Read4read)'

run-asm: build-asm
    ld target/shims.o -o target/shims
    ./target/shims; echo -e "\nexit code: $?"

build-asm:
    mkdir -p target
    rm target/shims target/shims.o || true
    as runtime/shims/x86_64-unknown-linux.S -o target/shims.o

clean:
    rm -rf target

# Two packages that both print, linked together. `println$` expands to a
# `BufWriter<Stdout>` -- a generic conform instantiation each package emits
# independently -- so this fails to link outright if a conform method built
# from a template gets strong linkage. The `cc` below is the assertion; the
# diff then confirms both packages' output actually survived folding.
build-multi-print: build-std build-plat
    ./target/debug/omgc -v examples/multi_print/printlib/ --name=printlib --extern=core:runtime/core/ --extern=std:runtime/std/ -o target/printlib.o
    ./target/debug/omgc -v examples/multi_print/app/ --extern=core:runtime/core/ --extern=std:runtime/std/ --extern=printlib:examples/multi_print/printlib/ -o target/multi_print.o
    cc -Wl,--gc-sections target/multi_print.o target/printlib.o target/std.o target/core.o target/plat.o -o target/multi_print

test-multi-print: build-multi-print
    ./target/multi_print > target/multi_print.stdout
    diff tests/multi_print.expected target/multi_print.stdout
