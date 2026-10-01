#!/bin/sh
# LLVM libunwind as libgcc_s.so.1: the unwinder that Rust programs built
# for x86_64-unknown-linux-musl link against when they are dynamically
# linked (as eDEX-DE is), in place of GCC's libgcc_s. Exports the
# _Unwind_* (Itanium ABI) and unw_* interfaces.
# Called by tools/install-port.sh with: SRC_DIR BUILD_DIR DEST_DIR
set -e
VER=19.1.7
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
. "$ROOT/tools/port-lib.sh"
SRC="$1"; BUILD="$2"; DEST="$3"
fetch https://github.com/llvm/llvm-project/releases/download/llvmorg-$VER/libunwind-$VER.src.tar.xz \
    10e650f436bc416674f01b5e01177405886f4d0f0b2568c6808044eacad52ea0 "$SRC/libunwind-$VER.src.tar.xz"
rm -rf "$BUILD/libunwind-$VER.src"
tar -xJf "$SRC/libunwind-$VER.src.tar.xz" -C "$BUILD"
cd "$BUILD/libunwind-$VER.src"
CC="$ROOT/tools/rustos-cc"
FLAGS="-O2 -fPIC -fvisibility=hidden -funwind-tables -D_LIBUNWIND_IS_NATIVE_ONLY -Iinclude"
OBJS=""
for f in src/UnwindLevel1.c src/UnwindLevel1-gcc-ext.c src/Unwind-sjlj.c; do
    $CC $FLAGS -std=c99 -c "$f" -o "${f%.c}.o"
    OBJS="$OBJS ${f%.c}.o"
done
for f in src/UnwindRegistersSave.S src/UnwindRegistersRestore.S; do
    $CC $FLAGS -c "$f" -o "${f%.S}.o"
    OBJS="$OBJS ${f%.S}.o"
done
$CC $FLAGS -x c++ -std=c++17 -nostdinc++ -fno-exceptions -fno-rtti -c src/libunwind.cpp -o src/libunwind.o
OBJS="$OBJS src/libunwind.o"
$CC -shared -nostdlib -Wl,-soname,libgcc_s.so.1 -o libgcc_s.so.1 $OBJS -lc
ar rcs libunwind.a $OBJS
strip --strip-unneeded libgcc_s.so.1
install -D -m 755 libgcc_s.so.1 "$DEST/lib/libgcc_s.so.1"
ln -sf libgcc_s.so.1 "$DEST/lib/libgcc_s.so"
install -m 644 libunwind.a "$DEST/lib/libunwind.a"
install -D -m 644 LICENSE.TXT "$DEST/share/doc/libunwind/LICENSE"
