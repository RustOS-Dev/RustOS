// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * LinuxKPI softirqs: BH disable/enable, tasklets, and the hook the RustOS
 * softirq thread calls (src/linuxkpi/sched.rs). Softirq work runs with
 * SOFTIRQ_OFFSET in the preempt count, as in Linux, so in_softirq() and
 * spin_lock_bh() behave the same.
 */
#include <linux/bottom_half.h>
#include <linux/interrupt.h>
#include <linux/list.h>
#include <linux/preempt.h>
#include <linux/spinlock.h>
#include "kpi.h"

void __local_bh_enable_ip(unsigned long ip, unsigned int cnt)
{
	preempt_count_sub(cnt);
}

static DEFINE_RAW_SPINLOCK(kpi_tasklet_lock);
static struct tasklet_struct *kpi_tasklet_head;
static struct tasklet_struct **kpi_tasklet_tail = &kpi_tasklet_head;

void tasklet_init(struct tasklet_struct *t, void (*func)(unsigned long), unsigned long data)
{
	t->next = NULL;
	t->state = 0;
	atomic_set(&t->count, 0);
	t->func = func;
	t->use_callback = false;
	t->data = data;
}

void tasklet_setup(struct tasklet_struct *t, void (*callback)(struct tasklet_struct *))
{
	t->next = NULL;
	t->state = 0;
	atomic_set(&t->count, 0);
	t->callback = callback;
	t->use_callback = true;
	t->data = 0;
}

void __tasklet_schedule(struct tasklet_struct *t)
{
	unsigned long flags;

	raw_spin_lock_irqsave(&kpi_tasklet_lock, flags);
	t->next = NULL;
	*kpi_tasklet_tail = t;
	kpi_tasklet_tail = &t->next;
	raw_spin_unlock_irqrestore(&kpi_tasklet_lock, flags);
	rustos_kpi_softirq_raise();
}

void __tasklet_hi_schedule(struct tasklet_struct *t)
{
	__tasklet_schedule(t);
}

void tasklet_kill(struct tasklet_struct *t)
{
	while (test_and_set_bit(TASKLET_STATE_SCHED, &t->state))
		rustos_kpi_yield();
	while (test_bit(TASKLET_STATE_RUN, &t->state))
		rustos_kpi_yield();
	clear_bit(TASKLET_STATE_SCHED, &t->state);
}

void tasklet_unlock(struct tasklet_struct *t)
{
	smp_mb__before_atomic();
	clear_bit(TASKLET_STATE_RUN, &t->state);
	smp_mb__after_atomic();
}

void tasklet_unlock_wait(struct tasklet_struct *t)
{
	while (test_bit(TASKLET_STATE_RUN, &t->state))
		rustos_kpi_yield();
}

static void kpi_run_tasklets(void)
{
	struct tasklet_struct *list;
	unsigned long flags;

	raw_spin_lock_irqsave(&kpi_tasklet_lock, flags);
	list = kpi_tasklet_head;
	kpi_tasklet_head = NULL;
	kpi_tasklet_tail = &kpi_tasklet_head;
	raw_spin_unlock_irqrestore(&kpi_tasklet_lock, flags);

	while (list) {
		struct tasklet_struct *t = list;

		list = list->next;
		if (tasklet_trylock(t)) {
			if (!atomic_read(&t->count)) {
				clear_bit(TASKLET_STATE_SCHED, &t->state);
				if (t->use_callback)
					t->callback(t);
				else
					t->func(t->data);
				tasklet_unlock(t);
				continue;
			}
			tasklet_unlock(t);
		}
		/* Disabled or running elsewhere: try again next round. */
		__tasklet_schedule(t);
	}
}

/* Other softirq users (NAPI) register here. */
static void (*kpi_softirq_hooks[8])(void);

void kpi_softirq_register(void (*fn)(void))
{
	for (int i = 0; i < ARRAY_SIZE(kpi_softirq_hooks); i++) {
		if (!kpi_softirq_hooks[i]) {
			kpi_softirq_hooks[i] = fn;
			return;
		}
	}
}

/* Called by the RustOS softirq thread whenever softirq work was raised. */
void kpi_softirq_run(void)
{
	preempt_count_add(SOFTIRQ_OFFSET);
	kpi_run_tasklets();
	for (int i = 0; i < ARRAY_SIZE(kpi_softirq_hooks) && kpi_softirq_hooks[i]; i++)
		kpi_softirq_hooks[i]();
	preempt_count_sub(SOFTIRQ_OFFSET);
}
