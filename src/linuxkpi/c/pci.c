// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * LinuxKPI PCI, interrupts and DMA.
 *
 * A struct pci_dev is created for every device in the RustOS PCI list
 * (src/linuxkpi/pci.rs) and added to the device core under pci0000:00, so
 * it appears in /sys/bus/pci; pci_register_driver() registers a driver
 * with the bus, which matches ID tables and probes.
 *
 * Linux IRQ numbers for PCI devices are KPI_IRQ_BASE + device index;
 * request_irq() routes the device's INTx (through ACPI _PRT) or MSI to a
 * trampoline that runs the handler in hard-IRQ context.
 *
 * There is no IOMMU: DMA addresses are physical addresses, and x86 keeps
 * caches coherent, so the sync calls are barriers.
 */
#include <linux/acpi.h>
#include <linux/dma-mapping.h>
#include <linux/interrupt.h>
#include <linux/io.h>
#include <linux/irq.h>
#include <linux/kthread.h>
#include <linux/pci.h>
#include <linux/slab.h>
#include "kpi.h"

#define KPI_IRQ_BASE	1024
#define KPI_MAX_PCI	256

struct kpi_pci_dev {
	struct pci_dev pdev;
	u32 idx;
};

static struct kpi_pci_dev *kpi_pci[KPI_MAX_PCI];
static u32 kpi_pci_n;
static DEFINE_MUTEX(kpi_pci_lock);


static u32 kpi_idx(const struct pci_dev *dev)
{
	return container_of(dev, struct kpi_pci_dev, pdev)->idx;
}

/* ----------------------------------------------------------- config space */

#define KPI_CFG_READ(name, type, size)						\
int pci_read_config_##name(const struct pci_dev *dev, int where, type *val)	\
{										\
	*val = (type)rustos_kpi_pci_read(kpi_idx(dev), where, size);		\
	return PCIBIOS_SUCCESSFUL;						\
}
#define KPI_CFG_WRITE(name, type, size)						\
int pci_write_config_##name(const struct pci_dev *dev, int where, type val)	\
{										\
	rustos_kpi_pci_write(kpi_idx(dev), where, size, val);			\
	return PCIBIOS_SUCCESSFUL;						\
}
KPI_CFG_READ(byte, u8, 1)
KPI_CFG_READ(word, u16, 2)
KPI_CFG_READ(dword, u32, 4)
KPI_CFG_WRITE(byte, u8, 1)
KPI_CFG_WRITE(word, u16, 2)
KPI_CFG_WRITE(dword, u32, 4)

u8 pci_find_capability(struct pci_dev *dev, int cap)
{
	u16 status;
	u8 pos;
	int ttl = 48;

	pci_read_config_word(dev, PCI_STATUS, &status);
	if (!(status & PCI_STATUS_CAP_LIST))
		return 0;
	pci_read_config_byte(dev, PCI_CAPABILITY_LIST, &pos);
	while (pos >= 0x40 && ttl--) {
		u8 id;

		pos &= ~3;
		pci_read_config_byte(dev, pos + PCI_CAP_LIST_ID, &id);
		if (id == 0xff)
			break;
		if (id == cap)
			return pos;
		pci_read_config_byte(dev, pos + PCI_CAP_LIST_NEXT, &pos);
	}
	return 0;
}

u16 pci_find_ext_capability(struct pci_dev *dev, int cap)
{
	u32 header;
	int pos = PCI_CFG_SPACE_SIZE, ttl = (PCI_CFG_SPACE_EXP_SIZE - PCI_CFG_SPACE_SIZE) / 8;

	if (!pci_is_pcie(dev))
		return 0;
	pci_read_config_dword(dev, pos, &header);
	if (header == 0 || header == 0xffffffff)
		return 0;
	while (ttl-- > 0) {
		if (PCI_EXT_CAP_ID(header) == cap && pos != 0)
			return pos;
		pos = PCI_EXT_CAP_NEXT(header);
		if (pos < PCI_CFG_SPACE_SIZE)
			break;
		pci_read_config_dword(dev, pos, &header);
	}
	return 0;
}

int pcie_capability_read_word(struct pci_dev *dev, int pos, u16 *val)
{
	*val = 0;
	if (!dev->pcie_cap)
		return PCIBIOS_DEVICE_NOT_FOUND;
	return pci_read_config_word(dev, dev->pcie_cap + pos, val);
}

int pcie_capability_read_dword(struct pci_dev *dev, int pos, u32 *val)
{
	*val = 0;
	if (!dev->pcie_cap)
		return PCIBIOS_DEVICE_NOT_FOUND;
	return pci_read_config_dword(dev, dev->pcie_cap + pos, val);
}

int pcie_capability_write_word(struct pci_dev *dev, int pos, u16 val)
{
	if (!dev->pcie_cap)
		return PCIBIOS_DEVICE_NOT_FOUND;
	return pci_write_config_word(dev, dev->pcie_cap + pos, val);
}

int pcie_capability_clear_and_set_word_unlocked(struct pci_dev *dev, int pos, u16 clear, u16 set)
{
	u16 v;

	pcie_capability_read_word(dev, pos, &v);
	return pcie_capability_write_word(dev, pos, (v & ~clear) | set);
}

int pcie_capability_clear_and_set_word_locked(struct pci_dev *dev, int pos, u16 clear, u16 set)
{
	return pcie_capability_clear_and_set_word_unlocked(dev, pos, clear, set);
}

/* --------------------------------------------------------------- devices */

static void kpi_pci_command(struct pci_dev *dev, u16 set, u16 clear)
{
	u16 cmd;

	pci_read_config_word(dev, PCI_COMMAND, &cmd);
	pci_write_config_word(dev, PCI_COMMAND, (cmd & ~clear) | set);
}

static int kpi_pci_enable(struct pci_dev *dev, unsigned long types)
{
	u16 set = 0;

	for (int i = 0; i < PCI_STD_NUM_BARS; i++) {
		unsigned long f = pci_resource_flags(dev, i);

		if ((f & IORESOURCE_MEM) && (types & IORESOURCE_MEM))
			set |= PCI_COMMAND_MEMORY;
		if ((f & IORESOURCE_IO) && (types & IORESOURCE_IO))
			set |= PCI_COMMAND_IO;
	}
	kpi_pci_command(dev, set, 0);
	atomic_inc(&dev->enable_cnt);
	dev->current_state = PCI_D0;
	return 0;
}

int pci_enable_device(struct pci_dev *dev)
{
	return kpi_pci_enable(dev, IORESOURCE_MEM | IORESOURCE_IO);
}

int pci_enable_device_mem(struct pci_dev *dev)
{
	return kpi_pci_enable(dev, IORESOURCE_MEM);
}

void pci_disable_device(struct pci_dev *dev)
{
	if (atomic_dec_return(&dev->enable_cnt) == 0)
		kpi_pci_command(dev, 0, PCI_COMMAND_MASTER);
	dev->is_busmaster = 0;
}

void pci_set_master(struct pci_dev *dev)
{
	kpi_pci_command(dev, PCI_COMMAND_MASTER, 0);
	dev->is_busmaster = 1;
}

void pci_clear_master(struct pci_dev *dev)
{
	kpi_pci_command(dev, 0, PCI_COMMAND_MASTER);
	dev->is_busmaster = 0;
}

int pci_set_mwi(struct pci_dev *dev)
{
	kpi_pci_command(dev, PCI_COMMAND_INVALIDATE, 0);
	return 0;
}

void pci_clear_mwi(struct pci_dev *dev)
{
	kpi_pci_command(dev, 0, PCI_COMMAND_INVALIDATE);
}

int pci_select_bars(struct pci_dev *dev, unsigned long flags)
{
	int bars = 0;

	for (int i = 0; i < PCI_STD_NUM_BARS; i++)
		if (pci_resource_flags(dev, i) & flags)
			bars |= 1 << i;
	return bars;
}

int pci_request_selected_regions(struct pci_dev *dev, int bars, const char *name)
{
	return 0;
}

int pci_request_selected_regions_exclusive(struct pci_dev *dev, int bars, const char *name)
{
	return 0;
}

void pci_release_selected_regions(struct pci_dev *dev, int bars)
{
}

int pci_request_regions(struct pci_dev *dev, const char *name)
{
	return 0;
}

void pci_release_regions(struct pci_dev *dev)
{
}

void __iomem *pci_ioremap_bar(struct pci_dev *pdev, int bar)
{
	if (!(pci_resource_flags(pdev, bar) & IORESOURCE_MEM))
		return NULL;
	return ioremap(pci_resource_start(pdev, bar), pci_resource_len(pdev, bar));
}

void __iomem *pci_iomap(struct pci_dev *dev, int bar, unsigned long maxlen)
{
	unsigned long len = pci_resource_len(dev, bar);

	if (maxlen && len > maxlen)
		len = maxlen;
	if (pci_resource_flags(dev, bar) & IORESOURCE_IO)
		return ioport_map(pci_resource_start(dev, bar), len);
	return ioremap(pci_resource_start(dev, bar), len);
}

int pci_save_state(struct pci_dev *dev)
{
	return 0;
}

void pci_restore_state(struct pci_dev *dev)
{
}

int pci_set_power_state(struct pci_dev *dev, pci_power_t state)
{
	dev->current_state = state;
	return 0;
}

int pci_enable_wake(struct pci_dev *dev, pci_power_t state, bool enable)
{
	return 0;
}

int pci_wake_from_d3(struct pci_dev *dev, bool enable)
{
	return 0;
}

int pcix_get_mmrbc(struct pci_dev *dev)
{
	u16 cmd;
	int cap = pci_find_capability(dev, PCI_CAP_ID_PCIX);

	if (!cap)
		return -EINVAL;
	pci_read_config_word(dev, cap + PCI_X_CMD, &cmd);
	return 512 << ((cmd & PCI_X_CMD_MAX_READ) >> 2);
}

int pcix_set_mmrbc(struct pci_dev *dev, int mmrbc)
{
	return 0;
}

static const struct pci_device_id *kpi_pci_match(const struct pci_device_id *ids,
						 struct pci_dev *dev)
{
	for (; ids && (ids->vendor || ids->subvendor || ids->class_mask); ids++) {
		if ((ids->vendor == PCI_ANY_ID || ids->vendor == dev->vendor) &&
		    (ids->device == PCI_ANY_ID || ids->device == dev->device) &&
		    (ids->subvendor == PCI_ANY_ID || ids->subvendor == dev->subsystem_vendor) &&
		    (ids->subdevice == PCI_ANY_ID || ids->subdevice == dev->subsystem_device) &&
		    !((ids->class ^ dev->class) & ids->class_mask))
			return ids;
	}
	return NULL;
}

static struct device *kpi_pci_root;

/* PCI devices are never hot-removed: nothing to free. */
static void kpi_pci_release(struct device *dev)
{
}

static struct kpi_pci_dev *kpi_pci_create(u32 idx)
{
	struct kpi_pci_info info;
	struct kpi_pci_dev *k;
	struct pci_dev *dev;
	static struct pci_bus *buses[256];

	if (rustos_kpi_pci_get(idx, &info))
		return NULL;
	k = kzalloc(sizeof(*k), GFP_KERNEL);
	if (!k)
		return NULL;
	k->idx = idx;
	dev = &k->pdev;
	if (!buses[info.bus]) {
		buses[info.bus] = kzalloc(sizeof(struct pci_bus), GFP_KERNEL);
		buses[info.bus]->number = info.bus;
		INIT_LIST_HEAD(&buses[info.bus]->devices);
	}
	dev->bus = buses[info.bus];
	dev->devfn = PCI_DEVFN(info.dev, info.func);
	dev->vendor = info.vendor;
	dev->device = info.device;
	dev->subsystem_vendor = info.subvendor;
	dev->subsystem_device = info.subdevice;
	dev->class = info.class;
	dev->revision = info.revision;
	dev->pin = info.irq_pin;
	dev->current_state = PCI_D0;
	dev->dma_mask = DMA_BIT_MASK(32);
	dev->dev.dma_mask = &dev->dma_mask;
	dev->dev.coherent_dma_mask = DMA_BIT_MASK(32);
	device_initialize(&dev->dev);
	dev->dev.bus = &pci_bus_type;
	dev->dev.parent = kpi_pci_root;
	dev->dev.release = kpi_pci_release;
	dev_set_name(&dev->dev, "%04x:%02x:%02x.%d", info.segment, info.bus, info.dev, info.func);
	for (int i = 0; i < PCI_STD_NUM_BARS; i++) {
		struct resource *r = &dev->resource[i];

		if (!info.bars[i].size)
			continue;
		r->start = info.bars[i].start;
		r->end = r->start + info.bars[i].size - 1;
		r->flags = (info.bars[i].flags & 1 ? IORESOURCE_MEM : 0) |
			   (info.bars[i].flags & 2 ? IORESOURCE_IO : 0) |
			   (info.bars[i].flags & 4 ? IORESOURCE_PREFETCH : 0) |
			   (info.bars[i].flags & 8 ? IORESOURCE_MEM_64 : 0);
		r->name = dev_name(&dev->dev);
	}
	dev->pm_cap = pci_find_capability(dev, PCI_CAP_ID_PM);
	dev->pcie_cap = pci_find_capability(dev, PCI_CAP_ID_EXP);
	dev->msi_cap = pci_find_capability(dev, PCI_CAP_ID_MSI);
	dev->msix_cap = pci_find_capability(dev, PCI_CAP_ID_MSIX);
	if (dev->pcie_cap) {
		u16 flags;

		pci_read_config_word(dev, dev->pcie_cap + PCI_EXP_FLAGS, &flags);
		dev->pcie_flags_reg = flags;
	}
	if (info.irq_pin || dev->msi_cap)
		dev->irq = KPI_IRQ_BASE + idx;
	return k;
}

/* ------------------------------------------------------------- the bus */

static int kpi_pci_bus_match(struct device *dev, const struct device_driver *drv)
{
	return kpi_pci_match(to_pci_driver(drv)->id_table, to_pci_dev(dev)) != NULL;
}

static int kpi_pci_bus_probe(struct device *dev)
{
	struct pci_dev *pdev = to_pci_dev(dev);
	struct pci_driver *drv = to_pci_driver(dev->driver);
	const struct pci_device_id *id = kpi_pci_match(drv->id_table, pdev);
	int err;

	if (!id)
		return -ENODEV;
	pdev->driver = drv;
	err = drv->probe(pdev, id);
	if (err)
		pdev->driver = NULL;
	return err;
}

static void kpi_pci_bus_remove(struct device *dev)
{
	struct pci_dev *pdev = to_pci_dev(dev);

	if (pdev->driver && pdev->driver->remove)
		pdev->driver->remove(pdev);
	pdev->driver = NULL;
}

static void kpi_pci_bus_shutdown(struct device *dev)
{
	struct pci_dev *pdev = to_pci_dev(dev);

	if (pdev->driver && pdev->driver->shutdown)
		pdev->driver->shutdown(pdev);
}

static int kpi_pci_bus_uevent(const struct device *dev, struct kobj_uevent_env *env)
{
	const struct pci_dev *pdev = to_pci_dev(dev);

	if (add_uevent_var(env, "PCI_CLASS=%04X", pdev->class) ||
	    add_uevent_var(env, "PCI_ID=%04X:%04X", pdev->vendor, pdev->device) ||
	    add_uevent_var(env, "PCI_SUBSYS_ID=%04X:%04X", pdev->subsystem_vendor,
			   pdev->subsystem_device) ||
	    add_uevent_var(env, "PCI_SLOT_NAME=%s", pci_name(pdev)))
		return -ENOMEM;
	return add_uevent_var(env, "MODALIAS=pci:v%08Xd%08Xsv%08Xsd%08Xbc%02Xsc%02Xi%02X",
			      pdev->vendor, pdev->device, pdev->subsystem_vendor,
			      pdev->subsystem_device, (u8)(pdev->class >> 16),
			      (u8)(pdev->class >> 8), (u8)pdev->class);
}

#define KPI_PCI_ATTR(field, fmt)						static ssize_t field##_show(struct device *dev, struct device_attribute *attr,				    char *buf)						{											return sysfs_emit(buf, fmt, to_pci_dev(dev)->field);			}										static DEVICE_ATTR_RO(field)
KPI_PCI_ATTR(vendor, "0x%04x\n");
KPI_PCI_ATTR(device, "0x%04x\n");
KPI_PCI_ATTR(subsystem_vendor, "0x%04x\n");
KPI_PCI_ATTR(subsystem_device, "0x%04x\n");
KPI_PCI_ATTR(revision, "0x%02x\n");
KPI_PCI_ATTR(class, "0x%06x\n");
KPI_PCI_ATTR(irq, "%u\n");

static struct attribute *kpi_pci_dev_attrs[] = {
	&dev_attr_vendor.attr, &dev_attr_device.attr, &dev_attr_subsystem_vendor.attr,
	&dev_attr_subsystem_device.attr, &dev_attr_revision.attr, &dev_attr_class.attr,
	&dev_attr_irq.attr, NULL,
};
ATTRIBUTE_GROUPS(kpi_pci_dev);

const struct bus_type pci_bus_type = {
	.name = "pci",
	.match = kpi_pci_bus_match,
	.probe = kpi_pci_bus_probe,
	.remove = kpi_pci_bus_remove,
	.shutdown = kpi_pci_bus_shutdown,
	.uevent = kpi_pci_bus_uevent,
	.dev_groups = kpi_pci_dev_groups,
};

/* Register the PCI bus and add every PCI device to the device core. */
int kpi_pci_bus_init(void)
{
	u32 n = rustos_kpi_pci_count(), companions = 0;
	int err = bus_register(&pci_bus_type);

	if (err)
		return err;
	kpi_pci_root = root_device_register("pci0000:00");
	if (IS_ERR(kpi_pci_root))
		return PTR_ERR(kpi_pci_root);
	for (; kpi_pci_n < n && kpi_pci_n < KPI_MAX_PCI; kpi_pci_n++) {
		struct kpi_pci_dev *k = kpi_pci_create(kpi_pci_n);

		kpi_pci[kpi_pci_n] = k;
		if (k)
			kpi_acpi_pci_companion(&k->pdev);
		if (k && device_add(&k->pdev.dev))
			dev_warn(&k->pdev.dev, "cannot add to the device core\n");
		if (k && ACPI_HANDLE(&k->pdev.dev))
			companions++;
	}
	pr_info("linuxkpi: %u PCI devices, %u with ACPI companions\n", kpi_pci_n, companions);
	return 0;
}

int __pci_register_driver(struct pci_driver *drv, struct module *owner, const char *mod_name)
{
	int err;

	drv->driver.name = drv->name;
	drv->driver.bus = &pci_bus_type;
	drv->driver.owner = owner;
	drv->driver.mod_name = mod_name;
	err = driver_register(&drv->driver);
	kpi_netdev_open_pending();
	return err;
}

void pci_unregister_driver(struct pci_driver *drv)
{
	driver_unregister(&drv->driver);
}

struct pci_dev *pci_dev_get(struct pci_dev *dev)
{
	if (dev)
		get_device(&dev->dev);
	return dev;
}

void pci_dev_put(struct pci_dev *dev)
{
	if (dev)
		put_device(&dev->dev);
}

/* ------------------------------------------------------------------- MSI */

int pci_enable_msi(struct pci_dev *dev)
{
	if (!dev->msi_cap || !rustos_kpi_pci_has_msi(kpi_idx(dev)))
		return -EINVAL;
	dev->msi_enabled = 1;
	return 0;
}

void pci_disable_msi(struct pci_dev *dev)
{
	dev->msi_enabled = 0;
}

int pci_alloc_irq_vectors(struct pci_dev *dev, unsigned int min_vecs, unsigned int max_vecs,
			  unsigned int flags)
{
	if ((flags & PCI_IRQ_MSI) && !pci_enable_msi(dev))
		return 1;
	if ((flags & PCI_IRQ_INTX) && dev->pin)
		return 1;
	return -ENOSPC;
}

void pci_free_irq_vectors(struct pci_dev *dev)
{
	dev->msi_enabled = 0;
}

int pci_irq_vector(struct pci_dev *dev, unsigned int nr)
{
	return nr == 0 ? dev->irq : -EINVAL;
}

/* ------------------------------------------------------------- interrupts */

struct kpi_irq {
	irq_handler_t handler;
	irq_handler_t thread_fn;
	void *dev_id;
	const char *name;
	unsigned int irq;
	int routed;		/* vector routed (RustOS cannot unroute) */
	int running;
	int disabled;
	unsigned long thread_pending;
	struct task_struct *thread;
};

static struct kpi_irq kpi_irqs[KPI_MAX_PCI];

static struct kpi_irq *kpi_irq_desc(unsigned int irq)
{
	if (irq < KPI_IRQ_BASE || irq >= KPI_IRQ_BASE + KPI_MAX_PCI)
		return NULL;
	return &kpi_irqs[irq - KPI_IRQ_BASE];
}

static int kpi_irq_thread(void *data)
{
	struct kpi_irq *d = data;

	while (!kthread_should_stop()) {
		set_current_state(TASK_INTERRUPTIBLE);
		if (!test_and_clear_bit(0, &d->thread_pending)) {
			schedule();
			continue;
		}
		__set_current_state(TASK_RUNNING);
		if (d->thread_fn)
			d->thread_fn(d->irq, d->dev_id);
	}
	return 0;
}

/* Interrupt context, interrupts disabled (RustOS sends the EOI after). */
static void kpi_irq_trampoline(void *arg)
{
	struct kpi_irq *d = arg;
	irqreturn_t ret;

	if (!READ_ONCE(d->handler) || READ_ONCE(d->disabled))
		return;
	preempt_count_add(HARDIRQ_OFFSET);
	WRITE_ONCE(d->running, 1);
	ret = d->handler(d->irq, d->dev_id);
	if ((ret & IRQ_WAKE_THREAD) && d->thread) {
		set_bit(0, &d->thread_pending);
		wake_up_process(d->thread);
	}
	WRITE_ONCE(d->running, 0);
	preempt_count_sub(HARDIRQ_OFFSET);
}

static irqreturn_t kpi_default_primary(int irq, void *dev_id)
{
	return IRQ_WAKE_THREAD;
}

int request_threaded_irq(unsigned int irq, irq_handler_t handler, irq_handler_t thread_fn,
			 unsigned long flags, const char *name, void *dev)
{
	struct kpi_irq *d = kpi_irq_desc(irq);
	struct kpi_pci_dev *k;

	if (!d)
		return -EINVAL;
	if (!handler)
		handler = kpi_default_primary;
	k = kpi_pci[irq - KPI_IRQ_BASE];
	d->irq = irq;
	d->dev_id = dev;
	d->name = name;
	d->thread_fn = thread_fn;
	d->disabled = 0;
	if (thread_fn && !d->thread) {
		d->thread = kthread_run(kpi_irq_thread, d, "irq/%u-%s", irq, name);
		if (IS_ERR(d->thread)) {
			d->thread = NULL;
			return -ENOMEM;
		}
	}
	WRITE_ONCE(d->handler, handler);
	if (!d->routed) {
		int v = rustos_kpi_pci_irq(irq - KPI_IRQ_BASE, k && k->pdev.msi_enabled,
					   kpi_irq_trampoline, d);

		if (v < 0) {
			WRITE_ONCE(d->handler, NULL);
			return -ENODEV;
		}
		d->routed = 1;
	}
	return 0;
}

int request_any_context_irq(unsigned int irq, irq_handler_t handler, unsigned long flags,
			    const char *name, void *dev_id)
{
	int r = request_threaded_irq(irq, handler, NULL, flags, name, dev_id);

	return r ? r : IRQC_IS_HARDIRQ;
}

void synchronize_irq(unsigned int irq)
{
	struct kpi_irq *d = kpi_irq_desc(irq);

	while (d && READ_ONCE(d->running))
		cpu_relax();
}

const void *free_irq(unsigned int irq, void *dev_id)
{
	struct kpi_irq *d = kpi_irq_desc(irq);

	if (!d)
		return NULL;
	WRITE_ONCE(d->handler, NULL);
	synchronize_irq(irq);
	return d->name;
}

void disable_irq_nosync(unsigned int irq)
{
	struct kpi_irq *d = kpi_irq_desc(irq);

	if (d)
		WRITE_ONCE(d->disabled, d->disabled + 1);
}

void disable_irq(unsigned int irq)
{
	disable_irq_nosync(irq);
	synchronize_irq(irq);
}

void enable_irq(unsigned int irq)
{
	struct kpi_irq *d = kpi_irq_desc(irq);

	if (d && d->disabled)
		WRITE_ONCE(d->disabled, d->disabled - 1);
}

/* -------------------------------------------------------------------- DMA */

int dma_set_mask(struct device *dev, u64 mask)
{
	if (!dev->dma_mask)
		return -EIO;
	*dev->dma_mask = mask;
	return 0;
}

int dma_set_coherent_mask(struct device *dev, u64 mask)
{
	dev->coherent_dma_mask = mask;
	return 0;
}

void *dma_alloc_attrs(struct device *dev, size_t size, dma_addr_t *dma_handle, gfp_t gfp,
		      unsigned long attrs)
{
	u64 mask = dev ? dev->coherent_dma_mask : DMA_BIT_MASK(32);
	struct page *page;

	if (mask <= DMA_BIT_MASK(32))
		gfp |= __GFP_DMA32;
	page = alloc_pages(gfp | __GFP_ZERO, get_order(size));
	if (!page)
		return NULL;
	*dma_handle = page_to_phys(page);
	return page_address(page);
}

void dma_free_attrs(struct device *dev, size_t size, void *cpu_addr, dma_addr_t dma_handle,
		    unsigned long attrs)
{
	if (cpu_addr)
		free_pages((unsigned long)cpu_addr, get_order(size));
}

dma_addr_t dma_map_page_attrs(struct device *dev, struct page *page, size_t offset, size_t size,
			      enum dma_data_direction dir, unsigned long attrs)
{
	dma_addr_t addr = page_to_phys(page) + offset;
	u64 mask = dev && dev->dma_mask ? *dev->dma_mask : DMA_BIT_MASK(64);

	if (addr + size - 1 > mask) {
		dev_err_once(dev, "DMA address %pad above the device's mask (no bounce buffers yet)\n",
			     &addr);
		return DMA_MAPPING_ERROR;
	}
	return addr;
}

void dma_unmap_page_attrs(struct device *dev, dma_addr_t addr, size_t size,
			  enum dma_data_direction dir, unsigned long attrs)
{
}

void __dma_sync_single_for_cpu(struct device *dev, dma_addr_t addr, size_t size,
			       enum dma_data_direction dir)
{
	mb();
}

void __dma_sync_single_for_device(struct device *dev, dma_addr_t addr, size_t size,
				  enum dma_data_direction dir)
{
	mb();
}

unsigned int dma_map_sg_attrs(struct device *dev, struct scatterlist *sg, int nents,
		     enum dma_data_direction dir, unsigned long attrs)
{
	struct scatterlist *s;
	int i;

	for_each_sg(sg, s, nents, i) {
		s->dma_address = sg_phys(s);
		sg_dma_len(s) = s->length;
	}
	return nents;
}

void dma_unmap_sg_attrs(struct device *dev, struct scatterlist *sg, int nents,
			enum dma_data_direction dir, unsigned long attrs)
{
}
