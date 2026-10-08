// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * LinuxKPI time: jiffies, timer_list and workqueues.
 *
 * Timers are RustOS one-shot timers whose callbacks run in the LinuxKPI
 * softirq thread. A timer_list stores its RustOS handle in entry.next and
 * is pending while entry.pprev is non-NULL (what timer_pending() tests).
 * A firing whose handle no longer matches (cancelled or re-armed after it
 * was queued) is ignored; timer_delete_sync() waits while the callback of
 * that timer is running.
 *
 * Workqueues have their own worker threads. One lock covers every queue,
 * and running work items are tracked so cancel_work_sync()/flush_work()
 * can wait for them.
 */
#include <linux/jiffies.h>
#include <linux/ktime.h>
#include <linux/timekeeping.h>
#include <linux/sched.h>
#include <linux/slab.h>
#include <linux/spinlock.h>
#include <linux/timer.h>
#include <linux/hrtimer.h>
#include <linux/workqueue.h>
#include <linux/async.h>
#include "kpi.h"

/* x86_64 Linux makes jiffies an alias of jiffies_64 in its linker script. */
u64 jiffies_64 __cacheline_aligned_in_smp = INITIAL_JIFFIES;
extern unsigned long volatile jiffies __attribute__((alias("jiffies_64")));

/* Called from the RustOS timer tick on CPU 0. */
void kpi_jiffies_update(void)
{
	WRITE_ONCE(jiffies_64, INITIAL_JIFFIES + rustos_kpi_nanos() / (NSEC_PER_SEC / HZ));
}

/* ---------------------------------------------------------------- timers */

static DEFINE_RAW_SPINLOCK(kpi_timer_lock);
static struct timer_list *kpi_running_timer;

static void kpi_timer_fire(void *arg, u64 handle);

void timer_init_key(struct timer_list *timer, void (*func)(struct timer_list *),
		    unsigned int flags, const char *name, struct lock_class_key *key)
{
	timer->entry.pprev = NULL;
	timer->entry.next = NULL;
	timer->function = func;
	timer->flags = flags;
}

/* Lock held. Returns 1 if the timer was pending. */
static int kpi_timer_detach(struct timer_list *timer)
{
	if (!timer->entry.pprev)
		return 0;
	rustos_kpi_timer_cancel((u64)timer->entry.next);
	timer->entry.pprev = NULL;
	timer->entry.next = NULL;
	return 1;
}

static int kpi_mod_timer(struct timer_list *timer, unsigned long expires, bool pending_only)
{
	unsigned long flags;
	long delta;
	u64 handle;
	int was;

	raw_spin_lock_irqsave(&kpi_timer_lock, flags);
	if (pending_only && !timer->entry.pprev) {
		raw_spin_unlock_irqrestore(&kpi_timer_lock, flags);
		return 0;
	}
	was = kpi_timer_detach(timer);
	timer->expires = expires;
	delta = (long)(expires - jiffies);
	if (delta < 0)
		delta = 0;
	handle = rustos_kpi_timer_start(rustos_kpi_nanos() + jiffies_to_nsecs(delta),
					kpi_timer_fire, timer);
	timer->entry.next = (struct hlist_node *)handle;
	timer->entry.pprev = &timer->entry.next;
	raw_spin_unlock_irqrestore(&kpi_timer_lock, flags);
	return was;
}

static void kpi_timer_fire(void *arg, u64 handle)
{
	struct timer_list *timer = arg;
	unsigned long flags;

	raw_spin_lock_irqsave(&kpi_timer_lock, flags);
	if (!timer->entry.pprev || (u64)timer->entry.next != handle) {
		raw_spin_unlock_irqrestore(&kpi_timer_lock, flags);
		return;
	}
	timer->entry.pprev = NULL;
	timer->entry.next = NULL;
	kpi_running_timer = timer;
	raw_spin_unlock_irqrestore(&kpi_timer_lock, flags);

	timer->function(timer);

	raw_spin_lock_irqsave(&kpi_timer_lock, flags);
	kpi_running_timer = NULL;
	raw_spin_unlock_irqrestore(&kpi_timer_lock, flags);
}

int mod_timer(struct timer_list *timer, unsigned long expires)
{
	return kpi_mod_timer(timer, expires, false);
}

int mod_timer_pending(struct timer_list *timer, unsigned long expires)
{
	return kpi_mod_timer(timer, expires, true);
}

int timer_reduce(struct timer_list *timer, unsigned long expires)
{
	if (timer_pending(timer) && time_before_eq(timer->expires, expires))
		return 1;
	return kpi_mod_timer(timer, expires, false);
}

void add_timer(struct timer_list *timer)
{
	kpi_mod_timer(timer, timer->expires, false);
}

void add_timer_on(struct timer_list *timer, int cpu)
{
	add_timer(timer);
}

void add_timer_local(struct timer_list *timer)
{
	add_timer(timer);
}

void add_timer_global(struct timer_list *timer)
{
	add_timer(timer);
}

int timer_delete(struct timer_list *timer)
{
	unsigned long flags;
	int was;

	raw_spin_lock_irqsave(&kpi_timer_lock, flags);
	was = kpi_timer_detach(timer);
	raw_spin_unlock_irqrestore(&kpi_timer_lock, flags);
	return was;
}

int timer_delete_sync_try(struct timer_list *timer)
{
	unsigned long flags;
	int was;

	raw_spin_lock_irqsave(&kpi_timer_lock, flags);
	if (kpi_running_timer == timer) {
		raw_spin_unlock_irqrestore(&kpi_timer_lock, flags);
		return -1;
	}
	was = kpi_timer_detach(timer);
	raw_spin_unlock_irqrestore(&kpi_timer_lock, flags);
	return was;
}

int timer_delete_sync(struct timer_list *timer)
{
	int was = timer_delete(timer);

	while (READ_ONCE(kpi_running_timer) == timer)
		rustos_kpi_yield();
	return was;
}

int timer_shutdown_sync(struct timer_list *timer)
{
	int was = timer_delete_sync(timer);

	timer->function = NULL;
	return was;
}

int timer_shutdown(struct timer_list *timer)
{
	int was = timer_delete(timer);

	timer->function = NULL;
	return was;
}

/* --------------------------------------------------------------- hrtimers */

/*
 * hrtimers are RustOS one-shot timers too. The handle lives in
 * node.node.__rb_parent_color; state is HRTIMER_STATE_ENQUEUED while one
 * is armed. Expiry times are in the timer's clock (node.expires): monotonic
 * and boot time are the same clock here, and CLOCK_REALTIME/TAI deadlines
 * are converted when armed. Callbacks run in the softirq thread.
 */
static struct hrtimer_cpu_base kpi_hrtimer_cpu_base;
static struct hrtimer_clock_base kpi_hrtimer_bases[] = {
	{ .cpu_base = &kpi_hrtimer_cpu_base, .index = 0, .clockid = CLOCK_MONOTONIC },
	{ .cpu_base = &kpi_hrtimer_cpu_base, .index = 1, .clockid = CLOCK_REALTIME },
	{ .cpu_base = &kpi_hrtimer_cpu_base, .index = 2, .clockid = CLOCK_BOOTTIME },
	{ .cpu_base = &kpi_hrtimer_cpu_base, .index = 3, .clockid = CLOCK_TAI },
};

/* From kernel/time/hrtimer.c: add, saturating at KTIME_MAX. */
ktime_t ktime_add_safe(const ktime_t lhs, const ktime_t rhs)
{
	ktime_t res = ktime_add_unsafe(lhs, rhs);

	if (res < 0 || res < lhs || res < rhs)
		res = ktime_set(KTIME_SEC_MAX, 0);
	return res;
}

static ktime_t kpi_clock_now(clockid_t clock)
{
	return clock == CLOCK_REALTIME || clock == CLOCK_TAI ? rustos_kpi_realtime_ns() :
							       rustos_kpi_nanos();
}

static void kpi_hrtimer_fire(void *arg, u64 handle);

void hrtimer_setup(struct hrtimer *timer, enum hrtimer_restart (*function)(struct hrtimer *),
		   clockid_t clock_id, enum hrtimer_mode mode)
{
	struct hrtimer_clock_base *base = &kpi_hrtimer_bases[0];

	memset(timer, 0, sizeof(*timer));
	for (int i = 0; i < ARRAY_SIZE(kpi_hrtimer_bases); i++)
		if (kpi_hrtimer_bases[i].clockid == clock_id)
			base = &kpi_hrtimer_bases[i];
	timer->base = base;
	ACCESS_PRIVATE(timer, function) = function;
	timer->is_soft = !!(mode & HRTIMER_MODE_SOFT);
	timer->is_hard = !!(mode & HRTIMER_MODE_HARD);
}

/* Lock held. */
static int kpi_hrtimer_detach(struct hrtimer *timer)
{
	if (!(timer->state & HRTIMER_STATE_ENQUEUED))
		return 0;
	rustos_kpi_timer_cancel(timer->node.node.__rb_parent_color);
	timer->node.node.__rb_parent_color = 0;
	WRITE_ONCE(timer->state, HRTIMER_STATE_INACTIVE);
	return 1;
}

/* Lock held: arm for node.expires. */
static void kpi_hrtimer_arm(struct hrtimer *timer)
{
	ktime_t now = kpi_clock_now(timer->base->clockid);
	s64 delta = ktime_to_ns(ktime_sub(timer->node.expires, now));

	if (delta < 0)
		delta = 0;
	timer->node.node.__rb_parent_color =
		rustos_kpi_timer_start(rustos_kpi_nanos() + delta, kpi_hrtimer_fire, timer);
	WRITE_ONCE(timer->state, HRTIMER_STATE_ENQUEUED);
}

void hrtimer_start_range_ns(struct hrtimer *timer, ktime_t tim, u64 range_ns,
			    const enum hrtimer_mode mode)
{
	unsigned long flags;

	raw_spin_lock_irqsave(&kpi_timer_lock, flags);
	kpi_hrtimer_detach(timer);
	if (mode & HRTIMER_MODE_REL)
		tim = ktime_add_safe(kpi_clock_now(timer->base->clockid), tim);
	timer->node.expires = tim;
	timer->_softexpires = tim;
	kpi_hrtimer_arm(timer);
	raw_spin_unlock_irqrestore(&kpi_timer_lock, flags);
}

static void kpi_hrtimer_fire(void *arg, u64 handle)
{
	struct hrtimer *timer = arg;
	enum hrtimer_restart (*fn)(struct hrtimer *);
	enum hrtimer_restart ret;
	unsigned long flags;

	raw_spin_lock_irqsave(&kpi_timer_lock, flags);
	if (!(timer->state & HRTIMER_STATE_ENQUEUED) ||
	    timer->node.node.__rb_parent_color != handle) {
		raw_spin_unlock_irqrestore(&kpi_timer_lock, flags);
		return;
	}
	timer->node.node.__rb_parent_color = 0;
	WRITE_ONCE(timer->state, HRTIMER_STATE_INACTIVE);
	WRITE_ONCE(timer->base->running, timer);
	fn = ACCESS_PRIVATE(timer, function);
	raw_spin_unlock_irqrestore(&kpi_timer_lock, flags);

	ret = fn(timer);

	raw_spin_lock_irqsave(&kpi_timer_lock, flags);
	/* Restart unless the callback (or someone else) re-armed it. */
	if (ret == HRTIMER_RESTART && !(timer->state & HRTIMER_STATE_ENQUEUED))
		kpi_hrtimer_arm(timer);
	WRITE_ONCE(timer->base->running, NULL);
	raw_spin_unlock_irqrestore(&kpi_timer_lock, flags);
}

int hrtimer_try_to_cancel(struct hrtimer *timer)
{
	unsigned long flags;
	int ret;

	raw_spin_lock_irqsave(&kpi_timer_lock, flags);
	if (READ_ONCE(timer->base->running) == timer)
		ret = -1;
	else
		ret = kpi_hrtimer_detach(timer);
	raw_spin_unlock_irqrestore(&kpi_timer_lock, flags);
	return ret;
}

int hrtimer_cancel(struct hrtimer *timer)
{
	int ret;

	for (;;) {
		ret = hrtimer_try_to_cancel(timer);
		if (ret >= 0)
			return ret;
		rustos_kpi_yield();
	}
}

bool hrtimer_active(const struct hrtimer *timer)
{
	return (READ_ONCE(timer->state) & HRTIMER_STATE_ENQUEUED) ||
	       READ_ONCE(timer->base->running) == timer;
}

ktime_t hrtimer_cb_get_time(const struct hrtimer *timer)
{
	return kpi_clock_now(timer->base->clockid);
}

u64 hrtimer_forward(struct hrtimer *timer, ktime_t now, ktime_t interval)
{
	ktime_t delta = ktime_sub(now, hrtimer_get_expires(timer));
	u64 orun = 1;

	if (delta < 0)
		return 0;
	if (interval < 1)
		interval = 1;
	if (delta >= interval) {
		s64 incr = ktime_to_ns(interval);

		orun = ktime_divns(delta, incr);
		hrtimer_add_expires_ns(timer, incr * orun);
		if (hrtimer_get_expires_tv64(timer) > now)
			return orun;
		orun++;
	}
	hrtimer_add_expires(timer, interval);
	return orun;
}

ktime_t __hrtimer_get_remaining(const struct hrtimer *timer, bool adjust)
{
	return ktime_sub(hrtimer_get_expires(timer), kpi_clock_now(timer->base->clockid));
}

/* ------------------------------------------------------------ workqueues */

/*
 * Workers per workqueue: ordered queues have one; the others start with
 * KPI_WQ_MIN_WORKERS and grow up to KPI_WQ_MAX_WORKERS, a worker being
 * added whenever one takes an item and none is left idle. Like Linux's
 * worker pools this keeps queued work from starving behind items that
 * block on it (mac80211's wiphy work holds the lock other items wait on).
 */
#define KPI_WQ_MIN_WORKERS	2
#define KPI_WQ_MAX_WORKERS	64
#define KPI_MAX_RUNNING		256

struct workqueue_struct {
	char name[32];
	unsigned int flags;
	int nr_workers;
	struct list_head pending;
	wait_queue_head_t more;		/* work arrived */
	wait_queue_head_t idle;		/* a work item finished */
	int running;
	int idle_workers;		/* workers waiting for work */
	int max_workers;
};

static DEFINE_RAW_SPINLOCK(kpi_wq_lock);
static struct work_struct *kpi_running_work[KPI_MAX_RUNNING];

/* Every workqueue, for kpi_wq_dump(). */
#define KPI_MAX_WQS 128
static struct workqueue_struct *kpi_wqs[KPI_MAX_WQS];

struct workqueue_struct *system_wq, *system_percpu_wq, *system_highpri_wq, *system_long_wq,
	*system_unbound_wq, *system_dfl_wq, *system_freezable_wq, *system_power_efficient_wq,
	*system_freezable_power_efficient_wq, *system_bh_wq, *system_bh_highpri_wq;

static bool kpi_work_running(struct work_struct *work)
{
	for (int i = 0; i < KPI_MAX_RUNNING; i++)
		if (READ_ONCE(kpi_running_work[i]) == work)
			return true;
	return false;
}

static void kpi_worker(void *arg)
{
	struct workqueue_struct *wq = arg;
	unsigned long flags;

	for (;;) {
		struct work_struct *work = NULL;
		bool grow;
		int slot;

		raw_spin_lock_irqsave(&kpi_wq_lock, flags);
		wq->idle_workers++;
		raw_spin_unlock_irqrestore(&kpi_wq_lock, flags);
		wait_event(wq->more, !list_empty(&wq->pending));
		raw_spin_lock_irqsave(&kpi_wq_lock, flags);
		wq->idle_workers--;
		if (list_empty(&wq->pending)) {
			raw_spin_unlock_irqrestore(&kpi_wq_lock, flags);
			continue;
		}
		/* Keep a worker free for what gets queued while this runs. */
		grow = wq->idle_workers == 0 && wq->nr_workers < wq->max_workers;
		if (grow)
			wq->nr_workers++;
		work = list_first_entry(&wq->pending, struct work_struct, entry);
		list_del_init(&work->entry);
		for (slot = 0; slot < KPI_MAX_RUNNING && kpi_running_work[slot]; slot++)
			;
		if (slot < KPI_MAX_RUNNING)
			kpi_running_work[slot] = work;
		clear_bit(WORK_STRUCT_PENDING_BIT, work_data_bits(work));
		wq->running++;
		raw_spin_unlock_irqrestore(&kpi_wq_lock, flags);
		if (grow)
			rustos_kpi_spawn(kpi_worker, wq, wq->name);

		work->func(work);

		raw_spin_lock_irqsave(&kpi_wq_lock, flags);
		if (slot < KPI_MAX_RUNNING)
			kpi_running_work[slot] = NULL;
		wq->running--;
		raw_spin_unlock_irqrestore(&kpi_wq_lock, flags);
		wake_up_all(&wq->idle);
	}
}

struct workqueue_struct *alloc_workqueue_noprof(const char *fmt, unsigned int flags,
						 int max_active, ...)
{
	struct workqueue_struct *wq = kzalloc(sizeof(*wq), GFP_KERNEL);
	va_list ap;

	if (!wq)
		return NULL;
	va_start(ap, max_active);
	vsnprintf(wq->name, sizeof(wq->name), fmt, ap);
	va_end(ap);
	wq->flags = flags;
	INIT_LIST_HEAD(&wq->pending);
	init_waitqueue_head(&wq->more);
	init_waitqueue_head(&wq->idle);
	wq->max_workers = (flags & __WQ_ORDERED) || max_active == 1 ? 1 : KPI_WQ_MAX_WORKERS;
	wq->nr_workers = min(wq->max_workers, KPI_WQ_MIN_WORKERS);
	for (int i = 0; i < KPI_MAX_WQS; i++) {
		if (!cmpxchg(&kpi_wqs[i], NULL, wq))
			break;
	}
	for (int i = 0; i < wq->nr_workers; i++)
		rustos_kpi_spawn(kpi_worker, wq, wq->name);
	return wq;
}

/* sysrq state dump: workqueues with queued or running work. Lock-free
 * reads: the dump must not wait on a lock a stuck thread holds. */
void kpi_wq_dump(void);
void kpi_wq_dump(void)
{
	for (int i = 0; i < KPI_MAX_WQS; i++) {
		struct workqueue_struct *wq = READ_ONCE(kpi_wqs[i]);
		int queued = 0;
		struct list_head *p;

		if (!wq)
			continue;
		if (raw_spin_trylock(&kpi_wq_lock)) {
			list_for_each(p, &wq->pending)
				queued++;
			raw_spin_unlock(&kpi_wq_lock);
		} else {
			queued = -1;
		}
		if (queued || READ_ONCE(wq->running))
			pr_info("[sysrq] workqueue %s: %d queued, %d running\n", wq->name, queued,
				READ_ONCE(wq->running));
	}
}

void destroy_workqueue(struct workqueue_struct *wq)
{
	__flush_workqueue(wq);
	/* The workers stay parked on the empty queue; the struct leaks. */
}

static void kpi_queue(struct workqueue_struct *wq, struct work_struct *work)
{
	unsigned long flags;

	raw_spin_lock_irqsave(&kpi_wq_lock, flags);
	list_add_tail(&work->entry, &wq->pending);
	raw_spin_unlock_irqrestore(&kpi_wq_lock, flags);
	wake_up(&wq->more);
}

/* Work items disabled by disable_work*() (and how often): queueing them
 * fails until enable_work*() balances the count. Linux keeps the count in
 * work->data; a small table does here. */
#define KPI_MAX_DISABLED 128
static struct {
	struct work_struct *work;
	int count;
} kpi_disabled[KPI_MAX_DISABLED];

static bool kpi_work_disabled(struct work_struct *work)
{
	for (int i = 0; i < KPI_MAX_DISABLED; i++)
		if (READ_ONCE(kpi_disabled[i].work) == work)
			return true;
	return false;
}

bool queue_work_on(int cpu, struct workqueue_struct *wq, struct work_struct *work)
{
	if (kpi_work_disabled(work))
		return false;
	if (test_and_set_bit(WORK_STRUCT_PENDING_BIT, work_data_bits(work)))
		return false;
	kpi_queue(wq, work);
	return true;
}

bool queue_work_node(int node, struct workqueue_struct *wq, struct work_struct *work)
{
	return queue_work_on(WORK_CPU_UNBOUND, wq, work);
}

void delayed_work_timer_fn(struct timer_list *t)
{
	struct delayed_work *dwork = timer_container_of(dwork, t, timer);

	if (kpi_work_disabled(&dwork->work)) {
		clear_bit(WORK_STRUCT_PENDING_BIT, work_data_bits(&dwork->work));
		return;
	}
	kpi_queue(dwork->wq, &dwork->work);
}

bool queue_delayed_work_on(int cpu, struct workqueue_struct *wq, struct delayed_work *dwork,
			   unsigned long delay)
{
	if (kpi_work_disabled(&dwork->work))
		return false;
	if (test_and_set_bit(WORK_STRUCT_PENDING_BIT, work_data_bits(&dwork->work)))
		return false;
	dwork->wq = wq;
	dwork->cpu = cpu;
	if (!delay) {
		kpi_queue(wq, &dwork->work);
		return true;
	}
	mod_timer(&dwork->timer, jiffies + delay);
	return true;
}

/* Remove a queued work item. Returns true if it was pending. */
static bool kpi_cancel(struct work_struct *work)
{
	unsigned long flags;
	bool was;

	raw_spin_lock_irqsave(&kpi_wq_lock, flags);
	if (!list_empty(&work->entry))
		list_del_init(&work->entry);
	was = test_and_clear_bit(WORK_STRUCT_PENDING_BIT, work_data_bits(work));
	raw_spin_unlock_irqrestore(&kpi_wq_lock, flags);
	return was;
}

bool mod_delayed_work_on(int cpu, struct workqueue_struct *wq, struct delayed_work *dwork,
			 unsigned long delay)
{
	bool was;

	timer_delete(&dwork->timer);
	was = kpi_cancel(&dwork->work);
	queue_delayed_work_on(cpu, wq, dwork, delay);
	return was;
}

bool cancel_work(struct work_struct *work)
{
	return kpi_cancel(work);
}

bool cancel_work_sync(struct work_struct *work)
{
	bool was = kpi_cancel(work);

	while (kpi_work_running(work))
		rustos_kpi_yield();
	return was;
}

bool cancel_delayed_work(struct delayed_work *dwork)
{
	bool t = timer_delete(&dwork->timer);

	return kpi_cancel(&dwork->work) || t;
}

bool cancel_delayed_work_sync(struct delayed_work *dwork)
{
	bool t = timer_delete_sync(&dwork->timer);

	return cancel_work_sync(&dwork->work) || t;
}

bool disable_work(struct work_struct *work)
{
	unsigned long flags;
	bool was = kpi_cancel(work);
	int free = -1;

	raw_spin_lock_irqsave(&kpi_wq_lock, flags);
	for (int i = 0; i < KPI_MAX_DISABLED; i++) {
		if (kpi_disabled[i].work == work) {
			kpi_disabled[i].count++;
			free = -2;
			break;
		}
		if (!kpi_disabled[i].work && free == -1)
			free = i;
	}
	if (free >= 0) {
		kpi_disabled[free].work = work;
		kpi_disabled[free].count = 1;
	}
	raw_spin_unlock_irqrestore(&kpi_wq_lock, flags);
	WARN_ON_ONCE(free == -1);
	return was;
}

bool disable_work_sync(struct work_struct *work)
{
	bool was = disable_work(work);

	while (kpi_work_running(work))
		rustos_kpi_yield();
	return was;
}

bool enable_work(struct work_struct *work)
{
	unsigned long flags;
	bool enabled = true;

	raw_spin_lock_irqsave(&kpi_wq_lock, flags);
	for (int i = 0; i < KPI_MAX_DISABLED; i++) {
		if (kpi_disabled[i].work != work)
			continue;
		if (--kpi_disabled[i].count == 0)
			kpi_disabled[i].work = NULL;
		else
			enabled = false;
		break;
	}
	raw_spin_unlock_irqrestore(&kpi_wq_lock, flags);
	return enabled;
}

bool disable_delayed_work(struct delayed_work *dwork)
{
	bool t = timer_delete(&dwork->timer);

	return disable_work(&dwork->work) || t;
}

bool disable_delayed_work_sync(struct delayed_work *dwork)
{
	bool t = timer_delete_sync(&dwork->timer);

	return disable_work_sync(&dwork->work) || t;
}

bool enable_delayed_work(struct delayed_work *dwork)
{
	return enable_work(&dwork->work);
}

unsigned int work_busy(struct work_struct *work)
{
	unsigned int ret = 0;

	if (test_bit(WORK_STRUCT_PENDING_BIT, work_data_bits(work)))
		ret |= WORK_BUSY_PENDING;
	if (kpi_work_running(work))
		ret |= WORK_BUSY_RUNNING;
	return ret;
}

bool flush_work(struct work_struct *work)
{
	bool waited = false;

	while (test_bit(WORK_STRUCT_PENDING_BIT, work_data_bits(work)) || kpi_work_running(work)) {
		waited = true;
		rustos_kpi_yield();
	}
	return waited;
}

bool flush_delayed_work(struct delayed_work *dwork)
{
	if (timer_delete_sync(&dwork->timer))
		kpi_queue(dwork->wq, &dwork->work);
	return flush_work(&dwork->work);
}

void __flush_workqueue(struct workqueue_struct *wq)
{
	wait_event(wq->idle, list_empty(&wq->pending) && READ_ONCE(wq->running) == 0);
}

void drain_workqueue(struct workqueue_struct *wq)
{
	__flush_workqueue(wq);
}

struct work_struct *current_work(void)
{
	return NULL;
}

int kpi_workqueues_init(void)
{
	system_percpu_wq = alloc_workqueue("events", 0, 0);
	system_wq = system_percpu_wq;
	system_highpri_wq = alloc_workqueue("events_highpri", WQ_HIGHPRI, 0);
	system_long_wq = alloc_workqueue("events_long", 0, 0);
	system_unbound_wq = alloc_workqueue("events_unbound", WQ_UNBOUND, 0);
	system_dfl_wq = system_unbound_wq;
	system_freezable_wq = system_percpu_wq;
	system_power_efficient_wq = system_percpu_wq;
	system_freezable_power_efficient_wq = system_percpu_wq;
	system_bh_wq = system_highpri_wq;
	system_bh_highpri_wq = system_highpri_wq;
	if (!system_percpu_wq || !system_highpri_wq || !system_long_wq || !system_unbound_wq)
		return -ENOMEM;
	/* kernel/async.c's queue (start_kernel() sets it up in Linux): drivers
	 * that prefer asynchronous probing (sdhci-pci) use it. */
	async_init();
	return 0;
}

/* -------------------------------------------- jiffies conversions (time.c) */

unsigned int jiffies_to_msecs(const unsigned long j)
{
	return (MSEC_PER_SEC / HZ) * j;
}

unsigned int jiffies_to_usecs(const unsigned long j)
{
	return (USEC_PER_SEC / HZ) * j;
}

u64 jiffies64_to_nsecs(u64 j)
{
	return j * (NSEC_PER_SEC / HZ);
}

u64 jiffies64_to_msecs(const u64 j)
{
	return j * (MSEC_PER_SEC / HZ);
}

unsigned long __msecs_to_jiffies(const unsigned int m)
{
	if ((int)m < 0)
		return MAX_JIFFY_OFFSET;
	return DIV_ROUND_UP(m, MSEC_PER_SEC / HZ);
}

unsigned long __usecs_to_jiffies(const unsigned int u)
{
	if (u > jiffies_to_usecs(MAX_JIFFY_OFFSET))
		return MAX_JIFFY_OFFSET;
	return DIV_ROUND_UP(u, USEC_PER_SEC / HZ);
}

u64 nsecs_to_jiffies64(u64 n)
{
	return div_u64(n, NSEC_PER_SEC / HZ);
}

unsigned long nsecs_to_jiffies(u64 n)
{
	return (unsigned long)nsecs_to_jiffies64(n);
}

clock_t jiffies_to_clock_t(unsigned long x)
{
	return x / (HZ / USER_HZ);
}

void jiffies_to_timespec64(const unsigned long jiffies, struct timespec64 *value)
{
	u64 ns = jiffies64_to_nsecs(jiffies);

	value->tv_sec = div_u64_rem(ns, NSEC_PER_SEC, (u32 *)&value->tv_nsec);
}

/* ----------------------------------------------------------- timekeeping */

ktime_t ktime_get(void)
{
	return rustos_kpi_nanos();
}

ktime_t ktime_get_raw(void)
{
	return rustos_kpi_nanos();
}

ktime_t ktime_get_with_offset(enum tk_offsets offs)
{
	/* Boot time and TAI are taken as monotonic (no suspend, no leap seconds). */
	return offs == TK_OFFS_REAL ? rustos_kpi_realtime_ns() : rustos_kpi_nanos();
}

ktime_t ktime_get_coarse_with_offset(enum tk_offsets offs)
{
	return ktime_get_with_offset(offs);
}

u64 ktime_get_mono_fast_ns(void)
{
	return rustos_kpi_nanos();
}

u64 ktime_get_raw_fast_ns(void)
{
	return rustos_kpi_nanos();
}

u64 ktime_get_boot_fast_ns(void)
{
	return rustos_kpi_nanos();
}

u64 ktime_get_real_fast_ns(void)
{
	return rustos_kpi_realtime_ns();
}

void ktime_get_ts64(struct timespec64 *ts)
{
	*ts = ns_to_timespec64(rustos_kpi_nanos());
}

void ktime_get_real_ts64(struct timespec64 *ts)
{
	*ts = ns_to_timespec64(rustos_kpi_realtime_ns());
}

time64_t ktime_get_seconds(void)
{
	return rustos_kpi_nanos() / NSEC_PER_SEC;
}

time64_t ktime_get_real_seconds(void)
{
	return rustos_kpi_realtime_ns() / NSEC_PER_SEC;
}

/* Every workqueue runs its items on its own threads; nothing to tune. */
void workqueue_set_min_active(struct workqueue_struct *wq, int min_active)
{
}

struct timespec64 ns_to_timespec64(s64 nsec)
{
	struct timespec64 ts = { 0, 0 };
	s32 rem;

	if (likely(nsec > 0)) {
		ts.tv_sec = div_u64_rem(nsec, NSEC_PER_SEC, &rem);
		ts.tv_nsec = rem;
	} else if (nsec < 0) {
		ts.tv_sec = -div_u64_rem(-nsec - 1, NSEC_PER_SEC, &rem) - 1;
		ts.tv_nsec = NSEC_PER_SEC - rem - 1;
	}
	return ts;
}

/* From kernel/time/timer.c (round to a whole second, never into the past). */
static unsigned long kpi_round_jiffies(unsigned long j, bool force_up)
{
	unsigned long original = j;
	int rem = j % HZ;

	if (rem < HZ / 4 && !force_up)
		j = j - rem;
	else
		j = j - rem + HZ;
	return time_is_after_jiffies(j) ? j : original;
}

unsigned long round_jiffies(unsigned long j)
{
	return kpi_round_jiffies(j, false);
}

unsigned long round_jiffies_relative(unsigned long j)
{
	unsigned long j0 = jiffies;

	return kpi_round_jiffies(j + j0, false) - j0;
}

unsigned long round_jiffies_up(unsigned long j)
{
	return kpi_round_jiffies(j, true);
}

unsigned long round_jiffies_up_relative(unsigned long j)
{
	unsigned long j0 = jiffies;

	return kpi_round_jiffies(j + j0, true) - j0;
}

void set_normalized_timespec64(struct timespec64 *ts, time64_t sec, s64 nsec)
{
	while (nsec >= NSEC_PER_SEC) {
		nsec -= NSEC_PER_SEC;
		++sec;
	}
	while (nsec < 0) {
		nsec += NSEC_PER_SEC;
		--sec;
	}
	ts->tv_sec = sec;
	ts->tv_nsec = nsec;
}

void ktime_get_clock_ts64(clockid_t id, struct timespec64 *ts)
{
	if (id == CLOCK_REALTIME)
		ktime_get_real_ts64(ts);
	else
		*ts = ktime_to_timespec64(ktime_get());
}

/* No cross-timestamping between device and system clocks (PTP). */
int get_device_system_crosststamp(int (*get_time_fn)(ktime_t *device_time,
						     struct system_counterval_t *sys_counterval,
						     void *ctx),
				  void *ctx, struct system_time_snapshot *history,
				  struct system_device_crosststamp *xtstamp)
{
	return -EOPNOTSUPP;
}

u64 sched_clock(void)
{
	return ktime_get_ns();
}

/* Monotonic time to another clock (offsets of real time; boot and TAI
 * equal monotonic here: no suspend, no leap second table). */
ktime_t ktime_mono_to_any(ktime_t tmono, enum tk_offsets offs)
{
	if (offs == TK_OFFS_REAL)
		return ktime_add(tmono, ktime_sub(ktime_get_real(), ktime_get()));
	return tmono;
}

/* No NTP adjustment: the raw monotonic clock is the monotonic clock. */
void ktime_get_raw_ts64(struct timespec64 *ts)
{
	ktime_get_ts64(ts);
}
