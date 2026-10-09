# Porting software to RustOS

RustOS speaks the Linux x86-64 system-call ABI, so C programs built
against **musl** run unchanged, statically or dynamically linked. The
repository pins upstream musl (`third_party/musl`, v1.2.5) and builds a
sysroot from it.

## The sysroot and `rustos-cc`

```sh
git submodule update --init third_party/musl
tools/build-musl.sh            # -> target/sysroot (about 20 s; cached)
tools/rustos-cc -O2 -o hello hello.c            # dynamic
tools/rustos-cc -O2 -static -o hello hello.c    # static
```

`tools/rustos-cc` is `gcc` with a specs file (like `musl-gcc`): musl's
headers, `crt1.o`/`crti.o`/`crtn.o`, `libc`, and the dynamic linker path
`/lib/ld-musl-x86_64.so.1`. The sysroot also carries the host's Linux UAPI
headers (`linux/`, `asm/`), which musl does not ship. Every kernel build
installs musl's `libc.so` as `/lib/ld-musl-x86_64.so.1` (musl's libc is
also its dynamic linker); shared libraries are found in `/lib`,
`/usr/local/lib` and `/usr/lib`.

What works: stdio, malloc (including `mremap`), pthreads with
thread-local storage, mutexes and condition variables (futexes),
`dlopen`, signals, `fork`/`exec`/`wait`, files and directories, TCP/UDP
sockets, `poll`/`select`/`epoll`, `eventfd`/`timerfd`/`signalfd`, file
`mmap`, pseudo-terminals. See [SYSCALLS.md](SYSCALLS.md) and
[LIMITATIONS.md](LIMITATIONS.md) for what is missing (e.g. `ptrace`,
System V IPC).

The boot image contains a few test programs built this way:
`musl-hello`, `musl-hello-static`, `musl-threads`, `musl-dlopen` (with
`/usr/lib/libplugin.so`) and `musl-libctest` (a small libc conformance
run); the `musl` boot scenario runs them.

## C++: `rustos-c++`

`tools/rustos-c++` is `g++` on the same sysroot with LLVM's C++ runtime
(libc++, libc++abi and libunwind 18) instead of the host's libstdc++. The
`libcxx` port builds that runtime into the sysroot (static, PIC); the
runtime is linked statically into each program.

```sh
tools/install-port.sh libcxx                  # once: runtime into target/sysroot
tools/rustos-c++ -O2 -std=c++20 -o hello hello.cpp
```

Exceptions, RTTI, threads, `<filesystem>`, `<regex>` and the rest of the
library work; `cxxtest` (installed by the port, run by the `cxx` scenario)
checks them. The specs add `--eh-frame-hdr` to every link, which unwinding
needs.

## Rust std programs: `rustos-cargo`

Rust programs that use the standard library build for
`x86_64-unknown-linux-musl` (in `rust-toolchain.toml`), linked
dynamically against RustOS's musl: `tools/rustos-cargo` is cargo with
`-C target-feature=-crt-static`, `tools/rustos-cc` as the linker and the
pinned nightly, whatever toolchain the project names.

```sh
tools/install-port.sh libcxx                 # once: LLVM's libunwind for panics
cd my-project && /path/to/RustOS/tools/rustos-cargo build --release
# C libraries from a port stage (pkg-config, -L and -rpath-link):
RUSTOS_STAGE=target/ports/build/labwc/stage tools/rustos-cargo build --release
```

* The unwinder (std asks for `-lgcc_s`) is LLVM's libunwind from the
  libcxx port, linked statically; RustOS has no `libgcc_s.so`.
* Build scripts' C code (the `cc` crate) is compiled with `rustos-cc` /
  `rustos-c++`; `-sys` crates find their libraries in `$RUSTOS_STAGE`
  through `tools/cross/rustos-pkg-config`.
* Inside the RustOS checkout (e.g. ports under `target/`) cargo runs from
  outside it, so the kernel's `.cargo/config.toml` (build-std, the kernel
  target) does not apply.

`ports/rust-hello` is the test program: threads, files, processes, a pty,
calloop (epoll, timerfd, eventfd), signals (sigaction, signalfd), unwinding,
sockets, `/proc`, CPU-time accounting, `dlopen("libEGL.so.1")` and a Wayland
connection. The `rust-std` scenario (`tests/scenarios/linux/rust-std.txt`,
weston and rust-hello ports in the image) runs it against a headless
Weston. eDEX-DE (`ports/edex-de`, [DESKTOP.md](DESKTOP.md)) is built the
same way.

## Meson and pkg-config

`tools/cross/meson-cross.ini.in` is a meson cross file (replace `@ROOT@`
with the repository path) and `tools/cross/rustos-pkg-config` a pkg-config
that only sees libraries staged under `$RUSTOS_STAGE` (installed there with
`DESTDIR`, prefix `/usr/local`). `ports/weston/build.sh` shows how ports use
them, including a native build for tools that run on the build host
(`wayland-scanner`, passed with `-Dbuild.pkg_config_path`).

## Ports

Recipes live in `ports/NAME/build.sh`; `tools/install-port.sh` downloads
the pinned source (checked by SHA-256), builds it with `rustos-cc` and
stages the result in `target/ports/NAME` (laid out like `/usr/local`).

| Port | Result |
|------|--------|
| `busybox` | BusyBox 1.36.1, static (`defconfig` minus a few applets needing missing kernel features) |
| `curl` | curl 8.10.1 with mbedTLS 3.6.2, static, HTTPS against `/etc/ssl/certs/ca-certificates.crt` |
| `quickjs` | QuickJS-ng 0.16.2 (from the `rquickjs-sys` crate's vendored copy): `qjs`, `qjsc`, `run-test262`, and `libquickjs.a` + headers for embedding |
| `wpa_supplicant`, `hostapd` | 2.11 with OpenSSL and libnl (nl80211) |
| `libcxx` | libc++/libc++abi/libunwind 18 into the sysroot, and `cxxtest` |
| `weston` | Weston 14 (DRM and headless backends, pixman and GL renderers, desktop and kiosk shells, `weston-terminal`), Mesa 26.2 (EGL on GBM and Wayland, GLES 3.1; softpipe, virgl, zink, iris, RADV, ANV), the Vulkan loader and `vulkaninfo`, `kmscube`, and their stack, shared: wayland 1.26, wayland-protocols, libxkbcommon + xkeyboard-config, pixman, cairo, freetype, fontconfig, libpng, zlib, expat, libffi, libdrm 2.4.134 (amdgpu, nouveau; `modetest`), libevdev, mtdev, libudev-zero, libinput, seatd/libseat, libdisplay-info. Needs meson, ninja, cmake, gperf, bison, flex, hwdata, glslang-tools, Python mako/pyyaml/ply and LLVM 18 with clang, libclc and SPIRV-LLVM-Translator on the build host (Mesa's `mesa_clc` is built for the host) |
| `labwc` | labwc 0.20 (on wlroots 0.20: DRM and libinput backends, GLES2 renderer, XWayland) and the foot terminal; Xwayland 24.1 with the X11 client libraries (libxcb, xcb-util(-wm), libX11, libXext, libXfixes, libxkbfile, xkbcomp, libxshmfence, libxcvt, libXfont2, libepoxy, libmd) and `xhello`, a minimal X client; GLib, Pango, HarfBuzz, FriBidi, libxml2, PCRE2, fcft, utf8proc and tllist; builds on the `weston` port's libraries (build that first) |
| `rust-hello` | Rust std test program (`tools/rustos-cargo`), on the `weston` port's libwayland |
| `edex-de` | eDEX-DE (RustOS-Dev/eDEX-DE-RS, pinned commit; `edex-de`, `edex-greeter`) built with `tools/rustos-cargo`, D-Bus 1.16 (`dbus-daemon`, a session bus), the JetBrains Mono Nerd Font, the session (`edex-session`, `desktop`) and its defaults; builds on the `labwc` port and needs `libcxx` in the sysroot (LLVM libunwind for Rust panics). See [DESKTOP.md](DESKTOP.md) |
| `gtk` | GTK 3.24 with only its Wayland backend (`gtk3-demo`, `gtk3-widget-factory`), gdk-pixbuf (PNG built in), ATK (from at-spi2-core, without D-Bus) and cairo-gobject; builds on the `labwc` port (build that first) |

Release images carry the desktop: `tools/release-ports.sh` builds `libcxx`,
`weston`, `labwc` and `edex-de` and puts only what eDEX-DE uses into
`target/ports-root` (no Weston desktop, demos or test tools);
`write_to_drive.sh` runs it before building the kernel.

Running labwc (default config in `/usr/local/etc/xdg/labwc`: its
autostart opens foot; Super+Return or Alt+Return opens another; X11
programs start Xwayland on `:0` when they first connect):

```sh
mkdir -p /tmp/xdg; chmod 700 /tmp/xdg
export XDG_RUNTIME_DIR=/tmp/xdg LIBSEAT_BACKEND=builtin
export WLR_RENDERER_ALLOW_SOFTWARE=1   # only where GL is softpipe (QEMU, no GPU driver)
labwc &
DISPLAY=:0 xhello &                    # an X11 window through Xwayland
WAYLAND_DISPLAY=wayland-0 gtk3-widget-factory &   # GTK 3 (ports/gtk)
```

The desktop ports together need about 1 GiB of RAM when they are in
the boot image (`--initramfs`).

Running Weston (kernel with a DRM driver, e.g. `--features linux-drivers`):

```sh
mkdir -p /tmp/xdg; chmod 700 /tmp/xdg
export XDG_RUNTIME_DIR=/tmp/xdg LIBSEAT_BACKEND=builtin
weston --backend=drm --renderer=pixman &     # or --renderer=gl (Mesa)
WAYLAND_DISPLAY=wayland-1 weston-terminal &
```

OpenGL ES and Vulkan come from Mesa: `kmscube` draws on a KMS display
with EGL/GBM (softpipe through kms_swrast where there is no supported
GPU: the `gl-kmscube` scenario), and `weston --renderer=gl` composites
with it (`desktop-gl`). On AMD and Intel GPUs Mesa's RADV/ANV, iris and
zink (OpenGL on Vulkan) take over once the kernel drives the GPU
(`linux.enable=amdgpu` / `i915`).

libseat's embedded seat takes the console (VT_PROCESS, `K_OFF`) and DRM
master; libinput finds input devices through libudev-zero, which reads
`/sys/dev/char`, `/sys/class/input` and uevents. The `desktop` scenario
does this on QEMU's standard VGA and types into the terminal.

The ports named in `ports/default.list` (busybox, curl, quickjs,
wpa_supplicant, hostapd) are built by the
kernel build on first use and installed in the boot image under
`/usr/bin`; later builds reuse them until their `build.sh` changes. Set
`RUSTOS_PORTS=0` to build without them (a port that fails to build, e.g.
without network access for its sources, is left out with a warning).
Downloaded sources are kept in `target/ports/src` (or
`$RUSTOS_PORTS_CACHE`); CI caches that directory. BusyBox applets are run
as `busybox APPLET` so they do not shadow the rbox tools.

```sh
tools/install-port.sh busybox               # stage only
tools/install-port.sh --initramfs busybox   # also put it in the boot image
cargo build                                 # (then boot as usual)
```

The `musl` scenario runs a test262 subset (`tests/test262.list`, pinned
commit, fetched by `tools/fetch-test262.sh`) with `run-test262` on RustOS
and expects the same result as on the host; the known failures of this
QuickJS-ng version are listed in `tests/test262_errors.txt`.

Staged files can instead be copied to `/storage/usr/local` on a RustOS
drive. `--initramfs` adds everything in `target/ports-root` to the next
kernel build; delete that directory to drop the ports again.

A new port needs a `ports/NAME/build.sh` that takes `SRC_DIR BUILD_DIR
DEST_DIR`, uses `fetch URL SHA256 FILE` from `tools/port-lib.sh`, builds
with `CC=tools/rustos-cc` (usually static, `--host=x86_64-linux-musl` for
autoconf) and installs into `DEST_DIR/bin`, `DEST_DIR/lib`, ...

## RustOS's own dynamic linker

Rust and libc-free programs in this repository use `/lib/ld-rustos.so.1`
(`userland/ldso`) instead: eager binding, thread-local storage (static
TLS for the program and its libraries, `__tls_get_addr`), and `dlopen`/
`dlsym`/`dlclose`/`dlerror` (link against the `/lib/libdl.so` stub). The
`dynlink` scenario exercises it (`dyntest`, `tlstest`, `dltest`).
