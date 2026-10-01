#!/bin/sh
# hostapd (nl80211 driver, WPA2/WPA3-SAE, PMF, integrated EAP server) and
# hostapd_cli, static against musl, OpenSSL and libnl. RustOS uses it for
# hotspots and the wifi-hwsim scenarios.
# Called by tools/install-port.sh with: SRC_DIR BUILD_DIR DEST_DIR
set -e
VER=2.11
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
. "$ROOT/tools/port-lib.sh"
. "$ROOT/tools/port-deps.sh"
SRC="$1"; BUILD="$2"; DEST="$3"
SSL=$(dep_openssl "$SRC")
NL=$(dep_libnl "$SRC")
fetch https://w1.fi/releases/hostapd-$VER.tar.gz \
    2b3facb632fd4f65e32f4bf82a76b4b72c501f995a4f62e330219fe7aed1747a "$SRC/hostapd-$VER.tar.gz"
rm -rf "$BUILD/hostapd-$VER"
tar -xzf "$SRC/hostapd-$VER.tar.gz" -C "$BUILD"
cd "$BUILD/hostapd-$VER/hostapd"
cp "$ROOT/ports/hostapd/config" .config
make -j"$(nproc)" CC="$ROOT/tools/rustos-cc" \
    EXTRA_CFLAGS="-I$SSL/include -I$NL/include/libnl3" \
    LDFLAGS="-static -L$SSL/lib -L$NL/lib" \
    hostapd hostapd_cli >/dev/null
for b in hostapd hostapd_cli; do
    install -s -D -m 755 "$b" "$DEST/bin/$b"
done
