#!/bin/sh
# QuickJS-ng (JavaScript engine), statically linked against musl.
# Called by tools/install-port.sh with: SRC_DIR BUILD_DIR DEST_DIR
#
# The source is the QuickJS-ng tree vendored in the rquickjs-sys crate on
# crates.io (QuickJS-ng 0.16.2, unmodified; the crate's own patches are not
# applied). Installs:
#   bin/qjs, bin/qjsc, bin/run-test262   interpreter, bytecode compiler,
#                                        test262 runner
#   lib/libquickjs.a, include/quickjs/   engine + quickjs-libc for embedding
set -e
CRATE=rquickjs-sys-0.14.0
URL=https://static.crates.io/crates/rquickjs-sys/$CRATE.crate
SHA256=cee271d0eeba64f0915b846cb7ae02e16faf3dfdffdca91731101d9d30fe3423
SRC="$1"; BUILD="$2"; DEST="$3"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
. "$ROOT/tools/port-lib.sh"
fetch "$URL" "$SHA256" "$SRC/$CRATE.crate"
rm -rf "$BUILD/$CRATE"
tar -xzf "$SRC/$CRATE.crate" -C "$BUILD" "$CRATE/quickjs"
cd "$BUILD/$CRATE/quickjs"
CC="$ROOT/tools/rustos-cc"
CFLAGS="-O2 -D_GNU_SOURCE -DQJS_BUILD_LIBC -Wno-array-bounds -Wno-format-truncation"
LIB="quickjs.c libregexp.c libunicode.c dtoa.c quickjs-libc.c"
for f in $LIB; do
    $CC $CFLAGS -c "$f" -o "${f%.c}.o"
done
ar rcs libquickjs.a quickjs.o libregexp.o libunicode.o dtoa.o quickjs-libc.o
$CC $CFLAGS -static -o qjs qjs.c gen/repl.c gen/standalone.c libquickjs.a -lm
$CC $CFLAGS -static -o qjsc qjsc.c libquickjs.a -lm
$CC $CFLAGS -static -o run-test262 run-test262.c libquickjs.a -lm -lpthread
install -D -m 755 qjs "$DEST/bin/qjs"
install -D -m 755 qjsc "$DEST/bin/qjsc"
install -D -m 755 run-test262 "$DEST/bin/run-test262"
install -D -m 644 libquickjs.a "$DEST/lib/libquickjs.a"
for h in quickjs.h quickjs-libc.h; do
    install -D -m 644 "$h" "$DEST/include/quickjs/$h"
done
install -D -m 644 LICENSE "$DEST/share/doc/quickjs/LICENSE"
