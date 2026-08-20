backend := "cranelift"

omgc-features := if backend == "llvm" {
    "--features llvm"
} else {
    ""
}

test-all: build-omgc build-runtime
    @echo "[*] Starting test-runner..."
    ./bin/test-runner

build-runtime: build-core build-std build-plat
    @echo "[*] Runtime built successfully"

build-omgc:
    @echo "[*] Building omgc ({{backend}})..."
    cargo build {{omgc-features}}

build-core: build-omgc
    @echo "[*] Building 'core'..."
    ./bin/omgc-debug -v runtime/plat/libc/ --name=plat --extern=core:runtime/core/ -o target/plat.o

build-plat: build-omgc
    @echo "[*] Building 'plat'..."
    ./bin/omgc-debug -v runtime/plat/libc/ --name=plat --extern=core:runtime/core/ -o target/plat.o

build-std: build-omgc
    @echo "[*] Building 'std'..."
    ./bin/omgc-debug -v runtime/std/ --extern=core:runtime/core/ -o target/std.o

### OLD JUSTFILE TASKS ###
# Below, we have *exclusively* old tasks
# They have been adapted and are kept here
# for short-time reference. Soon enough, they
# will be gone. Before though, there is more
# work to do when it comes to the testing of
# this project. The tasks above this section
# are new and gonna be used for the newer tests.

__old_run-exec DEBUGGER="": __old_build-asm __old_build-exe
    # ld target/hello.o target/shims.o -o target/example # no libc
    cc -Wl,--gc-sections target/main.o target/mathlib.o target/core.o target/std.o target/plat.o -o target/example   # with libc
    {{DEBUGGER}} ./target/example firstarg secondarg; echo -e "\nexit code: $?"

__old_build-exe: __old_build-core __old_build-plat __old_build-std
    rm target/example || true
    RUST_BACKTRACE=1 cargo build
    ./target/debug/omgc -v oldtests/examples/mathlib/ -o target/mathlib.o
    ./target/debug/omgc -v oldtests/examples/dev/ --extern=mathlib:oldtests/examples/mathlib/ --extern=core:runtime/core/ --extern=std:runtime/std/ --extern=plat:runtime/plat/libc/ -o target/main.o

# `runtime/plat/` is a plain directory, not a package -- each subdirectory
# under it (just `libc/` today) is its own independent, honestly-named
# package (`runtime/plat/libc/libc.omg`) that presents as the *same*
# declared identity `plat` purely via `--name=`/`--extern=plat:...`, never
# by renaming its own files (see docs/guide/platform-glue.md). Picking a
# platform is exactly choosing which directory these two flags point at --
# there is no compiler-level selection mechanism, this is it. `plat` gets
# no other privilege `core` has (no eager-discovery/ambient-prelude
# exemption); registering it is enough for `oldtests/examples/dev`'s reference to
# `core::platform::GlobalAllocator` to find its glue, even though nothing in
# `oldtests/examples/dev` ever imports `plat` itself.
__old_build-plat: __old_build-core
    ./target/debug/omgc -v runtime/plat/libc/ --name=plat --extern=core:runtime/core/ -o target/plat.o

# `omgc` takes a package root directory. That directory is its root module:
# `<dir>/<basename>.omg`, when present, owns the root and the root's other
# entries are its children. Without that file the root is a namespace-only
# module. `main` in the root module, not a special filename, receives the C
# entry symbol.
__old_build-core:
    mkdir -p target
    RUST_BACKTRACE=1 cargo build
    ./target/debug/omgc -v runtime/core/ -o target/core.o

__old_build-std: __old_build-core
    ./target/debug/omgc -v runtime/std/ --extern=core:runtime/core/ -o target/std.o

__old_build-io-demo: __old_build-std __old_build-plat
    ./target/debug/omgc -v oldtests/examples/io_demo/ --extern=core:runtime/core/ --extern=std:runtime/std/ --extern=plat:runtime/plat/libc/ -o target/io_demo.o
    cc -Wl,--gc-sections target/io_demo.o target/core.o target/std.o target/plat.o -o target/io_demo

__old_test-io: __old_build-io-demo
    ./target/io_demo < oldtests/tests/io_demo.stdin > target/io_demo.stdout
    diff oldtests/tests/io_demo.expected target/io_demo.stdout

__old_build-stdio-contract: __old_build-std __old_build-plat
    ./target/debug/omgc -v oldtests/examples/stdio_contract/ --extern=core:runtime/core/ --extern=std:runtime/std/ --extern=plat:runtime/plat/libc/ -o target/stdio_contract.o
    cc -Wl,--gc-sections target/stdio_contract.o target/core.o target/std.o target/plat.o -o target/stdio_contract

__old_test-stdio-contract: __old_build-stdio-contract
    ./target/stdio_contract > target/stdio_contract.stdout
    diff oldtests/tests/stdio_contract.expected target/stdio_contract.stdout

__old_build-core-only: __old_build-core
    ./target/debug/omgc -v oldtests/examples/core_only/ --extern=core:runtime/core/ -o target/core_only.o
    cc -Wl,--gc-sections target/core_only.o target/core.o -o target/core_only

__old_test-core-only: __old_build-core-only
    ./target/core_only
    ! readelf -rW target/core.o | rg 'Standard(Output|Error|Input)|GlobalAllocator'

# Ranges are ordinary `core` values, so this needs nothing but `core` -- which
# is itself the assertion that range iteration implies no allocator and no
# platform glue. `range_demo` returns a distinct exit code per failed case.
__old_build-range: __old_build-core
    ./target/debug/omgc -v oldtests/examples/range_demo/ --extern=core:runtime/core/ -o target/range_demo.o
    cc -Wl,--gc-sections target/range_demo.o target/core.o -o target/range_demo

__old_test-range: __old_build-range
    ./target/range_demo

# `char`'s semantics need execution to mean anything -- whether `from_u32`
# actually rejects a surrogate, whether `Successor` skips the hole. Needs only
# `core`, which is also the assertion that none of it pulls in an allocator.
__old_build-char: __old_build-core
    ./target/debug/omgc -v oldtests/examples/char_demo/ --extern=core:runtime/core/ -o target/char_demo.o
    cc -Wl,--gc-sections target/char_demo.o target/core.o -o target/char_demo

__old_test-char: __old_build-char
    ./target/char_demo

# Spec composition is assertion-heavy precisely because so much of it is
# *selection* -- which blanket wins, which spec's same-named body a call
# reaches, which vtable section a narrowing cast lands on. None of that is
# observable at compile time, so this demo asserts each case by execution and
# returns a distinct exit code per failed one. Needs only `core`: specs,
# blankets and narrowing casts must imply no allocator and no platform glue.
__old_build-spec-dispatch: __old_build-core
    ./target/debug/omgc -v oldtests/examples/spec_dispatch/ --extern=core:runtime/core/ -o target/spec_dispatch.o
    cc -Wl,--gc-sections target/spec_dispatch.o target/core.o -o target/spec_dispatch

__old_test-spec-dispatch: __old_build-spec-dispatch
    ./target/spec_dispatch

# The three-tier spec-function call ladder (`S::fn()` / `P::fn(...)` /
# `<S : P>::fn(...)`) is assertion-heavy for the same reason: which
# conform's body a spelling reaches is a runtime fact. Needs `std` for the
# real `Default::default()` case; everything else is `core`-only.
__old_build-spec-calls: __old_build-core __old_build-std
    ./target/debug/omgc -v oldtests/examples/spec_calls/ --extern=core:runtime/core/ --extern=std:runtime/std/ -o target/spec_calls.o
    cc -Wl,--gc-sections target/spec_calls.o target/core.o target/std.o -o target/spec_calls

__old_test-spec-calls: __old_build-spec-calls
    ./target/spec_calls

# Only `main` in the root module may receive the bare C entry symbol; a child
# module's identically named function remains normally mangled. Compiled with
# no `--extern` at all, so this also covers a package that never registers
# `core`.
__old_build-root-layout:
    mkdir -p target
    RUST_BACKTRACE=1 cargo build
    ./target/debug/omgc -v oldtests/examples/root_layout/ -o target/root_layout.o

# The second assertion spells the child's *whole* mangled path out on
# purpose: a bare `4main` match would still pass if discovery regressed to
# treating the root directory as a container, which would make `nested` a
# top-level module (`C6nested`) instead of `root_layout::nested`
# (`NtC11root_layout6nested`).
__old_test-root-layout: __old_build-root-layout
    test "$(nm --defined-only target/root_layout.o | rg -c ' main$')" = 1
    nm --defined-only target/root_layout.o | rg '_omg_NvNtC11root_layout6nested4main'

# `std` allocation works with only allocator glue; this target deliberately
# omits `plat.o`, so any retained console path is a link failure.
__old_build-allocator-only: __old_build-std
    ./target/debug/omgc -v oldtests/examples/allocator_only/ --extern=core:runtime/core/ --extern=std:runtime/std/ -o target/allocator_only.o
    cc -Wl,--gc-sections target/allocator_only.o target/core.o target/std.o -o target/allocator_only

# The link above is the primary assertion: it omits `plat.o`, so a retained
# console path is an undefined-reference failure before this recipe runs at
# all. The check below is the secondary one -- that `--gc-sections` actually
# *dropped* the console code rather than the link having succeeded for some
# unrelated reason. Deliberately over *defined* symbols: `nm -u` on a linked
# executable can never report anything, so asserting on it proves nothing.
__old_test-allocator-only: __old_build-allocator-only
    ./target/allocator_only
    ! nm --defined-only target/allocator_only | rg 'Std(out|err|in).*(Write5write|Read4read)'

__old_run-asm: __old_build-asm
    ld target/shims.o -o target/shims
    ./target/shims; echo -e "\nexit code: $?"

__old_build-asm:
    mkdir -p target
    rm target/shims target/shims.o || true
    as runtime/shims/x86_64-unknown-linux.S -o target/shims.o

__old_clean:
    rm -rf target

# Two packages that both print, linked together. `println$` expands to a
# `BufWriter<Stdout>` -- a generic conform instantiation each package emits
# independently -- so this fails to link outright if a conform method built
# from a template gets strong linkage. The `cc` below is the assertion; the
# diff then confirms both packages' output actually survived folding.
__old_build-multi-print: __old_build-std __old_build-plat
    ./target/debug/omgc -v oldtests/examples/multi_print/printlib/ --name=printlib --extern=core:runtime/core/ --extern=std:runtime/std/ -o target/printlib.o
    ./target/debug/omgc -v oldtests/examples/multi_print/app/ --extern=core:runtime/core/ --extern=std:runtime/std/ --extern=printlib:oldtests/examples/multi_print/printlib/ -o target/multi_print.o
    cc -Wl,--gc-sections target/multi_print.o target/printlib.o target/std.o target/core.o target/plat.o -o target/multi_print

__old_test-multi-print: __old_build-multi-print
    ./target/multi_print > target/multi_print.stdout
    diff oldtests/tests/multi_print.expected target/multi_print.stdout

# --- the LLVM backend's own gates -----------------------------------------

# The LLVM gates run the same programs through `--backend=llvm`, at both
# `-O0` and `-O3` -- a wrong explicit alignment passes every functional
# test at `-O0` and fails at `-O3`, so one opt level alone proves nothing.
__old_llvm-opt := "-O0"

__old_build-llvm:
    RUST_BACKTRACE=1 cargo build --features llvm

__old_build-core-llvm: __old_build-llvm
    ./target/debug/omgc -v runtime/core/ -o target/core-llvm.o --backend=llvm {{__old_llvm-opt}}

__old_build-std-llvm: __old_build-core-llvm
    ./target/debug/omgc -v runtime/std/ --extern=core:runtime/core/ -o target/std-llvm.o --backend=llvm {{__old_llvm-opt}}

__old_build-plat-llvm: __old_build-core-llvm
    ./target/debug/omgc -v runtime/plat/libc/ --name=plat --extern=core:runtime/core/ -o target/plat-llvm.o --backend=llvm {{__old_llvm-opt}}

# The whole-program LLVM counterpart of `run-exec` (expects the same 69).
# Unlike `run-exec`, which is a demo recipe you read the output of, this one
# is a *gate* -- so it asserts the exit code rather than echoing it. `;
# test $? = 69`, never `; echo $?` (the recipe's status becomes the `echo`'s,
# so it can never fail -- it reported a passing segfault for exactly that
# reason) and never `|| test $? = 69` either (`||` never runs when the
# program *succeeds*, so a program that wrongly exited 0 would pass too).
__old_run-exec-llvm: __old_build-std-llvm __old_build-plat-llvm
    ./target/debug/omgc -v oldtests/examples/mathlib/ -o target/mathlib-llvm.o --backend=llvm {{__old_llvm-opt}}
    ./target/debug/omgc -v oldtests/examples/dev/ --extern=mathlib:oldtests/examples/mathlib/ --extern=core:runtime/core/ --extern=std:runtime/std/ --extern=plat:runtime/plat/libc/ -o target/main-llvm.o --backend=llvm {{__old_llvm-opt}}
    cc -Wl,--gc-sections target/main-llvm.o target/mathlib-llvm.o target/core-llvm.o target/std-llvm.o target/plat-llvm.o -o target/example-llvm
    ./target/example-llvm firstarg secondarg; test $? = 69

__old_test-core-only-llvm: __old_build-core-llvm
    ./target/debug/omgc -v oldtests/examples/core_only/ --extern=core:runtime/core/ -o target/core_only-llvm.o --backend=llvm {{__old_llvm-opt}}
    cc -Wl,--gc-sections target/core_only-llvm.o target/core-llvm.o -o target/core_only-llvm
    ./target/core_only-llvm

__old_test-range-llvm: __old_build-core-llvm
    ./target/debug/omgc -v oldtests/examples/range_demo/ --extern=core:runtime/core/ -o target/range_demo-llvm.o --backend=llvm {{__old_llvm-opt}}
    cc -Wl,--gc-sections target/range_demo-llvm.o target/core-llvm.o -o target/range_demo-llvm
    ./target/range_demo-llvm

__old_test-char-llvm: __old_build-core-llvm
    ./target/debug/omgc -v oldtests/examples/char_demo/ --extern=core:runtime/core/ -o target/char_demo-llvm.o --backend=llvm {{__old_llvm-opt}}
    cc -Wl,--gc-sections target/char_demo-llvm.o target/core-llvm.o -o target/char_demo-llvm
    ./target/char_demo-llvm

__old_test-spec-dispatch-llvm: __old_build-core-llvm
    ./target/debug/omgc -v oldtests/examples/spec_dispatch/ --extern=core:runtime/core/ -o target/spec_dispatch-llvm.o --backend=llvm {{__old_llvm-opt}}
    cc -Wl,--gc-sections target/spec_dispatch-llvm.o target/core-llvm.o -o target/spec_dispatch-llvm
    ./target/spec_dispatch-llvm

__old_test-spec-calls-llvm: __old_build-core-llvm __old_build-std-llvm
    ./target/debug/omgc -v oldtests/examples/spec_calls/ --extern=core:runtime/core/ --extern=std:runtime/std/ -o target/spec_calls-llvm.o --backend=llvm {{__old_llvm-opt}}
    cc -Wl,--gc-sections target/spec_calls-llvm.o target/core-llvm.o target/std-llvm.o -o target/spec_calls-llvm
    ./target/spec_calls-llvm

__old_test-root-layout-llvm: __old_build-llvm
    ./target/debug/omgc -v oldtests/examples/root_layout/ -o target/root_layout-llvm.o --backend=llvm {{__old_llvm-opt}}
    test "$(nm --defined-only target/root_layout-llvm.o | rg -c ' main$')" = 1
    nm --defined-only target/root_layout-llvm.o | rg '_omg_NvNtC11root_layout6nested4main'

__old_test-allocator-only-llvm: __old_build-std-llvm
    ./target/debug/omgc -v oldtests/examples/allocator_only/ --extern=core:runtime/core/ --extern=std:runtime/std/ -o target/allocator_only-llvm.o --backend=llvm {{__old_llvm-opt}}
    cc -Wl,--gc-sections target/allocator_only-llvm.o target/core-llvm.o target/std-llvm.o -o target/allocator_only-llvm
    ./target/allocator_only-llvm
    ! nm --defined-only target/allocator_only-llvm | rg 'Std(out|err|in).*(Write5write|Read4read)'

__old_test-io-llvm: __old_build-std-llvm __old_build-plat-llvm
    ./target/debug/omgc -v oldtests/examples/io_demo/ --extern=core:runtime/core/ --extern=std:runtime/std/ --extern=plat:runtime/plat/libc/ -o target/io_demo-llvm.o --backend=llvm {{__old_llvm-opt}}
    cc -Wl,--gc-sections target/io_demo-llvm.o target/core-llvm.o target/std-llvm.o target/plat-llvm.o -o target/io_demo-llvm
    ./target/io_demo-llvm < oldtests/tests/io_demo.stdin > target/io_demo-llvm.stdout
    diff oldtests/tests/io_demo.expected target/io_demo-llvm.stdout

__old_test-stdio-contract-llvm: __old_build-std-llvm __old_build-plat-llvm
    ./target/debug/omgc -v oldtests/examples/stdio_contract/ --extern=core:runtime/core/ --extern=std:runtime/std/ --extern=plat:runtime/plat/libc/ -o target/stdio_contract-llvm.o --backend=llvm {{__old_llvm-opt}}
    cc -Wl,--gc-sections target/stdio_contract-llvm.o target/core-llvm.o target/std-llvm.o target/plat-llvm.o -o target/stdio_contract-llvm
    ./target/stdio_contract-llvm > target/stdio_contract-llvm.stdout
    diff oldtests/tests/stdio_contract.expected target/stdio_contract-llvm.stdout

__old_test-multi-print-llvm: __old_build-std-llvm __old_build-plat-llvm
    ./target/debug/omgc -v oldtests/examples/multi_print/printlib/ --name=printlib --extern=core:runtime/core/ --extern=std:runtime/std/ -o target/printlib-llvm.o --backend=llvm {{__old_llvm-opt}}
    ./target/debug/omgc -v oldtests/examples/multi_print/app/ --extern=core:runtime/core/ --extern=std:runtime/std/ --extern=printlib:oldtests/examples/multi_print/printlib/ -o target/multi_print-llvm.o --backend=llvm {{__old_llvm-opt}}
    cc -Wl,--gc-sections target/multi_print-llvm.o target/printlib-llvm.o target/std-llvm.o target/core-llvm.o target/plat-llvm.o -o target/multi_print-llvm
    ./target/multi_print-llvm > target/multi_print-llvm.stdout
    diff oldtests/tests/multi_print.expected target/multi_print-llvm.stdout

# A Cranelift `core.o` linked against an LLVM `main.o` -- the mixed-backend
# link, which is the whole point of the shared seam (symbols, linkage, and
# the calling convention must agree across backends or this fails to link,
# let alone run).
__old_build-mixed: __old_build-core
    RUST_BACKTRACE=1 cargo build --features llvm
    ./target/debug/omgc -v oldtests/examples/core_only/ --extern=core:runtime/core/ -o target/core_only-mixed.o --backend=llvm

__old_test-mixed: __old_build-mixed
    cc -Wl,--gc-sections target/core_only-mixed.o target/core.o -o target/core_only-mixed
    ./target/core_only-mixed

# The whole LLVM suite, at both opt levels. A `__old_llvm-opt=` override has to
# come *before* the recipe names -- `just recipe __old_llvm-opt=-O3` is parsed as
# a recipe *argument*, not an assignment, so the -O3 half silently never ran
# when it was written that way round.
__old_test-llvm: __old_build-llvm
    just test-core-only-llvm test-range-llvm test-char-llvm test-spec-dispatch-llvm test-spec-calls-llvm test-root-layout-llvm test-allocator-only-llvm test-io-llvm test-stdio-contract-llvm test-multi-print-llvm run-exec-llvm
    just __old_llvm-opt="-O3" test-core-only-llvm test-range-llvm test-char-llvm test-spec-dispatch-llvm test-spec-calls-llvm test-root-layout-llvm test-allocator-only-llvm test-io-llvm test-stdio-contract-llvm test-multi-print-llvm run-exec-llvm
