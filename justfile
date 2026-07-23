run-exec DEBUGGER="": build-asm build-exe
    # ld target/hello.o target/shims.o -o target/example # no libc
    cc target/main.o target/mathlib.o target/core.o -o target/example   # with libc
    {{DEBUGGER}} ./target/example firstarg secondarg; echo -e "\nexit code: $?"

build-exe: build-core
    rm target/example || true
    RUST_BACKTRACE=1 cargo build
    ./target/debug/omgc -v examples/extern_lib/mathlib.omg -o target/mathlib.o
    ./target/debug/omgc -v examples/dev/main.omg --extern=mathlib:examples/extern_lib/mathlib.omg --extern=core:omega-core/core/core.omg -o target/main.o

# Points straight at `core`'s real root file -- `omgc` recognizes the
# `dir/dir.omg` nested-root convention automatically (`omega-core/core/core.omg`'s
# parent directory is itself named `core`, matching the file's own stem),
# so no sentinel/placeholder path is needed here. Built the same way any
# other `--extern` dependency is: its own standalone `omgc` invocation,
# producing an object file the final link pulls in alongside `mathlib.o`.
build-core:
    mkdir -p target
    RUST_BACKTRACE=1 cargo build
    ./target/debug/omgc -v omega-core/core/core.omg --name=core -o target/core.o

run-asm: build-asm
    ld target/shims.o -o target/shims
    ./target/shims; echo -e "\nexit code: $?"

build-asm:
    mkdir -p target
    rm target/shims target/shims.o || true
    as omega-shims/x86_64-unknown-linux.S -o target/shims.o

clean:
    rm -rf target
