run-exec DEBUGGER="": build-asm build-exe
    # ld target/hello.o target/shims.o -o target/example # no libc
    cc target/main.o target/mathlib.o target/core.o -o target/example   # with libc
    {{DEBUGGER}} ./target/example firstarg secondarg; echo -e "\nexit code: $?"

build-exe: build-core
    rm target/example || true
    RUST_BACKTRACE=1 cargo build
    ./target/debug/omgc -v examples/extern_lib/ --name=mathlib -o target/mathlib.o
    ./target/debug/omgc -v examples/dev/ --extern=mathlib:examples/extern_lib/ --extern=core:runtime/core/ -o target/main.o

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
