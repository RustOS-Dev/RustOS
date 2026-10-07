// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * LinuxKPI odds and ends: module parameter ops, system_state, random
 * numbers, linker-script symbols Linux code expects.
 */
#include <linux/capability.h>
#include <linux/ctype.h>
#include <linux/crc32.h>
#include <linux/dmi.h>
#include <linux/io.h>
#include <linux/overflow.h>
#include <linux/pm_qos.h>
#include <linux/vmalloc.h>
#include <asm/processor.h>
#include <asm/cpu_device_id.h>
#include <asm/iosf_mbi.h>
#include <linux/kernel.h>
#include <linux/limits.h>
#include <linux/module.h>
#include <linux/moduleparam.h>
#include <linux/random.h>
#include <linux/refcount.h>
#include <linux/seq_file.h>
#include <linux/proc_fs.h>
#include <linux/string.h>
#include <net/dropreason.h>
#include <linux/irq_work.h>
#include <linux/iommu.h>
#include "kpi.h"

enum system_states system_state = SYSTEM_RUNNING;

/* kernel/locking/spinlock.c: in_lock_functions() compares against these. */
char __lock_text_start[0], __lock_text_end[0];

/*
 * Module parameters keep their compiled-in defaults for now; setting them
 * from kernel.conf (<module>.<param>=) is planned with the device core.
 */
static int kpi_param_set(const char *val, const struct kernel_param *kp)
{
	return 0;
}

static int kpi_param_get(char *buffer, const struct kernel_param *kp)
{
	return 0;
}

#define KPI_PARAM_OPS(name) \
	const struct kernel_param_ops param_ops_##name = { .set = kpi_param_set, .get = kpi_param_get }
KPI_PARAM_OPS(byte);
KPI_PARAM_OPS(short);
KPI_PARAM_OPS(ushort);
KPI_PARAM_OPS(int);
KPI_PARAM_OPS(uint);
KPI_PARAM_OPS(long);
KPI_PARAM_OPS(ulong);
KPI_PARAM_OPS(ullong);
KPI_PARAM_OPS(hexint);
KPI_PARAM_OPS(charp);
KPI_PARAM_OPS(bool);
KPI_PARAM_OPS(bool_enable_only);
KPI_PARAM_OPS(bint);
KPI_PARAM_OPS(invbool);
KPI_PARAM_OPS(string);
const struct kernel_param_ops param_array_ops = { .set = kpi_param_set, .get = kpi_param_get };

void get_random_bytes(void *buf, size_t len)
{
	u8 *p = buf;

	while (len) {
		u64 r = rustos_kpi_random_u64();
		size_t n = min(len, sizeof(r));

		memcpy(p, &r, n);
		p += n;
		len -= n;
	}
}

u8 get_random_u8(void)
{
	return rustos_kpi_random_u64();
}

u16 get_random_u16(void)
{
	return rustos_kpi_random_u64();
}

u32 get_random_u32(void)
{
	return rustos_kpi_random_u64();
}

u64 get_random_u64(void)
{
	return rustos_kpi_random_u64();
}

u32 __get_random_u32_below(u32 ceil)
{
	return ((u64)get_random_u32() * ceil) >> 32;
}

/* RustOS's generator is seeded before any driver runs (RDRAND/RDSEED). */
bool rng_is_initialized(void)
{
	return true;
}

int wait_for_random_bytes(void)
{
	return 0;
}

/* MODULE_VERSION() in built-in code (there is no /sys/module). */
ssize_t __modver_version_show(const struct module_attribute *mattr, struct module_kobject *mk,
			      char *buf)
{
	const struct module_version_attribute *vattr =
		container_of_const(mattr, struct module_version_attribute, mattr);

	return sysfs_emit(buf, "%s\n", vattr->version);
}

/* From lib/refcount.c. */
void refcount_warn_saturate(refcount_t *r, enum refcount_saturation_type t)
{
	static const char *const what[] = {
		[REFCOUNT_ADD_NOT_ZERO_OVF] = "saturated; leaking memory",
		[REFCOUNT_ADD_OVF] = "saturated; leaking memory",
		[REFCOUNT_ADD_UAF] = "addition on 0; use-after-free",
		[REFCOUNT_SUB_UAF] = "underflow; use-after-free",
		[REFCOUNT_DEC_LEAK] = "decrement hit 0; leaking memory",
	};

	refcount_set(r, REFCOUNT_SATURATED);
	WARN(1, "refcount_t: %s\n",
	     t < ARRAY_SIZE(what) && what[t] ? what[t] : "unknown saturation event!?");
}

/* ------------------------------------------------------------------ CRC32 */

/* Table-driven CRC32 (IEEE 802.3), little- and big-endian bit order, as
 * lib/crc/crc32-main.c defines them (no pre/post inversion). */
static u32 kpi_crc32_le_tab[256], kpi_crc32_be_tab[256];

static void kpi_crc32_init(void)
{
	for (u32 i = 0; i < 256; i++) {
		u32 le = i, be = i << 24;

		for (int k = 0; k < 8; k++) {
			le = (le >> 1) ^ (le & 1 ? 0xedb88320 : 0);
			be = (be << 1) ^ (be & 0x80000000 ? 0x04c11db7 : 0);
		}
		kpi_crc32_le_tab[i] = le;
		kpi_crc32_be_tab[i] = be;
	}
}

u32 crc32_le(u32 crc, const void *p, size_t len)
{
	const u8 *b = p;

	if (unlikely(!kpi_crc32_le_tab[1]))
		kpi_crc32_init();
	while (len--)
		crc = (crc >> 8) ^ kpi_crc32_le_tab[(crc & 0xff) ^ *b++];
	return crc;
}

u32 crc32_be(u32 crc, const void *p, size_t len)
{
	const u8 *b = p;

	if (unlikely(!kpi_crc32_be_tab[1]))
		kpi_crc32_init();
	while (len--)
		crc = (crc << 8) ^ kpi_crc32_be_tab[(crc >> 24) ^ *b++];
	return crc;
}

/* ----------------------------------------------------------------- sscanf */

/*
 * vsscanf for the conversions kernel code uses: %d %i %u %x %X %o with
 * hh/h/l/ll/z/j modifiers, %s, %c, %n, %%, field widths and '*'.
 */
int vsscanf(const char *buf, const char *fmt, va_list args)
{
	const char *str = buf;
	int num = 0;

	while (*fmt) {
		int width = -1, qual = 0, base = 10;
		bool skip = false, is_signed = false;
		unsigned long long val;
		char *end;

		if (isspace(*fmt)) {
			fmt = skip_spaces(fmt);
			str = skip_spaces(str);
			continue;
		}
		if (*fmt != '%' || fmt[1] == '%') {
			if (*fmt == '%')
				fmt++;
			if (*fmt++ != *str++)
				break;
			continue;
		}
		fmt++;
		if (*fmt == '*') {
			skip = true;
			fmt++;
		}
		if (isdigit(*fmt))
			width = simple_strtoul(fmt, (char **)&fmt, 10);
		switch (*fmt) {
		case 'h':
			qual = 'h';
			if (*++fmt == 'h') {
				qual = 'H';
				fmt++;
			}
			break;
		case 'l':
			qual = 'l';
			if (*++fmt == 'l') {
				qual = 'L';
				fmt++;
			}
			break;
		case 'z':
		case 'j':
		case 'L':
			qual = 'L';
			fmt++;
			break;
		}
		if (!*fmt)
			break;
		if (*fmt == 'n') {
			if (!skip)
				*va_arg(args, int *) = str - buf;
			fmt++;
			continue;
		}
		if (!*str)
			break;
		switch (*fmt++) {
		case 'c':
			if (width < 0)
				width = 1;
			if (skip) {
				while (width-- > 0 && *str)
					str++;
			} else {
				char *s = va_arg(args, char *);

				while (width-- > 0 && *str)
					*s++ = *str++;
				num++;
			}
			continue;
		case 's': {
			char *s = skip ? NULL : va_arg(args, char *);

			if (width < 0)
				width = INT_MAX;
			str = skip_spaces(str);
			while (*str && !isspace(*str) && width-- > 0) {
				if (s)
					*s++ = *str;
				str++;
			}
			if (s) {
				*s = '\0';
				num++;
			}
			continue;
		}
		case 'd':
			is_signed = true;
			break;
		case 'i':
			is_signed = true;
			base = 0;
			break;
		case 'u':
			break;
		case 'x':
		case 'X':
			base = 16;
			break;
		case 'o':
			base = 8;
			break;
		default:
			return num;
		}
		str = skip_spaces(str);
		{
			/* Honour the field width by scanning a bounded copy. */
			char tmp[32];
			const char *src = str;
			int n = 0;

			while (src[n] && n < (int)sizeof(tmp) - 1 && (width < 0 || n < width))
				n++;
			memcpy(tmp, src, n);
			tmp[n] = '\0';
			if (is_signed && tmp[0] == '-') {
				val = -(long long)simple_strtoull(tmp + 1, &end, base);
				if (end == tmp + 1)
					return num;
			} else {
				val = simple_strtoull(tmp[0] == '+' ? tmp + 1 : tmp, &end, base);
				if (end == tmp || (tmp[0] == '+' && end == tmp + 1))
					return num;
			}
			str += end - tmp;
		}
		if (skip)
			continue;
		switch (qual) {
		case 'H':
			*va_arg(args, u8 *) = val;
			break;
		case 'h':
			*va_arg(args, u16 *) = val;
			break;
		case 'l':
			*va_arg(args, unsigned long *) = val;
			break;
		case 'L':
			*va_arg(args, unsigned long long *) = val;
			break;
		default:
			*va_arg(args, unsigned int *) = val;
		}
		num++;
	}
	return num;
}

int sscanf(const char *buf, const char *fmt, ...)
{
	va_list args;
	int i;

	va_start(args, fmt);
	i = vsscanf(buf, fmt, args);
	va_end(args);
	return i;
}

/* ------------------------------------------------------------- odds/ends */

/* Drop reasons are only names for tracing, which is off. */
void drop_reasons_register_subsys(enum skb_drop_reason_subsys subsys,
				  const struct drop_reason_list *list)
{
}

void drop_reasons_unregister_subsys(enum skb_drop_reason_subsys subsys)
{
}

/* Netlink extended-ack tracepoint (tracing is off). */
void do_trace_netlink_extack(const char *msg)
{
}

/* No capabilities in RustOS: root has them all. */
bool ns_capable(struct user_namespace *ns, int cap)
{
	return rustos_kpi_current_uid() == 0;
}

bool ns_capable_noaudit(struct user_namespace *ns, int cap)
{
	return ns_capable(ns, cap);
}

bool capable(int cap)
{
	return ns_capable(NULL, cap);
}

bool file_ns_capable(const struct file *file, struct user_namespace *ns, int cap)
{
	return ns_capable(ns, cap);
}

/* Module parameters are set once at boot (src/params.rs). */
void kernel_param_lock(struct module *mod)
{
}

void kernel_param_unlock(struct module *mod)
{
}

/* ------------------------------------------------------- M32 odds and ends */

/* CPU feature bits are not published to Linux code: boot_cpu_has() is
 * false for everything, which only turns off optional paths (e1000e's
 * ART cross-timestamping, for one). */
struct cpuinfo_x86 boot_cpu_data;

/* Static keys are plain variables here (jump labels are off). */
bool static_key_initialized = true;

/* CPU latency requests (PM QoS) have nothing to act on: RustOS does not
 * use deep C-states. */
void cpu_latency_qos_add_request(struct pm_qos_request *req, s32 value)
{
}

void cpu_latency_qos_update_request(struct pm_qos_request *req, s32 new_value)
{
}

void cpu_latency_qos_remove_request(struct pm_qos_request *req)
{
}

/* No DMI (SMBIOS) table matching: per-machine quirk tables match nothing. */
const struct dmi_system_id *dmi_first_match(const struct dmi_system_id *list)
{
	return NULL;
}

int dmi_check_system(const struct dmi_system_id *list)
{
	return 0;
}

/* No DMI strings: quirk tables keyed on them never match. */
const char *dmi_get_system_info(int field)
{
	return NULL;
}

void memset_io(volatile void __iomem *dst, int c, size_t count)
{
	for (size_t i = 0; i < count; i++)
		writeb(c, dst + i);
}

void memcpy_fromio(void *dst, const volatile void __iomem *src, size_t count)
{
	u8 *d = dst;

	for (size_t i = 0; i < count; i++)
		d[i] = readb(src + i);
}

void *vmalloc_array_noprof(size_t n, size_t size)
{
	size_t bytes;

	if (unlikely(check_mul_overflow(n, size, &bytes)))
		return NULL;
	return vmalloc_noprof(bytes);
}

/* ------------------------------------------------------------- seq_file */

/* Text output into a seq_file's buffer (for driver show functions); the
 * buffer is the caller's (there is no procfs here). */
void seq_vprintf(struct seq_file *m, const char *f, va_list args)
{
	int len;

	if (m->count < m->size) {
		len = vsnprintf(m->buf + m->count, m->size - m->count, f, args);
		if (m->count + len < m->size) {
			m->count += len;
			return;
		}
	}
	m->count = m->size;	/* overflow */
}

void seq_printf(struct seq_file *m, const char *f, ...)
{
	va_list args;

	va_start(args, f);
	seq_vprintf(m, f, args);
	va_end(args);
}

void seq_putc(struct seq_file *m, char c)
{
	if (m->count < m->size)
		m->buf[m->count++] = c;
}

int seq_write(struct seq_file *m, const void *data, size_t len)
{
	if (m->count + len < m->size) {
		memcpy(m->buf + m->count, data, len);
		m->count += len;
		return 0;
	}
	m->count = m->size;
	return -1;
}

void __seq_puts(struct seq_file *m, const char *s)
{
	seq_write(m, s, strlen(s));
}

/* No per-CPU-model quirks: x86_match_cpu() tables match nothing (the
 * drivers' Intel SoC workarounds are for Atom-era boards). */
const struct x86_cpu_id *x86_match_cpu(const struct x86_cpu_id *match)
{
	return NULL;
}

bool dmi_match(enum dmi_field f, const char *str)
{
	return false;
}

/* The Intel SoC sideband bus (IOSF MBI) is not available. */
bool iosf_mbi_available(void)
{
	return false;
}

int iosf_mbi_read(u8 port, u8 opcode, u32 offset, u32 *mdr)
{
	return -ENODEV;
}

int iosf_mbi_write(u8 port, u8 opcode, u32 offset, u32 mdr)
{
	return -ENODEV;
}

void add_device_randomness(const void *buf, size_t len)
{
}

void add_input_randomness(unsigned int type, unsigned int code, unsigned int value)
{
}

/* ------------------------------------------------------------- procfs */

/* No /proc for Linux code: entries are created as placeholders so their
 * users carry on (input's /proc/bus/input files, for one). */
static struct {
	char pad[64];
} kpi_proc_placeholder;

struct proc_dir_entry *proc_mkdir(const char *name, struct proc_dir_entry *parent)
{
	return (struct proc_dir_entry *)&kpi_proc_placeholder;
}

struct proc_dir_entry *proc_create(const char *name, umode_t mode, struct proc_dir_entry *parent,
				   const struct proc_ops *proc_ops)
{
	return (struct proc_dir_entry *)&kpi_proc_placeholder;
}

void remove_proc_entry(const char *name, struct proc_dir_entry *parent)
{
}

/* fs/seq_file.c list helpers, and the file operations /proc files use
 * (never called here: no procfs). */
struct list_head *seq_list_start(struct list_head *head, loff_t pos)
{
	struct list_head *lh;

	list_for_each(lh, head)
		if (pos-- == 0)
			return lh;
	return NULL;
}

struct list_head *seq_list_next(void *v, struct list_head *head, loff_t *ppos)
{
	struct list_head *lh = ((struct list_head *)v)->next;

	++*ppos;
	return lh == head ? NULL : lh;
}

ssize_t seq_read(struct file *file, char __user *buf, size_t size, loff_t *ppos)
{
	return -EIO;
}

loff_t seq_lseek(struct file *file, loff_t offset, int whence)
{
	return -ESPIPE;
}

void *__seq_open_private(struct file *f, const struct seq_operations *ops, int psize)
{
	return NULL;
}

int seq_open_private(struct file *filp, const struct seq_operations *ops, int psize)
{
	return -ENOMEM;
}

int seq_release_private(struct inode *inode, struct file *file)
{
	return 0;
}

/* ------------------------------------------------ more procfs (M34 sound) */

struct proc_dir_entry *proc_mkdir_mode(const char *name, umode_t mode,
				       struct proc_dir_entry *parent)
{
	return (struct proc_dir_entry *)&kpi_proc_placeholder;
}

struct proc_dir_entry *proc_create_data(const char *name, umode_t mode,
					struct proc_dir_entry *parent,
					const struct proc_ops *proc_ops, void *data)
{
	return (struct proc_dir_entry *)&kpi_proc_placeholder;
}

struct proc_dir_entry *proc_symlink(const char *name, struct proc_dir_entry *parent,
				    const char *dest)
{
	return (struct proc_dir_entry *)&kpi_proc_placeholder;
}

void proc_set_size(struct proc_dir_entry *de, loff_t size)
{
}

void proc_remove(struct proc_dir_entry *de)
{
}

int single_open(struct file *file, int (*show)(struct seq_file *, void *), void *data)
{
	return -ENOENT;
}

int single_open_size(struct file *file, int (*show)(struct seq_file *, void *), void *data,
		     size_t size)
{
	return -ENOENT;
}

int single_release(struct inode *inode, struct file *file)
{
	return 0;
}

/* --------------------------------------------------- odds (M34 sound) */

/* SIGIO notification is not delivered (O_ASYNC on device files). */
int fasync_helper(int fd, struct file *filp, int on, struct fasync_struct **fapp)
{
	return 0;
}

void kill_fasync(struct fasync_struct **fp, int sig, int band)
{
}

ssize_t memory_read_from_buffer(void *to, size_t count, loff_t *ppos, const void *from,
				size_t available)
{
	loff_t pos = *ppos;

	if (pos < 0)
		return -EINVAL;
	if (pos >= available)
		return 0;
	if (count > available - pos)
		count = available - pos;
	memcpy(to, from + pos, count);
	*ppos = pos + count;
	return count;
}

const struct dmi_device *dmi_find_device(int type, const char *name, const struct dmi_device *from)
{
	return NULL;
}

bool cpu_latency_qos_request_active(struct pm_qos_request *req)
{
	return false;
}

/* Linux pids are not tracked for RustOS threads. */
pid_t pid_vnr(struct pid *pid)
{
	return 0;
}

void put_pid(struct pid *pid)
{
}

/* kernel/time/clocksource.c */
void clocks_calc_mult_shift(u32 *mult, u32 *shift, u32 from, u32 to, u32 maxsec)
{
	u64 tmp;
	u32 sft, sftacc = 32;

	tmp = ((u64)maxsec * from) >> 32;
	while (tmp) {
		tmp >>= 1;
		sftacc--;
	}
	for (sft = 32; sft > 0; sft--) {
		tmp = (u64)to << sft;
		tmp += from / 2;
		do_div(tmp, from);
		if ((tmp >> sftacc) == 0)
			break;
	}
	*mult = tmp;
	*shift = sft;
}

/* ------------------------------------------------------------ irq_work */

/* Run the work at once, with interrupts off as from an interrupt. */
bool irq_work_queue(struct irq_work *work)
{
	unsigned long flags;

	local_irq_save(flags);
	work->func(work);
	local_irq_restore(flags);
	return true;
}


/* There is no IOMMU (DMA addresses are physical): devices use no
 * translation domain, and nothing can be mapped into one. */
int iommu_device_use_default_domain(struct device *dev)
{
	return 0;
}

void iommu_device_unuse_default_domain(struct device *dev)
{
}

int iommu_map(struct iommu_domain *domain, unsigned long iova, phys_addr_t paddr,
	      size_t size, int prot, gfp_t gfp)
{
	return -ENODEV;
}

size_t iommu_unmap(struct iommu_domain *domain, unsigned long iova, size_t size)
{
	return 0;
}

struct iommu_domain *iommu_get_domain_for_dev(struct device *dev)
{
	return NULL;
}
