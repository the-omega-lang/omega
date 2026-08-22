test-all: build-omgc build-runtime
    @echo "[*] Starting test-runner..."
    ./bin/test-runner

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
    ./bin/omgc-debug -v runtime/plat/libc/ --name=plat --import=core:runtime/core/ -o target/plat.o

build-std: build-omgc
    @echo "[*] Building 'std'..."
    ./bin/omgc-debug -v runtime/std/ --import=core:runtime/core/ -o target/std.o

