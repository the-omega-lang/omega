test-all: build-omgc build-runtime
    @echo "[*] Starting test-runner..."
    ./bin/test-runner

playground: build-omgc build-runtime
    @echo "[*] Running playground..."
    rm target/playground || true
    ./bin/omgc-debug -v playground/ --import=core:runtime/core/ --import=std:runtime/std/ --import=plat:runtime/plat/libc/ -o target/playground.o
    cc target/core.o target/plat.o target/std.o target/playground.o -o target/playground
    ./target/playground


build-runtime: build-core build-plat build-std
    @echo "[*] Runtime built successfully"

build-omgc:
    @echo "[*] Building omgc..."
    cargo build

build-core: build-omgc
    @echo "[*] Building 'core'..."
    ./bin/omgc-debug -v runtime/core/ -o target/core.o

build-plat: build-omgc
    @echo "[*] Building 'plat'..."
    ./bin/omgc-debug -v plat:runtime/plat/libc/ --import=core:runtime/core/ -o target/plat.o

build-std: build-omgc
    @echo "[*] Building 'std'..."
    ./bin/omgc-debug -v runtime/std/ --import=core:runtime/core/ -o target/std.o

