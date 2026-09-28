#!/bin/sh
# curl with mbedTLS, both built against musl (static).
# Called by tools/install-port.sh with: SRC_DIR BUILD_DIR DEST_DIR
set -e
CURL=8.10.1
MBED=3.6.2
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
. "$ROOT/tools/port-lib.sh"
SRC="$1"; BUILD="$2"; DEST="$3"
fetch https://github.com/Mbed-TLS/mbedtls/releases/download/mbedtls-$MBED/mbedtls-$MBED.tar.bz2 \
    8b54fb9bcf4d5a7078028e0520acddefb7900b3e66fec7f7175bb5b7d85ccdca "$SRC/mbedtls-$MBED.tar.bz2"
fetch https://curl.se/download/curl-$CURL.tar.xz \
    73a4b0e99596a09fa5924a4fb7e4b995a85fda0d18a2c02ab9cf134bebce04ee "$SRC/curl-$CURL.tar.xz"
PREFIX="$BUILD/deps"
rm -rf "$BUILD/mbedtls-$MBED" "$BUILD/curl-$CURL" "$PREFIX"
tar -xjf "$SRC/mbedtls-$MBED.tar.bz2" -C "$BUILD"
tar -xJf "$SRC/curl-$CURL.tar.xz" -C "$BUILD"
make -C "$BUILD/mbedtls-$MBED" -j"$(nproc)" CC="$ROOT/tools/rustos-cc" AR=ar lib >/dev/null
make -C "$BUILD/mbedtls-$MBED" DESTDIR="$PREFIX" install >/dev/null
cd "$BUILD/curl-$CURL"
./configure --host=x86_64-linux-musl CC="$ROOT/tools/rustos-cc" --disable-shared --enable-static \
    --with-mbedtls="$PREFIX" --without-libpsl --without-zlib --without-brotli --without-zstd \
    --without-nghttp2 --without-libidn2 --disable-ldap --disable-rtsp --disable-dict \
    --disable-telnet --disable-tftp --disable-pop3 --disable-imap --disable-smtp \
    --disable-gopher --disable-mqtt --with-ca-bundle=/etc/ssl/certs/ca-certificates.crt \
    LDFLAGS="-static" >/dev/null
make -j"$(nproc)" >/dev/null
install -D -m 755 src/curl "$DEST/bin/curl"
