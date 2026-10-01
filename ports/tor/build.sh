#!/bin/sh
# Tor (client, relay code included) with OpenSSL, libevent and zlib, all
# built against musl and linked statically. Used by the eDEX-DE Privacy
# panel through the `tor` service (svc) and /usr/libexec/edex-de/edex-tor-mode.
# Called by tools/install-port.sh with: SRC_DIR BUILD_DIR DEST_DIR
set -e
TOR=0.4.8.17
OPENSSL=3.0.16
LIBEVENT=2.1.12-stable
ZLIB=1.3.1
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
. "$ROOT/tools/port-lib.sh"
SRC="$1"; BUILD="$2"; DEST="$3"
CC="$ROOT/tools/rustos-cc"
J="-j$(nproc)"
fetch https://zlib.net/fossils/zlib-$ZLIB.tar.gz \
    9a93b2b7dfdac77ceba5a558a580e74667dd6fede4585b91eefb60f03b72df23 "$SRC/zlib-$ZLIB.tar.gz"
fetch https://github.com/openssl/openssl/releases/download/openssl-$OPENSSL/openssl-$OPENSSL.tar.gz \
    57e03c50feab5d31b152af2b764f10379aecd8ee92f16c985983ce4a99f7ef86 "$SRC/openssl-$OPENSSL.tar.gz"
fetch https://github.com/libevent/libevent/releases/download/release-$LIBEVENT/libevent-$LIBEVENT.tar.gz \
    92e6de1be9ec176428fd2367677e61ceffc2ee1cb119035037a27d346b0403bb "$SRC/libevent-$LIBEVENT.tar.gz"
fetch https://dist.torproject.org/tor-$TOR.tar.gz \
    79b4725e1d4b887b9e68fd09b0d2243777d5ce3cd471e538583bcf6f9d8cdb56 "$SRC/tor-$TOR.tar.gz"
PREFIX="$BUILD/deps"
rm -rf "$BUILD/zlib-$ZLIB" "$BUILD/openssl-$OPENSSL" "$BUILD/libevent-$LIBEVENT" "$BUILD/tor-$TOR" "$PREFIX"
for t in zlib-$ZLIB openssl-$OPENSSL libevent-$LIBEVENT tor-$TOR; do
    tar -xzf "$SRC/$t.tar.gz" -C "$BUILD"
done

cd "$BUILD/zlib-$ZLIB"
CC="$CC" ./configure --static --prefix="$PREFIX" >/dev/null
make $J >/dev/null
make install >/dev/null

# no-async: musl has no makecontext/swapcontext.
cd "$BUILD/openssl-$OPENSSL"
CC="$CC" ./Configure linux-x86_64 no-shared no-tests no-async no-engine \
    --prefix="$PREFIX" --libdir=lib --openssldir=/etc/ssl >/dev/null
make $J build_libs >/dev/null
make install_dev >/dev/null

cd "$BUILD/libevent-$LIBEVENT"
./configure --host=x86_64-linux-musl CC="$CC" --prefix="$PREFIX" --disable-shared --enable-static \
    --disable-openssl --disable-samples --disable-libevent-regress >/dev/null
make $J >/dev/null
make install >/dev/null

cd "$BUILD/tor-$TOR"
./configure --host=x86_64-linux-musl CC="$CC" --prefix=/usr --sysconfdir=/etc --localstatedir=/var \
    --enable-static-tor \
    --with-openssl-dir="$PREFIX" --with-libevent-dir="$PREFIX" --with-zlib-dir="$PREFIX" \
    --disable-asciidoc --disable-manpage --disable-html-manual --disable-unittests \
    --disable-systemd --disable-seccomp --disable-libscrypt --disable-lzma --disable-zstd \
    --disable-tool-name-check >/dev/null
make $J src/app/tor src/tools/tor-resolve >/dev/null
strip src/app/tor src/tools/tor-resolve
install -D -m 755 src/app/tor "$DEST/bin/tor"
install -D -m 755 src/tools/tor-resolve "$DEST/bin/tor-resolve"
install -D -m 644 src/config/geoip "$DEST/share/tor/geoip"
install -D -m 644 src/config/geoip6 "$DEST/share/tor/geoip6"
install -D -m 644 LICENSE "$DEST/share/tor/LICENSE"
