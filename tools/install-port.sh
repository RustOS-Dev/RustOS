#!/bin/sh
# Build a port (ports/NAME/build.sh) against the musl sysroot.
#
#   tools/install-port.sh NAME              # stage in target/ports/NAME
#   tools/install-port.sh --initramfs NAME  # also put it in the boot image
#                                           # (under /usr/local; rebuild the
#                                           # kernel afterwards)
#
# Staged files mirror /usr/local: copy them to /storage/usr/local on a
# RustOS drive (or use --initramfs) and add /usr/local/bin to PATH.
set -e
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
INITRAMFS=0
if [ "$1" = "--initramfs" ]; then INITRAMFS=1; shift; fi
NAME="$1"
if [ -z "$NAME" ] || [ ! -f "$ROOT/ports/$NAME/build.sh" ]; then
    echo "usage: $0 [--initramfs] NAME   (ports: $(ls "$ROOT/ports" | tr '\n' ' '))" >&2
    exit 2
fi
"$ROOT/tools/build-musl.sh"
SRC="$ROOT/target/ports/src"
BUILD="$ROOT/target/ports/build/$NAME"
DEST="$ROOT/target/ports/$NAME"
mkdir -p "$SRC" "$BUILD"
rm -rf "$DEST"
sh "$ROOT/ports/$NAME/build.sh" "$SRC" "$BUILD" "$DEST"
echo "install-port: $NAME staged in $DEST"
if [ "$INITRAMFS" = 1 ]; then
    mkdir -p "$ROOT/target/ports-root/usr/local"
    cp -a "$DEST/." "$ROOT/target/ports-root/usr/local/"
    echo "install-port: $NAME added to target/ports-root (included by the next kernel build)"
fi
