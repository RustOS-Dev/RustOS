# The eDEX desktop

[eDEX-DE](https://github.com/RustOS-Dev/eDEX-DE-RS) is the RustOS desktop: a sci-fi shell
(terminal tabs and application windows in the centre panel, file browser, system monitor,
settings) on its own Wayland compositor, `edex-comp`, built with Smithay. It is milestone **M43**
in [ROADMAP.md](ROADMAP.md).

Status:

* **The full desktop runs on softpipe.** With Mesa (M41) in the image, `svc start edex` starts
  `edex-comp --greeter` on tty1; edex-comp renders with GLES through Mesa's GBM/EGL (softpipe via
  kms_swrast on Linux bochs DRM, `--features linux-drivers`), the login screen (`edex-greeter`)
  checks root's password through `edex-auth`, and edex-comp starts the session: the shell
  (`edex-de`) with its terminal, file browser and system panels. `SUPER+Alt+L` locks the session
  with `edex-greeter --lock`, the password unlocks it, and logging out brings the login screen
  back. The shell and the greeter draw with wgpu's GL backend on softpipe (no Vulkan device
  without a GPU: RADV and ANV need hardware, there is no lavapipe). The `desktop-edex-session`
  scenario covers all of it.
* edex-comp falls back to **pixman** on DRM dumb buffers when GBM/EGL do not come up, and
  `EDEX_RENDERER=pixman` forces it; the `desktop-edex` scenario runs it with `weston-terminal`.
* Software rendering is slow under QEMU without KVM: the first frame of the login screen and of the
  shell takes about 30 s, later frames a few seconds. The shell and the greeter turn their
  animations off and redraw only what changed on a software rasterizer; edex-comp itself is much
  cheaper with `EDEX_RENDERER=pixman` there (softpipe clients send shared-memory buffers).
* The RustOS side that does not need graphics is done: CPU-time accounting, the `svc` service
  manager, Linux signal frames for Go and Rust programs, and the ports below. In an image with them,
  `tor` checks its configuration, `wg` makes keys, `lyrebird` and `snowflake-client` start, and a
  dynamically linked Rust program unwinds a panic through the `libunwind` port.
* The session runs as root (RustOS has no other accounts yet), without the desktop services of
  M42 (seatd, D-Bus, PipeWire, UPower, `rustos-nmd`): their panels say "unavailable".

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
| Outputs, rendering | DRM/KMS (M35, M38–M40), Mesa GBM/EGL and GLES (M41; softpipe without a GPU driver), pixman on dumb buffers as the fallback |
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
| WireGuard | `wg` (port) and the kernel driver (LinuxKPI `wireguard` group, `--features linux-wireguard`; `ip link add wg0 type wireguard`) |

## Building it into the image

Like Weston (M37), the desktop is installed under `/usr/local` from the ports tree:

```sh
git submodule update --init third_party/musl
rustup target add x86_64-unknown-linux-musl
tools/install-port.sh --initramfs weston      # Wayland, libinput, seatd, pixman, Mesa, ...
tools/install-port.sh --initramfs libunwind   # libgcc_s.so.1 for dynamically linked Rust
tools/install-port.sh --initramfs edex-de     # EDEX_SRC=~/eDEX-DE-RS for a local checkout
tools/install-port.sh --initramfs jetbrains-mono-nerd
cargo build --features linux-drivers          # DRM (bochs, virtio-gpu, simpledrm) and HID
```

`edex-de` builds against the weston port's stage (`target/ports/build/weston/stage`, or
`RUSTOS_WESTON_STAGE`, through `tools/cross/rustos-pkg-config`), links with `tools/rustos-cc` and
`libgcc_s.so.1` from the `libunwind` port, and fails if a binary needs a library the stage does not
have. Every binary is built with its default features: `edex-comp` links `libgbm`, `libxkbcommon`,
`libinput`, `libseat`, `libudev` and `libpixman-1` and loads `libEGL` at run time; `edex-de` and
`edex-greeter` load `libwayland-client`, `libEGL` and `libvulkan` at run time. With Mesa and eDEX
in the initramfs the VM needs more than 512 MiB (the desktop scenarios run with `-m 1G`). The Privacy panel's ports are
optional: `tor`, `tor-pt` and `wireguard-tools`. Together they add about 80 MB to the image: Tor
with its GeoIP files (28 MB), the Go pluggable transports (33 MB); the fonts are 19 MB.

## Turning it on

The desktop with its login screen, on tty1 at every boot:

```sh
svc enable edex && svc start edex
```

The `edex` service runs on tty1, where the console shell also runs (its output still reaches the
serial port; use another console, `Ctrl+Alt+F2`, for a text shell). RustOS's root account has no
password until you set one (`passwd`, or `printf '\nNEW\n' | edex-auth passwd root`); the login
screen lists root. eDEX-DE's own configuration is `~/.config/edex-de/config.toml`. Once the M42
services are ported, enable them too (`svc enable seatd`, `dbus`, `upower`, `rustos-nmd`).

The compositor alone, with a Wayland client of your choice, from a console:

```sh
mkdir -p /tmp/xdg; chmod 700 /tmp/xdg
export XDG_RUNTIME_DIR=/tmp/xdg LIBSEAT_BACKEND=builtin
edex-comp run --run weston-terminal > /tmp/edex.log 2>&1 &
edex-comp state                       # outputs and windows
edex-comp msg '{"cmd":"exit"}'
```

`--run` starts only the given programs, not the eDEX session; windows tile over the whole output,
on the theme's background. `SUPER+Shift+Q` closes a window.

## Until users are separate

RustOS runs everything as uid 0 today, and eDEX-DE's session does too: the shell calls `svc`
(whose control FIFO is root-only), the Tor helpers in `/usr/local/libexec/edex-de` (they write
`/storage/etc/tor`) and writes the backlight directly. When sessions run as ordinary users these
go through a small privilege broker (an open M43 item), and `edex-auth` is installed set-uid root;
it already only lets callers change their own account.

## Testing

eDEX-DE's own unit tests run on a Linux host (its CI). On RustOS two scenarios run it on QEMU's
standard VGA (Linux bochs DRM, a kernel built with `--features linux-drivers`, the weston,
libunwind and edex-de ports installed with `--initramfs`, `-m 1G` from their `.args` files).

`tests/scenarios/linux/desktop-edex.txt`, the compositor without Mesa:

1. starts `EDEX_RENDERER=pixman edex-comp run --run weston-terminal` with
   `LIBSEAT_BACKEND=builtin` and waits for `rendering with pixman` and `output Virtual-1 enabled
   1280x800 (pixman)` in its log;
2. checks `edex-comp state` lists the output and the `weston-terminal` window, tiled and focused;
3. checks a `screendump`: the theme's background in the outer gap, the focus border in the
   theme's accent, the terminal over the rest of the output;
4. types `echo edex-typed > /tmp/k` with `sendkey` and reads the file on the serial console;
5. makes edex-comp exit with `edex-comp msg '{"cmd":"exit"}'`.

`tests/scenarios/linux/desktop-edex-session.txt`, the session (also needs the
`jetbrains-mono-nerd` port):

1. sets root's password with `edex-auth passwd` and starts the `edex` service; edex-comp's log
   (tty1, mirrored on the serial port) shows GLES on `Virtual-1` and the greeter on softpipe;
2. checks the login screen's screendump (theme background, the login box's frame and header);
3. logs in with `sendkey` (Enter picks root, the password, Enter), waits for the shell, checks
   `edex-de ipc state` (connected to edex-comp, softpipe, terminal focused), `edex-comp state` and
   the shell's screendump (top bar, accent, side panels and their frames, terminal background);
4. types `echo shell-typed > /tmp/s` into the shell's terminal and reads the file on the serial
   console;
5. locks with `SUPER+Alt+L` (`edex-comp` reports `locked`, the screendump shows the lock screen
   over the shell), unlocks with the password (the shell is back);
6. logs out with `edex-comp msg '{"cmd":"exit"}'`: the service starts edex-comp again and the
   login screen comes back.

```sh
tools/run-scenarios.sh target/x86_64-rustos/debug/rustos tests/scenarios/linux/desktop-edex.txt \
    tests/scenarios/linux/desktop-edex-session.txt
```

With Mesa and eDEX-DE in the image, `desktop-gl` needs `-m 1G` too (CI runs it before installing
eDEX-DE).
