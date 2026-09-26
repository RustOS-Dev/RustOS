# RustOS Architecture

RustOS is a monolithic, preemptive, SMP x86-64 kernel written in Rust
(`no_std`, nightly), booted by UEFI through `bootloader` 0.11. User programs
run in ring 3 with their own address spaces and talk to the kernel through a
Linux-compatible system-call ABI; the userland (init, shell, coreutils,
network tools, dynamic linker) lives in `userland/` and is packed into an
initramfs embedded in the kernel image.

```
┌──────────────────────── ring 3 ─────────────────────────────────────────┐
│ init  sh  rbox (coreutils)  nettools (ip, wget, wifi, ...)  ld-rustos.so │
│                      rustos-rt (syscall wrappers, alloc, I/O)            │
├──────────── syscall / int 0x80 ──────────── signals / page faults ───────┤
│ process: fork/clone/execve(ELF, PIE, PT_INTERP)/wait, fds, signals, VM   │
│ sched: threads, run queue, wait queues, sleeping mutexes, SMP            │
│ vfs: tmpfs root, devfs, procfs, sysfs, FAT, ext2/3/4, pipes, TTY         │
│ net: smoltcp per interface, sockets, DHCP        block: cache, GPT/MBR   │
├──────────────────────────────────────────────────────────────────────────┤
│ drivers: NVMe AHCI virtio · xHCI+USB classes · e1000 igc r8169 virtio-net│
│          iwlwifi · PS/2 · framebuffer · serial · RNG                     │
│ platform: ACPI+AML · LAPIC/IOAPIC · MSI/MSI-X · PCIe · HPET/TSC · RTC    │
│ mm: frame allocator · page tables · heap · DMA · MMIO                    │
└──────────────────────────────────────────────────────────────────────────┘
```

## Source layout

| Path | Contents |
|------|----------|
| `src/main.rs`, `src/lib.rs` | entry point, `kernel_init`, `start_userspace`, test harness |
| `src/arch/x86_64/` | GDT/TSS, IDT and trap stubs, exceptions, APIC, ACPI, SMP, syscall entry, per-CPU data, RTC |
| `src/mm/` | frame allocator, kernel mappings, DMA buffers, kernel stacks |
| `src/allocator/` | kernel heap |
| `src/sched/` | threads, scheduler, wait queues, sleeping mutex |
| `src/process/` | processes, ELF loader, address spaces, fds, signals, uaccess |
| `src/syscall/` | system-call dispatch (fs, memory, process, misc) |
| `src/vfs/`, `src/fs/` | VFS core, tmpfs/devfs/procfs/sysfs/pipes; FAT and ext2 |
| `src/block/` | block layer, buffer cache, partitions |
| `src/usb/` | xHCI, USB core, hub, HID, storage, CDC Ethernet |
| `src/net/` | network core, sockets, socket syscalls |
| `src/drivers/` | block, net, wifi, virtio, console, framebuffer, serial, PS/2, RNG |
| `src/tty.rs`, `src/klog.rs`, `src/time.rs`, `src/firmware.rs`, `src/initramfs.rs` | terminal, kernel log, time, firmware loader, initramfs |
| `crates/` | host-testable libraries: `wlan`, `usb-desc`, `fat-format`, `rustos-rt`, `create-image` |
| `userland/` | init, sh, rbox, nettools, ldso, dynamic-linking tests, root filesystem files |

## Boot

1. **UEFI → bootloader 0.11**: maps the kernel (PIE) in the upper half,
   provides the memory map, a complete physical-memory mapping, the GOP
   framebuffer and the RSDP.
2. **`kernel_init`** (BSP, interrupts off): framebuffer and serial console,
   boot GDT and IDT, memory manager (frame bitmap from the memory map,
   kernel heap), per-CPU GDT/TSS and GS block, ACPI tables, LAPIC/IOAPIC,
   TSC calibration, PCIe ECAM, scheduler (the boot flow becomes thread
   `kmain`), LAPIC timer, `syscall` MSRs, IPIs, **application processors**,
   PS/2; then interrupts on, VFS (tmpfs root with devfs, procfs, sysfs), the
   TTY input thread.
3. **`start_userspace`**: unpack the initramfs into the root, start the
   network core, probe all buses (`drivers::probe_all`: storage, NICs,
   Wi-Fi, USB), automount partitions (`/storage`, `/boot/efi`,
   `/mnt/<dev>`), then exec `/sbin/init`.
4. **init** runs `/etc/rc` (network configuration, Wi-Fi autoconnect) and
   keeps a login shell on the console.

## Memory

Kernel virtual layout (upper half):

```
0xffff_8000_0000_0000  bootloader range: kernel image, boot stack, boot info,
                       framebuffer, complete physical-memory map
0xffff_c000_0000_0000  kernel heap
0xffff_d000_0000_0000  MMIO mappings (uncached)
0xffff_e000_0000_0000  kernel thread stacks (64 KiB + guard page, recycled)
```

User layout (lower half, per process):

```
0x0000_0000_0040_0000  static executables (ET_EXEC)
0x0000_5555_5555_0000  position-independent executables
     ...  heap (brk) above the image
0x0000_6fff_0000_0000  dynamic linker (ld-rustos.so.1)
     ...  mmap region, allocated downwards from 0x0000_7000_0000_0000
0x0000_7fff_ffff_0000  top of the 8 MiB stack
```

* **Frames**: bitmap allocator with contiguous/aligned/limited (below
  4 GiB) runs for DMA, per-frame reference counts for copy-on-write, and a
  separate path for sub-1 MiB pages (AP trampoline).
* **Address spaces** (`process/vm.rs`): areas (anonymous, physical/MMIO,
  permissions), demand paging, copy-on-write fork, `mmap`/`munmap`/
  `mprotect`/`brk`, TLB shootdown to other CPUs on unmap/protect/fork.
* **DMA**: `DmaBuffer` gives zeroed, physically contiguous, page-aligned
  memory (x86 DMA is cache coherent).

## Processes and threads

* Every thread has a kernel stack; context switches save callee-saved
  registers and swap stacks (`sched_switch`). FPU/SSE state (`fxsave`) and
  FS base are switched for user threads.
* **Scheduler**: preemptive round robin on a global run queue shared by
  all CPUs (quantum = 3 ticks at 250 Hz), idle thread per CPU, `on_cpu`
  handshake so a thread is never resumed on two CPUs, reschedule IPIs to
  idle CPUs, timed sleeps, wait queues, sleeping mutexes, preemption
  guards.
* **Processes**: `fork`/`vfork`/`clone` (threads with shared VM and fd
  table, `CLONE_SETTLS`, child TID), `execve` with argv/envp/auxv, shebang
  scripts, static, static-PIE and dynamically linked ELF programs,
  `wait4`, process groups/sessions, `exit_group`.
* **Signals**: POSIX-style handlers with `sigaction` flags, masks,
  `sigreturn`, alternate stacks, job control (SIGTSTP/SIGCONT/SIGTTIN),
  faults turned into SIGSEGV/SIGFPE/SIGILL/SIGBUS.
* **User access**: `copy_from_user`/`copy_to_user` validate ranges and
  fault pages in.

### Dynamic linking

A program with `PT_INTERP` is mapped together with its interpreter,
`/lib/ld-rustos.so.1` (a static-PIE Rust program in `userland/ldso`,
relocated by the kernel and started with `AT_BASE`/`AT_PHDR`/`AT_ENTRY`).
It loads `DT_NEEDED` libraries from `/lib` and `/usr/lib`, resolves symbols
through GNU or SysV hash tables (program first, then libraries in load
order), applies `RELATIVE`, `64`, `GLOB_DAT`, `JUMP_SLOT` (eager binding)
and `COPY` relocations, restores segment permissions, runs the libraries'
`DT_INIT`/`DT_INIT_ARRAY` and jumps to the program. TLS relocations and
`dlopen` are not supported.

## SMP

`smp::start_aps` copies a real-mode trampoline to a page below 1 MiB and
starts every MADT processor with INIT-SIPI-SIPI. The trampoline goes real
→ protected → long mode on temporary page tables (the kernel's plus an
identity map of the first 2 MiB), then `ap_entry` loads the kernel page
tables and the BSP's PAT/CR0/CR4 setup, builds the CPU's GDT/TSS/IDT and
GS block, enables its LAPIC and timer and joins the scheduler. Cross-CPU
work uses IPIs: TLB shootdown (generation counted; CPUs spinning with
interrupts off answer pending requests from their wait loops), reschedule
kicks and halt (panic, power-off, reboot).

## Interrupts and time

The IDT has stubs for all 256 vectors building a common `TrapFrame`;
handlers are registered per vector (`idt::register`, `alloc_vector` for
MSI/MSI-X). Legacy IRQs are routed through the IOAPIC (MADT overrides,
`_PRT` for PCI INTx). Time comes from the TSC (monotonic nanoseconds), the
LAPIC timer drives scheduling and sleeps, and the RTC provides wall-clock
time (`settimeofday`/`ntpdate` adjust it).

## Filesystems and I/O

* **VFS**: inode trait objects, mount table, path resolution with
  symlinks, permissions (mode bits, uid/gid, umask), open-file objects
  with offsets and flags, `poll`.
* **tmpfs** root, **devfs** (`/dev/null`, `zero`, `urandom`, `tty`,
  `console`, `fb0`, block devices, `input/mice`), **procfs** (processes,
  `meminfo`, `mounts`, `net/*`, `interrupts`, ...), **sysfs**
  (`class/net`, `class/block`, CPUs), **pipes** and FIFOs.
* **FAT** (12/16/32, long names, read/write), **ext2** (read/write),
  **ext3/ext4** (read-only, extents).
* **Block layer**: `BlockDevice` trait, write-back buffer cache,
  GPT/MBR partitions exposed as devices, automount rules.
* **TTY**: one console terminal (framebuffer + COM1) with a POSIX line
  discipline (canonical mode, echo, erase/kill, `VINTR`/`VSUSP`/`VEOF`),
  `termios` ioctls, window size, foreground process group.

## Devices

* **PCI**: ECAM enumeration, BAR sizing/mapping, capabilities, MSI/MSI-X
  setup (`enable_msix`, `enable_msi`, `enable_msi_or_intx`).
* **Storage**: NVMe, AHCI, virtio-blk, USB mass storage.
* **USB**: xHCI (interrupt driven, hot-plug), device enumeration and
  configuration choice, class drivers: hub, HID keyboard/mouse, mass
  storage, CDC ECM/NCM/RNDIS Ethernet.
* **Network**: see [NETWORKING.md](NETWORKING.md); Wi-Fi: see
  [WIFI.md](WIFI.md).
* **Power-off/reboot**: `drivers::shutdown` flushes filesystems and caches,
  sends NVMe shutdown notifications, resets NICs and stops USB before ACPI
  S5 (`_PTS`, `_S5` via AML) or reset.

## System calls

Linux x86-64 numbers through `syscall` (and `int 0x80`); see
[SYSCALLS.md](SYSCALLS.md). Userland uses `crates/rustos-rt`.

## Testing

* Host unit tests for the pure crates (`crates/wlan`, `usb-desc`,
  `fat-format`).
* Kernel tests (`cargo test`) boot under QEMU per test binary.
* Boot scenarios (`tools/run-scenarios.sh`) drive the serial console with
  real devices: shell language and job control, storage (NVMe, virtio,
  FAT, ext2/4, mkfs), USB (hub, HID, storage hot-plug), networking with
  every NIC model, HTTPS, dynamic linking and stress tests. They run with
  2 CPUs by default (`RUSTOS_SMP` overrides).
