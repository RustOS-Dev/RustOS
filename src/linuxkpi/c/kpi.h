/* SPDX-License-Identifier: GPL-2.0-or-later */
/*
 * Services the RustOS kernel provides to the LinuxKPI C glue
 * (implemented in src/linuxkpi/*.rs). Everything Linux code calls is
 * implemented in C in this directory on top of these, so Linux struct
 * layouts only ever meet code compiled against Linux's own headers.
 */
#ifndef _RUSTOS_KPI_H
#define _RUSTOS_KPI_H

#include <linux/types.h>

/* Memory (src/linuxkpi/mm.rs). Physical addresses; 0 means failure. */
u64 rustos_kpi_page_offset(void);
u64 rustos_kpi_max_pfn(void);
u64 rustos_kpi_map_zeroed(u64 virt, u64 size);	/* 0 on success */
u64 rustos_kpi_alloc_frames(u64 count, u64 align, int below_4g);
void rustos_kpi_free_frames(u64 phys, u64 count);
void *rustos_kpi_vmalloc(u64 size);
void rustos_kpi_vfree(const void *addr);
int rustos_kpi_is_vmalloc(const void *addr);
void *rustos_kpi_ioremap(u64 phys, u64 size, int wc);
void rustos_kpi_iounmap(void *addr);

/* Logging (src/linuxkpi/mod.rs). */
void rustos_kpi_log(int level, const char *msg, u64 len);
void rustos_kpi_backtrace(void);
void rustos_kpi_panic(const char *msg) __attribute__((noreturn));

/* Time and scheduling (src/linuxkpi/sched.rs). */
u64 rustos_kpi_nanos(void);
void rustos_kpi_delay_ns(u64 ns);
u64 rustos_kpi_thread_id(void);
void **rustos_kpi_task_slot(void);	/* per-thread slot for the task_struct shadow */
void rustos_kpi_sleep(u64 deadline_ns);	/* until woken; 0 = no timeout */
void rustos_kpi_wake(u64 tid);
void rustos_kpi_yield(void);
u64 rustos_kpi_spawn(void (*fn)(void *), void *arg, const char *name);
u32 rustos_kpi_cpu_id(void);
u32 rustos_kpi_cpu_count(void);
void rustos_kpi_set_cpu_offset(u32 cpu, u64 off);

/* Timers: fn(arg, handle) runs in the LinuxKPI softirq thread once
 * `deadline_ns` has passed. */
u64 rustos_kpi_timer_start(u64 deadline_ns, void (*fn)(void *, u64), void *arg);
int rustos_kpi_timer_cancel(u64 handle);	/* 1 if it had not fired */
void rustos_kpi_softirq_raise(void);

/* PCI and interrupts (src/linuxkpi/pci.rs). Devices are indexes into the
 * RustOS PCI list. */
struct kpi_pci_bar {
	u64 start;
	u64 size;
	u32 flags;			/* 1 mem, 2 I/O, 4 prefetchable, 8 64-bit */
};
struct kpi_pci_info {
	u16 segment;
	u8 bus, dev, func, revision, irq_pin;
	u16 vendor, device, subvendor, subdevice;
	u32 class;			/* class << 16 | subclass << 8 | prog-if */
	struct kpi_pci_bar bars[6];
};
u32 rustos_kpi_pci_count(void);
int rustos_kpi_pci_get(u32 idx, struct kpi_pci_info *out);
u32 rustos_kpi_pci_read(u32 idx, u32 off, u32 size);
void rustos_kpi_pci_write(u32 idx, u32 off, u32 size, u32 val);
/* Route the device's interrupt (INTx, or MSI if msi) to fn(arg) in
 * interrupt context. Returns the vector or -1. */
int rustos_kpi_pci_irq(u32 idx, int msi, void (*fn)(void *), void *arg);
int rustos_kpi_pci_has_msi(u32 idx);
u64 rustos_kpi_random_u64(void);

/* Network devices (src/linuxkpi/net.rs). */
u64 rustos_kpi_netdev_register(void *dev, const u8 *mac, u32 mtu, int wireless,
			       const char *driver, char *name, u32 name_len);
void rustos_kpi_netdev_rx(u64 handle, const void *data, u32 len);
void rustos_kpi_netdev_carrier(u64 handle, int on);
void rustos_kpi_netdev_mtu(u64 handle, u32 mtu);

/* Wall-clock time (src/linuxkpi/sched.rs). */
u64 rustos_kpi_realtime_ns(void);

/* sysfs (src/linuxkpi/sysfs.rs); paths are relative to /sys. */
int rustos_kpi_sysfs_mkdir(const char *path);
int rustos_kpi_sysfs_add_file(const char *path, u32 mode, void *cookie);
int rustos_kpi_sysfs_add_link(const char *path, const char *target);
void rustos_kpi_sysfs_remove(const char *path);
int rustos_kpi_sysfs_rename(const char *old_path, const char *new_path);

/* User memory (src/linuxkpi/mm.rs): return the bytes not copied. */
unsigned long rustos_kpi_copy_from_user(void *to, const void *from, unsigned long n);
unsigned long rustos_kpi_copy_to_user(void *to, const void *from, unsigned long n);

/* RCU (src/linuxkpi/sched.rs): wait for a grace period. */
void rustos_kpi_rcu_synchronize(void);

/* Firmware (src/linuxkpi/firmware.rs): load name into alloc()'d memory. */
int rustos_kpi_firmware_load(const char *name, void *(*alloc)(size_t), void **data,
			     size_t *size);

/* ACPI (src/linuxkpi/acpi.rs); values in the tagged encoding. */
int rustos_kpi_acpi_eval(const char *path, const void *args, size_t args_len,
			 void *(*alloc)(size_t), void **out, size_t *out_len);
int rustos_kpi_acpi_exists(const char *path);
int rustos_kpi_acpi_pci_path(u8 bus, u8 dev, u8 func, char *buf, size_t len);
int rustos_kpi_acpi_table(const u8 *sig, u32 instance, u64 *phys, u64 *len);

/* Shared between the C glue files. */
void kpi_netdev_open_pending(void);
struct pci_dev;
void kpi_acpi_pci_companion(struct pci_dev *pdev);

#endif
