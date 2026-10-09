#!/bin/sh
# rust-hello: a Rust std test program (threads, files, processes, a pty,
# calloop, signals, unwinding, sockets, dlopen of libEGL.so.1 and a Wayland
# connection), built with tools/rustos-cargo against the weston port's
# stage (libwayland-client). The rust-std scenario runs it.
# Called by tools/install-port.sh with: SRC_DIR BUILD_DIR DEST_DIR
set -e
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
SRC="$1"; BUILD="$2"; DEST="$3"
STAGE="$ROOT/target/ports/build/weston/stage"
[ -f "$STAGE/usr/local/lib/libwayland-client.so" ] ||
    { echo "rust-hello port: build the weston port first" >&2; exit 1; }
cd "$ROOT/ports/rust-hello"
RUSTOS_STAGE="$STAGE" CARGO_INCREMENTAL=0 "$ROOT/tools/rustos-cargo" build --release --locked \
    --target-dir "$BUILD/target"
mkdir -p "$DEST/bin"
cp "$BUILD/target/x86_64-unknown-linux-musl/release/rust-hello" "$DEST/bin/"
