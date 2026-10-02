// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * What the imported device core (drivers/base, lib/kobject.c) needs
 * around it: uevents, ACPI and IRQ-affinity hooks for platform devices,
 * the I/O resource tree, and the subsystems driver_init() starts that
 * RustOS does not have (CPU and container devices, the block class).
 */
#include <linux/radix-tree.h>
#include <linux/vmalloc.h>
#include <linux/acpi.h>
#include <linux/backing-dev-defs.h>
#include <linux/blkdev.h>
#include <linux/cpu.h>
#include <linux/cpuhotplug.h>
#include <linux/device.h>
#include <linux/interrupt.h>
#include <linux/ioport.h>
#include <linux/resource_ext.h>
#include <linux/slab.h>
#include <linux/irq.h>
#include <linux/kobject.h>
#include <linux/platform_device.h>
#include <linux/printk.h>
#include <linux/string.h>
#include <linux/workqueue.h>
#include "kpi.h"

bool initcall_debug;

void dump_stack_lvl(const char *log_lvl)
{
	rustos_kpi_backtrace();
}

void dump_stack(void)
{
	rustos_kpi_backtrace();
}

/* ---------------------------------------------------------------- uevents */

/*
 * Netlink uevents come with M36 (NETLINK_KOBJECT_UEVENT); until then
 * they are dropped, as on a Linux system with no listener.
 */
int kobject_uevent_env(struct kobject *kobj, enum kobject_action action, char *envp_ext[])
{
	return 0;
}

int kobject_uevent(struct kobject *kobj, enum kobject_action action)
{
	return kobject_uevent_env(kobj, action, NULL);
}

int kobject_synth_uevent(struct kobject *kobj, const char *buf, size_t count)
{
	return 0;
}

/* From lib/kobject_uevent.c. */
int add_uevent_var(struct kobj_uevent_env *env, const char *format, ...)
{
	va_list args;
	int len;

	if (env->envp_idx >= ARRAY_SIZE(env->envp)) {
		WARN(1, KERN_ERR "add_uevent_var: too many keys\n");
		return -ENOMEM;
	}
	va_start(args, format);
	len = vsnprintf(&env->buf[env->buflen], sizeof(env->buf) - env->buflen, format, args);
	va_end(args);
	if (len >= (sizeof(env->buf) - env->buflen)) {
		WARN(1, KERN_ERR "add_uevent_var: buffer size too small\n");
		return -ENOMEM;
	}
	env->envp[env->envp_idx++] = &env->buf[env->buflen];
	env->buflen += len + 1;
	return 0;
}

/* ----------------------------------------------- ACPI device-core hooks */

int acpi_device_uevent_modalias(const struct device *dev, struct kobj_uevent_env *env)
{
	return -ENODEV;
}

int acpi_device_modalias(struct device *dev, char *buf, int size)
{
	return -ENODEV;
}

enum dev_dma_attr acpi_get_dma_attr(struct acpi_device *adev)
{
	return DEV_DMA_COHERENT;
}

int acpi_dma_configure_id(struct device *dev, enum dev_dma_attr attr, const u32 *input_id)
{
	return 0;
}

/* --------------------------------------------------------- IRQ affinity */

unsigned int irq_calc_affinity_vectors(unsigned int minvec, unsigned int maxvec,
				       const struct irq_affinity *affd)
{
	return maxvec;
}

struct irq_affinity_desc *irq_create_affinity_masks(unsigned int nvec, struct irq_affinity *affd)
{
	return NULL;
}

int irq_update_affinity_desc(unsigned int irq, struct irq_affinity_desc *affinity)
{
	return 0;
}

/* ------------------------------------------------------------- resources */

struct resource_entry *resource_list_create_entry(struct resource *res, size_t extra_size)
{
	struct resource_entry *entry = kzalloc(sizeof(*entry) + extra_size, GFP_KERNEL);

	if (entry) {
		INIT_LIST_HEAD(&entry->node);
		entry->res = res ? res : &entry->__res;
	}
	return entry;
}

void resource_list_free(struct list_head *head)
{
	struct resource_entry *entry, *tmp;

	list_for_each_entry_safe(entry, tmp, head, node)
		resource_list_destroy_entry(entry);
}

/*
 * The resource tree is not kept: RustOS assigns and tracks BARs itself.
 * Requests always succeed.
 */
struct resource ioport_resource = {
	.name = "PCI IO",
	.start = 0,
	.end = IO_SPACE_LIMIT,
	.flags = IORESOURCE_IO,
};

struct resource iomem_resource = {
	.name = "PCI mem",
	.start = 0,
	.end = -1,
	.flags = IORESOURCE_MEM,
};

int insert_resource(struct resource *parent, struct resource *new)
{
	return 0;
}

int release_resource(struct resource *old)
{
	return 0;
}

int request_resource(struct resource *root, struct resource *new)
{
	return 0;
}

/*
 * Regions are not checked for conflicts (RustOS keeps no resource tree):
 * the caller gets a resource describing what it asked for. They are kept
 * on a list so that __release_region() can free them.
 */
static LIST_HEAD(kpi_regions);
static DEFINE_SPINLOCK(kpi_regions_lock);

struct kpi_region {
	struct list_head link;
	struct resource res;
};

struct resource *__request_region(struct resource *parent, resource_size_t start,
				  resource_size_t n, const char *name, int flags)
{
	struct kpi_region *r = kzalloc(sizeof(*r), GFP_KERNEL);

	if (!r)
		return NULL;
	r->res.start = start;
	r->res.end = start + n - 1;
	r->res.name = name;
	r->res.flags = (parent ? parent->flags & IORESOURCE_TYPE_BITS : 0) | IORESOURCE_BUSY | flags;
	spin_lock(&kpi_regions_lock);
	list_add(&r->link, &kpi_regions);
	spin_unlock(&kpi_regions_lock);
	return &r->res;
}

void __release_region(struct resource *parent, resource_size_t start, resource_size_t n)
{
	struct kpi_region *r, *found = NULL;

	spin_lock(&kpi_regions_lock);
	list_for_each_entry(r, &kpi_regions, link) {
		if (r->res.start == start && resource_size(&r->res) == n) {
			list_del(&r->link);
			found = r;
			break;
		}
	}
	spin_unlock(&kpi_regions_lock);
	kfree(found);
}

struct kpi_devm_region {
	struct resource *parent;
	resource_size_t start, n;
};

static void kpi_devm_region_release(struct device *dev, void *res)
{
	struct kpi_devm_region *d = res;

	__release_region(d->parent, d->start, d->n);
}

static int kpi_devm_region_match(struct device *dev, void *res, void *data)
{
	struct kpi_devm_region *a = res, *b = data;

	return a->parent == b->parent && a->start == b->start && a->n == b->n;
}

struct resource *__devm_request_region(struct device *dev, struct resource *parent,
				      resource_size_t start, resource_size_t n, const char *name)
{
	struct kpi_devm_region *d;
	struct resource *res;

	d = devres_alloc(kpi_devm_region_release, sizeof(*d), GFP_KERNEL);
	if (!d)
		return NULL;
	res = __request_region(parent, start, n, name, 0);
	if (!res) {
		devres_free(d);
		return NULL;
	}
	d->parent = parent;
	d->start = start;
	d->n = n;
	devres_add(dev, d);
	return res;
}

void __devm_release_region(struct device *dev, struct resource *parent, resource_size_t start,
			   resource_size_t n)
{
	struct kpi_devm_region match = { parent, start, n };

	WARN_ON(devres_release(dev, kpi_devm_region_release, kpi_devm_region_match, &match));
}

/* Device coredumps (drivers/base/devcoredump.c): not kept; the driver's
 * buffer is freed as devcoredump does once read. */
void dev_coredumpv(struct device *dev, void *data, size_t datalen, gfp_t gfp)
{
	dev_info(dev, "firmware coredump (%zu bytes) discarded\n", datalen);
	vfree(data);
}

/* --------------------------------------- subsystems driver_init() starts */

struct backing_dev_info noop_backing_dev_info;

int bdi_init(struct backing_dev_info *bdi)
{
	return 0;
}

const struct class block_class = {
	.name = "block",
};

const struct device_type part_type = {
	.name = "partition",
};

void __init cpu_dev_init(void)
{
}

void __init container_dev_init(void)
{
}

void swiotlb_dev_init(struct device *dev)
{
}

void cpufreq_suspend(void)
{
}

bool dev_add_physical_location(struct device *dev)
{
	return false;
}

const struct attribute_group dev_attr_physical_location_group = {
	.name = "physical_location",
};

int __cpuhp_setup_state(enum cpuhp_state state, const char *name, bool invoke,
			int (*startup)(unsigned int cpu), int (*teardown)(unsigned int cpu),
			bool multi_instance)
{
	/* CPUs do not come and go: run the startup callback on each now. */
	int cpu, ret = 0;

	if (invoke && startup)
		for_each_online_cpu(cpu)
			if ((ret = startup(cpu)) < 0)
				return ret;
	return state == CPUHP_AP_ONLINE_DYN ? CPUHP_AP_ONLINE_DYN + 1 : 0;
}

char *file_path(struct file *file, char *buf, int buflen)
{
	return ERR_PTR(-ENAMETOOLONG);
}

int get_cmdline(struct task_struct *task, char *buffer, int buflen)
{
	return 0;
}

struct kobject *kernel_kobj;

/* lib/maple_tree.c, in builds with regmap. */
void __weak maple_tree_init(void)
{
}

/* What start_kernel() sets up for library code (radix trees back IDRs,
 * maple trees back regmap caches), driver_init(), and the /sys/kernel
 * kobject (kernel/ksysfs.c). */
int kpi_devcore_init(void)
{
	radix_tree_init();
	maple_tree_init();
	driver_init();
	kernel_kobj = kobject_create_and_add("kernel", NULL);
	return kernel_kobj ? 0 : -ENOMEM;
}
