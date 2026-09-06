test-all: build-omgc build-runtime
    @echo "[*] Starting test-runner..."
    ./bin/test-runner

playground: build-omgc build-runtime
    @echo "[*] Running playground..."
    rm -rf target/playground target/playground-objects
    ./bin/omgc-debug -v playground/ --import=core:runtime/core/ --import=std:runtime/std/ --import=plat:runtime/plat/libc/ -o target/playground-objects
    cc -Wl,--gc-sections $(find target/playground-objects target/core target/plat target/std -name '*.o' | sort) -o target/playground
    ./target/playground


build-runtime: build-core build-plat build-std
    @echo "[*] Runtime built successfully"

build-omgc:
    @echo "[*] Building omgc..."
    cargo build

# Each package owns an output directory of per-source objects. The build is
# not incremental, so the directory is cleared first: a source that was
# deleted must not leave its object behind for the next link.
build-core: build-omgc
    @echo "[*] Building 'core'..."
    rm -rf target/core
    ./bin/omgc-debug -v runtime/core/ -o target/core

build-plat: build-omgc
    @echo "[*] Building 'plat'..."
    rm -rf target/plat
    ./bin/omgc-debug -v plat:runtime/plat/libc/ --import=core:runtime/core/ -o target/plat

build-std: build-omgc
    @echo "[*] Building 'std'..."
    rm -rf target/std
    ./bin/omgc-debug -v runtime/std/ --import=core:runtime/core/ -o target/std
