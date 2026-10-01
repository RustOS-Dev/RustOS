#!/bin/sh
# wireguard-tools: `wg` (keys, interface configuration over generic
# netlink), statically linked against musl (wg-quick needs bash and
# iproute2; NetworkManager configures tunnels instead). The tunnel
# itself needs the kernel's WireGuard driver (LinuxKPI group `wireguard`).
# Called by tools/install-port.sh with: SRC_DIR BUILD_DIR DEST_DIR
set -e
VER=1.0.20250521
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
. "$ROOT/tools/port-lib.sh"
SRC="$1"; BUILD="$2"; DEST="$3"
fetch https://git.zx2c4.com/wireguard-tools/snapshot/wireguard-tools-$VER.tar.xz \
    61f520e7c1664ae9301fa36a2b8e90cf4680887a71f456c290d5d8b879f1e2e6 "$SRC/wireguard-tools-$VER.tar.xz"
rm -rf "$BUILD/wireguard-tools-$VER"
tar -xJf "$SRC/wireguard-tools-$VER.tar.xz" -C "$BUILD"
cd "$BUILD/wireguard-tools-$VER/src"
make -j"$(nproc)" CC="$ROOT/tools/rustos-cc" LDFLAGS="-static" wg >/dev/null
strip wg
install -D -m 755 wg "$DEST/bin/wg"
