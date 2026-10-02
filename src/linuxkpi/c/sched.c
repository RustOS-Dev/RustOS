// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * LinuxKPI scheduling glue: `current`, schedule()/wake_up_process(),
 * delays and sleeps, kthreads, mutexes, completions and wait queues,
 * on top of the RustOS scheduler (src/linuxkpi/sched.rs).
 *
 * Every RustOS thread that runs Linux code gets a task_struct shadow on
 * first use of `current`. Only the fields Linux code reads are kept
 * meaningful: __state, pid (the RustOS thread id), comm, flags.
 *
 * The wait-queue and completion functions follow kernel/sched/wait.c and
 * completion.c, which cannot be compiled as-is because they need the
 * scheduler's internal kernel/sched/sched.h.
 */
#include <linux/completion.h>
#include <linux/delay.h>
#include <linux/hrtimer.h>
#include <linux/jiffies.h>
#include <linux/kthread.h>
#include <linux/mutex.h>
#include <linux/rwsem.h>
#include <linux/rtmutex.h>
#include <linux/ww_mutex.h>
#include <linux/sched.h>
#include <linux/sched/debug.h>
#include <linux/sched/signal.h>
#include <linux/sched/task.h>
#include <linux/sched/wake_q.h>
#include <linux/slab.h>
#include <linux/string.h>
#include <linux/swait.h>
#include <linux/wait.h>
#include "kpi.h"

/* ------------------------------------------------------------- current */

struct kpi_kthread {
	int (*threadfn)(void *data);
	void *data;
	int started;		/* set by wake_up_process() */
	int should_stop;
	int should_park;
	int result;
	struct completion exited;
	struct completion parked;	/* the thread reached kthread_parkme() */
};

/* Shared by all tasks: no Linux pids, signals or rlimits are tracked. */
static struct signal_struct kpi_signal;

static struct task_struct *kpi_new_task(const char *name)
{
	struct task_struct *t = kzalloc(sizeof(*t), GFP_ATOMIC);

	if (!t)
		rustos_kpi_panic("LinuxKPI: no memory for a task_struct");
	WRITE_ONCE(t->__state, TASK_RUNNING);
	refcount_set(&t->usage, 1);
	strscpy(t->comm, name ?: "rustos", sizeof(t->comm));
	t->prio = t->static_prio = t->normal_prio = MAX_RT_PRIO + 20;
	t->signal = &kpi_signal;
	return t;
}

struct task_struct *rustos_kpi_current(void)
{
	void **slot = rustos_kpi_task_slot();
	struct task_struct *t = *slot;

	if (unlikely(!t)) {
		t = kpi_new_task(NULL);
		t->pid = t->tgid = rustos_kpi_thread_id();
		*slot = t;
	}
	return t;
}

pid_t __task_pid_nr_ns(struct task_struct *task, enum pid_type type,
		       struct pid_namespace *ns)
{
	return task->pid;
}

/* ------------------------------------------------------------ schedule */

static noinline void kpi_sleep(u64 deadline_ns)
{
	rustos_kpi_sleep(deadline_ns, __builtin_return_address(0));
}

asmlinkage __visible void __sched schedule(void)
{
	struct task_struct *t = current;

	if (READ_ONCE(t->__state) == TASK_RUNNING) {
		rustos_kpi_yield();
		return;
	}
	kpi_sleep(0);
	WRITE_ONCE(t->__state, TASK_RUNNING);
}

long __sched schedule_timeout(long timeout)
{
	struct task_struct *t = current;
	u64 deadline;
	long left;

	if (timeout == MAX_SCHEDULE_TIMEOUT) {
		schedule();
		return timeout;
	}
	if (timeout < 0)
		timeout = 0;
	deadline = rustos_kpi_nanos() + jiffies_to_nsecs(timeout);
	if (READ_ONCE(t->__state) != TASK_RUNNING)
		kpi_sleep(deadline);
	WRITE_ONCE(t->__state, TASK_RUNNING);
	left = (long)nsecs_to_jiffies(deadline - min(deadline, rustos_kpi_nanos()));
	return left;
}

long __sched schedule_timeout_interruptible(long timeout)
{
	__set_current_state(TASK_INTERRUPTIBLE);
	return schedule_timeout(timeout);
}

long __sched schedule_timeout_killable(long timeout)
{
	__set_current_state(TASK_KILLABLE);
	return schedule_timeout(timeout);
}

long __sched schedule_timeout_uninterruptible(long timeout)
{
	__set_current_state(TASK_UNINTERRUPTIBLE);
	return schedule_timeout(timeout);
}

long __sched schedule_timeout_idle(long timeout)
{
	__set_current_state(TASK_IDLE);
	return schedule_timeout(timeout);
}

void __sched yield(void)
{
	rustos_kpi_yield();
}

/* preempt_enable() with a reschedule pending (CONFIG_PREEMPTION). */
void preempt_schedule(void)
{
	if (!preempt_count() && !irqs_disabled())
		rustos_kpi_yield();
}

void preempt_schedule_notrace(void)
{
	preempt_schedule();
}

static void kpi_kthread_start(struct task_struct *p);

int wake_up_state(struct task_struct *p, unsigned int state)
{
	if (!(READ_ONCE(p->__state) & state))
		return 0;
	WRITE_ONCE(p->__state, TASK_RUNNING);
	kpi_kthread_start(p);
	if (READ_ONCE(p->pid))
		rustos_kpi_wake(p->pid);
	return 1;
}

int wake_up_process(struct task_struct *p)
{
	return wake_up_state(p, TASK_NORMAL);
}

int default_wake_function(wait_queue_entry_t *curr, unsigned mode, int wake_flags, void *key)
{
	return wake_up_state(curr->private, mode);
}

/* -------------------------------------------------------------- delays */

void __ndelay(unsigned long nsecs)
{
	rustos_kpi_delay_ns(nsecs);
}

void __udelay(unsigned long usecs)
{
	rustos_kpi_delay_ns((u64)usecs * 1000);
}

/* udelay(n) passes n * 2^32/10^6, ndelay(n) n * 2^32/10^9: seconds in 32.32. */
void __const_udelay(unsigned long xloops)
{
	rustos_kpi_delay_ns(((u64)xloops * 1000000000ULL) >> 32);
}

void __delay(unsigned long loops)
{
	rustos_kpi_delay_ns(loops);
}

static void kpi_sleep_ns(u64 ns, unsigned int state)
{
	u64 deadline = rustos_kpi_nanos() + ns;

	while (rustos_kpi_nanos() < deadline) {
		__set_current_state(state);
		kpi_sleep(deadline);
		__set_current_state(TASK_RUNNING);
		if (state == TASK_INTERRUPTIBLE && signal_pending(current))
			break;
	}
}

void msleep(unsigned int msecs)
{
	kpi_sleep_ns((u64)msecs * NSEC_PER_MSEC, TASK_UNINTERRUPTIBLE);
}

unsigned long msleep_interruptible(unsigned int msecs)
{
	kpi_sleep_ns((u64)msecs * NSEC_PER_MSEC, TASK_INTERRUPTIBLE);
	return 0;
}

void __sched usleep_range_state(unsigned long min, unsigned long max, unsigned int state)
{
	kpi_sleep_ns((u64)min * NSEC_PER_USEC, state);
}


/* ------------------------------------------------------------- kthreads */

static void kpi_kthread_start(struct task_struct *p)
{
	struct kpi_kthread *k = (p->flags & PF_KTHREAD) ? p->worker_private : NULL;

	if (k && !READ_ONCE(k->started))
		WRITE_ONCE(k->started, 1);
}

static void kpi_kthread_main(void *arg)
{
	struct task_struct *t = arg;
	struct kpi_kthread *k = t->worker_private;
	void **slot = rustos_kpi_task_slot();

	*slot = t;
	WRITE_ONCE(t->pid, rustos_kpi_thread_id());
	t->tgid = t->pid;
	/* Created stopped: run once wake_up_process() was called. */
	while (!READ_ONCE(k->started)) {
		set_current_state(TASK_UNINTERRUPTIBLE);
		if (!READ_ONCE(k->started))
			schedule();
		__set_current_state(TASK_RUNNING);
	}
	__set_current_state(TASK_RUNNING);
	if (!READ_ONCE(k->should_stop))
		k->result = k->threadfn(k->data);
	complete_all(&k->exited);
}

struct task_struct *kthread_create_on_node(int (*threadfn)(void *data), void *data,
					   int node, const char namefmt[], ...)
{
	struct task_struct *t;
	struct kpi_kthread *k;
	char name[TASK_COMM_LEN];
	va_list ap;

	va_start(ap, namefmt);
	vsnprintf(name, sizeof(name), namefmt, ap);
	va_end(ap);
	k = kzalloc(sizeof(*k), GFP_KERNEL);
	if (!k)
		return ERR_PTR(-ENOMEM);
	t = kpi_new_task(name);
	/* Created stopped, as in Linux: wake_up_process() starts it. */
	WRITE_ONCE(t->__state, TASK_UNINTERRUPTIBLE);
	t->flags |= PF_KTHREAD;
	t->worker_private = k;
	k->threadfn = threadfn;
	k->data = data;
	init_completion(&k->exited);
	init_completion(&k->parked);
	rustos_kpi_spawn(kpi_kthread_main, t, name);
	return t;
}

bool kthread_should_stop(void)
{
	struct kpi_kthread *k = current->worker_private;

	return (current->flags & PF_KTHREAD) && k && READ_ONCE(k->should_stop);
}

bool kthread_should_park(void)
{
	struct kpi_kthread *k = current->worker_private;

	return (current->flags & PF_KTHREAD) && k && READ_ONCE(k->should_park);
}

/* Park: the thread calls kthread_parkme() when it sees kthread_should_park()
 * and sleeps there until unparked (or stopped). */
void kthread_parkme(void)
{
	struct kpi_kthread *k = current->worker_private;

	if (!(current->flags & PF_KTHREAD) || !k)
		return;
	complete(&k->parked);
	for (;;) {
		set_current_state(TASK_UNINTERRUPTIBLE);
		if (!READ_ONCE(k->should_park) || READ_ONCE(k->should_stop))
			break;
		schedule();
	}
	__set_current_state(TASK_RUNNING);
}

int kthread_park(struct task_struct *t)
{
	struct kpi_kthread *k = t->worker_private;

	if (READ_ONCE(k->should_park))
		return -EBUSY;
	reinit_completion(&k->parked);
	WRITE_ONCE(k->should_park, 1);
	WRITE_ONCE(k->started, 1);
	wake_up_process(t);
	wait_for_completion(&k->parked);
	return 0;
}

void kthread_unpark(struct task_struct *t)
{
	struct kpi_kthread *k = t->worker_private;

	WRITE_ONCE(k->should_park, 0);
	wake_up_process(t);
}

int kthread_stop(struct task_struct *t)
{
	struct kpi_kthread *k = t->worker_private;

	WRITE_ONCE(k->should_park, 0);
	WRITE_ONCE(k->should_stop, 1);
	WRITE_ONCE(k->started, 1);
	wake_up_process(t);
	wait_for_completion(&k->exited);
	return k->result;
}

void kthread_bind(struct task_struct *p, unsigned int cpu)
{
}

void set_user_nice(struct task_struct *p, long nice)
{
}

void sched_set_fifo(struct task_struct *p)
{
}

void sched_set_fifo_low(struct task_struct *p)
{
}

/* -------------------------------------------------------------- mutexes */

struct kpi_mutex_waiter {
	struct list_head list;
	struct task_struct *task;
};

void __mutex_init(struct mutex *lock, const char *name, struct lock_class_key *key)
{
	atomic_long_set(&lock->owner, 0);
	raw_spin_lock_init(&lock->wait_lock);
	INIT_LIST_HEAD(&lock->wait_list);
}

static bool kpi_mutex_try(struct mutex *lock)
{
	long zero = 0;

	return atomic_long_try_cmpxchg_acquire(&lock->owner, &zero, (long)current);
}

static int kpi_mutex_lock(struct mutex *lock, unsigned int state)
{
	struct kpi_mutex_waiter w;

	might_sleep();
	for (;;) {
		if (kpi_mutex_try(lock))
			return 0;
		raw_spin_lock(&lock->wait_lock);
		w.task = current;
		list_add_tail(&w.list, &lock->wait_list);
		set_current_state(state);
		if (kpi_mutex_try(lock)) {
			list_del(&w.list);
			__set_current_state(TASK_RUNNING);
			raw_spin_unlock(&lock->wait_lock);
			return 0;
		}
		raw_spin_unlock(&lock->wait_lock);
		schedule();
		raw_spin_lock(&lock->wait_lock);
		list_del(&w.list);
		raw_spin_unlock(&lock->wait_lock);
		if (state != TASK_UNINTERRUPTIBLE && signal_pending(current))
			return -EINTR;
	}
}

void __sched mutex_lock(struct mutex *lock)
{
	kpi_mutex_lock(lock, TASK_UNINTERRUPTIBLE);
}

int __sched mutex_lock_interruptible(struct mutex *lock)
{
	return kpi_mutex_lock(lock, TASK_INTERRUPTIBLE);
}

int __sched mutex_lock_killable(struct mutex *lock)
{
	return kpi_mutex_lock(lock, TASK_KILLABLE);
}

bool mutex_is_locked(struct mutex *lock)
{
	return atomic_long_read(&lock->owner) != 0;
}

int __sched mutex_trylock(struct mutex *lock)
{
	return kpi_mutex_try(lock);
}

void __sched mutex_unlock(struct mutex *lock)
{
	struct kpi_mutex_waiter *w;

	atomic_long_set_release(&lock->owner, 0);
	raw_spin_lock(&lock->wait_lock);
	w = list_first_entry_or_null(&lock->wait_list, struct kpi_mutex_waiter, list);
	if (w)
		wake_up_process(w->task);
	raw_spin_unlock(&lock->wait_lock);
}

void *kthread_data(struct task_struct *task)
{
	struct kpi_kthread *k = (task->flags & PF_KTHREAD) ? task->worker_private : NULL;

	return k ? k->data : NULL;
}

/* ------------------------------------------------- wound/wait mutexes */

/*
 * ww_mutex with the wait-die rule for every class: a transaction that
 * already holds locks and meets a lock held by an older transaction (a
 * smaller stamp) backs off with -EDEADLK; otherwise it waits. Wound-wait
 * classes (DRM's reservation_ww_class) get the same guarantee, since
 * their callers handle -EDEADLK the same way.
 */
static bool kpi_ww_must_die(struct ww_mutex *lock, struct ww_acquire_ctx *ctx)
{
	struct ww_acquire_ctx *hold = READ_ONCE(lock->ctx);

	return hold && ctx->acquired > 0 && (long)(ctx->stamp - hold->stamp) > 0;
}

static int kpi_ww_lock(struct ww_mutex *lock, struct ww_acquire_ctx *ctx, unsigned int state)
{
	struct mutex *m = &lock->base;
	struct kpi_mutex_waiter w;

	if (!ctx)
		return kpi_mutex_lock(m, state);
	if (READ_ONCE(lock->ctx) == ctx)
		return -EALREADY;
	might_sleep();
	for (;;) {
		if (kpi_mutex_try(m))
			goto locked;
		if (kpi_ww_must_die(lock, ctx))
			return -EDEADLK;
		raw_spin_lock(&m->wait_lock);
		w.task = current;
		list_add_tail(&w.list, &m->wait_list);
		set_current_state(state);
		if (kpi_mutex_try(m)) {
			list_del(&w.list);
			__set_current_state(TASK_RUNNING);
			raw_spin_unlock(&m->wait_lock);
			goto locked;
		}
		raw_spin_unlock(&m->wait_lock);
		schedule();
		raw_spin_lock(&m->wait_lock);
		list_del(&w.list);
		raw_spin_unlock(&m->wait_lock);
		if (state != TASK_UNINTERRUPTIBLE && signal_pending(current))
			return -EINTR;
	}
locked:
	WRITE_ONCE(lock->ctx, ctx);
	ctx->acquired++;
	return 0;
}

int ww_mutex_lock(struct ww_mutex *lock, struct ww_acquire_ctx *ctx)
{
	return kpi_ww_lock(lock, ctx, TASK_UNINTERRUPTIBLE);
}

int ww_mutex_lock_interruptible(struct ww_mutex *lock, struct ww_acquire_ctx *ctx)
{
	return kpi_ww_lock(lock, ctx, TASK_INTERRUPTIBLE);
}

int ww_mutex_trylock(struct ww_mutex *lock, struct ww_acquire_ctx *ctx)
{
	if (!kpi_mutex_try(&lock->base))
		return 0;
	if (ctx) {
		WRITE_ONCE(lock->ctx, ctx);
		ctx->acquired++;
	}
	return 1;
}

void ww_mutex_unlock(struct ww_mutex *lock)
{
	struct ww_acquire_ctx *ctx = READ_ONCE(lock->ctx);

	if (ctx) {
		if (ctx->acquired > 0)
			ctx->acquired--;
		WRITE_ONCE(lock->ctx, NULL);
	}
	mutex_unlock(&lock->base);
}

/* --------------------------------------------------------------- rwsems */

/* count: the number of readers, or -1 while a writer holds the lock. */
struct kpi_rwsem_waiter {
	struct list_head list;
	struct task_struct *task;
};

void __init_rwsem(struct rw_semaphore *sem, const char *name, struct lock_class_key *key)
{
	atomic_long_set(&sem->count, 0);
	atomic_long_set(&sem->owner, 0);
	raw_spin_lock_init(&sem->wait_lock);
	INIT_LIST_HEAD(&sem->wait_list);
}

static bool kpi_rwsem_try_read(struct rw_semaphore *sem)
{
	long c = atomic_long_read(&sem->count);

	while (c >= 0)
		if (atomic_long_try_cmpxchg_acquire(&sem->count, &c, c + 1))
			return true;
	return false;
}

static bool kpi_rwsem_try_write(struct rw_semaphore *sem)
{
	long zero = 0;

	if (!atomic_long_try_cmpxchg_acquire(&sem->count, &zero, -1))
		return false;
	atomic_long_set(&sem->owner, (long)current);
	return true;
}

static int kpi_rwsem_lock(struct rw_semaphore *sem, bool (*try)(struct rw_semaphore *),
			  unsigned int state)
{
	struct kpi_rwsem_waiter w;

	might_sleep();
	for (;;) {
		if (try(sem))
			return 0;
		raw_spin_lock(&sem->wait_lock);
		w.task = current;
		list_add_tail(&w.list, &sem->wait_list);
		set_current_state(state);
		if (try(sem)) {
			list_del(&w.list);
			__set_current_state(TASK_RUNNING);
			raw_spin_unlock(&sem->wait_lock);
			return 0;
		}
		raw_spin_unlock(&sem->wait_lock);
		schedule();
		raw_spin_lock(&sem->wait_lock);
		list_del(&w.list);
		raw_spin_unlock(&sem->wait_lock);
		if (state != TASK_UNINTERRUPTIBLE && signal_pending(current))
			return -EINTR;
	}
}

/* Waiters retry; wake them all when the lock may have become available. */
static void kpi_rwsem_wake(struct rw_semaphore *sem)
{
	struct kpi_rwsem_waiter *w;

	raw_spin_lock(&sem->wait_lock);
	list_for_each_entry(w, &sem->wait_list, list)
		wake_up_process(w->task);
	raw_spin_unlock(&sem->wait_lock);
}

void __sched down_read(struct rw_semaphore *sem)
{
	kpi_rwsem_lock(sem, kpi_rwsem_try_read, TASK_UNINTERRUPTIBLE);
}

int __sched down_read_interruptible(struct rw_semaphore *sem)
{
	return kpi_rwsem_lock(sem, kpi_rwsem_try_read, TASK_INTERRUPTIBLE);
}

int __sched down_read_killable(struct rw_semaphore *sem)
{
	return kpi_rwsem_lock(sem, kpi_rwsem_try_read, TASK_KILLABLE);
}

int down_read_trylock(struct rw_semaphore *sem)
{
	return kpi_rwsem_try_read(sem);
}

void __sched down_write(struct rw_semaphore *sem)
{
	kpi_rwsem_lock(sem, kpi_rwsem_try_write, TASK_UNINTERRUPTIBLE);
}

int __sched down_write_killable(struct rw_semaphore *sem)
{
	return kpi_rwsem_lock(sem, kpi_rwsem_try_write, TASK_KILLABLE);
}

int down_write_trylock(struct rw_semaphore *sem)
{
	return kpi_rwsem_try_write(sem);
}

void up_read(struct rw_semaphore *sem)
{
	if (atomic_long_dec_return_release(&sem->count) == 0)
		kpi_rwsem_wake(sem);
}

void up_write(struct rw_semaphore *sem)
{
	atomic_long_set(&sem->owner, 0);
	atomic_long_set_release(&sem->count, 0);
	kpi_rwsem_wake(sem);
}

void downgrade_write(struct rw_semaphore *sem)
{
	atomic_long_set(&sem->owner, 0);
	atomic_long_set_release(&sem->count, 1);
	kpi_rwsem_wake(sem);
}

/* ---------------------------------------------------------- completions */

void __init_swait_queue_head(struct swait_queue_head *q, const char *name,
			     struct lock_class_key *key)
{
	raw_spin_lock_init(&q->lock);
	INIT_LIST_HEAD(&q->task_list);
}

static void kpi_complete(struct completion *x, bool all)
{
	struct swait_queue *w, *tmp;
	unsigned long flags;

	raw_spin_lock_irqsave(&x->wait.lock, flags);
	if (all)
		x->done = UINT_MAX;
	else if (x->done != UINT_MAX)
		x->done++;
	list_for_each_entry_safe(w, tmp, &x->wait.task_list, task_list) {
		wake_up_process(w->task);
		if (!all)
			break;
	}
	raw_spin_unlock_irqrestore(&x->wait.lock, flags);
}

void complete(struct completion *x)
{
	kpi_complete(x, false);
}

void complete_all(struct completion *x)
{
	kpi_complete(x, true);
}

static long kpi_wait_for_completion(struct completion *x, long timeout, unsigned int state)
{
	struct swait_queue w = { .task = current };
	unsigned long flags;
	u64 deadline = timeout == MAX_SCHEDULE_TIMEOUT ? 0 :
		       rustos_kpi_nanos() + jiffies_to_nsecs(timeout);

	raw_spin_lock_irqsave(&x->wait.lock, flags);
	while (!x->done) {
		list_add_tail(&w.task_list, &x->wait.task_list);
		__set_current_state(state);
		raw_spin_unlock_irqrestore(&x->wait.lock, flags);
		kpi_sleep(deadline);
		__set_current_state(TASK_RUNNING);
		raw_spin_lock_irqsave(&x->wait.lock, flags);
		list_del(&w.task_list);
		if (deadline && !x->done && rustos_kpi_nanos() >= deadline) {
			raw_spin_unlock_irqrestore(&x->wait.lock, flags);
			return 0;
		}
		if (state != TASK_UNINTERRUPTIBLE && signal_pending(current)) {
			raw_spin_unlock_irqrestore(&x->wait.lock, flags);
			return -ERESTARTSYS;
		}
	}
	if (x->done != UINT_MAX)
		x->done--;
	raw_spin_unlock_irqrestore(&x->wait.lock, flags);
	if (!deadline)
		return timeout ?: 1;
	return max_t(long, 1, nsecs_to_jiffies(deadline - min(deadline, rustos_kpi_nanos())));
}

void __sched wait_for_completion(struct completion *x)
{
	kpi_wait_for_completion(x, MAX_SCHEDULE_TIMEOUT, TASK_UNINTERRUPTIBLE);
}

unsigned long __sched wait_for_completion_timeout(struct completion *x, unsigned long timeout)
{
	return kpi_wait_for_completion(x, timeout, TASK_UNINTERRUPTIBLE);
}

int __sched wait_for_completion_interruptible(struct completion *x)
{
	long r = kpi_wait_for_completion(x, MAX_SCHEDULE_TIMEOUT, TASK_INTERRUPTIBLE);

	return r < 0 ? r : 0;
}

long __sched wait_for_completion_interruptible_timeout(struct completion *x,
							unsigned long timeout)
{
	return kpi_wait_for_completion(x, timeout, TASK_INTERRUPTIBLE);
}

int __sched wait_for_completion_killable(struct completion *x)
{
	long r = kpi_wait_for_completion(x, MAX_SCHEDULE_TIMEOUT, TASK_KILLABLE);

	return r < 0 ? r : 0;
}

long __sched wait_for_completion_killable_timeout(struct completion *x,
						  unsigned long timeout)
{
	return kpi_wait_for_completion(x, timeout, TASK_KILLABLE);
}

bool try_wait_for_completion(struct completion *x)
{
	unsigned long flags;
	bool ret = false;

	raw_spin_lock_irqsave(&x->wait.lock, flags);
	if (x->done) {
		if (x->done != UINT_MAX)
			x->done--;
		ret = true;
	}
	raw_spin_unlock_irqrestore(&x->wait.lock, flags);
	return ret;
}

bool completion_done(struct completion *x)
{
	return READ_ONCE(x->done) != 0;
}

/* ---------------------------------------------------------- wait queues */

void __init_waitqueue_head(struct wait_queue_head *wq_head, const char *name,
			   struct lock_class_key *key)
{
	spin_lock_init(&wq_head->lock);
	INIT_LIST_HEAD(&wq_head->head);
}

void add_wait_queue(struct wait_queue_head *wq_head, struct wait_queue_entry *wq_entry)
{
	unsigned long flags;

	wq_entry->flags &= ~WQ_FLAG_EXCLUSIVE;
	spin_lock_irqsave(&wq_head->lock, flags);
	__add_wait_queue(wq_head, wq_entry);
	spin_unlock_irqrestore(&wq_head->lock, flags);
}

void add_wait_queue_exclusive(struct wait_queue_head *wq_head, struct wait_queue_entry *wq_entry)
{
	unsigned long flags;

	wq_entry->flags |= WQ_FLAG_EXCLUSIVE;
	spin_lock_irqsave(&wq_head->lock, flags);
	__add_wait_queue_entry_tail(wq_head, wq_entry);
	spin_unlock_irqrestore(&wq_head->lock, flags);
}

void remove_wait_queue(struct wait_queue_head *wq_head, struct wait_queue_entry *wq_entry)
{
	unsigned long flags;

	spin_lock_irqsave(&wq_head->lock, flags);
	__remove_wait_queue(wq_head, wq_entry);
	spin_unlock_irqrestore(&wq_head->lock, flags);
}

static int kpi_wake_up_common(struct wait_queue_head *wq_head, unsigned int mode,
			      int nr_exclusive, int wake_flags, void *key)
{
	wait_queue_entry_t *curr, *next;

	list_for_each_entry_safe(curr, next, &wq_head->head, entry) {
		unsigned flags = curr->flags;
		int ret;

		if (WARN(!curr->func, "wait entry %px (flags %x private %px) on %px has no func\n",
			 curr, curr->flags, curr->private, wq_head))
			break;
		ret = curr->func(curr, mode, wake_flags, key);

		if (ret < 0)
			break;
		if (ret && (flags & WQ_FLAG_EXCLUSIVE) && !--nr_exclusive)
			break;
	}
	return nr_exclusive;
}

int __wake_up(struct wait_queue_head *wq_head, unsigned int mode, int nr_exclusive, void *key)
{
	unsigned long flags;
	int remaining;

	spin_lock_irqsave(&wq_head->lock, flags);
	remaining = kpi_wake_up_common(wq_head, mode, nr_exclusive, 0, key);
	spin_unlock_irqrestore(&wq_head->lock, flags);
	return nr_exclusive - remaining;
}

void __wake_up_locked(struct wait_queue_head *wq_head, unsigned int mode, int nr)
{
	kpi_wake_up_common(wq_head, mode, nr, 0, NULL);
}

void __wake_up_locked_key(struct wait_queue_head *wq_head, unsigned int mode, void *key)
{
	kpi_wake_up_common(wq_head, mode, 1, 0, key);
}

void __wake_up_sync_key(struct wait_queue_head *wq_head, unsigned int mode, void *key)
{
	__wake_up(wq_head, mode, 1, key);
}

void __wake_up_sync(struct wait_queue_head *wq_head, unsigned int mode)
{
	__wake_up(wq_head, mode, 1, NULL);
}

void prepare_to_wait(struct wait_queue_head *wq_head, struct wait_queue_entry *wq_entry, int state)
{
	unsigned long flags;

	wq_entry->flags &= ~WQ_FLAG_EXCLUSIVE;
	spin_lock_irqsave(&wq_head->lock, flags);
	if (list_empty(&wq_entry->entry))
		__add_wait_queue(wq_head, wq_entry);
	set_current_state(state);
	spin_unlock_irqrestore(&wq_head->lock, flags);
}

bool prepare_to_wait_exclusive(struct wait_queue_head *wq_head,
			       struct wait_queue_entry *wq_entry, int state)
{
	unsigned long flags;
	bool was_empty = false;

	wq_entry->flags |= WQ_FLAG_EXCLUSIVE;
	spin_lock_irqsave(&wq_head->lock, flags);
	if (list_empty(&wq_entry->entry)) {
		was_empty = list_empty(&wq_head->head);
		__add_wait_queue_entry_tail(wq_head, wq_entry);
	}
	set_current_state(state);
	spin_unlock_irqrestore(&wq_head->lock, flags);
	return was_empty;
}

void init_wait_entry(struct wait_queue_entry *wq_entry, int flags)
{
	wq_entry->flags = flags;
	wq_entry->private = current;
	wq_entry->func = autoremove_wake_function;
	INIT_LIST_HEAD(&wq_entry->entry);
}

long prepare_to_wait_event(struct wait_queue_head *wq_head, struct wait_queue_entry *wq_entry,
			   int state)
{
	unsigned long flags;
	long ret = 0;

	spin_lock_irqsave(&wq_head->lock, flags);
	if (signal_pending_state(state, current)) {
		list_del_init(&wq_entry->entry);
		ret = -ERESTARTSYS;
	} else {
		if (list_empty(&wq_entry->entry)) {
			if (wq_entry->flags & WQ_FLAG_EXCLUSIVE)
				__add_wait_queue_entry_tail(wq_head, wq_entry);
			else
				__add_wait_queue(wq_head, wq_entry);
		}
		set_current_state(state);
	}
	spin_unlock_irqrestore(&wq_head->lock, flags);
	return ret;
}

void finish_wait(struct wait_queue_head *wq_head, struct wait_queue_entry *wq_entry)
{
	unsigned long flags;

	__set_current_state(TASK_RUNNING);
	if (!list_empty_careful(&wq_entry->entry)) {
		spin_lock_irqsave(&wq_head->lock, flags);
		list_del_init(&wq_entry->entry);
		spin_unlock_irqrestore(&wq_head->lock, flags);
	}
}

int autoremove_wake_function(struct wait_queue_entry *wq_entry, unsigned mode, int sync,
			     void *key)
{
	int ret = default_wake_function(wq_entry, mode, sync, key);

	if (ret)
		list_del_init_careful(&wq_entry->entry);
	return ret;
}

int woken_wake_function(struct wait_queue_entry *wq_entry, unsigned mode, int sync, void *key)
{
	smp_mb();
	wq_entry->flags |= WQ_FLAG_WOKEN;
	return default_wake_function(wq_entry, mode, sync, key);
}

long wait_woken(struct wait_queue_entry *wq_entry, unsigned mode, long timeout)
{
	set_current_state(mode);
	if (!(wq_entry->flags & WQ_FLAG_WOKEN) && !kthread_should_stop())
		timeout = schedule_timeout(timeout);
	__set_current_state(TASK_RUNNING);
	smp_store_mb(wq_entry->flags, wq_entry->flags & ~WQ_FLAG_WOKEN);
	return timeout;
}

/* ------------------------------------------------------------- rt_mutex */

/* No priority inheritance (RustOS has no priority scheduling): an rt_mutex
 * is a sleeping lock owned through ->owner, retried after a short sleep
 * when contended (I2C bus locks, rarely contended). */
void __rt_mutex_init(struct rt_mutex *lock, const char *name, struct lock_class_key *key)
{
	raw_spin_lock_init(&lock->rtmutex.wait_lock);
	lock->rtmutex.waiters = RB_ROOT_CACHED;
	lock->rtmutex.owner = NULL;
}

int rt_mutex_trylock(struct rt_mutex *lock)
{
	return try_cmpxchg_acquire(&lock->rtmutex.owner, &(struct task_struct *){ NULL }, current);
}

void rt_mutex_lock(struct rt_mutex *lock)
{
	might_sleep();
	while (!rt_mutex_trylock(lock))
		usleep_range(50, 100);
}

void rt_mutex_unlock(struct rt_mutex *lock)
{
	smp_store_release(&lock->rtmutex.owner, NULL);
}

/* Deferred wake-up lists: wake at once (RustOS wake-ups never sleep, so
 * waking under the caller's lock is fine). */
void wake_q_add(struct wake_q_head *head, struct task_struct *task)
{
	wake_up_process(task);
}

void wake_up_q(struct wake_q_head *head)
{
}
