run-exec DEBUGGER="": build-asm build-exe
    # ld target/hello.o target/shims.o -o target/example # no libc
    cc target/main.o target/mathlib.o target/core.o target/plat.o -o target/example   # with libc
    {{DEBUGGER}} ./target/example firstarg secondarg; echo -e "\nexit code: $?"

build-exe: build-core build-plat
    rm target/example || true
    RUST_BACKTRACE=1 cargo build
    ./target/debug/omgc -v examples/extern_lib/ --name=mathlib -o target/mathlib.o
    ./target/debug/omgc -v examples/dev/ --extern=mathlib:examples/extern_lib/ --extern=core:runtime/core/ --extern=plat:runtime/plat/ -o target/main.o

# `plat` is a plain `--extern` package, not `core` -- it gets no eager-
# discovery or ambient-prelude privilege of its own. Registering it here is
# enough for `examples/dev`'s reference to `core::glue::GlobalAllocator` to
# find its glue (`plat::libc::glue::LibcAllocator`), even though nothing in
# `examples/dev` ever imports `plat` itself.
build-plat: build-core
    ./target/debug/omgc -v runtime/plat/ --extern=core:runtime/core/ -o target/plat.o

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

run-asm: build-asm
    ld target/shims.o -o target/shims
    ./target/shims; echo -e "\nexit code: $?"

build-asm:
    mkdir -p target
    rm target/shims target/shims.o || true
    as runtime/shims/x86_64-unknown-linux.S -o target/shims.o

clean:
    rm -rf target
