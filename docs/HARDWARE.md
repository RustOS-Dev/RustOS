# Hardware support

Status legend: **CI** = exercised by the QEMU boot scenarios on every
change; **written** = implemented from datasheets / reference drivers but
not yet run on real hardware; **n/a** = not supported.

## Platform

| Area | Support | Status |
|------|---------|--------|
| Boot | UEFI (bootloader 0.11), GOP framebuffer console, serial COM1 | CI (OVMF) |
| CPU | x86-64; SMP: every MADT processor started with INIT-SIPI, per-CPU GDT/TSS/GS block/LAPIC timer, global run queue, IPI TLB shootdown and reschedule | CI (2–4 vCPUs) |
| Interrupts | Local APIC / x2APIC, I/O APIC (MADT overrides), MSI and MSI-X, legacy INTx via ACPI `_PRT` | CI |
| ACPI | RSDP from the bootloader; MADT, MCFG, HPET, FADT; AML interpreter for `_S5`/`_PTS` (power-off) and `_PRT`; FADT reset register | CI |
| Timers | TSC (calibrated by CPUID 0x15, HPET, PM timer or PIT), LAPIC periodic tick, CMOS RTC wall clock | CI |
| PCI | PCIe ECAM (all segments) with CF8/CFC fallback, bridges, 64-bit BARs, capability walk, D0 power-up | CI |
| Power | ACPI S5 power-off, reboot via FADT reset register / 0xCF9 / 8042; drivers quiesce (NVMe shutdown notification, NIC resets, USB, cache flush) | CI (QEMU) |

## Storage

| Controller | Features | Status |
|------------|----------|--------|
| NVMe | admin + I/O queues, PRP lists, multiple namespaces, MSI-X, flush, shutdown notification | CI |
| AHCI (SATA) | BIOS/OS handoff, COMRESET, IDENTIFY, READ/WRITE DMA EXT, FLUSH, NCQ-less; ATAPI detected but unsupported | CI |
| virtio-blk | modern and transitional devices | CI |
| USB mass storage | Bulk-only transport, SCSI READ/WRITE(10/16), sense recovery, multi-LUN, hot-plug | CI |
| SD/MMC on SDHCI readers (PCI, incl. O2 Micro, GL975x, Arasan; ACPI) | Linux MMC core + `sdhci` via LinuxKPI, cards as `mmcblkN`, in release images | CI (`sdcard` on QEMU `sdhci-pci` + `sd-card`) |

Partitions: GPT and MBR. Filesystems: FAT12/16/32 with long names
(read/write), ext2/ext3/ext4 (read/write, journaled; checked with
`e2fsck` in CI). Block
devices are cached (write-back buffer cache, `sync`).

## USB

| Component | Status |
|-----------|--------|
| xHCI host controller (BIOS handoff, MSI-X, hot-plug, USB 2/3 ports, stall recovery) | CI (`qemu-xhci`) |
| Hubs (USB 2 and 3, TT for low/full-speed) | CI (`usb-hub`) |
| HID keyboard and mouse (report protocol, boot fallback) | CI (`usb-kbd`, `usb-mouse`) |
| HID tablet (absolute pointer) | CI (`usb-tablet`) |
| HID game pads, NKRO keyboards, media keys | host tests only |
| Mass storage (Bulk-Only) | CI (`usb-storage`) |
| USB Attached SCSI (bulk streams, 8 commands queued; USB 2 without streams) | CI (`usb-uas` on SuperSpeed and high-speed ports) |
| USB Audio Class 1/2 playback (isochronous) | CI (`usb-audio`) |
| Bluetooth controllers (class E0/01/01), Intel AX200/AX210/AX211 and MediaTek MT7921/MT7922/MT7925 firmware | written ([BLUETOOTH.md](BLUETOOTH.md)); MediaTek patch format host-tested |
| Ethernet: CDC ECM, RNDIS | CI (`usb-net`) |
| Ethernet: CDC NCM | written (NTB encoding host-tested) |
| Serial adapters: Linux `usb-serial` with `ftdi_sio`, `cp210x`, `ch341`, `pl2303`, `option` (3G/LTE modems), and `cdc_acm` (`/dev/ttyUSB*`, `/dev/ttyACM*`), in release images | CI (`usb-serial`: FTDI on QEMU `usb-serial`, data both ways, `stty`, unplug); others compiled |
| Linux USB drivers through the LinuxKPI USB core (`linux-usb`) | CI (`usb-net` with Linux usbnet) |
| Ethernet: Linux `usbnet` with `cdc_ether`, `rndis_host`, `cdc_ncm` (`--features linux-usbnet`, in release images instead of the native CDC ECM/RNDIS driver) | CI (`eth-usb-linux`, `eth-usb-linux-rndis`); NCM compiled |

## Network

| Device | Status |
|--------|--------|
| virtio-net | CI |
| Intel 82540EM (`e1000`), 82574L (`e1000e`) | CI |
| Intel I217/I218/I219 (PCH LAN) | written |
| Intel I225/I226 (`igc`) | written |
| Realtek RTL8111/8168/8125 (`r8169`) | written |
| Intel AX210 Wi-Fi, AX211/AX201 CNVi (incl. 6 GHz, power save) | written ([WIFI.md](WIFI.md)) |
| MediaTek MT7921/MT7921K (RZ608)/MT7920/MT7922 PCIe Wi-Fi: Linux `mt7921e` (mt76) via LinuxKPI, in release images (`linux-drivers`) | compiled, untested on hardware: `hwcheck wifi` results wanted |
| MediaTek MT7921AU USB Wi-Fi (0e8d:7961 and OEM IDs): Linux `mt7921u` on the LinuxKPI USB core, in release images | compiled, untested on hardware: `hwcheck wifi` results wanted |
| Linux 802.11 stack (cfg80211/mac80211) with wpa_supplicant/hostapd: WPA2, WPA3-SAE, PMF, PEAP | CI (`mac80211_hwsim`: `wifi-hwsim`, `wifi-hwsim-eap`) |
| Linux `e1000` (82540EM) via LinuxKPI | CI (`eth-linux-e1000`, `--features linux-e1000`) |
| Intel 82575/82576/I210/I211/I350: Linux `igb` via LinuxKPI, in release images | CI (`eth-igb` on QEMU `-device igb`) |
| Intel e1000e / I225-I226 / Realtek RTL8101-8127 devices the native drivers do not claim: Linux `e1000e`, `igc`, `r8169` (with phylib and the Realtek PHY driver), in release images | compiled, untested on hardware |
| Qualcomm Atheros / Killer E2200-E2600, AR8161/8171 (`alx`); Broadcom NetXtreme (`tg3`); Aquantia AQC107/108/113 (`atlantic`), in release images | compiled, untested on hardware |
| USB Ethernet: Realtek RTL8152/8153/8156/8157 (`r8152`), ASIX AX88772/AX88178/AX88179 (`asix`, `ax88179_178a`), iPhone tethering (`ipheth`), in release images | compiled, untested on hardware |

## Input and display

| Device | Status |
|--------|--------|
| PS/2 keyboard and mouse (i8042) | CI (keyboard) |
| USB HID keyboard/mouse | CI |
| GOP framebuffer text console (8x16 font, ANSI escapes, scrollback) | CI |
| `/dev/fb0` for user programs (mmap) | CI |
| Per-device evdev nodes with EVIOCG* ioctls, keyboard LEDs | CI |
| Bluetooth LE keyboards/mice (HID over GATT) | CI (`tools/fake-hci.py` over H4) |
| Bluetooth BR/EDR keyboards/mice (HIDP) | written |

## Audio

| Device | Status |
|--------|--------|
| Intel HD Audio (legacy, non-DSP) codecs | CI (`intel-hda`, `ich9-intel-hda`) |
| virtio-sound | CI |
| USB Audio Class 1/2 (playback) | CI (`usb-audio`) |

## Target machines

The roadmap's two reference systems are an **Intel AX210 laptop** and a
**generic Intel/AMD desktop** (I219/I225/RTL8168 Ethernet, AHCI/NVMe,
xHCI). Everything above that is marked *written* is aimed at them. When
testing on hardware:

1. Build a USB stick with `./write_to_drive.sh --drive /dev/sdX`. Wi-Fi and
   Bluetooth firmware ship in the image (`firmware/stock.list`); the script
   stops if it cannot download them.
2. Optional: turn on extra logging before the first boot by creating
   `etc/kernel.conf` on the stick's storage partition (see below).
3. Boot it in UEFI mode (Secure Boot off) and run **`hwcheck`**.
4. Copy the `hwcheck-DATE/` directory (and any `bugreport-*.txt`) from the
   storage partition — it is FAT32, readable from any OS — and report the
   results (machine, component, logs) so this table can be updated.

### `hwcheck`

`hwcheck` walks through the whole checklist and records every step with
its complete command output:

| Section | Steps |
|---------|-------|
| `system` | kernel version and parameters, CPU, memory, `lspci`, PCI config of network devices, `lsusb`, `lsblk`, interfaces, interrupts |
| `ethernet` | per wired interface: link/driver/speed, DHCP, ping gateway, DNS, HTTP, HTTPS, optional throughput (`--big URL`) |
| `wifi` | firmware loaded, scan, open network, WPA2/WPA3 network (asks for SSID and passphrase, or `--ssid`/`--pass`/`--open`), the same connectivity steps, group-key refresh wait (`--rekey 3600`), disconnect/reconnect |
| `storage` | 8 MiB write + sync + read-back with SHA-256 on `/storage` and every writable `/mnt/*` (throughput shown) |
| `wifi` (continued) | 6 GHz networks in the scan (Wi-Fi 6E access point needed), power-save status |
| `storage` (continued) | ext4 mounts, journal and quota messages |
| `usb` | UAS queue depth, audio and Bluetooth devices from the log; stick insertion and removal (interactive) |
| `audio` | every card plays a tone (asks whether it was heard) and records one second |
| `bluetooth` | controller up (firmware loaded), scan, then interactively: pair a keyboard or mouse, see its input, disconnect, and check it reconnects with the stored key |

Results go to `/storage/hwcheck-DATE/` (`summary.txt`, one log per step,
and a full `bugreport.txt`). Run one section with e.g. `hwcheck wifi`;
`hwcheck -y` never prompts. `bugreport` alone writes the report file
(`dmesg`, `/proc`, `/sys/class/net`, `wifi status`, PCI config dumps,
firmware listing, and the tail of the persistent log).

### Debug switches (`/storage/etc/kernel.conf`)

The file holds `key=value` words (a bare `key` means `1`; `#` comments)
and is read at boot once the storage partition is mounted;
`/proc/cmdline` shows the active set.

| Key | Effect |
|-----|--------|
| `log.persist=1` | mirror the kernel log to `/storage/log/kernel.log` every 2 s (rotated at 4 MiB) — survives hangs |
| `iwlwifi.debug=1` | log every Wi-Fi firmware command and notification |
| `net.debug=1` | log a one-line summary of every Ethernet frame sent and received |
| `bt.h4=com2` | start a Bluetooth H4 (UART) controller on a serial port at boot |

A Wi-Fi firmware crash always dumps the firmware's LMAC/UMAC error tables
to the log.

## Not supported

GPU acceleration, Bluetooth audio, HD Audio behind an Intel SOF DSP,
Broadcom/Realtek Wi-Fi, USB Wi-Fi dongles other than the MT7921AU, Thunderbolt
tunnelling beyond what firmware sets up, suspend/resume (S3).
