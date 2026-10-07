#!/bin/sh
# The desktop graphics stack, built as shared libraries against musl:
# Weston (Wayland compositor) with the DRM backend and both the pixman
# (software) and GL renderers, Mesa (EGL, GBM, OpenGL ES/GL and Vulkan
# drivers) and the libraries under them. Installs to /usr/local: weston,
# weston-terminal, libinput/libseat/libwayland/..., Mesa's libraries and
# drivers, XKB data, a fontconfig setup for the fonts in /usr/share/fonts,
# and modetest (libdrm).
#
# Mesa's drivers: softpipe (software GL, also on any KMS display through
# kms_swrast), virgl (QEMU virtio-gpu 3D), zink (GL on Vulkan: AMD's
# OpenGL here, on RADV), iris (Intel GL), RADV (AMD Vulkan, ACO) and ANV
# (Intel Vulkan). Not built: llvmpipe and lavapipe (need LLVM ported to
# RustOS), radeonsi (needs libelf), NVK (needs Rust cross-compiled for
# RustOS).
# Called by tools/install-port.sh with: SRC_DIR BUILD_DIR DEST_DIR
#
# Needs on the build host: meson, ninja, pkg-config, gperf, bison, flex,
# python3 with mako, pyyaml and ply, hwdata, glslang-tools, expat/libffi
# development files (for a native wayland-scanner), and LLVM 18 with
# clang, libclc and SPIRV-LLVM-Translator development files (Mesa's
# OpenCL-C kernel compiler, mesa_clc, runs on the host).
set -e
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
. "$ROOT/tools/port-lib.sh"
SRC="$1"; BUILD="$2"; DEST="$3"
STAGE="$BUILD/stage"
HOST="$BUILD/host"
STAMPS="$BUILD/stamps"
mkdir -p "$STAGE" "$HOST" "$STAMPS"
export RUSTOS_STAGE="$STAGE"
CROSS="$BUILD/cross.ini"
sed "s|@ROOT@|$ROOT|g" "$ROOT/tools/cross/meson-cross.ini.in" > "$CROSS"
MESON="${MESON:-meson}"
command -v "$MESON" >/dev/null || MESON="$HOME/.local/bin/meson"
# Shared libraries find each other in the stage at link time.
export LDFLAGS="-L$STAGE/usr/local/lib -Wl,-rpath-link,$STAGE/usr/local/lib"
export CPPFLAGS="-I$STAGE/usr/local/include"
CC="$ROOT/tools/rustos-cc"
J="-j$(nproc)"

# unpack ARCHIVE: extract into $BUILD (fresh), print the directory.
unpack() {
    d="$BUILD/src/$(basename "$1" | sed 's/\.tar\..*//')"
    rm -rf "$d"
    mkdir -p "$BUILD/src"
    tar -xf "$1" -C "$BUILD/src"
    echo "$d"
}

# done NAME / mark NAME: per-package stamps (rebuilds skip finished ones).
built() { [ -f "$STAMPS/$1" ]; }
mark() { touch "$STAMPS/$1"; }

# meson_pkg NAME SRCDIR [OPTIONS...]: cross-build and stage a meson project.
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
    echo "weston port: $name" >&2
}

# cmake_pkg NAME SRCDIR [CMAKE OPTIONS...]: cross-build and stage a CMake project.
cmake_pkg() {
    name="$1"; dir="$2"; shift 2
    b="$BUILD/b-$name"
    rm -rf "$b"
    PKG_CONFIG="$ROOT/tools/cross/rustos-pkg-config" cmake -G Ninja -S "$dir" -B "$b" \
        -DCMAKE_BUILD_TYPE=Release -DCMAKE_SYSTEM_NAME=Linux -DCMAKE_SYSTEM_PROCESSOR=x86_64 \
        -DCMAKE_C_COMPILER="$CC" -DCMAKE_CXX_COMPILER="$ROOT/tools/rustos-c++" \
        -DCMAKE_INSTALL_PREFIX=/usr/local -DCMAKE_INSTALL_LIBDIR=lib \
        -DCMAKE_FIND_ROOT_PATH="$STAGE/usr/local" -DCMAKE_PREFIX_PATH="$STAGE/usr/local" \
        "$@" >"$BUILD/$name.log" 2>&1 || { tail -30 "$BUILD/$name.log" >&2; exit 1; }
    ninja -C "$b" >>"$BUILD/$name.log" 2>&1 || { tail -40 "$BUILD/$name.log" >&2; exit 1; }
    DESTDIR="$STAGE" ninja -C "$b" install >>"$BUILD/$name.log" 2>&1
    mark "$name"
    echo "weston port: $name" >&2
}

# autotools_pkg NAME SRCDIR [CONFIGURE OPTIONS...]
autotools_pkg() {
    name="$1"; dir="$2"; shift 2
    (cd "$dir" && ./configure --host=x86_64-linux-musl CC="$CC" --prefix=/usr/local \
        --sysconfdir=/usr/local/etc "$@" >"$BUILD/$name.log" 2>&1 &&
        make $J >>"$BUILD/$name.log" 2>&1 &&
        make install DESTDIR="$STAGE" >>"$BUILD/$name.log" 2>&1) ||
        { tail -40 "$BUILD/$name.log" >&2; exit 1; }
    find "$STAGE" -name '*.la' -delete
    mark "$name"
    echo "weston port: $name" >&2
}

fetch https://github.com/libffi/libffi/releases/download/v3.4.6/libffi-3.4.6.tar.gz \
    b0dea9df23c863a7a50e825440f3ebffabd65df1497108e5d437747843895a4e "$SRC/libffi-3.4.6.tar.gz"
fetch https://github.com/libexpat/libexpat/releases/download/R_2_6_4/expat-2.6.4.tar.xz \
    a695629dae047055b37d50a0ff4776d1d45d0a4c842cf4ccee158441f55ff7ee "$SRC/expat-2.6.4.tar.xz"
fetch https://gitlab.freedesktop.org/wayland/wayland/-/releases/1.26.0/downloads/wayland-1.26.0.tar.xz \
    64176eaa46e4969903e286f8e5ef8331affc17fdf03ac9b58381d2b23162b7a3 "$SRC/wayland-1.26.0.tar.xz"
fetch https://gitlab.freedesktop.org/wayland/wayland-protocols/-/releases/1.49/downloads/wayland-protocols-1.49.tar.xz \
    ec4c8f74942d6dff7ace8b4ce4764f0ef9ff618a935d974ea77edee2ad240b14 "$SRC/wayland-protocols-1.49.tar.xz"
fetch_git https://github.com/xkbcommon/libxkbcommon xkbcommon-1.13.2 \
    d1442aaa6b635551182e83c3b55037f4c118c962 "$SRC/libxkbcommon-1.13.2"
fetch https://www.x.org/releases/individual/data/xkeyboard-config/xkeyboard-config-2.43.tar.xz \
    c810f362c82a834ee89da81e34cd1452c99789339f46f6037f4b9e227dd06c01 "$SRC/xkeyboard-config-2.43.tar.xz"
fetch https://www.x.org/releases/individual/lib/pixman-0.46.4.tar.xz \
    a098c33924754ad43f981b740f6d576c70f9ed1006e12221b1845431ebce1239 "$SRC/pixman-0.46.4.tar.xz"
fetch https://zlib.net/fossils/zlib-1.3.1.tar.gz \
    9a93b2b7dfdac77ceba5a558a580e74667dd6fede4585b91eefb60f03b72df23 "$SRC/zlib-1.3.1.tar.gz"
fetch https://download.sourceforge.net/libpng/libpng-1.6.44.tar.xz \
    60c4da1d5b7f0aa8d158da48e8f8afa9773c1c8baa5d21974df61f1886b8ce8e "$SRC/libpng-1.6.44.tar.xz"
fetch https://download.savannah.gnu.org/releases/freetype/freetype-2.13.3.tar.xz \
    0550350666d427c74daeb85d5ac7bb353acba5f76956395995311a9c6f063289 "$SRC/freetype-2.13.3.tar.xz"
fetch https://www.freedesktop.org/software/fontconfig/release/fontconfig-2.15.0.tar.xz \
    63a0658d0e06e0fa886106452b58ef04f21f58202ea02a94c39de0d3335d7c0e "$SRC/fontconfig-2.15.0.tar.xz"
fetch https://cairographics.org/releases/cairo-1.18.2.tar.xz \
    a62b9bb42425e844cc3d6ddde043ff39dbabedd1542eba57a2eb79f85889d45a "$SRC/cairo-1.18.2.tar.xz"
fetch https://dri.freedesktop.org/libdrm/libdrm-2.4.134.tar.xz \
    ac5e74d157830eb8bee44c6a6bf3ad49774ef0dd2a72bdad74a8f20308b52a95 "$SRC/libdrm-2.4.134.tar.xz"
fetch https://www.freedesktop.org/software/libevdev/libevdev-1.13.3.tar.xz \
    abf1aace86208eebdd5d3550ffded4c8d73bb405b796d51c389c9d0604cbcfbf "$SRC/libevdev-1.13.3.tar.xz"
fetch https://bitmath.org/code/mtdev/mtdev-1.1.7.tar.bz2 \
    a107adad2101fecac54ac7f9f0e0a0dd155d954193da55c2340c97f2ff1d814e "$SRC/mtdev-1.1.7.tar.bz2"
fetch_git https://github.com/illiliti/libudev-zero 1.0.3 \
    ee32ac5f6494047b9ece26e7a5920650cdf46655 "$SRC/libudev-zero-1.0.3"
fetch https://gitlab.freedesktop.org/libinput/libinput/-/archive/1.27.0/libinput-1.27.0.tar.gz \
    b11b900bf88ef68fe688c107226bb453ef26faf461ae2dcf9690b00009d660a6 "$SRC/libinput-1.27.0.tar.gz"
fetch_git https://git.sr.ht/~kennylevinsen/seatd 0.9.1 \
    566ffeb032af42865dc1210e48cec08368059bb9 "$SRC/seatd-0.9.1"
fetch https://gitlab.freedesktop.org/emersion/libdisplay-info/-/releases/0.2.0/downloads/libdisplay-info-0.2.0.tar.xz \
    5a2f002a16f42dd3540c8846f80a90b8f4bdcd067a94b9d2087bc2feae974176 "$SRC/libdisplay-info-0.2.0.tar.xz"
fetch https://archive.mesa3d.org/mesa-26.2.4.tar.xz \
    bce5f7fbebb934373b86c999a064d52fb5065878dc57f287f95346648ec832e9 "$SRC/mesa-26.2.4.tar.xz"
fetch_git https://github.com/KhronosGroup/Vulkan-Headers v1.4.365 \
    c46850864f4661461b0f6cb9922c058ffea4915e "$SRC/Vulkan-Headers-1.4.365"
fetch_git https://github.com/KhronosGroup/Vulkan-Loader v1.4.365 \
    f866657ff687a36d767cd7783bab800e31ebafaa "$SRC/Vulkan-Loader-1.4.365"
fetch_git https://github.com/KhronosGroup/Vulkan-Tools v1.4.365 \
    f13d435dd50dc616db0c10e7bac87cd3aa7c82e3 "$SRC/Vulkan-Tools-1.4.365"
fetch_git_commit https://gitlab.freedesktop.org/mesa/kmscube.git \
    f60e50e887d3c49e91ac9b06d8199b36152632fa "$SRC/kmscube-f60e50e"
fetch https://gitlab.freedesktop.org/wayland/weston/-/releases/14.0.1/downloads/weston-14.0.1.tar.xz \
    a8150505b126a59df781fe8c30c8e6f87da7013e179039eb844a5bbbcc7c79b3 "$SRC/weston-14.0.1.tar.xz"

# A native wayland-scanner (it runs on the build host).
if ! built host-wayland; then
    d=$(unpack "$SRC/wayland-1.26.0.tar.xz")
    rm -rf "$BUILD/b-host-wayland"
    "$MESON" setup "$BUILD/b-host-wayland" "$d" --prefix="$HOST" -Dlibraries=false \
        -Ddocumentation=false -Dtests=false -Ddtd_validation=false >"$BUILD/host-wayland.log" 2>&1
    ninja -C "$BUILD/b-host-wayland" install >>"$BUILD/host-wayland.log" 2>&1
    mark host-wayland
fi
export PKG_CONFIG_PATH="$HOST/lib/x86_64-linux-gnu/pkgconfig:$HOST/lib/pkgconfig:$HOST/share/pkgconfig"
export PATH="$HOST/bin:$PATH"

built libffi || autotools_pkg libffi "$(unpack "$SRC/libffi-3.4.6.tar.gz")" \
    --disable-docs --disable-multi-os-directory --disable-static
built expat || autotools_pkg expat "$(unpack "$SRC/expat-2.6.4.tar.xz")" \
    --without-docbook --without-examples --without-tests --disable-static
built wayland || meson_pkg wayland "$(unpack "$SRC/wayland-1.26.0.tar.xz")" \
    -Dscanner=false -Ddocumentation=false -Dtests=false -Ddtd_validation=false
built wayland-protocols || meson_pkg wayland-protocols \
    "$(unpack "$SRC/wayland-protocols-1.49.tar.xz")" -Dtests=false
built xkeyboard-config || meson_pkg xkeyboard-config \
    "$(unpack "$SRC/xkeyboard-config-2.43.tar.xz")" -Dxorg-rules-symlinks=false -Dnls=false
if ! built libxkbcommon; then
    rm -rf "$BUILD/src/libxkbcommon"
    cp -r "$SRC/libxkbcommon-1.13.2" "$BUILD/src/libxkbcommon"
    meson_pkg libxkbcommon "$BUILD/src/libxkbcommon" \
    -Denable-x11=false -Denable-docs=false -Denable-tools=false -Denable-xkbregistry=false \
    -Denable-wayland=false -Dxkb-config-root=/usr/local/share/X11/xkb
fi
built pixman || meson_pkg pixman "$(unpack "$SRC/pixman-0.46.4.tar.xz")" \
    -Dtests=disabled -Ddemos=disabled -Dgtk=disabled -Dlibpng=disabled -Dopenmp=disabled
if ! built zlib; then
    d=$(unpack "$SRC/zlib-1.3.1.tar.gz")
    (cd "$d" && CC="$CC" ./configure --prefix=/usr/local >"$BUILD/zlib.log" 2>&1 &&
        make $J >>"$BUILD/zlib.log" 2>&1 && make install DESTDIR="$STAGE" >>"$BUILD/zlib.log" 2>&1)
    mark zlib
fi
built libpng || autotools_pkg libpng "$(unpack "$SRC/libpng-1.6.44.tar.xz")" --disable-static
built freetype || meson_pkg freetype "$(unpack "$SRC/freetype-2.13.3.tar.xz")" \
    -Dbrotli=disabled -Dbzip2=disabled -Dharfbuzz=disabled -Dpng=enabled -Dzlib=system
built fontconfig || meson_pkg fontconfig "$(unpack "$SRC/fontconfig-2.15.0.tar.xz")" \
    -Ddoc=disabled -Dtests=disabled -Dtools=disabled -Dnls=disabled -Dcache-build=disabled \
    -Ddefault-fonts-dirs=/usr/share/fonts,/usr/local/share/fonts
built cairo || meson_pkg cairo "$(unpack "$SRC/cairo-1.18.2.tar.xz")" \
    -Dxlib=disabled -Dxcb=disabled -Dtests=disabled -Dglib=disabled -Dspectre=disabled \
    -Dsymbol-lookup=disabled -Dgtk2-utils=disabled -Dpng=enabled \
    -Dfreetype=enabled -Dfontconfig=enabled -Dzlib=enabled -Dquartz=disabled -Ddwrite=disabled
built libdrm-gpu || meson_pkg libdrm-gpu "$(unpack "$SRC/libdrm-2.4.134.tar.xz")" \
    -Dintel=disabled -Dradeon=disabled -Damdgpu=enabled -Dnouveau=enabled \
    -Dvmwgfx=disabled -Dfreedreno=disabled -Dvc4=disabled -Detnaviv=disabled \
    -Dexynos=disabled -Domap=disabled -Dtegra=disabled -Dman-pages=disabled \
    -Dvalgrind=disabled -Dcairo-tests=disabled -Dtests=true -Dinstall-test-programs=true
built libevdev || meson_pkg libevdev "$(unpack "$SRC/libevdev-1.13.3.tar.xz")" \
    -Dtests=disabled -Ddocumentation=disabled
built mtdev || autotools_pkg mtdev "$(unpack "$SRC/mtdev-1.1.7.tar.bz2")" --disable-static
if ! built libudev-zero; then
    rm -rf "$BUILD/src/libudev-zero"
    cp -r "$SRC/libudev-zero-1.0.3" "$BUILD/src/libudev-zero"
    make -C "$BUILD/src/libudev-zero" $J CC="$CC" PREFIX=/usr/local >"$BUILD/libudev-zero.log" 2>&1
    make -C "$BUILD/src/libudev-zero" install-shared install-headers DESTDIR="$STAGE" \
        PREFIX=/usr/local >>"$BUILD/libudev-zero.log" 2>&1 ||
        make -C "$BUILD/src/libudev-zero" install DESTDIR="$STAGE" PREFIX=/usr/local \
            >>"$BUILD/libudev-zero.log" 2>&1
    mark libudev-zero
fi
built libinput || meson_pkg libinput "$(unpack "$SRC/libinput-1.27.0.tar.gz")" \
    -Dlibwacom=false -Ddebug-gui=false -Dtests=false -Ddocumentation=false \
    -Dudev-dir=/usr/local/lib/udev
if ! built seatd; then
    rm -rf "$BUILD/src/seatd"
    cp -r "$SRC/seatd-0.9.1" "$BUILD/src/seatd"
    meson_pkg seatd "$BUILD/src/seatd" -Dlibseat-logind=disabled -Dlibseat-seatd=enabled \
        -Dlibseat-builtin=enabled -Dserver=enabled -Dexamples=disabled -Dman-pages=disabled
fi
built libdisplay-info || meson_pkg libdisplay-info "$(unpack "$SRC/libdisplay-info-0.2.0.tar.xz")"

# Mesa's build-time tools for the drivers whose shaders are partly OpenCL C
# (Intel): mesa_clc and the precompiler, built for the host.
if ! built host-mesa-clc; then
    d=$(unpack "$SRC/mesa-26.2.4.tar.xz")
    rm -rf "$BUILD/b-host-mesa"
    "$MESON" setup "$BUILD/b-host-mesa" "$d" --prefix="$HOST" --buildtype=release \
        -Dgallium-drivers= -Dvulkan-drivers= -Dplatforms= -Dglx=disabled -Degl=disabled \
        -Dgbm=disabled -Dopengl=false -Dgles1=disabled -Dgles2=disabled \
        -Dllvm=enabled -Dshared-llvm=enabled -Dmesa-clc=enabled -Dinstall-mesa-clc=true \
        -Dprecomp-compiler=enabled -Dinstall-precomp-compiler=true -Dbuild-tests=false \
        -Dvalgrind=disabled -Dlibunwind=disabled -Dzstd=disabled -Dxmlconfig=disabled \
        >"$BUILD/host-mesa-clc.log" 2>&1 || { tail -30 "$BUILD/host-mesa-clc.log" >&2; exit 1; }
    ninja -C "$BUILD/b-host-mesa" install >>"$BUILD/host-mesa-clc.log" 2>&1 ||
        { tail -40 "$BUILD/host-mesa-clc.log" >&2; exit 1; }
    mark host-mesa-clc
fi
built mesa || meson_pkg mesa "$(unpack "$SRC/mesa-26.2.4.tar.xz")" \
    -Dplatforms=wayland -Degl=enabled -Dgbm=enabled -Dglx=disabled -Dopengl=true \
    -Dgles1=disabled -Dgles2=enabled -Dglvnd=disabled \
    -Dgallium-drivers=softpipe,virgl,zink,iris \
    -Dvulkan-drivers=amd,intel -Dllvm=disabled -Damd-use-llvm=false \
    -Dmesa-clc=system -Dprecomp-compiler=system -Dintel-rt=disabled \
    -Dvideo-codecs= -Dgallium-va=disabled -Dvalgrind=disabled -Dlibunwind=disabled \
    -Dlmsensors=disabled -Dzstd=disabled -Dxmlconfig=enabled -Dbuild-tests=false \
    -Dandroid-libbacktrace=disabled
# The Vulkan loader (libvulkan.so.1: Vulkan applications, and zink, which
# runs OpenGL on RADV/ANV) and vulkaninfo.
built vulkan-headers || cmake_pkg vulkan-headers "$SRC/Vulkan-Headers-1.4.365"
built vulkan-loader || cmake_pkg vulkan-loader "$SRC/Vulkan-Loader-1.4.365" \
    -DBUILD_WSI_XCB_SUPPORT=OFF -DBUILD_WSI_XLIB_SUPPORT=OFF -DBUILD_WSI_XLIB_XRANDR_SUPPORT=OFF \
    -DBUILD_WSI_WAYLAND_SUPPORT=ON -DBUILD_TESTS=OFF -DUSE_GAS=OFF \
    -DVulkanHeaders_DIR="$STAGE/usr/local/share/cmake/VulkanHeaders"
built vulkaninfo || cmake_pkg vulkaninfo "$SRC/Vulkan-Tools-1.4.365" \
    -DBUILD_CUBE=OFF -DBUILD_ICD=OFF -DBUILD_VULKANINFO=ON -DBUILD_TESTS=OFF \
    -DBUILD_WSI_XCB_SUPPORT=OFF -DBUILD_WSI_XLIB_SUPPORT=OFF -DBUILD_WSI_WAYLAND_SUPPORT=ON \
    -DBUILD_WSI_DIRECTFB_SUPPORT=OFF \
    -DVulkanHeaders_DIR="$STAGE/usr/local/share/cmake/VulkanHeaders"
if ! built kmscube; then
    rm -rf "$BUILD/src/kmscube"
    cp -r "$SRC/kmscube-f60e50e" "$BUILD/src/kmscube"
    meson_pkg kmscube "$BUILD/src/kmscube" -Dgstreamer=disabled
fi
built weston || meson_pkg weston "$(unpack "$SRC/weston-14.0.1.tar.xz")" \
    -Dbackend-drm=true -Dbackend-headless=true -Dbackend-wayland=false -Dbackend-x11=false \
    -Dbackend-rdp=false -Dbackend-vnc=false -Dbackend-pipewire=false \
    -Dbackend-drm-screencast-vaapi=false -Drenderer-gl=true -Dxwayland=false \
    -Dsystemd=false -Dremoting=false -Dpipewire=false -Dimage-jpeg=false -Dimage-webp=false \
    -Dcolor-management-lcms=false -Dshell-ivi=false -Dshell-kiosk=true -Dshell-desktop=true \
    -Ddemo-clients=false -Dsimple-clients=shm -Dtools=terminal,info -Dtest-junit-xml=false \
    -Ddoc=false -Dtests=false -Dscreenshare=false -Dwcap-decode=false

# What the system needs at run time, under /usr/local.
mkdir -p "$DEST"
cp -a "$STAGE/usr/local/." "$DEST/"
rm -rf "$DEST/include" "$DEST/lib/pkgconfig" "$DEST/share/pkgconfig" "$DEST/share/man" \
    "$DEST/share/doc" "$DEST/share/wayland-protocols" "$DEST/share/aclocal" \
    "$DEST/share/gettext" "$DEST/share/info"
find "$DEST" -name '*.a' -delete
find "$DEST" -type f \( -name '*.so*' -o -perm -u+x \) -exec strip --strip-unneeded {} + 2>/dev/null || true
