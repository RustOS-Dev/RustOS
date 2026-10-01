# The eDEX desktop

[eDEX-DE](https://github.com/RustOS-Dev/eDEX-DE-RS) is the RustOS desktop: a sci-fi shell
(terminal tabs and application windows in the centre panel, file browser, system monitor,
settings) on its own Wayland compositor, `edex-comp`, built with Smithay. It is milestone **M43**
in [ROADMAP.md](ROADMAP.md).

Status: the RustOS side that does not need graphics is done (CPU-time accounting, the `svc`
service manager, Linux signal frames for Go and Rust programs, the ports below except `edex-de`
itself). In a `RUSTOS_DESKTOP=1` image under QEMU, `tor` checks its configuration, `wg` makes keys,
`lyrebird` and `snowflake-client` start, and a dynamically linked Rust program unwinds a panic
through the `libunwind` port. eDEX-DE builds for RustOS once M37 supplies the desktop libraries
and runs once the graphics milestones (M35–M42) are in. What it needs from RustOS, call by call,
is listed in eDEX-DE's
[docs/rustos.md](https://github.com/RustOS-Dev/eDEX-DE-RS/blob/master/docs/rustos.md).

## What runs

```
init → svc → service "edex" (tty1, root): edex-comp --greeter
         ├── edex-greeter (login screen) → edex-auth checks /storage/etc/shadow, /etc/shadow
         └── after login, as the user: Xwayland, dbus-daemon --session, pipewire, wireplumber,
             xdg-desktop-portal, edex-de (the shell); restarted when they crash
system services (svc): seatd, dbus, upower, rustos-nmd; tor when the Privacy panel turns it on
```

| eDEX-DE feature | RustOS piece |
|---|---|
| Outputs, rendering | DRM/KMS (M35, M38–M40), Mesa GBM/EGL (M41; softpipe from M37) |
| Seat, VT switching | M36 VT switching and DRM master handover, seatd (M42) |
| Input | evdev, libinput, libxkbcommon (M37) |
| Wayland clients, shared buffers | `AF_UNIX` with `SCM_RIGHTS`, memfd (M36) |
| Live configuration reload | inotify (M36) |
| System monitor | `/proc/stat`, `/proc/[pid]/stat`, `/proc/loadavg` (CPU-time accounting, done) |
| Services panel, Tor | `svc` (done; [SERVICES.md](SERVICES.md)) |
| Network panel | `rustos-nmd`, the NetworkManager D-Bus subset (M42) |
| Bluetooth panel | `/dev/bluetooth` ([BLUETOOTH.md](BLUETOOTH.md)) |
| Audio | PipeWire, WirePlumber (M42) |
| Battery, power | UPower, the login1 subset (M42) |
| Accounts, login, lock | `/etc/passwd`, `/storage/etc/shadow` (SHA-512 crypt) through `edex-auth` |
| WireGuard | `wg` (port) and the kernel driver (LinuxKPI `wireguard` group, after M28) |

## Building it into the image

```sh
git submodule update --init third_party/musl
rustup target add x86_64-unknown-linux-musl
RUSTOS_DESKTOP=1 cargo build                          # builds ports/desktop.list, then the kernel
EDEX_SRC=~/eDEX-DE-RS RUSTOS_DESKTOP=1 cargo build    # with a local eDEX-DE checkout
```

`ports/desktop.list` is built in order: `libunwind` (the `libgcc_s.so.1` that dynamically linked
Rust programs need), `jetbrains-mono-nerd`, `tor`, `tor-pt`, `wireguard-tools`, `edex-de`. A port
that fails is left out with a warning, as with the default ports; until M37 adds
`tools/cross/pkg-config` and the desktop libraries, that is `edex-de`. The ports are rebuilt only
when their `build.sh` changes (delete `target/ports/NAME` to force it). Besides eDEX-DE itself they add
about 80 MB to the image: Tor with its GeoIP files (28 MB), the Go pluggable transports (33 MB)
and the fonts (19 MB).

## Turning it on

```sh
svc enable seatd && svc enable dbus && svc enable upower && svc enable rustos-nmd
svc enable edex && svc start edex
```

The `edex` service runs on tty1, where the serial console's shell also runs; use another console
(`Ctrl+Alt+F2`) for a text shell. eDEX-DE's own configuration is `~/.config/edex-de/config.toml`.

## Until users are separate

RustOS runs everything as uid 0 today, and eDEX-DE's session does too: the shell calls `svc`
(whose control FIFO is root-only), the Tor helpers in `/usr/libexec/edex-de` (they write
`/storage/etc/tor`) and writes the backlight directly. When sessions run as ordinary users these
go through a small privilege broker (an open M43 item), and `edex-auth` is installed set-uid root;
it already only lets callers change their own account.

## Testing

eDEX-DE's own unit tests run on a Linux host (its CI). On RustOS, once M42 is in, a
`desktop-edex` scenario (QEMU with virtio-gpu) will:

1. enable seatd, dbus and `edex`, boot, and wait for `edex-comp` in `svc status edex --json`;
2. log in at the greeter with `sendkey` (user `root`, empty password);
3. check `edex-comp state` lists the output and `edex-de ipc state` reports a configured canvas and
   `"compositor": {"connected": true}`;
4. open a terminal window (`SUPER+Shift+Return`) and check it is tiled inside the app area;
5. compare a `screendump` region against the theme's colours;
6. lock (`SUPER+Alt+L`), unlock, and log out.
