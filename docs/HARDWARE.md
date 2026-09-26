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

Partitions: GPT and MBR. Filesystems: FAT12/16/32 with long names
(read/write), ext2 (read/write), ext3/ext4 (read-only, extents). Block
devices are cached (write-back buffer cache, `sync`).

## USB

| Component | Status |
|-----------|--------|
| xHCI host controller (BIOS handoff, MSI-X, hot-plug, USB 2/3 ports, stall recovery) | CI (`qemu-xhci`) |
| Hubs (USB 2 and 3, TT for low/full-speed) | CI (`usb-hub`) |
| HID boot keyboard and mouse | CI (`usb-kbd`, `usb-mouse`) |
| Mass storage | CI (`usb-storage`) |
| Ethernet: CDC ECM, RNDIS | CI (`usb-net`) |
| Ethernet: CDC NCM | written (NTB encoding host-tested) |

## Network

| Device | Status |
|--------|--------|
| virtio-net | CI |
| Intel 82540EM (`e1000`), 82574L (`e1000e`) | CI |
| Intel I217/I218/I219 (PCH LAN) | written |
| Intel I225/I226 (`igc`) | written |
| Realtek RTL8111/8168/8125 (`r8169`) | written |
| Intel AX210 Wi-Fi, AX211/AX201 CNVi | written ([WIFI.md](WIFI.md)) |

## Input and display

| Device | Status |
|--------|--------|
| PS/2 keyboard and mouse (i8042) | CI (keyboard) |
| USB HID keyboard/mouse | CI |
| GOP framebuffer text console (8x16 font, ANSI escapes, scrollback) | CI |
| `/dev/fb0` for user programs (mmap) | CI |

## Target machines

The roadmap's two reference systems are an **Intel AX210 laptop** and a
**generic Intel/AMD desktop** (I219/I225/RTL8168 Ethernet, AHCI/NVMe,
xHCI). Everything above that is marked *written* is aimed at them. When
testing on hardware:

1. Build a USB stick with `./write_to_drive.sh --drive /dev/sdX` (add
   `--ax210-firmware DIR` for Wi-Fi).
2. Boot it in UEFI mode (Secure Boot off).
3. Collect `dmesg > /storage/dmesg.txt` and `lspci; lsusb; lsblk; ip addr`
   output; the storage partition is readable from any OS.

Please report results (machine, component, log) so this table can be
updated.

## Not supported

GPU acceleration, audio, Bluetooth (the AX210's Bluetooth is a separate
USB function), Broadcom/Realtek Wi-Fi, USB Wi-Fi dongles, Thunderbolt
tunnelling beyond what firmware sets up, suspend/resume (S3).
