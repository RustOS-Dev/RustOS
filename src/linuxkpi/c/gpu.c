// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * LinuxKPI pieces the GPU drivers (TTM, the GPU scheduler, amdgpu) need
 * beyond the DRM core:
 *
 * - kthread workers (drm_vblank_work, the DRM commit thread);
 * - shmem and folio odds TTM's backup path uses (there is no swap, so
 *   writeout always fails and TTM keeps the pages);
 * - memory totals, CPU identification, power off / restart requests;
 * - interfaces RustOS has nothing behind, reported as absent: ACPI video
 *   backlight, HDMI CEC, perf PMUs, device coredumps, the VGA arbiter's
 *   decode callbacks.
 */
#include <acpi/video.h>
#include <linux/acpi.h>
#include <linux/devcoredump.h>
#include <linux/kthread.h>
#include <linux/mm.h>
#include <linux/mount.h>
#include <linux/pagemap.h>
#include <linux/perf_event.h>
#include <linux/pid.h>
#include <linux/power_supply.h>
#include <linux/reboot.h>
#include <linux/sched/signal.h>
#include <linux/sched/task.h>
#include <linux/shmem_fs.h>
#include <linux/shrinker.h>
#include <linux/slab.h>
#include <linux/vgaarb.h>
#include <linux/wait.h>
#include <media/cec-notifier.h>
#include <asm/set_memory.h>
#include "kpi.h"

/* ------------------------------------------------------- kthread workers */

void __kthread_init_worker(struct kthread_worker *worker, const char *name,
			   struct lock_class_key *key)
{
	memset(worker, 0, sizeof(*worker));
	raw_spin_lock_init(&worker->lock);
	INIT_LIST_HEAD(&worker->work_list);
	INIT_LIST_HEAD(&worker->delayed_work_list);
}

int kthread_worker_fn(void *worker_ptr)
{
	struct kthread_worker *worker = worker_ptr;
	struct kthread_work *work;

	for (;;) {
		set_current_state(TASK_INTERRUPTIBLE);
		if (kthread_should_stop()) {
			__set_current_state(TASK_RUNNING);
			return 0;
		}
		raw_spin_lock_irq(&worker->lock);
		work = list_first_entry_or_null(&worker->work_list, struct kthread_work, node);
		if (work)
			list_del_init(&work->node);
		worker->current_work = work;
		raw_spin_unlock_irq(&worker->lock);
		if (work) {
			__set_current_state(TASK_RUNNING);
			work->func(work);
		} else {
			schedule();
		}
	}
}

struct kthread_worker *kthread_create_worker_on_node(unsigned int flags, int node,
						     const char namefmt[], ...)
{
	struct kthread_worker *worker = kzalloc(sizeof(*worker), GFP_KERNEL);
	struct task_struct *task;
	char name[TASK_COMM_LEN];
	va_list args;

	if (!worker)
		return ERR_PTR(-ENOMEM);
	kthread_init_worker(worker);
	worker->flags = flags;
	va_start(args, namefmt);
	vsnprintf(name, sizeof(name), namefmt, args);
	va_end(args);
	task = kthread_run(kthread_worker_fn, worker, "%s", name);
	if (IS_ERR(task)) {
		kfree(worker);
		return ERR_CAST(task);
	}
	worker->task = task;
	return worker;
}

static bool kpi_work_pending(struct kthread_work *work)
{
	return !list_empty(&work->node);
}

bool kthread_queue_work(struct kthread_worker *worker, struct kthread_work *work)
{
	unsigned long flags;
	bool queued = false;

	raw_spin_lock_irqsave(&worker->lock, flags);
	if (!kpi_work_pending(work) && !work->canceling) {
		work->worker = worker;
		list_add_tail(&work->node, &worker->work_list);
		queued = true;
	}
	raw_spin_unlock_irqrestore(&worker->lock, flags);
	if (queued && worker->task)
		wake_up_process(worker->task);
	return queued;
}

struct kpi_flush_work {
	struct kthread_work work;
	struct completion done;
};

static void kpi_flush_fn(struct kthread_work *work)
{
	complete(&container_of(work, struct kpi_flush_work, work)->done);
}

/* Wait until @work, queued or running now, has finished: a marker queued
 * right behind it completes when the worker reaches it. */
void kthread_flush_work(struct kthread_work *work)
{
	struct kpi_flush_work fwork = { .work = KTHREAD_WORK_INIT(fwork.work, kpi_flush_fn) };
	struct kthread_worker *worker = work->worker;
	bool wait = false;

	if (!worker)
		return;
	init_completion(&fwork.done);
	raw_spin_lock_irq(&worker->lock);
	fwork.work.worker = worker;
	if (kpi_work_pending(work)) {
		list_add(&fwork.work.node, &work->node);
		wait = true;
	} else if (worker->current_work == work) {
		list_add(&fwork.work.node, &worker->work_list);
		wait = true;
	}
	raw_spin_unlock_irq(&worker->lock);
	if (wait) {
		wake_up_process(worker->task);
		wait_for_completion(&fwork.done);
	}
}

void kthread_flush_worker(struct kthread_worker *worker)
{
	struct kpi_flush_work fwork = { .work = KTHREAD_WORK_INIT(fwork.work, kpi_flush_fn) };

	init_completion(&fwork.done);
	kthread_queue_work(worker, &fwork.work);
	wait_for_completion(&fwork.done);
}

bool kthread_cancel_work_sync(struct kthread_work *work)
{
	struct kthread_worker *worker = work->worker;
	bool pending = false;

	if (!worker)
		return false;
	raw_spin_lock_irq(&worker->lock);
	work->canceling++;
	if (kpi_work_pending(work)) {
		list_del_init(&work->node);
		pending = true;
	}
	raw_spin_unlock_irq(&worker->lock);
	kthread_flush_work(work);
	raw_spin_lock_irq(&worker->lock);
	work->canceling--;
	raw_spin_unlock_irq(&worker->lock);
	return pending;
}

void kthread_destroy_worker(struct kthread_worker *worker)
{
	kthread_flush_worker(worker);
	kthread_stop(worker->task);
	WARN_ON(!list_empty(&worker->work_list));
	kfree(worker);
}

/* ------------------------------------------------------ waits and tasks */

int do_wait_intr(wait_queue_head_t *wq, wait_queue_entry_t *wait)
{
	if (likely(list_empty(&wait->entry)))
		__add_wait_queue_entry_tail(wq, wait);
	set_current_state(TASK_INTERRUPTIBLE);
	if (signal_pending(current))
		return -ERESTARTSYS;
	spin_unlock(&wq->lock);
	schedule();
	spin_lock(&wq->lock);
	return 0;
}

int atomic_dec_and_mutex_lock(atomic_t *cnt, struct mutex *lock)
{
	if (atomic_add_unless(cnt, -1, 1))
		return 0;
	mutex_lock(lock);
	if (!atomic_dec_and_test(cnt)) {
		mutex_unlock(lock);
		return 0;
	}
	return 1;
}

/* lib/refcount.c's (the rest of that file is in misc.c or unused). */
bool refcount_dec_not_one(refcount_t *r)
{
	unsigned int new, val = atomic_read(&r->refs);

	do {
		if (unlikely(val == REFCOUNT_SATURATED))
			return true;
		if (val == 1)
			return false;
		new = val - 1;
		if (new > val) {
			WARN_ONCE(new > val, "refcount_t: underflow; use-after-free.\n");
			return true;
		}
	} while (!atomic_try_cmpxchg_release(&r->refs, &val, new));
	return true;
}

/* The last reference to a task_struct shadow went away. */
void __put_task_struct_rcu_cb(struct rcu_head *rhp)
{
	kfree(container_of(rhp, struct task_struct, rcu));
}

/* Linux pids are not tracked (task_pid() is NULL), so no pid has a task. */
struct task_struct *pid_task(struct pid *pid, enum pid_type type)
{
	return NULL;
}

/* ------------------------------------------------------- memory, folios */

atomic_long_t _totalram_pages;
DEFINE_STATIC_KEY_FALSE(init_on_free);

void si_meminfo(struct sysinfo *val)
{
	u64 free;
	u64 total = rustos_kpi_mem_pages(&free);

	memset(val, 0, sizeof(*val));
	val->totalram = total;
	val->freeram = free;
	val->mem_unit = PAGE_SIZE;
}

int page_is_ram(unsigned long pfn)
{
	return pfn < max_pfn;
}

int is_vmalloc_or_module_addr(const void *x)
{
	return rustos_kpi_is_vmalloc(x);
}

void mark_page_accessed(struct page *page)
{
}

bool set_page_dirty(struct page *page)
{
	return folio_mark_dirty(page_folio(page));
}

/* Memory types are not changed per page (PAT entries for kernel mappings
 * stay write-back); amdgpu APUs share coherent system memory. */
int set_pages_array_uc(struct page **pages, int addrinarray)
{
	return 0;
}

int set_pages_wb(struct page *page, int numpages)
{
	return 0;
}

void __folio_lock(struct folio *folio)
{
	while (test_and_set_bit_lock(PG_locked, folio_flags(folio, 0)))
		cond_resched();
}

void folio_unlock(struct folio *folio)
{
	clear_bit_unlock(PG_locked, folio_flags(folio, 0));
}

bool folio_clear_dirty_for_io(struct folio *folio)
{
	return folio_test_clear_dirty(folio);
}

struct page *shmem_read_mapping_page_gfp(struct address_space *mapping, pgoff_t index,
					 gfp_t gfp)
{
	struct folio *folio = shmem_read_folio_gfp(mapping, index, gfp);

	if (IS_ERR(folio))
		return ERR_CAST(folio);
	return folio_file_page(folio, index);
}

/* Drop the whole pages in [start, end] (end inclusive, -1 for all). */
void shmem_truncate_range(struct inode *inode, loff_t start, loff_t end)
{
	struct address_space *mapping = inode->i_mapping;
	pgoff_t first = DIV_ROUND_UP(start, PAGE_SIZE);
	pgoff_t last = end == (loff_t)-1 ? ULONG_MAX : (end + 1) / PAGE_SIZE - 1;
	unsigned long index;
	struct folio *folio;

	if (end != (loff_t)-1 && (end + 1) / PAGE_SIZE == 0)
		return;
	xa_for_each_range(&mapping->i_pages, index, folio, first, last) {
		xa_erase(&mapping->i_pages, index);
		folio_put(folio);
	}
}

/* There is no swap: the folio stays in memory, dirty and locked. */
int shmem_writeout(struct folio *folio, struct swap_iocb **plug, struct list_head *folio_list)
{
	folio_mark_dirty(folio);
	return -ENOSPC;
}

unsigned long invalidate_mapping_pages(struct address_space *mapping, pgoff_t start,
				       pgoff_t end)
{
	return 0;
}

void unpin_user_page(struct page *page)
{
	put_page(page);
}

/* Memory pressure never calls back into drivers: shrinkers are accepted
 * and kept, but not run. */
struct shrinker *shrinker_alloc(unsigned int flags, const char *fmt, ...)
{
	return kzalloc(sizeof(struct shrinker), GFP_KERNEL);
}

void shrinker_register(struct shrinker *shrinker)
{
}

void shrinker_free(struct shrinker *shrinker)
{
	kfree(shrinker);
}

void kern_unmount(struct vfsmount *mnt)
{
}

/* DMA addresses are physical; a mask below the top of RAM may need
 * bounce buffers (amdgpu then enables its swiotlb paths). */
bool dma_addressing_limited(struct device *dev)
{
	u64 mask = dev->dma_mask ? *dev->dma_mask : dev->coherent_dma_mask;

	return mask < ((u64)max_pfn << PAGE_SHIFT) - 1;
}

/* ---------------------------------------------------------- CPU, power */

DEFINE_PER_CPU_READ_MOSTLY(struct cpuinfo_x86, cpu_info);
unsigned int __num_cores_per_package = 1;

/* Vendor, family and model from CPUID, for drivers' platform quirks.
 * Feature bits stay clear (see boot_cpu_data in misc.c). */
static int __init kpi_cpu_info_init(void)
{
	u32 eax, ebx, ecx, edx, fam, model;
	struct cpuinfo_x86 c = {};
	u64 free;

	cpuid(0, &eax, &ebx, &ecx, &edx);
	c.cpuid_level = eax;
	memcpy(c.x86_vendor_id, &ebx, 4);
	memcpy(c.x86_vendor_id + 4, &edx, 4);
	memcpy(c.x86_vendor_id + 8, &ecx, 4);
	if (!strcmp(c.x86_vendor_id, "GenuineIntel"))
		c.x86_vendor = X86_VENDOR_INTEL;
	else if (!strcmp(c.x86_vendor_id, "AuthenticAMD"))
		c.x86_vendor = X86_VENDOR_AMD;
	else if (!strcmp(c.x86_vendor_id, "HygonGenuine"))
		c.x86_vendor = X86_VENDOR_HYGON;
	else
		c.x86_vendor = X86_VENDOR_UNKNOWN;
	cpuid(1, &eax, &ebx, &ecx, &edx);
	fam = (eax >> 8) & 0xf;
	model = (eax >> 4) & 0xf;
	if (fam == 0xf)
		fam += (eax >> 20) & 0xff;
	if (fam >= 6)
		model += ((eax >> 16) & 0xf) << 4;
	c.x86 = fam;
	c.x86_model = model;
	c.x86_stepping = eax & 0xf;
	for (unsigned int cpu = 0; cpu < nr_cpu_ids; cpu++) {
		per_cpu(cpu_info, cpu) = c;
		per_cpu(cpu_info, cpu).cpu_index = cpu;
	}
	boot_cpu_data.x86_vendor = c.x86_vendor;
	boot_cpu_data.x86 = c.x86;
	boot_cpu_data.x86_model = c.x86_model;
	boot_cpu_data.x86_stepping = c.x86_stepping;
	__num_cores_per_package = rustos_kpi_cpu_count();
	atomic_long_set(&_totalram_pages, rustos_kpi_mem_pages(&free));
	return 0;
}
core_initcall(kpi_cpu_info_init);

void add_taint(unsigned int flag, enum lockdep_ok lockdep_ok)
{
}

void orderly_poweroff(bool force)
{
	pr_emerg("LinuxKPI: a driver requested power off\n");
	rustos_kpi_power(0);
}

void emergency_restart(void)
{
	rustos_kpi_power(1);
}

/* Desktops and laptops on AC alike are reported as mains-powered: battery
 * state is not bridged to the power_supply class. */
int power_supply_is_system_supplied(void)
{
	return 1;
}

static BLOCKING_NOTIFIER_HEAD(kpi_acpi_chain);

/* ACPI notifications (AC adapter, video events) are not delivered to
 * Linux code yet; registrations are kept. */
int register_acpi_notifier(struct notifier_block *nb)
{
	return blocking_notifier_chain_register(&kpi_acpi_chain, nb);
}

int unregister_acpi_notifier(struct notifier_block *nb)
{
	return blocking_notifier_chain_unregister(&kpi_acpi_chain, nb);
}

/* ------------------------------------------- interfaces with no backend */

/* No ACPI video device driver: native backlight control (the GPU's own
 * PWM), as on most systems since 2020, and no ACPI-provided EDIDs. */
enum acpi_backlight_type __acpi_video_get_backlight_type(bool native, bool *auto_detect)
{
	if (auto_detect)
		*auto_detect = true;
	return acpi_backlight_native;
}

int acpi_video_get_edid(struct acpi_device *device, int type, int device_id, void **edid)
{
	return -ENODEV;
}

void acpi_video_register_backlight(void)
{
}

/* No HDMI CEC. As in Linux builds without CEC, registration succeeds with
 * a token pointer that is never dereferenced. */
struct cec_notifier *cec_notifier_conn_register(struct device *hdmi_dev, const char *port_name,
						const struct cec_connector_info *conn_info)
{
	return (struct cec_notifier *)0xdeadfeed;
}

void cec_notifier_conn_unregister(struct cec_notifier *n)
{
}

void cec_notifier_set_phys_addr(struct cec_notifier *n, u16 pa)
{
}

void cec_fill_conn_info_from_drm(struct cec_connector_info *conn_info,
				 const struct drm_connector *connector)
{
	memset(conn_info, 0, sizeof(*conn_info));
}

/* No perf events: amdgpu's PMU is not registered (it carries on). */
int perf_pmu_register(struct pmu *pmu, const char *name, int type)
{
	return -ENODEV;
}

int perf_pmu_unregister(struct pmu *pmu)
{
	return 0;
}

void perf_event_update_userpage(struct perf_event *event)
{
}

/* Device coredumps are not kept: the dump is released at once. */
void dev_coredumpm_timeout(struct device *dev, struct module *owner, void *data,
			   size_t datalen, gfp_t gfp,
			   ssize_t (*read)(char *buffer, loff_t offset, size_t count,
					   void *data, size_t datalen),
			   void (*free)(void *data), unsigned long timeout)
{
	dev_info(dev, "device coredump of %zu bytes discarded\n", datalen);
	free(data);
}

/* Legacy VGA decoding is never rerouted between devices. */
int vga_client_register(struct pci_dev *pdev,
			unsigned int (*set_decode)(struct pci_dev *pdev, bool state))
{
	return 0;
}
