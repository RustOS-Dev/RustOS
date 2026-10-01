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
#include <linux/srcu.h>
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

/* ----------------------------------------------------------------- SRCU */

/*
 * Sleepable RCU, with the counting scheme of kernel/rcu/srcutree.c:
 * readers bump this CPU's lock count for the current index and later the
 * unlock count for the same index; a grace period flips the index and
 * waits until the old index's unlocks catch up with its locks, summed over
 * all CPUs. There is no callback machinery (call_srcu) yet.
 */
#define KPI_SRCU_READY 1	/* in srcu_usage::srcu_size_state */

int __srcu_read_lock(struct srcu_struct *ssp)
{
	struct srcu_ctr __percpu *scp = READ_ONCE(ssp->srcu_ctrp);

	this_cpu_inc(scp->srcu_locks.counter);
	smp_mb(); /* Order the count before the critical section. */
	return __srcu_ptr_to_ctr(ssp, scp);
}

void __srcu_read_unlock(struct srcu_struct *ssp, int idx)
{
	smp_mb(); /* Order the critical section before the count. */
	this_cpu_inc(__srcu_ctr_to_ptr(ssp, idx)->srcu_unlocks.counter);
}

static bool kpi_srcu_idle(struct srcu_struct *ssp, int idx)
{
	unsigned long locks = 0, unlocks = 0;
	int cpu;

	for_each_possible_cpu(cpu)
		unlocks += atomic_long_read(&per_cpu_ptr(ssp->sda, cpu)->srcu_ctrs[idx].srcu_unlocks);
	smp_mb(); /* Unlocks are summed before locks, as in srcutree.c. */
	for_each_possible_cpu(cpu)
		locks += atomic_long_read(&per_cpu_ptr(ssp->sda, cpu)->srcu_ctrs[idx].srcu_locks);
	return locks == unlocks;
}

static void kpi_srcu_wait_idle(struct srcu_struct *ssp, int idx)
{
	while (!kpi_srcu_idle(ssp, idx))
		schedule_timeout_uninterruptible(1);
}

/* DEFINE_STATIC_SRCU() leaves the mutexes uninitialized: set them up once. */
static void kpi_srcu_ready(struct srcu_struct *ssp)
{
	struct srcu_usage *sup = ssp->srcu_sup;
	unsigned long flags;

	if (smp_load_acquire(&sup->srcu_size_state) == KPI_SRCU_READY)
		return;
	spin_lock_irqsave(&ACCESS_PRIVATE(sup, lock), flags);
	if (sup->srcu_size_state != KPI_SRCU_READY) {
		mutex_init(&sup->srcu_gp_mutex);
		smp_store_release(&sup->srcu_size_state, KPI_SRCU_READY);
	}
	spin_unlock_irqrestore(&ACCESS_PRIVATE(sup, lock), flags);
}

void synchronize_srcu(struct srcu_struct *ssp)
{
	int idx;

	might_sleep();
	kpi_srcu_ready(ssp);
	mutex_lock(&ssp->srcu_sup->srcu_gp_mutex);
	idx = __srcu_ptr_to_ctr(ssp, READ_ONCE(ssp->srcu_ctrp));
	/* Readers that took the other index before the last flip. */
	kpi_srcu_wait_idle(ssp, idx ^ 1);
	smp_mb();
	WRITE_ONCE(ssp->srcu_ctrp, __srcu_ctr_to_ptr(ssp, idx ^ 1));
	smp_mb();
	kpi_srcu_wait_idle(ssp, idx);
	smp_mb();
	mutex_unlock(&ssp->srcu_sup->srcu_gp_mutex);
}

void synchronize_srcu_expedited(struct srcu_struct *ssp)
{
	synchronize_srcu(ssp);
}

int init_srcu_struct(struct srcu_struct *ssp)
{
	struct srcu_usage *sup = kzalloc(sizeof(*sup), GFP_KERNEL);

	if (!sup)
		return -ENOMEM;
	ssp->sda = alloc_percpu(struct srcu_data);
	if (!ssp->sda) {
		kfree(sup);
		return -ENOMEM;
	}
	spin_lock_init(&ACCESS_PRIVATE(sup, lock));
	mutex_init(&sup->srcu_gp_mutex);
	sup->srcu_size_state = KPI_SRCU_READY;
	sup->srcu_ssp = ssp;
	ssp->srcu_sup = sup;
	ssp->srcu_ctrp = &ssp->sda->srcu_ctrs[0];
	return 0;
}

void cleanup_srcu_struct(struct srcu_struct *ssp)
{
	if (!ssp->srcu_sup)
		return;
	if (WARN_ON(!kpi_srcu_idle(ssp, 0) || !kpi_srcu_idle(ssp, 1)))
		return;
	free_percpu(ssp->sda);
	kfree(ssp->srcu_sup);
	ssp->sda = NULL;
	ssp->srcu_sup = NULL;
}

int kpi_rcu_init(void)
{
	kpi_rcu_thread = kthread_run(kpi_rcu_thread_fn, NULL, "rcu");
	return IS_ERR(kpi_rcu_thread) ? PTR_ERR(kpi_rcu_thread) : 0;
}
