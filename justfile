run-exec DEBUGGER="": build-asm build-exe
    # ld target/hello.o target/shims.o -o target/example # no libc
    cc -Wl,--gc-sections target/main.o target/mathlib.o target/core.o target/std.o target/plat.o -o target/example   # with libc
    {{DEBUGGER}} ./target/example firstarg secondarg; echo -e "\nexit code: $?"

build-exe: build-core build-plat build-std
    rm target/example || true
    RUST_BACKTRACE=1 cargo build
    ./target/debug/omgc -v examples/extern_lib/ --name=mathlib -o target/mathlib.o
    ./target/debug/omgc -v examples/dev/ --extern=mathlib:examples/extern_lib/ --extern=core:runtime/core/ --extern=std:runtime/std/ --extern=plat:runtime/plat/libc/ -o target/main.o

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

# `omgc` takes a package's own root *directory*, not a file -- it discovers
# every module under it eagerly (the filesystem is the source of truth for
# what a package contains) and finds the entry itself: `<dir>/<name>.omg`,
# or a directory-shaped `<dir>/<name>/<name>.omg` (the same convention any
# *nested* directory-shaped module's own content already follows,
# recognized here too), else `<dir>/main.omg`. `core`'s own content lives
# at `runtime/core/core/core.omg` -- a directory-shaped module named
# `core`, rooted at `runtime/core/` (which is why that's the path given
# here, not `runtime/core/core/` itself) -- so no `--name=` override is
# needed, `core` already matches `runtime/core/`'s own basename. Built the
# same way any other `--extern` dependency is: its own standalone `omgc`
# invocation, producing an object file the final link pulls in alongside
# `mathlib.o`.
build-core:
    mkdir -p target
    RUST_BACKTRACE=1 cargo build
    ./target/debug/omgc -v runtime/core/ -o target/core.o

build-std: build-core
    ./target/debug/omgc -v runtime/std/ --name=std --extern=core:runtime/core/ -o target/std.o

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
# `BufWriter<Stdout>` -- a generic compose instantiation each package emits
# independently -- so this fails to link outright if a compose method built
# from a template gets strong linkage. The `cc` below is the assertion; the
# diff then confirms both packages' output actually survived folding.
build-multi-print: build-std build-plat
    ./target/debug/omgc -v examples/multi_print/printlib/ --name=printlib --extern=core:runtime/core/ --extern=std:runtime/std/ -o target/printlib.o
    ./target/debug/omgc -v examples/multi_print/app/ --extern=core:runtime/core/ --extern=std:runtime/std/ --extern=printlib:examples/multi_print/printlib/ -o target/multi_print.o
    cc -Wl,--gc-sections target/multi_print.o target/printlib.o target/std.o target/core.o target/plat.o -o target/multi_print

test-multi-print: build-multi-print
    ./target/multi_print > target/multi_print.stdout
    diff tests/multi_print.expected target/multi_print.stdout
