#!/bin/sh
# The desktop of release images (M43): build the ports eDEX-DE runs on and
# put only what the desktop uses into target/ports-root, which the next
# kernel build embeds (write_to_drive.sh runs this before building).
#
#   tools/release-ports.sh
#
# Ports: libcxx (C++ runtime), weston (the Wayland, Mesa and input stack;
# Weston itself is left out), labwc (wlroots, labwc, foot, Xwayland) and
# edex-de (eDEX-DE, D-Bus, the session). Test programs and demos (Weston's
# desktop and terminal, kmscube, the DRM and GL test tools, xhello) and the
# Vulkan drivers (no RustOS GPU driver can use them) stay out; they remain
# available as test ports (tools/install-port.sh --initramfs NAME).
#
# Replaces target/ports-root.
set -e
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PORTS="libcxx weston labwc edex-de"
for p in $PORTS; do
    "$ROOT/tools/install-port.sh" "$p"
done

OUT="$ROOT/target/ports-root"
rm -rf "$OUT"
mkdir -p "$OUT/usr/local"
for p in $PORTS; do
    cp -a "$ROOT/target/ports/$p/." "$OUT/usr/local/"
done

cd "$OUT/usr/local"
# Test programs and demos.
for f in weston weston-terminal weston-simple-shm weston-screenshooter kmscube \
    modetest modeprint proptest vbltest drmdevice amdgpu_stress texturator vulkaninfo \
    cairo-trace edid-decode di-edid-decode libinput libevdev-tweak-device mouse-dpi-tool \
    mtdev-test touchpad-edge-detector xhello cxxtest png-fix-itxt pngfix libpng-config \
    libpng16-config pcre2-config pcre2grep pcre2test xml2-config xmlcatalog xmllint xmlwf \
    pango-list pango-segmentation pango-view gdbus-codegen gi-compile-repository \
    gi-decompile-typelib gi-inspect-typelib glib-compile-resources glib-genmarshal \
    glib-gettextize glib-mkenums gobject-query gresource gtester gtester-report \
    dbus-test-tool seatd-launch; do
    rm -f "bin/$f"
done
rm -rf libexec/weston-desktop-shell libexec/weston-keyboard libexec/libinput lib/libweston-14 \
    lib/libweston-14.so* share/weston share/wayland-sessions/weston.desktop share/libweston-14 \
    lib/libvulkan_*.so share/vulkan lib/libcairo-script-interpreter.so* \
    include lib/pkgconfig lib/cmake share/pkgconfig share/aclocal share/man share/doc \
    share/gtk-doc share/gettext share/zsh share/fish share/bash-completion share/gdb \
    share/glib-2.0/gdb share/cmake
find lib -name '*.a' -delete
# Desktop entries of programs that are not in the image.
for d in share/applications/*.desktop; do
    [ -e "$d" ] || continue
    exe="$(sed -n 's/^Exec=\([^ ]*\).*/\1/p' "$d" | head -1)"
    [ -z "$exe" ] && continue
    case "$exe" in /*) [ -e ".$exe" ] || [ -e "$exe" ] || rm -f "$d" ;; *) [ -e "bin/$exe" ] || rm -f "$d" ;; esac
done
echo "release-ports: target/ports-root holds the desktop ($(du -sh "$OUT" | cut -f1)); rebuild the kernel"
