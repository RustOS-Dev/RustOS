# Troubleshooting

## Building

**`error: could not find ... rust-src` / wrong toolchain** — run `rustup
show` in the repository; the toolchain from `rust-toolchain.toml` is
installed automatically. Remove overrides (`rustup override unset`).

**Userland build fails inside `build.rs`** — the error from the nested
`cargo build` of `userland/` is printed; fix it there
(`cd userland && cargo build --release --target x86_64-unknown-none`) or
build the kernel alone with `RUSTOS_SKIP_USERLAND=1`.

**`LLVM ERROR: Do not know how to split the result of this operator`**
— a dependency used SSE code on the soft-float targets. The kernel and
userland force portable crypto backends (`aes_force_soft`,
`polyval_force_soft`, `force-soft` for sha1/sha2); new crypto crates may
need the same treatment.

**"no host C compiler: dynamic linking tests not installed"** — install
`gcc` (only the `dynlink` scenario needs it).

## Running in QEMU

**No output / hangs before the kernel banner** — check that OVMF is
installed (`/usr/share/OVMF/OVMF_CODE*.fd`); the serial console is the
terminal running `cargo run`.

**`KERNEL PANIC: no init program found`** — the initramfs lacks the
userland (built with `RUSTOS_SKIP_USERLAND`, or the userland build was
skipped by clippy). Run a plain `cargo build`.

**`[smp] TLB shootdown ... timed out`** — a CPU did not answer an IPI
within 500 ms. Under an overloaded emulator this is harmless; otherwise
build with `RUSTOS_SMP_DEBUG=1` to print where the silent CPU was, and
report it.

## Real hardware

**The stick does not boot** — RustOS boots in UEFI mode only: disable
CSM/legacy boot and Secure Boot, and pick the USB stick's UEFI entry.

**Black screen after the bootloader** — the GOP framebuffer console needs
a UEFI graphics mode; the same output goes to COM1 (115200 8N1) if the
machine has one. `bugreport` saves the log and system state to the
storage partition for reading on another computer; if the machine hangs,
put `log.persist=1` into `/storage/etc/kernel.conf` so the log is written
there continuously (see [HARDWARE.md](HARDWARE.md)).

**USB keyboard does not work** — check `lsusb`; keyboards behind hubs and
docks are supported, but some laptops route the internal keyboard through
PS/2 (i8042) emulation only when legacy USB support is enabled in the
firmware settings.

**Disk not found** — `lsblk` lists what the drivers found; `dmesg | grep
-i 'nvme\|ahci\|block'` shows probing. Controllers in RAID/"RST" mode are
not supported: switch the SATA mode to AHCI in the firmware settings.

## Networking

**No address on `eth0`** — `ip addr`, `dmesg | grep net`; the link must be
up (`cat /sys/class/net/eth0/carrier`). `dhcp eth0 -t 20` restarts DHCP;
a static address can be set in `/storage/etc/network.conf`.

**`wget https://...` fails with `InvalidCertificate`** — check the clock
(`date`; fix with `ntpdate`), the host name, and that the CA bundle exists
(`/etc/ssl/certs/ca-certificates.crt`). `wget -k` skips verification;
`wget -v` shows the negotiated TLS version and cipher.

**Web pages do not load on a hotel/café Wi-Fi** — the network probably
has a captive portal: `netcheck` tells you, and `browse --portal` opens
the login page.

**Name resolution fails** — `cat /etc/resolv.conf`; add entries to
`/etc/hosts` for local names.

## Wi-Fi

**`wifi` says `state=no-firmware`** — the image was built without its stock
firmware (no network during the build, or `RUSTOS_FIRMWARE=0`; the build
prints a warning). Rebuild with network access, or install
`iwlwifi-ty-a0-gf-a0-72.ucode` and `iwlwifi-ty-a0-gf-a0.pnvm` into
`/storage/lib/firmware` (see [WIFI.md](WIFI.md)); the driver retries on the
next `wifi` command.

**Wi-Fi misbehaves in a way the log does not explain** — add
`iwlwifi.debug=1` to `/storage/etc/kernel.conf`, reboot, reproduce, and
run `bugreport`.

**`firmware start failed` / `no ALIVE`** — `dmesg | grep iwlwifi` shows
the failing step, `CSR_INT`/`GP_CNTRL` and the secure-boot status
registers. Check the hardware RF-kill switch (`RF-kill switch on` in the
log) and that the `.ucode` matches the adapter (TY for AX210, SO for
AX211/AX201).

**Network not found / association fails** — `wifi scan` must list it;
WPA3-only networks need management-frame protection (supported);
Enterprise networks are not supported.

**Connects but traffic stalls, is slow, or the firmware asserts** — the
802.11n/ac/ax and aggregation paths are the newest code. Narrow it down
in `/storage/etc/kernel.conf`, one line at a time, reconnecting after
each reboot: `iwlwifi.agg=0` (no A-MPDU), then `iwlwifi.mode=vht` (no
802.11ax), `iwlwifi.width=20`, and finally `iwlwifi.mode=legacy`. Report
which setting helps, with a `bugreport`; `wifi status` shows the mode and
rate in use.
