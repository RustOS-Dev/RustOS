#!/bin/sh
# LLVM's C++ runtime (libc++, libc++abi, libunwind 18) for musl, installed
# into the build sysroot (target/sysroot), where tools/rustos-c++ finds it:
# headers in usr/include/c++/v1, static PIC libraries in usr/lib. Also
# builds cxxtest, a small C++ conformance program (installed in bin).
# Called by tools/install-port.sh with: SRC_DIR BUILD_DIR DEST_DIR
set -e
V=18.1.8
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
. "$ROOT/tools/port-lib.sh"
SRC="$1"; BUILD="$2"; DEST="$3"
SYSROOT="${RUSTOS_SYSROOT:-$ROOT/target/sysroot}"
URL=https://github.com/llvm/llvm-project/releases/download/llvmorg-$V
fetch $URL/libcxx-$V.src.tar.xz bdecf90be0072bc720fd5c9c8ab061cdb197edd0c8ad3e170dc3e6bfaa49f388 "$SRC/libcxx-$V.src.tar.xz"
fetch $URL/libcxxabi-$V.src.tar.xz 256c30d724eeb72713bc08ae1692f53aaf4ebe8a1d662c92bf59e69d6c53dce9 "$SRC/libcxxabi-$V.src.tar.xz"
fetch $URL/libunwind-$V.src.tar.xz c31577d16978b0da0e472ef751f74893a5b459a7ea4a383b75f7ab93cf1e6877 "$SRC/libunwind-$V.src.tar.xz"
fetch $URL/cmake-$V.src.tar.xz 59badef592dd34893cd319d42b323aaa990b452d05c7180ff20f23ab1b41e837 "$SRC/cmake-$V.src.tar.xz"
fetch $URL/runtimes-$V.src.tar.xz 9997c2e91e5438e2963306ba5019d85b5384b467535632738d8670ced8f07cb3 "$SRC/runtimes-$V.src.tar.xz"
fetch $URL/llvm-$V.src.tar.xz f68cf90f369bc7d0158ba70d860b0cb34dbc163d6ff0ebc6cfa5e515b9b2e28d "$SRC/llvm-$V.src.tar.xz"
# The runtimes build expects the monorepo layout.
T="$BUILD/llvm-project"
rm -rf "$T" "$BUILD/b"
mkdir -p "$T"
for p in libcxx libcxxabi libunwind cmake runtimes; do
    tar -xJf "$SRC/$p-$V.src.tar.xz" -C "$T"
    mv "$T/$p-$V.src" "$T/$p"
done
tar -xJf "$SRC/llvm-$V.src.tar.xz" -C "$T" "llvm-$V.src/cmake" "llvm-$V.src/utils/llvm-lit" 2>/dev/null ||
    tar -xJf "$SRC/llvm-$V.src.tar.xz" -C "$T" "llvm-$V.src/cmake"
mv "$T/llvm-$V.src" "$T/llvm"
RUSTOS_CXX_BOOTSTRAP=1 cmake -G Ninja -S "$T/runtimes" -B "$BUILD/b" \
    -DCMAKE_BUILD_TYPE=Release -DCMAKE_SYSTEM_NAME=Linux -DCMAKE_SYSTEM_PROCESSOR=x86_64 \
    -DCMAKE_C_COMPILER="$ROOT/tools/rustos-cc" -DCMAKE_CXX_COMPILER="$ROOT/tools/rustos-c++" \
    -DCMAKE_ASM_COMPILER="$ROOT/tools/rustos-cc" \
    -DCMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
    -DCMAKE_INSTALL_PREFIX="$SYSROOT/usr" \
    -DLLVM_ENABLE_RUNTIMES="libcxx;libcxxabi;libunwind" -DLLVM_INCLUDE_TESTS=OFF \
    -DLIBCXX_HAS_MUSL_LIBC=ON -DLIBCXX_ENABLE_SHARED=OFF -DLIBCXX_INCLUDE_TESTS=OFF \
    -DLIBCXX_INCLUDE_BENCHMARKS=OFF -DLIBCXX_USE_COMPILER_RT=OFF \
    -DLIBCXX_ENABLE_STATIC_ABI_LIBRARY=OFF -DLIBCXX_CXX_ABI=libcxxabi \
    -DLIBCXXABI_ENABLE_SHARED=OFF -DLIBCXXABI_USE_LLVM_UNWINDER=ON \
    -DLIBCXXABI_USE_COMPILER_RT=OFF -DLIBCXXABI_INCLUDE_TESTS=OFF \
    -DLIBUNWIND_ENABLE_SHARED=OFF -DLIBUNWIND_USE_COMPILER_RT=OFF -DLIBUNWIND_INCLUDE_TESTS=OFF \
    >"$BUILD/cmake.log" 2>&1 || { tail -40 "$BUILD/cmake.log" >&2; exit 1; }
RUSTOS_CXX_BOOTSTRAP=1 ninja -C "$BUILD/b" >"$BUILD/build.log" 2>&1 || { tail -40 "$BUILD/build.log" >&2; exit 1; }
ninja -C "$BUILD/b" install >>"$BUILD/build.log" 2>&1
mkdir -p "$DEST/bin"
"$ROOT/tools/rustos-c++" -O2 -std=c++20 -pthread -o "$DEST/bin/cxxtest" "$ROOT/ports/libcxx/cxxtest.cpp"
strip "$DEST/bin/cxxtest"
