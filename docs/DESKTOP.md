# The eDEX desktop

[eDEX-DE](https://github.com/RustOS-Dev/eDEX-DE-RS) is the RustOS desktop: a sci-fi shell
(terminal tabs and application windows in the centre panel, file browser, system monitor,
settings) on its own Wayland compositor, `edex-comp`, built with Smithay. It is milestone **M43**
in [ROADMAP.md](ROADMAP.md).

Status:

* **The compositor runs.** `edex-comp` builds against the M37 Wayland stack (the weston port's
  stage) and runs on Linux DRM (`--features linux-drivers`; tested on QEMU's bochs) with
  libseat's builtin seat, libinput and libudev-zero. Without Mesa it is built without its `gpu`
  feature and renders with **pixman** into DRM dumb buffers. The `desktop-edex` scenario runs it
  with `weston-terminal`, tiles the window, types into it and screendumps the result.
* **The shell and the greeter wait for Mesa (M41).** `edex-de` and `edex-greeter` build and are
  installed, but draw with wgpu (Vulkan, or GLES through EGL). The `edex` service starts
  `edex-comp --greeter`, whose login screen is `edex-greeter`, so it waits for M41 too.
* The RustOS side that does not need graphics is done: CPU-time accounting, the `svc` service
  manager, Linux signal frames for Go and Rust programs, and the ports below. In an image with them,
  `tor` checks its configuration, `wg` makes keys, `lyrebird` and `snowflake-client` start, and a
  dynamically linked Rust program unwinds a panic through the `libunwind` port.

What eDEX-DE needs from RustOS, call by call, is listed in eDEX-DE's
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
| Outputs, rendering | DRM/KMS (M35, M38–M40); pixman on dumb buffers now, Mesa GBM/EGL from M41 |
| Seat, VT switching | M36 VT switching and DRM master handover, libseat's builtin seat (M37), seatd (M42) |
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

Like Weston (M37), the desktop is installed under `/usr/local` from the ports tree:

```sh
git submodule update --init third_party/musl
rustup target add x86_64-unknown-linux-musl
tools/install-port.sh --initramfs weston      # Wayland, libinput, seatd, pixman, ...
tools/install-port.sh --initramfs libunwind   # libgcc_s.so.1 for dynamically linked Rust
tools/install-port.sh --initramfs edex-de     # EDEX_SRC=~/eDEX-DE-RS for a local checkout
tools/install-port.sh --initramfs jetbrains-mono-nerd
cargo build --features linux-drivers          # DRM (bochs, virtio-gpu, simpledrm) and HID
```

`edex-de` builds against the weston port's stage (`target/ports/build/weston/stage`, or
`RUSTOS_WESTON_STAGE`, through `tools/cross/rustos-pkg-config`), links with `tools/rustos-cc` and
`libgcc_s.so.1` from the `libunwind` port, and fails if a binary needs a library the stage does not
have. `edex-comp` is built with `--no-default-features` (pixman; no GBM or EGL); `edex-comp` needs
`libwayland-server`, `libxkbcommon`, `libinput`, `libseat`, `libudev` and `libpixman-1`. The Privacy panel's ports are
optional: `tor`, `tor-pt` and `wireguard-tools`. Together they add about 80 MB to the image: Tor
with its GeoIP files (28 MB), the Go pluggable transports (33 MB); the fonts are 19 MB.

## Turning it on

Today (M37, no Mesa): the compositor with a Wayland client of your choice, from a console, like
Weston:

```sh
mkdir -p /tmp/xdg; chmod 700 /tmp/xdg
export XDG_RUNTIME_DIR=/tmp/xdg LIBSEAT_BACKEND=builtin
edex-comp run --run weston-terminal > /tmp/edex.log 2>&1 &
edex-comp state                       # outputs and windows
edex-comp msg '{"cmd":"exit"}'
```

`--run` starts only the given programs, not the eDEX session (the shell, D-Bus, PipeWire);
windows tile over the whole output, on the theme's background. `SUPER+Shift+Q` closes a window.

Once Mesa is in (M41) and the desktop services (M42), the full desktop with its login screen:

```sh
svc enable seatd && svc enable dbus && svc enable upower && svc enable rustos-nmd
svc enable edex && svc start edex
```

The `edex` service runs on tty1, where the serial console's shell also runs; use another console
(`Ctrl+Alt+F2`) for a text shell. eDEX-DE's own configuration is `~/.config/edex-de/config.toml`.

## Until users are separate

RustOS runs everything as uid 0 today, and eDEX-DE's session does too: the shell calls `svc`
(whose control FIFO is root-only), the Tor helpers in `/usr/local/libexec/edex-de` (they write
`/storage/etc/tor`) and writes the backlight directly. When sessions run as ordinary users these
go through a small privilege broker (an open M43 item), and `edex-auth` is installed set-uid root;
it already only lets callers change their own account.

## Testing

eDEX-DE's own unit tests run on a Linux host (its CI). On RustOS, the
`tests/scenarios/linux/desktop-edex.txt` scenario (QEMU's standard VGA, Linux bochs DRM, a kernel
built with `--features linux-drivers` and the weston, libunwind and edex-de ports installed with
`--initramfs`):

1. starts `edex-comp run --run weston-terminal` with `LIBSEAT_BACKEND=builtin` and waits for
   `rendering with pixman` and `output Virtual-1 enabled 1280x800 (pixman)` in its log;
2. checks `edex-comp state` lists the output and the `weston-terminal` window, tiled and focused;
3. checks a `screendump`: the theme's background in the outer gap, the focus border in the
   theme's accent, the terminal over the rest of the output;
4. types `echo edex-typed > /tmp/k` with `sendkey` and reads the file on the serial console;
5. makes edex-comp exit with `edex-comp msg '{"cmd":"exit"}'`.

```sh
python3 tools/qemu-console-test.py target/x86_64-rustos/debug/rustos tests/scenarios/linux/desktop-edex.txt
```

Once Mesa (M41) and M42 are in, a `desktop-edex-session` scenario will also enable seatd, dbus and
`edex`, log in at the greeter with `sendkey`, check `edex-de ipc state` (`"compositor":
{"connected": true}`), open a terminal window with `SUPER+Shift+Return`, compare the shell's
colours, and lock, unlock and log out.
