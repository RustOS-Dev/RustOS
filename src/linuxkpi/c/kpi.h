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
/* Until woken; 0 = no timeout. `site` (the caller) shows as the thread's
 * wait channel in state dumps. */
void rustos_kpi_sleep(u64 deadline_ns, void *site);
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
int rustos_kpi_pci_claimed(u32 idx);
u64 rustos_kpi_random_u64(void);

/* Network devices (src/linuxkpi/net.rs). */
/* `ether`: 0 for interfaces that carry no Ethernet frames (radiotap
 * monitors), which RustOS's IP stack leaves alone. */
u64 rustos_kpi_netdev_register(void *dev, const u8 *mac, u32 mtu, int wireless, int ether,
			       const char *driver, const char *name);
int rustos_kpi_netdev_ifindex(u64 handle);
/* The interface was opened (1) or closed (0) on the Linux side. */
void rustos_kpi_netdev_state(u64 handle, int up);
void rustos_kpi_net_kick(void);	/* a transmit queue has room again */
void rustos_kpi_netdev_unregister(u64 handle);
void rustos_kpi_netdev_set_mac(u64 handle, const u8 *mac);
int rustos_kpi_ifname_free(const char *name);
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

/* Character devices and descriptors (src/linuxkpi/chrdev.rs). */
int rustos_kpi_devnode_add(const char *name, u32 devt, int block);
void rustos_kpi_devnode_remove(const char *name);
void rustos_kpi_waitq_wake(void *waitq);
int rustos_kpi_fd_install(void *file, int cloexec, int fd);
int rustos_kpi_fd_reserve(int cloexec);
void rustos_kpi_fd_unreserve(int fd);
void *rustos_kpi_fd_file(int fd);

/* Netlink (src/linuxkpi/net.rs): a kernel socket for protocol `unit`
 * gets user datagrams through input() and closed user ports through
 * release(); NULLs unregister. */
void rustos_kpi_netlink_register(u32 unit, void (*input)(u32, u32, const void *, size_t),
				 void (*release)(u32, u32));
int rustos_kpi_netlink_unicast(u32 proto, u32 portid, const void *data, size_t len);
int rustos_kpi_netlink_multicast(u32 proto, u32 group, u32 exclude_portid, const void *data,
				 size_t len);	/* sockets reached */
int rustos_kpi_netlink_has_listeners(u32 proto, u32 group);

/* USB (src/linuxkpi/usb.rs). Endpoints are addresses (bit 7: IN); 0 is
 * the control endpoint, whose URBs carry `setup`. Errors are -errno. */
int rustos_kpi_usb_control(u64 handle, const u8 *setup, void *data, u32 timeout_ms);
int rustos_kpi_usb_submit(u64 handle, u8 ep, void *buf, u32 len, const u8 *setup,
			  int zero_packet, void *urb);	/* completes via kpi_usb_complete() */
int rustos_kpi_usb_cancel(u64 handle, u8 ep, void *urb);
int rustos_kpi_usb_clear_halt(u64 handle, u8 ep);
int rustos_kpi_usb_set_interface(u64 handle, u32 ifnum, u32 alt, int select);
int rustos_kpi_usb_claim(u64 handle, u32 ifnum);	/* 1: claimed for Linux */
void *rustos_kpi_usb_cookie(u64 handle);
void rustos_kpi_usb_set_cookie(u64 handle, void *cookie);

/* Credentials of the calling process (src/linuxkpi/sched.rs). */
u32 rustos_kpi_current_uid(void);

/* Shared between the C glue files. */
void kpi_netdev_open_pending(void);
bool kpi_uaccess_kernel(const void *addr, unsigned long n);
struct pci_dev;
void kpi_acpi_pci_companion(struct pci_dev *pdev);

#endif
