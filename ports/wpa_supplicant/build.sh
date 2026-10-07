#!/bin/sh
# wpa_supplicant (nl80211 driver, control interface, WPA2/WPA3-SAE, PMF,
# OWE, EAP-PEAP/TTLS/TLS) and wpa_cli, static against musl, OpenSSL and
# libnl. Called by tools/install-port.sh with: SRC_DIR BUILD_DIR DEST_DIR
set -e
VER=2.11
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
. "$ROOT/tools/port-lib.sh"
. "$ROOT/tools/port-deps.sh"
SRC="$1"; BUILD="$2"; DEST="$3"
SSL=$(dep_openssl "$SRC")
NL=$(dep_libnl "$SRC")
fetch https://w1.fi/releases/wpa_supplicant-$VER.tar.gz \
    912ea06f74e30a8e36fbb68064d6cdff218d8d591db0fc5d75dee6c81ac7fc0a "$SRC/wpa_supplicant-$VER.tar.gz"
rm -rf "$BUILD/wpa_supplicant-$VER"
tar -xzf "$SRC/wpa_supplicant-$VER.tar.gz" -C "$BUILD"
cd "$BUILD/wpa_supplicant-$VER/wpa_supplicant"
cp "$ROOT/ports/wpa_supplicant/config" .config
make -j"$(nproc)" CC="$ROOT/tools/rustos-cc" \
    EXTRA_CFLAGS="-I$SSL/include -I$NL/include/libnl3" \
    LDFLAGS="-static -L$SSL/lib -L$NL/lib" \
    wpa_supplicant wpa_cli wpa_passphrase >/dev/null
for b in wpa_supplicant wpa_cli wpa_passphrase; do
    install -s -D -m 755 "$b" "$DEST/bin/$b"
done
