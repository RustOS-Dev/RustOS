#!/bin/sh
# eDEX-DE, the RustOS desktop (https://github.com/RustOS-Dev/eDEX-DE-RS):
#   bin/edex-comp, edex-de, edex-greeter, edex-auth
#   share/edex-de/themes, share/applications, libexec/edex-de (Tor helpers)
#   etc/edex-greeter/greeter.toml
# Dynamically linked against musl and the desktop libraries of M37-M41
# (wayland, libxkbcommon, libinput, seatd, libudev-zero, libdrm, Mesa's
# GBM/EGL, dbus), which tools/cross/pkg-config finds in the sysroot, and
# the libunwind port's libgcc_s.so.1 (install that port first).
# Needs Rust with the x86_64-unknown-linux-musl target.
#
# EDEX_SRC=/path/to/eDEX-DE-RS builds that checkout instead of the pinned
# commit.
# Called by tools/install-port.sh with: SRC_DIR BUILD_DIR DEST_DIR
set -e
COMMIT=3980d323c0976e40f11c6fbc8fdfb970de5944d8
SHA256=4fd3b26c13cc1c781c67f5524de1245a2b51443f71d47a93cc61522a8f9da545
TARGET=x86_64-unknown-linux-musl
BINS="edex-comp edex-de edex-greeter edex-auth"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
. "$ROOT/tools/port-lib.sh"
SRC="$1"; BUILD="$2"; DEST="$3"
if [ -n "$EDEX_SRC" ]; then
    TREE="$(cd "$EDEX_SRC" && pwd)"
else
    fetch https://github.com/RustOS-Dev/eDEX-DE-RS/archive/$COMMIT.tar.gz $SHA256 \
        "$SRC/edex-de-$COMMIT.tar.gz"
    rm -rf "$BUILD/eDEX-DE-RS-$COMMIT"
    tar -xzf "$SRC/edex-de-$COMMIT.tar.gz" -C "$BUILD"
    TREE="$BUILD/eDEX-DE-RS-$COMMIT"
fi
if [ -z "$EDEX_PKG_CONFIG" ] && [ ! -x "$ROOT/tools/cross/pkg-config" ]; then
    echo "edex-de: tools/cross/pkg-config is missing (the desktop libraries arrive with M37)" >&2
    exit 1
fi
UNWIND="$ROOT/target/ports/libunwind/lib"
if [ ! -f "$UNWIND/libgcc_s.so.1" ]; then
    echo "edex-de: build the libunwind port first (tools/install-port.sh libunwind)" >&2
    exit 1
fi
"$ROOT/tools/build-musl.sh"
CC="$ROOT/tools/rustos-cc"
export PKG_CONFIG="${EDEX_PKG_CONFIG:-$ROOT/tools/cross/pkg-config}" PKG_CONFIG_ALLOW_CROSS=1
export CC_x86_64_unknown_linux_musl="$CC"
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER="$CC"
# Dynamic (the libraries above are shared), baseline x86-64, musl's start
# files from the sysroot (through rustos-cc) rather than Rust's, and
# libgcc_s (the unwinder Rust's std needs when dynamically linked).
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS="-C target-feature=-crt-static -C target-cpu=x86-64 -C link-self-contained=no -L $UNWIND"
export CARGO_TARGET_DIR="$BUILD/target"
cd "$TREE"
PKGS=""
for b in ${EDEX_BINS:-$BINS}; do PKGS="$PKGS -p $b"; done
cargo build --release --locked --target $TARGET $PKGS
OUT="$BUILD/target/$TARGET/release"
for b in ${EDEX_BINS:-$BINS}; do
    strip "$OUT/$b"
    install -D -m 755 "$OUT/$b" "$DEST/bin/$b"
done
mkdir -p "$DEST/share/edex-de/themes" "$DEST/share/applications" "$DEST/libexec/edex-de"
install -m 644 themes/*.toml "$DEST/share/edex-de/themes/"
install -m 644 packaging/applications/*.desktop "$DEST/share/applications/"
install -m 755 share/libexec/* "$DEST/libexec/edex-de/"
install -D -m 644 packaging/greeter/greeter.toml "$DEST/etc/edex-greeter/greeter.toml"
install -D -m 644 LICENSE "$DEST/share/edex-de/LICENSE"
