#!/bin/sh
# Build musl (third_party/musl) into the RustOS sysroot at target/sysroot
# and write the GCC specs file used by tools/rustos-cc. Does nothing when
# the sysroot already holds this musl revision.
#
# Usage: tools/build-musl.sh [SYSROOT]
set -e
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$ROOT/third_party/musl"
SYSROOT="${1:-$ROOT/target/sysroot}"
BUILD="$ROOT/target/musl-build"
if [ ! -x "$SRC/configure" ]; then
    echo "build-musl: $SRC is missing (git submodule update --init third_party/musl)" >&2
    exit 1
fi
REV="$(git -C "$SRC" rev-parse HEAD 2>/dev/null || echo unknown)"
STAMP="$SYSROOT/.musl-$REV"
SPECS="$SYSROOT/usr/lib/rustos-gcc.specs"
# musl's specs leave out --eh-frame-hdr, which gcc's own link spec passes:
# without the PT_GNU_EH_FRAME header the unwinder finds no FDEs and C++
# exceptions terminate. (Also fixes sysroots built before this was added.)
add_eh_frame_hdr() {
    grep -q -- '--eh-frame-hdr' "$SPECS" ||
        sed -i 's|-nostdlib %{shared:-shared}|-nostdlib --eh-frame-hdr %{shared:-shared}|' "$SPECS"
}
if [ -f "$STAMP" ]; then
    add_eh_frame_hdr
    exit 0
fi
rm -rf "$BUILD" "$SYSROOT"
mkdir -p "$BUILD" "$SYSROOT"
cd "$BUILD"
# Userland may use SSE: the kernel saves FPU/SSE state per thread.
CC="${HOST_CC:-gcc}" AR="${HOST_AR:-ar}" RANLIB="${HOST_RANLIB:-ranlib}" CFLAGS="-O2 -g0" "$SRC/configure" \
    --target=x86_64 --prefix=/usr --syslibdir=/lib --disable-wrapper >/dev/null
make -j"$(nproc 2>/dev/null || echo 4)" >/dev/null
make install DESTDIR="$SYSROOT" >/dev/null
# Linux UAPI headers (linux/, asm/, ...) from the host, which musl does
# not ship; ports such as BusyBox need them.
for d in linux asm-generic mtd; do
    [ -d "/usr/include/$d" ] && cp -r "/usr/include/$d" "$SYSROOT/usr/include/"
done
for a in /usr/include/x86_64-linux-gnu/asm /usr/include/asm; do
    if [ -d "$a" ]; then
        cp -r "$a" "$SYSROOT/usr/include/asm"
        break
    fi
done
# Programs look for the dynamic linker at /lib/ld-musl-x86_64.so.1.
sh "$SRC/tools/musl-gcc.specs.sh" "$SYSROOT/usr/include" "$SYSROOT/usr/lib" \
    /lib/ld-musl-x86_64.so.1 > "$SPECS"
add_eh_frame_hdr
touch "$STAMP"
echo "build-musl: musl $(cat "$SRC/VERSION") installed in $SYSROOT"
