# Round 4 plan: Linux drivers, Wi-Fi on any card, everyday hardware, GPUs, desktop

RustOS is GPL-2.0-or-later, so the kernel can include Linux's GPL-2.0-only
code (the combined kernel is then distributed under GPLv2). This round uses
that: rather than rewriting each driver in Rust, RustOS gets a **LinuxKPI**
layer, a Rust implementation of the Linux kernel APIs that drivers call.
Linux driver sources are then compiled into the kernel mostly unmodified.
FreeBSD runs Intel, Realtek and MediaTek Wi-Fi and the AMD/Intel GPU drivers
the same way.

The RustOS core stays native Rust: memory management, scheduler, VFS and
filesystems, the TCP/IP stack, the xHCI host controller, the syscall
interface. Linux code is used for **device drivers and the subsystems built
around them** (802.11, DRM, HID, ALSA, V4L2, MMC), where the value is in tens
of thousands of chip-specific workarounds that cannot be rewritten
economically.

All sizes below are measured on Linux **6.18.54** (the current longterm
release, pinned for this round) and count `.c`/`.h` lines.

---

## 1. Goals

1. **Wi-Fi on nearly any card**, starting with the user's MediaTek
   MT7921K (RZ608, `14c3:0608`). Then every chip family Linux's mac80211
   drivers cover: Intel 7000→BE200, MediaTek, Realtek, Qualcomm Atheros, and
   Broadcom FullMAC. Includes USB adapters for machines whose internal card is
   unsupported.
2. **Everyday hardware**: USB and PCIe Ethernet adapters, I2C touchpads,
   SD card readers, USB serial adapters, webcams, laptop audio quirks and
   microphones, Bluetooth firmware for MediaTek/Realtek.
3. **Graphics a desktop environment can be ported to**:
   - Linux's DRM core and uAPI (`/dev/dri/card0`), on the firmware framebuffer for every machine (`efidrm`/`simpledrm`);
   - then **amdgpu** (AMD), **nouveau** (NVIDIA) and i915/xe (Intel);
   - then Mesa (OpenGL/Vulkan);
   - then Wayland compositors and desktop environments.

## 2. Decisions

| Decision | Choice | Why |
|---|---|---|
| Driver strategy | Compile Linux drivers through LinuxKPI | Linux carries the chip quirks and firmware handling; rewriting e.g. mt76 (115k lines) or amdgpu (880k) is not realistic |
| Linux version | 6.18 LTS, pinned (`v6.18.54` at import), updated deliberately | LTS gets fixes for years; one pinned API surface for the shim |
| Linux subsystems compiled as-is | mac80211, cfg80211, DRM core, TTM, GPU scheduler, dma-buf, HID core, input-mt, I2C core, gpiolib, MMC core, V4L2 core, videobuf2, ALSA core, usbnet, usb-serial | They are drivers' direct dependencies; compiling them is less work and fewer bugs than imitating them |
| Linux subsystems reimplemented in Rust (the shim) | memory (`kmalloc`, `struct page`), scheduler interface (`current`, wait queues, kthreads), locking, timers, workqueues, RCU, IRQs, PCI, USB core (on our xHCI), DMA mapping, firmware loading, netdev ↔ our network stack, input → our evdev, sysfs as a thin bridge, ACPI on our `acpi` crate | These are RustOS's own core; the shim adapts Linux's calling conventions to it |
| Wi-Fi userland | **wpa_supplicant** over nl80211 (cfg80211's netlink API); `wifi` command becomes its front-end | WPA2/WPA3/Enterprise, roaming, all tested against real APs for 20 years; `crates/wlan` stays for the native AX210 driver until it is retired |
| Kernel C compiler | clang/LLVM (Linux builds with `LLVM=1`) | Same toolchain family as rustc; LTO across C/Rust possible later |
| C++ | Userland only (Mesa ACO, HarfBuzz, Qt) | No Linux kernel code is C++ |
| Firmware | Never committed; `write_to_drive.sh` provisions it from the host's linux-firmware | Licences of firmware blobs differ from the GPL |
| Native drivers already written | Kept while the Linux equivalent is unproven on hardware; then retired when the Linux driver covers the same chips (iwlwifi, r8169, e1000e, igc, HDA) | Less code to maintain; Linux's versions handle far more chip variants |

**Alternatives considered:**
- **LKL** (Linux as a library) was rejected. It runs a second kernel (its own scheduler, memory and IRQ model) inside RustOS, and upstream LKL cannot drive PCI devices directly.
- **Nova** (the Rust NVIDIA driver, `drivers/gpu/nova-core`) is written against Linux's Rust `kernel` crate abstractions, not C APIs. In 6.18 it does not drive displays yet. It is a candidate for later (§9.6).
- **Rewriting drivers in Rust** stays the option for small devices where it is cheaper than the shim surface (Bluetooth firmware loaders, §8.4).

---

## 3. Milestones and order

| # | Milestone | Depends on | Testable in CI |
|---|---|---|---|
| **M27** | LinuxKPI foundation + Linux `e1000` in QEMU | — | yes |
| **M28** | Linux networking glue, cfg80211/mac80211, nl80211, wpa_supplicant, `mac80211_hwsim` | M27 | yes (hwsim) |
| **M29** | **MediaTek MT7921/MT7922 PCIe** (user's card) | M28 | host tests; hardware |
| **M30** | LinuxKPI USB core; MT7921AU; Linux `usbnet`/`cdc_ether` in QEMU; MediaTek BT firmware | M28 | yes (QEMU usb-net) |
| **M31** | Wi-Fi coverage: iwlwifi (all), rtw88, rtw89, mt76 family, mt7601u, ath9k/ath9k_htc, ath10k/11k/12k, brcmfmac | M29, M30 | hwsim regressions; hardware |
| **M32** | Ethernet coverage: r8152, AX88179/ASIX, igb, alx, tg3, atlantic, Linux r8169/e1000e/igc | M27, M30 | yes (`igb`, usb-net); hardware |
| **M33** | Laptop platform: I2C, GPIO, I2C-HID touchpads, HID core, MMC/SD, USB serial, UVC webcams, BT firmware | M27, M30 | partly (sdhci, usb-serial, uvc via passthrough) |
| **M34** | Linux sound: ALSA core, HDA with codec quirks, USB audio, SOF/ACP microphones, OSS emulation | M27, M30 | yes (HDA in QEMU) |
| **M35** | DRM core, dma-buf, `efidrm`/`simpledrm`, `bochs`, `virtio-gpu` | M27 | yes |
| **M36** | Desktop kernel features (Unix fd passing, memfd, inotify, uevents, VT switching, kernel FPU, …) | — | yes |
| **M37** | C++ toolchain, Wayland stack, software-rendered Weston desktop | M35, M36, M33 (touchpads) | yes |
| **M38** | AMD GPUs: amdgpu display, command submission, power | M35, M36 | hardware / VFIO |
| **M39** | NVIDIA GPUs: nouveau with GSP firmware | M35, M36 | hardware / VFIO |
| **M40** | Intel GPUs: i915 and xe | M35, M36 | hardware / VFIO |
| **M41** | Mesa: RADV/radeonsi, NVK, iris/ANV, zink, EGL/GBM | M37 + one of M38–M40 | virtio-gpu venus (optional) |
| **M42** | Desktop environments: Weston → labwc/Sway → Xwayland → GTK → Qt/KDE | M37, M41 | yes (Weston, labwc on virtio-gpu) |

```
M27 ─► M28 ─► M29 ─► M31
  │      └──► M30 ─┬► M31
  │                ├► M32
  │                ├► M33
  │                └► M34
  └──► M35 ─┐
     M36 ───┼─► M37 ─────────────┐
            └─► M38 / M39 / M40 ─► M41 ─► M42
```

**Recommended sequence:**
1. M27 → M28 → M29: the user's Wi-Fi first.
2. Then M30: the USB layer, which unlocks USB Wi-Fi, USB Ethernet, webcams and serial adapters.
3. Then M35/M36/M37: a desktop on every machine.
4. Then the GPU drivers for the user's GPUs.
5. M31–M34 run alongside, as hardware becomes available for testing.

---

## 4. Import and build infrastructure (part of M27)

### 4.1 Source layout

```
third_party/linux/
  VERSION                 # "v6.18.54" + commit id
  MANIFEST                # every imported file, its SPDX licence and sha256
  PATCHES/                # numbered patches against upstream (kept minimal)
  include/  lib/  kernel/locking/  drivers/...  net/...  sound/...
                          # imported files, same paths as in Linux
src/linuxkpi/             # the Rust shim (see §5)
  include/                # RustOS replacements for Linux headers
    asm/                  # our "arch" headers (percpu, current, irqflags, …)
    generated/autoconf.h  # generated from configs/rustos.config
  configs/rustos.config   # Kconfig fragment: everything RustOS compiles
tools/linux-import.py     # import/update tool
```

`tools/linux-import.py`:
- **Import:** takes a Linux checkout at the pinned tag and a list of driver groups (`mt7921`, `mac80211`, `amdgpu`, …). It copies the needed `.c`/`.h` files, closing over `#include`s, and records them in `MANIFEST`. Imported files are never edited in place; changes live in `PATCHES/`.
- **Kconfig:** generates `autoconf.h` by running Linux's own `scripts/kconfig/conf` with `ARCH=x86_64` and `configs/rustos.config` (`olddefconfig`) in a scratch Linux tree.
- **Linux updates:** with `--update vX.Y.Z` it re-imports, reapplies `PATCHES/` and reports conflicts. Updates happen at LTS point releases.

### 4.2 Compiling

- **Build glue:** `build.rs` compiles the imported files with **clang** through the `cc` crate into one static library per group (`liblinux_mac80211.a`, `liblinux_mt7921.a`, …), linked into the kernel. Only the groups for enabled features are built (Cargo features `linux-wifi`, `linux-drm-amd`, …), so kernel build time grows with what is enabled.
- **Flags**, mirroring Linux's x86_64 Kbuild with RustOS's code model:

  ```
  -std=gnu11 -nostdinc -isystem <clang resource dir>
  -include include/linux/compiler-version.h -include include/linux/kconfig.h
  -include src/linuxkpi/include/rustos-prelude.h
  -I src/linuxkpi/include -I third_party/linux/include -I third_party/linux/arch/x86/include
  -I third_party/linux/include/uapi -I third_party/linux/arch/x86/include/uapi
  -D__KERNEL__ -DMODULE=0 -DKBUILD_MODNAME='"<object>"' -DKBUILD_BASENAME=...
  -fno-common -fno-strict-aliasing -fno-delete-null-pointer-checks -fPIE -mcmodel=small
  -mno-red-zone -mno-sse -mno-mmx -mno-sse2 -mno-avx -msoft-float
  -fno-asynchronous-unwind-tables -fno-stack-protector -ffreestanding
  -Wno-pointer-sign -Wno-gnu -Wno-address-of-packed-member -Wno-unused-parameter
  ```

- **Code model:** the RustOS kernel is a position-independent executable (`relocation-model = pic`, `code-model = small`), while Linux assumes `-mcmodel=kernel`. The imported C is compiled `-fPIE -mcmodel=small`. Linux's x86 inline asm that hard-codes `-mcmodel=kernel` assumptions (percpu `%gs:` symbol offsets, `__ex_table` PC-relative entries) is excluded by providing our own `asm/percpu.h` and `asm/extable.h` (§5.2).
- **Floating point:** files that need it (amdgpu DML, some audio/DSP helpers) are compiled with `-msse2 -mhard-float` and must only run between `kernel_fpu_begin()` and `kernel_fpu_end()` (M36). The build keeps a per-file list, mirroring Linux's `CFLAGS_<file> = $(CC_FLAGS_FPU)`.
- **Section collection:** `module_init`/`module_exit`, `subsys_initcall` and the rest become entries in a linker section (`.linuxkpi_initcalls.<level>`). The shim runs them in Linux's level order at boot. `MODULE_*` macros compile to nothing except `MODULE_FIRMWARE`, which is collected into a table (`/proc/linux/firmware` lists what drivers may request).
- **Symbols:** the Rust side exports the shim with `#[no_mangle] extern "C"`. Imported C calls these, and imported C exports `EXPORT_SYMBOL`s to other C files directly. `bindgen` generates Rust declarations for the few C structures the shim reads (`struct pci_driver`, `struct net_device_ops`, `struct usb_driver`, …) from the real Linux headers, so layouts always match the imported code.
- **Undefined symbols:** a link-time check (`tools/linux-undefined.py`) lists every undefined symbol per group. That list is each milestone's work queue.

### 4.3 Licences

- **Recorded:** `MANIFEST` records each file's SPDX tag.
- **Allowed:** everything imported is GPL-2.0-only, GPL-2.0-or-later, GPL-2.0 WITH Linux-syscall-note, MIT, BSD or ISC. All are compatible with RustOS's GPL-2.0-or-later as a combined GPLv2 work.
- **Rejected:** the import tool refuses files without an SPDX tag, so nothing slips in unrecorded.
- **Firmware:** blobs are never imported.

---

## 5. LinuxKPI: what the shim implements (M27 core, extended per milestone)

Each item lists the Linux API surface, how RustOS provides it, and the
gotchas. Header-only APIs (`list.h`, `bitops.h`, `kernel.h`, `overflow.h`,
`string.h` helpers, most of `skbuff.h`'s inline functions) come straight
from Linux's headers.

### 5.1 Kernel-wide headers and `arch`

- **Linux x86 headers used as-is:**
  - `asm/atomic.h`, `asm/cmpxchg.h`, `asm/barrier.h`, `asm/bitops.h`, `asm/io.h` (`readl`/`writel`), `asm/msr.h`;
  - `asm/processor.h` (only `cpu_relax`), `asm/page_types.h`, `asm/string_64.h`, `asm/byteorder`, `asm/unaligned`.

  These are plain x86 instructions, valid in any x86_64 kernel.
- **RustOS replacements** in `src/linuxkpi/include/asm/`:
  - **per-CPU:** `percpu.h` uses per-CPU arrays indexed by `raw_smp_processor_id()` instead of `%gs:` offsets. RustOS's per-CPU block stays where it is.
  - **current task:** `current.h` — `current` is a `struct task_struct *` shadow object per RustOS thread.
  - **interrupt flags:** `irqflags.h` maps to RustOS `cli/sti` with nesting.
  - **memory map:** `page.h`, `pgtable_types.h` — `__va`/`__pa` through RustOS's physical-memory offset; `PAGE_OFFSET` as a variable.
  - **no-op or fallback headers:** `smap.h`, `paravirt.h` (none), `alternative.h` (no patching: always the baseline instruction), `jump_label.h` (no static keys: plain branches), `static_call.h` (indirect calls).
  - **exception tables:** `extable.h` — no `__ex_table`. `copy_to_user`/`get_user` go through the shim (§5.11).
  - **FPU:** `fpu/api.h` — `kernel_fpu_begin/end` (M36).
- **Kconfig choices** that keep the surface small: `CONFIG_SMP=y`, `NR_CPUS=64`, `CONFIG_PREEMPT_NONE=y` (Linux code then assumes no involuntary preemption inside spinlocks, which matches §5.4). These are disabled:
  - `CONFIG_DEBUG_*`, `CONFIG_LOCKDEP`, `CONFIG_KASAN`, `CONFIG_KCSAN`;
  - `CONFIG_TRACING`/tracepoints (compile to nothing), `CONFIG_DEBUG_FS` (stubs return errors), `CONFIG_PM_SLEEP` (until suspend exists);
  - `CONFIG_MODULES` (all built in), `CONFIG_SYSFS` (bridged instead, §5.12), `CONFIG_PARAVIRT`, `CONFIG_JUMP_LABEL`, `CONFIG_RETPOLINE`/mitigations.

### 5.2 Tasks, `current`, scheduling

- **`struct task_struct` shadow:**
  - allocated on first use for any RustOS thread that enters Linux code, freed when the thread exits;
  - fields drivers touch: `comm`, `pid`, `flags` (`PF_KTHREAD`, `PF_MEMALLOC`), `state`, `cpus_mask`, a pointer to the RustOS thread.
- **Sleeping:**
  - `schedule()`, `schedule_timeout*()`, `set_current_state()`, `__set_current_state()`, `wake_up_process()`, `cond_resched()`, `yield()`, `might_sleep()` (a debug check only);
  - all map to the RustOS scheduler's block/wake primitives.
  - **The race to avoid** is Linux's "set state, check condition, schedule" pattern. A wake-up between the check and `schedule()` must not be lost. RustOS fixed exactly this race in round 3, and the shim reuses that design: `wake_up_process` on a thread that is still `TASK_RUNNING` sets a pending-wake flag that makes the next `schedule()` return immediately.
- **Kernel threads:** `kthread_create`/`kthread_run`/`kthread_stop`/`kthread_should_stop`/`kthread_park` create RustOS kernel threads.
- **Wait queues and completions:**
  - `wait_queue_head_t`/`wait_event*`/`wake_up*` use Linux's own header macros on top of `prepare_to_wait`/`finish_wait`/`__wake_up` in the shim;
  - `completion` (`wait_for_completion*`, `complete*`) is implemented in the shim.
- **Locking:**
  - `spinlock_t`/`raw_spinlock_t` use Linux's `qspinlock` (`kernel/locking/qspinlock.c`, imported) with RustOS's IRQ save/restore and preemption-disable counter;
  - `rwlock_t` uses Linux's `qrwlock`;
  - `mutex`, `rw_semaphore` and `semaphore` are implemented in the shim on RustOS wait queues. Linux's versions depend on scheduler internals (optimistic spinning, wake_q);
  - `ww_mutex` (DRM/TTM) is implemented in the shim following `kernel/locking/ww_mutex.h`'s wait-die/wound-wait rules.
  - `lockdep_*` annotations compile to nothing.
- **Preemption counter:** `preempt_disable`/`enable`, `in_interrupt`, `in_atomic` and `irqs_disabled` are backed by RustOS's per-CPU preempt/IRQ counters. A RustOS spinlock holder already cannot be preempted; the shim exposes the counter.

### 5.3 Time

- **Clocks:**
  - `jiffies`/`jiffies_64` advance at `HZ=1000` from the RustOS timer tick (a per-tick increment, so the existing tick is enough);
  - `ktime_get*`, `ktime_get_boottime`, `ktime_get_real*` and `local_clock` come from the TSC clocksource RustOS already has.
- **Delays:** `msleep`, `usleep_range`, `udelay`/`ndelay`/`mdelay` (busy-wait on the TSC), `fsleep`.
- **Timers:** `timer_list` (`timer_setup`, `mod_timer`, `del_timer_sync`, `timer_delete*`) and `hrtimer` (`hrtimer_init`/`setup`, `hrtimer_start`, `hrtimer_cancel`, `HRTIMER_MODE_REL/ABS`) go into RustOS's timer wheel.
  - Callbacks run in a per-CPU softirq-like context: interrupts enabled, no sleeping, the same rule as Linux.
  - `del_timer_sync` waits for a running callback.

### 5.4 Deferred work and softirqs

- **Workqueues:**
  - `alloc_workqueue`/`alloc_ordered_workqueue`/`create_singlethread_workqueue`, `queue_work`/`queue_delayed_work`/`mod_delayed_work`, `flush_work`/`flush_workqueue`, `cancel_work_sync`/`cancel_delayed_work_sync`;
  - the system workqueues (`system_wq`, `system_highpri_wq`, `system_unbound_wq`, `system_long_wq`, `system_freezable_wq`).
  - Each workqueue is a pool of RustOS kernel threads; ordered ones have one. `WQ_MEM_RECLAIM` gets a reserved thread.
- **Tasklets and softirqs:** `tasklet_init`/`setup`/`schedule`/`kill` and `local_bh_disable`/`enable`. A per-CPU "softirq" kernel thread runs tasklets and NET_RX work. `local_bh_disable` marks the CPU so that work waits.
  - mac80211 relies on `local_bh_disable` to serialize RX against TX status. On RustOS that becomes a per-CPU counter that holds off the softirq thread on that CPU.
- **NAPI:** `netif_napi_add`, `napi_schedule`, `napi_complete_done`, `napi_gro_receive` (no GRO: passes through), `napi_disable`/`enable`. The poll functions run in the softirq thread with a budget of 64.

### 5.5 RCU

mac80211, cfg80211 and DRM use RCU heavily. A simple, correct implementation is enough:

- **Read side:** `rcu_read_lock` = `preempt_disable`. The `rcu_dereference*` and `rcu_assign_pointer` macros come from Linux's headers.
- **Waiting for readers:** `synchronize_rcu` waits until every CPU has passed a quiescent state. That is a context switch, the idle loop, or a return to user mode, tracked with a per-CPU counter RustOS bumps in the scheduler. `synchronize_rcu_expedited` sends an IPI to speed it up.
- **Deferred freeing:** `call_rcu`, `kfree_rcu` and `rcu_barrier` queue callbacks to an RCU kernel thread, which runs them after a grace period.
- **Variants:**
  - `srcu_*` (used by DRM): per-`srcu_struct` counters with the classic two-index algorithm;
  - `rcu_read_lock_bh`/`sched` are aliases;
  - `list_*_rcu`, `hlist_*_rcu` and RCU-protected `idr`/`xarray` come from the headers.

### 5.6 Memory

- **Small allocations:**
  - `kmalloc`, `kzalloc`, `kcalloc`, `krealloc`, `kfree`, `kmalloc_array`, `kvmalloc`/`kvfree`, `kmemdup`, `kstrdup`, `devm_kmalloc` (§5.12);
  - `kmem_cache_*` (a fixed-size wrapper);
  - all on the RustOS kernel heap. `GFP_KERNEL` may sleep; `GFP_ATOMIC` must not, which comes from a reserve pool.
  - `GFP_DMA32` allocations come from the RustOS DMA allocator's below-4-GiB zone. `__GFP_ZERO`, `__GFP_NOWARN` and `__GFP_RETRY_MAYFAIL` are honoured.
- **Large allocations:** `vmalloc`/`vzalloc`/`vfree`/`vmap`/`vunmap` map non-contiguous frames into a kernel virtual range (RustOS's MMIO mapper area).
- **`struct page` and the memory map:**
  - DRM/TTM, `skb` fragments, scatterlists and `dma_map_page` all need `struct page`.
  - The shim allocates a **`mem_map` array** of `struct page` covering all physical RAM at boot: 64 bytes per 4 KiB page, so 1.6% of RAM. `pfn_to_page`/`page_to_pfn`/`page_address` are array arithmetic. RustOS's frame allocator keeps its own metadata; `struct page` holds only the Linux-visible state.
  - **Allocation:** `alloc_pages`/`__free_pages`/`get_page`/`put_page`/`page_ref_*`.
  - **Mapping:** `kmap`/`kmap_local_page` (no highmem: `page_address`).
  - **Folios:** the `folio` wrappers DRM 6.18 uses (`folio_get`, `page_folio`, …) map onto single pages or compound pages.
- **Scatterlists:** `sg_alloc_table`, `sg_set_page`, `for_each_sgtable_*`, `sg_alloc_table_from_pages` from `lib/scatterlist.c` (imported).
- **MMIO:** `ioremap`/`ioremap_wc`/`ioremap_uc`/`iounmap`, `memremap`, `pci_iomap`, `devm_ioremap*` use RustOS MMIO mapping with the PAT memory types RustOS already programs (UC, WC). `io_mapping_*` and `arch_phys_wc_add` are needed for GPU BARs.
- **User-space mappings** (needed by DRM and V4L2 `mmap`):
  - `struct vm_area_struct` is a shadow built when a Linux driver's `file_operations.mmap` is called;
  - `remap_pfn_range`, `io_remap_pfn_range`, `vm_insert_page`, `vmf_insert_pfn(_prot)` and `vm_map_pages` populate RustOS page tables;
  - a `vm_operations_struct.fault` handler is called from RustOS's page-fault handler for VMAs owned by a Linux driver.
  - RustOS's `mmap` path gains an "owned by a device file" VMA type for this.
- **Shared memory objects (`shmem`):**
  - DRM GEM objects for many drivers use `shmem_file_setup` and `shmem_read_mapping_page`. The shim backs them with the RustOS page cache's anonymous-memory objects (memfd, M36).

### 5.7 Interrupts

- **Handlers:** `request_irq`, `request_threaded_irq`, `devm_request_*irq`, `free_irq`, `disable_irq`/`enable_irq`/`disable_irq_nosync`, `synchronize_irq`.
  - IRQs come from RustOS's IOAPIC/MSI/MSI-X allocation.
  - Threaded handlers get a RustOS kernel thread. `IRQF_SHARED` chains handlers. `IRQF_ONESHOT` masks the line until the thread finishes.
- **PCI MSI/MSI-X:** `pci_alloc_irq_vectors` (`PCI_IRQ_MSIX | PCI_IRQ_MSI | PCI_IRQ_INTX`) and `pci_irq_vector` use RustOS's existing MSI/MSI-X code. Legacy INTx is routed through `_PRT` (fixed in round 3).

### 5.8 DMA

- **Mapping:** `dma_alloc_coherent`/`dma_free_coherent`, `dmam_alloc_coherent`, `dma_map_single`/`page`/`sg`/`sgtable`, `dma_unmap_*`, `dma_sync_*_for_cpu`/`device`, `dma_set_mask_and_coherent`, `dma_pool_*`.
  - There is no IOMMU, so bus address = physical address. x86 is cache-coherent, so syncs are barriers only.
  - **Devices with a 32-bit DMA mask** (some USB and Wi-Fi chips) get coherent memory below 4 GiB. Streaming mappings of buffers above 4 GiB are bounced through a small **swiotlb**-style pool (Linux's `kernel/dma/swiotlb.c` logic, reimplemented).
- **DMA-BUF:** `dma-buf` itself is imported (`drivers/dma-buf`, 10k lines: `dma-buf.c`, `dma-fence*.c`, `dma-resv.c`, `sync_file.c`). Its file descriptors become RustOS file objects through the shim's `anon_inode_getfile` (§5.11).

### 5.9 PCI

- **Driver structure:** `struct pci_dev` is allocated by the shim for each RustOS PCI device (vendor/device/subsystem IDs, class, revision, BARs, IRQ pin, `dev` with `dma_mask`, parent bus).
  - `pci_register_driver` matches `id_table` entries against RustOS's device list (the existing PCI driver registry gains a "Linux driver" kind) and calls `probe`.
- **Config space and resources:**
  - `pci_read/write_config_*`, `pcie_capability_*`, `pci_find_capability`/`ext_capability`;
  - `pci_enable_device`/`_mem`, `pci_set_master`, `pci_request_regions`, `pci_resource_start/len/flags`, `pcim_*` managed variants;
  - `pci_save/restore_state` (no suspend yet: stubs).
- **Power and link control:** `pci_set_power_state` (D0 only), `pci_disable_link_state` (ASPM: RustOS leaves ASPM as the firmware set it; the call is recorded). `pcie_get_speed_cap`/`width_cap` are used by amdgpu.
- **Reset:** `pci_reset_function`, `pcie_flr`, and secondary bus reset for GPUs. Needed for driver retries and VFIO testing.
- **Firmware-provided ROMs:** `pci_map_rom` for the AMD/NVIDIA VBIOS. ACPI `VFCT`/`ATRM` is used as a fallback (amdgpu has that code; the shim provides `acpi_get_table`).

### 5.10 Firmware loading

- **API:** `request_firmware`, `request_firmware_nowait`, `firmware_request_nowarn`, `request_firmware_direct`, `release_firmware`, `firmware_request_platform`.
- **Search path:** RustOS's existing firmware directories (`/lib/firmware`, `/storage/lib/firmware`, `/boot/efi/firmware`, …).
- **Compression:** `.zst` and `.xz` variants are decompressed in the kernel (`zstd`/`xz` decoders imported from Linux `lib/zstd`, `lib/xz`). Distributions ship compressed firmware, and `write_to_drive.sh` can then copy files as-is.
- **Asynchronous requests:** `_nowait` requests retry for 60 s after boot, which covers the storage partition being mounted after drivers probe. This replaces the iwlwifi-specific retry RustOS has now.

### 5.11 Character devices, files, user memory

- **Device nodes:**
  - `register_chrdev`/`cdev_add`/`alloc_chrdev_region`, `misc_register`, `class_create`/`device_create` (creates `/dev` nodes in RustOS devfs);
  - `struct file_operations` (`open`/`release`/`read`/`write`/`poll`/`unlocked_ioctl`/`compat_ioctl`/`mmap`/`llseek`/`fasync`) are called from RustOS's VFS through a "Linux cdev" inode type.
  - `struct file`/`struct inode` shadows carry `private_data`, `f_flags` and `f_mode`.
- **Anonymous fds:** `anon_inode_getfile`/`anon_inode_getfd`, `get_unused_fd_flags`, `fd_install`, `fget`/`fput`, `fdget`. Used by dma-buf, sync_file and DRM syncobj.
- **User memory:** `copy_to_user`, `copy_from_user`, `get_user`/`put_user`, `strncpy_from_user`, `clear_user`, `access_ok`, `memdup_user`. These use RustOS's existing user-copy routines (fault-safe).
- **poll:** `poll_wait`/`poll_table` maps to RustOS's per-object wait queues (round 3 M17).

### 5.12 Device model, sysfs

- **Device core from Linux:** `drivers/base` is imported (`core.c`, `bus.c`, `driver.c`, `dd.c`, `devres.c`, `class.c`, `platform.c`, `component.c`, `property.c`, `swnode.c`), with `kobject` from `lib/kobject.c`. This gives authentic probe ordering, deferred probing, `devm_*` resource management, `device_link`, components (used by DRM/ALSA) and firmware-node properties. **kernfs/sysfs** is not imported; instead:
  - `sysfs_create_group`/`file`/`link` and `device_create_file` register attributes with a **RustOS sysfs bridge**. It exposes them under `/sys/devices/...` and `/sys/class/<class>/...`, which RustOS's `/sys` already has.
  - The `show`/`store` callbacks are called on read and write. Desktop userland needs these (DRM connectors, backlight, input devices, net interfaces, M36).
- **Runtime PM:** `pm_runtime_*` (`get_sync`, `put_autosuspend`, `enable`, `use_autosuspend`, …) are implemented as "always active" first. Real runtime suspend comes later for laptop power. System suspend is out of scope for this round.

### 5.13 ACPI, platform and firmware nodes

- **ACPI from RustOS:** the shim implements `acpi_evaluate_object`, `acpi_evaluate_integer`, `acpi_evaluate_dsm`/`acpi_check_dsm`, `acpi_get_handle`, `acpi_dev_*`, `acpi_get_table`, `ACPI_COMPANION`, and the `acpi_device` match tables used by I2C-HID, GPIO and the platform drivers. They run on RustOS's AML interpreter (the vendored `acpi` crate).
  - ACPICA is not imported; it would duplicate the AML interpreter.
- **Resource descriptors:** `_CRS` resources (`Memory32Fixed`, `Interrupt`, `GpioInt`, `GpioIo`, `I2cSerialBus`, `SpiSerialBus`) need parsers in `third_party/acpi` (now `LibUnimplemented`) for M33.
- **Platform devices:** ACPI devices with `_HID`s that Linux platform drivers match (`AMDI0010` I2C, `AMDI0030` GPIO, `PNP0C50` I2C-HID) become `platform_device`s / I2C clients.

### 5.14 Networking glue (M28)

- **Network interfaces:** `struct net_device` from `alloc_netdev_mqs`/`alloc_etherdev`, `register_netdev`/`unregister_netdev`, `netif_carrier_on`/`off`, `netif_start/stop/wake_queue` and per-queue variants, `dev_addr_set`/`eth_hw_addr_set`, `netdev_priv`, `ndo_*` callbacks.
  - Each registered `net_device` becomes a RustOS `NetDevice`. Transmit calls `ndo_start_xmit` with an skb; receive (`netif_rx`, `netif_receive_skb`, `napi_gro_receive`) hands frames to RustOS's stack.
  - **Carrier changes** start or stop RustOS's DHCP client, as they do for native drivers today.
- **`sk_buff`:** the struct and inline helpers come from `include/linux/skbuff.h`. The shim implements the out-of-line core:
  - `__alloc_skb`, `__netdev_alloc_skb`, `dev_alloc_skb`, `kfree_skb`, `consume_skb`, `dev_kfree_skb_any`;
  - `skb_clone`, `skb_copy`, `pskb_expand_head`, `skb_put`/`push`/`pull` failure paths, `skb_copy_bits`, `skb_linearize`;
  - frags with `page_frag`, `skb_queue_*` with locking;
  - `skb_tx_timestamp` (no-op), checksum helpers (`skb_checksum_help`).
  - Linux's `net/core/skbuff.c` (7,000 lines) is not imported; it drags in socket accounting and page_pool.
- **Netlink:**
  - `AF_NETLINK` sockets are implemented in RustOS (Rust): `NETLINK_ROUTE` (links, addresses, routes: enough for `ip` and wpa_supplicant), `NETLINK_GENERIC`, `NETLINK_KOBJECT_UEVENT` (M36).
  - Generic netlink family registration (`genl_register_family`, `genlmsg_*`, `nla_*`) is provided for C users. `lib/nlattr.c` is imported; `net/netlink/genetlink.c` is reimplemented in Rust over RustOS netlink, about 600 lines of logic.
- **`AF_PACKET`** (`SOCK_RAW`/`SOCK_DGRAM`, protocol filter, `PACKET_ADD_MEMBERSHIP`) in Rust. Used by wpa_supplicant for EAPOL on drivers without control-port-over-nl80211, by DHCP clients, and by `tcpdump`.
- **ethtool:** `ethtool_ops` get link settings, driver info and stats through a RustOS `SIOCETHTOOL` path. Optional, for `hwcheck`.
- **Crypto API** (mac80211, cfg80211; iwlwifi/mt76 offload crypto to hardware but mac80211 keeps software paths):
  - `crypto_alloc_aead("ccm(aes)"|"gcm(aes)")`, `crypto_alloc_shash("cmac(aes)")`, `crypto_alloc_skcipher("ctr(aes)")` (FILS), `gmac(aes)`, `arc4` (WEP/TKIP), `michael_mic` (in cfg80211), `crc32_le`;
  - the shim implements this subset of the crypto API in Rust using the RustCrypto crates RustOS already uses (`aes`, `ccm`, `aes-gcm`, `cmac`, `ctr`).
  - Linux's `crypto/` (110k lines) is not imported.

### 5.15 USB core (M30)

Linux USB drivers run on RustOS's xHCI driver:

- **Devices and interfaces:** `struct usb_device` and `struct usb_interface` (descriptors from RustOS's enumeration), `usb_register_driver` matching on `usb_device_id` tables (vendor/product, class, interface info). The RustOS USB core gains a "Linux driver" class binding next to its native class drivers.
- **URBs:** `usb_alloc_urb`, `usb_fill_*_urb`, `usb_submit_urb`, `usb_kill_urb`, `usb_poison_urb`, `usb_free_urb`, anchors (`usb_anchor_urb`, `usb_kill_anchored_urbs`, `usb_wait_anchor_empty_timeout`).
  - Control, bulk, interrupt and isochronous URBs become RustOS transfer requests. Completions run in the softirq thread, as in Linux.
  - Bulk streams use RustOS's existing UAS stream support.
- **Synchronous helpers and device control:** `usb_control_msg`, `usb_bulk_msg`, `usb_interrupt_msg`; `usb_set_interface`, `usb_clear_halt`, `usb_reset_device`, `usb_reset_endpoint`, `usb_driver_claim_interface`, `usb_ifnum_to_if`, `usb_get_dev`/`put_dev`, `usb_autopm_*` (no-ops).
- **Isochronous IN:** needed by UVC and USB audio capture. Round 3 only did OUT; M30 adds IN to xHCI.

### 5.16 Input, HID, I2C, GPIO (M33)

- **Input:** `input_allocate_device`, `input_register_device`, `input_set_abs_params`, `input_event`/`input_report_*`/`input_sync`, `input_set_capability`, with `drivers/input/input-mt.c` imported for multitouch slots.
  - Each Linux `input_dev` becomes a RustOS evdev node, so the existing `/dev/input/eventN` code and its `EVIOCG*` ioctls serve both native and Linux drivers.
- **HID:** the core is imported (`drivers/hid/hid-core.c`, `hid-input.c`, `hid-quirks.c`, `hid-generic.c`, `hid-multitouch.c`, plus vendor drivers on demand), with `i2c-hid` (`i2c-hid-core.c`, `i2c-hid-acpi.c`).
  - USB HID stays native unless a device needs a Linux quirk driver. Later, `usbhid` itself may be imported to unify.
- **I2C:** `drivers/i2c/i2c-core-*.c` imported (with `i2c-core-acpi.c` on the shim's ACPI). Controllers: `i2c-designware-platdrv`, `-core`, `-master`, `-common`, `-amdpsp`, `-pcidrv` (Intel LPSS).
- **GPIO:** `drivers/gpio/gpiolib.c`, `gpiolib-acpi-core.c`, `gpiolib-cdev.c` (optional) and `pinctrl-amd.c`. The Intel pinctrl drivers (`pinctrl-intel.c` plus per-chipset files: Tiger Lake, Alder Lake, Meteor Lake) come next. `pinctrl` core is imported as needed.
  - GPIO interrupts are chained through the shim's IRQ domain (`irq_domain_*`, `generic_handle_irq`, `irq_chip`), a small genirq subset.

### 5.17 Shim size estimate

About 15–25k lines of Rust plus about 3k lines of C replacement headers,
built up over M27–M35. FreeBSD's equivalent (`sys/compat/linuxkpi`,
including the 802.11 layer RustOS does not need because it compiles
mac80211) is of the same order.

---

## 6. M27 — LinuxKPI foundation

**Deliverables:**
- **Import and build infrastructure:** `third_party/linux` at `v6.18.54`, `tools/linux-import.py`, the `MANIFEST` licence check, `configs/rustos.config`, `build.rs` C compilation, and the initcall and symbol checks (§4).
- **Shim core:** §5.1–5.12, enough for a PCI network driver.
- **Proof driver:** Linux `drivers/net/ethernet/intel/e1000/` (5 files, about 9k lines) compiled through the shim, probing QEMU's `-device e1000` in place of RustOS's native e1000 (Cargo feature `linux-e1000`).
  - It uses PCI, BAR mapping, MSI/INTx, DMA coherent and streaming mappings, NAPI, netdev, skb, timers, workqueues, ethtool and firmware-free probing. That is most of what Wi-Fi drivers need, testable in CI.
- **Diagnostics:**
  - `dmesg` shows Linux `printk`/`dev_*`/`netdev_*` output with the right levels.
  - The `%p` extensions drivers use (`%pM`, `%pI4`, `%pI6c`, `%pa`, `%pad`, `%pe`, `%*ph`, `%pS`, `%pV`, `%pOF`) are handled by the shim's `vsnprintf`. Linux's `lib/vsprintf.c` is imported with the symbol-lookup parts stubbed.
  - `WARN_ON` prints a backtrace (RustOS's unwinder over frame pointers) and continues. `BUG_ON` panics.

**Tests:**
- **`eth-linux-e1000` scenario:** the native `eth-e1000` scenario run against the Linux driver (DHCP, ping, 4 MiB TCP download with checksum).
- **`linuxkpi` host tests:** RCU grace periods, workqueue flush ordering, timer cancellation races, `ww_mutex` deadlock avoidance, skb operations. These run as Rust unit tests with the shim compiled for the host.
- **`kapitest` additions:** a C test module (built only in test images) exercising wait queues, completions, kthreads and `hrtimer` in the kernel.

**Size:** large, the biggest single milestone in the round; everything later reuses it.

## 7. Wi-Fi

### 7.1 M28 — cfg80211, mac80211, nl80211, wpa_supplicant, hwsim

**Imported:**
- `net/wireless` (cfg80211, 55k lines). From its Makefile: `core`, `sysfs`, `radiotap`, `util`, `reg`, `scan`, `nl80211`, `mlme`, `ibss`, `sme`, `chan`, `ethtool`, `mesh`, `ap`, `ocb`, `michael-mic`, `pmsr`.
  - `CONFIG_CFG80211_WEXT=n`.
  - `CONFIG_CFG80211_REQUIRE_SIGNED_REGDB=n`: `regulatory.db` is loaded unsigned from the firmware directory, and `write_to_drive.sh` provisions it from the host (`wireless-regdb`).
- `net/mac80211` (90k lines): `main`, `status`, `driver-ops`, `sta_info`, `wep`, `aead_api`, `wpa`, `scan`, `offchannel`, `ht`, `agg-tx`, `agg-rx`, `vht`, `he`, `eht`, `s1g`, `ibss`, `iface`, `link`, `rate`, `tkip`, `aes_cmac`, `aes_gmac`, `fils_aead`, `cfg`, `ethtool`, `rx`, `spectmgmt`, `tx`, `key`, `util`, `parse`, `wme`, `chan`, `mlme`, `tdls`, `ocb`, `airtime`.
  - Plus the `minstrel_ht` rate control, which host-rate-control chips need (mt7601u, rtw88 USB, ath9k).
- `drivers/net/wireless/virtual/mac80211_hwsim.c`: a software radio pair.

**RustOS work:**
- **Networking glue** (§5.14): netdev, skb, NAPI, netlink/genetlink, AF_PACKET, the crypto subset.
- **Interface flags and ioctls:** `SIOCGIFFLAGS`/`SIOCSIFFLAGS` (interface up/down), `SIOCGIFINDEX`, `SIOCGIFHWADDR`; RTNETLINK `RTM_NEWLINK`/`GETLINK`/`SETLINK`, operstate, `IFF_LOWER_UP`.
- **wpa_supplicant port** (current release) (`ports/wpa_supplicant`, BSD licence):
  - nl80211 driver with `libnl-tiny`, control interface over `AF_UNIX` `SOCK_DGRAM`, `CONFIG_SAE=y`, `CONFIG_IEEE80211W=y`, `CONFIG_OWE=y`;
  - EAP methods (PEAP, TTLS, TLS) are built too: WPA-Enterprise comes for free.
  - The TLS backend is OpenSSL or wolfSSL, ported as a dependency.
- **hostapd** (same source tree) for the test AP on hwsim.
- **`wifi` command rewrite:** the same commands (`scan`, `connect`, `--save`, `forget`, `auto`, `power`), implemented by talking to wpa_supplicant's control socket (`SCAN`, `SCAN_RESULTS`, `ADD_NETWORK`, `SET_NETWORK`, `SELECT_NETWORK`, `SAVE_CONFIG`, `STATUS`).
  - `/storage/etc/wifi.conf` stays the saved-network file. `init` generates `wpa_supplicant.conf` from it, or `wifi` migrates it.
  - `/etc/rc` starts `wpa_supplicant -i wlan0 -c ... -B` when a wireless interface exists.
- **Native AX210 driver:** stays the default for `8086:2725` until the Linux iwlwifi path (M31) passes `hwcheck` on hardware. Then native iwlwifi and `crates/wlan` are retired.

**Tests:**
- **`wifi-hwsim` scenario** (CI): two hwsim radios; hostapd runs on `wlan1` with WPA2-PSK, then with WPA3-SAE (H2E) and PMF; `wifi connect` on `wlan0`.
  - Asserts association, DHCP over the hwsim link (hostapd side runs a DHCP server from busybox `udhcpd`), and ping.
  - Then: a group rekey (`wpa_group_rekey=5`), disconnect and reconnect, roaming between two APs with the same SSID.
- **WPA-Enterprise:** PEAP-MSCHAPv2 against hostapd's internal EAP server.

### 7.2 M29 — MediaTek MT7921/MT7922 (PCIe): the user's card

**Imported** (`drivers/net/wireless/mediatek/mt76`, ISC licence):
- `mt76` core: `mmio`, `util`, `dma`, `mac80211`, `eeprom`, `tx`, `agg-rx`, `mcu`, `scan`, `channel`, `pci` (`trace`, `debugfs`, `wed`, `npu`, `testmode` excluded by Kconfig).
- `mt76-connac-lib`: `mt76_connac_mcu`, `mt76_connac_mac`, `mt76_connac3_mac`.
- `mt792x-lib`: `mt792x_core`, `mt792x_mac`, `mt792x_dma`, `mt792x_acpi_sar` (`ACPI` SAR power tables via the shim's ACPI).
- `mt7921`: `mac`, `mcu`, `main`, `init` (common) and `pci`, `pci_mac`, `pci_mcu` (`mt7921e`).
- `mt7925` (Wi-Fi 7, MT7925 / RZ717) can come along cheaply: same libraries, 11k more lines.

**Devices** (from `mt7921/pci.c`): `14c3:7961` (MT7921), `14c3:0608` (MT7921K/RZ608, the user's), `14c3:7922` and `0b48:7922` (MT7922), `14c3:0616` (RZ616), `14c3:7920` (MT7920).

**Firmware** (`mt792x.h`):
- MT7921: `mediatek/WIFI_MT7961_patch_mcu_1_2_hdr.bin`, `mediatek/WIFI_RAM_CODE_MT7961_1.bin`.
- MT7922: `mediatek/WIFI_MT7922_patch_mcu_1_1_hdr.bin`, `mediatek/WIFI_RAM_CODE_MT7922_1.bin`.
- MT7920: `mediatek/WIFI_RAM_CODE_MT7961_1a.bin`.
- `write_to_drive.sh` gains `--mediatek-firmware DIR` and auto-detection. Its firmware logic generalizes to "copy every file any compiled driver lists in `MODULE_FIRMWARE`, if present on the host" (§10).

**Shim needs beyond M27/M28:**
- ACPI `_DSM` for SAR tables;
- `pci_disable_link_state` (the driver disables ASPM L0s/L1 on some systems);
- `ieee80211_hw` registration (from mac80211);
- the `mt76` DMA ring and wake handling, which uses `napi` and `tasklet` heavily.

**Bring-up procedure on the user's laptop:**
1. Build with `--features linux-wifi` (default once stable) and flash with `write_to_drive.sh`, which copies the MediaTek firmware.
2. Boot and check `dmesg`: probe, `ASIC revision`, firmware load (`HW/SW Version`, `WM Firmware Version`), `wlan0` registered.
3. Run `hwcheck wifi` (scan, WPA2, WPA3, DHCP, ping, HTTP, rekey wait, reconnect) and send the result folder.
4. `kernel.conf` switches for iterations:
   - `linux.debug=mt76` raises the module's `dev_dbg` output;
   - `mt7921e.disable_aspm=1`;
   - `mt76.disable_usb_sg=1` (for M30).

**Size:** small to medium of new work once M28 exists. The driver is imported, not written. Most effort is debugging shim behaviour on real hardware.

### 7.3 M30 — USB core shim, MT7921AU, Linux usbnet, MediaTek Bluetooth

- **USB shim:** the LinuxKPI USB core (§5.15) plus xHCI isochronous IN.
- **CI proof:** Linux `usbnet` + `cdc_ether` + `rndis_host` + `cdc_ncm` driving QEMU's `-device usb-net`. The same scenario as RustOS's native `eth-usb`, validating URBs, bulk/interrupt pipes and netdev on USB.
- **MT7921AU:**
  - imported: `mt76-usb` (`usb.c`), `mt792x-usb` and `mt7921/usb.c`;
  - devices: `0e8d:7961`, `3574:6211`, `0846:9060` (Netgear A8000), `0846:9065`, `35bc:0107` (Alfa AWUS036AXML and similar);
  - it reuses all of M29.
  - This is the **recommended "Wi-Fi anywhere" adapter**: Wi-Fi 6E and widely sold.
- **MediaTek Bluetooth:** RustOS keeps its native Bluetooth stack (round 3), so `btmtk`'s firmware download is **translated** into the native `btusb` driver rather than compiled. It is about 400 lines and depends on Linux's `hci_dev` internals.
  - The flow: WMT patch download in sections, function enable, then a reset.
  - Firmware: `mediatek/BT_RAM_CODE_MT7961_1_2_hdr.bin` (MT7921), `BT_RAM_CODE_MT7922_1_1_hdr.bin` (MT7922), `mt7925/BT_RAM_CODE_MT7925_1_1_hdr.bin` (MT7925).
- **Test:**
  - CI: usb-net through Linux usbnet.
  - Hardware or QEMU USB passthrough (`-device usb-host,vendorid=0x0e8d,productid=0x7961`) for MT7921AU on a developer machine.

### 7.4 M31 — Wi-Fi coverage

Import order (by how common the hardware is, and shared code):

| Driver | Lines (6.18) | Chips | Firmware dir | Notes |
|---|---|---|---|---|
| `iwlwifi` (+ `mvm`, `mld`) | 180k | Intel 7260/3160/7265 → 8260/8265 → 9260/9560 → AX200/AX201/AX210/AX211 → BE200/BE201 | `iwlwifi-*.ucode`, `.pnvm` | `mld` is chosen automatically for Wi-Fi 7 devices with firmware API ≥ 97; `dvm` (pre-7000) excluded; replaces native AX210 driver after `hwcheck` passes |
| `rtw88` | 162k | RTL8821CE/8822BE/8822CE/8723DE (PCIe), RTL8821CU/8822BU/8822CU/8723DU/8811CU (USB) | `rtw88/*.bin` | common in budget laptops and cheap USB adapters |
| `rtw89` | 291k | RTL8852AE/BE/CE, 8851BE, 8922AE (Wi-Fi 7), USB variants | `rtw89/*.bin` | larger; after rtw88 |
| `mt76` remainder | (115k total) | MT7612U/MT7632U (`mt76x2u`), MT7610U (`mt76x0u`), MT7663 (`mt7615`), MT7915/MT7916 (`mt7915`), MT7925 | `mediatek/*` | shares core with M29 |
| `mt7601u` | 8k | MT7601U USB (very cheap 802.11n) | `mt7601u.bin` | host rate control (minstrel) |
| `ath9k` + `ath9k_htc` | 88k | AR9xxx PCIe, AR9271/AR7010 USB | `ath9k_htc/htc_9271-1.4.0.fw` (open firmware) | |
| `ath10k` (pci) | 85k | QCA6174, QCA9377 (many 2016–2020 laptops) | `ath10k/*/board-2.bin`, `firmware-6.bin` | |
| `ath11k` (pci) + MHI + QMI | 81k + `drivers/bus/mhi`, `net/qrtr`, `drivers/soc/qcom/qmi*` | WCN6855 / QCA2066 / QCA6390 (many 2020+ laptops) | `ath11k/*` | needs the MHI bus and QRTR protocol: imported too |
| `ath12k` | 91k | WCN7850 (Wi-Fi 7 laptops) | `ath12k/*` | after ath11k |
| `brcmfmac` | 40k | Broadcom BCM43xx FullMAC (PCIe/USB) | `brcm/*` | FullMAC: uses cfg80211 directly, no mac80211 |

- **Per driver:** import it, resolve its undefined symbols in the shim, add the PCI/USB IDs to `hwcheck`'s known list, add its firmware to provisioning, and run a regression on `wifi-hwsim`. Shim changes must not break other drivers.
- **Each driver's acceptance:** a `hwcheck wifi` pass on at least one device, recorded in `docs/HARDWARE.md`. Without a hardware report it is listed as "compiled, untested".

## 8. Everyday hardware

### 8.1 M32 — Ethernet

| Driver | Lines | Hardware | Notes |
|---|---|---|---|
| `r8152` | (in `drivers/net/usb`) | RTL8152/8153/8156/8157 USB-C and USB 3 adapters, many docks | the most common USB Ethernet chip |
| `usbnet` + `ax88179_178a`, `asix`, `cdc_ncm`, `cdc_ether`, `rndis_host`, `ipheth` | `drivers/net/usb` (54k total) | ASIX adapters, phones (Android RNDIS/NCM, iPhone `ipheth`) | `ipheth` gives iPhone tethering, which also needs `usbmuxd` (port) for pairing |
| `igb` | 29k | Intel I210/I211/I350/I354 | QEMU has `-device igb`: CI scenario |
| `alx` | 5k | Killer E2200–E2600, Atheros AR8161/8171/8172 | |
| `tg3` | 22k | Broadcom NetXtreme (Dell/HP/Lenovo desktops) | needs `tg3` firmware for some chips |
| `atlantic` | 30k | Aquantia/Marvell AQC107/108/113 (5/10 GbE) | |
| Linux `r8169`, `e1000e`, `igc` | (16k, …) | same chips as RustOS's native drivers, plus every variant and quirk | replace the natives once each passes `hwcheck ethernet` |

**Shim needs:** PHY library (`drivers/net/phy`: `phylib` core, `realtek` and `marvell` PHYs; imported), MDIO bus, `ethtool` link settings, `net_dim`.

**Tests:**
- `eth-igb` (CI);
- `eth-usb-linux` (CI, usb-net);
- hardware runs for the rest.

### 8.2 M33 — Laptop platform: touchpads, card readers, serial, webcams

- **Touchpads and touchscreens:** I2C core + DesignWare + GPIO/pinctrl + I2C-HID + HID core + `hid-multitouch` (§5.16).
  - `PNP0C50` devices are found through the shim's ACPI matching.
  - The user's MT7921K laptop is most likely an AMD Ryzen system. AMD touchpads use `AMDI0010` I2C controllers and `AMDI0030` GPIO, both covered by the first import.
  - **Polling fallback:** for an unsupported GPIO controller, the shim polls `i2c-hid` at 100 Hz. Linux has no such fallback; it is a small RustOS patch in `PATCHES/`.
  - **Test:** QEMU cannot emulate I2C-HID on x86, so this is tested on hardware: `hwcheck input` (new section: lists touch devices, asks for a two-finger scroll, checks `ABS_MT_SLOT` events).
- **SD/MMC:**
  - `drivers/mmc/core` (26k) plus `sdhci`, `sdhci-pci` (`08 05` class), `sdhci-acpi`, `rtsx_pci_sdmmc`, `rtsx_usb_sdmmc`, and `drivers/misc/cardreader` (`rtsx_pcr`, `rtsx_usb`, Realtek readers).
  - The block devices register with RustOS's block layer as `mmcblkN` (the shim's `blk-mq` subset for MMC: `blk_mq_alloc_tag_set`, `blk_mq_init_queue`, request completion, bridged to RustOS's block request queue).
  - **Test:** QEMU `-device sdhci-pci -device sd-card,drive=...` scenario, FAT32 and ext4 round trips (CI).
- **USB serial:**
  - `drivers/usb/serial` (`usb-serial`, `generic`, `ftdi_sio`, `cp210x`, `ch341`, `pl2303`, `option` for LTE modems) and `drivers/usb/class/cdc-acm.c`;
  - `tty_driver`/`tty_port` shim onto RustOS's TTY layer (`/dev/ttyUSB0`, `/dev/ttyACM0`, termios baud/parity/flow control).
  - **Test:** QEMU `-device usb-serial` (FTDI emulation) scenario (CI).
- **Webcams:**
  - `drivers/media/v4l2-core` (28k), `videobuf2` (7k) and `uvcvideo` (13k), giving the full V4L2 uAPI on `/dev/video0` and `/dev/media0`;
  - needs xHCI isochronous IN (M30) and `vb2_dma_contig`/`vmalloc` memory ops.
  - **Test:** QEMU USB passthrough of a webcam on a developer machine; `hwcheck webcam` grabs a frame to the storage partition.
- **Bluetooth firmware**, translated like btmtk: Realtek `btrtl` (`rtl_bt/rtl8761bu_fw.bin` and config, `rtl8852*`), and Broadcom `btbcm` patchram.

### 8.3 M34 — Linux sound

RustOS's native HDA driver works for basic playback but has no codec quirk
table. Many laptops' speakers, headphone jacks and especially **internal
microphones** need quirks or a DSP driver (SOF on Intel, ACP on AMD).

- **Imported:**
  - `sound/core` (PCM, control, timer, `seq` optional, `oss` emulation for the existing `/dev/dsp` users);
  - `sound/hda` (6.18 layout: `core/`, `common/`, `controllers/intel.c` = `snd-hda-intel`, and `codecs/`: `realtek/`, `hdmi/`, `cirrus/`, `conexant.c`, `generic.c`, `helpers/`, `side-codecs/`);
  - `sound/usb` (USB audio with explicit feedback and capture);
  - `sound/soc/sof` (including `sof/amd`) + `sound/soc/amd` (`acp/` generic ACP, `yc/` Yellow Carp/Rembrandt, `ps/` Pink Sardine, `rpl/`, `renoir/`, `vangogh/`) for DMIC microphones; `sound/soc/intel` for SOF-based Intel laptops.
  - ASoC is large; it comes last and only for the platforms in hand.
- **uAPI:** this gives the full **ALSA uAPI** (`/dev/snd/*`), which M42's PipeWire needs, replacing the plan to write ALSA uAPI by hand.
- **Native drivers:** native HDA, USB audio and virtio-sound are retired after hardware reports (virtio-sound has a Linux driver too: `sound/virtio`).
- **Tests:** the existing `audio` scenario runs against Linux HDA (QEMU `intel-hda` + `hda-duplex`) and `sound/virtio`.

### 8.4 What stays native or translated

- **Bluetooth:** native stack (round 3). Vendor firmware loaders (`btmtk`, `btrtl`, `btbcm`) are translated into it, because Linux's loaders depend on `hci_dev` internals.
  - Importing all of `net/bluetooth` (with BlueZ in userland) is a possible later switch if the native stack falls short. It would bring A2DP and LE Audio through BlueZ/PipeWire.
- **NVMe, AHCI, xHCI, USB mass storage/UAS, ext4, FAT, network stack:** native.

## 9. Graphics

### 9.1 M35 — DRM core, dma-buf, firmware-framebuffer and QEMU display drivers

**Imported:**
- `drivers/gpu/drm/*.c` (DRM core, 80k lines): `drm_drv`, `drm_file`, `drm_ioctl`, `drm_gem`, `drm_prime`, `drm_atomic*`, `drm_crtc*`, `drm_plane*`, `drm_connector`, `drm_encoder`, `drm_framebuffer`, `drm_fourcc`, `drm_modes`, `drm_edid`, `drm_mode_config`, `drm_mode_object`, `drm_property`, `drm_blend`, `drm_color_mgmt`, `drm_damage_helper`, `drm_vblank`, `drm_syncobj`, `drm_mm`, `drm_buddy`, `drm_exec`, `drm_gem_shmem_helper`, `drm_gem_atomic_helper`, `drm_gem_framebuffer_helper`, `drm_fb_helper`, `drm_fbdev_*`, `drm_client*`, `drm_format_helper`, `drm_managed`, `drm_panic` (optional), `drm_privacy_screen` (stub).
- `drivers/gpu/drm/display` (DP/HDMI/DSC helpers, 20k), `drivers/gpu/drm/ttm` (11k), `drivers/gpu/drm/scheduler` (4k).
- `drivers/dma-buf` (10k): dma-buf, fences, `dma-resv`, `sync_file`, `udmabuf` (optional), `dma-heap` (optional).
- **Display drivers:** `drivers/gpu/drm/sysfb` (`efidrm.c`, `simpledrm.c`, `drm_sysfb*.c`) for the UEFI GOP framebuffer on every machine; `drivers/gpu/drm/tiny/bochs.c` (QEMU `-vga std`); `drivers/gpu/drm/virtio` (virtio-gpu, 6k).

**RustOS work:**
- **Device nodes:** `/dev/dri/card0`, `/dev/dri/renderD128` and `/dev/dri/by-path/` from the cdev shim.
  - The `sysfs` bridge exposes `/sys/class/drm/card0-*` connectors (`status`, `modes`, `edid`, `enabled`), which compositors and `libudev-zero` read.
- **Framebuffer description:** the GOP framebuffer is handed to `efidrm`/`simpledrm` as a `screen_info`/`platform_device`, as Linux's `sysfb` does from the EFI framebuffer.
- **Console handover:** the DRM fbdev emulation provides `/dev/fb0`. RustOS's framebuffer console moves onto the DRM client API (`drm_client`), or keeps drawing to GOP until a native GPU driver takes over (`aperture` helpers: `aperture_remove_conflicting_*` must evict RustOS's console and `efidrm` when amdgpu/nouveau load).
- **Existing graphics programs:** `browse -g` keeps using `/dev/fb0` (fbdev emulation).
- **GOP mode choice:** the boot image picks the largest GOP mode up to the panel's native size; `write_to_drive.sh --resolution WxH` overrides it.

**Tests (CI):**
- `drm-bochs` and `drm-virtio` scenarios run `modetest` (libdrm, ported in M37; before that a small C test with the uAPI headers):
  - list connectors and modes, set a mode, page-flip with events, check the image with QEMU `screendump`;
  - PRIME export/import between `card0` and `renderD128`, `sync_file` waits.
- `drm-efidrm` on `-vga none` with OVMF's GOP.

### 9.2 M36 — Kernel features for a desktop userland

Native Rust work; each item is small.

- **Unix sockets:** `SCM_RIGHTS`, `SCM_CREDENTIALS`, `SO_PEERCRED`, `SO_PASSCRED` on `AF_UNIX` (stream and datagram), and abstract socket names. Wayland, D-Bus and wpa_supplicant depend on these.
- **Shared memory:** `memfd_create` with `MFD_CLOEXEC`/`MFD_ALLOW_SEALING`, `F_ADD_SEALS`/`F_GET_SEALS`, `/dev/shm` tmpfs (`shm_open`), and `MAP_SHARED` of anonymous, memfd and shm objects across `fork` and fd passing.
- **`inotify`:** `inotify_init1`, `inotify_add_watch`/`rm_watch`, with events from the VFS (create, delete, modify, move, attrib, close-write).
- **uevents:** `NETLINK_KOBJECT_UEVENT` broadcasts from the device model bridge (add, remove, change), with `/sys/.../uevent` files.
  - libudev-zero, libinput, Weston/wlroots hotplug and `mdev` use them.
- **VT switching for compositors:**
  - `VT_SETMODE` with `VT_PROCESS`, `VT_RELDISP`, `VT_ACTIVATE`/`WAITACTIVE`, `KDSKBMODE` (`K_OFF`);
  - DRM master drop/set on switch;
  - `EVIOCREVOKE`.
  - libseat's builtin backend then works, and `seatd` is ported as a daemon option.
- **Kernel FPU sections:** `kernel_fpu_begin`/`end` (XSAVE of the current thread's state, preempt-disabled) and `kernel_fpu_begin_mask`. Used by amdgpu DML, RAID/CRC helpers and some audio code.
- **Small syscalls:** `pidfd_open`, `pidfd_send_signal`, `close_range`, `statx`, `copy_file_range`, and `prctl(PR_SET_NAME)` if missing. `sched_getaffinity` and `timerfd` already exist.
- **POSIX timers and clocks:** `CLOCK_MONOTONIC_RAW`, `CLOCK_BOOTTIME`, `clock_nanosleep(TIMER_ABSTIME)`.
- **Tests:**
  - `userland/musltest` C programs per feature;
  - a `desktop-kernel` scenario (fd passing between processes, memfd seals, inotify events, a uevent on USB hotplug via `device_add`, VT switch with a held DRM master).

### 9.3 M37 — C++, the Wayland stack, a software-rendered desktop

- **Toolchain:**
  - **C++ runtime:** LLVM `libc++` + `libc++abi` + `libunwind`, built against the musl sysroot with clang (`tools/rustos-c++`). Alternatively libstdc++ from a musl GCC.
  - **Build systems:** Meson and CMake cross files (`tools/cross/`), a pkg-config wrapper, and a `ports/` recipe format with dependency ordering (extending `tools/install-port.sh`).
  - **Rust userland:** check whether `x86_64-unknown-linux-musl` static Rust binaries run unmodified on RustOS. Its syscall interface follows Linux. If they do, Rust ports (Mesa's NAK compiler, some Wayland tools) need no new target; if not, the gaps become M36 items.
- **Ports, in order:**
  1. **base:** zlib, zstd, xz, libffi, expat, libxml2, pcre2, libpng, libjpeg-turbo, libwebp;
  2. **text:** freetype, harfbuzz (C++), fribidi, fontconfig, a font set (DejaVu, Noto subset);
  3. **2D graphics:** pixman, cairo, libdrm (with `modetest`, `proptest`);
  4. **Wayland and keyboard:** wayland + wayland-protocols, libxkbcommon + xkeyboard-config;
  5. **input and seats:** libevdev, mtdev, libudev-zero, libinput (needs M33 touchpads for laptops), seatd/libseat;
  6. **IPC:** dbus (reference `dbus-daemon`, optional at this stage).
- **First desktop:**
  - Weston (current release) with the DRM backend and **pixman renderer**: `weston-terminal`, `weston-simple-shm`, foot terminal;
  - runs on `efidrm`, so any machine;
  - `browse -g` gets a Wayland backend (`xdg-shell`, `wl_shm`).
- **Software OpenGL:** Mesa `softpipe` (no LLVM) for EGL/GLES clients; `llvmpipe` once LLVM is ported (optional, large).
- **Tests (CI):** `desktop` scenario on `-vga std`/virtio-gpu.
  - Starts Weston, waits for its ready message, launches `weston-terminal`, types `echo ok` with `sendkey`, and checks a `screendump` region against a reference.
  - Mouse moves via QEMU `mouse_move`.

### 9.3a Target machines for the GPU milestones

The user's desktop (`AriPC`) has two GPUs, and both are supported by Linux 6.18. They set the order of M38 and M39.

| GPU | PCI ID | Linux driver | Identity in 6.18 | Firmware |
|---|---|---|---|---|
| AMD Raphael (Ryzen 7000 integrated graphics, RDNA2, 2 CUs) | `1002:164e` | amdgpu | GC 10.3.6, DCN 3.1.5, PSP 13.0.5, SDMA 5.2.6, VCN 3.1.2 | `amdgpu/gc_10_3_6_{ce,pfp,me,mec,mec2,rlc}.bin`, `psp_13_0_5_{toc,ta}.bin`, `sdma_5_2_6.bin`, `dcn_3_1_5_dmcub.bin`, `vcn_3_1_2.bin` |
| NVIDIA GeForce RTX 5070 (Blackwell) | `10de:2f04` | nouveau | chipset `0x1b5` = `GB205` (`nv1b5_chipset`: `gb202_disp`, `gb202_gsp`, `gb202_fsp`) | GSP-RM **570.144**: `nvidia/gb205/gsp/fmc-570.144.bin`, `bootloader-570.144.bin`, `gsp-570.144.bin` |

**Order:**
1. **The Raphael iGPU (M38) first.** amdgpu is the most mature open driver, display support for DCN 3.1.5 is complete, and the firmware set is small. A monitor must be connected to the motherboard's video output, and the iGPU enabled in the BIOS.
2. **The RTX 5070 (M39) after.** Blackwell support in nouveau is recent (GSP-RM 570, a new FSP/FMC boot sequence through `gb202_fsp`, and the reorganised NVD5.0 display engine), so it is likely to need more debugging.
   - Only GSP-based operation exists for Blackwell, so power management is handled by NVIDIA's firmware.
   - Whether Mesa's NVK supports GB20x for Vulkan has to be checked against the Mesa release at M41 time.

**Development loop on this machine, with two GPUs:**
- **nouveau via VFIO:** the host Linux runs its desktop on the Raphael iGPU and passes the RTX 5070 to QEMU running RustOS. This is the classic, well-supported VFIO setup.
  - Host setup: IOMMU on in the BIOS; `amd_iommu=on iommu=pt`; bind `10de:2f04` and its HDMI audio function to `vfio-pci`.
  - Run: `qemu-system-x86_64 -machine q35 -device vfio-pci,host=01:00.0,multifunction=on -device vfio-pci,host=01:00.1 ...`.
  - A nouveau change is tested in about a minute with no reboot of the host.
- **amdgpu on bare metal**, from the USB stick: passing an AMD *integrated* GPU through to a VM needs its VBIOS extracted from the `VFCT` ACPI table and is fragile.
  - Optionally the reverse, with the host on the NVIDIA card, if iGPU passthrough works on this board.
- **Before M38/M39, M35's `efidrm` is the display:** whichever GPU the BIOS chose as primary provides the GOP framebuffer, so RustOS's software desktop (M37) runs on either output.

### 9.4 M38 — AMD GPUs (amdgpu)

- **Imported:**
  - `drivers/gpu/drm/amd` (1.24M lines including generated register headers): `amdgpu` (355k), `display` (527k: DC, DMUB, DML/DML2), `pm` (`swsmu`, `powerplay` for older parts), `amdkfd` (excluded: compute, later), `include`, `acp` (excluded unless audio needs it).
  - The register headers (`include/asic_reg`, several hundred MB of `#define`s) are imported only for the GPU generations enabled in `configs/rustos.config`, to keep build times sane.
- **Firmware:** `amdgpu/*.bin` (GC, SDMA, PSP (`*_sos`, `*_ta`, `*_toc`), SMU, DMCUB, VCN, MES, IMU, per GPU, e.g. `amdgpu/gc_11_0_0_pfp.bin`).
  - `write_to_drive.sh` identifies the GPU (`lspci` PCI ID → amdgpu's IP-discovery-based firmware list from `MODULE_FIRMWARE`) or copies all of `amdgpu/` (≈ 60 MB).
- **Steps:**
  1. **Display (KMS):**
     - amdgpu base: PCI, IP discovery, VBIOS (PCI ROM, `VFCT`/`ATRM`), GMC (VRAM manager, GART), IH (interrupt handler ring), PSP and SMU firmware load;
     - DC with DMUB: native modes on eDP/DP/HDMI/USB-C, multiple monitors, hotplug, eDP backlight (`/sys/class/backlight`), DSC, FreeSync off.
     - The DML/DML2 files compile with FPU flags and run inside `DC_FP_START`/`DC_FP_END`, so `kernel_fpu_begin` is required (M36).
     - **Result:** Weston at native resolution on every output, with real vblank.
  2. **Rendering:** GEM/TTM with VRAM and GTT placement, the VM (GPUVM page tables), GFX/compute/SDMA rings, `drm_sched`, `AMDGPU_CS`/`AMDGPU_WAIT_CS`/`AMDGPU_GEM_*`/`AMDGPU_INFO`/`AMDGPU_CTX`, `drm_syncobj`. This is what Mesa (M41) needs.
  3. **Power and video:**
     - SMU DPM (clocks scale with load, fan control on dGPUs), GFXOFF, runtime PM for laptop dGPUs;
     - VCN video decode (`amdgpu` + Mesa VA-API) later.
- **Shim needs:**
  - `kernel_fpu_*`, `pci_map_rom`;
  - ACPI `ATIF`/`ATCS`/`ATPX` (`acpi_evaluate_object`), `i2c_adapter` for DDC/AUX, `hwmon` (stub);
  - `mmu_notifier` (needed for userptr: stubbed until needed), `dma_fence` chains.
- **Targets:** first the user's Raphael iGPU (§9.3a), then Ryzen APUs in general (Vega/RDNA2/RDNA3/RDNA3.5 graphics) and Radeon RX 400 → RX 9000 (Polaris/Vega/RDNA1–4).
  - Southern Islands and Sea Islands (HD 7000/R9 200) need `radeon` or amdgpu's SI/CIK support and are out of scope.
- **Testing:**
  - The user's AMD machine.
  - For fast iteration, **VFIO passthrough**: on a Linux host with a second AMD GPU (or the iGPU passed through while the host uses another), `qemu ... -device vfio-pci,host=0000:03:00.0,x-vga=on`. RustOS boots in QEMU with the real GPU attached, and a driver change needs no USB stick.
  - `hwcheck display` (new section): modes and EDID per connector, hotplug, a page-flip timing histogram, backlight steps; `hwcheck gpu`: firmware load, ring tests (`amdgpu_test_ring_*` output), GPU reset.

### 9.5 M39 — NVIDIA GPUs (nouveau)

- **Imported:** `drivers/gpu/drm/nouveau` (225k), including `nvkm` and the GSP-RM interface (`nvkm/subdev/gsp/rm/r535` and `r570`).
- **Firmware** (`nvidia/<chip>/gsp/`):
  - nouveau 6.18 supports GSP-RM **535.113.01** and **570.144** on Turing (TU1xx), Ampere (GA10x), Ada (AD10x), Hopper (GH100) and Blackwell (GB10x/GB20x);
  - files: `booter_load-*`, `booter_unload-*`, `bootloader-*`, `gsp-*`, `fwsec` (from linux-firmware).
- **Steps:**
  1. **Turing and newer through GSP:** GSP boot (falcon/sec2, FWSEC), then RM RPCs for display, memory and channels.
     - Power and clocks are handled by GSP, so performance is right from the start.
     - KMS: native modes, multiple monitors, hotplug, backlight via RM.
  2. **Kepler to Volta without GSP:** display works; Maxwell 2+ stays at boot clocks (no reclocking without NVIDIA's signed firmware). Acceptable for a desktop, slow for 3D.
  3. **NVK support:** `DRM_NOUVEAU_VM_INIT`/`VM_BIND`/`EXEC`, `drm_gpuvm`, syncobjs.
- **Why not NVIDIA's own driver:**
  - the proprietary module needs NVIDIA's closed glibc userland;
  - `open-gpu-kernel-modules` (MIT/GPL) pairs with the same closed userland.
- **Nova:** watched. If Linux's Rust `nova-drm` gains display support, porting it means implementing Linux's Rust `kernel` crate abstractions (a Rust LinuxKPI). That is a different shim, and is not planned unless nova overtakes nouveau.
- **Testing:** as M38 (hardware, VFIO).

### 9.6 M40 — Intel GPUs (i915 and xe)

The most common laptop GPUs. Same LinuxKPI path:
- `drivers/gpu/drm/i915` (408k) for Gen9–Gen12 (Skylake → Alder/Raptor Lake);
- `drivers/gpu/drm/xe` (119k) for Lunar Lake/Battlemage and newer (xe shares display code with i915: `i915/display` is compiled into xe).

Firmware: `i915/*_guc_*.bin`, `*_huc_*.bin`, `*_dmc*.bin`, `xe/*`. Display
first, then rendering (`execbuf`/`vm_bind`) for Mesa's `iris`/`ANV`.

### 9.7 M41 — Mesa

- **Port:** Mesa 25.x via the meson cross files.
- **Drivers built per enabled GPU:**
  - **AMD:** `radeonsi` (OpenGL) and **RADV** (Vulkan, ACO compiler in C++, no LLVM needed). Check at port time whether the Mesa release can build `radeonsi` without LLVM; if not, use `zink` over RADV for OpenGL until LLVM is ported.
  - **NVIDIA:** **NVK** (Vulkan). Its NAK compiler is Rust, built for the musl target checked in M37. `zink` provides OpenGL over Vulkan.
  - **Intel:** `iris` (OpenGL) and **ANV** (Vulkan).
  - **QEMU:** `virgl`/`venus` for virtio-gpu 3D testing (optional; needs QEMU with virglrenderer on the CI host).
  - **Fallback:** `softpipe`/`llvmpipe`.
- **Window-system integration:** EGL with the GBM and Wayland platforms, `libgbm`, Vulkan WSI (`VK_KHR_wayland_surface`, `VK_KHR_display`).
- **Tests:** `kmscube` (GBM/KMS), `eglinfo`, `vulkaninfo`, `vkcube`, `glmark2-es2-wayland`. On hardware, results go into `hwcheck gpu`. In CI they run on venus if available, otherwise softpipe.

### 9.8 M42 — Desktop environments

Ported in order of what each requires:

1. **Weston** (M37), with the GL renderer once M41 is done.
2. **wlroots stack:**
   - compositors: **labwc** (stacking, Openbox-like: the default desktop proposal) or **Sway** (tiling);
   - tools: `foot`, `fuzzel`, `waybar`, `mako`, `swaybg`, `swaylock`, `grim`, `slurp`, `wl-clipboard`, `kanshi`.
   - Needs: libinput, libseat, xkbcommon, pixman, and EGL/GLES2 (M41) or wlroots' pixman renderer without it.
3. **Xwayland:** X11 applications through `libxcb`, `libX11`, `xkbcomp`, `xorg-server` (Xwayland only), font utilities.
4. **GTK:**
   - libraries: GLib (needs `inotify`, `eventfd`, `pidfd` → M36), Pango, gdk-pixbuf, libepoxy, GTK 3 and GTK 4 (GL or Cairo renderer);
   - applications: a file manager (Thunar or PCManFM-GTK3), a text editor (Mousepad or gedit), a terminal (`foot` is already there);
   - **Firefox** is a large port and its own project: Rust, C++, Wayland, a JS engine, sandboxing via `seccomp`, which RustOS lacks, so it runs with the sandbox disabled.
5. **Qt 6 and KDE Plasma:** Qt 6 (C++, Wayland platform plugin), then KDE Frameworks and Plasma.
   - Plasma needs D-Bus, polkit, a logind-compatible session API (`elogind` or a RustOS session service implementing the parts of `org.freedesktop.login1` Plasma uses), UPower, NetworkManager or a shim (§ below), and PipeWire.
   - The largest port; last.
6. **GNOME** requires systemd; out of scope.

**Desktop services:**
- **Sessions and seats:** `seatd`, or a RustOS-native `login1` subset.
- **Audio:** PipeWire + WirePlumber on ALSA (M34).
- **Network:** `wpa_supplicant` (M28) plus a small **NetworkManager D-Bus subset** (`rustos-nmd`) or `iwd`, so desktop network applets (nm-applet, Plasma's) can scan and connect.
- **Power:** UPower over `/sys/class/power_supply`. RustOS exposes ACPI battery and AC there (extends round 3's battery code).
- **Notifications, portals:** `mako`, `xdg-desktop-portal-wlr`.

**Tests (CI):**
- a `desktop-labwc` scenario on virtio-gpu (QEMU): login to labwc, open `foot` and a GTK app, screenshot comparison;
- on hardware, `hwcheck desktop` records compositor start time, frame timing and input latency.

---

## 10. Firmware provisioning (`write_to_drive.sh`), generalized

- **What to copy:** the kernel build produces `target/firmware-list.txt`, every `MODULE_FIRMWARE` name from the compiled Linux drivers plus the native drivers' lists. `write_to_drive.sh` copies each file that exists on the host (`/lib/firmware`, `/usr/lib/firmware`, also `.zst`/`.xz`).
- **Default: only the hardware present.** It copies only firmware for hardware in the target machine. It reads `/sys/bus/pci/devices/*/modalias` and `/sys/bus/usb/devices/*/modalias` from the host, which is the right list when the stick is made on the machine it will boot. `--all-firmware` copies every listed file, for sticks that boot on many machines.
- **Explicit sources:** `--firmware DIR` points at a linux-firmware checkout, which covers all vendors in one flag and replaces `--ax210-firmware` and `--mediatek-firmware` (both kept as aliases).
- **Regulatory database:** `regulatory.db` is copied from `wireless-regdb` for cfg80211.

## 11. Testing and hardware bring-up

| Area | CI (QEMU) | Developer machine (QEMU + passthrough) | User hardware |
|---|---|---|---|
| LinuxKPI core | Linux e1000, igb, usb-net, sdhci, usb-serial, HDA, virtio-sound, bochs, virtio-gpu | — | — |
| 802.11 stack, WPA2/3/Enterprise, roaming | `mac80211_hwsim` + hostapd | — | — |
| Wi-Fi chips | — | USB adapters (MT7921AU, MT7612U, rtw88 USB, AR9271) via `usb-host` passthrough | `hwcheck wifi` |
| Ethernet chips | igb, e1000, usb-net | USB adapters via passthrough | `hwcheck ethernet` |
| Touchpads, card readers, webcams | sdhci | webcam via passthrough | `hwcheck input`, `sdcard`, `webcam` |
| GPUs | bochs, virtio-gpu (+ venus optional) | **VFIO passthrough** of a real GPU | `hwcheck display`, `gpu` |
| Desktop | Weston, labwc on virtio-gpu | — | `hwcheck desktop` |

- **A cheap test bench** gives most coverage per dollar:
  - an MT7921AU adapter;
  - an MT7612U adapter;
  - an RTL8821CU adapter;
  - an AR9271 adapter;
  - an RTL8153 Ethernet adapter;
  - an AX88179 Ethernet adapter;
  - a UVC webcam;
  - an FTDI serial cable;
  - a used Intel AX200/9260 M.2 card.
- **For GPUs**, a desktop Linux machine with a spare PCIe slot for VFIO passthrough.

**Bug-report loop on hardware:**
1. The user runs `hwcheck` (whole or one section).
2. It writes `/storage/hwcheck-DATE/` with logs, `dmesg` at `linux.debug=<module>` verbosity, PCI/USB dumps and firmware versions.
3. The developer fixes, pushes to `main`, and the user reruns `write_to_drive.sh` (which pulls and builds).

## 12. Risks

| Risk | Mitigation |
|---|---|
| The shim's semantics differ subtly from Linux (sleeping in atomic context, RCU grace periods, workqueue ordering, `local_bh_disable`) and cause rare hangs or corruption | Host unit tests per primitive; Linux's own e1000/igb/usb-net/hwsim as CI canaries; debug kernel option that asserts `might_sleep` in atomic context and checks lock ordering per RustOS lock classes |
| Linux API churn between LTS releases | Pin one LTS; update once a year; `PATCHES/` minimal; `linux-undefined.py` diff shows new API needs before an update |
| Build time and kernel size grow (amdgpu alone is ~1.2M lines with headers) | Per-feature Cargo flags; only enabled GPU generations' register headers; `sccache`/ccache for C; LTO optional |
| GPU drivers need features RustOS lacks (suspend/resume, runtime PM, hwmon, IOMMU) | Not required for display and rendering; `pm_runtime` always-on; suspend is a later round |
| Firmware not on the host, or packaged differently | Generalized provisioning (§10) with `--firmware DIR`; `dmesg` names every missing file |
| Hardware access for testing | Cheap USB test bench; VFIO for GPUs; `hwcheck` reports from users |
| Desktop userland is hundreds of ports, each surfacing kernel gaps | M36 front-loads known gaps; `strace`-like syscall log (`kernel.conf: syscall.trace=<pid>`) to find the rest |

## 13. Size summary (imported vs new)

| Milestone | Imported Linux (approx. lines) | New RustOS code |
|---|---|---|
| M27 | 30k (lib, drivers/base, locking, e1000) | large (shim core) |
| M28 | 150k (cfg80211, mac80211, hwsim) | medium (netlink, netdev, crypto shim; wpa_supplicant port; `wifi` rewrite) |
| M29 | 45k (mt76 core, connac, mt792x, mt7921) | small |
| M30 | 30k (usbnet family, mt76-usb) | medium (USB shim, isoch IN) |
| M31 | 1M+ (all Wi-Fi drivers + MHI/QRTR) | small per driver |
| M32 | 120k | small per driver |
| M33 | 250k (HID, I2C, GPIO, MMC, V4L2, UVC, usb-serial) | medium (input/block/tty bridges) |
| M34 | 400k+ (ALSA core, HDA, USB audio, SOF/ACP) | medium |
| M35 | 140k (DRM core, display helpers, TTM, sched, dma-buf, sysfb, bochs, virtio) | medium |
| M36 | — | medium |
| M37 | — (userland ports) | large (porting) |
| M38 | 1.2M (amdgpu + DC + pm) | medium |
| M39 | 225k (nouveau) | medium |
| M40 | 530k (i915 + xe) | medium |
| M41 | — (Mesa port) | large (porting) |
| M42 | — (desktop ports) | very large (porting) |

The imported code dwarfs everything RustOS has written so far. That is the
point of the approach: the remaining work is the shim, the bridges into
RustOS's own subsystems, porting userland, and debugging on real hardware.
