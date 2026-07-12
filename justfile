run-exec DEBUGGER="": build-asm build-exe
    # ld target/hello.o target/shims.o -o target/example # no libc
    cc target/main.o target/mathlib.o -o target/example   # with libc
    {{DEBUGGER}} ./target/example firstarg secondarg; echo -e "\nexit code: $?"

build-exe:
    rm target/example || true
    RUST_BACKTRACE=1 cargo build
    ./target/debug/omgc -v examples/extern_lib/mathlib.omg -o target/mathlib.o
    ./target/debug/omgc -v examples/dev/main.omg --extern=mathlib:examples/extern_lib/mathlib.omg -o target/main.o

run-asm: build-asm
    ld target/shims.o -o target/shims
    ./target/shims; echo -e "\nexit code: $?"

build-asm:
    mkdir -p target
    rm target/shims target/shims.o || true
    as omega-shims/x86_64-unknown-linux.S -o target/shims.o

clean:
    rm -rf target
