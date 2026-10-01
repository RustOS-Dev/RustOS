# Round 4 plan: Linux drivers through LinuxKPI, Wi-Fi on any card, everyday hardware, GPUs and a desktop

## Context

Rounds 1–3 (M0–M26) are done. `main` = the working branch (`claude/comprehensive-implementation-plan-eg7p8k`), latest commit `4575a5b`.

Round 3 ended with these fixes:
- the stress-scenario timeout;
- the vendored `acpi` crate whose AML panics became errors (`third_party/acpi`, `machine-pc` scenario);
- `write_to_drive.sh` working piped from curl, under sudo, and with a non-rustup cargo first in PATH.

What prompted this round:
- **Wi-Fi:** the user's laptop has a **MediaTek MT7921K (RZ608, `14c3:0608`)**, which RustOS cannot use (only Intel AX210 is supported). The user wants Wi-Fi on "pretty much any card" plus the easy, generic devices.
- **Graphics:** the user's desktop (`AriPC`) has an **NVIDIA RTX 5070 (GB205, `10de:2f04`)** and an **AMD Raphael iGPU (`1002:164e`)**. The user wants AMD and NVIDIA graphics so that a desktop environment can be ported.
- **Licence:** the user relicensed RustOS to **GPL-2.0-or-later** (done: `17f6297`) and asked to borrow any Linux code needed.

**Approach:** RustOS gets a **LinuxKPI** layer: a Rust implementation of the Linux kernel APIs that drivers call (as FreeBSD does).
- **Compiled from Linux, mostly unmodified:** device drivers and the subsystems around them: 802.11 (mac80211/cfg80211), DRM, HID, I2C/GPIO, MMC, V4L2, ALSA, usbnet and usb-serial.
- **Native Rust, unchanged:** the RustOS core: memory management, scheduler, VFS and filesystems, the smoltcp network stack, xHCI and the syscall interface.
- **Pinned Linux version:** **v6.18.54** (current longterm, checked on kernel.org). All file lists, device IDs, firmware names and sizes below come from that tree.

This document is the single source for round 4; `docs/ROADMAP.md` carries the checklist.

## Conventions (unchanged from rounds 1–3)

- **Branch and pushes:**
  - Work on `claude/comprehensive-implementation-plan-eg7p8k`, one commit per milestone step.
  - Push the branch with retries, then fast-forward `main` (the user asked for everything on `main`).
  - No pull requests unless asked. Tags can't be pushed from this session: the user tags.
- **Every commit passes:** `cargo fmt --check`, `cargo clippy -- -D warnings`, host crate tests, and the QEMU scenarios touched. The full scenario suite and the 4-CPU suite run at each milestone end.
- **Each milestone:** gets ROADMAP checkboxes, a deviations note, doc updates (`HARDWARE.md`, `LIMITATIONS.md`, per-area docs) and a `hwcheck` section for hardware it adds.
- **Licences:**
  - Imported Linux files keep their SPDX headers.
  - Firmware is never committed. `write_to_drive.sh` copies it from the host's linux-firmware.

---

## 1. Decisions

| Topic | Decision |
|---|---|
| Driver source | Linux 6.18 LTS drivers compiled via LinuxKPI; pinned, updated deliberately at LTS point releases |
| Linux subsystems imported as-is | mac80211, cfg80211, `drivers/base` (device core), `lib/` helpers, qspinlock/qrwlock, DRM core + display helpers + TTM + scheduler, dma-buf, HID core, input-mt, I2C core, gpiolib, MMC core, V4L2 core, videobuf2, ALSA core, usbnet, usb-serial, phylib |
| Reimplemented in Rust (the shim) | tasks/`current`, scheduling, locks other than spinlocks, RCU, timers, workqueues/softirq/NAPI, memory (`kmalloc`, `struct page`, vmalloc, user mappings), IRQs, DMA, PCI, USB core on our xHCI, firmware loading, cdev/anon fds, sysfs bridge, ACPI on our AML interpreter, netdev/skb bridge, netlink, crypto subset, input → our evdev |
| Kernel C compiler | clang (Linux builds with `LLVM=1`) via the `cc` crate in `build.rs`; `bindgen` for Rust views of Linux structs |
| Wi-Fi userland | wpa_supplicant (nl80211) + hostapd for tests; `wifi` command becomes its front-end; WPA-Enterprise comes with it |
| Bluetooth | stays native (round 3 stack); vendor firmware loaders (btmtk, btrtl, btbcm) translated to Rust |
| Native drivers | kept until the Linux equivalent passes `hwcheck` on hardware, then retired: iwlwifi + `crates/wlan`, e1000/e1000e, igc, r8169, HDA, USB audio, virtio-sound |
| C++ | userland only (Mesa ACO, HarfBuzz, Qt); libc++/libc++abi/libunwind on musl |
| GPU order | AMD Raphael iGPU (amdgpu) first, then RTX 5070 (nouveau, GSP 570.144), then Intel (i915/xe) |
| Rejected | LKL (second kernel inside RustOS, no direct PCI), NVIDIA proprietary/open-gpu-kernel-modules (closed glibc userland), nova for now (Linux-Rust-abstraction based, no display in 6.18), rewriting drivers in Rust (except tiny ones) |

---

## 2. Milestones and order

| # | Milestone | Depends on | CI-testable |
|---|---|---|---|
| **M27** | LinuxKPI foundation; Linux `e1000` in QEMU | — | yes |
| **M28** | Networking glue, cfg80211/mac80211, netlink, wpa_supplicant/hostapd, `mac80211_hwsim` | M27 | yes |
| **M29** | **MT7921/MT7921K/MT7922 PCIe** (user's laptop) | M28 | host tests; hardware |
| **M30** | LinuxKPI USB core, xHCI isoch IN, Linux usbnet, MT7921AU, MediaTek BT firmware | M28 | yes (usb-net) |
| **M31** | Wi-Fi coverage: iwlwifi (all), rtw88, rtw89, mt76 family, mt7601u, ath9k/ath9k_htc, ath10k/11k/12k, brcmfmac | M29, M30 | hwsim regression; hardware |
| **M32** | Ethernet coverage: r8152, usbnet family, igb, alx, tg3, atlantic, Linux r8169/e1000e/igc | M27, M30 | yes (igb, usb-net) |
| **M33** | Laptop platform: I2C/GPIO, I2C-HID touchpads, HID core, MMC/SD, USB serial, UVC webcams, BT firmware | M27, M30 | partly |
| **M34** | Linux sound: ALSA core, HDA codecs, USB audio, SOF/ACP mics, OSS emulation | M27, M30 | yes |
| **M35** | DRM core, dma-buf, efidrm/simpledrm, bochs, virtio-gpu | M27 | yes |
| **M36** | Desktop kernel features | — | yes |
| **M37** | C++, Wayland stack, software-rendered Weston desktop | M33, M35, M36 | yes |
| **M38** | **AMD amdgpu** (Raphael first) | M35, M36 | bare metal |
| **M39** | **NVIDIA nouveau** (RTX 5070, GSP) | M35, M36 | VFIO |
| **M40** | Intel i915/xe | M35, M36 | hardware |
| **M41** | Mesa: RADV/radeonsi, NVK, iris/ANV, zink, EGL/GBM | M37 + M38/39/40 | optional (venus) |
| **M42** | Desktops: Weston → labwc/Sway → Xwayland → GTK → Qt/KDE | M37, M41 | yes (virtio-gpu) |

**Execution order:**
1. M27 → M28 → M29: the user's Wi-Fi.
2. Then M30.
3. Then M35 → M36 → M37: a desktop on every machine.
4. Then M38 → M39.
5. M31–M34, M40 and M41–M42 follow, interleaved as hardware reports arrive.

---

## 3. Import and build infrastructure (M27)

### 3.1 Layout

```
third_party/linux/
  VERSION            v6.18.54 + commit
  MANIFEST           path, SPDX licence, sha256 of every imported file
  PATCHES/           numbered patches against upstream (kept minimal)
  include/ lib/ kernel/locking/ drivers/... net/... sound/...   (Linux paths)
src/linuxkpi/        Rust shim (mod.rs + one module per area, §4)
  include/           RustOS replacements for Linux headers (asm/*, rustos-prelude.h)
  include/generated/autoconf.h   generated from configs/rustos.config
  configs/rustos.config          Kconfig fragment (what RustOS compiles)
tools/linux-import.py            import / update / licence check
tools/linux-undefined.py         per-group list of unresolved symbols
```

### 3.2 `tools/linux-import.py`

- **Import:** `import <group>...` copies the files for named groups (`e1000`, `mac80211`, `mt7921`, `drm-core`, `amdgpu`, …) from a Linux checkout at the pinned tag, closing over `#include`s. Imported files are never edited in place; changes live in `PATCHES/`.
- **Licence check:** refuses files without an SPDX tag and writes `MANIFEST`. Allowed licences: GPL-2.0-only/-or-later, GPL-2.0 WITH Linux-syscall-note, MIT, BSD, ISC.
- **Kconfig:** generates `autoconf.h` by running Linux's `scripts/kconfig/conf --olddefconfig` with `ARCH=x86_64` on `configs/rustos.config` in a scratch tree.
- **Linux updates:** `update vX.Y.Z` re-imports, reapplies `PATCHES/`, and reports conflicts and new undefined symbols.

### 3.3 Compilation (`build.rs`)

- **Build dependencies:** add `cc` and `bindgen`. Today `build.rs` compiles only userland C, and `Cargo.toml` has no `[features]`.
- **Cargo features:** `linux-e1000`, `linux-wifi`, `linux-usb`, `linux-eth`, `linux-platform`, `linux-sound`, `linux-drm`, `linux-drm-amd`, `linux-drm-nouveau`, `linux-drm-intel`. Only enabled groups are compiled, each into its own static library.
- **Flags:**
  - `-std=gnu11 -nostdinc -D__KERNEL__ -include linux/kconfig.h -include rustos-prelude.h`
  - include paths: `src/linuxkpi/include` first, then the Linux `include`, `arch/x86/include` and the uapi dirs
  - `-fPIE -mcmodel=small -mno-red-zone -mno-sse -mno-mmx -msoft-float -ffreestanding`
  - `-fno-common -fno-strict-aliasing -fno-delete-null-pointer-checks -fno-stack-protector`
  - These match `x86_64-rustos.json`: `code-model: small`, `relocation-model: pic`, soft-float, no red zone.
- **FPU files:** a per-file list (amdgpu DML/DML2, a few others), mirroring Linux's `CFLAGS_x = $(CC_FLAGS_FPU)`, is compiled with `-msse2 -mhard-float`. Those files only run inside `kernel_fpu_begin/end` (M36).
- **Initcalls:** `module_init`/`*_initcall` go into `.linuxkpi_initcalls.<level>` sections, run in level order at boot. They are hooked into `src/drivers/mod.rs::probe_all` after the native probes.
- **Module metadata:** `MODULE_FIRMWARE` names are collected into a table, published as `/proc/linux/firmware` and in `target/firmware-list.txt` (§9). Other `MODULE_*` macros compile to nothing.
- **Symbols:**
  - The Rust shim exports `#[no_mangle] extern "C"` functions.
  - `bindgen` generates layouts of the Linux structs the shim touches (`pci_driver`, `pci_dev`, `net_device_ops`, `usb_driver`, `file_operations`, `sk_buff`, …) from the real headers.
  - `tools/linux-undefined.py` (from `llvm-nm`) gives each milestone its work queue.

---

## 4. LinuxKPI design, mapped to RustOS internals

Integration points found in the tree (paths relative to repo root). "Gap"
means RustOS lacks it today and M27 adds it.

### 4.1 Headers and arch

- **Linux x86 headers used as-is:** `asm/atomic.h`, `cmpxchg.h`, `barrier.h`, `bitops.h`, `io.h`, `msr.h`, `unaligned`, `byteorder`.
- **Replaced in `src/linuxkpi/include/asm/`:**
  - per-CPU and current task: `percpu.h` (arrays indexed by CPU id from `src/arch/x86_64/cpu.rs::this().cpu_id`), `current.h`;
  - CPU state: `irqflags.h`, `page.h`/`pgtable_types.h`, `extable.h` (none);
  - patching mechanisms with fallbacks: `alternative.h` (baseline instructions), `jump_label.h` (plain branches), `static_call.h` (indirect calls);
  - `paravirt.h` (none), `fpu/api.h`.
- **Kconfig:** `SMP=y`, `NR_CPUS=64`, `PREEMPT_NONE=y`, `HZ_250=y` to match `src/time.rs::HZ = 250`. These are off:
  - debug and sanitizers: `MODULES`, `DEBUG_*`, `LOCKDEP`, `KASAN`, `KCSAN`;
  - tracing, debugfs and sleep states: `TRACING`, `DEBUG_FS`, `PM_SLEEP`;
  - mechanisms RustOS bridges or lacks: `SYSFS` (bridged), `PARAVIRT`, `JUMP_LABEL`, `RETPOLINE`/mitigations.

### 4.2 Tasks and scheduling

- **`struct task_struct` shadow:** allocated lazily per RustOS thread (`src/sched/mod.rs:55 Thread`), keyed by `current_tid()`, and freed in the thread exit path. Fields: `comm`, `pid`, `flags`, `state`, `cpus_mask`, plus a back-pointer.
- **Blocking:** `set_current_state` + `schedule()` map to `sched::prepare_block()` + `sched::schedule()`. `wake_up_process` → `sched::wake()`.
  - The existing `wakeup_pending` flag already gives Linux's "set state, check condition, schedule" pattern its no-lost-wakeup guarantee.
- **Timed sleeps:** `schedule_timeout` → `sleep_until` / `arm_timeout` (make `arm_timeout` pub(crate) → pub for the shim).
- **`cond_resched`/`yield`** → `yield_now()`.
- **kthreads:** `kthread_create/run/stop/should_stop/park` → `sched::spawn` + a stop flag.
- **Preemption state:** `preempt_disable/enable`, `in_atomic`, `irqs_disabled` → `PerCpu.preempt_count` and `sched::PreemptGuard`; `in_interrupt` from an IRQ-depth counter (gap: add one in `src/arch/x86_64/idt.rs` dispatch).

### 4.3 Locking

- **Spinlocks:** `spinlock_t`/`rwlock_t` use Linux's imported `kernel/locking/qspinlock.c` and `qrwlock.c`, wrapped with IRQ save/restore and `preempt_count`.
- **Sleeping locks:** `mutex`, `rw_semaphore`, `semaphore` are implemented in the shim on `src/sched/wait.rs WaitQueue`. The Linux struct layouts are kept: owner, wait list → our queue pointer.
- **`ww_mutex`** (DRM/TTM): wait-die/wound-wait in the shim.
- **Completions and wait queues:**
  - `completion` → `WaitQueue::wait_until`/`wake_all`;
  - `wait_queue_head_t` uses Linux's header macros on top of shim `prepare_to_wait`/`finish_wait`/`__wake_up`, with `WaitQueue::add_hook` used for poll.

### 4.4 Time, timers, deferred work, RCU

- **Clocks:**
  - `jiffies` counts at 250 Hz from `time::ticks()`;
  - `ktime_get*` → `time::nanos()`, `ktime_get_real` → `time::realtime_nanos()`;
  - `udelay/ndelay/mdelay` → `time::delay_us`, `msleep`/`usleep_range` → sleeps.
- **Timers:** `timer_list` and `hrtimer` use `sched::add_timer(deadline, Arc<dyn TimerTarget>)` with a generation counter for cancellation.
  - **Gap:** `add_timer` has no cancel. Add `cancel_timer(handle)` (heap removal or a tombstone) and a "running callback" flag so `del_timer_sync`/`hrtimer_cancel` can wait.
  - Callbacks are moved out of IRQ context into the per-CPU softirq thread. Linux timer callbacks run with IRQs on.
- **Softirq threads:** one per CPU, running tasklets, timer callbacks, NAPI polls and URB completions.
  - `local_bh_disable/enable` is a per-CPU counter that holds that CPU's softirq thread off.
  - `NAPI` polls run with budget 64; `napi_gro_receive` passes through (no GRO).
- **Workqueues:** `alloc_workqueue`/`alloc_ordered_workqueue`, `queue_(delayed_)work`, `mod_delayed_work`, `flush_*`, `cancel_*_sync`, and the system workqueues.
  - Each is a pool of `sched::spawn` threads (ordered queues: one thread).
  - `sched::defer` (kworker) stays for native code.
- **RCU:**
  - `rcu_read_lock` = `preempt_disable`;
  - `synchronize_rcu` waits for a per-CPU quiescent-state counter, bumped by the scheduler on context switch, idle and return to user (a small hook in `schedule()`);
  - `synchronize_rcu_expedited` uses `apic::send_ipi`;
  - `call_rcu`/`kfree_rcu`/`rcu_barrier` go through an RCU thread;
  - `srcu` uses the two-index counter algorithm.

### 4.5 Memory

- **Small allocations:** `kmalloc` family / `kmem_cache` → the kernel heap (`src/allocator.rs`) with size headers. `GFP_ATOMIC` uses a reserve pool, and `GFP_DMA32` → `mm::dma` below-4 GiB.
- **`struct page`:** a `mem_map` array covering all RAM, built at boot from the frame allocator's memory map (64 B per 4 KiB page, about 1.6% of RAM).
  - `alloc_pages`/`__free_pages` → `mm::frame` `alloc`/`alloc_contiguous`/`free_contiguous` via `with_frames`.
  - `page_address` → `phys_to_virt`; folio wrappers over single and compound pages.
- **vmalloc:** `vmalloc`/`vmap` → `mm::map_kernel_pages`.
  - **Gap:** there is no VA allocator. Add a simple range allocator over the existing `src/mm/mod.rs:32-38` windows.
- **MMIO:** `ioremap*`/`memremap`/`pci_iomap`/`io_mapping_*` → `mm::map_mmio`.
  - **Gap:** it only maps UC with no unmap. Add a cache type (UC/WC/WB via PAT; WC is needed for GPU BARs) and `unmap_mmio`.
- **Scatterlists:** imported from `lib/scatterlist.c`.
- **User mappings:** `remap_pfn_range`, `io_remap_pfn_range`, `vm_insert_page`, `vmf_insert_pfn(_prot)`, `vm_map_pages`, and `vm_operations_struct.fault`.
  - **Gaps:**
    - add `FileLike::mmap` to `src/vfs/mod.rs:269` (today device mmap is hard-wired to `FbDev` in `src/drivers/mod.rs:15`);
    - add a `Backing::Device{ops}` VMA kind to `src/process/vm.rs:41`, faulting through the driver's `fault` handler;
    - add a cache type on `Backing::Phys` (it is UC-only at `vm.rs:416`).
- **`shmem` (GEM):** backed by M36's memfd/anonymous objects (`Backing::Shm` exists).
- **`copy_to/from_user`, `get/put_user`** → RustOS's existing user-copy routines.

### 4.6 Interrupts, DMA, PCI, ACPI, firmware

- **IRQs:** `request_(threaded_)irq`, `free_irq`, `disable/enable_irq`, `synchronize_irq`.
  - MSI/MSI-X via `PciDevice::enable_msix`/`enable_msi`/`enable_intx` (`src/pci/mod.rs:368/420/443`) and `idt::alloc_vector(_block)`; IOAPIC through `apic::route_gsi`/`mask_gsi`.
  - Threaded handlers get a thread; `IRQF_SHARED` chains handlers.
  - `pci_alloc_irq_vectors`/`pci_irq_vector` sit on top of these.
- **DMA:** `dma_alloc_coherent`/`dma_map_*`/`dma_sync_*`/`dma_pool_*` use `mm::dma` (`DmaBuffer::new`, `new_32bit`, `new_constrained`). There is no IOMMU, so bus address = physical address, and syncs are barriers.
  - Streaming mappings above a device's DMA mask are bounced through a swiotlb-style pool.
- **PCI:** `struct pci_dev` is built from `PciDevice` (`src/pci/mod.rs:155`).
  - Config access → `read*/write*`; `pci_enable_device` → `enable()`; `pci_set_master` → `enable_bus_master()`;
  - capabilities → `find_capability`/`find_ext_capability`; `pci_reset_function` → `function_reset`; ASPM → `disable_aspm`.
  - **Gap:** there is no driver registry; `probe_all()` hard-codes matches. Add `pci::register_driver` with id-table matching, used by `pci_register_driver` (and later by native drivers too).
  - **Also new:** `pci_map_rom`, `pcie_get_speed_cap`/`width_cap`, and secondary bus reset.
- **ACPI:** `acpi_evaluate_object/integer`, `acpi_evaluate_dsm`, `acpi_get_handle`, `acpi_get_table`, `ACPI_COMPANION`, and `acpi_device_id` matching.
  - **Gap:** `with_interpreter` is private. Add a public `acpi::eval(path, args)`, `acpi::dsm(...)` and `acpi::table(sig)` in `src/arch/x86_64/acpi.rs`.
  - Implement the `_CRS` GPIO/I2C/SPI resource parsers in `third_party/acpi` (now `LibUnimplemented`) for M33.
- **Firmware:** `request_firmware(_nowait)`, `firmware_request_nowarn`, `request_firmware_direct`, `release_firmware` → `firmware::load`/`load_any` (`src/firmware.rs`).
  - Move iwlwifi's retry loop (`src/drivers/wifi/iwlwifi/mod.rs:665-680`) into a generic `_nowait` retry: 60 s after boot, plus on-demand.
  - Add `.zst`/`.xz` decompression (Linux `lib/zstd`, `lib/xz` imported).

### 4.7 Device model, files, sysfs, logging

- **Device core:** `drivers/base` is imported (core, bus, driver, dd, devres, class, platform, component, property, swnode) with `lib/kobject.c`. This gives real probe ordering, deferred probe and `devm_*`.
- **Character devices:** `register_chrdev`/`cdev_add`/`misc_register`/`device_create` → `vfs::devfs::register` with a `LinuxCdev` `FileLike` that calls `file_operations`.
  - `poll` → the per-object `WaitQueue`, and `mmap` → the new `FileLike::mmap`.
  - **Gap:** `devfs::register` allows one subdirectory level; DRM needs `/dev/dri/`, which fits.
- **Anonymous fds:** `anon_inode_getfile/getfd`, `get_unused_fd_flags`, `fd_install`, `fget/fput`.
- **sysfs:** **Gap:** `src/vfs/sysfs.rs` is read-only path matching. Add a registration tree (`sysfs::add(path, show, store)`), so the device core's `sysfs_create_group`/`device_create_file` and `uevent` files appear under `/sys/devices` and `/sys/class/*`. Existing generated nodes stay.
- **Runtime PM:** `pm_runtime_*` is always active.
- **Logging:**
  - `printk`/`dev_*`/`netdev_*` → `klog`, with levels;
  - `%p` extensions via imported `lib/vsprintf.c` (symbol lookup stubbed);
  - `WARN_ON` = backtrace and continue.
- **Kernel options:** `module_param` → `params::get("<module>.<param>")` (`src/params.rs`). `linux.debug=<module>` raises `dev_dbg`.

### 4.8 Networking glue (M28)

- **net_device:** `alloc_netdev_mqs`/`alloc_etherdev`/`register_netdev` create a `LinuxNetDev` implementing `net::NetDevice` (`src/net/mod.rs:41`) and call `net::register` (`:804`).
  - **Transmit:** `transmit()` builds an skb and calls `ndo_start_xmit` (queue stop/wake honoured).
  - **Receive:** `netif_rx`/`netif_receive_skb`/`napi_gro_receive` push frames into a per-device RX queue drained by `receive()`, and call `net::kick()` (`:759`). The stack stays pull-based.
  - **Carrier:** `netif_carrier_on/off` → `link_up()`, so the existing `Iface::poll` link-change path restarts DHCP.
  - `kind()` = `Wireless` when `ieee80211_ptr` is set.
- **`sk_buff`:** inline helpers from Linux's header; the out-of-line core in the shim (alloc/free/clone/copy/expand/linearize, frags, queues, checksum help).
- **Netlink** (gap, Rust): `AF_NETLINK` with `NETLINK_ROUTE` (link/addr/route get/set: what `ip` and wpa_supplicant need), `NETLINK_GENERIC` (family registration from C: `genl_register_family`, `genlmsg_*`; `lib/nlattr.c` imported) and `NETLINK_KOBJECT_UEVENT` (M36).
- **`AF_PACKET`** (gap, Rust): raw/dgram, protocol filter, membership.
- **Socket ioctls:** `SIOCGIFFLAGS/SIOCSIFFLAGS/SIOCGIFINDEX/SIOCGIFHWADDR/SIOCETHTOOL` on socket fds.
- **Crypto subset:** `ccm(aes)`, `gcm(aes)`, `cmac(aes)`, `gmac(aes)`, `ctr(aes)`, `arc4`, `crc32_le` (the set mac80211/cfg80211 request) in Rust on the existing RustCrypto crates.
- **Unix sockets:** `AF_UNIX` exists (`src/net/socket.rs`), but `socketpair` is pipe-based (`syscalls.rs:289`). M28 needs `SOCK_DGRAM` Unix sockets for wpa_supplicant's control interface (M36 completes `SCM_RIGHTS` etc.).

### 4.9 USB core (M30)

- **Devices:** `struct usb_device`/`usb_interface` built from `src/usb/mod.rs:86 UsbDevice` and the `crates/usb-desc` descriptors.
- **Binding:** `usb_register_driver` → `usb::register_driver(name, probe)` (`:339`), matching `usb_device_id` tables.
- **URBs:** control → `control_in/out`; bulk/interrupt → `submit`/`wait`/`cancel` (`:259/290/294`); isochronous → `submit_isoch` (`:276`). Completions run in the softirq thread. Also anchors, `usb_kill_urb` and `usb_poison_urb`.
- **Gap:** isochronous **IN** in `src/usb/xhci.rs` (round 3 did OUT only), needed by UVC and USB audio capture.
- **Sync helpers:** `usb_control_msg`/`bulk_msg`/`interrupt_msg`, `usb_set_interface`, `usb_clear_halt` → `clear_halt`, `usb_reset_device`, `on_detach` → disconnect.

### 4.10 Input/HID/I2C/GPIO (M33)

- **Input:** `input_allocate/register_device`, `input_set_abs_params`, `input_event` → `drivers::input::register(Info)` / `InputDev::emit`/`sync` (`src/drivers/input.rs:210/244/278`). `input-mt.c` is imported for multitouch.
- **IRQ domains:** `irq_domain`/`irq_chip`/`generic_handle_irq` subset for GPIO interrupts.

**Shim size:** about 15–25k lines of Rust plus 3k lines of replacement headers, built up over M27–M35.

---

## 5. M27 — LinuxKPI foundation

**Steps:**
1. **Infrastructure:** `tools/linux-import.py`, `third_party/linux` at v6.18.54, `MANIFEST`, `configs/rustos.config`, `build.rs` clang compilation, the Cargo features, the initcall section, the undefined-symbol report.
2. **RustOS gaps** (§4): timer cancel; IRQ-depth counter; VA range allocator; MMIO cache types and unmap; `FileLike::mmap` + `Backing::Device` + cached `Backing::Phys`; PCI driver registry; public ACPI eval/DSM/table; sysfs registration tree; firmware `_nowait` retry; RCU quiescent hook in `schedule()`.
3. **Shim core:** §4.1–4.7 (arch headers, tasks, locks, time/timers/softirq/workqueues/RCU, memory/`struct page`, IRQ, DMA, PCI, firmware, cdev/anon fds, device core, logging).
4. **Proof:** Linux `drivers/net/ethernet/intel/e1000` (about 9k lines) behind `--features linux-e1000`, replacing the native e1000 for `8086:100e` in that build.

**Tests:**
- `eth-linux-e1000` scenario: the `eth-e1000` steps (DHCP, ping, 4 MiB HTTP download with checksum) on the Linux driver;
- host unit tests for RCU grace periods, workqueue flush ordering, timer cancel races, `ww_mutex`, skb ops;
- a test-image-only C module exercising wait queues, completions, kthreads and hrtimers in the kernel.

**Progress** (details in [LINUXKPI.md](LINUXKPI.md)):
- [x] Step 1: import tooling, `third_party/linux` (v6.18.54), clang build, `linuxkpi`/`linux-e1000` features, initcall sections.
- [x] Step 4: Linux `e1000` passes `tests/scenarios/linux/eth-linux-e1000` (DHCP, ping, 4 MiB HTTP both ways), run in CI on 2 and 4 CPUs.
- [ ] Step 3: shim core. Done so far:
  - memory, per-CPU data, preemption, tasks/kthreads, mutexes, completions and wait queues;
  - timers, softirq/tasklets, workqueues;
  - printk/`%p` extensions;
  - PCI/MSI/INTx, DMA;
  - netdev/skb/NAPI.
  - Still to do: RCU, `ww_mutex`, firmware, cdev/anon fds, the `drivers/base` device core, sysfs.
- [ ] Step 2: RustOS gaps (the list above). None done yet; the `_PIC` fix below came up during step 4.
- [ ] Tests: the kernel self-test (`src/linuxkpi/c/selftest.c`) stands in for the test-image C module; host unit tests to do.

**Deviations so far:**
- **No `cc`/`bindgen`.** `build/linuxkpi.rs` drives clang directly, with parallel, incremental builds that track the `.d` dependency files. The Rust side never touches Linux structs: the C glue in `src/linuxkpi/c/`, compiled against the real headers, owns every Linux layout. It passes small `#[repr(C)]` records declared in `kpi.h` to Rust.
- **`CONFIG_PREEMPT=y`, not `PREEMPT_NONE`,** so that Linux spinlocks raise the shared `preempt_count` and `in_atomic()` is accurate.
- **Interrupt routing fix.** RustOS now calls `\_PIC(1)` when the AML interpreter starts, as Linux does. Without it, firmware `_PRT` methods (QEMU q35 among them) return legacy-PIC link routing. On q35 that made the e1000 resolve to GSI 1.
  - With correct routing, PCI devices share GSIs (on q35, 16–23). `PciDevice::enable_intx` therefore keeps one vector per GSI and runs every handler registered on it; before this, a second device on a GSI took over the first one's interrupt.
- **Timer cancel.** Timers are cancelled in the LinuxKPI layer (a handle table plus the softirq thread), not in `sched::add_timer`.

## 6. Wi-Fi

### M28 — 802.11 stack

**Imported:**
- `net/wireless` (cfg80211, 55k lines): core, sysfs, radiotap, util, reg, scan, nl80211, mlme, ibss, sme, chan, ethtool, mesh, ap, ocb, michael-mic, pmsr.
  - `CFG80211_WEXT=n`, `CFG80211_REQUIRE_SIGNED_REGDB=n`; `regulatory.db` is provisioned.
- `net/mac80211` (90k): main, status, driver-ops, sta_info, wep, aead_api, wpa, scan, offchannel, ht, agg-tx/rx, vht, he, eht, s1g, ibss, iface, link, rate, tkip, aes_cmac, aes_gmac, fils_aead, cfg, ethtool, rx, spectmgmt, tx, key, util, parse, wme, chan, mlme, tdls, ocb, airtime, plus `minstrel_ht`.
- `drivers/net/wireless/virtual/mac80211_hwsim.c`.

**RustOS:** §4.8 glue; netlink and genetlink; `AF_PACKET`; socket ioctls; Unix `SOCK_DGRAM`.

**Userland:**
- **Ports:** `ports/libnl-tiny`, `ports/wpa_supplicant` (nl80211, ctrl iface, SAE, PMF, OWE, EAP PEAP/TTLS/TLS with a TLS library port), `ports/hostapd` (for tests).
- **`wifi` rewrite:** `userland/nettools/src/wifi.rs` becomes a wpa_supplicant control-socket client with the same commands (`scan`, `connect … --save`, `forget`, `auto`, `power`).
- **Configuration:** `/storage/etc/wifi.conf` is kept and translated into `wpa_supplicant.conf`.
- **Startup:** `/etc/rc` starts wpa_supplicant for wireless interfaces. `/etc/rc` currently runs `wifi auto -q &`; that call is kept as the wrapper.

**Native AX210 path:** stays the default for `8086:2725` until Linux iwlwifi (M31) passes `hwcheck`.

**Tests:**
- `wifi-hwsim` scenario: hostapd on `wlan1` (WPA2-PSK, then WPA3-SAE-H2E with PMF), `wifi connect` on `wlan0`, DHCP from busybox `udhcpd` on the AP side, ping, group rekey (`wpa_group_rekey=5`), disconnect/reconnect, roaming between two BSSs;
- `wifi-hwsim-eap`: PEAP-MSCHAPv2 against hostapd's EAP server.

### M29 — MT7921K on the user's laptop

**Imported** (`drivers/net/wireless/mediatek/mt76`, ISC):
- core: mmio, util, dma, mac80211, eeprom, tx, agg-rx, mcu, scan, channel, pci;
- connac: mt76_connac_mcu, mt76_connac_mac, mt76_connac3_mac;
- mt792x: mt792x_core, _mac, _dma, _acpi_sar;
- `mt7921/`: mac, mcu, main, init, pci, pci_mac, pci_mcu (7.3k lines);
- optional: `mt7925/` (Wi-Fi 7).

**Devices:** `14c3:7961`, `14c3:0608` (user's RZ608), `14c3:7922`, `0b48:7922`, `14c3:0616`, `14c3:7920`.

**Firmware:**
- MT7921: `mediatek/WIFI_MT7961_patch_mcu_1_2_hdr.bin`, `WIFI_RAM_CODE_MT7961_1.bin`;
- MT7922: `WIFI_MT7922_patch_mcu_1_1_hdr.bin`, `WIFI_RAM_CODE_MT7922_1.bin`;
- MT7920: `WIFI_RAM_CODE_MT7961_1a.bin`.

**Shim additions:** ACPI `_DSM` (SAR tables), `pci_disable_link_state` → `disable_aspm`.

**Bring-up loop:**
1. `write_to_drive.sh` (copies the firmware).
2. Boot and check `dmesg` (probe, ASIC revision, `WM Firmware Version`, `wlan0`).
3. `hwcheck wifi` (scan, WPA2, WPA3, DHCP, ping, HTTP, rekey, reconnect); the user sends the result folder.
4. Switches in `kernel.conf`: `linux.debug=mt76`, `mt7921e.disable_aspm=1`.

### M30 — USB shim, MT7921AU, Bluetooth firmware

- **USB shim:** §4.9 plus xHCI isochronous IN.
- **CI proof:** Linux `usbnet` + `cdc_ether` + `rndis_host` + `cdc_ncm` driving QEMU `usb-net` (`eth-usb-linux` scenario).
- **MT7921AU:**
  - imported: mt76-usb (`usb.c`), `mt792x_usb.c`, `mt7921/usb.c`;
  - devices: `0e8d:7961`, `3574:6211`, `0846:9060`, `0846:9065`, `35bc:0107`;
  - this is the recommended "Wi-Fi anywhere" adapter.
- **MediaTek BT:** btmtk's WMT patch download is translated into `src/usb/btusb.rs`. Firmware: `mediatek/BT_RAM_CODE_MT7961_1_2_hdr.bin`, `BT_RAM_CODE_MT7922_1_1_hdr.bin`, `mt7925/BT_RAM_CODE_MT7925_1_1_hdr.bin`.
- **Test:** QEMU `usb-host` passthrough of an MT7921AU on a developer machine.

### M31 — Wi-Fi coverage

| Driver | Lines | Chips |
|---|---|---|
| iwlwifi (mvm + mld; dvm excluded) | 180k | Intel 7260 → AX210/AX211 → BE200 (mld picked for Wi-Fi 7 with fw API ≥ 97); then retire native iwlwifi + `crates/wlan` |
| rtw88 | 162k | RTL8821CE/8822BE/8822CE/8723DE, RTL8821CU/8822BU/8822CU/8723DU/8811CU |
| rtw89 | 291k | RTL8852AE/BE/CE, 8851BE, 8922AE, USB variants |
| mt76 rest | — | mt76x2u (MT7612U), mt76x0u, mt7615, mt7915, mt7925 |
| mt7601u | 8k | MT7601U |
| ath9k + ath9k_htc | 88k | AR9xxx, AR9271 (open firmware) |
| ath10k / ath11k (+ MHI, QRTR, QMI) / ath12k | 85k / 81k / 91k | QCA6174/9377, WCN6855/QCA2066, WCN7850 |
| brcmfmac | 40k | Broadcom FullMAC |

**Per driver:**
- import it and resolve its undefined symbols;
- add its firmware to provisioning and its IDs to `hwcheck`;
- run the `wifi-hwsim` regression.

**Acceptance:** a `hwcheck wifi` pass on one device; otherwise it is listed "compiled, untested" in `HARDWARE.md`.

## 7. Everyday hardware

### M32 — Ethernet

| Driver | Hardware |
|---|---|
| `r8152` | RTL8152/8153/8156/8157 USB adapters and docks |
| `usbnet` + `ax88179_178a`, `asix`, `cdc_ncm`, `cdc_ether`, `rndis_host`, `ipheth` | ASIX adapters, Android tethering, iPhone tethering (with a `usbmuxd` port) |
| `igb` | Intel I210/I211/I350 (CI: QEMU `-device igb`) |
| `alx` | Killer E2200–E2600, AR8161/8171 |
| `tg3` (22k) | Broadcom NetXtreme |
| `atlantic` | AQC107/108/113 |
| Linux `r8169`, `e1000e`, `igc` | replace the natives after `hwcheck ethernet` |

**Plus:** `drivers/net/phy` (phylib, realtek, marvell), MDIO, ethtool link settings.

**Tests:** `eth-igb`, `eth-usb-linux` (CI); hardware for the rest.

### M33 — Laptop platform

- **Touchpads and touchscreens:**
  - imported: I2C core (with `i2c-core-acpi`), DesignWare I2C (`AMDI0010`, Intel LPSS PCI), gpiolib + `gpiolib-acpi-core`, `pinctrl-amd` (`AMDI0030`; Intel pinctrl per chipset later), `i2c-hid-core` + `i2c-hid-acpi` (`PNP0C50`), and HID core (`hid-core`, `hid-input`, `hid-quirks`, `hid-generic`, `hid-multitouch`);
  - multitouch reaches `/dev/input/eventN` through §4.10;
  - a polling fallback (a small RustOS patch) covers unsupported GPIO controllers;
  - test: `hwcheck input` (new section).
- **SD/MMC:**
  - imported: `drivers/mmc/core`, `sdhci`, `sdhci-pci`, `sdhci-acpi`, `rtsx_pci_sdmmc`, `rtsx_usb_sdmmc`, `drivers/misc/cardreader`;
  - a blk-mq subset bridges to RustOS's block layer (`mmcblkN`);
  - test (CI): QEMU `sdhci-pci` + `sd-card` with FAT32 and ext4 round trips.
- **USB serial:**
  - imported: `drivers/usb/serial` (usb-serial, generic, ftdi_sio, cp210x, ch341, pl2303, option) and `cdc-acm`;
  - a `tty_driver`/`tty_port` shim onto RustOS's TTY layer;
  - test (CI): QEMU `usb-serial`.
- **Webcams:**
  - imported: `v4l2-core`, `videobuf2`, `uvcvideo`, giving `/dev/video0` and `/dev/media0`;
  - needs isochronous IN (M30);
  - test: `hwcheck webcam` grabs a frame.
- **Bluetooth firmware:** btrtl (`rtl_bt/*`) and btbcm (`brcm/*.hcd`) translated into `btusb`.

### M34 — Linux sound

- **Imported:**
  - `sound/core` (PCM, control, timer, OSS emulation for existing `/dev/dsp` users);
  - `sound/hda` in the 6.18 layout: `core/`, `common/`, `controllers/intel.c` (snd-hda-intel), and `codecs/` (`realtek/`, `hdmi/`, `cirrus/`, `conexant.c`, `generic.c`, `helpers/`, `side-codecs/`);
  - `sound/usb`, `sound/virtio`;
  - later: `sound/soc/sof` (+ `sof/amd`), `sound/soc/amd` (`acp/`, `yc/`, `ps/`, `rpl/`, `renoir/`, `vangogh/`) and `sound/soc/intel`, for DMIC microphones.
- **Result:** the full ALSA uAPI (`/dev/snd/*`) for PipeWire.
- **Native drivers:** `src/sound/{hda,virtio_snd}.rs` and `src/usb/audio.rs` are retired after reports.
- **Test:** the `audio` scenario on Linux HDA and virtio-sound (CI).

## 8. Graphics and desktop

### M35 — DRM

- **Imported:**
  - DRM core, `drivers/gpu/drm/*.c` (80k): drv, file, ioctl, gem, prime, atomic*, crtc*, plane*, connector, encoder, framebuffer, fourcc, modes, edid, mode_config/object, property, blend, color_mgmt, damage_helper, vblank, syncobj, mm, buddy, exec, gem_shmem_helper, gem_atomic_helper, gem_framebuffer_helper, fb_helper, fbdev_*, client*, format_helper, managed;
  - `drm/display` (20k), `ttm` (11k), `scheduler` (4k);
  - `drivers/dma-buf` (dma-buf, fences, dma-resv, sync_file);
  - display drivers: `drm/sysfb` (`efidrm`, `simpledrm`, `drm_sysfb*`), `drm/tiny/bochs.c`, `drm/virtio`.
- **RustOS:**
  - device nodes: `/dev/dri/card0` and `renderD128`;
  - sysfs bridge: `/sys/class/drm/card0-*` (status, modes, edid, enabled);
  - the GOP framebuffer described to `efidrm`;
  - **console handover:** the framebuffer console (`src/drivers/framebuffer.rs`) moves onto `drm_client`, and aperture helpers evict it and efidrm when a GPU driver loads;
  - `/dev/fb0` comes from DRM fbdev emulation (`browse -g` keeps working);
  - GOP mode: largest up to the panel's native size; `write_to_drive.sh --resolution WxH` overrides it.
- **Tests (CI):** `drm-bochs`, `drm-virtio`, `drm-efidrm` with `modetest` (connectors, modes, set mode, page flip with events, PRIME, sync_file) and QEMU `screendump`.

### M36 — Desktop kernel features (native Rust)

- **Unix sockets:** `SCM_RIGHTS`, `SCM_CREDENTIALS`, `SO_PEERCRED`, `SO_PASSCRED`, abstract names, a real `AF_UNIX` `socketpair` (today pipe-based).
- **Shared memory:** `memfd_create` + seals; `/dev/shm` tmpfs; `MAP_SHARED` anonymous/memfd/shm across `fork` and fd passing.
- **`inotify`:** with events from the VFS.
- **uevents:** `NETLINK_KOBJECT_UEVENT` broadcasts from the device-model bridge, and `/sys/.../uevent` files.
- **VT switching:** `VT_SETMODE`/`VT_PROCESS`/`VT_RELDISP`/`VT_ACTIVATE`/`WAITACTIVE`, `KDSKBMODE K_OFF`, DRM master handover, `EVIOCREVOKE`.
- **Kernel FPU:** `kernel_fpu_begin/end` (XSAVE, preempt-disabled), for amdgpu DML.
- **Small syscalls:** `pidfd_open`, `pidfd_send_signal`, `close_range`, `statx`, `copy_file_range`, `CLOCK_BOOTTIME`/`MONOTONIC_RAW`, `clock_nanosleep(TIMER_ABSTIME)`. (`sched_getaffinity` and `timerfd` exist.)
- **Tests:** musltest C programs, plus a `desktop-kernel` scenario (fd passing, memfd seals, inotify, a uevent on USB `device_add`, a VT switch with a DRM master).

### M37 — C++ and a software-rendered desktop

- **Toolchain:**
  - libc++/libc++abi/libunwind on musl, with clang (`tools/rustos-c++`);
  - meson/cmake cross files and a pkg-config wrapper (`tools/cross/`);
  - port recipe dependency ordering in `tools/install-port.sh`;
  - check that `x86_64-unknown-linux-musl` Rust binaries run unmodified (needed for NVK's NAK); any gaps become M36 items.
- **Ports, in order:**
  1. base: zlib, zstd, xz, libffi, expat, libxml2, pcre2, libpng, libjpeg-turbo, libwebp;
  2. text: freetype, harfbuzz, fribidi, fontconfig, DejaVu/Noto;
  3. 2D graphics: pixman, cairo, libdrm (`modetest`);
  4. Wayland and keyboard: wayland, wayland-protocols, libxkbcommon + xkeyboard-config;
  5. input and seats: libevdev, mtdev, libudev-zero, libinput, seatd/libseat;
  6. IPC: dbus.
- **First desktop:** Weston with the DRM backend and pixman renderer, `weston-terminal`, foot, and a Wayland backend for `browse -g`. It runs on efidrm on **any** GPU.
- **Software OpenGL:** Mesa softpipe for EGL/GLES.
- **Test:** a `desktop` scenario on `-vga std`/virtio-gpu (Weston up, terminal, `sendkey`, a `screendump` region check).

### Target hardware for M38/M39 (user's desktop `AriPC`)

| GPU | ID | Driver | Identity (6.18) | Firmware |
|---|---|---|---|---|
| AMD Raphael iGPU (RDNA2) | `1002:164e` | amdgpu | GC 10.3.6, DCN 3.1.5, PSP 13.0.5, SDMA 5.2.6, VCN 3.1.2 | `amdgpu/gc_10_3_6_{ce,pfp,me,mec,mec2,rlc}.bin`, `psp_13_0_5_{toc,ta}.bin`, `sdma_5_2_6.bin`, `dcn_3_1_5_dmcub.bin`, `vcn_3_1_2.bin` |
| NVIDIA RTX 5070 (Blackwell) | `10de:2f04` | nouveau | chipset `0x1b5` `GB205` (`gb202_disp`, `gb202_gsp`, `gb202_fsp`) | GSP-RM 570.144: `nvidia/gb205/gsp/{fmc,bootloader,gsp}-570.144.bin` |

**Development loop:**
- **nouveau through VFIO:**
  - the host runs on the Raphael iGPU and passes the RTX 5070 (`01:00.0` + its audio function `01:00.1`) to QEMU running RustOS;
  - host setup: IOMMU on in the BIOS, `amd_iommu=on iommu=pt`, both functions bound to `vfio-pci`;
  - deliverable: `tools/vfio-run.sh`.
- **amdgpu on bare metal** from the USB stick, with the monitor on the motherboard output and the iGPU enabled in the BIOS.

### M38 — AMD amdgpu (Raphael first)

- **Imported:** `drivers/gpu/drm/amd`:
  - `amdgpu` (355k), `display` (527k: DC, DMUB, DML/DML2 with FPU flags), `pm` (swsmu), `include`;
  - register headers only for enabled IP generations;
  - `amdkfd` excluded.
- **Steps:**
  1. **Display:**
     - IP discovery, VBIOS (`pci_map_rom`, `VFCT`/`ATRM`), GMC, IH, PSP + SMU firmware;
     - DC/DMUB: native modes, multiple monitors, hotplug, backlight.
     - **Result:** Weston at native resolution with real vblank.
  2. **Rendering:** GEM/TTM (VRAM/GTT), GPUVM, GFX/compute/SDMA rings, `drm_sched`, `AMDGPU_CS`/`WAIT_CS`/`GEM_*`/`INFO`/`CTX`, syncobj.
  3. **Power and video:** SMU DPM, GFXOFF; VCN decode later.
- **Shim additions:** `kernel_fpu_*`, `pci_map_rom`, ACPI `ATIF`/`ATCS`/`ATPX`, `i2c_adapter` (DDC/AUX), hwmon stubs, `mmu_notifier` stub, `dma_fence` chains, WC MMIO.
- **Firmware:** `write_to_drive.sh` copies the files `MODULE_FIRMWARE` lists for the detected GPU (or `--all-firmware`).
- **Tests:**
  - `hwcheck display`: modes, EDID, hotplug, page-flip timing histogram, backlight;
  - `hwcheck gpu`: firmware, ring tests, reset.

### M39 — NVIDIA nouveau (RTX 5070)

- **Imported:** `drivers/gpu/drm/nouveau` (225k) with `nvkm/subdev/gsp/rm/r535` and `r570`.
- **Boot sequence:** FSP/FMC boot (`gb202_fsp`), then GSP-RM RPCs for display (NVD5.0 display engine), memory and channels. Power and clocks are run by GSP.
- **NVK support:** VM_INIT/VM_BIND/EXEC, `drm_gpuvm`, syncobj.
- **Coverage:** older GPUs (Kepler–Ada) work by the same path. Turing+ use GSP; pre-Turing has display only, and Maxwell 2+ runs at boot clocks.
- **Development:** VFIO loop (above).
- **Tests:** `hwcheck display`/`gpu` as M38.

### M40 — Intel i915/xe

- **Drivers:** `drm/i915` (408k) for Gen9–Gen12; `drm/xe` (119k, shares i915 display) for Lunar Lake/Battlemage+.
- **Firmware:** GuC/HuC/DMC.
- **Order:** display first, then execbuf/vm_bind.

### M41 — Mesa

- **AMD:** RADV (ACO, no LLVM) and radeonsi. If radeonsi needs LLVM in the Mesa release, use zink over RADV for OpenGL until LLVM is ported.
- **NVIDIA:** NVK, with zink for OpenGL. Check GB20x support in the Mesa release at port time.
- **Intel:** iris/ANV.
- **QEMU:** virgl/venus (optional, CI).
- **Fallback:** softpipe/llvmpipe.
- **Window-system integration:** EGL (GBM, Wayland), Vulkan WSI.
- **Tests:** `kmscube`, `eglinfo`, `vulkaninfo`, `vkcube`, `glmark2-es2-wayland`.

### M42 — Desktop environments

1. **Weston** with the GL renderer.
2. **labwc (default) or Sway:** foot, fuzzel, waybar, mako, swaybg, swaylock, grim/slurp, wl-clipboard, kanshi.
3. **Xwayland:** xcb/X11 libraries, xkbcomp.
4. **GTK:** GLib, Pango, gdk-pixbuf, libepoxy, GTK 3/4; Thunar/PCManFM, Mousepad. Firefox is later and large; it runs with its sandbox off (no seccomp).
5. **Qt 6 / KDE Plasma:** D-Bus, polkit, a logind subset (`elogind`, or a native `login1` service), UPower, PipeWire.
6. **GNOME** needs systemd and is out of scope.

**Services:**
- **sessions:** seatd;
- **audio:** PipeWire + WirePlumber on ALSA (M34);
- **network:** wpa_supplicant plus a small NetworkManager D-Bus subset (`rustos-nmd`);
- **power:** UPower over `/sys/class/power_supply` (bridge ACPI battery/AC);
- **portals and notifications:** `xdg-desktop-portal-wlr`, mako.

**Test:** a `desktop-labwc` scenario on virtio-gpu (labwc, foot, a GTK app, screenshot check).

---

## 9. Firmware provisioning (`write_to_drive.sh`)

- **What to copy:** the build writes `target/firmware-list.txt` (all `MODULE_FIRMWARE` names plus the native lists). The script copies each listed file that exists on the host (`/lib/firmware`, `/usr/lib/firmware`, including `.zst`/`.xz`), for **hardware present** on the host by default (`/sys/bus/{pci,usb}/devices/*/modalias`).
- **Flags:**
  - `--all-firmware` copies every listed file;
  - `--firmware DIR` points at a linux-firmware checkout and supersedes `--ax210-firmware` (kept as an alias, plus `--mediatek-firmware`);
  - `--resolution WxH` sets the GOP mode.
- **Regulatory database:** `regulatory.db` is copied from wireless-regdb.
- **Unchanged behaviour:** the existing sudo/pinned-toolchain/piped-curl logic stays as it is.

## 10. Critical files

- **New:**
  - `third_party/linux/**`, `src/linuxkpi/**` (one module per §4 area);
  - `tools/linux-import.py`, `tools/linux-undefined.py`, `tools/vfio-run.sh`, `tools/rustos-c++`, `tools/cross/*`;
  - `ports/{libnl-tiny,wpa_supplicant,hostapd,…}`;
  - new scenarios in `tests/scenarios/`.
- **Modified:**
  - build: `build.rs` (C compilation, firmware list), `Cargo.toml` (features, `cc`/`bindgen`);
  - scheduler: `src/sched/mod.rs` (timer cancel, RCU quiescent hook, pub `arm_timeout`), `src/arch/x86_64/idt.rs` (IRQ depth);
  - memory: `src/mm/mod.rs` (VA allocator, MMIO cache types/unmap), `src/process/vm.rs` (`Backing::Device`, cached `Phys`), `src/syscall/mem.rs` + `src/vfs/mod.rs` (`FileLike::mmap`);
  - device plumbing:
    - PCI and probing: `src/pci/mod.rs` (driver registry), `src/drivers/mod.rs` (probe via registry, initcalls);
    - ACPI: `src/arch/x86_64/acpi.rs` (public eval/DSM/table);
    - files: `src/vfs/sysfs.rs` (registration tree), `src/vfs/devfs.rs`;
    - firmware: `src/firmware.rs` (`_nowait` retry, decompression);
  - networking: `src/net/{mod,socket,syscalls}.rs` (netlink, AF_PACKET, Unix dgram/SCM_RIGHTS, ioctls);
  - USB: `src/usb/{mod,xhci,btusb}.rs` (isoch IN, Linux binding, vendor BT firmware);
  - `src/drivers/input.rs` (Linux input bridge);
  - `src/drivers/framebuffer.rs` (DRM client handover);
  - `third_party/acpi/src/aml/resource.rs` (GPIO/I2C/SPI descriptors);
  - userland and scripts: `userland/nettools/src/wifi.rs` (wpa_supplicant client), `userland/rbox/src/hw.rs` (new hwcheck sections), `userland/init` / `/etc/rc`, `write_to_drive.sh`;
  - docs: `docs/ROADMAP.md`, `docs/ROADMAP-ROUND4.md` (replaced by this plan), `docs/HARDWARE.md`, `docs/LIMITATIONS.md`, new `docs/LINUXKPI.md`.

## 11. Verification

- **Every commit:** `cargo fmt --check`, `cargo clippy -- -D warnings`, host tests (`crates/*`, plus `src/linuxkpi` host tests), the touched scenarios.
- **Milestone end:** `tools/run-scenarios.sh` full suite plus `RUSTOS_SMP=4`, the release image boot on `-machine pc` and `q35` (as done in round 3), and `write_to_drive.sh` against a loop device.
- **New CI scenarios:**
  - M27: `eth-linux-e1000`;
  - M28: `wifi-hwsim`, `wifi-hwsim-eap`;
  - M30: `eth-usb-linux`;
  - M32: `eth-igb`;
  - M33: `sdcard`, `usb-serial`;
  - M34: `audio` (Linux);
  - M35: `drm-bochs`, `drm-virtio`, `drm-efidrm`;
  - M36: `desktop-kernel`;
  - M37: `desktop`;
  - M42: `desktop-labwc`.
- **Hardware:**
  - `hwcheck wifi` on the MT7921K laptop (M29) and an MT7921AU via passthrough (M30);
  - `hwcheck display`/`gpu` on AriPC: Raphael on bare metal (M38), RTX 5070 via VFIO (M39);
  - results are recorded in `docs/HARDWARE.md`.
- **Recommended test bench:** MT7921AU, MT7612U, RTL8821CU, AR9271, RTL8153 and AX88179 adapters, a UVC webcam, an FTDI cable, a used AX200/9260 card.

## 12. Risks

| Risk | Mitigation |
|---|---|
| Shim semantics differ from Linux (atomic sleeps, RCU, workqueue order, BH) | host tests per primitive; Linux e1000/igb/usb-net/hwsim as CI canaries; debug option asserting `might_sleep` in atomic context |
| Linux API churn | pinned LTS, yearly updates, minimal `PATCHES/`, undefined-symbol diff before updating |
| Build time/size (amdgpu ≈ 1.2M lines with headers) | per-feature Cargo flags, only enabled IP generations' headers, ccache |
| Missing PM/suspend/IOMMU | not needed for display/rendering; runtime PM always-on |
| Firmware absent on the host | generalized provisioning with `--firmware DIR`; `dmesg` names missing files |
| Blackwell support in nouveau/NVK is new | AMD iGPU first; VFIO loop for fast nouveau iteration |
| Desktop userland is hundreds of ports | M36 front-loads known kernel gaps; add `kernel.conf syscall.trace=<pid>` |

## 13. Progress

Track milestone status in [ROADMAP.md](ROADMAP.md) (round 4 checklist). M27 starts with the import tooling and the kernel C build.
