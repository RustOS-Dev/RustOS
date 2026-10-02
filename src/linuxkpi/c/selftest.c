// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * Boot-time self-test of the LinuxKPI primitives, run by
 * src/linuxkpi/mod.rs before any Linux driver probes. Each failure is
 * logged; the return value is the number of failures.
 */
#include <linux/acpi.h>
#include <linux/completion.h>
#include <linux/interrupt.h>
#include <linux/irq.h>
#include <linux/irqdomain.h>
#include <linux/delay.h>
#include <linux/jiffies.h>
#include <linux/kthread.h>
#include <linux/mm.h>
#include <linux/mutex.h>
#include <linux/percpu.h>
#include <linux/rcupdate.h>
#include <linux/skbuff.h>
#include <linux/srcu.h>
#include <linux/ww_mutex.h>
#include <linux/slab.h>
#include <linux/spinlock.h>
#include <linux/string.h>
#include <linux/timer.h>
#include <linux/vmalloc.h>
#include <linux/wait.h>
#include <linux/workqueue.h>
#include "kpi.h"

static int failures;

#define CHECK(cond, what) do {						\
	if (!(cond)) {							\
		pr_err("linuxkpi self-test: %s failed (%s:%d)\n",	\
		       what, __FILE__, __LINE__);			\
		failures++;						\
	}								\
} while (0)

static DEFINE_PER_CPU(int, kpi_test_counter);

static void test_memory(void)
{
	void *small[32];
	void *big = kmalloc(9000, GFP_KERNEL | __GFP_ZERO);
	struct page *pages = alloc_pages(GFP_KERNEL, 2);
	char *v = vmalloc(3 * PAGE_SIZE + 5);

	for (int i = 0; i < 32; i++) {
		size_t sz = 8 << (i % 9);

		small[i] = kmalloc(sz, GFP_KERNEL);
		CHECK(small[i] && ((unsigned long)small[i] & (min_t(size_t, sz, 16) - 1)) == 0,
		      "kmalloc alignment");
		if (small[i])
			memset(small[i], i, sz);
	}
	for (int i = 0; i < 32; i++) {
		size_t sz = 8 << (i % 9);

		CHECK(!small[i] || memchr_inv(small[i], i, sz) == NULL, "kmalloc contents");
		kfree(small[i]);
	}
	CHECK(big && !memchr_inv(big, 0, 9000), "kzalloc large");
	CHECK(big && virt_to_page(big) == pfn_to_page(__pa(big) >> PAGE_SHIFT), "virt_to_page");
	CHECK(big && __va(__pa(big)) == big, "__va(__pa())");
	kfree(big);
	CHECK(pages && page_address(pages) && page_ref_count(pages) == 1, "alloc_pages");
	if (pages) {
		memset(page_address(pages), 0xa5, 4 * PAGE_SIZE);
		__free_pages(pages, 2);
	}
	CHECK(v && is_vmalloc_addr(v), "vmalloc");
	if (v) {
		v[3 * PAGE_SIZE + 4] = 1;
		vfree(v);
	}
	CHECK(ksize(kmalloc(100, GFP_KERNEL)) >= 100, "ksize");
}

static void test_percpu(void)
{
	this_cpu_inc(kpi_test_counter);
	this_cpu_add(kpi_test_counter, 2);
	CHECK(this_cpu_read(kpi_test_counter) == 3, "this_cpu ops");
	CHECK(per_cpu(kpi_test_counter, raw_smp_processor_id()) == 3, "per_cpu()");
	CHECK(raw_smp_processor_id() == rustos_kpi_cpu_id(), "smp_processor_id");
}

static DEFINE_SPINLOCK(test_lock);
static DEFINE_MUTEX(test_mutex);

static void test_locks(void)
{
	unsigned long flags;

	spin_lock_irqsave(&test_lock, flags);
	CHECK(in_atomic() && irqs_disabled(), "spin_lock_irqsave state");
	spin_unlock_irqrestore(&test_lock, flags);
	CHECK(!in_atomic(), "preempt count after unlock");
	spin_lock_bh(&test_lock);
	CHECK(in_softirq(), "spin_lock_bh state");
	spin_unlock_bh(&test_lock);
	mutex_lock(&test_mutex);
	CHECK(mutex_is_locked(&test_mutex) && !mutex_trylock(&test_mutex), "mutex held");
	mutex_unlock(&test_mutex);
	CHECK(!mutex_is_locked(&test_mutex), "mutex released");
}

static unsigned long contended;
static DECLARE_COMPLETION(contend_done);

static int contend_fn(void *data)
{
	for (int i = 0; i < 200000; i++) {
		spin_lock(&test_lock);
		contended++;
		spin_unlock(&test_lock);
	}
	complete(&contend_done);
	return 0;
}

static void test_contention(void)
{
	struct task_struct *a = kthread_run(contend_fn, NULL, "kpi-spin-a");
	struct task_struct *b = kthread_run(contend_fn, NULL, "kpi-spin-b");

	CHECK(!IS_ERR(a) && !IS_ERR(b), "contention threads");
	wait_for_completion(&contend_done);
	wait_for_completion(&contend_done);
	CHECK(contended == 400000, "spinlock mutual exclusion");
	if (contended != 400000)
		pr_err("linuxkpi self-test: counter %lu, expected 400000\n", contended);
}

static DECLARE_COMPLETION(kthread_done);

static int test_thread_fn(void *data)
{
	mutex_lock(&test_mutex);
	*(int *)data = 42;
	mutex_unlock(&test_mutex);
	complete(&kthread_done);
	while (!kthread_should_stop())
		msleep(1);
	return 7;
}

static void test_kthread(void)
{
	static int value;
	struct task_struct *t = kthread_run(test_thread_fn, &value, "kpi-test");

	CHECK(!IS_ERR(t), "kthread_run");
	if (IS_ERR(t))
		return;
	CHECK(wait_for_completion_timeout(&kthread_done, msecs_to_jiffies(2000)), "completion");
	CHECK(value == 42, "kthread ran");
	CHECK(kthread_stop(t) == 7, "kthread_stop result");
}

static struct timer_list test_timer;
static DECLARE_COMPLETION(timer_done);
static DECLARE_WAIT_QUEUE_HEAD(test_wq);
static int timer_hits;

static void test_timer_fn(struct timer_list *t)
{
	timer_hits++;
	complete(&timer_done);
	wake_up(&test_wq);
}

static void test_timers(void)
{
	unsigned long start = jiffies;
	u64 t0 = rustos_kpi_nanos();

	msleep(20);
	CHECK(rustos_kpi_nanos() - t0 >= 20 * NSEC_PER_MSEC, "msleep duration");
	CHECK(time_after(jiffies, start), "jiffies advance");

	timer_setup(&test_timer, test_timer_fn, 0);
	mod_timer(&test_timer, jiffies + msecs_to_jiffies(10));
	CHECK(timer_pending(&test_timer), "timer_pending");
	CHECK(wait_for_completion_timeout(&timer_done, msecs_to_jiffies(2000)), "timer fired");
	CHECK(!timer_pending(&test_timer), "timer not pending after firing");

	/* A deleted timer must not fire. */
	mod_timer(&test_timer, jiffies + msecs_to_jiffies(20));
	CHECK(timer_delete_sync(&test_timer) == 1, "timer_delete_sync of pending timer");
	msleep(40);
	CHECK(timer_hits == 1, "deleted timer did not fire");

	mod_timer(&test_timer, jiffies + msecs_to_jiffies(10));
	CHECK(wait_event_timeout(test_wq, timer_hits == 2, msecs_to_jiffies(2000)) > 0,
	      "wait_event_timeout woken by timer");
}

static int work_runs;
static void test_work_fn(struct work_struct *w)
{
	work_runs++;
}
static DECLARE_WORK(test_work, test_work_fn);
static DECLARE_DELAYED_WORK(test_dwork, test_work_fn);

static void test_workqueues(void)
{
	queue_work(system_wq, &test_work);
	flush_work(&test_work);
	CHECK(work_runs == 1, "queue_work + flush_work");
	schedule_delayed_work(&test_dwork, msecs_to_jiffies(10));
	CHECK(delayed_work_pending(&test_dwork), "delayed work pending");
	msleep(100);
	CHECK(work_runs == 2, "delayed work ran");
	schedule_delayed_work(&test_dwork, msecs_to_jiffies(50));
	CHECK(cancel_delayed_work_sync(&test_dwork), "cancel_delayed_work_sync");
	msleep(80);
	CHECK(work_runs == 2, "cancelled delayed work did not run");
}

static atomic_t rcu_reader_state;
static DECLARE_COMPLETION(rcu_reader_done);
static DECLARE_COMPLETION(rcu_cb_done);

static int rcu_reader_fn(void *data)
{
	rcu_read_lock();
	atomic_set(&rcu_reader_state, 1);
	mdelay(50);
	atomic_set(&rcu_reader_state, 2);
	rcu_read_unlock();
	complete(&rcu_reader_done);
	return 0;
}

struct rcu_test_obj {
	int value;
	struct rcu_head rcu;
};

static void rcu_test_cb(struct rcu_head *head)
{
	struct rcu_test_obj *o = container_of(head, struct rcu_test_obj, rcu);

	o->value = 42;
	complete(&rcu_cb_done);
}

static void test_rcu(void)
{
	static struct rcu_test_obj obj;
	struct rcu_test_obj *freed = kmalloc(sizeof(*freed), GFP_KERNEL);
	struct task_struct *t = kthread_run(rcu_reader_fn, NULL, "kpi-rcu-reader");

	CHECK(!IS_ERR(t), "RCU reader thread");
	if (IS_ERR(t))
		return;
	while (atomic_read(&rcu_reader_state) == 0)
		msleep(1);
	synchronize_rcu();
	CHECK(atomic_read(&rcu_reader_state) == 2, "synchronize_rcu waits for readers");
	wait_for_completion(&rcu_reader_done);

	call_rcu(&obj.rcu, rcu_test_cb);
	CHECK(wait_for_completion_timeout(&rcu_cb_done, msecs_to_jiffies(2000)) &&
	      obj.value == 42, "call_rcu callback");
	if (freed)
		kfree_rcu(freed, rcu);
	rcu_barrier();
}

static DEFINE_WW_CLASS(kpi_test_ww_class);

static void test_ww_mutex(void)
{
	struct ww_mutex a, b;
	struct ww_acquire_ctx old, young;

	ww_mutex_init(&a, &kpi_test_ww_class);
	ww_mutex_init(&b, &kpi_test_ww_class);
	ww_acquire_init(&old, &kpi_test_ww_class);
	ww_acquire_init(&young, &kpi_test_ww_class);
	CHECK(!ww_mutex_lock(&a, &old), "ww_mutex_lock (old)");
	CHECK(!ww_mutex_lock(&b, &young), "ww_mutex_lock (young)");
	CHECK(ww_mutex_lock(&a, &young) == -EDEADLK, "wait-die: younger backs off");
	CHECK(ww_mutex_lock(&a, &old) == -EALREADY, "ww_mutex_lock -EALREADY");
	ww_mutex_unlock(&b);
	CHECK(young.acquired == 0, "ww acquired count");
	CHECK(!ww_mutex_lock(&b, &old) && old.acquired == 2, "older takes released lock");
	ww_mutex_unlock(&a);
	ww_mutex_unlock(&b);
	ww_acquire_fini(&old);
	ww_acquire_fini(&young);
}

DEFINE_STATIC_SRCU(kpi_test_srcu);
static atomic_t srcu_reader_state;
static DECLARE_COMPLETION(srcu_reader_done);
static DECLARE_COMPLETION(srcu_cb_done);

static int srcu_reader_fn(void *data)
{
	int idx = srcu_read_lock(&kpi_test_srcu);

	atomic_set(&srcu_reader_state, 1);
	msleep(30);	/* SRCU readers may sleep. */
	atomic_set(&srcu_reader_state, 2);
	srcu_read_unlock(&kpi_test_srcu, idx);
	complete(&srcu_reader_done);
	return 0;
}

static void srcu_test_cb(struct rcu_head *head)
{
	complete(&srcu_cb_done);
}

static void test_srcu(void)
{
	static struct rcu_head head;
	struct task_struct *t = kthread_run(srcu_reader_fn, NULL, "kpi-srcu-reader");

	CHECK(!IS_ERR(t), "SRCU reader thread");
	if (IS_ERR(t))
		return;
	while (atomic_read(&srcu_reader_state) == 0)
		msleep(1);
	synchronize_srcu(&kpi_test_srcu);
	CHECK(atomic_read(&srcu_reader_state) == 2, "synchronize_srcu waits for readers");
	wait_for_completion(&srcu_reader_done);
	call_srcu(&kpi_test_srcu, &head, srcu_test_cb);
	srcu_barrier(&kpi_test_srcu);
	CHECK(completion_done(&srcu_cb_done), "call_srcu + srcu_barrier");
}

static void test_skb(void)
{
	struct sk_buff *skb = alloc_skb(128, GFP_KERNEL);
	u8 buf[8];

	CHECK(skb, "alloc_skb");
	if (!skb)
		return;
	skb_reserve(skb, 16);
	skb_put_data(skb, "payload!", 8);
	memcpy(skb_push(skb, 4), "hdr:", 4);
	CHECK(skb->len == 12 && !memcmp(skb->data, "hdr:payload!", 12), "skb put/push");
	skb_pull(skb, 4);
	CHECK(skb->len == 8 && !memcmp(skb->data, "payload!", 8), "skb_pull");
	CHECK(!pskb_expand_head(skb, 64, 64, GFP_KERNEL) && skb_headroom(skb) >= 64 + 16 &&
	      !memcmp(skb->data, "payload!", 8), "pskb_expand_head keeps data");
	skb_trim(skb, 3);
	CHECK(skb->len == 3, "skb_trim");
	CHECK(!skb_copy_bits(skb, 1, buf, 2) && !memcmp(buf, "ay", 2), "skb_copy_bits");
	kfree_skb(skb);
}

static void test_acpi(void)
{
	struct acpi_buffer buf = { ACPI_ALLOCATE_BUFFER, NULL };
	unsigned long long sta;
	acpi_handle sb;
	union acpi_object *o;

	CHECK(ACPI_SUCCESS(acpi_get_handle(NULL, "\\_SB", &sb)) && sb, "acpi_get_handle");
	CHECK(acpi_get_handle(NULL, "\\_SB.NOPE", &sb) == AE_NOT_FOUND, "acpi_get_handle missing");
	/* Every x86 machine's DSDT has a PCI root bridge with a _PRT package. */
	if (ACPI_SUCCESS(acpi_evaluate_object(NULL, "\\_SB.PCI0._PRT", NULL, &buf))) {
		o = buf.pointer;
		CHECK(o->type == ACPI_TYPE_PACKAGE && o->package.count > 0 &&
		      o->package.elements[0].type == ACPI_TYPE_PACKAGE, "_PRT package");
		kfree(o);
	}
	if (acpi_has_method(NULL, "\\_SB.PCI0._STA"))
		CHECK(ACPI_SUCCESS(acpi_evaluate_integer(NULL, "\\_SB.PCI0._STA", NULL, &sta)),
		      "acpi_evaluate_integer");
}

/* An interrupt controller like a GPIO chip's: a domain, a level flow
 * handler, a one-shot threaded handler that keeps the line masked. */
static int irq_masks, irq_unmasks, irq_thread_runs;
static DECLARE_COMPLETION(irq_thread_done);

static void test_irq_mask(struct irq_data *d)
{
	irq_masks++;
}

static void test_irq_unmask(struct irq_data *d)
{
	irq_unmasks++;
}

static struct irq_chip test_irq_chip = {
	.name = "kpi-test",
	.irq_mask = test_irq_mask,
	.irq_unmask = test_irq_unmask,
};

static int test_irq_map(struct irq_domain *d, unsigned int virq, irq_hw_number_t hw)
{
	irq_set_chip_and_handler(virq, &test_irq_chip, handle_level_irq);
	irq_set_chip_data(virq, d->host_data);
	return 0;
}

static const struct irq_domain_ops test_irq_ops = {
	.map = test_irq_map,
	.xlate = irq_domain_xlate_twocell,
};

static irqreturn_t test_irq_thread(int irq, void *dev)
{
	irq_thread_runs++;
	/* Still masked while the thread runs. */
	CHECK(irq_masks == irq_unmasks + 1, "one-shot line masked in thread");
	complete(&irq_thread_done);
	return IRQ_HANDLED;
}

static void test_irq_domain(void)
{
	struct irq_domain *d = irq_domain_create_linear(NULL, 4, &test_irq_ops, &test_irq_chip);
	unsigned int virq;
	unsigned long flags;

	CHECK(d, "irq_domain_create_linear");
	if (!d)
		return;
	virq = irq_create_mapping(d, 2);
	CHECK(virq && irq_find_mapping(d, 2) == virq && !irq_find_mapping(d, 1),
	      "irq_create_mapping");
	CHECK(irq_get_irq_data(virq)->hwirq == 2 &&
	      irq_get_chip_data(virq) == &test_irq_chip, "irq data");
	CHECK(!request_threaded_irq(virq, NULL, test_irq_thread, IRQF_ONESHOT |
				    IRQF_TRIGGER_LOW, "kpi-test", &test_irq_chip),
	      "request_threaded_irq");
	CHECK(irq_get_trigger_type(virq) == IRQ_TYPE_LEVEL_LOW, "trigger type");
	irq_masks = irq_unmasks = 0;
	local_irq_save(flags);
	CHECK(!generic_handle_domain_irq(d, 2), "generic_handle_domain_irq");
	local_irq_restore(flags);
	CHECK(wait_for_completion_timeout(&irq_thread_done, HZ), "irq thread ran");
	synchronize_irq(virq);
	CHECK(irq_thread_runs == 1 && irq_masks == 1 && irq_unmasks == 1,
	      "one-shot mask/unmask");
	/* Disabled: the interrupt is remembered, not handled. */
	disable_irq(virq);
	disable_irq(virq);
	enable_irq(virq);
	local_irq_save(flags);
	generic_handle_domain_irq(d, 2);
	local_irq_restore(flags);
	CHECK(irq_thread_runs == 1, "disabled irq not handled");
	enable_irq(virq);
	free_irq(virq, &test_irq_chip);
	irq_dispose_mapping(virq);
	CHECK(!irq_find_mapping(d, 2), "irq_dispose_mapping");
	irq_domain_remove(d);
}

/* QEMU's PS/2 keyboard: IO(0x60), IO(0x64), IRQNoFlags(1). */
static acpi_status test_kbd_res(struct acpi_resource *r, void *ctx)
{
	int *seen = ctx;

	if (r->type == ACPI_RESOURCE_TYPE_IO && r->data.io.minimum == 0x60)
		*seen |= 1;
	if (r->type == ACPI_RESOURCE_TYPE_IRQ && r->data.irq.interrupt_count == 1 &&
	    r->data.irq.interrupts[0] == 1)
		*seen |= 2;
	if (r->type == ACPI_RESOURCE_TYPE_END_TAG)
		*seen |= 4;
	return AE_OK;
}

static void test_acpi_devices(void)
{
	struct acpi_device *kbd = acpi_dev_get_first_match_dev("PNP0303", NULL, -1);
	acpi_handle a, b;
	int seen = 0;

	CHECK(ACPI_SUCCESS(acpi_get_handle(NULL, "\\_SB.PCI0", &a)) &&
	      ACPI_SUCCESS(acpi_get_handle(NULL, "\\_SB_.PCI0", &b)) && a == b,
	      "ACPI path normalisation");
	if (!kbd)
		return;	/* not every machine has one */
	CHECK(!strcmp(dev_name(&kbd->dev), "PNP0303:00"), "ACPI device name");
	CHECK(acpi_fetch_acpi_dev(kbd->handle) == kbd, "acpi_fetch_acpi_dev");
	CHECK(ACPI_SUCCESS(acpi_walk_resources(kbd->handle, METHOD_NAME__CRS, test_kbd_res,
					       &seen)) && seen == 7, "_CRS resources");
	acpi_dev_put(kbd);
}

static void test_printf(void)
{
	static const u8 mac[6] = { 0x52, 0x54, 0x00, 0x12, 0x34, 0x56 };
	static const u8 ip[4] = { 10, 0, 2, 15 };
	char buf[96];

	snprintf(buf, sizeof(buf), "%pM %pI4 %5d|%-3s|%#x %pe %*phC", mac, ip, -42, "ab", 255,
		 ERR_PTR(-ENOMEM), 3, mac);
	CHECK(!strcmp(buf, "52:54:00:12:34:56 10.0.2.15   -42|ab |0xff -ENOMEM 52:54:00"),
	      "vsnprintf formats");
	if (strcmp(buf, "52:54:00:12:34:56 10.0.2.15   -42|ab |0xff -ENOMEM 52:54:00"))
		pr_err("linuxkpi self-test: got \"%s\"\n", buf);
}

int kpi_selftest(void)
{
	static const struct { const char *name; void (*fn)(void); } tests[] = {
		{ "memory", test_memory }, { "per-CPU", test_percpu },
		{ "locks", test_locks }, { "printf", test_printf },
		{ "kthread", test_kthread }, { "spinlock contention", test_contention },
		{ "timers", test_timers },
		{ "workqueues", test_workqueues },
		{ "RCU", test_rcu },
		{ "ww_mutex", test_ww_mutex },
		{ "SRCU", test_srcu },
		{ "skb", test_skb },
		{ "ACPI", test_acpi },
		{ "ACPI devices", test_acpi_devices },
		{ "IRQ domains", test_irq_domain },
	};

	failures = 0;
	for (int i = 0; i < ARRAY_SIZE(tests); i++) {
		printk(KERN_DEBUG "linuxkpi self-test: %s\n", tests[i].name);
		tests[i].fn();
	}
	return failures;
}
