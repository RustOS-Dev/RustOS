# RustOS

An x86-64 operating system written in Rust: a preemptive SMP kernel with
ring-3 processes, a Linux-compatible system-call ABI, NVMe/AHCI/USB storage,
FAT and ext2/3/4 filesystems, a full USB stack, an in-tree TCP/IP stack with
wired, USB and Intel Wi-Fi 6E drivers (WPA2/WPA3), HTTPS, a POSIX-like shell
and a small userland — all booted by UEFI.

Built on the foundation of [Philipp Oppermann's "Writing an OS in
Rust"](https://os.phil-opp.com/) and grown well beyond it.

## Features

**Platform** — UEFI boot (bootloader 0.11), GOP framebuffer console and
serial, ACPI with an AML interpreter (power-off, `_PRT` routing), LAPIC/
x2APIC and IOAPIC, MSI/MSI-X, PCIe ECAM, HPET/TSC/RTC timekeeping, **SMP**
(all cores, IPI TLB shootdown).

**Processes** — per-process address spaces with demand paging,
copy-on-write `fork` and file-backed `mmap` (page cache), `clone` threads
with futexes, a per-CPU scheduler with affinity and nice, `execve` of
static, PIE and **dynamically linked** ELF programs (`ld-rustos`, musl's
`ld-musl`), POSIX signals and job control, pipes, epoll/eventfd/timerfd/
signalfd, pseudo-terminals, ~180 Linux system calls.

**Storage** — NVMe, AHCI, virtio-blk and USB mass storage; GPT/MBR; a
write-back buffer cache; FAT12/16/32 with long names (read/write),
**ext2/ext3/ext4 read/write** (jbd2 journal replay and ordered-mode
journaling, extents, metadata checksums, htree directories); automount of
partitions; `mkfs.fat`.

**USB** — interrupt-driven xHCI with hot-plug, hubs and bulk streams; HID
with report-descriptor parsing (keyboards, mice, tablets, game pads —
`/dev/input/event0`, `js0`); mass storage over Bulk-Only and **USB
Attached SCSI**; Ethernet (CDC ECM, NCM, RNDIS — phone tethering).

**Networking** — smoltcp-based stack with DHCP, DNS, BSD sockets; NIC
drivers for virtio-net, Intel e1000/e1000e/I219, I225/I226 (igc), Realtek
RTL8111/8168/8125 (r8169); **Intel AX210 Wi-Fi** (also AX211/AX201) with
WPA2-PSK and WPA3-SAE; tools `ip`, `ifconfig`, `ifup`, `ping`, `nslookup`,
`netstat`, `nc`, **`wget` with HTTPS (TLS 1.2/1.3)**, `httpd`, `ntpdate`, `wifi`,
`netcheck` (captive-portal detection); IPv4 and IPv6 (SLAAC, DHCPv6, RDNSS);
**`browse`, a lynx-like text web browser** with CSS layout (flexbox, grid,
tables, floats), JavaScript (QuickJS-ng with a DOM, fetch, storage,
WebSocket), a graphical mode on the framebuffer (`browse -g`: fonts,
images, canvas), forms, cookies and HTTPS that can log into captive-portal
Wi-Fi ([docs/BROWSER.md](docs/BROWSER.md), [docs/CSS.md](docs/CSS.md),
[docs/JAVASCRIPT.md](docs/JAVASCRIPT.md)).

**Userland** — `init` with a service manager (`svc`), `sh` (pipes,
redirection, variables, globbing, command substitution, functions, job
control), `rbox` (≈90 coreutils:
`ls`, `cp`, `grep -E`, `sed`, `find`, `dd`, `sha256sum`, `top`, …), all on
the `rustos-rt` runtime; four virtual consoles (Alt-F1..F4) and
pseudo-terminals; optional logins.

**C programs** — upstream **musl** builds into a sysroot; `tools/rustos-cc`
compiles C programs (static or dynamic, pthreads, `dlopen`) that run
unchanged; BusyBox, curl and the QuickJS-ng JavaScript engine (`qjs`)
ship in the default image ([docs/PORTING.md](docs/PORTING.md)).

Status of every component on real hardware is tracked in
[docs/HARDWARE.md](docs/HARDWARE.md); gaps are listed in
[docs/LIMITATIONS.md](docs/LIMITATIONS.md).

## Quick start

### Prerequisites

```bash
# The pinned nightly toolchain is installed automatically from rust-toolchain.toml.
curl https://sh.rustup.rs -sSf | sh

# QEMU and OVMF (UEFI firmware) for running and testing
sudo apt install qemu-system-x86 ovmf          # Debian/Ubuntu
# Optional, for tests: e2fsprogs (ext2 images), openssl + python3 (HTTPS
# test), gcc (dynamic-linking test programs), curl
```

### Build and run

```bash
git clone https://github.com/RustOS-Dev/RustOS.git
cd RustOS
cargo run                         # build, make a UEFI disk image, boot in QEMU
RUSTOS_SMP=4 cargo run            # with four CPUs
RUSTOS_QEMU_ARGS="-device e1000 -netdev user,id=n0" cargo run   # extra devices
```

`cargo build` compiles the userland (`userland/`) and packs it into the
initramfs embedded in the kernel; `run-qemu-uefi.sh` turns the kernel into
a GPT disk image (`crates/create-image`) and boots it with OVMF.

### Install on a USB stick

```bash
./write_to_drive.sh --drive /dev/sdX
# with Intel Wi-Fi firmware (auto-detected from /lib/firmware otherwise):
./write_to_drive.sh --drive /dev/sdX --ax210-firmware ~/linux-firmware/intel/iwlwifi
```

This creates a UEFI boot partition and a FAT32 storage partition
(`RUSTOS_ROOT`, mounted at `/storage`) for persistent files, network and
Wi-Fi configuration and firmware. Boot the stick in **UEFI mode** with
Secure Boot disabled.

### First steps

```
root@rustos:/# ip addr                       # interfaces get DHCP automatically
root@rustos:/# wget https://example.com/     # HTTPS with certificate checking
root@rustos:/# wifi scan
root@rustos:/# wifi connect "My Network" 'passphrase' --save
root@rustos:/# browse --portal                # log into a hotel / café Wi-Fi
root@rustos:/# browse duckduckgo.com/lite     # text web browsing
root@rustos:/# lsblk; lsusb; lspci; dmesg | tail
root@rustos:/# ls /sys/class/net; cat /proc/cpuinfo
```

## Documentation

| Document | Contents |
|----------|----------|
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | kernel structure, boot, memory, processes, SMP, drivers |
| [docs/SYSCALLS.md](docs/SYSCALLS.md) | system-call ABI and the implemented calls |
| [docs/NETWORKING.md](docs/NETWORKING.md) | network stack, sockets, configuration, tools, HTTPS |
| [docs/WIFI.md](docs/WIFI.md) | Intel Wi-Fi driver, firmware, `wifi` usage |
| [docs/BLUETOOTH.md](docs/BLUETOOTH.md) | Bluetooth keyboards and mice (LE and BR/EDR HID), pairing, `bt` |
| [docs/AUDIO.md](docs/AUDIO.md) | Sound cards (HD Audio, USB audio, virtio-sound), `/dev/dsp`, `play`/`rec`/`mixer` |
| [docs/HARDWARE.md](docs/HARDWARE.md) | supported hardware and validation status |
| [docs/LIMITATIONS.md](docs/LIMITATIONS.md) | known gaps |
| [docs/ROADMAP.md](docs/ROADMAP.md) | the completion plan (M0–M26) and its status |
| [docs/ROADMAP-ROUND4.md](docs/ROADMAP-ROUND4.md) | round 4 plan: more Wi-Fi chips, everyday hardware, GPUs and a desktop |
| [docs/BROWSER.md](docs/BROWSER.md) | the `browse` text web browser and captive-portal login |
| [docs/CSS.md](docs/CSS.md) | the CSS engine and layout (supported CSS, character cells) |
| [docs/JAVASCRIPT.md](docs/JAVASCRIPT.md) | JavaScript in the browser: the `jsd` helper, supported APIs, security model |
| [docs/PORTING.md](docs/PORTING.md) | musl sysroot, `rustos-cc`, ports (BusyBox, curl, QuickJS) |
| [docs/SHELL_COMMANDS.md](docs/SHELL_COMMANDS.md) | shell and command reference |
| [docs/SERVICES.md](docs/SERVICES.md) | init's service manager: service files, restart policies, `svc` and its JSON |
| [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md), [docs/TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md) | contributing, debugging |

## Project structure

```
RustOS/
├── src/                  kernel (arch, mm, sched, process, syscall, vfs, fs,
│                         block, usb, net, drivers, tty, time, ...)
├── crates/
│   ├── wlan/             802.11 frames, WPA2/WPA3 supplicant (host-tested)
│   ├── usb-desc/         USB descriptors, HID report parser, NCM blocks (host-tested)
│   ├── ext4-core/        ext4 checksums, htree hashes, jbd2 journal (host-tested)
│   ├── fat-format/       FAT formatter used by the image tools and mkfs
│   ├── rustos-rt/        userland runtime (syscalls, alloc, fs, net, io)
│   └── create-image/     builds the UEFI disk image
├── userland/             init, sh, rbox, nettools, ldso, dyntest, root/ (etc files)
├── tests/                kernel tests, QEMU profiles, boot scenarios
├── tools/                qemu-console-test.py, run-scenarios.sh
├── run-qemu-uefi.sh      cargo runner (image + QEMU)
└── write_to_drive.sh     USB installer (with firmware provisioning)
```

## Testing

```bash
cargo test                          # kernel tests, each booted in QEMU
tools/run-scenarios.sh              # boot scenarios driving the shell
tools/run-scenarios.sh target/x86_64-rustos/debug/rustos tests/scenarios/https.txt
(cd crates/wlan && cargo test)      # host tests (also usb-desc, fat-format)
cargo fmt --check && cargo clippy -- -D warnings
```

Scenarios cover the shell language and job control, storage (NVMe,
virtio-blk, FAT, ext2/ext4 with journal replay and e2fsck, mkfs), USB
(hub, HID, tablet, storage hot-plug, UAS),
networking with each NIC model, USB Ethernet, HTTPS, dynamic linking and
stress tests; they run with 2 CPUs by default (`RUSTOS_SMP` overrides). CI
runs all of this on every push.

## Contributing

See [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md). Please run the tests,
`cargo fmt` and `cargo clippy` before sending changes, and report results
from real hardware so [docs/HARDWARE.md](docs/HARDWARE.md) stays accurate.

## License

RustOS is free software: you can redistribute it and/or modify it under the
terms of the GNU General Public License as published by the Free Software
Foundation, either **version 2 of the License, or (at your option) any later
version** (SPDX: `GPL-2.0-or-later`). See [LICENSE](LICENSE) for the GPLv2
text.

Code from other projects keeps its own licence:
- `third_party/acpi`: MIT or Apache-2.0.
- `third_party/musl`: MIT.
- Code imported from Linux (see [docs/ROADMAP-ROUND4.md](docs/ROADMAP-ROUND4.md)) keeps its SPDX headers, mostly `GPL-2.0-only`, `GPL-2.0 OR MIT` or `MIT`. A kernel that includes `GPL-2.0-only` files is distributed as a whole under GPLv2.
- The `rsh` shell submodule is a separate program with its own licence.

Firmware files (Intel, MediaTek, AMD, NVIDIA) are not part of this repository.
`write_to_drive.sh` copies them from the host's `linux-firmware`
installation, under their own redistribution terms.
