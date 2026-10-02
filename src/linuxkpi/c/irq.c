// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * Linux interrupt descriptors for interrupts that are not PCI functions'
 * own: ACPI global system interrupts (platform devices' Interrupt()
 * resources) and interrupt domains (GPIO controllers demultiplexing pin
 * interrupts, as pinctrl-amd does for touchpads).
 *
 * The descriptors are Linux's struct irq_desc, so irq_chip drivers and
 * the inline accessors work unchanged; this file is the small subset of
 * kernel/irq that runs them: flow handlers, threaded and one-shot
 * handlers, linear and tree domains (no hierarchies), trigger types.
 *
 * Linux IRQ numbers: PCI devices' are below KPI_VIRQ_BASE (c/pci.c);
 * descriptors here are KPI_VIRQ_BASE + n. A GSI is routed by RustOS
 * (shared with any PCI INTx users of the line) and masked at the I/O APIC
 * while one-shot threads run.
 */
#include <linux/interrupt.h>
#include <linux/irq.h>
#include <linux/irqdesc.h>
#include <linux/irqdomain.h>
#include <linux/kthread.h>
#include <linux/mutex.h>
#include <linux/radix-tree.h>
#include <linux/slab.h>
#include <linux/acpi.h>
#include "kpi.h"

#define KPI_VIRQ_BASE	2048
#define KPI_NR_VIRQ	1024

/* desc->core_internal_state__do_not_mess_with_it */
#define KPI_IS_PENDING	BIT(0)
#define KPI_IS_ONESHOT	BIT(1)	/* an action is IRQF_ONESHOT */
/* action->thread_flags */
#define KPI_TF_RUN	0

static struct irq_desc *kpi_descs[KPI_NR_VIRQ];
static DEFINE_MUTEX(kpi_desc_lock);
static LIST_HEAD(kpi_domains);

static void kpi_noop(struct irq_data *d)
{
}

static unsigned int kpi_noop_ret(struct irq_data *d)
{
	return 0;
}

struct irq_chip no_irq_chip = {
	.name = "none",
	.irq_startup = kpi_noop_ret,
	.irq_shutdown = kpi_noop,
	.irq_enable = kpi_noop,
	.irq_disable = kpi_noop,
	.irq_ack = kpi_noop,
};

struct irq_chip dummy_irq_chip = {
	.name = "dummy",
	.irq_startup = kpi_noop_ret,
	.irq_shutdown = kpi_noop,
	.irq_enable = kpi_noop,
	.irq_disable = kpi_noop,
	.irq_ack = kpi_noop,
	.irq_mask = kpi_noop,
	.irq_unmask = kpi_noop,
};

static bool kpi_is_virq(unsigned int irq)
{
	return irq >= KPI_VIRQ_BASE && irq < KPI_VIRQ_BASE + KPI_NR_VIRQ;
}

struct irq_desc *irq_to_desc(unsigned int irq)
{
	return kpi_is_virq(irq) ? READ_ONCE(kpi_descs[irq - KPI_VIRQ_BASE]) : NULL;
}

static unsigned int kpi_alloc_desc(void)
{
	struct irq_desc *desc;
	unsigned int i;

	desc = kzalloc(sizeof(*desc), GFP_KERNEL);
	if (!desc)
		return 0;
	raw_spin_lock_init(&desc->lock);
	init_waitqueue_head(&desc->wait_for_threads);
	mutex_init(&desc->request_mutex);
	desc->irq_data.common = &desc->irq_common_data;
	desc->irq_data.chip = &no_irq_chip;
	desc->handle_irq = handle_bad_irq;
	desc->depth = 1;
	desc->irq_common_data.state_use_accessors = IRQD_IRQ_DISABLED | IRQD_IRQ_MASKED;
	mutex_lock(&kpi_desc_lock);
	for (i = 0; i < KPI_NR_VIRQ; i++) {
		if (!kpi_descs[i]) {
			desc->irq_data.irq = KPI_VIRQ_BASE + i;
			WRITE_ONCE(kpi_descs[i], desc);
			break;
		}
	}
	mutex_unlock(&kpi_desc_lock);
	if (i == KPI_NR_VIRQ) {
		kfree(desc);
		return 0;
	}
	return KPI_VIRQ_BASE + i;
}

static void kpi_free_desc(unsigned int irq)
{
	struct irq_desc *desc = irq_to_desc(irq);

	if (!desc)
		return;
	mutex_lock(&kpi_desc_lock);
	WRITE_ONCE(kpi_descs[irq - KPI_VIRQ_BASE], NULL);
	mutex_unlock(&kpi_desc_lock);
	kfree(desc);
}

/* ------------------------------------------------------------ chip calls */

static void kpi_state_set(struct irq_desc *desc, u32 bits)
{
	desc->irq_common_data.state_use_accessors |= bits;
}

static void kpi_state_clr(struct irq_desc *desc, u32 bits)
{
	desc->irq_common_data.state_use_accessors &= ~bits;
}

static void kpi_mask(struct irq_desc *desc)
{
	struct irq_chip *chip = desc->irq_data.chip;

	if (irqd_irq_masked(&desc->irq_data))
		return;
	if (chip->irq_mask)
		chip->irq_mask(&desc->irq_data);
	kpi_state_set(desc, IRQD_IRQ_MASKED);
}

static void kpi_unmask(struct irq_desc *desc)
{
	struct irq_chip *chip = desc->irq_data.chip;

	if (!irqd_irq_masked(&desc->irq_data))
		return;
	if (chip->irq_unmask)
		chip->irq_unmask(&desc->irq_data);
	kpi_state_clr(desc, IRQD_IRQ_MASKED);
}

static void kpi_mask_ack(struct irq_desc *desc)
{
	struct irq_chip *chip = desc->irq_data.chip;

	if (chip->irq_mask_ack && !irqd_irq_masked(&desc->irq_data)) {
		chip->irq_mask_ack(&desc->irq_data);
		kpi_state_set(desc, IRQD_IRQ_MASKED);
		return;
	}
	kpi_mask(desc);
	if (chip->irq_ack)
		chip->irq_ack(&desc->irq_data);
}

static void kpi_startup(struct irq_desc *desc)
{
	struct irq_chip *chip = desc->irq_data.chip;

	desc->depth = 0;
	kpi_state_clr(desc, IRQD_IRQ_DISABLED);
	if (chip->irq_startup) {
		chip->irq_startup(&desc->irq_data);
		kpi_state_clr(desc, IRQD_IRQ_MASKED);
	} else if (chip->irq_enable) {
		chip->irq_enable(&desc->irq_data);
		kpi_state_clr(desc, IRQD_IRQ_MASKED);
	} else {
		kpi_unmask(desc);
	}
	kpi_state_set(desc, IRQD_IRQ_STARTED);
}

static void kpi_shutdown(struct irq_desc *desc)
{
	struct irq_chip *chip = desc->irq_data.chip;

	desc->depth = 1;
	kpi_state_set(desc, IRQD_IRQ_DISABLED);
	if (chip->irq_shutdown)
		chip->irq_shutdown(&desc->irq_data);
	else if (chip->irq_disable)
		chip->irq_disable(&desc->irq_data);
	else
		kpi_mask(desc);
	kpi_state_set(desc, IRQD_IRQ_MASKED);
	kpi_state_clr(desc, IRQD_IRQ_STARTED);
}

static void kpi_bus_lock(struct irq_desc *desc)
{
	struct irq_chip *chip = desc->irq_data.chip;

	if (chip->irq_bus_lock)
		chip->irq_bus_lock(&desc->irq_data);
}

static void kpi_bus_sync_unlock(struct irq_desc *desc)
{
	struct irq_chip *chip = desc->irq_data.chip;

	if (chip->irq_bus_sync_unlock)
		chip->irq_bus_sync_unlock(&desc->irq_data);
}

static int kpi_set_type(struct irq_desc *desc, unsigned int type)
{
	struct irq_chip *chip = desc->irq_data.chip;
	int ret = 0;

	type &= IRQ_TYPE_SENSE_MASK;
	if (type == IRQ_TYPE_NONE)
		return 0;
	if (chip->irq_set_type)
		ret = chip->irq_set_type(&desc->irq_data, type);
	if (ret == 0 || ret == IRQ_SET_MASK_OK_NOCOPY) {
		irqd_set_trigger_type(&desc->irq_data, type);
		if (type & IRQ_TYPE_LEVEL_MASK)
			kpi_state_set(desc, IRQD_LEVEL);
		else
			kpi_state_clr(desc, IRQD_LEVEL);
		ret = 0;
	}
	return ret;
}

/* ---------------------------------------------------------- flow handlers */

static void kpi_wake_thread(struct irq_desc *desc, struct irqaction *action)
{
	if (test_and_set_bit(KPI_TF_RUN, &action->thread_flags))
		return;
	desc->threads_oneshot |= action->thread_mask;
	atomic_inc(&desc->threads_active);
	wake_up_process(action->thread);
}

/* Run the handlers; desc->lock held, dropped around them. */
static irqreturn_t kpi_handle_event(struct irq_desc *desc)
{
	unsigned int irq = desc->irq_data.irq;
	irqreturn_t ret = IRQ_NONE;
	struct irqaction *action;

	desc->core_internal_state__do_not_mess_with_it &= ~KPI_IS_PENDING;
	kpi_state_set(desc, IRQD_IRQ_INPROGRESS);
	raw_spin_unlock(&desc->lock);
	for (action = desc->action; action; action = action->next) {
		irqreturn_t r = action->handler(irq, action->dev_id);

		if (r == IRQ_WAKE_THREAD && action->thread)
			kpi_wake_thread(desc, action);
		ret |= r;
	}
	raw_spin_lock(&desc->lock);
	kpi_state_clr(desc, IRQD_IRQ_INPROGRESS);
	desc->tot_count++;
	return ret;
}

static bool kpi_can_handle(struct irq_desc *desc)
{
	if (!desc->action || irqd_irq_disabled(&desc->irq_data)) {
		desc->core_internal_state__do_not_mess_with_it |= KPI_IS_PENDING;
		return false;
	}
	return true;
}

static void kpi_cond_unmask(struct irq_desc *desc)
{
	if (!irqd_irq_disabled(&desc->irq_data) && irqd_irq_masked(&desc->irq_data) &&
	    !desc->threads_oneshot)
		kpi_unmask(desc);
}

void handle_level_irq(struct irq_desc *desc)
{
	raw_spin_lock(&desc->lock);
	kpi_mask_ack(desc);
	if (kpi_can_handle(desc)) {
		kpi_handle_event(desc);
		kpi_cond_unmask(desc);
	}
	raw_spin_unlock(&desc->lock);
}

void handle_edge_irq(struct irq_desc *desc)
{
	struct irq_chip *chip = desc->irq_data.chip;

	raw_spin_lock(&desc->lock);
	if (!kpi_can_handle(desc)) {
		kpi_mask_ack(desc);
		raw_spin_unlock(&desc->lock);
		return;
	}
	if (chip->irq_ack)
		chip->irq_ack(&desc->irq_data);
	kpi_handle_event(desc);
	raw_spin_unlock(&desc->lock);
}

void handle_fasteoi_irq(struct irq_desc *desc)
{
	struct irq_chip *chip = desc->irq_data.chip;

	raw_spin_lock(&desc->lock);
	if (!kpi_can_handle(desc)) {
		kpi_mask(desc);
		goto out;
	}
	if (desc->core_internal_state__do_not_mess_with_it & KPI_IS_ONESHOT)
		kpi_mask(desc);
	kpi_handle_event(desc);
	kpi_cond_unmask(desc);
out:
	if (chip->irq_eoi)
		chip->irq_eoi(&desc->irq_data);
	raw_spin_unlock(&desc->lock);
}

void handle_simple_irq(struct irq_desc *desc)
{
	raw_spin_lock(&desc->lock);
	if (kpi_can_handle(desc))
		kpi_handle_event(desc);
	raw_spin_unlock(&desc->lock);
}

void handle_untracked_irq(struct irq_desc *desc)
{
	handle_simple_irq(desc);
}

/* A child interrupt handled from its parent's thread (I2C GPIO expanders). */
void handle_nested_irq(unsigned int irq)
{
	struct irq_desc *desc = irq_to_desc(irq);
	struct irqaction *action;

	if (!desc)
		return;
	raw_spin_lock_irq(&desc->lock);
	action = desc->action;
	if (!action || irqd_irq_disabled(&desc->irq_data)) {
		desc->core_internal_state__do_not_mess_with_it |= KPI_IS_PENDING;
		raw_spin_unlock_irq(&desc->lock);
		return;
	}
	kpi_state_set(desc, IRQD_IRQ_INPROGRESS);
	raw_spin_unlock_irq(&desc->lock);
	for (; action; action = action->next)
		if (action->thread_fn)
			action->thread_fn(irq, action->dev_id);
	raw_spin_lock_irq(&desc->lock);
	kpi_state_clr(desc, IRQD_IRQ_INPROGRESS);
	raw_spin_unlock_irq(&desc->lock);
}

void handle_bad_irq(struct irq_desc *desc)
{
	pr_warn_ratelimited("irq %u: no handler\n", desc->irq_data.irq);
	if (desc->irq_data.chip->irq_ack)
		desc->irq_data.chip->irq_ack(&desc->irq_data);
}

int handle_irq_desc(struct irq_desc *desc)
{
	if (!desc)
		return -EINVAL;
	desc->handle_irq(desc);
	return 0;
}

int generic_handle_irq(unsigned int irq)
{
	return handle_irq_desc(irq_to_desc(irq));
}

int generic_handle_irq_safe(unsigned int irq)
{
	unsigned long flags;
	int ret;

	local_irq_save(flags);
	ret = generic_handle_irq(irq);
	local_irq_restore(flags);
	return ret;
}

int generic_handle_domain_irq(struct irq_domain *domain, unsigned int hwirq)
{
	return handle_irq_desc(__irq_resolve_mapping(domain, hwirq, NULL));
}

int generic_handle_domain_irq_safe(struct irq_domain *domain, unsigned int hwirq)
{
	unsigned long flags;
	int ret;

	local_irq_save(flags);
	ret = generic_handle_domain_irq(domain, hwirq);
	local_irq_restore(flags);
	return ret;
}

/* ------------------------------------------------------- descriptor setup */

int irq_set_chip(unsigned int irq, const struct irq_chip *chip)
{
	struct irq_desc *desc = irq_to_desc(irq);

	if (!desc)
		return -EINVAL;
	desc->irq_data.chip = (struct irq_chip *)(chip ?: &no_irq_chip);
	return 0;
}

int irq_set_chip_data(unsigned int irq, void *data)
{
	struct irq_desc *desc = irq_to_desc(irq);

	if (!desc)
		return -EINVAL;
	desc->irq_data.chip_data = data;
	return 0;
}

int irq_set_handler_data(unsigned int irq, void *data)
{
	struct irq_desc *desc = irq_to_desc(irq);

	if (!desc)
		return -EINVAL;
	desc->irq_common_data.handler_data = data;
	return 0;
}

struct irq_data *irq_get_irq_data(unsigned int irq)
{
	struct irq_desc *desc = irq_to_desc(irq);

	return desc ? &desc->irq_data : NULL;
}

int irq_set_parent(int irq, int parent_irq)
{
	struct irq_desc *desc = irq_to_desc(irq);

	if (!desc)
		return -EINVAL;
	desc->parent_irq = parent_irq;
	return 0;
}

void __irq_set_handler(unsigned int irq, irq_flow_handler_t handle, int is_chained,
		       const char *name)
{
	struct irq_desc *desc = irq_to_desc(irq);
	unsigned long flags;

	if (!desc)
		return;
	raw_spin_lock_irqsave(&desc->lock, flags);
	desc->handle_irq = handle ?: handle_bad_irq;
	desc->name = name;
	if (handle && handle != handle_bad_irq && is_chained) {
		desc->status_use_accessors |= IRQ_NOREQUEST | IRQ_NOPROBE | IRQ_NOTHREAD;
		kpi_startup(desc);
	}
	raw_spin_unlock_irqrestore(&desc->lock, flags);
}

void irq_set_chip_and_handler_name(unsigned int irq, const struct irq_chip *chip,
				   irq_flow_handler_t handle, const char *name)
{
	irq_set_chip(irq, chip);
	__irq_set_handler(irq, handle, 0, name);
}

void irq_set_chained_handler_and_data(unsigned int irq, irq_flow_handler_t handle, void *data)
{
	irq_set_handler_data(irq, data);
	__irq_set_handler(irq, handle, 1, NULL);
}

void irq_modify_status(unsigned int irq, unsigned long clr, unsigned long set)
{
	struct irq_desc *desc = irq_to_desc(irq);
	unsigned long flags;

	if (!desc)
		return;
	raw_spin_lock_irqsave(&desc->lock, flags);
	desc->status_use_accessors = (desc->status_use_accessors & ~clr) | set;
	raw_spin_unlock_irqrestore(&desc->lock, flags);
}

int irq_set_irq_type(unsigned int irq, unsigned int type)
{
	struct irq_desc *desc = irq_to_desc(irq);
	unsigned long flags;
	int ret;

	if (!desc)
		return 0;	/* PCI: fixed by the bus */
	kpi_bus_lock(desc);
	raw_spin_lock_irqsave(&desc->lock, flags);
	ret = kpi_set_type(desc, type);
	raw_spin_unlock_irqrestore(&desc->lock, flags);
	kpi_bus_sync_unlock(desc);
	return ret;
}

/* Interrupts do not wake the system (no suspend); chips still hear it. */
int irq_set_irq_wake(unsigned int irq, unsigned int on)
{
	struct irq_desc *desc = irq_to_desc(irq);
	struct irq_chip *chip;

	if (!desc)
		return 0;
	chip = desc->irq_data.chip;
	if (on ? desc->wake_depth++ == 0 : desc->wake_depth && --desc->wake_depth == 0) {
		if (chip->irq_set_wake)
			chip->irq_set_wake(&desc->irq_data, on);
	}
	return 0;
}

bool irq_check_status_bit(unsigned int irq, unsigned int bitmask)
{
	struct irq_desc *desc = irq_to_desc(irq);

	return desc && (desc->status_use_accessors & bitmask);
}

/* ------------------------------------------------------- request and free */

static irqreturn_t kpi_default_primary_handler(int irq, void *dev_id)
{
	return IRQ_WAKE_THREAD;
}

static irqreturn_t kpi_nested_primary_handler(int irq, void *dev_id)
{
	WARN(1, "primary handler called for nested irq %d\n", irq);
	return IRQ_NONE;
}

static void kpi_finalize_oneshot(struct irq_desc *desc, struct irqaction *action)
{
	if (!(action->flags & IRQF_ONESHOT))
		return;
	kpi_bus_lock(desc);
	raw_spin_lock_irq(&desc->lock);
	/* Retriggered while the thread ran: it runs again, line stays masked. */
	if (!test_bit(KPI_TF_RUN, &action->thread_flags)) {
		desc->threads_oneshot &= ~action->thread_mask;
		kpi_cond_unmask(desc);
	}
	raw_spin_unlock_irq(&desc->lock);
	kpi_bus_sync_unlock(desc);
}

static int kpi_irq_thread(void *data)
{
	struct irqaction *action = data;
	struct irq_desc *desc = irq_to_desc(action->irq);

	while (!kthread_should_stop()) {
		set_current_state(TASK_INTERRUPTIBLE);
		if (!test_and_clear_bit(KPI_TF_RUN, &action->thread_flags)) {
			schedule();
			continue;
		}
		__set_current_state(TASK_RUNNING);
		action->thread_fn(action->irq, action->dev_id);
		kpi_finalize_oneshot(desc, action);
		if (atomic_dec_and_test(&desc->threads_active))
			wake_up(&desc->wait_for_threads);
	}
	return 0;
}

static int kpi_virq_request(unsigned int irq, irq_handler_t handler, irq_handler_t thread_fn,
			    unsigned long flags, const char *name, void *dev_id)
{
	struct irq_desc *desc = irq_to_desc(irq);
	struct irq_chip *chip;
	struct irqaction *action, **p;
	unsigned long used = 0, irqflags;
	bool nested;
	int ret = 0;

	if (!desc || (desc->status_use_accessors & IRQ_NOREQUEST))
		return -EINVAL;
	if (!handler && !thread_fn)
		return -EINVAL;
	nested = desc->status_use_accessors & IRQ_NESTED_THREAD;
	if (nested) {
		if (!thread_fn)
			return -EINVAL;
		handler = kpi_nested_primary_handler;
	} else if (!handler) {
		handler = kpi_default_primary_handler;
		flags |= IRQF_ONESHOT;
	}
	action = kzalloc(sizeof(*action), GFP_KERNEL);
	if (!action)
		return -ENOMEM;
	action->handler = handler;
	action->thread_fn = thread_fn;
	action->flags = flags;
	action->name = name;
	action->dev_id = dev_id;
	action->irq = irq;
	if (thread_fn && !nested) {
		action->thread = kthread_create(kpi_irq_thread, action, "irq/%u-%s", irq, name);
		if (IS_ERR(action->thread)) {
			kfree(action);
			return -ENOMEM;
		}
	}

	chip = desc->irq_data.chip;
	mutex_lock(&desc->request_mutex);
	if (!desc->action && chip->irq_request_resources) {
		ret = chip->irq_request_resources(&desc->irq_data);
		if (ret)
			goto out_mutex;
	}
	kpi_bus_lock(desc);
	raw_spin_lock_irqsave(&desc->lock, irqflags);
	if (desc->action && !(desc->action->flags & flags & IRQF_SHARED)) {
		ret = -EBUSY;
		goto out_unlock;
	}
	for (p = &desc->action; *p; p = &(*p)->next)
		used |= (*p)->thread_mask;
	if (flags & IRQF_ONESHOT) {
		if (used == ~0UL) {
			ret = -EBUSY;
			goto out_unlock;
		}
		action->thread_mask = 1UL << ffz(used);
		desc->core_internal_state__do_not_mess_with_it |= KPI_IS_ONESHOT;
	}
	*p = action;
	if (p == &desc->action) {
		if (flags & IRQF_TRIGGER_MASK)
			ret = kpi_set_type(desc, flags & IRQF_TRIGGER_MASK);
		if (!ret && !(flags & IRQF_NO_AUTOEN))
			kpi_startup(desc);
		if (ret)
			desc->action = NULL;
	}
out_unlock:
	raw_spin_unlock_irqrestore(&desc->lock, irqflags);
	kpi_bus_sync_unlock(desc);
	if (ret && !desc->action && chip->irq_release_resources)
		chip->irq_release_resources(&desc->irq_data);
out_mutex:
	mutex_unlock(&desc->request_mutex);
	if (ret) {
		if (action->thread)
			kthread_stop(action->thread);
		kfree(action);
		return ret;
	}
	if (action->thread)
		wake_up_process(action->thread);
	return 0;
}

static void kpi_virq_synchronize(struct irq_desc *desc)
{
	while (irqd_irq_inprogress(&desc->irq_data))
		cpu_relax();
	wait_event(desc->wait_for_threads, !atomic_read(&desc->threads_active));
}

static const void *kpi_virq_free(unsigned int irq, void *dev_id)
{
	struct irq_desc *desc = irq_to_desc(irq);
	struct irqaction *action = NULL, **p;
	unsigned long flags;
	const char *name;

	mutex_lock(&desc->request_mutex);
	kpi_bus_lock(desc);
	raw_spin_lock_irqsave(&desc->lock, flags);
	for (p = &desc->action; *p; p = &(*p)->next) {
		if ((*p)->dev_id == dev_id) {
			action = *p;
			*p = action->next;
			break;
		}
	}
	if (action && !desc->action) {
		kpi_shutdown(desc);
		desc->core_internal_state__do_not_mess_with_it &= ~KPI_IS_ONESHOT;
	}
	raw_spin_unlock_irqrestore(&desc->lock, flags);
	kpi_bus_sync_unlock(desc);
	if (!action) {
		mutex_unlock(&desc->request_mutex);
		WARN(1, "trying to free already-free IRQ %u\n", irq);
		return NULL;
	}
	kpi_virq_synchronize(desc);
	if (action->thread)
		kthread_stop(action->thread);
	if (!desc->action && desc->irq_data.chip->irq_release_resources)
		desc->irq_data.chip->irq_release_resources(&desc->irq_data);
	mutex_unlock(&desc->request_mutex);
	name = action->name;
	kfree(action);
	return name;
}

int request_threaded_irq(unsigned int irq, irq_handler_t handler, irq_handler_t thread_fn,
			 unsigned long flags, const char *name, void *dev)
{
	if (kpi_is_virq(irq))
		return kpi_virq_request(irq, handler, thread_fn, flags, name, dev);
	return kpi_pci_request_irq(irq, handler, thread_fn, flags, name, dev);
}

int request_any_context_irq(unsigned int irq, irq_handler_t handler, unsigned long flags,
			    const char *name, void *dev_id)
{
	int r = request_threaded_irq(irq, handler, NULL, flags, name, dev_id);

	return r ? r : IRQC_IS_HARDIRQ;
}

const void *free_irq(unsigned int irq, void *dev_id)
{
	if (kpi_is_virq(irq))
		return irq_to_desc(irq) ? kpi_virq_free(irq, dev_id) : NULL;
	return kpi_pci_free_irq(irq, dev_id);
}

void synchronize_irq(unsigned int irq)
{
	struct irq_desc *desc = irq_to_desc(irq);

	if (desc)
		kpi_virq_synchronize(desc);
	else if (!kpi_is_virq(irq))
		kpi_pci_synchronize_irq(irq);
}

void disable_irq_nosync(unsigned int irq)
{
	struct irq_desc *desc = irq_to_desc(irq);
	unsigned long flags;

	if (!kpi_is_virq(irq)) {
		kpi_pci_disable_irq(irq);
		return;
	}
	if (!desc)
		return;
	kpi_bus_lock(desc);
	raw_spin_lock_irqsave(&desc->lock, flags);
	if (desc->depth++ == 0) {
		struct irq_chip *chip = desc->irq_data.chip;

		kpi_state_set(desc, IRQD_IRQ_DISABLED);
		if (chip->irq_disable) {
			chip->irq_disable(&desc->irq_data);
			kpi_state_set(desc, IRQD_IRQ_MASKED);
		} else {
			kpi_mask(desc);
		}
	}
	raw_spin_unlock_irqrestore(&desc->lock, flags);
	kpi_bus_sync_unlock(desc);
}

void disable_irq(unsigned int irq)
{
	disable_irq_nosync(irq);
	synchronize_irq(irq);
}

bool disable_hardirq(unsigned int irq)
{
	disable_irq_nosync(irq);
	return true;
}

void enable_irq(unsigned int irq)
{
	struct irq_desc *desc = irq_to_desc(irq);
	unsigned long flags;
	bool resend = false;

	if (!kpi_is_virq(irq)) {
		kpi_pci_enable_irq(irq);
		return;
	}
	if (!desc)
		return;
	kpi_bus_lock(desc);
	raw_spin_lock_irqsave(&desc->lock, flags);
	if (desc->depth && --desc->depth == 0) {
		struct irq_chip *chip = desc->irq_data.chip;

		kpi_state_clr(desc, IRQD_IRQ_DISABLED);
		if (chip->irq_enable) {
			chip->irq_enable(&desc->irq_data);
			kpi_state_clr(desc, IRQD_IRQ_MASKED);
		} else {
			kpi_unmask(desc);
		}
		/* An edge that came while disabled is not seen again: replay it. */
		resend = (desc->core_internal_state__do_not_mess_with_it & KPI_IS_PENDING) &&
			 !irqd_is_level_type(&desc->irq_data);
	}
	raw_spin_unlock_irqrestore(&desc->lock, flags);
	kpi_bus_sync_unlock(desc);
	if (resend) {
		local_irq_save(flags);
		desc->handle_irq(desc);
		local_irq_restore(flags);
	}
}

/* -------------------------------------------------------------- domains */

struct irq_domain *irq_domain_instantiate(const struct irq_domain_info *info)
{
	struct irq_domain *d;
	int err;

#ifdef CONFIG_IRQ_DOMAIN_HIERARCHY
	if (info->parent)
		return ERR_PTR(-EOPNOTSUPP);
#endif
	d = kzalloc(struct_size(d, revmap, info->size), GFP_KERNEL);
	if (!d)
		return ERR_PTR(-ENOMEM);
	d->ops = info->ops;
	d->host_data = info->host_data;
	d->fwnode = info->fwnode;
	d->bus_token = info->bus_token;
	d->dev = info->dev;
	d->hwirq_max = info->hwirq_max ?: info->size;
	d->revmap_size = info->size;
	d->flags = info->domain_flags;
	d->root = d;
	d->exit = info->exit;
	d->name = info->dev ? dev_name(info->dev) : "kpi";
	mutex_init(&d->mutex);
	INIT_RADIX_TREE(&d->revmap_tree, GFP_KERNEL);
	if (info->init) {
		err = info->init(d);
		if (err) {
			kfree(d);
			return ERR_PTR(err);
		}
	}
	mutex_lock(&kpi_desc_lock);
	list_add(&d->link, &kpi_domains);
	mutex_unlock(&kpi_desc_lock);
	if (info->virq_base)
		pr_warn("irq domain %s: legacy IRQ ranges are not supported\n", d->name);
	return d;
}

static void kpi_devm_domain_release(struct device *dev, void *res)
{
	irq_domain_remove(*(struct irq_domain **)res);
}

struct irq_domain *devm_irq_domain_instantiate(struct device *dev,
					       const struct irq_domain_info *info)
{
	struct irq_domain **dr, *d;

	dr = devres_alloc(kpi_devm_domain_release, sizeof(*dr), GFP_KERNEL);
	if (!dr)
		return ERR_PTR(-ENOMEM);
	d = irq_domain_instantiate(info);
	if (IS_ERR(d)) {
		devres_free(dr);
		return d;
	}
	*dr = d;
	devres_add(dev, dr);
	return d;
}

struct irq_domain *irq_domain_create_simple(struct fwnode_handle *fwnode, unsigned int size,
					    unsigned int first_irq,
					    const struct irq_domain_ops *ops, void *host_data)
{
	struct irq_domain_info info = {
		.fwnode = fwnode,
		.size = size,
		.hwirq_max = size,
		.ops = ops,
		.host_data = host_data,
	};
	struct irq_domain *d = irq_domain_instantiate(&info);

	return IS_ERR(d) ? NULL : d;
}

static struct irq_data **kpi_revmap_slot(struct irq_domain *d, irq_hw_number_t hwirq)
{
	if (hwirq < d->revmap_size)
		return &d->revmap[hwirq];
	return NULL;
}

int irq_domain_associate(struct irq_domain *domain, unsigned int virq, irq_hw_number_t hwirq)
{
	struct irq_desc *desc = irq_to_desc(virq);
	struct irq_data **slot;
	int ret;

	if (!desc || hwirq >= domain->hwirq_max)
		return -EINVAL;
	mutex_lock(&domain->mutex);
	desc->irq_data.hwirq = hwirq;
	desc->irq_data.domain = domain;
	if (domain->ops->map) {
		ret = domain->ops->map(domain, virq, hwirq);
		if (ret) {
			desc->irq_data.domain = NULL;
			desc->irq_data.hwirq = 0;
			mutex_unlock(&domain->mutex);
			return ret;
		}
	}
	slot = kpi_revmap_slot(domain, hwirq);
	if (slot)
		rcu_assign_pointer(*slot, &desc->irq_data);
	else
		radix_tree_insert(&domain->revmap_tree, hwirq, &desc->irq_data);
	domain->mapcount++;
	mutex_unlock(&domain->mutex);
	return 0;
}

struct irq_desc *__irq_resolve_mapping(struct irq_domain *domain, irq_hw_number_t hwirq,
				       unsigned int *irq)
{
	struct irq_data **slot, *data;

	if (!domain)
		return NULL;
	slot = kpi_revmap_slot(domain, hwirq);
	data = slot ? rcu_dereference_raw(*slot) : radix_tree_lookup(&domain->revmap_tree, hwirq);
	if (!data)
		return NULL;
	if (irq)
		*irq = data->irq;
	return irq_data_to_desc(data);
}

unsigned int irq_create_mapping_affinity(struct irq_domain *domain, irq_hw_number_t hwirq,
					 const struct irq_affinity_desc *affinity)
{
	unsigned int virq;

	if (!domain)
		return 0;
	if (__irq_resolve_mapping(domain, hwirq, &virq))
		return virq;
	virq = kpi_alloc_desc();
	if (!virq)
		return 0;
	if (irq_domain_associate(domain, virq, hwirq)) {
		kpi_free_desc(virq);
		return 0;
	}
	return virq;
}

void irq_dispose_mapping(unsigned int virq)
{
	struct irq_desc *desc = irq_to_desc(virq);
	struct irq_domain *domain;
	struct irq_data **slot;
	irq_hw_number_t hwirq;

	if (!desc)
		return;
	domain = desc->irq_data.domain;
	if (domain) {
		hwirq = desc->irq_data.hwirq;
		mutex_lock(&domain->mutex);
		if (domain->ops->unmap)
			domain->ops->unmap(domain, virq);
		slot = kpi_revmap_slot(domain, hwirq);
		if (slot)
			rcu_assign_pointer(*slot, NULL);
		else
			radix_tree_delete(&domain->revmap_tree, hwirq);
		domain->mapcount--;
		mutex_unlock(&domain->mutex);
	}
	synchronize_rcu();
	kpi_free_desc(virq);
}

void irq_domain_remove(struct irq_domain *domain)
{
	if (domain->exit)
		domain->exit(domain);
	mutex_lock(&kpi_desc_lock);
	list_del(&domain->link);
	mutex_unlock(&kpi_desc_lock);
	kfree(domain);
}

struct irq_data *irq_domain_get_irq_data(struct irq_domain *domain, unsigned int virq)
{
	struct irq_data *d = irq_get_irq_data(virq);

	return d && d->domain == domain ? d : NULL;
}

struct irq_domain *irq_find_matching_fwspec(struct irq_fwspec *fwspec,
					    enum irq_domain_bus_token bus_token)
{
	struct irq_domain *d, *found = NULL;

	mutex_lock(&kpi_desc_lock);
	list_for_each_entry(d, &kpi_domains, link) {
		bool match;

		if (d->ops->select && bus_token != DOMAIN_BUS_ANY)
			match = d->ops->select(d, fwspec, bus_token);
		else
			match = d->fwnode && d->fwnode == fwspec->fwnode &&
				(bus_token == DOMAIN_BUS_ANY || d->bus_token == bus_token);
		if (match) {
			found = d;
			break;
		}
	}
	mutex_unlock(&kpi_desc_lock);
	return found;
}

struct irq_domain *irq_get_default_domain(void)
{
	return NULL;
}

unsigned int irq_create_fwspec_mapping(struct irq_fwspec *fwspec)
{
	struct irq_domain *d = irq_find_matching_fwspec(fwspec, DOMAIN_BUS_WIRED);
	unsigned long hwirq;
	unsigned int type = IRQ_TYPE_NONE, virq;

	if (!d)
		d = irq_find_matching_fwspec(fwspec, DOMAIN_BUS_ANY);
	if (!d)
		return 0;
#ifdef CONFIG_IRQ_DOMAIN_HIERARCHY
	if (d->ops->translate) {
		if (d->ops->translate(d, fwspec, &hwirq, &type))
			return 0;
	} else
#endif
	if (d->ops->xlate) {
		if (d->ops->xlate(d, to_of_node(fwspec->fwnode), fwspec->param,
				  fwspec->param_count, &hwirq, &type))
			return 0;
	} else {
		hwirq = fwspec->param[0];
	}
	virq = irq_create_mapping(d, hwirq);
	if (virq && type != IRQ_TYPE_NONE)
		irq_set_irq_type(virq, type);
	return virq;
}

int irq_domain_xlate_onecell(struct irq_domain *d, struct device_node *ctrlr,
			     const u32 *intspec, unsigned int intsize,
			     unsigned long *out_hwirq, unsigned int *out_type)
{
	if (WARN_ON(intsize < 1))
		return -EINVAL;
	*out_hwirq = intspec[0];
	*out_type = IRQ_TYPE_NONE;
	return 0;
}

int irq_domain_xlate_twocell(struct irq_domain *d, struct device_node *ctrlr,
			     const u32 *intspec, unsigned int intsize,
			     irq_hw_number_t *out_hwirq, unsigned int *out_type)
{
	if (WARN_ON(intsize < 2))
		return -EINVAL;
	*out_hwirq = intspec[0];
	*out_type = intspec[1] & IRQ_TYPE_SENSE_MASK;
	return 0;
}

int irq_domain_xlate_onetwocell(struct irq_domain *d, struct device_node *ctrlr,
				const u32 *intspec, unsigned int intsize,
				unsigned long *out_hwirq, unsigned int *out_type)
{
	if (WARN_ON(intsize < 1))
		return -EINVAL;
	*out_hwirq = intspec[0];
	*out_type = intsize > 1 ? intspec[1] & IRQ_TYPE_SENSE_MASK : IRQ_TYPE_NONE;
	return 0;
}

int irq_domain_translate_twocell(struct irq_domain *d, struct irq_fwspec *fwspec,
				 unsigned long *out_hwirq, unsigned int *out_type)
{
	if (WARN_ON(fwspec->param_count < 2))
		return -EINVAL;
	*out_hwirq = fwspec->param[0];
	*out_type = fwspec->param[1] & IRQ_TYPE_SENSE_MASK;
	return 0;
}

int irq_domain_translate_onecell(struct irq_domain *d, struct irq_fwspec *fwspec,
				 unsigned long *out_hwirq, unsigned int *out_type)
{
	if (WARN_ON(fwspec->param_count < 1))
		return -EINVAL;
	*out_hwirq = fwspec->param[0];
	*out_type = IRQ_TYPE_NONE;
	return 0;
}

/* ---------------------------------------------------------- ACPI GSIs */

struct kpi_gsi {
	struct list_head list;
	u32 gsi;
	unsigned int irq;
	bool routed;
};

static LIST_HEAD(kpi_gsis);

static struct kpi_gsi *kpi_gsi_of(struct irq_data *d)
{
	return irq_data_get_irq_chip_data(d);
}

static void kpi_gsi_trampoline(void *arg)
{
	struct irq_desc *desc = arg;

	desc->handle_irq(desc);
}

static unsigned int kpi_gsi_startup(struct irq_data *d)
{
	struct kpi_gsi *g = kpi_gsi_of(d);
	u32 type = irqd_get_trigger_type(d);

	if (!g->routed) {
		if (rustos_kpi_gsi_request(g->gsi, !!(type & IRQ_TYPE_LEVEL_MASK),
					   !!(type & (IRQ_TYPE_LEVEL_LOW | IRQ_TYPE_EDGE_FALLING)),
					   kpi_gsi_trampoline, irq_data_to_desc(d)) < 0) {
			pr_err("GSI %u: no interrupt vector\n", g->gsi);
			return 0;
		}
		g->routed = true;
	} else {
		rustos_kpi_gsi_mask(g->gsi, 0);
	}
	return 0;
}

static void kpi_gsi_mask(struct irq_data *d)
{
	rustos_kpi_gsi_mask(kpi_gsi_of(d)->gsi, 1);
}

static void kpi_gsi_unmask(struct irq_data *d)
{
	rustos_kpi_gsi_mask(kpi_gsi_of(d)->gsi, 0);
}

/* The line may be shared with RustOS's PCI INTx users: leave it unmasked;
 * the flow handler ignores it with no action. */
static void kpi_gsi_shutdown(struct irq_data *d)
{
}

static int kpi_gsi_set_type(struct irq_data *d, unsigned int type)
{
	/* Routed once with the type from ACPI; later changes are not applied. */
	return 0;
}

static struct irq_chip kpi_gsi_chip = {
	.name = "IO-APIC",
	.irq_startup = kpi_gsi_startup,
	.irq_shutdown = kpi_gsi_shutdown,
	.irq_mask = kpi_gsi_mask,
	.irq_unmask = kpi_gsi_unmask,
	.irq_set_type = kpi_gsi_set_type,
};

int acpi_register_gsi(struct device *dev, u32 gsi, int trigger, int polarity)
{
	struct kpi_gsi *g;
	unsigned int type;

	mutex_lock(&kpi_desc_lock);
	list_for_each_entry(g, &kpi_gsis, list) {
		if (g->gsi == gsi) {
			mutex_unlock(&kpi_desc_lock);
			return g->irq;
		}
	}
	mutex_unlock(&kpi_desc_lock);
	g = kzalloc(sizeof(*g), GFP_KERNEL);
	if (!g)
		return -ENOMEM;
	g->gsi = gsi;
	g->irq = kpi_alloc_desc();
	if (!g->irq) {
		kfree(g);
		return -ENOSPC;
	}
	if (trigger == ACPI_LEVEL_SENSITIVE)
		type = polarity == ACPI_ACTIVE_HIGH ? IRQ_TYPE_LEVEL_HIGH : IRQ_TYPE_LEVEL_LOW;
	else
		type = polarity == ACPI_ACTIVE_LOW ? IRQ_TYPE_EDGE_FALLING : IRQ_TYPE_EDGE_RISING;
	irq_set_chip_and_handler_name(g->irq, &kpi_gsi_chip, handle_fasteoi_irq, "gsi");
	irq_set_chip_data(g->irq, g);
	irqd_set_trigger_type(irq_get_irq_data(g->irq), type);
	mutex_lock(&kpi_desc_lock);
	list_add(&g->list, &kpi_gsis);
	mutex_unlock(&kpi_desc_lock);
	return g->irq;
}

void acpi_unregister_gsi(u32 gsi)
{
}

int acpi_gsi_to_irq(u32 gsi, unsigned int *irq)
{
	int r = acpi_register_gsi(NULL, gsi, ACPI_LEVEL_SENSITIVE, ACPI_ACTIVE_LOW);

	if (r < 0)
		return r;
	*irq = r;
	return 0;
}

/* Legacy (ISA) interrupts' trigger and polarity from MADT overrides. */
int acpi_get_override_irq(u32 gsi, int *trigger, int *polarity)
{
	u32 g;
	int level, low;

	if (gsi >= 16)
		return -1;
	rustos_kpi_isa_irq(gsi, &g, &level, &low);
	/* As x86's: is-level and is-active-low flags. */
	*trigger = level;
	*polarity = low;
	return 0;
}

int irq_domain_xlate_twothreecell(struct irq_domain *d, struct device_node *ctrlr,
				  const u32 *intspec, unsigned int intsize,
				  irq_hw_number_t *out_hwirq, unsigned int *out_type)
{
	if (WARN_ON(intsize < 2))
		return -EINVAL;
	*out_hwirq = intspec[0];
	*out_type = intspec[1] & IRQ_TYPE_SENSE_MASK;
	return 0;
}

const struct fwnode_operations irqchip_fwnode_ops;

bool can_request_irq(unsigned int irq, unsigned long irqflags)
{
	struct irq_desc *desc = irq_to_desc(irq);

	return !desc || !desc->action || (desc->action->flags & irqflags & IRQF_SHARED);
}

/* Hierarchical domains (interrupt controllers stacked on a parent, as
 * some GPIO chips are on SoCs) are not supported: such chips fail to
 * create their domain. */
int __irq_domain_alloc_irqs(struct irq_domain *domain, int irq_base, unsigned int nr_irqs,
			    int node, void *arg, bool realloc,
			    const struct irq_affinity_desc *affinity)
{
	return -EOPNOTSUPP;
}

int irq_domain_alloc_irqs_parent(struct irq_domain *domain, unsigned int irq_base,
				 unsigned int nr_irqs, void *arg)
{
	return -EOPNOTSUPP;
}

void irq_domain_free_irqs_common(struct irq_domain *domain, unsigned int virq,
				 unsigned int nr_irqs)
{
}

void irq_domain_set_info(struct irq_domain *domain, unsigned int virq, irq_hw_number_t hwirq,
			 const struct irq_chip *chip, void *chip_data, irq_flow_handler_t handler,
			 void *handler_data, const char *handler_name)
{
	irq_set_chip_and_handler_name(virq, chip, handler, handler_name);
	irq_set_chip_data(virq, chip_data);
	irq_set_handler_data(virq, handler_data);
}
