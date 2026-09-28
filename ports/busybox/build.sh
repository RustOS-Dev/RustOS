#!/bin/sh
# BusyBox, statically linked against musl.
# Called by tools/install-port.sh with: SRC_DIR BUILD_DIR DEST_DIR
set -e
VERSION=1.36.1
URL=https://busybox.net/downloads/busybox-$VERSION.tar.bz2
SHA256=b8cc24c9574d809e7279c3be349795c5d5ceb6fdf19ca709f80cde50e47de314
SRC="$1"; BUILD="$2"; DEST="$3"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
. "$ROOT/tools/port-lib.sh"
fetch "$URL" "$SHA256" "$SRC/busybox-$VERSION.tar.bz2"
rm -rf "$BUILD/busybox-$VERSION"
tar -xjf "$SRC/busybox-$VERSION.tar.bz2" -C "$BUILD"
cd "$BUILD/busybox-$VERSION"
make defconfig >/dev/null
# Static, and without applets that need kernel features RustOS lacks
# (or that do not build against musl headers).
set_config() { sed -i "s/^# $1 is not set\$/$1=y/; s/^$1=.*/$1=$2/" .config; }
set_config CONFIG_STATIC y
for opt in TC FEATURE_TC_INGRESS SELINUX FEATURE_HAVE_RPC FEATURE_MOUNT_NFS FEATURE_INETD_RPC \
           SWAPON SWAPOFF NSENTER UNSHARE LINUXRC HWCLOCK; do
    sed -i "s/^CONFIG_$opt=y/# CONFIG_$opt is not set/" .config
done
yes "" | make oldconfig >/dev/null
make -j"$(nproc)" CC="$ROOT/tools/rustos-cc" HOSTCC=gcc busybox >/dev/null
install -D -m 755 busybox "$DEST/bin/busybox"
