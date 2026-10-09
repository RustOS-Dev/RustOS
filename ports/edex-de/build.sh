#!/bin/sh
# eDEX-DE, RustOS's desktop (M43): the sci-fi desktop shell from RustOS-Dev/eDEX-DE-RS at a
# pinned commit, built with tools/rustos-cargo (Rust std, musl) against the labwc port's
# libraries (libwayland, libxkbcommon, Mesa's EGL for wgpu's GLES backend), plus D-Bus 1.16
# (dbus-daemon for a session bus, so its notifications server works), the JetBrains Mono Nerd
# Font, the session (edex-session, desktop) and their configuration. Installs to /usr/local:
#   bin/edex-de, bin/edex-greeter, bin/edex-session, bin/desktop, bin/dbus-daemon, ...
#   share/edex-de/{themes,assets,greeter,config.toml}, share/applications/edex-*.desktop
#   share/fonts/jetbrains-mono-nerd/, share/licenses/{edex-de,jetbrains-mono-nerd}/
# Called by tools/install-port.sh with: SRC_DIR BUILD_DIR DEST_DIR
#
# Builds on the labwc port (tools/install-port.sh labwc first; it builds on weston), and needs
# the libcxx port in the sysroot (LLVM's libunwind for Rust panics).
set -e
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
. "$ROOT/tools/port-lib.sh"
SRC="$1"; BUILD="$2"; DEST="$3"
BASE="$ROOT/target/ports/build/labwc"
[ -f "$BASE/stamps/labwc" ] || { echo "edex-de port: build the labwc port first" >&2; exit 1; }
STAGE="$BUILD/stage"
STAMPS="$BUILD/stamps"
mkdir -p "$STAGE" "$STAMPS"
if [ ! -f "$STAMPS/base" ]; then
    rm -rf "$STAGE" "$STAMPS"
    mkdir -p "$STAMPS"
    cp -a "$BASE/stage" "$STAGE"
    find "$STAGE" -type f > "$BUILD/base-files"
    touch "$STAMPS/base"
fi
export RUSTOS_STAGE="$STAGE"
CROSS="$BUILD/cross.ini"
sed "s|@ROOT@|$ROOT|g" "$ROOT/tools/cross/meson-cross.ini.in" > "$CROSS"
MESON="${MESON:-meson}"
command -v "$MESON" >/dev/null || MESON="$HOME/.local/bin/meson"
export LDFLAGS="-L$STAGE/usr/local/lib -Wl,-rpath-link,$STAGE/usr/local/lib"
export CPPFLAGS="-I$STAGE/usr/local/include"
built() { [ -f "$STAMPS/$1" ]; }
mark() { touch "$STAMPS/$1"; }

# eDEX-DE-RS: the RustOS port branch (window-manager backends with labwc, the RustOS system
# backends, the local greeter backend).
EDEX_REPO=https://github.com/RustOS-Dev/eDEX-DE-RS
EDEX_COMMIT=b72acca4933c4f351ca9308a51975389cb3c2850
DBUS=1.16.2
NERD=3.4.0

fetch https://dbus.freedesktop.org/releases/dbus/dbus-$DBUS.tar.xz \
    0ba2a1a4b16afe7bceb2c07e9ce99a8c2c3508e5dec290dbb643384bd6beb7e2 "$SRC/dbus-$DBUS.tar.xz"
fetch https://github.com/ryanoasis/nerd-fonts/releases/download/v$NERD/JetBrainsMono.tar.xz \
    ef552a3e638f25125c6ad4c51176a6adcdce295ab1d2ffacf0db060caf8c1582 "$SRC/JetBrainsMono-nerd-$NERD.tar.xz"
fetch_git_commit "$EDEX_REPO" "$EDEX_COMMIT" "$SRC/edex-de-rs-$EDEX_COMMIT"

if ! built dbus; then
    rm -rf "$BUILD/src/dbus-$DBUS" "$BUILD/b-dbus"
    mkdir -p "$BUILD/src"
    tar -xJf "$SRC/dbus-$DBUS.tar.xz" -C "$BUILD/src"
    "$MESON" setup "$BUILD/b-dbus" "$BUILD/src/dbus-$DBUS" --cross-file "$CROSS" \
        --prefix=/usr/local --libdir=lib --sysconfdir=/usr/local/etc --localstatedir=/var \
        --buildtype=release -Ddefault_library=shared --wrap-mode=nofallback \
        -Dsystemd=disabled -Dx11_autolaunch=disabled -Dlaunchd=disabled -Dkqueue=disabled \
        -Dselinux=disabled -Dapparmor=disabled -Dlibaudit=disabled -Dinotify=enabled \
        -Depoll=enabled -Dmodular_tests=disabled -Ddoxygen_docs=disabled -Dxml_docs=disabled \
        -Dqt_help=disabled -Dducktype_docs=disabled -Duser_session=false -Dtools=true \
        -Dmessage_bus=true -Dsession_socket_dir=/tmp -Dtraditional_activation=true \
        -Dsystem_pid_file=/run/dbus.pid -Dsystem_socket=/run/dbus/system_bus_socket \
        >"$BUILD/dbus.log" 2>&1 || { tail -30 "$BUILD/dbus.log" >&2; exit 1; }
    ninja -C "$BUILD/b-dbus" >>"$BUILD/dbus.log" 2>&1 || { tail -40 "$BUILD/dbus.log" >&2; exit 1; }
    DESTDIR="$STAGE" "$MESON" install -C "$BUILD/b-dbus" --no-rebuild >>"$BUILD/dbus.log" 2>&1
    mark dbus
    echo "edex-de port: dbus" >&2
fi

# eDEX-DE itself (release build, the whole workspace's binaries).
E="$BUILD/src/edex-de-rs"
if ! built "edex-$EDEX_COMMIT"; then
    rm -rf "$E"
    cp -r "$SRC/edex-de-rs-$EDEX_COMMIT" "$E"
    CARGO_INCREMENTAL=0 "$ROOT/tools/rustos-cargo" build --release --locked --bins \
        --manifest-path "$E/Cargo.toml" --target-dir "$BUILD/target" \
        >"$BUILD/edex.log" 2>&1 || { tail -40 "$BUILD/edex.log" >&2; exit 1; }
    mark "edex-$EDEX_COMMIT"
    echo "edex-de port: eDEX-DE $EDEX_COMMIT" >&2
fi
OUT="$BUILD/target/x86_64-unknown-linux-musl/release"

# What this port adds to the labwc stage: dbus.
mkdir -p "$DEST"
(cd "$STAGE" && find . -type f) | sed 's|^\./||' | while read -r f; do
    grep -qxF "$STAGE/$f" "$BUILD/base-files" && continue
    case "$f" in usr/local/*) ;; *) continue ;; esac
    rel="${f#usr/local/}"
    mkdir -p "$DEST/$(dirname "$rel")"
    cp -a "$STAGE/$f" "$DEST/$rel"
done
(cd "$STAGE/usr/local" && find . -type l) | while read -r l; do
    case "$l" in ./lib/libdbus*) ;; *) continue ;; esac
    [ -e "$DEST/$l" ] || { mkdir -p "$DEST/$(dirname "$l")"; cp -a "$STAGE/usr/local/$l" "$DEST/$l"; }
done
rm -rf "$DEST/include" "$DEST/lib/pkgconfig" "$DEST/lib/cmake" "$DEST/share/doc" \
    "$DEST/share/man" "$DEST/lib/dbus-1.0/include"

# eDEX-DE: binaries, themes, assets, desktop entries, licence.
install -D -m 755 "$OUT/edex-de" "$DEST/bin/edex-de"
install -D -m 755 "$OUT/edex-greeter" "$DEST/bin/edex-greeter"
mkdir -p "$DEST/share/edex-de/themes" "$DEST/share/edex-de/assets" "$DEST/share/applications"
cp "$E"/themes/*.toml "$DEST/share/edex-de/themes/"
cp "$E"/assets/*.svg "$DEST/share/edex-de/assets/"
cp "$E"/packaging/applications/*.desktop "$DEST/share/applications/"
install -D -m 644 "$E/LICENSE" "$DEST/share/licenses/edex-de/LICENSE"
printf 'eDEX-DE (https://github.com/RustOS-Dev/eDEX-DE-RS) commit %s, GPL-3.0.\nIt ships as a separate program in RustOS images; the RustOS kernel is GPL-2.0-or-later.\n' \
    "$EDEX_COMMIT" > "$DEST/share/licenses/edex-de/SOURCE"

# RustOS's session: defaults (foot as the terminal), the greeter's labwc configuration, the
# session script and the `desktop` command.
install -D -m 644 "$ROOT/ports/edex-de/config.toml" "$DEST/share/edex-de/config.toml"
mkdir -p "$DEST/share/edex-de/greeter"
cp "$ROOT/ports/edex-de/greeter/rc.xml" "$ROOT/ports/edex-de/greeter/menu.xml" "$DEST/share/edex-de/greeter/"
install -D -m 755 "$ROOT/ports/edex-de/edex-session" "$DEST/bin/edex-session"
install -D -m 755 "$ROOT/ports/edex-de/desktop" "$DEST/bin/desktop"

# JetBrains Mono Nerd Font (OFL-1.1): regular, bold and italics.
F="$BUILD/src/nerd-font"
rm -rf "$F"
mkdir -p "$F" "$DEST/share/fonts/jetbrains-mono-nerd"
tar -xJf "$SRC/JetBrainsMono-nerd-$NERD.tar.xz" -C "$F"
for s in Regular Bold Italic BoldItalic; do
    cp "$F/JetBrainsMonoNerdFont-$s.ttf" "$DEST/share/fonts/jetbrains-mono-nerd/"
done
install -D -m 644 "$F/OFL.txt" "$DEST/share/licenses/jetbrains-mono-nerd/OFL.txt"

find "$DEST" -type f \( -name '*.so*' -o -perm -u+x \) ! -name edex-session ! -name desktop \
    -exec strip --strip-unneeded {} + 2>/dev/null || true
