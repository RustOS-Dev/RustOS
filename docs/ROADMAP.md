# RustOS Roadmap — Platform, Drivers, USB, PCIe, Networking & WiFi

This is the working plan for finishing everything RustOS has proposed or only
partly implemented. Tick items off here as they land.

## Status checklist

All milestones are implemented on `claude/comprehensive-implementation-plan-eg7p8k`.
"QEMU" means covered by the boot scenarios in CI; "hardware pending" means
written but not yet run on a real machine (see [HARDWARE.md](HARDWARE.md)).

- [x] **M0** Correctness fixes, CI device profiles, kernel log, driver model, DMA/MMIO helpers, timekeeping
- [x] **M1** ACPI (RSDP/MADT/MCFG/HPET/FADT), LAPIC + IOAPIC, PCIe ECAM, capabilities, MSI/MSI-X, RTC
- [x] **M2** Per-process address spaces, ring 3, `syscall`/`sysret`, preemptive scheduler, fork/execve(argv)/wait, fd table, pipes, TTY, devfs
- [x] **M3** Block layer + buffer cache, NVMe I/O queues R/W, AHCI R/W, virtio-blk, unified GPT/MBR, FAT32 LFN write, ext2 R/W, ext4 R/O
- [x] **M4** Interrupt-driven xHCI, hotplug, USB core + class drivers, hubs, HID keyboard/mouse, hardened BOT mass storage, CDC-ECM/NCM/RNDIS (NCM: hardware pending)
- [x] **M5** In-tree network core (`NetDevice`, smoltcp-backed stack), DHCP, DNS, BSD sockets, network tools
- [x] **M6** Wired NICs: virtio-net, e1000/e1000e (QEMU); I219, igc (I225/I226), r8169 (RTL8111/8168/8125) (hardware pending)
- [x] **M7** Intel AX210 WiFi: firmware provisioning, PCIe gen3 context-info transport, MVM, `crates/wlan` 802.11 core, WPA2-PSK + WPA3-SAE supplicant (host-tested; hardware pending)
- [x] **M8** SMP, ACPI AML shutdown, device shutdown hooks, docs rewrite and hardware matrix
- [x] **M9** Userland and shell: pipes, redirection, variables, globbing, job control, command-audit leftovers, symlinks, permissions, TLS (`wget https`), `/sys`, stress tests, dynamic linking

Deviations from the original plan: SMP uses one global run queue (per-CPU
queues were not needed for correctness); IPv6 has SLAAC but no DHCPv6;
Wi-Fi uses legacy rates (no HT/VHT/HE yet). See
[LIMITATIONS.md](LIMITATIONS.md) for the full list of gaps.

### Round 2

- [x] **M10** Hardware bring-up kit: `hwcheck` checklist runner, `bugreport`, boot-time debug switches (`/storage/etc/kernel.conf`), persistent kernel log
- [x] **M11** Networking for the browser: `url`/`http` crates (cookies, forms, gzip, redirects), TLS 1.2 + 1.3 (rustls), IPv6 DNS (AAAA, DHCPv6, RDNSS), captive-portal detection
- [x] **M12** `browse`: lynx-like text web browser with forms, cookies, HTTPS and captive-portal login
- [x] **M13** Wi-Fi speed: HT/VHT/HE capabilities, 20–160 MHz, MIMO, A-MPDU/A-MSDU, reorder buffer, regulatory channel list, power save
- [x] **M14** Kernel: per-CPU run queues, epoll/eventfd/timerfd/signalfd, page cache and file-backed `mmap`, pseudo-terminals, virtual consoles
- [x] **M15** musl libc sysroot and `rustos-cc`, dynamic-linker TLS and `dlopen`, ported software
- [x] **M16** ext4 journal replay and read/write, USB HID report descriptors, USB Attached SCSI

Round 2 deviations: M13 keeps 4 KiB receive buffers and advertises the
smallest A-MSDU/MPDU limits instead of 12 KiB buffers; receive BA
sessions use ADD_STA (not RX_BAID_ALLOCATION_CONFIG); Wi-Fi power save
and 6 GHz are deferred. The HE/VHT paths follow the Linux iwlwifi layouts
(checked by a boot-time size self-test) but are unverified on hardware.
M14 keeps one poll/epoll wake-up queue (every source notifies it) instead
of per-object queues, and the page cache serves mmap only (read/write stay
coherent with it but go to the filesystem); login is optional (off by
default). M15 ships busybox and curl as optional ports (built on request
with tools/install-port.sh) rather than in the default image.
M16 journals metadata only (ordered mode, one transaction in the log at a
time, checkpointed synchronously at each commit); directory lookups scan
linearly (inserts keep the htree index valid); inline_data, bigalloc,
quotas and meta_bg mount read-only. UAS keeps one command in flight (tags
cycle over the streams) and needs SuperSpeed bulk streams, otherwise the
device's Bulk-Only alternate setting is used. The input devices have no
EVIOCG* ioctls yet.

### Round 3

- [x] **M17** Per-object poll wait queues, page cache for read/write, `KD_GRAPHICS` VTs, busybox/curl/QuickJS in the default image
- [x] **M18** CSS engine (`crates/css`) and box layout (`crates/layout`) for the text browser
- [x] **M19** JavaScript in `browse`: QuickJS-based `jsd` helper with a DOM, events, fetch/XHR, storage
- [x] **M20** Graphical browser `browse -g`: fonts, images, painting, mouse, canvas
- [x] **M21** Wi-Fi remainder: 12 KiB receive buffers, BAID receive BA, power save, 6 GHz with SAE-H2E, 160 MHz, BT coexistence
- [x] **M22** ext4 remainder: multi-transaction journal, data=journal, fast_commit replay, htree lookups, inline_data, bigalloc, meta_bg, quotas, casefold, large_dir, ea_inode
- [x] **M23** USB remainder: UAS queueing and USB 2 UAS, isochronous transfers, evdev ioctls
- [x] **M24** `hwcheck`/`bugreport` coverage for round 3 features
- [x] **M25** Audio: sound core with OSS `/dev/dsp`, Intel HDA, USB Audio Class 1/2, virtio-sound
- [x] **M26** Bluetooth: USB/H4 HCI, AX210 firmware, L2CAP/SMP/GATT, HID over GATT and BR/EDR HID

Round 3 deviations so far:
- **M18**
  - Script changes cause a full restyle and relayout of the page, not the planned incremental one; this is fast enough for text-mode pages.
  - `position: sticky` is laid out as `relative`.
- **M19** — protocol and library:
  - The browser–jsd protocol is JSON lines rather than length-prefixed binary.
  - The JS library is embedded as source; it is not precompiled with `qjsc`.
- **M19** — parsing and loading:
  - The HTML parser is not incremental. Scripts run in document order after parsing, and `document.write` inserts its markup after the running script.
  - Frames are not loaded.
- **M19** — security and runtime:
  - CSP is not enforced.
  - A script past its 10 s budget is stopped and logged; the user is not asked first.
  - `Intl` is a small English-only implementation.
- **Fixes found while testing M18/M19**
  - The scheduler no longer puts a thread to sleep when it is preempted between marking itself blocked and calling `schedule()`. Such a preemption could strand the thread while it held the lock its wait condition takes, for example a pipe's buffer lock.
  - Pipe reads copy in bulk instead of byte by byte.
  - ext4 lookups in indexed directories follow the htree, which was planned for M22. Before this, each lookup scanned the directory.
- **M20** — rendering and formats:
  - Text uses fontdue with kerning but without rustybuzz shaping, so there are no complex scripts or ligatures.
  - WOFF2 web fonts are not loaded (they need Brotli), and neither are WebP or SVG images.
  - GIFs show their first frame.
- **M20** — interface: there are no tabs, text selection, context menu or zoom.
- **M20** — canvas: `<canvas>` is rasterized by a JS 2D context in jsd rather than through an RPC to `crates/paint`.
- **M21** — untested on hardware: no emulator has an iwlwifi device, so everything here is checked only against the Linux structure layouts and host tests. This covers the 12K buffers, the BAID command, power save, 6 GHz scanning and association, and BT coexistence. `hwcheck` covers these items for a hardware run.
- **M21** — 6 GHz scanning:
  - PSC channels are scanned passively.
  - Active probes go only to BSSIDs and short SSIDs learned from neighbor reports.
  - FILS discovery frames and unsolicited broadcast probe responses are not used to shorten the dwell.
- **M21** — power save: power save is device and MAC power save with beacon filtering. uAPSD and TWT are not used.
- **M22** — journal:
  - Transactions accumulate in the log and are checkpointed in batches. There is no background kworker; the 5 s flusher and the pressure points trigger checkpoints instead.
  - Journal barriers are the existing device flushes.
  - Async commit only drops the flush before the commit block.
- **M22** — fast_commit is replayed but never written.
- **M22** — test images: fast-commit images come from `tools/fc-inject.py`, a synthetic writer checked against e2fsck's own replay, not from a Linux crash. `mke2fs -d` with quota writes wrong usage, so the test harness repairs those images first.
- **M22** — features:
  - inline_data converts on first change.
  - ea_inode is read-only for values: there is no setxattr.
  - encrypt and verity mount read-only.
  - Project quotas are accounted but not settable via ioctl: there is no FS_IOC_FSSETXATTR.
- **M23**:
  - Isochronous transfers are one TD per packet, scheduled with SIA and refilled by the driver's streaming thread, not from an IRQ callback. Only OUT (playback) streams are used.
  - The evdev ioctls are tested with `evtest -i`/`-l` rather than a musl C program.
- **M25**:
  - Every card mixes at 48 kHz, 16-bit stereo.
  - HDA:
    - the headphone jack is checked at each playback start (unsolicited responses are ignored);
    - there is no HDMI/ELD support and no quirk table beyond EAPD and GPIO 0.
  - USB audio: playback only, with no explicit feedback. Its volume is applied in software, and the feature unit is set to 0 dB.
  - The browser plays WAV and MP3 (`nanomp3`); there is no Ogg/Vorbis. The QEMU wav backend cannot feed capture, so recording is tested on a card with the `none` backend (silence).
- **M24** — the checks are written: 6 GHz and power save in `wifi`, the ext4/UAS summaries, and the new `audio` and `bluetooth` sections, plus `bugreport` additions. Running them on the AX210 laptop is up to the user; fixes from those reports come later.
- **M26**:
  - The control interface is `/dev/bluetooth`, a command-line device that `bt` writes commands to and reads output from, not a socket family.
  - A2DP is not done (optional in the plan).
  - Legacy PIN pairing is refused.
  - BR/EDR HID and the USB transport, including Intel firmware loading, are written but untested in QEMU. The scenario covers LE over H4 with `tools/fake-hci.py`.
  - H4 on COM3/COM4 is polled; COM2 uses IRQ 3.
- **Found during M26:** smoltcp's SLAAC keeps a stale router-solicitation deadline after its retries go unanswered, so `netd` polled in a busy loop and used a whole CPU on networks without an IPv6 router. `netd` now waits (20 ms, or until kicked) when a poll moves no packets.
- **M19** — tests: a host end-to-end suite (`crates/jsproto/tests/jsd.rs`) plus the `browser-js` and `captive-portal-js` scenarios stand in for the WPT subset.

### Round 4 (planned)

Detailed plan: [ROADMAP-ROUND4.md](ROADMAP-ROUND4.md). RustOS is
GPL-2.0-or-later, so Linux drivers are compiled in through a LinuxKPI layer
(Linux 6.18 LTS) instead of being rewritten.

- [x] **M27** LinuxKPI foundation: import tooling, C build, shim core (tasks, locking, RCU, timers, workqueues, memory/`struct page`, IRQ, DMA, PCI, firmware, cdev, device model); Linux e1000 in QEMU
  - [x] import tooling + C build; [x] Linux e1000 in QEMU (`eth-linux-e1000`); [x] shim remainder (RCU, SRCU, ww_mutex, firmware, cdev/misc/anon fds, device core, sysfs, ACPI); [x] RustOS gaps (PAT/WC, VA allocator, device mmap, IRQ depth, PCI claims); in-kernel self-test instead of host unit tests
- [x] **M28** Linux networking glue (netdev, skb, NAPI, netlink, AF_PACKET, crypto subset), cfg80211 + mac80211, wpa_supplicant/hostapd, `mac80211_hwsim` CI
  - `wifi-hwsim`:
    - WPA2 and DHCP over the air;
    - WPA3-SAE + PMF on a two-BSS AP;
    - group rekey, roam, reconnect, wrong password.
  - `wifi-hwsim-eap`: PEAP-MSCHAPv2.
  - `wifi` drives wpa_supplicant for Linux-driver interfaces.
  - Firmware for supported cards ships in the image (`firmware/stock.list`).
- [ ] **M29** MediaTek MT7921/MT7921K (RZ608)/MT7922 PCIe via Linux mt76
  - [x] mt76 + mt792x + mt7921e compiled and linked into release images (`linux-drivers`); PCI driver registers; firmware ships in the image; ACPI SAR (`mt792x_acpi_sar`) on the AML interpreter
  - [ ] hardware: `hwcheck wifi` on the MT7921K laptop (compiled, untested on hardware)
- [ ] **M30** LinuxKPI USB core, xHCI isochronous IN, Linux usbnet in QEMU, MT7921AU, MediaTek Bluetooth firmware
  - [x] LinuxKPI USB core (device/interface model on the `usb` bus, URBs on per-endpoint workers, unlink/kill, hot-unplug)
  - [x] Linux usbnet + cdc_ether + rndis_host in QEMU (`eth-usb-linux`, `eth-usb-linux-rndis`); cdc_ncm compiled
  - [x] MT7921AU (`mt7921u`, mt76 USB) compiled into release images; untested on hardware
  - [x] MediaTek Bluetooth firmware (btmtk WMT patch download in the native btusb; format host-tested); untested on hardware
  - [ ] isochronous URBs in the LinuxKPI USB core: moved to M34, where Linux snd-usb-audio on QEMU `usb-audio` tests them (the xHCI driver already queues isochronous IN TDs)
- [ ] **M31** Wi-Fi coverage: iwlwifi (all), rtw88, rtw89, mt76 family, mt7601u, ath9k/ath9k_htc, ath10k/11k/12k, brcmfmac
- [ ] **M32** Ethernet coverage: r8152, usbnet family, igb, alx, tg3, atlantic, Linux r8169/e1000e/igc
  - [x] Linux igb in QEMU (`eth-igb`); e1000e, igc, alx, tg3, atlantic, r8169 (+ phylib, Realtek PHY), r8152, ASIX, ipheth compiled into release images; their firmware ships
  - [ ] hardware: `hwcheck ethernet` on each family (compiled, untested on hardware)
- [x] **M33** Laptop platform: I2C/GPIO, I2C-HID touchpads, HID core, MMC/SD, USB serial, UVC webcams, Realtek/Broadcom BT firmware
  - [x] USB serial (usb-serial, ftdi_sio, cp210x, ch341, pl2303, option, cdc-acm) on RustOS terminals (`usb-serial`); `stty`
  - [x] SD/MMC: Linux MMC core + sdhci-pci/sdhci-acpi, cards as `mmcblkN` (`sdcard`)
  - [x] Linux input core + HID core (hid-generic, hid-multitouch, quirk drivers), i2c-hid core; Linux usbhid behind `linux-usbhid` (`usb-hid-linux`)
  - [x] ACPI platform devices, IRQ domains, gpiolib + pinctrl-amd, DesignWare I2C, i2c-core-acpi: I2C-HID touchpads on AMD (compiled, untested on hardware; `acpi-platform`)
  - [ ] Intel LPSS I2C and Intel pin controllers
  - [x] UVC webcams (media controller, V4L2, videobuf2, uvcvideo), isochronous URBs in the LinuxKPI USB core, xHCI isochronous IN; `vgrab`, `hwcheck webcam` (untested on hardware)
  - [x] Realtek (btrtl) and Broadcom (btbcm) Bluetooth firmware loading in btusb; rtl_bt firmware ships (untested on hardware)
- [ ] **M34** Linux sound: ALSA core, HDA codecs, USB audio, SOF/ACP microphones, OSS emulation
  - [x] ALSA core, OSS emulation, snd-hda-intel + codecs, snd-usb-audio on isochronous URBs (`audio-linux`); in release images
  - [ ] SOF/ACP DSP microphones; Linux virtio-sound (native driver kept)
- [x] **M35** DRM core, dma-buf, efidrm/simpledrm, bochs, virtio-gpu
  - [x] DRM core, KMS helpers, GEM shmem, dma-buf/sync_file, bochs; firmware framebuffer handover; `drmtest` (`drm-bochs`)
  - [x] Linux virtio core and virtio-gpu (`drm-virtio`)
  - [x] Console as a DRM client, `/dev/fb0` on the console's buffer, simpledrm on the firmware framebuffer (`drm-simpledrm`); efidrm not used (Linux picks simpledrm with `SYSFB_SIMPLEFB`, and efidrm needs `CONFIG_EFI`)
  - [x] PRIME and sync_file in `drmtest`; `/sys/class/drm` connectors. TTM, the GPU scheduler and `drm/display` are imported with amdgpu (M38): none of the M35 drivers use them
- [x] **M36** Desktop kernel features: SCM_RIGHTS, memfd, inotify, uevents, VT switching, kernel FPU
  - [x] memfd + seals, `/dev/shm`, inotify, pidfds, `close_range`, `copy_file_range`; netlink uevents (Linux device model, native USB and input); `VT_PROCESS` release handshake, `VT_WAITACTIVE`, `K_OFF`, `EVIOCREVOKE`; kernel FPU sections (`desktop-kernel`, `musl`). Unix fd passing and credentials came with M28
- [x] **M37** C++ runtime, Wayland stack, software-rendered Weston desktop
  - [x] `rustos-c++` + libc++ (`cxx`); meson cross files; Weston 14 and its stack (`ports/weston`) on DRM with pixman, libinput, libseat (`desktop`). Software GL (Mesa softpipe) moves to M41 with the rest of Mesa
- [ ] **M38** AMD GPUs (amdgpu): display, rendering, power
  - [x] amdgpu + DC, TTM, GPU scheduler, DP/HDMI helpers, buddy/GPUVM compiled from Linux (`linux-drm-amd`; list generated from the Makefile by `tools/kbuild-group.py`); in release images, opt-in with `linux.enable=amdgpu`; Raphael firmware ships; `hwcheck display`. Compiled, untested on hardware
  - [ ] Bring-up on the Raphael iGPU (display, then rendering and power): needs hardware
- [ ] **M39** NVIDIA GPUs (nouveau, GSP firmware)
  - [x] nouveau compiled from Linux (`linux-drm-nouveau`, list generated from its Kbuild); in release images, opt-in with `linux.enable=nouveau`; GSP-RM 570.144 for Turing through Blackwell GB20x on the storage partition (`firmware/storage.list`, `.links` for linux-firmware's symlinks); `tools/vfio-run.sh`. Compiled, untested on hardware
  - [ ] Bring-up on the RTX 5070 (GB205) through VFIO: needs hardware
- [ ] **M40** Intel GPUs (i915, xe)
- [ ] **M41** Mesa: RADV/radeonsi, NVK, iris/ANV, zink, EGL/GBM
- [ ] **M42** Desktop environments: Weston, labwc/Sway, Xwayland, GTK, Qt/KDE
- [ ] **M43** eDEX desktop: [eDEX-DE](https://github.com/RustOS-Dev/eDEX-DE-RS) (its own Smithay compositor `edex-comp`, shell, greeter, `edex-auth`) as the RustOS desktop; needs M36, M37, M41 and M42's seatd, D-Bus, PipeWire, UPower, login1 subset and `rustos-nmd` ([DESKTOP.md](DESKTOP.md))
  - [x] CPU-time accounting: per-CPU user/nice/system/idle/irq/softirq ticks, per-thread and per-process utime/stime with reaped children, Linux-format `/proc/stat`, `/proc/[pid]/stat` times, `/proc/uptime` idle, `/proc/loadavg` (5 s fixed-point EWMA), real `getrusage`/`times`/`wait4` rusage and CPU-time clocks (`kapitest cputime`)
  - [x] `svc` service manager: init supervises `/etc/svc/NAME.conf` services (exec, tty, user, restart policy with 1-30 s back-off and give-up, `after=` ordering, env), enabled list on `/storage`, `svc list/status/start/stop/restart/enable/disable` with the `--json` contract, control over a FIFO in `/run/svc` (`svc` scenario, `crates/svcconf` host tests)
  - [x] desktop ports, installed under `/usr/local` with `tools/install-port.sh --initramfs` like Weston: `libunwind` (as `libgcc_s.so.1`, for dynamically linked Rust programs), `jetbrains-mono-nerd`, `tor`, `tor-pt` (lyrebird, snowflake-client), `wireguard-tools` (`wg`), `edex-de`
  - [x] `edex` service (eDEX greeter on tty1, disabled until the desktop is installed)
  - [x] large and multithreaded programs: exec streams segments from the file (no whole-file copy in the kernel heap), and a process's address space is freed only after its last thread is gone
  - [x] Linux signal frames (siginfo, ucontext, sigaltstack) and per-thread signal masks and pending sets, `rt_sigtimedwait`; Go programs run (static Go 1.24 binaries: goroutines, async preemption via SIGURG, nil-pointer panics recovered from SIGSEGV; `kapitest sigframes`, `musl-libctest`)
  - [ ] `edex-de` port built against the weston stage and booted (`linux/desktop-edex` scenario); eDEX's shell and greeter render with wgpu and wait for Mesa (M41), edex-comp renders with pixman until then
  - [ ] LinuxKPI `wireguard` group (`drivers/net/wireguard`, `lib/crypto` curve25519/chacha20poly1305/blake2s, `udp_tunnel`) behind `linux-wireguard`, on M28's netdev and genetlink glue
  - [ ] privilege broker for the desktop's system actions (`svc`, Tor helpers, backlight) once sessions run as ordinary users

## Context

RustOS is a ~10.8k-line x86_64 UEFI kernel (bootloader_api 0.11). Today it has:
- **PCI**: legacy CF8/CFC port-IO scan only (`src/pci/mod.rs`), no ECAM, no capabilities, no MSI/MSI-X; `src/block/mod.rs` duplicates its own `pci_read32`/`pci_mmio_bar`.
- **Interrupts**: 8259 PIC only (timer + PS/2 keyboard) in `src/arch/x86_64/interrupts.rs`; no APIC/IOAPIC, no ACPI table parsing beyond `reboot.rs`.
- **USB**: polled xHCI (`src/usb/xhci/`), root-hub ports only, bulk-only mass storage; no hubs, no HID, no hotplug events, no interrupts.
- **Storage**: NVMe/AHCI single-sector **read-only** polled probe in `src/block/mod.rs`; writes only via USB; FAT32 (`src/fs/fat32`) + ramfs VFS.
- **Processes**: ring-0 ELF exec with longjmp exit (`src/process/mod.rs`), no isolation, no scheduler, 6 syscalls (`src/syscall/mod.rs`).
- **Networking/WiFi**: removed in `8e65add` (it lived in the external `tcp-ip` submodule, incl. partial Intel AX210 bring-up: firmware provisioning `4860126`/`5b678d8`, wake sequencing, data queues, association reporting). Syscalls 300–310 and `net/wifi/ping/ifconfig/netstat` commands were deleted.
- Docs (`docs/LIMITATIONS.md`, `ARCHITECTURE.md`) list further planned items (pipes, redirection, env vars, globbing, job control — mostly rsh-side, needing kernel support).

**Goal**: finish everything proposed or partially implemented and bring the kernel to real-hardware usefulness: full PCIe, interrupt-driven drivers, complete USB stack, full storage R/W, a proper process model, an **in-tree** network stack, wired NICs, and **Intel AX210 WiFi with WPA2-PSK + WPA3-SAE**. Priority hardware: AX210 laptop and generic Intel/AMD desktop (I219/I225/RTL8168 Ethernet, AHCI/NVMe, xHCI); QEMU is the CI target.

Decisions taken with the user: in-tree network stack (no submodule); full process-model rework (ring 3, preemptive, POSIX-ish); WiFi = station mode, open/WPA2-PSK/WPA3-SAE (no Enterprise).

## Guiding principles

- Each milestone lands as a series of small PRs on `claude/comprehensive-implementation-plan-eg7p8k` (or follow-up branches), CI green (check/fmt/clippy + new QEMU test job) at every step.
- Pure protocol logic (802.11 frames, supplicant, SAE, GPT/FAT parsing, USB descriptor/HID report parsing) goes in **host-testable `no_std` crates under `crates/`** so it gets `cargo test` coverage with standard test vectors; the kernel only glues hardware to it.
- Prefer mature `no_std` crates over hand-rolling: `acpi` + `aml` (ACPI), `smoltcp` (IP/TCP/UDP/DHCP/DNS — wrapped behind `src/net/stack.rs` so it stays in-tree and swappable), RustCrypto (`sha1`, `sha2`, `hmac`, `pbkdf2`, `aes`, `aes-kw`/`aes-gcm`/`ccm`, `p256`, `cmac`), `xmas-elf` optional.
- Replace every busy-wait/poll loop with timer-based timeouts, then with interrupt + wait-queue completion.

---

## Milestone 0 — Correctness fixes, infrastructure & driver framework

**0a. Fix known-broken code first** (found in audit):
- NVMe/AHCI compute DMA phys as `heap_virt - PHYS_MEM_OFFSET` (`src/block/mod.rs:277,308,471`) — wrong because the heap lives at `0x4444_4444_0000`; `nvme::read_sector` (`:411`) is a stub returning `false`; AHCI puts FIS-receive inside the command table, never IDENTIFYs (size 0), uses enumerate index as port number (`:1117`); every `lsblk` re-resets the NVMe controller.
- `memory::dma_alloc` (`src/arch/x86_64/memory/mod.rs:40`) allocates from heap, never frees (every USB transfer leaks), no contiguity guarantee → replaced by the frame-based DMA allocator below.
- `reboot.rs::find_rsdp` scans legacy BIOS areas and ignores `boot_info.rsdp_addr` (fails on pure-UEFI); reset should try FADT reset register first.
- `_print` falls back to VGA `0xb8000` (`src/drivers/vga.rs:11`), unmapped under bootloader 0.11 → route fallback to serial.
- `main.rs:136` panics when no FAT32 root is found → fall back to ramfs root with a warning.
- PCI `enumerate()` brute-forces all buses *and* recurses bridges → duplicate devices.
- xHCI `bulk_in/out` accept any TRANSFER_EVENT without matching slot/EP; CSW never checked.
- Tests: `integration_storage_test` asserts VFS without init; `basic_boot` prints via VGA; network test deleted — make all existing tests pass under QEMU.
- CI workflows still request `llvm-tools-preview` (renamed in `128b880`); remove stray `crates/create-image/src/main.rs.bak`; `find_ax210` stays for M7; `crates/rustos-rt` declares syscalls (pipe, dup2, waitpid, getcwd, chdir, getdents64, net) the kernel lacks — reconciled in M2/M5.
- ELF exec cannot pass argv (tools get args via `queue_stdin_line` hack) → fixed properly in M2.

1. **CI boots the kernel**: the existing `cargo test` QEMU job gets device profiles: extend `run-qemu-uefi.sh` to append `$RUSTOS_QEMU_ARGS` and per-test configs (`tests/qemu-profiles/{nvme,ahci,xhci,e1000,virtio-net}.args`), with a timeout.
2. **Kernel log**: `src/log.rs` ring buffer + `log` crate facade, levels, `dmesg` command; serial + framebuffer sinks.
3. **Driver model** `src/drivers/mod.rs`: `trait PciDriver { fn matches(&PciDevice)->bool; fn probe(PciDevice)->Result<..> }`, static registry, device tree listing (`lspci`, `lsusb`, `lsdev`).
4. **DMA & MMIO helpers** `src/mm/dma.rs`: physically-contiguous allocation (≤4 GiB option, alignment, zeroed), `DmaBuffer<T>` with phys/virt; `map_mmio(phys,len)` mapping BARs with `NO_CACHE|WRITE_THROUGH` instead of relying on the phys-offset mapping (`memory::PHYS_MEM_OFFSET`). Reuse `memory::GLOBAL_MAPPER`/`GLOBAL_FRAME_ALLOC` (`src/arch/x86_64/memory/`); frame allocator needs deallocation + contiguous runs (current `frame_allocator.rs` is bump-style).
5. **Time**: `src/time.rs` monotonic clock (PIT first, TSC/HPET in M1), `sleep_us/ms`, `Deadline` for timeouts used by all drivers.

## Milestone 1 — Platform: ACPI, APIC, PCIe

1. **ACPI** (`src/arch/x86_64/acpi.rs`, `acpi` crate): RSDP from `boot_info.rsdp_addr`; parse MADT, MCFG, HPET, FADT, DMAR (report only). Fold the ad-hoc parsing in `reboot.rs` into it. Later `aml` crate for `_S5`, `_PRT` (legacy IRQ routing), `_PS0`.
2. **LAPIC + IOAPIC**: mask/disable 8259, LAPIC timer calibrated against HPET/PIT, IOAPIC redirection for ISA IRQs (keyboard IRQ1 via MADT overrides), spurious/error vectors, EOI. Dynamic **vector allocator** and `register_irq(vector, handler)` in `interrupts.rs` (IDT entries generated for 48–239).
3. **PCIe** rewrite of `src/pci/mod.rs`:
   - ECAM via MCFG (4 KiB config space, all segments), CF8/CFC fallback.
   - Recursive bus scan through PCI-PCI bridges, multifunction, header types 0/1.
   - BAR sizing (write-ones), 32/64-bit, IO vs MMIO, prefetchable; enable memory/IO/bus-master in COMMAND.
   - Capability & extended-capability walk (PM, MSI, MSI-X, PCIe, vendor-specific, AER, L1SS).
   - **MSI and MSI-X** allocation (table/PBA mapping, per-vector masking) integrated with the vector allocator; INTx fallback via `_PRT`.
   - Power state D0 transitions, FLR/function reset, ASPM control (AX210 needs L1 tweaks).
   - Delete duplicated PCI code in `src/block/mod.rs` and `src/usb` in favour of `pci::`.
4. **RTC/CMOS** wall clock → `clock_gettime(CLOCK_REALTIME)` and FAT timestamps.

## Milestone 2 — Memory & process model (ring 3, preemptive)

1. **VMM**: per-process `AddressSpace` (new PML4 sharing kernel upper half), user mapping API, page-fault handler supporting demand-zero, COW for fork, guard pages; kernel `vmalloc` region; frame reference counts.
2. **Ring 3**: user code/data segments + TSS `rsp0` in `gdt.rs`; `syscall/sysret` via STAR/LSTAR/SFMASK (keep `int 0x80` for compat); `copy_from_user/copy_to_user` with range validation (replace raw pointer derefs in `src/syscall/mod.rs`).
3. **Scheduler** `src/sched/`: kernel threads + user processes, preemptive round-robin on LAPIC timer, per-thread kernel stacks, wait queues, sleep, `yield`. The existing async executor (`src/task/executor.rs`) runs as a kernel thread; drivers block on wait queues woken from IRQ handlers.
4. **Process lifecycle**: replace the longjmp model in `src/process/mod.rs` with real `fork/clone`, `execve(argv, envp)` (ELF loader keeps its PT_LOAD logic, adds auxv/stack setup), `exit`, `wait4`, `getpid/getppid`, zombies/reaping, minimal signals (SIGKILL/SIGINT/SIGCHLD, Ctrl-C from TTY).
5. **Files/FDs**: per-process fd table over a unified `File` trait (VFS file, pipe, TTY, socket, device); syscalls `pipe2, dup/dup2, lseek, stat/fstat, getdents64, mkdir/unlink/rename, chdir/getcwd, ioctl (TTY), poll/select, brk/mmap/munmap, nanosleep, clock_gettime`. Adopt **Linux x86_64 syscall numbers** (existing 0/1/2/3/59/60 already match).
6. **TTY**: line discipline in kernel (cooked/raw modes), `/dev/tty`, `/dev/null`, `/dev/zero`, `/dev/fb0`, devfs mount.
7. **Userspace**: update `crates/rustos-rt` wrappers + `docs/SYSCALLS.md`; kernel support unblocks rsh (`RustOS-Dev/rsh` submodule) pipes, redirection, env vars, job control from `LIMITATIONS.md` — rsh changes tracked as separate PRs in that repo. Migrate built-in `src/shell/commands.rs` tools progressively to `/bin` programs (`src/bin_commands.rs`).

## Milestone 3 — Storage completion

1. **Block layer** `src/block/`: split `mod.rs` into `nvme.rs`, `ahci.rs`, `virtio_blk.rs`, `partition.rs`, `cache.rs`. Single `BlockDevice` trait (currently in `src/usb/mod.rs`) moved here with multi-sector `read_blocks/write_blocks/flush`, sector size, request queue; LRU buffer cache with write-back + `sync`.
2. **NVMe**: proper admin/IO queue pairs sized from CAP, PRP lists (and SGL optional), multi-namespace, WRITE/FLUSH, MSI-X completion interrupts, shutdown notification on reboot/poweroff.
3. **AHCI**: BIOS/OS handoff, port init + COMRESET, DMA READ/WRITE EXT multi-sector with PRDT, FLUSH CACHE, IDENTIFY, interrupts, hotplug.
4. **virtio-blk** (modern virtio-pci, shared `src/drivers/virtio/` virtqueue code also used by virtio-net).
5. **Partitions**: one GPT+MBR parser (merge `block::parse_gpt_partitions` and `usb::gpt_partitions_for_device_pub`) in a host-testable crate; partitions exposed as block devices (`PartitionBlockDevice` generalized).
6. **Filesystems**: finish FAT32 (LFN create, nested dirs, timestamps, FSInfo, truncate/append, cluster chain freeing); add ext2 read/write + ext4 read-only (FsType already detects them); `mount/umount` for any device; root-FS selection from boot partition GUID.

## Milestone 4 — USB stack completion

1. **xHCI core** (`src/usb/xhci/`): BIOS legacy handoff (USBLEGSUP), scratchpad buffers, correct ring sizing + link TRBs, MSI-X interrupter 0 with event-ring dequeue in IRQ, command/transfer completion via wait queues (replace polling `wait_event`), port status change → hotplug connect/disconnect, USB2 vs USB3 reset paths, 64-byte context support, stop/reset endpoint + stall recovery.
2. **USB core** `src/usb/core/`: device model (config/interface/alt-setting/endpoint), descriptor parsing (host-testable crate), control/bulk/interrupt/isochronous(later) transfer API, class-driver registry matched on class/subclass/protocol or VID:PID.
3. **Hub driver**: USB2 and USB3 hubs, route strings, TT info for LS/FS devices behind HS hubs, per-port power/reset, hub status interrupt endpoint.
4. **HID**: boot-protocol keyboard + mouse, then report-descriptor parser; feeds `src/task/keyboard.rs` (critical for laptops without PS/2 emulation) and a new mouse/input event queue.
5. **Mass storage**: harden BOT (CSW validation, reset recovery, REQUEST SENSE, multi-LUN), expose via M3 block layer; UAS later.
6. **USB networking**: CDC-ECM/NCM and RNDIS → `NetDevice` (useful phone tethering + extra test path).

## Milestone 5 — Network core (in-tree)

1. `src/net/`: `NetDevice` trait (`mac`, `mtu`, `link_state`, `transmit(PacketBuf)`, RX delivery via IRQ-driven queue), interface registry (`eth0`, `wlan0`, `lo`), loopback device, DMA-friendly packet buffers.
2. `src/net/stack.rs`: smoltcp `Interface` per device driven by a network kernel thread; Ethernet/ARP, IPv4/ICMP/UDP/TCP, **DHCPv4 client**, **DNS** resolver (`/etc/resolv.conf`), IPv6 + SLAAC + ICMPv6/NDP (phase 2).
3. **Sockets**: BSD socket syscalls with Linux numbers (`socket, bind, connect, listen, accept4, sendto/recvfrom, sendmsg/recvmsg, shutdown, getsockopt/setsockopt, getsockname/getpeername`), AF_INET/AF_INET6 stream/dgram + raw ICMP, sockets as `File` in fd table, blocking + non-blocking + `poll`.
4. Userspace tools (restoring what `8e65add` removed, as `/bin` programs on rustos-rt): `ip`/`ifconfig`, `ping`, `dhcp`, `nslookup`, `netstat`, `nc`, `wget`/`http`, plus `wifi` (M7). Network config file `/etc/network.conf` (auto DHCP on link-up).

## Milestone 6 — Wired Ethernet drivers

Each in `src/drivers/net/`, MSI/MSI-X, descriptor rings in DMA memory, link-change interrupts, checksum offload where simple:
- **virtio-net** (QEMU CI primary).
- **e1000 / e1000e**: 82540EM (QEMU `-device e1000`), 82574L (`e1000e`), and **I217/I218/I219** (desktop/laptop PCH LAN — needs ULP/ME handling, PHY via MDIO).
- **igc**: I225/I226 2.5GbE.
- **r8169**: Realtek RTL8111/8168/8125-family (most common AMD/Intel desktop NICs), PHY init tables per chip revision.

## Milestone 7 — Intel AX210 WiFi (station, open/WPA2/WPA3)

Resurrect the design intent of the removed tcp-ip AX210 work (see commits `4860126`, `86b78b1`, `f25e044`, `c9b895a`, `f3acfbd`, `952a3e7`, `458318f`) as an in-tree driver `src/drivers/wifi/iwlwifi/`. The old driver never had a working data path (`DriverError::Unsupported`) or a real 4-way handshake, so this is a from-scratch implementation reusing only its interfaces: PCI IDs 8086:2725/51F0/54F0/7F70 (`pci::find_ax210`, `src/pci/mod.rs:159`), ≥3 MiB BAR0 mapping, the error taxonomy (firmware missing/IML/ALIVE timeout, MAC clock timeout, command timeout, auth/assoc failures) and the `/lib/firmware` VFS loader with -72 → -71 fallback (recover from `git show ecfee0e^:src/net.rs`).
1. **Firmware provisioning**: restore `write_to_drive.sh --ax210-firmware` / `RUSTOS_AX210_FIRMWARE` + auto-detect (`4860126`, `5b678d8`; searches `/lib/firmware`, `/usr/lib/firmware`, `/lib/firmware/updates`, `/usr/lib/linux-firmware`) to copy `iwlwifi-ty-a0-gf-a0-*.ucode` + `.pnvm` into `/lib/firmware` on the RUSTOS_ROOT partition; optional `create-image --firmware-dir`. Kernel parses TLV (IML, runtime sections, capabilities, API flags, debug TLVs).
2. **PCIe gen2 transport**: NIC wake / MAC access request, APM/power, context-info (gen3 "context info v2" for AX210) firmware load, MSI-X with RX/non-RX causes, TFH TX queues (TFD/TB), RX queues (RSS multi-queue, RB allocation), command queue with sync/async host commands, ALIVE handling, error-dump on SW error.
3. **MVM layer**: NVM/OTP read, PNVM load, PHY DB & init-complete, MCC/regulatory update (LAR), SAR/PPAG defaults, PHY context, MAC context add/modify, binding, time events, UMAC scan (active/passive, 2.4/5/6 GHz channels), add/modify station, TX command w/ firmware rate scaling (TLC config), key install (CCMP-128/GCMP-256 in HW, BIGTK/IGTK for PMF), BA sessions (optional, later), power-save disabled initially.
4. **802.11 / WLAN core** as host-testable crate `crates/wlan/`: frame (de)serialisation, IE parsing (SSID, rates, RSN, HT/VHT/HE caps, RSNXE), BSS table, auth/assoc/deauth state machine, 802.11↔802.3 (LLC/SNAP) data conversion, beacon-loss detection, reconnect.
5. **Supplicant** in `crates/wlan/src/rsn/`: WPA2-PSK (PBKDF2-SHA1 PMK, EAPOL-Key 4-way + group handshake, PTK/GTK/IGTK derivation, MIC with HMAC-SHA1/AES-CMAC, AES key wrap), WPA3-SAE (P-256, hash-to-element + hunting-and-pecking fallback, anti-clogging, PMKSA caching), PMF/802.11w mandatory for SAE; transition-mode handling. Tested against IEEE 802.11 Annex J and hostapd test vectors.
6. **Integration**: `wlan0` as a `NetDevice` → DHCP via M5; `wifi scan|connect <ssid> [--psk]|status|disconnect` tool + `/etc/wifi.conf` autoconnect; ioctl/netlink-lite syscall interface for WiFi control.
7. Keep `NetDevice`/WLAN core driver-agnostic so other iwlwifi-mvm parts (AX200/AX201/AX211, 9260) are mostly table entries later.

## Milestone 8 — SMP, power & polish

- SMP bring-up (INIT-SIPI from MADT), per-CPU data (GS base), per-CPU run queues, IPI TLB shootdown, lock audit (`spin` → IRQ-safe spinlocks).
- ACPI `_S5` shutdown via AML, `_PTS`, clean device shutdown hooks (NVMe, xHCI, NICs).
- PS/2 mouse; framebuffer console speed-ups; optional Intel HDA audio and AMD/Intel GPU are **out of scope** (stretch list only).
- Docs: rewrite `docs/ARCHITECTURE.md`, `LIMITATIONS.md`, `SYSCALLS.md`, add `docs/NETWORKING.md`, `docs/WIFI.md`, `docs/HARDWARE.md` (tested hardware matrix).

## Milestone 9 — Userland & shell items from the roadmap/audit

Kernel pieces from M2 unblock these; rsh-side work goes to `RustOS-Dev/rsh`, built-ins in `src/shell/commands.rs`:
- Pipes, `>`/`>>`/`<` redirection, env vars/`$VAR`, globbing, command substitution, job control (`&`, `jobs`, `fg`, `bg`), `source`, `cd -`.
- Command audit leftovers: real `ls -l` timestamps/sizes/`total`, `ps` from the real process table, `grep -E/-F` + stdin, `cat` stdin, `cp -p/-i/-f`, `rm -i/-I`, `mv -i`, `mkdir -m`, `mount -o`, `umount` busy check.
- VFS: symlinks, permissions/mode bits, inode-based handles with streaming reads (FAT32 currently loads whole files).
- Stress tests the Phase 5 docs marked blocked: >100-level directories, >100 MB files with checksums, repeated hotplug mount cycles, memory pressure.
- Network extras listed in the removed `NETWORK_INTEGRATION.md`: async socket I/O, IPv6, TLS (`embedded-tls`/rustls-no_std) for `wget https`, static-IP CLI.
- Dynamic linking (ld.so-style loader) as the last item.

## Out of scope (stretch list)

802.11 AP mode, self-hosting, system suspend/resume. (GPUs, desktops, more Wi-Fi chips, USB Wi-Fi adapters and WPA-Enterprise (via wpa_supplicant) moved into round 4; HDA audio and Bluetooth were done in round 3.)

## Dependency order

M0 → M1 → {M2, M3, M4 in parallel} → M5 (kernel stack can start after M1; socket syscalls need M2) → M6 → M7 → M8 → M9 (shell items can trickle in as soon as the matching M2 syscalls land). M7's `crates/wlan` host-side work can start any time after M0.

## First execution step (done)

Commit this plan as `docs/ROADMAP.md` (with a per-milestone checkbox list), remove the stale network/socket sections from `docs/SYSCALLS.md` and `docs/LIMITATIONS.md` (pointing to the roadmap), link it from `README.md`, push to `claude/comprehensive-implementation-plan-eg7p8k`, and start M0a on the same branch.

## Verification

- **Every PR**: `cargo check`, `cargo fmt --check`, `cargo clippy`, host `cargo test` for `crates/*` (wlan, partition, usb-desc, fat parsers).
- **QEMU integration tests** (new CI job, `tests/*.rs` with per-test QEMU profiles):
  - Platform: APIC timer ticks, MSI-X delivered (virtio device), ECAM config reads match CF8.
  - Storage: `-device nvme`, `-device ahci -device ide-hd`, `virtio-blk`: write/read-back/flush, FAT32 + ext2 file round trips across reboot image.
  - USB: `-device qemu-xhci -device usb-hub -device usb-kbd -device usb-mouse -device usb-storage`: enumeration through hub, HID key injection via QEMU monitor `sendkey`, hotplug via `device_add/device_del`.
  - Process: ring-3 program faulting on kernel address is killed, fork/exec/wait/pipe tests, preemption test (two busy loops both progress).
  - Network: `-netdev user` with `virtio-net`, `e1000`, `e1000e`, `rtl8139`/(`usb-net` for CDC): DHCP lease 10.0.2.15, `ping 10.0.2.2`, DNS via 10.0.2.3, TCP connect to a host `hostfwd`/guestfwd echo server, UDP echo.
- **WiFi** (no emulator exists): host tests of `crates/wlan` with Annex J/hostapd vectors (PMK/PTK, EAPOL MIC, SAE commit/confirm); on-hardware checklist on the AX210 laptop — firmware ALIVE, scan list, open AP associate, WPA2-PSK associate + DHCP + ping, WPA3-SAE associate, rekey survival, disconnect/reconnect — logged via `dmesg` to the storage partition.
- **Real hardware matrix** (desktop): I219/I225/RTL8168 link + DHCP, NVMe/AHCI R/W, xHCI keyboard through hub, recorded in `docs/HARDWARE.md`.
