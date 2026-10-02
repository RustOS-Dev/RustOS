# LinuxKPI: Linux drivers in RustOS

RustOS runs unmodified Linux device drivers by compiling them, along with the
Linux library code they need, into the kernel. They run on top of **LinuxKPI**, a
layer that implements the Linux kernel APIs those drivers call using RustOS
services. FreeBSD uses the same approach. The plan is in
[ROADMAP-ROUND4.md](ROADMAP-ROUND4.md) (M27 onwards).

Status: M27 proof. Linux 6.18.54's `e1000` driver runs in QEMU behind
`--features linux-e1000`. It gets a DHCP lease, pings, and moves 4 MiB over
HTTP both ways (`tests/scenarios/linux/eth-linux-e1000.txt`).

M28: Linux's 802.11 stack (cfg80211, mac80211) and `mac80211_hwsim` run
behind `--features linux-wifi` / `linux-hwsim`. hostapd and wpa_supplicant
(ports) drive it over nl80211, and `wifi` drives wpa_supplicant.
`tests/scenarios/linux/wifi-hwsim.txt` covers:
- WPA2 with DHCP over the air;
- WPA3-SAE with PMF on a two-BSS AP;
- group rekeying, roaming, reconnecting, and a wrong password.

`wifi-hwsim-eap.txt` covers PEAP-MSCHAPv2.

M32: Linux `igb` passes the same steps on QEMU's 82576
(`tests/scenarios/linux/eth-igb.txt`). e1000e, igc, alx, tg3, atlantic,
r8169, r8152, ASIX and ipheth are compiled into release images and take
the devices native drivers do not claim. Their firmware ships in the image.

M33: USB serial adapters and CDC ACM devices are RustOS terminals
(`--features linux-serial`). The `usb-serial` scenario moves data both ways
over QEMU's FTDI adapter, changes the speed with `stty`, and unplugs the
adapter while it is open.

M33: SD cards on SDHCI readers are RustOS disks (`mmcblkN`,
`--features linux-mmc`). Linux's MMC core and sdhci-pci/sdhci-acpi find
the cards. `c/mmc.c` stands in for mmc_block and turns reads and writes into
single MMC requests. The `sdcard` scenario covers ext4 read and write,
remount, and FAT formatting.

M33: Linux's input core and HID core (`hid-generic`, `hid-multitouch`, the
`hid-*` quirk drivers) and `i2c-hid` are in release images
(`--features linux-hid`). `c/input.c` is an input handler in place of evdev: every
Linux input device becomes a RustOS `/dev/input/eventN` with the same
capabilities, keyboards also type on the console, and the console's lock keys
drive the keyboard LEDs. USB keyboards and mice stay on the native usbhid
driver in release images; `--features linux-usbhid` hands them to Linux usbhid
instead, which the `usb-hid-linux` scenario runs (typing on the console,
tablet buttons and absolute axes through `evtest`). I2C-HID touchpads need the
ACPI I2C controller and GPIO drivers, which are not imported yet.

M33: ACPI devices are Linux device objects (`c/acpiscan.c`): the namespace
walk gives each Device its _HID/_CID ids, _UID and _STA. `_CRS` buffers are
converted to ACPICA resources, including GpioInt/GpioIo and I2cSerialBus.
Devices with a _HID become platform devices, except I2C/SPI/UART slaves,
which their controller's driver enumerates. `c/irq.c` is a small interrupt
core on Linux's `irq_desc`: ACPI GSIs and interrupt domains (GPIO
controllers), level/edge/fasteoi flow handlers, and one-shot threaded
handlers. `--features linux-platform` (in release images) adds gpiolib with
its ACPI part, the pin-control core, pinctrl-amd (AMDI0030), DesignWare I2C
(AMDI0010) with regmap, i2c-core-acpi and fixed-rate clocks (`c/clk.c`):
the chain to an I2C-HID touchpad on AMD laptops. QEMU has none of these
devices, so the chain is compiled but untested on hardware. The
`acpi-platform` scenario checks the ACPI scan, and the boot self-test checks
`_CRS` conversion and a GPIO-style domain with a one-shot thread. Intel LPSS
I2C (intel-lpss PCI) and Intel pin controllers are not imported yet.

M35: graphics through Linux's DRM core, KMS helpers and the GEM shmem
helper (`--features linux-drm`), with the bochs driver for QEMU's standard
VGA (`linux-drm-bochs`, in release images): `/dev/dri/card0` with
modesetting, dumb buffers, page flips and their events. GEM objects live
in shmem files that `c/drm.c` implements (an xarray of zeroed pages). The
firmware framebuffer is an aperture owner, as Linux's sysfb devices are:
when a DRM driver takes the display, RustOS stops drawing its console
there (text continues on the serial port; M35.3 moves the console onto
DRM). `drmtest` (rbox) exercises the KMS API; the `drm-bochs` scenario
checks the result with a QEMU screendump. virtio-gpu runs on Linux's
virtio core and PCI transport (`linux-drm-virtio`, in release images),
which take only virtio devices without a native RustOS driver; the
`drm-virtio` scenario is the same test on QEMU's virtio-gpu-pci (2D; no
virgl).

M34: Linux sound (`--features linux-sound`, in release images in place of
the native HDA and USB audio drivers): the ALSA core with OSS emulation,
snd-hda-intel with the codec drivers, and snd-usb-audio on the LinuxKPI
isochronous URBs. `/dev/snd/*` are ALSA's devices, and `/dev/dsp`,
`/dev/dsp1`, ... and `/dev/mixer*` its OSS emulation, so RustOS's `play`,
`rec`, `beep` and `mixer` work unchanged. `/proc/asound/cards` lists the
ALSA cards (`c/sound.c`). The `audio-linux` scenario plays tones on HDA and
USB audio, which the host checks, and records on a second HDA. virtio-sound
keeps its native driver. DSP microphones (SOF, AMD ACP) are not imported.

M33: webcams through Linux's media controller, V4L2 core, videobuf2
(vmalloc buffers) and uvcvideo (`--features linux-video`, in release
images): `/dev/videoN` and `/dev/mediaN`. Buffers map into user space page
by page (`remap_vmalloc_range`). dma-buf is imported for videobuf2 and for
DRM later; dma-buf files carry a pseudo-filesystem dentry, so buffers are
freed on their last close as in Linux. The LinuxKPI USB core now carries
isochronous URBs: each packet is an xHCI isochronous TD, with up to four
URBs queued per endpoint and per-packet lengths and status reported back.
For isochronous IN, the xHCI driver now counts the bytes received.
QEMU has no USB camera, so uvcvideo is untested on hardware. `vgrab`
(rbox) captures frames, and `hwcheck webcam` runs it. User-pointer
buffers (V4L2_MEMORY_USERPTR) are not supported.

M30: the LinuxKPI USB core runs Linux USB drivers on RustOS's xHCI driver
(`--features linux-usb`). Linux's `usbnet` with `cdc_ether` and
`rndis_host` drives QEMU's `usb-net` behind `--features linux-usbnet`
(`tests/scenarios/linux/eth-usb-linux.txt` and `eth-usb-linux-rndis.txt`).
The scenarios cover DHCP, ping, and 4 MiB over HTTP both ways, plus unplugging
the adapter mid-session and plugging it back in. `cdc_ncm` is compiled with
them; QEMU has no NCM device to test it on.

## Building

```sh
cargo build --features linuxkpi      # the LinuxKPI core and its boot self-test
cargo build --features linux-e1000   # plus Linux's e1000 driver
```

You need clang 15 or newer (`RUSTOS_CLANG` overrides the binary). `llvm-ar` and
`llvm-objcopy` come from the toolchain's `llvm-tools` component or from `PATH`
(`RUSTOS_LLVM_AR` overrides `llvm-ar`). A build without `linux-*` features
compiles no C and is unchanged.

In a `linux-e1000` kernel the Linux driver claims the 8254x IDs it supports. The
native e1000 driver still handles the e1000e/I219 families.

## Layout

| Path | What |
|---|---|
| `third_party/linux/` | Linux v6.18.54 files, imported unmodified; `VERSION`, `MANIFEST` (path, SPDX licence, sha256), generated headers (`generated/`) |
| `tools/linux-import.py` | `config` (Kconfig → autoconf.h and generated asm headers), `import GROUP...` (copies sources plus their `#include` closure), `check` (licences, hashes), `undefined ARCHIVE...` (unresolved symbols) |
| `src/linuxkpi/configs/rustos.config` | Kconfig fragment: SMP, `NR_CPUS=64`, `PREEMPT`, `HZ_250`, PCI/MSI, NET, … |
| `src/linuxkpi/groups/*.list` | source files per group: Linux paths, or `kpi:` for the C glue |
| `src/linuxkpi/cflags.txt` | clang flags (PIE, small code model, no SSE/x87, the kernel header search path) |
| `src/linuxkpi/include/` | headers that override Linux's: `asm/percpu.h`, `preempt.h`, `current.h`, `bug.h`, `rustos-prelude.h` |
| `src/linuxkpi/c/` | C glue written against the real Linux headers (below) |
| `src/linuxkpi/*.rs` | the RustOS services the glue calls (`rustos_kpi_*`, declared in `c/kpi.h`) |
| `build/linuxkpi.rs` | the build step behind `build.rs`: parallel, incremental clang builds, one whole-archive static library per group |

Cargo features map to groups in `build/linuxkpi.rs` (`FEATURES`):
- `linuxkpi` builds `proof`, `kpi` and `base`;
- `linux-e1000` adds `e1000`;
- `linux-wifi` adds `crypto`, `netlink`, `cfg80211`, `mac80211`, and `linux-hwsim` adds `hwsim`;
- `linux-usb` adds `usb` (the USB core);
- `linux-usbnet` adds `usbnet` with `linux-phy` (and turns off the native CDC ECM/RNDIS driver);
- `linux-mt7921` adds `mt7921` and `mt7921u`;
- `linux-i2c` adds `i2c`, `linux-phy` adds `phy` (phylib, MDIO, phylink), and `linux-eth` adds `eth` (igb, e1000e, igc, alx, tg3, atlantic, r8169) with both;
- `linux-serial` adds `tty` and `usbserial` (usb-serial, ftdi_sio, cp210x, ch341, pl2303, option, cdc-acm);
- `linux-mmc` adds `mmc` (MMC core, SDHCI hosts, the block bridge);
- `linux-drivers` is the release set (`linux-mt7921`, `linux-eth`, `linux-usbnet`, `linux-serial`, `linux-mmc`).

## How the pieces fit

- **Compiling Linux code.** Linux code is compiled as the RustOS kernel is: a
  position-independent executable with the small code model, loaded at
  `0xffff8000…`. Floating point and SIMD are off.
- **Struct layouts.** Struct layouts always come from the Linux headers. Only
  the C glue touches Linux structs. Rust sees opaque pointers and small
  `#[repr(C)]` records shared through `kpi.h`.
- **Per-CPU data.** `%gs` points at RustOS's per-CPU block, so the Linux per-CPU
  code is the generic variant. Each CPU's offset sits at `%gs:72`, and all
  per-CPU variables live in one section, `kpipcpu`.
- **Preemption.** `preempt_count` is the RustOS per-CPU counter. Linux spinlocks
  therefore disable RustOS preemption, and `in_atomic()` and `in_softirq()`
  work.
- **Tasks.** A `task_struct` shadow is attached lazily to each RustOS thread
  (`Thread.linux_task`). Linux's "set state, check condition, `schedule()`"
  maps onto RustOS's `prepare_block`/`schedule` and `wakeup_pending`, so no
  wakeup is lost. Threads blocked in Linux code are kept alive by the LinuxKPI
  thread registry.
- **Timers and softirqs.** Timers are RustOS one-shot timers. Their callbacks,
  tasklets and NAPI polls run in the `linux-softirq` thread, so Linux timer
  callbacks run with interrupts on, as in Linux. `jiffies` advances at 250 Hz.
- **Workqueues.** Workqueues are pools of kernel threads (`events`,
  `events_highpri`, `events_long`, `events_unbound`, plus driver queues).
- **Memory.**
  - `kmalloc` is size-classed on direct-map frames; larger requests use whole
    pages.
  - `struct page` comes from a `vmemmap` at `0xffff_f000_0000_0000`.
  - `vmalloc` uses `0xffff_f800_0000_0000–0xffff_fe00_0000_0000`.
  - There is no IOMMU, so DMA addresses are physical addresses.
- **PCI.** `pci_register_driver` matches ID tables against the PCI devices that
  native RustOS drivers have not claimed, and probes the matches directly.
  - `request_irq` routes INTx (through ACPI `_PRT`) or MSI to a trampoline that
    runs the handler in hard-IRQ context. Threaded handlers get a kernel
    thread.
- **Networking.** A registered `net_device` becomes a RustOS `NetDevice`
  (`src/linuxkpi/net.rs`).
  - **Transmit:** smoltcp's transmit builds an `sk_buff` and calls
    `ndo_start_xmit`.
  - **Receive:** `napi_gro_receive`/`netif_receive_skb` copy frames into the
    device's receive queue.
  - **Carrier:** carrier changes reach the RustOS link state, which restarts
    DHCP.
  - **Opening:** interfaces are opened once the probe that registered them
    returns.
- **RCU.**
  - `rcu_read_lock()` disables preemption, which is legal because RCU readers may not sleep.
  - A CPU is quiescent when it context-switches, or when a timer tick finds it with preemption enabled (`PerCpu::rcu_qs`).
  - `synchronize_rcu()` waits for every other CPU's count to move. `call_rcu`/`kfree_rcu` callbacks run in the `rcu` kthread, one grace period per batch.
- **Firmware.**
  - `request_firmware()` and its variants read from RustOS's firmware search path (`src/firmware.rs`).
  - Until a minute after boot, a lookup that misses waits for the boot drive to be mounted, because USB sticks enumerate late.
  - `request_firmware_nowait()` runs on `system_long_wq`.
- **Device core and sysfs.**
  - Linux's `drivers/base` runs unmodified, so bus matching, probing, devres, classes and platform devices behave as in Linux.
  - Its sysfs calls go through `c/sysfs.c` into RustOS's `/sys` registry. kobject directories, attributes, groups and links appear in `/sys`, and reads and writes call the Linux `show()`/`store()` methods. Removal waits for running calls.
  - PCI devices are added under `/sys/devices/pci0000:00` and bound through `pci_bus_type`.
  - Uevents are dropped until netlink uevents arrive (M36).
- **Initcalls.** Linux `module_init`/`*_initcall` entries are renamed into
  `kpi_initcall_<level>` sections. They run in level order after the native
  drivers have probed (`src/drivers/mod.rs`).
- **Boot self-test.** Before any Linux driver runs, `kpi_selftest()` checks
  memory, per-CPU data, locks, printf, kthreads, spinlock contention, timers and
  workqueues. LinuxKPI stays off if the test fails.

### Networking and 802.11

- **net_device.** `c/net.c` implements registration as in `net/core/dev.c`,
  without qdiscs.
  - It sends the netdevice notifiers (POST_INIT, REGISTER, UP, GOING_DOWN, DOWN, UNREGISTER).
  - `dev_open`/`dev_close` are tied to RustOS's link up/down (`ip link`, SIOCSIFFLAGS) in both directions.
  - Freeing is deferred to `rtnl_unlock()`.
  - Each netdev is a RustOS interface with the same ifindex and name.
- **Frames.**
  - Received skbs are copied out as Ethernet frames.
  - RustOS frames become skbs and go through `__dev_queue_xmit` (with `ndo_select_queue`).
  - skbs support clones, copies and queues.
- **Netlink.** User sockets are RustOS's (`src/net/netlink.rs`).
  - A Linux kernel socket (`c/netlink.c`) gets their datagrams through `cfg->input`.
  - Its unicasts and multicasts are copied back out.
  - Dumps run to completion when requested.
  - Generic netlink, `lib/nlattr.c` and nl80211 are Linux's own.
- **Namespaces.** A single `init_net`. `pernet_operations` run once, at registration.
- **Crypto.** `c/crypto.c` implements `ccm(aes)`, `gcm(aes)`, `cmac(aes)` and `ctr(aes)` on `lib/crypto` (AES, AES-GCM, ARC4), behind the AEAD, shash and skcipher APIs mac80211 uses.
- **Locks shared with Linux.** Rust state that Linux code reaches with preemption off must use `sync::IrqMutex`, as the netlink and packet sockets do.

### USB

- **Devices.** RustOS enumerates USB devices and selects their
  configuration (`src/usb/mod.rs`).
  - Interfaces no RustOS driver claims are offered to Linux (`src/linuxkpi/usb.rs`).
  - On the first offer, `c/usb.c` builds the `struct usb_device` from the device's
    descriptors (parsed by Linux's `drivers/usb/core/config.c`) and the interfaces
    of the active configuration.
  - Each offered interface is added to the device core on the `usb` bus, which
    matches `usb_driver` ID tables and probes, as `drivers/usb/core/driver.c` does.
  - `usb_driver_claim_interface` (CDC data interfaces) works on interfaces RustOS
    has not offered yet.
- **URBs.** Linux's `drivers/usb/core/urb.c` (submission checks, anchors,
  kill/poison) is used as is. `usb_hcd_submit_urb`/`usb_hcd_unlink_urb` hand URBs
  to RustOS.
  - RustOS runs each endpoint's transfers in order on a worker thread, through
    bounce buffers, so there are no scatter-gather lists.
  - Each URB is given back on that thread, with bottom halves disabled, as
    Linux's HCD giveback does.
  - Unlinking stops the endpoint (`Xhci::wait_abortable`) and completes the URB
    with the unlink status.
  - Isochronous URBs are not supported yet.
- **Synchronous calls.** `usb_control_msg` goes straight to RustOS's control
  transfer; `usb_bulk_msg` waits on a URB.
- **Settings.** `usb_set_interface` re-enables endpoints on the controller (drop
  and add in one Configure Endpoint command).
- **Not supported.** `usb_reset_device` (logged, then success), runtime PM
  (`CONFIG_PM` is off).
- **Unplug.** The interfaces are removed from the device core, so drivers
  disconnect; their URBs fail with `-ESHUTDOWN`.
- **Group lists.** Besides file paths, a group's `.list` can hold
  `module: NAME`, which sets KBUILD_MODNAME (driver and log names) for the
  files that follow. It can also hold `cflags: ...` for extra flags, like a
  Makefile's `ccflags-y`; `@LINUX@` is the imported tree. Files in a group
  are linked in list order, which is also their initcall order within a
  level, so keep Linux's link order (the MDIO bus before phylib, for one).
- **Features without a RustOS counterpart.** XDP (no BPF), tc flow
  offload, MSI-X (one vector per device; drivers fall back to MSI), PCI VPD
  and the ethtool netlink extras are stubbed in `c/netstubs.c` and
  `c/pci.c`. They report "not supported", so drivers take their plain paths.
- **tty drivers.** Each registered tty device (`tty_register_device`) is a
  RustOS terminal (`src/tty.rs`, `Sink::Driver`) with RustOS's line
  discipline, starting out raw. Opening it installs and opens the Linux tty.
  Output goes to `ops->write` (waiting on `write_room`), and `TCSETS` reaches
  `ops->set_termios`. Linux's `tty_port.c`, `tty_buffer.c` and
  `tty_baudrate.c` are used as is. The port's client operations deliver
  flip-buffer input to the terminal (`c/tty.c`, `src/linuxkpi/tty.rs`).
  Unplugging hangs the terminal up: readers get EOF and the node goes away.
- **Transmit flow control.** A Linux netdev whose queues are all stopped
  reports itself not ready (`NetDevice::tx_ready`). The RustOS stack then holds
  packets instead of dropping them, and `netif_wake_queue` kicks it.

## Debugging

- **Log output.** Linux messages appear in the kernel log as `[linux] …`.
  `linux.debug` in `kernel.conf` also shows `KERN_DEBUG` messages, including the
  self-test stages.
- **`WARN_ON` and `BUG`.** `WARN_ON` prints a backtrace and continues. `BUG`
  and `panic()` stop the kernel with the message.
- **Undefined symbols.** `tools/linux-import.py undefined <archive>` lists what
  a group still needs from the glue. It reads the group archives in
  `target/x86_64-rustos/*/build/rustos-*/out/linuxkpi/`.

## Adding a driver

1. Import it.
   - `tools/linux-import.py --linux <checkout at v6.18.54> import <group>`
     copies the sources and their headers.
   - Add the group's `.list` and a Cargo feature (in `Cargo.toml` and in
     `FEATURES` in `build/linuxkpi.rs`).
   - Enable its Kconfig symbols in `configs/rustos.config` and rerun
     `tools/linux-import.py config`.
2. Build, then implement what the link step reports as undefined. Prefer
   importing Linux's own implementation, from `lib/` or a subsystem, over
   rewriting it.
3. Add a scenario under `tests/scenarios/linux/` and a CI step.

## Licensing

RustOS is GPL-2.0-or-later.
- **Linux files:** keep their SPDX tags. `MANIFEST` records each file's licence,
  and `tools/linux-import.py check` (run in CI) rejects untagged or modified
  files. Untagged Linux files count as GPL-2.0-only. A kernel built with
  `linux-*` features is therefore distributable under GPL-2.0.
- **Firmware:** never committed.
