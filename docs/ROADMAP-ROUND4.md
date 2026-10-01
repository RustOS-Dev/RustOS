# Round 4 plan: Wi-Fi beyond Intel, everyday hardware, GPUs and the road to a desktop

Round 4 goals, in priority order:

1. **Wi-Fi on the machines people actually have.** First the MediaTek
   MT7921K (RZ608), then the cheap and common chips around it. This
   includes a USB adapter that gives any machine Wi-Fi.
2. **The easy, high-coverage devices** that keep a laptop or desktop from
   being usable. These are USB Ethernet adapters, the common desktop NICs,
   I2C touchpads, SD card readers, USB serial adapters and webcams.
3. **Graphics that a desktop environment can be ported onto.**
   - First a Linux-compatible DRM/KMS interface on the firmware
     framebuffer, so a software-rendered Wayland desktop runs on *any*
     GPU.
   - Then native AMD and NVIDIA drivers, built from Linux's C drivers
     through a compatibility layer, and Mesa for acceleration.

Each milestone below says what it needs, what it reuses, how it gets
tested, and roughly how big it is. Sizes are relative to work already done.
The AX210 driver (about 4,400 lines over `crates/wlan`) is the yardstick for
"one Wi-Fi driver".

## Principles for this round

- **No common Wi-Fi hardware interface exists.** Every chip family needs its own
  driver and its own vendor firmware. "Generic" Wi-Fi in practice means a
  shared 802.11 layer (`crates/wlan`, already chip-independent) plus a thin
  per-chip hardware layer. M27 builds that split.
- **Rust for networking and Wi-Fi, Linux C for GPUs.**
  - Wi-Fi and Ethernet drivers are a few thousand lines each and stay native Rust. They are written from the Linux drivers and the datasheets.
  - GPU drivers are hundreds of thousands of lines (amdgpu alone is the largest driver in Linux). Rewriting them is not realistic. They are compiled from Linux C sources against a Rust implementation of the Linux kernel APIs they use ("LinuxKPI", as FreeBSD does with drm-kmod).
- **C in the kernel, C++ only in userland.** The GPU kernel drivers are C, so the kernel needs a C build (clang via the `cc` crate, `bindgen` for the Rust side), not C++. C++ is needed in userland for Mesa's shader compilers (ACO), HarfBuzz, LLVM and Qt. It comes from a C++ runtime in the musl sysroot.
- **Licences.**
  - RustOS is GPL-3.0. Linux code under GPL-2.0-*only* cannot be linked into it.
  - MIT, ISC, BSD, "GPL-2.0 OR MIT" and GPL-2.0-or-later code can.
  - amdgpu, nouveau and most of the DRM core are MIT or dual-licensed. Every imported file is checked, and the GPL-2.0-only ones are rewritten in Rust against the same interface.
  - mt76 (the MediaTek Wi-Fi driver we port from) is ISC.
- **Hardware is the test bench.** No emulator has these Wi-Fi chips or real GPUs.
  - Each driver ships with a `hwcheck` section and a `<driver>.debug=1` switch in `kernel.conf` that logs every firmware command and event.
  - QEMU covers what it can: virtio-gpu, the bochs display, USB passthrough of real adapters, and VFIO passthrough of a real GPU. See "Testing" below.
- **Firmware is provisioned by `write_to_drive.sh`.** It copies the needed `linux-firmware` files from the host, as it does for the AX210.

---

## Phase A — Wi-Fi

### M27 — Wi-Fi driver framework (prerequisite for everything in Phase A)

The AX210 driver mixes chip code with logic every chip needs. M27 moves the
shared parts into `src/net/wifi/`. Only the hardware layer stays per driver.

- **Shared layer:**
  - BSS table and scan merging, network selection (band preference, saved networks), roaming and beacon-loss handling.
  - `wlan0` `NetDevice` glue, the `wifi` ioctls and `/proc/net/wireless`.
  - Regulatory channel list, block-ack reorder buffer, replay checks, power-save policy.
  - The `crates/wlan` station machine (auth/assoc, WPA2 4-way, WPA3-SAE).
- **Hardware trait**, roughly what Linux's mac80211 asks of a driver:
  - `start/stop`, `scan(req)`, `set_channel(chandef)`, `add_interface`, `bss_info_changed`.
  - `sta_add/remove`, `set_key`, `ampdu_action`, `tx(frame, info)`, `set_power`.
  - Receive frames and status reports go up through a common channel.
- **Two firmware models.**
  - "Full offload": the firmware scans and does rate control (iwlwifi, mt7921).
  - "Host does more": the driver supplies rate control and timing (MT7601U, Realtek). For these the shared layer adds a small Minstrel-style rate controller and software retransmission bookkeeping.
- **iwlwifi is ported onto the framework first** with no behaviour change. The AX210 hardware-pending status is unchanged, and the host tests of `crates/wlan` must stay green.
- **Size:** small to medium; mostly moving code.
- **Test:** a `mac80211_hwsim`-style **simulated radio** (`src/net/wifi/sim.rs`, debug builds only). It joins an in-kernel software AP built on the test authenticator `crates/wlan` already has, so CI covers scan → WPA2/WPA3 join → DHCP → ping end to end for the first time.

### M28 — MediaTek MT7921 / MT7922 (PCIe)

- **Devices:**
  - MT7921 (`14c3:7961`) and MT7921K/RZ608 (`14c3:0608`): Wi-Fi 6E, the user's laptop.
  - MT7922/RZ616 (`14c3:7922`, `14c3:0616`), common in AMD laptops and the Framework 13/16.
  - Same driver family, small per-chip differences.
- **Firmware** from `linux-firmware/mediatek/`:
  - MT7921: `WIFI_MT7961_patch_mcu_1_2_hdr.bin` and `WIFI_RAM_CODE_MT7961_1.bin`.
  - MT7922: `WIFI_MT7922_patch_mcu_1_1_hdr.bin` and `WIFI_RAM_CODE_MT7922_1.bin`.
  - `write_to_drive.sh` gains `--mediatek-firmware DIR` and auto-detection like `--ax210-firmware`.
- **Driver parts** (from Linux `mt76`, `mt76_connac`, `mt7921`):
  1. **PCIe bring-up**: driver/firmware power-ownership handshake (`LPCTL`), WFDMA reset, TX/RX/MCU rings, interrupts (MSI), ASPM quirk handling.
  2. **Firmware download**: patch semaphore, ROM patch, then the RAM code image via the MCU download protocol; wait for `N9` ready.
  3. **MCU commands** (connac2 "UNI" and legacy commands):
     - setup: `NIC_CAPABILITY`, `CHANNEL_SWITCH`, `SET_RX_FILTER`, `SET_DEVICE_INFO`, `SET_BSS_INFO` (basic, HE, rate, protection), `STA_REC_UPDATE`;
     - scan: `HW_SCAN` and scheduled scan;
     - keys: `KEY` (pairwise, group, BIGTK);
     - power: `SET_PS`, `CLC` regulatory/country code, `SET_RATE_TXPOWER`.
  4. **Data path**: TXD/TXP descriptors, `TXFREE` events, RXD parsing (A-MSDU de-aggregation, hardware-decrypted frames with PN for replay checks), block-ack sessions (firmware aggregates).
  5. **Management frames**: authentication and association frames from `crates/wlan` go out as raw 802.11 frames (`mt76_tx` with the management queue); WPA3-SAE runs on the host as on the AX210.
- **Size:** about one AX210 driver (4,000–5,000 lines) plus host tests of the descriptor and command layouts.
- **Test:**
  - Host tests of every command and descriptor layout against byte dumps made from the Linux structures.
  - `hwcheck wifi` on the user's MT7921K.
  - `mt7921.debug=1` logs commands and events.

### M29 — MT7921AU (USB) and MediaTek Bluetooth

- **MT7921AU** (`0e8d:7961`, `0846:9060`; e.g. Alfa AWUS036AXML, Netgear A8000) is the same chip on USB:
  - it reuses all of M28's MCU, firmware and descriptor code;
  - only the bus layer changes (USB bulk endpoints for MCU, data and events, and the USB vendor requests for register access).
- **This is the "Wi-Fi on any machine" option**: a Wi-Fi 6E USB adapter that works wherever our xHCI stack does, including machines with unsupported internal cards. It is also a way to test M28's firmware logic on any machine.
- **MediaTek Bluetooth (btmtk)**:
  - the MT7921/7922 Bluetooth function is a USB device (`0e8d:...`, `13d3:...`, `0489:...`);
  - it needs `mediatek/BT_RAM_CODE_MT7961_1_2_hdr.bin` (MT7922: `BT_RAM_CODE_MT7922_1_1_hdr.bin`), loaded through vendor HCI commands (`WMT` patch download, then function enable);
  - once loaded, the existing HCI/L2CAP/SMP/HID stack works unchanged.
- **Size:** small to medium: USB bus layer (~1,000 lines) plus btmtk (~400 lines).
- **Test:**
  - QEMU USB passthrough (`-device usb-host,vendorid=0x0e8d,productid=0x7961`) of a real adapter on the developer's Linux machine. This gives fast iteration without reflashing a stick.
  - The `bluetooth` hwcheck section covers the BT side.

### M30 — Older Intel chips (iwlwifi families)

Most Intel laptops since 2015. Same MVM firmware command API as the AX210,
so most of the driver is shared.

| Family | Examples | Transport | Firmware | Work |
|---|---|---|---|---|
| 22000 | AX200 (`8086:2723`), AX201 (CNVi, gen2), AX101 | context info gen2 | `iwlwifi-cc-a0-*.ucode`, `iwlwifi-QuZ-*` | small: older context-info layout, fewer TLVs |
| 9000 | 9260, 9560/9462 (CNVi), 9461 | gen1 (TFD rings, no context info) | `iwlwifi-9260-th-b0-jf-b0-46.ucode`, `iwlwifi-9000-pu-b0-jf-b0-46.ucode` | medium: gen1 transport (`iwl_trans_pcie` load/queue code) |
| 8000 | 8265, 8260 | gen1 | `iwlwifi-8265-36.ucode` | small once 9000 works: older command versions |
| 7000 | 7265D, 3168, 7260 | gen1 | `iwlwifi-7265D-29.ucode` | small: older scan/TX API versions |

- **Size:** medium in total; the gen1 transport is the main new piece.
- **Test:** `hwcheck wifi` on any machine with one of these cards.
  Intel cards are common and cheap on the used market, so buying test cards is practical.

### M31 — Cheap USB Wi-Fi adapters (the generic fallback)

USB adapters mean one driver covers every machine with a USB port. They are
chosen by how common, cheap and simple they are:

| Chip | Typical adapters | Linux driver | Firmware | Notes |
|---|---|---|---|---|
| MT7601U | very cheap 802.11n 150 Mb/s adapters (`148f:7601`) | `mt7601u` (~4k lines C) | `mt7601u.bin` | simplest; host rate control (M27's Minstrel-lite) |
| MT7612U / MT7632U | many 802.11ac adapters (`0e8d:7612`, Netgear A6210) | `mt76x2u` | `mt7662u.bin`, `mt7662u_rom_patch.bin` | shares `mt76` core with M28/M29 |
| AR9271 | Atheros 802.11n adapters (TP-Link TL-WN722N v1, Alfa AWUS036NHA) | `ath9k_htc` | `htc_9271-1.4.0.fw` (open-source firmware) | well documented, open firmware |
| RTL8821CU / RTL8822BU / RTL8811CU | most current cheap Realtek AC adapters | `rtw88` (USB) | `rtw88/rtw8821c_fw.bin`, `rtw8822b_fw.bin` | `rtw88` also covers the PCIe RTL8821CE/8822CE in laptops (M32) |
| RTL8188EU / RTL8192EU / RTL8188FU | the cheapest Realtek N adapters | `rtl8xxxu` | `rtlwifi/rtl8188eufw.bin` etc. | lower priority |

- **Order:** MT7612U (shares `mt76` with M28), then MT7601U, then AR9271, then rtw88 USB.
- **Size:** each small to medium; the `mt76`-family ones are smallest because M28 already holds the shared code.
- **Test:** USB passthrough in QEMU on the developer's machine, then a `hwcheck wifi` run.
  These adapters cost $5–25, so buying one of each is the cheapest test bench in the plan.

### M32 — Realtek PCIe Wi-Fi in laptops

- **`rtw88` PCIe** (RTL8821CE, RTL8822BE, RTL8822CE) reuses M31's `rtw88` core with a PCIe bus layer. These are common in budget laptops.
- **`rtw89`** (RTL8852AE/BE/CE, RTL8851BE) is newer Wi-Fi 6/6E with a different firmware interface: a separate, larger driver, done after rtw88.
- **Size:** rtw88 PCIe small after M31; rtw89 large.

### Later (not in round 4 unless hardware turns up)

- Qualcomm `ath10k` (QCA6174, QCA9377) and `ath11k` (WCN6855, QCA2066). These need the QMI and MHI protocols just to talk to the chip, so they are very large.
- Broadcom `brcmfmac`: full-offload, simple interface, but rare in PCs.
- Intel BE200 Wi-Fi 7.
- WPA-Enterprise (EAP-PEAP/TTLS/TLS in `crates/wlan`, reusing the TLS stack). It is independent of chips and would be worth doing when it is needed.

---

## Phase B — Everyday hardware ("the easy ones")

### M33 — USB and PCIe Ethernet

USB Ethernet adapters, most common first:

- **Realtek RTL8152/8153/8156** (`r8152`; most USB-C and USB 3 Ethernet adapters and docks, 100M/1G/2.5G).
  - Many of these also offer a standard CDC-ECM configuration, which already works. Plugging one in is the first test.
  - The native driver adds the vendor configuration (faster, 2.5G) and adapters without ECM.
- **ASIX AX88179/AX88179A/AX88772** (`ax88179_178a`, `asix`; the other common family). The AX88179A also offers CDC-NCM, which we support.
- **Android/iPhone tethering:**
  - RNDIS/NCM are done, and the first real-phone test is part of this milestone.
  - iPhone tethering needs `ipheth`, a small driver, but it also needs usbmuxd pairing. That is optional.

PCIe network cards, most common first:

- **Intel I210/I211/I350** (`igb`): on a large share of desktop motherboards. Close to e1000e and igc, so small.
- **Killer/Atheros E2200–E2600, AR8161/8171** (`alx`): gaming motherboards and laptops. Small.
- **Broadcom NetXtreme** (`tg3`): Dell/HP/Lenovo desktops and workstations. Medium (larger chip-quirk table).
- **Realtek RTL8126** (5 GbE) added to `r8169`, which already does RTL8125. Small.
- **Aquantia/Marvell AQC107/AQC113** (`atlantic`; 5/10 GbE). Medium, lower priority.

**Size:** each NIC small to medium.

**Test:**
- QEMU has `igb` (`-device igb`, QEMU 8+), so `igb` gets a CI scenario like `eth-e1000`.
- USB adapters work through QEMU USB passthrough.
- The rest use the `hwcheck ethernet` section.

### M34 — Laptop input, card readers, serial, webcams, more Bluetooth

- **I2C HID touchpads and touchscreens:**
  - Most laptops since about 2015 connect the touchpad over I2C, not PS/2. The PS/2 fallback is often absent.
  - **Controller:** DesignWare I2C (AMD `AMDI0010`, Intel LPSS PCI devices), found through ACPI.
  - **Device:** I2C-HID (`PNP0C50`/`ACPI0C50`). The HID descriptor is read over I2C, and reports go through the existing USB HID report parser and evdev.
  - **Interrupts:** ACPI `GpioInt` resources need GPIO controller drivers. AMD `AMDI0030` is simple. Intel pinctrl has a different driver per chipset generation, so start with Tiger Lake and later.
  - **Fallback:** poll the device at 100–200 Hz when its GPIO controller isn't supported yet. Touchpads still work, at slightly higher power use.
  - **ACPI parsing:** the vendored `acpi` crate's GPIO, I2C and SPI resource descriptors (currently `LibUnimplemented`) need real parsers.
  - **Multitouch:** report the multitouch axes in evdev (`ABS_MT_*`) for libinput later (M37).
- **SD card readers:**
  - **Generic SDHCI** (PCI class `08 05`): the standard interface many readers use. Small to medium. QEMU has `sdhci-pci`, so this gets a CI scenario.
  - **Realtek RTS5227/5249/522A/525A** (`rtsx_pci`): the common laptop readers that are *not* SDHCI. Medium.
- **USB serial:**
  - CDC-ACM (generic class: Arduinos, modems, many boards);
  - FTDI FT232/FT2232, Silicon Labs CP210x, WCH CH340/CH341, Prolific PL2303;
  - each is a small vendor driver. All appear as `/dev/ttyUSBn`/`ttyACMn` with termios, reusing the TTY layer.
- **Webcams (UVC):**
  - USB Video Class is a *generic class*: one driver covers almost every webcam.
  - It needs isochronous IN transfers (round 3 only did OUT) and frame reassembly for MJPEG and YUYV.
  - It provides a minimal V4L2 interface (`/dev/video0`: `QUERYCAP`, `ENUM_FMT`, `S_FMT`, `REQBUFS`, `QBUF`/`DQBUF`, `STREAMON`) so that ported apps (and the browser's `getUserMedia` later) can use it.
  - Medium.
- **More Bluetooth firmware loaders:**
  - Realtek `btrtl` (`rtl_bt/rtl8761bu_fw.bin` etc.; most cheap USB BT adapters and Realtek combo cards);
  - Broadcom patchram (`brcm/*.hcd`, optional).
  - Small each. Generic CSR and Broadcom adapters already work without firmware.

---

## Phase C — Graphics and the desktop

The path is ordered so that a **software-rendered desktop works on every
machine first** (M35–M37), on the framebuffer UEFI firmware already sets
up. Then native GPU drivers make it faster and add native resolutions,
multiple monitors and hotplug (M38–M41). A desktop environment needs the
DRM/KMS interface and the userland libraries far more than it needs GPU
acceleration.

### M35 — DRM/KMS on the firmware framebuffer, plus QEMU display drivers

- **`/dev/dri/card0` with the Linux DRM uAPI**, enough for libdrm, Weston and wlroots:
  - device: `VERSION`, `GET_CAP`, `SET_CLIENT_CAP`, `SET_MASTER`/`DROP_MASTER`, `AUTH_MAGIC`;
  - resources: `MODE_GETRESOURCES`/`GETCONNECTOR`/`GETENCODER`/`GETCRTC`/`GETPLANERESOURCES`/`GETPLANE`/`GETPROPERTY`/`OBJ_GETPROPERTIES`;
  - modesetting: `SETCRTC`, `ADDFB2`/`RMFB`, `PAGE_FLIP` with vblank events, `DIRTYFB`, `ATOMIC` (compositors increasingly require atomic);
  - buffers: `MODE_CREATE_DUMB`/`MAP_DUMB`/`DESTROY_DUMB`, GEM handles and `PRIME` export/import as dma-buf fds (M36).
- **`simpledrm` equivalent** on the GOP framebuffer:
  - one connector, one fixed mode, vblank timed by a 60 Hz timer;
  - the text console hands over through `KD_GRAPHICS` (already done) and DRM master.
- **Mode choice:** the boot image picks the largest GOP mode up to the panel's native size; `write_to_drive.sh --resolution WxH` overrides it.
  Without a native driver, the mode can only change at boot.
- **Native drivers for QEMU's displays** (small Rust drivers) so the KMS code is tested in CI with real mode changes and page flips:
  - `bochs` (`-vga std`, `-device bochs-display`; DISPI registers);
  - `virtio-gpu` (2D resources, scanouts, flush). Its 3D (virgl/venus) is a later option to test Mesa in QEMU.
- **Existing users move onto DRM:** the `/dev/fb0` users (`browse -g`) can keep fbdev, which becomes a DRM fbdev emulation.
- **Size:** medium.
- **Test:** CI scenarios run libdrm's `modetest` (ported in M37; until then a small Rust test program) on `-vga std` and virtio-gpu, and check the framebuffer through the QEMU monitor's `screendump`.

### M36 — Kernel features a desktop userland needs

Gaps found in the current syscall list (see `docs/SYSCALLS.md`):

- **Unix-socket fd passing:**
  - `SCM_RIGHTS` and `SCM_CREDENTIALS` on `AF_UNIX`, plus `SO_PEERCRED`.
  - Wayland passes every buffer this way, and D-Bus needs credentials.
- **Shared memory:**
  - `memfd_create` with sealing (`F_ADD_SEALS`, `F_GET_SEALS`);
  - `/dev/shm` (tmpfs) for `shm_open`;
  - `MAP_SHARED` of anonymous and shm memory between processes. Wayland clients share pixel buffers this way.
- **`inotify`:** fontconfig, GLib/GIO, Qt and many apps watch files with it.
- **dma-buf fds and `sync_file` fences:**
  - the kernel object that GPU buffers and their completion fences are passed around as;
  - `poll()` on them;
  - `DMA_BUF_IOCTL_SYNC`.
- **Device discovery for libudev replacements** (libudev-zero or mdev-style):
  - `NETLINK_KOBJECT_UEVENT` hotplug events;
  - `/sys/class/{drm,input,sound,net}` with `uevent`, `dev`, `device/` links and modalias attributes;
  - `/run/udev`-free operation.
- **Seats and VT switching:**
  - `VT_SETMODE` process-controlled switching with `VT_RELDISP`;
  - DRM master drop/acquire on switch;
  - `EVIOCGRAB` (exists), `EVIOCREVOKE`;
  - libseat's "builtin" backend can then run without a daemon, or `seatd` is ported.
- **Kernel FPU sections:** `kernel_fpu_begin/end` with XSAVE/XRSTOR so kernel code can use SSE/AVX. amdgpu's display bandwidth code (DML) is floating point. Fast memcpy/CRC can use it too.
- **Minor syscalls:** `pidfd_open`, `close_range`, `statx` and `copy_file_range` where libraries assume them.
- **ALSA PCM/control uAPI** (`/dev/snd/pcmC0D0p`, `controlC0`) alongside OSS. PipeWire, PulseAudio and SDL speak ALSA, not OSS.
- **Size:** medium in total; each item is small, and the batch is broad.
- **Test:**
  - host-built C test programs per feature in `userland/musltest`, run by a `desktop-kernel` scenario: fd passing, memfd seals, inotify events, uevents on USB hotplug, VT switch;
  - LTP subsets where they port.

### M37 — C++ toolchain and the software-rendered desktop

- **C++ in the sysroot.** Build libstdc++ against musl (a `musl-cross-make` style GCC, or GCC's runtime with the existing specs file), or LLVM libc++/libc++abi/libunwind with clang. Add `tools/rustos-c++`.
- **Build systems.** Meson and CMake cross files (`tools/cross/meson.ini`, `cmake.toolchain`) and pkg-config wrappers so ports build without patches to their build systems.
- **Rust userland ports.** Most of these are C, but some newer tools are Rust (Mesa's NVK compiler, some Wayland apps). Our syscall interface follows Linux and musl, so check whether `x86_64-unknown-linux-musl` static Rust binaries run unmodified. If they do, Rust userland ports need no new target.
- **Ports, in dependency order** (each one a `ports/<name>` recipe):
  1. base libraries: zlib, libffi, expat, libpng, libjpeg-turbo, pcre2;
  2. text and drawing: freetype, harfbuzz (C++), fontconfig, a font package (DejaVu/Noto subset), pixman, cairo;
  3. Wayland: libdrm, wayland, wayland-protocols, libxkbcommon plus xkeyboard-config data;
  4. input and seats: libevdev, mtdev, libudev-zero, libinput, seatd/libseat;
  5. optional: dbus.
- **First desktop:**
  - Weston with the DRM backend and the **pixman (software) renderer**, plus `weston-terminal` and `foot`;
  - runs on any GPU through M35's framebuffer device, with mouse, keyboard and touchpad (M34) through libinput;
  - `browse -g` gets a Wayland window backend (`wl_shm` buffers) so it runs inside the desktop.
- **Software OpenGL:** Mesa `softpipe` (no LLVM) gives EGL/GLES for apps that insist on GL. `llvmpipe` (fast) needs an LLVM port, which is large and deferred until a need appears.
- **Size:** large, mostly porting work. Each port is small, but there are many and they surface kernel gaps.
- **Test:** a `desktop` QEMU scenario:
  - boots Weston on `-vga std`, starts a terminal through the Weston test protocol or `weston-terminal --shell`;
  - types into it with `sendkey`, and checks the screen with `screendump` against an expected region;
  - also runs on the developer's laptops through the GOP framebuffer.

### M38 — LinuxKPI: compiling Linux's GPU drivers into RustOS

The compatibility layer that lets AMD and NVIDIA (and Intel) drivers be built from Linux sources.

- **`crates/linuxkpi`** implements the Linux kernel APIs the DRM drivers use, in Rust with a C ABI. It is built up feature by feature, by compiling the drivers and resolving what is missing:
  - **memory:** `kmalloc`/`kzalloc`/`kvmalloc`/`vmalloc`, `kmem_cache`, page allocation, `dma_alloc_coherent`, `dma_map_page`/`sg`, `ioremap`/`memremap`, `io_mapping`;
  - **locking and lifetime:**
    - `mutex`, `spinlock` (+ `_irqsave`), `rwsem`, `ww_mutex` (used by DRM and TTM);
    - `completion`, `wait_queue`, `atomic_t`, `kref`;
    - `rcu_read_lock` (a simple grace-period implementation is enough here);
  - **time and deferred work:** `jiffies`, `ktime`, `hrtimer`, `timer_list`, `workqueue`, `delayed_work`, `kthread`, `tasklet`;
  - **data structures:** `idr`/`ida`, `xarray`, `rbtree`, `list.h`, `hashtable`, `bitmap`. Most are header-only; `lib/` files that are GPL-2.0-or-later can be used;
  - **bus and platform:**
    - PCI (`pci_dev`, config space, BARs, MSI/MSI-X via the existing PCI code);
    - `request_firmware` (from the firmware directories);
    - I2C/DDC (EDID reads), ACPI (`_DSM`, `ATRM`/`VFCT` to read the VBIOS), backlight;
  - **misc:** `printk` → klog, `module_param` → `kernel.conf`, debugfs as no-ops;
  - **DMA-BUF/fences:** `dma_fence`, `dma_resv`, `sync_file` (shared with M36).
- **C build:**
  - DRM sources are vendored from a pinned Linux LTS release (e.g. 6.12) under `third_party/linux-drm/`, only the needed files, with a generated `autoconf.h`/Kconfig.
  - `build.rs` compiles them with clang through the `cc` crate: `-mcmodel=kernel`-compatible PIE flags, `-mno-red-zone`, `-mgeneral-regs-only` except files that need FP (wrapped in M36's FPU sections), `-fno-strict-aliasing`.
  - `bindgen` generates the Rust side of the boundary.
- **DRM core from Linux** replaces M35's Rust uAPI handling for these drivers, or bridges to it:
  - `drm_drv`, `drm_ioctl`, `drm_gem`, `drm_prime`, `drm_atomic*`, `drm_crtc*`, `drm_edid`, `drm_dp_*`, `drm_hdmi*`, `drm_mm`, `drm_buddy`, `drm_exec`;
  - the GPU scheduler, TTM.
  - **Licence:** a script checks every file's licence on import (see Principles).
- **Proof of the layer:** compile Linux's **virtio-gpu** (or bochs) DRM driver through LinuxKPI and run it in QEMU. CI then tests the compatibility layer end to end before any real GPU is involved.
- **Size:** large. This is the foundation for M39–M41; Intel i915/xe would also become possible on it.

### M39 — AMD GPUs (amdgpu)

- **Targets:**
  - AMD APUs (Ryzen 2000 and later, Vega and RDNA2/3 graphics);
  - Radeon RX 400 and later (Polaris, Vega, RDNA1–4).
  - Older GCN 1.0/1.1 parts (the `radeon` driver) are out of scope.
- **Firmware:** `linux-firmware/amdgpu/` (PSP, SMU, DMCUB, GFX and SDMA microcode, VCN; chosen by chip name). `write_to_drive.sh --gpu-firmware` provisions the files for the detected GPU from the host.
- **Steps:**
  1. **Display first (KMS):**
     - amdgpu base, IP discovery, PSP and SMU firmware loading;
     - Display Core (DC/DCN) for native resolutions, multiple monitors (eDP, DP/USB-C, HDMI), hotplug, eDP backlight.
     - DC's bandwidth calculations need the kernel FPU sections (M36).
     - Result: the M37 desktop at native resolution on every output, with real vblank.
  2. **Memory and command submission:** amdgpu VM, GEM/TTM buffer objects, GFX/compute/SDMA rings, the GPU scheduler, the `AMDGPU_CS` ioctls. This is what Mesa (M41) needs.
  3. **Power:** SMU-driven clock management (the GPU otherwise idles at boot clocks), runtime power gating for laptops, and video decode (VCN) later.
- **Size:** very large in imported C, and medium in new Rust (LinuxKPI gaps, glue).
- **Test:**
  - The user's AMD machine (the MT7921K laptop is likely an AMD Ryzen laptop).
  - Faster iteration with **VFIO passthrough**: on a Linux host with a second GPU, QEMU with `-device vfio-pci,host=...` boots RustOS with the real GPU attached, so a driver change needs no USB stick.

### M40 — NVIDIA GPUs (nouveau)

- **Targets:**
  - **Turing and newer** (RTX 20xx–40xx, GTX 16xx), using NVIDIA's **GSP firmware** (`linux-firmware/nvidia/<chip>/gsp/`). nouveau's GSP-RM path leaves power management and clocks to the GPU's own firmware, so they work.
  - **Kepler to Pascal** (GTX 600–1000) for display. Without GSP, Maxwell 2 and later run at boot clocks: fine for a desktop, slow for 3D.
- **Why not the alternatives:**
  - NVIDIA's own drivers are out: the proprietary module needs NVIDIA's closed glibc userspace.
  - `open-gpu-kernel-modules` is MIT but pairs with the same closed userspace.
  - **nova** (the new Rust NVIDIA driver in Linux) is GPL-2.0, so its code cannot be copied. Its design informs a possible later native-Rust GSP driver.
- **Steps:**
  1. **Display (KMS):** nouveau `nvkm` display engine plus DRM glue through LinuxKPI; on Turing+, GSP boot first (falcon/sec2 loading of the GSP image).
  2. **Channels and memory for NVK:** GEM, VM_BIND, channels (`DRM_NOUVEAU_EXEC`) as NVK uses them.
- **Size:** very large in imported C; medium in new glue, reusing M38/M39's LinuxKPI.
- **Test:** user hardware and VFIO passthrough as for M39.

### M41 — Mesa: hardware-accelerated OpenGL and Vulkan

- **Port Mesa** (meson cross build from M37):
  - **AMD:** `radeonsi` (OpenGL) and `RADV` (Vulkan, using the C++ ACO compiler, so no LLVM is needed for Vulkan).
  - **NVIDIA:** `NVK` (Vulkan; its NAK compiler is Rust, which is where M37's Rust userland check matters), with `zink` providing OpenGL on top of Vulkan.
  - `zink` over RADV also gives OpenGL on AMD without LLVM if porting LLVM is avoided.
- **EGL/GBM** with the DRM platform, so Weston/wlroots use their GL renderer and apps get hardware GL and Vulkan.
- **Size:** large, mostly porting work and surfacing kernel uAPI gaps.
- **Test:** `kmscube`, `eglinfo`, `vulkaninfo`, `vkcube` on hardware; Weston's GL renderer in the `desktop` scenario via virtio-gpu 3D (venus/virgl) in QEMU if that path is added.

### M42 — Porting a desktop environment

Order by what each one requires:

1. **Weston** (M37): the reference compositor and the first target.
2. **wlroots compositors** with their standard tools:
   - compositors: **labwc** (stacking, Openbox-like) or **Sway** (tiling);
   - tools: foot, fuzzel/wofi, waybar, mako, grim, swaybg.
   - Needs: libinput, libseat, xkbcommon, pixman, and EGL from M41, or the pixman renderer without it.
   - This is the first "real desktop".
3. **Xwayland** for X11 applications: libX11/xcb stack, xkbcomp, fonts.
4. **GTK 3/4 applications:**
   - libraries: GLib, Pango, gdk-pixbuf, libepoxy;
   - GTK 4 wants GL; its Cairo fallback works without M41.
   - Gives Firefox (later, very large), file managers and editors.
5. **Qt 6 and KDE Plasma:** C++, D-Bus, a logind-like service, polkit. KDE assumes much of the Linux userland, so it comes last.
6. **GNOME** depends on systemd and is out of scope.

- **Audio:** PipeWire on M36's ALSA uAPI.
- **Networking UI:** a small D-Bus service in front of `wifi`/`dhcp`, or a port of `iwd`'s D-Bus interface shape, so desktop network applets can work.

---

## Dependencies and order

```
M27 ──► M28 ──► M29 ──► M31 (mt76 family first)
  └───► M30            M32 (after M31's rtw88)
M33, M34 independent (M34's I2C-HID before M37 for laptops)
M35 ──► M36 ──► M37 (software desktop on any GPU)
                  └──► M38 ──► M39 (AMD) ──► M41 ──► M42
                         └───► M40 (NVIDIA) ──┘
```

**Suggested sequence:**
1. M27 → M28 → M29: the user's Wi-Fi and Bluetooth, plus a USB adapter for every other machine.
2. In parallel with hardware test rounds on those: M33 and M34.
3. M35 → M36 → M37: a desktop on every machine via the framebuffer.
4. M38 → M39 / M40 → M41 → M42.

## Testing

| What | Where |
|---|---|
| Wi-Fi framework, WPA2/WPA3 join, DHCP over Wi-Fi | CI, through M27's simulated radio and software AP |
| Wi-Fi/BT/NIC/command and descriptor layouts | host tests against byte dumps of the Linux structures |
| USB Wi-Fi/Ethernet/serial/webcam drivers | QEMU USB passthrough on the developer's Linux machine, then `hwcheck` |
| `igb`, SDHCI, bochs/virtio-gpu KMS, LinuxKPI virtio-gpu | CI scenarios (QEMU emulates these) |
| Desktop stack (Weston, terminal, input) | CI `desktop` scenario on `-vga std` |
| MT7921, Intel 7000–AX200, Realtek PCIe, touchpads | user hardware: `hwcheck` + `<driver>.debug=1` logs |
| amdgpu, nouveau, Mesa | user hardware, or VFIO GPU passthrough into QEMU |

`hwcheck` gains sections as drivers land (`wifi` per chip, `touchpad`,
`sdcard`, `webcam`, `display`: modes, EDID, hotplug, page-flip timing,
`gpu`: firmware load, ring tests).

## Main risks

- **Hardware access.** Every non-QEMU driver needs a real device for testing, and every debugging round needs a run of `hwcheck`. Buying the cheap USB adapters (M29, M31, M33) and setting up VFIO passthrough cut that loop the most.
- **LinuxKPI scope.**
  - amdgpu and nouveau use a wide slice of the Linux kernel API. FreeBSD's drm-kmod shows it can be done, and also that it is ongoing work: each Linux update moves the API.
  - Pinning one LTS release and updating deliberately keeps this manageable.
- **Licences.** Any GPL-2.0-only DRM file has to be reimplemented. The licence script on import prevents accidents.
- **Firmware.** Some chips need firmware the user's distribution packages in compressed or split form. `write_to_drive.sh` already handles `.xz`/`.zst` and gains per-family name lists.
- **Desktop userland size.** Each port is small, but there are hundreds, and they will keep surfacing kernel gaps. M36 front-loads the known ones.
