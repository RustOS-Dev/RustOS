#!/bin/sh
# A stacking Wayland desktop on the weston port's libraries: labwc (an
# Openbox-like compositor on wlroots, using Mesa's GLES or pixman), the
# foot terminal, and the libraries they add: GLib, HarfBuzz, FriBidi,
# Pango, libxml2, PCRE2, fcft, utf8proc and tllist. Installs to
# /usr/local, with a default labwc configuration in /usr/local/etc/xdg/labwc.
# Called by tools/install-port.sh with: SRC_DIR BUILD_DIR DEST_DIR
#
# Builds on the weston port (tools/install-port.sh weston first): its
# stage holds wayland, libdrm, Mesa, pixman, cairo, libinput, libseat...
set -e
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
. "$ROOT/tools/port-lib.sh"
SRC="$1"; BUILD="$2"; DEST="$3"
BASE="$ROOT/target/ports/build/weston"
[ -f "$BASE/stamps/weston" ] || { echo "labwc port: build the weston port first" >&2; exit 1; }
STAGE="$BUILD/stage"
HOST="$BASE/host"
STAMPS="$BUILD/stamps"
mkdir -p "$STAGE" "$STAMPS"
# This port's stage starts as a copy of weston's, so everything is found
# in one place and this port's output is what it adds.
if [ ! -f "$STAMPS/base" ]; then
    # A new base invalidates everything built on the old one.
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
export PKG_CONFIG_PATH="$HOST/lib/x86_64-linux-gnu/pkgconfig:$HOST/lib/pkgconfig:$HOST/share/pkgconfig"
export PATH="$HOST/bin:$PATH"
CC="$ROOT/tools/rustos-cc"
J="-j$(nproc)"

unpack() {
    d="$BUILD/src/$(basename "$1" | sed 's/\.tar\..*//')"
    rm -rf "$d"
    mkdir -p "$BUILD/src"
    tar -xf "$1" -C "$BUILD/src"
    # Archives from codeberg unpack to the bare project name.
    [ -d "$d" ] || d="$BUILD/src/$2"
    echo "$d"
}
built() { [ -f "$STAMPS/$1" ]; }
mark() { touch "$STAMPS/$1"; }
meson_pkg() {
    name="$1"; dir="$2"; shift 2
    b="$BUILD/b-$name"
    rm -rf "$b"
    "$MESON" setup "$b" "$dir" --cross-file "$CROSS" --prefix=/usr/local --libdir=lib \
        --sysconfdir=/usr/local/etc --buildtype=release -Ddefault_library=shared \
        --wrap-mode=nofallback -Dbuild.pkg_config_path="$PKG_CONFIG_PATH" \
        "$@" >"$BUILD/$name.log" 2>&1 || { tail -30 "$BUILD/$name.log" >&2; exit 1; }
    ninja -C "$b" >>"$BUILD/$name.log" 2>&1 || { tail -40 "$BUILD/$name.log" >&2; exit 1; }
    DESTDIR="$STAGE" "$MESON" install -C "$b" --no-rebuild >>"$BUILD/$name.log" 2>&1
    mark "$name"
    echo "labwc port: $name" >&2
}
autotools_pkg() {
    name="$1"; dir="$2"; shift 2
    (cd "$dir" && ./configure --host=x86_64-linux-musl CC="$CC" --prefix=/usr/local \
        --sysconfdir=/usr/local/etc "$@" >"$BUILD/$name.log" 2>&1 &&
        make $J >>"$BUILD/$name.log" 2>&1 &&
        make install DESTDIR="$STAGE" >>"$BUILD/$name.log" 2>&1) ||
        { tail -40 "$BUILD/$name.log" >&2; exit 1; }
    find "$STAGE" -name '*.la' -delete
    mark "$name"
    echo "labwc port: $name" >&2
}

fetch https://github.com/PCRE2Project/pcre2/releases/download/pcre2-10.49/pcre2-10.49.tar.bz2 \
    53c156e1ba416a20da8e65395daa132da0d80e76910424caca3fcdae7831d384 "$SRC/pcre2-10.49.tar.bz2"
fetch https://download.gnome.org/sources/glib/2.88/glib-2.88.3.tar.xz \
    ab24d24e698dfa1e408b7bcdb508f4aafc906185a8b8ce72fdf79bbbdc9b383b "$SRC/glib-2.88.3.tar.xz"
fetch https://github.com/harfbuzz/harfbuzz/releases/download/14.6.0/harfbuzz-14.6.0.tar.xz \
    d07a007327277708a2a73ae437887cdbaf282937f6d03ca5467723e9099af586 "$SRC/harfbuzz-14.6.0.tar.xz"
fetch https://github.com/fribidi/fribidi/releases/download/v1.0.17/fribidi-1.0.17.tar.xz \
    6949dcde27d41cebad1fd741fcafc36d55a1020d2d872d4a6eb3914caabbada2 "$SRC/fribidi-1.0.17.tar.xz"
fetch https://download.gnome.org/sources/pango/1.56/pango-1.56.4.tar.xz \
    17065e2fcc5f5a5bdbffc884c956bfc7c451a96e8c4fb2f8ad837c6413cb5a01 "$SRC/pango-1.56.4.tar.xz"
fetch https://download.gnome.org/sources/libxml2/2.15/libxml2-2.15.4.tar.xz \
    98087fd181d9070724f3fbc65c7377db03038eb92bd882374daff44940138821 "$SRC/libxml2-2.15.4.tar.xz"
fetch https://gitlab.freedesktop.org/wlroots/wlroots/-/releases/0.20.1/downloads/wlroots-0.20.1.tar.gz \
    a8541187baecaa2620938afacde88266cb7efa5928cb09d579d8efb07bc4901b "$SRC/wlroots-0.20.1.tar.gz"
fetch_git https://github.com/labwc/labwc 0.20.2 97f28877a343e062f3178d201f0248cd9c2610cf "$SRC/labwc-0.20.2"
fetch https://github.com/JuliaStrings/utf8proc/releases/download/v2.12.0/utf8proc-2.12.0.tar.gz \
    a393fbef160835fb315bc3e91ba8d86f7a73a7cec9e6198b6c60b848b498bfeb "$SRC/utf8proc-2.12.0.tar.gz"
fetch https://codeberg.org/dnkl/tllist/archive/1.1.0.tar.gz \
    0e7b7094a02550dd80b7243bcffc3671550b0f1d8ba625e4dff52517827d5d23 "$SRC/tllist-1.1.0.tar.gz"
fetch https://codeberg.org/dnkl/fcft/archive/3.3.3.tar.gz \
    b0c0f4a599f43723736c8565b8b84337c4195077f07f1bb8bb3252bb13a2306a "$SRC/fcft-3.3.3.tar.gz"
fetch https://codeberg.org/dnkl/foot/archive/1.28.0.tar.gz \
    4296be402b5684d049534598e69db92b918f92beac9dab76b585207045f0b037 "$SRC/foot-1.28.0.tar.gz"

built pcre2 || autotools_pkg pcre2 "$(unpack "$SRC/pcre2-10.49.tar.bz2")" \
    --disable-static --enable-pcre2-16=no --enable-pcre2-32=no
built glib || meson_pkg glib "$(unpack "$SRC/glib-2.88.3.tar.xz")" \
    -Dtests=false -Dintrospection=disabled -Dnls=disabled -Dlibmount=disabled \
    -Dselinux=disabled -Dxattr=false -Dman-pages=disabled -Ddocumentation=false \
    -Dsysprof=disabled -Dglib_debug=disabled -Dlibelf=disabled
built harfbuzz || meson_pkg harfbuzz "$(unpack "$SRC/harfbuzz-14.6.0.tar.xz")" \
    -Dglib=enabled -Dgobject=disabled -Dfreetype=enabled -Dcairo=disabled -Dicu=disabled \
    -Dtests=disabled -Ddocs=disabled -Dintrospection=disabled -Dutilities=disabled \
    -Dbenchmark=disabled
built fribidi || meson_pkg fribidi "$(unpack "$SRC/fribidi-1.0.17.tar.xz")" \
    -Ddocs=false -Dtests=false -Dbin=false
built pango || meson_pkg pango "$(unpack "$SRC/pango-1.56.4.tar.xz")" \
    -Dintrospection=disabled -Ddocumentation=false -Dbuild-testsuite=false \
    -Dbuild-examples=false -Dxft=disabled -Dcairo=enabled -Dfontconfig=enabled \
    -Dfreetype=enabled -Dlibthai=disabled -Dsysprof=disabled
built libxml2 || meson_pkg libxml2 "$(unpack "$SRC/libxml2-2.15.4.tar.xz")" \
    -Dpython=disabled -Dicu=disabled -Dzlib=enabled -Dhttp=disabled -Ddocs=disabled \
    -Dreadline=disabled -Dhistory=disabled
built wlroots || meson_pkg wlroots "$(unpack "$SRC/wlroots-0.20.1.tar.gz")" \
    -Dxwayland=disabled -Dbackends=drm,libinput -Drenderers=gles2 -Dallocators=gbm \
    -Dsession=enabled -Dexamples=false -Dxcb-errors=disabled -Dcolor-management=disabled \
    -Dlibliftoff=disabled
if ! built labwc; then
    rm -rf "$BUILD/src/labwc"
    cp -r "$SRC/labwc-0.20.2" "$BUILD/src/labwc"
    meson_pkg labwc "$BUILD/src/labwc" -Dxwayland=disabled -Dsvg=disabled -Dicon=disabled \
        -Dnls=disabled -Dman-pages=disabled -Dtest=disabled
fi
if ! built utf8proc; then
    d=$(unpack "$SRC/utf8proc-2.12.0.tar.gz")
    make -C "$d" $J CC="$CC" prefix=/usr/local libutf8proc.so >"$BUILD/utf8proc.log" 2>&1
    make -C "$d" CC="$CC" prefix=/usr/local DESTDIR="$STAGE" install >>"$BUILD/utf8proc.log" 2>&1
    rm -f "$STAGE/usr/local/lib/libutf8proc.a"
    mark utf8proc
fi
built tllist || meson_pkg tllist "$(unpack "$SRC/tllist-1.1.0.tar.gz" tllist)"
built fcft || meson_pkg fcft "$(unpack "$SRC/fcft-3.3.3.tar.gz" fcft)" \
    -Dgrapheme-shaping=enabled -Drun-shaping=enabled -Dtest-text-shaping=false \
    -Ddocs=disabled -Dsvg-backend=none -Dexamples=false
built foot || meson_pkg foot "$(unpack "$SRC/foot-1.28.0.tar.gz" foot)" \
    -Ddocs=disabled -Dtests=false -Dthemes=false -Dime=true -Dgrapheme-clustering=enabled \
    -Dterminfo=disabled -Dutmp-backend=none -Dsystemd-units-dir=

# A default session: foot on Super+Return / Alt+Return and in the root menu.
mkdir -p "$STAGE/usr/local/etc/xdg/labwc"
cp "$ROOT/ports/labwc/rc.xml" "$ROOT/ports/labwc/menu.xml" "$ROOT/ports/labwc/autostart" \
    "$STAGE/usr/local/etc/xdg/labwc/"

# What this port adds to weston's files.
mkdir -p "$DEST"
(cd "$STAGE" && find . -type f) | sed 's|^\./||' | while read -r f; do
    grep -qxF "$STAGE/$f" "$BUILD/base-files" && continue
    case "$f" in usr/local/*) ;; *) continue ;; esac
    rel="${f#usr/local/}"
    mkdir -p "$DEST/$(dirname "$rel")"
    cp -a "$STAGE/$f" "$DEST/$rel"
done
(cd "$STAGE/usr/local" && find . -type l) | while read -r l; do
    [ -e "$DEST/$l" ] || { mkdir -p "$DEST/$(dirname "$l")"; cp -a "$STAGE/usr/local/$l" "$DEST/$l"; }
done
rm -rf "$DEST/include" "$DEST/lib/pkgconfig" "$DEST/share/pkgconfig" "$DEST/share/man" \
    "$DEST/share/doc" "$DEST/share/gettext" "$DEST/share/aclocal" "$DEST/share/gdb" \
    "$DEST/share/glib-2.0/gdb" "$DEST/libexec/installed-tests"
find "$DEST" -name '*.a' -delete
find "$DEST" -type f \( -name '*.so*' -o -perm -u+x \) -exec strip --strip-unneeded {} + 2>/dev/null || true
