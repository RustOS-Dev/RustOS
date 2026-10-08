#!/bin/sh
# GTK 3 on Wayland, on the labwc port's libraries (GLib, Pango, HarfBuzz,
# libepoxy, ...): gdk-pixbuf, ATK (from at-spi2-core, without D-Bus) and
# GTK 3 with its Wayland backend only, plus gtk3-demo and
# gtk3-widget-factory. Installs to /usr/local.
# Called by tools/install-port.sh with: SRC_DIR BUILD_DIR DEST_DIR
#
# Builds on the labwc port (tools/install-port.sh labwc first).
set -e
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
. "$ROOT/tools/port-lib.sh"
SRC="$1"; BUILD="$2"; DEST="$3"
BASE="$ROOT/target/ports/build/labwc"
[ -f "$BASE/stamps/foot" ] || { echo "gtk port: build the labwc port first" >&2; exit 1; }
STAGE="$BUILD/stage"
HOST="$ROOT/target/ports/build/weston/host"
STAMPS="$BUILD/stamps"
mkdir -p "$STAGE" "$STAMPS"
# This port's stage starts as a copy of labwc's, so everything is found
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
    echo "gtk port: $name" >&2
}
autotools_pkg() {
    name="$1"; dir="$2"; shift 2
    (cd "$dir" && PKG_CONFIG="$ROOT/tools/cross/rustos-pkg-config" CC_FOR_BUILD=cc \
        ./configure --host=x86_64-linux-musl CC="$CC" --prefix=/usr/local \
        --sysconfdir=/usr/local/etc "$@" >"$BUILD/$name.log" 2>&1 &&
        make $J >>"$BUILD/$name.log" 2>&1 &&
        make install DESTDIR="$STAGE" >>"$BUILD/$name.log" 2>&1) ||
        { tail -40 "$BUILD/$name.log" >&2; exit 1; }
    find "$STAGE" -name '*.la' -delete
    mark "$name"
    echo "gtk port: $name" >&2
}

fetch https://download.gnome.org/sources/gdk-pixbuf/2.42/gdk-pixbuf-2.42.12.tar.xz \
    b9505b3445b9a7e48ced34760c3bcb73e966df3ac94c95a148cb669ab748e3c7 "$SRC/gdk-pixbuf-2.42.12.tar.xz"
fetch https://download.gnome.org/sources/at-spi2-core/2.58/at-spi2-core-2.58.9.tar.xz \
    c8eacbe2640038178f2c2cd7abef2c23c7a4777909119f9d815c7151b39fb82a "$SRC/at-spi2-core-2.58.9.tar.xz"
fetch https://download.gnome.org/sources/gtk/3.24/gtk-3.24.52.tar.xz \
    80931fa472a77b9a164f6740e3c0b444fac6770054632d35a7ff9d679e5e7b9f "$SRC/gtk-3.24.52.tar.xz"

built gdk-pixbuf || meson_pkg gdk-pixbuf "$(unpack "$SRC/gdk-pixbuf-2.42.12.tar.xz")" \
    -Dpng=enabled -Djpeg=disabled -Dtiff=disabled -Dgif=disabled -Dothers=disabled \
    -Dbuiltin_loaders=png -Dintrospection=disabled -Dman=false -Dgtk_doc=false \
    -Dtests=false -Dinstalled_tests=false
built atk || meson_pkg atk "$(unpack "$SRC/at-spi2-core-2.58.9.tar.xz")" \
    -Datk_only=true -Dintrospection=disabled -Ddocs=false -Dx11=disabled
built gtk || meson_pkg gtk "$(unpack "$SRC/gtk-3.24.52.tar.xz")" \
    -Dx11_backend=false -Dwayland_backend=true -Dbroadway_backend=false \
    -Dprint_backends=file -Dcolord=no -Dintrospection=false -Dgtk_doc=false -Dman=false \
    -Ddemos=true -Dexamples=false -Dtests=false -Dinstalled_tests=false

# GSettings schemas are compiled with the host tool (the format is portable).
glib-compile-schemas "$STAGE/usr/local/share/glib-2.0/schemas"

# What this port adds to labwc's files.
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
# gschemas.compiled changed in place: always ship it.
mkdir -p "$DEST/share/glib-2.0/schemas"
cp "$STAGE/usr/local/share/glib-2.0/schemas/gschemas.compiled" "$DEST/share/glib-2.0/schemas/"
rm -rf "$DEST/include" "$DEST/lib/pkgconfig" "$DEST/share/pkgconfig" "$DEST/share/man" \
    "$DEST/share/doc" "$DEST/share/gettext" "$DEST/share/aclocal" "$DEST/share/gtk-doc" \
    "$DEST/share/locale" "$DEST/share/installed-tests" "$DEST/libexec/installed-tests"
find "$DEST" -name '*.a' -delete
find "$DEST" -type f \( -name '*.so*' -o -perm -u+x \) -exec strip --strip-unneeded {} + 2>/dev/null || true
