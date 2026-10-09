# The desktop: eDEX-DE

RustOS's graphical session is [eDEX-DE](https://github.com/RustOS-Dev/eDEX-DE-RS), a sci-fi
desktop shell written in Rust. It provides a terminal, a file browser, a system dashboard, a
launcher, settings, notifications and a power menu. On RustOS it runs as the shell of the
[labwc](https://labwc.github.io) Wayland compositor (M42). eDEX draws with wgpu's GLES backend on
Mesa's EGL. With no GPU 3D driver that means Mesa's software renderer (softpipe).

Images built with `write_to_drive.sh` (and the release workflow) start the desktop at boot on
the first console. The serial console keeps its shell.

## Starting and stopping

| | |
|---|---|
| Boot | `/etc/rc` runs `edex-session --boot` unless `kernel.conf` has `desktop=none` |
| Shell | `desktop start`, `desktop stop`, `desktop status` |
| Logs | `/var/log/edex-session.log` (session), `/tmp/xdg-runtime-0/labwc.log` (labwc and eDEX) |

`kernel.conf` keys (in `/storage/etc/kernel.conf`, read through `/proc/cmdline`):

| Key | Effect |
|---|---|
| `desktop=none` | no desktop at boot (`0`, `off` and `no` work too) |
| `desktop.login=1` | the eDEX greeter asks for a login first (see below) |
| `desktop.user=NAME` | the account the desktop runs as without the greeter (default `root`) |

In QEMU the same keys can come from the host as the fw_cfg file `opt/rustos/kernel.conf`
(`-fw_cfg name=opt/rustos/kernel.conf,string=desktop=none`). The test harness passes
`desktop=none` to every scenario except `desktop-edex`.

## What the session runs

`edex-session` (`ports/edex-de/edex-session`):

1. Exports the environment: `LIBSEAT_BACKEND=builtin` (seatd's built-in backend, no seat
   daemon), `WLR_RENDERER_ALLOW_SOFTWARE=1`, `WGPU_BACKEND=gl`, `XDG_*`, and
   `MESA_EXTENSION_OVERRIDE=-GL_ARB_compute_shader`. The last one works around a wgpu 29 bug: wgpu
   binds textures with layout qualifiers whenever the GL context offers compute shaders, and
   softpipe offers `GL_ARB_compute_shader` in a GLSL 3.30 context that cannot express them. All
   textures then read unit 0, and no text is drawn. Programs started from eDEX inherit the
   override.
2. Creates `XDG_RUNTIME_DIR` (`/tmp/xdg-runtime-UID`).
3. Starts a D-Bus session bus (`dbus-daemon --session`, D-Bus 1.16) for eDEX's notifications
   server. Without `dbus-daemon` the desktop runs with no notifications. There is no system bus.
4. Writes labwc's configuration from eDEX's settings on first start (`edex-de labwc-config` into
   `~/.config/edex-de/labwc`), then runs `labwc -C ~/.config/edex-de/labwc -s 'edex-session
   --shell-loop'`. The shell loop restarts `edex-de run --wm labwc` when it crashes, up to five
   times a minute, and then ends the session.
5. When labwc exits, stops the bus and returns to the text console.

## Keys

The labwc bindings are generated from eDEX's settings (Settings → Window manager, written to
`rc.xml`):

| Key | Action |
|---|---|
| Super, Super+Space | launcher |
| Super+Return | focus the terminal |
| Super+F1 | focus the file browser |
| Super+Shift+Return | foot |
| Super+Comma | settings |
| Super+P / Super+N / Super+Escape | privacy, notifications, power menu |
| Super+Q, Alt+F4 / Super+Shift+Q | close / kill the window |
| Super+F | maximize (eDEX's: full width, side panels hidden) |
| Super+Shift+F | fullscreen |
| Super+M | minimize into a tab |
| Super+V | labwc maximize |
| Super+Tab, Alt+Tab / Super+Shift+Tab | next / previous window |
| Super+Ctrl+F | toggle the side panels |
| Super+Shift+R | reload labwc and eDEX |
| Super+Shift+arrows | move the window to an edge |
| Volume and brightness keys | ALSA mixer, backlight |

Inside eDEX: Ctrl+Shift+T and Ctrl+Shift+W open and close terminal tabs.

## Windows

eDEX talks to labwc through wlr-foreign-toplevel-management. Windows appear as tabs in the
centre panel, where they can be activated, minimized, maximized and closed. labwc maximizes
normal windows into the area between eDEX's panels, which is the terminal slot. eDEX's maximize
hides the side panels so a window gets the full width. labwc has no workspaces, so the workspace
strip is hidden.

Keyboard focus: labwc moves focus away from a layer surface that turns from exclusive to
on-demand keyboard interactivity. So while the shell has the keyboard, eDEX keeps an exclusive
grab. It yields the grab when a new window appears, when a tab activates or restores a window,
and when the pointer leaves the shell for a window.

## Configuration

* `~/.config/edex-de/config.toml`. On first start it is copied from
  `/usr/local/share/edex-de/config.toml`, RustOS's defaults: Tron theme, JetBrains Mono Nerd Font,
  foot as the launcher's terminal, and animations, scanlines and the boot animation off, because
  everything is drawn in software.
* Themes: `/usr/local/share/edex-de/themes`. Fonts: `/usr/local/share/fonts`.
* `edex-de ipc …` controls the running shell: `state` (JSON, including what the dashboard shows),
  `show launcher|settings|privacy|notifications|power`, `focus terminal|filesystem`,
  `action maximize|minimize|close`, `notify SUMMARY BODY`, `reload`.

## What RustOS provides to eDEX's settings and panels

eDEX detects RustOS from `/proc/version` and uses its `rustos` system backends:

| Area | Backend |
|---|---|
| Audio | ALSA mixer controls (`/dev/snd/controlC*`), OSS mixer fallback |
| Network | `ip -br addr`; Wi-Fi through `wifi` (status, scan, connect, forget) |
| Bluetooth | `bt` (status, scan, pair, connect) |
| Power | `/sys/class/power_supply`, `poweroff`, `reboot` |
| Brightness | `/sys/class/backlight` |
| Users | `/etc/passwd`, `/etc/shadow` (on the storage partition first) |
| Services | the commands `/etc/rc` starts |
| Dashboard | `/proc/stat` (per CPU), `/proc/meminfo`, `/proc/loadavg`, `/proc/PID/stat`, `/sys/class/net/*/statistics`, `/proc/mounts` and statfs |

Rows with no backend are hidden or marked "not available on RustOS": Tor, Tailscale, VPNs and
WireGuard (also in the top bar), fingerprint login, suspend and hibernate (RustOS has no S3),
lid action, screen lock and idle, power profiles, the firewall, the keyring and systemd services.

## Greeter

With `desktop.login=1`, `edex-session` first runs `edex-greeter --backend local` fullscreen under
labwc (`/usr/local/share/edex-de/greeter/rc.xml`). It checks the password against `/etc/shadow`
(SHA-512 crypt; the storage partition's copy first, as `login` and `passwd` use it). An account
with an empty password field logs in without one. After a login the desktop runs as that user
(`edex-greeter run-as USER -- edex-session --desktop`). Logging out returns to the greeter.

Without `desktop.login`, the session logs in automatically as `desktop.user` (root by default).

## Building

```sh
tools/install-port.sh --initramfs libcxx
tools/install-port.sh --initramfs weston
tools/install-port.sh --initramfs labwc
tools/install-port.sh --initramfs edex-de
cargo build --features linux-drivers
```

`tools/release-ports.sh` builds the same chain and keeps only what the desktop uses. Weston's
desktop and terminal, kmscube and the DRM and GL test tools stay out. This is
the image `write_to_drive.sh` writes; `RUSTOS_DESKTOP=0` writes one without the desktop.
`ports/edex-de/build.sh` pins the eDEX-DE commit, D-Bus 1.16.2 and the Nerd Font (sha256).
eDEX-DE is GPL-3.0; its licence and source commit are in
`/usr/local/share/licenses/edex-de`.

## Testing

`tests/scenarios/linux/desktop-edex.txt` (QEMU, bochs DRM, softpipe, 1.5 GiB, CI) checks the
following:

* the session starts at boot: labwc backend, D-Bus notifications server, Tron theme, softpipe;
* the dashboard shows CPU load, memory in use and processes;
* a screendump has the Tron colours;
* keys typed with QEMU's `sendkey` reach eDEX's terminal and run a command;
* the launcher opens over IPC, and foot started from it tiles into the terminal slot.

`desktop-edex-login.txt` boots with `desktop.login=1`. It checks that the greeter fills the screen
without a title bar and that Enter logs root in, after which the greeter exits and the desktop
starts.

## Performance and limits

* All drawing is in software. Under QEMU's TCG a full eDEX frame takes seconds. eDEX handles input
  between frames, so typing shows with a delay. labwc, which composites with softpipe too, uses
  most of a CPU while windows change.
* Hardware: none of this has run on real hardware yet. The image's Mesa has iris (Intel) and
  zink on the AMD and Intel Vulkan drivers, for the GPUs RustOS drives (amdgpu and i915, opt-in in
  `kernel.conf`). Whether eDEX gets hardware rendering there is untested. On every other display
  (efidrm, bochs, virtio-gpu) it draws with softpipe.
