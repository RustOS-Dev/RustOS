#!/bin/sh
# eDEX-DE, the RustOS desktop (https://github.com/RustOS-Dev/eDEX-DE-RS):
#   bin/edex-comp, edex-de, edex-greeter, edex-auth
#   share/edex-de/themes, share/applications, libexec/edex-de (Tor helpers)
#   etc/edex-greeter/greeter.toml
# Installed under /usr/local (tools/install-port.sh --initramfs edex-de).
#
# Built for x86_64-unknown-linux-musl, dynamically linked against musl and
# the Wayland stack and Mesa of the weston port's stage (wayland,
# libxkbcommon, libinput, libseat, libudev-zero, pixman, libgbm), found
# through tools/cross/rustos-pkg-config, and the libunwind port's
# libgcc_s.so.1. Install those two ports first. Needs Rust with the
# x86_64-unknown-linux-musl target.
#
# Every binary is built with its default features. edex-comp renders with
# GLES through Mesa's GBM and EGL (libgbm is linked, libEGL is loaded at run
# time; softpipe through kms_swrast when there is no GPU driver) and falls
# back to pixman on DRM dumb buffers when GBM/EGL do not come up
# (EDEX_RENDERER=pixman forces it). edex-de and edex-greeter draw with wgpu,
# which loads Vulkan or EGL at run time: without a Vulkan device they use
# GLES (softpipe in QEMU).
#
# EDEX_SRC=/path/to/eDEX-DE-RS builds that checkout instead of the pinned
# commit. RUSTOS_WESTON_STAGE overrides the weston stage's location.
# Called by tools/install-port.sh with: SRC_DIR BUILD_DIR DEST_DIR
set -e
# The session on Mesa (linux/desktop-edex-session: the shell and the greeter
# on softpipe, the lock screen, edex-comp's GLES output on bochs) needs
# eDEX-DE 197938c or later; older commits still pass linux/desktop-edex.
COMMIT=7daae7ecec54ace772e77e11343a9cb74203e08e
SHA256=a7d30db0cd97ad7a7c98e614a3e7e3512335dd1cc8bb4dd8136390dabad80d0c
TARGET=x86_64-unknown-linux-musl
BINS="edex-comp edex-de edex-greeter edex-auth"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
. "$ROOT/tools/port-lib.sh"
SRC="$1"; BUILD="$2"; DEST="$3"

STAGE="${RUSTOS_WESTON_STAGE:-$ROOT/target/ports/build/weston/stage}"
if [ ! -d "$STAGE/usr/local/lib/pkgconfig" ]; then
    echo "edex-de: no weston stage in $STAGE: build the weston port first (tools/install-port.sh --initramfs weston)" >&2
    exit 1
fi
STAGE="$(cd "$STAGE" && pwd)"
UNWIND="$ROOT/target/ports/libunwind/lib"
if [ ! -f "$UNWIND/libgcc_s.so.1" ]; then
    echo "edex-de: build the libunwind port first (tools/install-port.sh --initramfs libunwind)" >&2
    exit 1
fi

if [ -n "$EDEX_SRC" ]; then
    TREE="$(cd "$EDEX_SRC" && pwd)"
else
    fetch https://github.com/RustOS-Dev/eDEX-DE-RS/archive/$COMMIT.tar.gz $SHA256 \
        "$SRC/edex-de-$COMMIT.tar.gz"
    rm -rf "$BUILD/eDEX-DE-RS-$COMMIT"
    tar -xzf "$SRC/edex-de-$COMMIT.tar.gz" -C "$BUILD"
    TREE="$BUILD/eDEX-DE-RS-$COMMIT"
fi
# Cargo reads .cargo/config.toml from every directory above the one it
# runs in, and RustOS's own (build-std, the x86_64-rustos target) breaks
# a std build for musl ("duplicate lang item"). Build from a copy outside
# the RustOS tree.
case "$TREE/" in
"$ROOT"/*)
    WORK="$(mktemp -d "${TMPDIR:-/tmp}/edex-de-port.XXXXXX")"
    trap 'rm -rf "$WORK"' EXIT
    tar -C "$TREE" --exclude=./target -cf - . | tar -C "$WORK" -xf -
    TREE="$WORK" ;;
esac

"$ROOT/tools/build-musl.sh"
CC="$ROOT/tools/rustos-cc"
LIB="$STAGE/usr/local/lib"
export PKG_CONFIG="$ROOT/tools/cross/rustos-pkg-config" RUSTOS_STAGE="$STAGE"
export PKG_CONFIG_ALLOW_CROSS=1
export CC_x86_64_unknown_linux_musl="$CC"
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER="$CC"
# Dynamic (the libraries above are shared), baseline x86-64 (the kernel
# saves FPU state with FXSAVE), musl's start files from the sysroot
# (through rustos-cc) rather than Rust's, the stage's libraries, and
# libgcc_s (the unwinder Rust's std needs when dynamically linked).
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS="-C target-feature=-crt-static -C target-cpu=x86-64 -C link-self-contained=no -L $LIB -C link-arg=-Wl,-rpath-link,$LIB -L $UNWIND"
export CARGO_TARGET_DIR="$BUILD/target"
cd "$TREE"
WANT="${EDEX_BINS:-$BINS}"
# Every binary with its default features: edex-comp with `gpu` (GLES
# through Mesa's GBM/EGL, pixman as the fallback).
PKGS=""
for b in $WANT; do PKGS="$PKGS -p $b"; done
cargo build --release --locked --target $TARGET $PKGS

OUT="$BUILD/target/$TARGET/release"
for b in $WANT; do
    # Every library a binary needs must come from the stage, musl or libunwind.
    for lib in $(readelf -d "$OUT/$b" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p'); do
        case "$lib" in
        libc.so|libgcc_s.so.1) ;;
        *) if [ ! -e "$LIB/$lib" ]; then
               echo "edex-de: $b needs $lib, which the weston stage does not have" >&2
               exit 1
           fi ;;
        esac
    done
    strip "$OUT/$b"
    install -D -m 755 "$OUT/$b" "$DEST/bin/$b"
done
mkdir -p "$DEST/share/edex-de/themes" "$DEST/share/applications" "$DEST/libexec/edex-de"
install -m 644 themes/*.toml "$DEST/share/edex-de/themes/"
install -m 644 packaging/applications/*.desktop "$DEST/share/applications/"
install -m 755 share/libexec/* "$DEST/libexec/edex-de/"
install -D -m 644 packaging/greeter/greeter.toml "$DEST/etc/edex-greeter/greeter.toml"
install -D -m 644 LICENSE "$DEST/share/edex-de/LICENSE"
