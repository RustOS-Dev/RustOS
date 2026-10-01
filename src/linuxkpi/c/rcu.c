// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * RCU for LinuxKPI.
 *
 * Readers disable preemption (legal: RCU readers may not sleep, even with
 * CONFIG_PREEMPT_RCU), so a CPU that context-switches, or takes a timer
 * tick with preemption enabled, has left every reader it was in. RustOS
 * counts those quiescent states per CPU (PerCpu::rcu_qs) and
 * rustos_kpi_rcu_synchronize() waits for every other CPU's count to move.
 *
 * call_rcu() callbacks are queued to the "rcu" kthread, which waits for a
 * grace period per batch and runs them with bottom halves disabled, as
 * Linux runs them from softirq context.
 */
#include <linux/completion.h>
#include <linux/kthread.h>
#include <linux/mm.h>
#include <linux/rcupdate.h>
#include <linux/sched.h>
#include <linux/slab.h>
#include <linux/spinlock.h>
#include <linux/wait.h>
#include "kpi.h"

void __rcu_read_lock(void)
{
	preempt_disable();
}

void __rcu_read_unlock(void)
{
	preempt_enable();
}

void synchronize_rcu(void)
{
	might_sleep();
	rustos_kpi_rcu_synchronize();
}

void synchronize_rcu_expedited(void)
{
	synchronize_rcu();
}

/* ------------------------------------------------------------ callbacks */

static DEFINE_SPINLOCK(kpi_rcu_lock);
static struct rcu_head *kpi_rcu_head;
static struct rcu_head **kpi_rcu_tail = &kpi_rcu_head;
static DECLARE_WAIT_QUEUE_HEAD(kpi_rcu_wq);
static struct task_struct *kpi_rcu_thread;

/* kvfree_call_rcu() stores the rcu_head's offset in the object as func. */
#define KPI_KVFREE_MAX_OFFSET 4096

static void kpi_rcu_enqueue(struct rcu_head *head, rcu_callback_t func)
{
	unsigned long flags;

	head->func = func;
	head->next = NULL;
	spin_lock_irqsave(&kpi_rcu_lock, flags);
	*kpi_rcu_tail = head;
	kpi_rcu_tail = &head->next;
	spin_unlock_irqrestore(&kpi_rcu_lock, flags);
	wake_up(&kpi_rcu_wq);
}

void call_rcu(struct rcu_head *head, rcu_callback_t func)
{
	kpi_rcu_enqueue(head, func);
}

void kvfree_call_rcu(struct rcu_head *head, void *ptr)
{
	if (!head) {
		/* kfree_rcu_mightsleep(): no rcu_head in the object. */
		synchronize_rcu();
		kvfree(ptr);
		return;
	}
	kpi_rcu_enqueue(head, (rcu_callback_t)((unsigned long)head - (unsigned long)ptr));
}

static bool kpi_rcu_pending(void)
{
	return READ_ONCE(kpi_rcu_head) != NULL;
}

static int kpi_rcu_thread_fn(void *data)
{
	for (;;) {
		struct rcu_head *list;
		unsigned long flags;

		wait_event(kpi_rcu_wq, kpi_rcu_pending());
		spin_lock_irqsave(&kpi_rcu_lock, flags);
		list = kpi_rcu_head;
		kpi_rcu_head = NULL;
		kpi_rcu_tail = &kpi_rcu_head;
		spin_unlock_irqrestore(&kpi_rcu_lock, flags);

		synchronize_rcu();
		local_bh_disable();
		while (list) {
			struct rcu_head *next = list->next;
			unsigned long off = (unsigned long)list->func;

			if (off < KPI_KVFREE_MAX_OFFSET)
				kvfree((void *)list - off);
			else
				list->func(list);
			list = next;
		}
		local_bh_enable();
		cond_resched();
	}
	return 0;
}

struct kpi_rcu_barrier {
	struct rcu_head head;
	struct completion done;
};

static void kpi_rcu_barrier_cb(struct rcu_head *head)
{
	complete(&container_of(head, struct kpi_rcu_barrier, head)->done);
}

/* Callbacks run in queue order, so the barrier's runs after all earlier ones. */
void rcu_barrier(void)
{
	struct kpi_rcu_barrier b;

	init_completion(&b.done);
	call_rcu(&b.head, kpi_rcu_barrier_cb);
	wait_for_completion(&b.done);
}

int kpi_rcu_init(void)
{
	kpi_rcu_thread = kthread_run(kpi_rcu_thread_fn, NULL, "rcu");
	return IS_ERR(kpi_rcu_thread) ? PTR_ERR(kpi_rcu_thread) : 0;
}
