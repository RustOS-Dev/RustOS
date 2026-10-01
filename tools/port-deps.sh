# Libraries several ports link against, built once (static, musl) under
# target/ports/deps/NAME-VERSION and reused. Source after tools/port-lib.sh.
#   dep_openssl SRC_DIR  -> prints the install prefix
#   dep_libnl SRC_DIR    -> prints the install prefix

DEPS_ROOT="$ROOT/target/ports/deps"

OPENSSL=3.5.4
LIBNL=3.11.0

dep_openssl() {
    prefix="$DEPS_ROOT/openssl-$OPENSSL"
    if [ ! -f "$prefix/.done" ]; then
        fetch https://github.com/openssl/openssl/releases/download/openssl-$OPENSSL/openssl-$OPENSSL.tar.gz \
            967311f84955316969bdb1d8d4b983718ef42338639c621ec4c34fddef355e99 "$1/openssl-$OPENSSL.tar.gz"
        b="$DEPS_ROOT/build/openssl-$OPENSSL"
        rm -rf "$b" "$prefix"
        mkdir -p "$DEPS_ROOT/build"
        tar -xzf "$1/openssl-$OPENSSL.tar.gz" -C "$DEPS_ROOT/build"
        (cd "$b" && CC="$ROOT/tools/rustos-cc" ./Configure linux-x86_64 no-shared no-tests \
            no-docs no-apps no-engine no-dso no-async no-afalgeng no-module \
            --prefix="$prefix" --libdir=lib --openssldir=/etc/ssl >/dev/null &&
            make -j"$(nproc)" build_libs >/dev/null && make install_dev >/dev/null) >&2 || exit 1
        touch "$prefix/.done"
    fi
    echo "$prefix"
}

dep_libnl() {
    prefix="$DEPS_ROOT/libnl-$LIBNL"
    if [ ! -f "$prefix/.done" ]; then
        fetch https://github.com/thom311/libnl/releases/download/libnl3_${LIBNL%%.*}_$(echo "$LIBNL" | cut -d. -f2)_$(echo "$LIBNL" | cut -d. -f3)/libnl-$LIBNL.tar.gz \
            2a56e1edefa3e68a7c00879496736fdbf62fc94ed3232c0baba127ecfa76874d "$1/libnl-$LIBNL.tar.gz"
        b="$DEPS_ROOT/build/libnl-$LIBNL"
        rm -rf "$b" "$prefix"
        mkdir -p "$DEPS_ROOT/build"
        tar -xzf "$1/libnl-$LIBNL.tar.gz" -C "$DEPS_ROOT/build"
        (cd "$b" && ./configure --host=x86_64-linux-musl CC="$ROOT/tools/rustos-cc" \
            --prefix="$prefix" --disable-shared --enable-static --disable-cli \
            --disable-debug >/dev/null &&
            make -j"$(nproc)" >/dev/null 2>&1 && make install >/dev/null 2>&1) >&2 || exit 1
        touch "$prefix/.done"
    fi
    echo "$prefix"
}
