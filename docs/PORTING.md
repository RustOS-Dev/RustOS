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
| `weston` | Weston 14 (DRM and headless backends, pixman renderer, desktop and kiosk shells, `weston-terminal`) and its stack, shared: wayland 1.23, wayland-protocols, libxkbcommon + xkeyboard-config, pixman, cairo, freetype, fontconfig, libpng, zlib, expat, libffi, libdrm (with `modetest`), libevdev, mtdev, libudev-zero, libinput, seatd/libseat, libdisplay-info. Needs meson, ninja, gperf, bison and hwdata on the build host |

Running Weston (kernel with a DRM driver, e.g. `--features linux-drivers`):

```sh
mkdir -p /tmp/xdg; chmod 700 /tmp/xdg
export XDG_RUNTIME_DIR=/tmp/xdg LIBSEAT_BACKEND=builtin
weston --backend=drm --renderer=pixman &
WAYLAND_DISPLAY=wayland-1 weston-terminal &
```

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

### Desktop ports

The eDEX-DE desktop and the ports it uses are installed like Weston, under
`/usr/local` (see [DESKTOP.md](DESKTOP.md)):

```sh
tools/install-port.sh --initramfs weston       # the Wayland stack (M37)
tools/install-port.sh --initramfs libunwind
tools/install-port.sh --initramfs edex-de      # needs the weston stage
tools/install-port.sh --initramfs jetbrains-mono-nerd
tools/install-port.sh --initramfs tor          # optional: Privacy panel
tools/install-port.sh --initramfs tor-pt
tools/install-port.sh --initramfs wireguard-tools
```

| Port | Result |
|------|--------|
| `libunwind` | LLVM libunwind 19.1.7 as `/usr/local/lib/libgcc_s.so.1`: the unwinder dynamically linked Rust programs (`x86_64-unknown-linux-musl` without `crt-static`) need |
| `jetbrains-mono-nerd` | JetBrains Mono Nerd Font 3.4.0 (regular and Mono, four styles), eDEX-DE's font |
| `tor` | Tor 0.4.8.17 with OpenSSL 3.0.16, libevent 2.1.12 and zlib 1.3.1, static; GeoIP files in `/usr/local/share/tor` |
| `tor-pt` | lyrebird 0.6.1 (obfs4) and snowflake-client 2.11.0, static Go builds (needs the host's Go), and Snowflake's default bridge lines |
| `wireguard-tools` | `wg` 1.0.20250521, static (keys and interface configuration; `wg-quick` needs bash and iproute2) |
| `edex-de` | eDEX-DE at a pinned commit (`EDEX_SRC=DIR` for a checkout): `edex-comp`, `edex-de`, `edex-greeter`, `edex-auth`, themes, Tor helpers, greeter config; built with Rust for `x86_64-unknown-linux-musl` against the weston port's stage |

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
