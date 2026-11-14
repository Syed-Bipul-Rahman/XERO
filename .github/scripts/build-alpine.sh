#!/bin/sh
# Build a static xeroc with LLVM and lld inside, on Alpine Linux (musl).
# Runs in an alpine:3.21 container with the repository at the current
# directory. Usage: build-alpine.sh [test]
set -e
apk add --no-cache bash git build-base cmake ninja python3 curl xz rust cargo \
    llvm18-dev llvm18-static zlib-static zstd-static libxml2-dev ncurses-static linux-headers > /tmp/apk.log 2>&1 \
    || { tail -5 /tmp/apk.log; exit 1; }
src=$PWD

# static lld libraries matching LLVM 18 (Alpine ships lld only as shared libraries)
V=18.1.8
mkdir -p /tmp/llvm-src && cd /tmp/llvm-src
for p in lld libunwind cmake; do
    curl -fsSL "https://github.com/llvm/llvm-project/releases/download/llvmorg-$V/$p-$V.src.tar.xz" | tar xJ
    mv "$p-$V.src" "$p"
done
mkdir -p llvm
# the LLVM CMake exports of Alpine name a few libraries it does not ship
for f in $(grep -ohE "lib/lib[A-Za-z0-9_]+\.a" /usr/lib/llvm18/lib/cmake/llvm/LLVMExports*.cmake | sort -u); do
    [ -e "/usr/lib/llvm18/$f" ] || ar rc "/usr/lib/llvm18/$f"
done
cmake -G Ninja -S lld -B lld-build -DCMAKE_BUILD_TYPE=Release \
    -DLLVM_DIR=/usr/lib/llvm18/lib/cmake/llvm -DLLVM_MAIN_SRC_DIR=/tmp/llvm-src/llvm \
    -DLLVM_INCLUDE_TESTS=OFF -DBUILD_SHARED_LIBS=OFF > /tmp/lld-cmake.log 2>&1 \
    || { echo "::error title=lld cmake::$(tail -30 /tmp/lld-cmake.log | tr '\n' '|')"; exit 1; }
ninja -C lld-build lldCOFF lldELF lldMachO lldWasm lldCommon > /tmp/lld-build.log 2>&1 \
    || { echo "::error title=lld build::$(grep error /tmp/lld-build.log | head -20 | tr '\n' '|')"; exit 1; }
cp lld-build/lib/liblld*.a /usr/lib/llvm18/lib/
mkdir -p /usr/lib/llvm18/include/lld && cp -r lld/include/lld/Common /usr/lib/llvm18/include/lld/

cd "$src"
export XERO_LLVM_CONFIG=/usr/lib/llvm18/bin/llvm-config
export RUSTFLAGS="-C target-feature=+crt-static"
export XERO_LLVM_EXTRA_LIBS="gcc"
if ! cargo build --release --features llvm > /tmp/build.log 2>&1; then
    echo "::error title=alpine build::$(grep -E "error|undefined|cannot find" /tmp/build.log | sort -u | head -30 | tr '\n' '|')"
    exit 1
fi
strip target/release/xeroc
ls -l target/release/xeroc
./target/release/xeroc --version
./target/release/xeroc run examples/hello.xr
if [ "$1" = "test" ]; then
    cargo test --release --features llvm > /tmp/test.log 2>&1 \
        || { echo "::error title=alpine test::$(tail -40 /tmp/test.log | tr '\n' '|')"; exit 1; }
    tail -3 /tmp/test.log
fi
