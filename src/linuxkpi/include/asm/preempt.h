/* SPDX-License-Identifier: GPL-2.0-or-later */
/*
 * RustOS replacement for arch/x86/include/asm/preempt.h: Linux's
 * preempt_count is RustOS's own per-CPU preemption counter, so a Linux
 * spin_lock() or local_bh_disable() also holds off RustOS preemption, and
 * in_interrupt()/in_atomic() see RustOS's state. The need-resched flag is
 * RustOS's separate per-CPU `need_resched` word.
 */
#ifndef __ASM_PREEMPT_H
#define __ASM_PREEMPT_H

#include <linux/compiler.h>
#include <linux/stringify.h>
#include <rustos/percpu-layout.h>

#define __rustos_pc_ref	"%%gs:" __stringify(RUSTOS_PERCPU_PREEMPT_COUNT)

static __always_inline int preempt_count(void)
{
	int pc;

	asm volatile("movl " __rustos_pc_ref ", %0" : "=r"(pc));
	return pc;
}

static __always_inline void preempt_count_set(int pc)
{
	asm volatile("movl %0, " __rustos_pc_ref : : "r"(pc) : "memory");
}

#define init_task_preempt_count(p) do { } while (0)
#define init_idle_preempt_count(p, cpu) do { } while (0)

static __always_inline void set_preempt_need_resched(void)
{
	asm volatile("movl $1, %%gs:" __stringify(RUSTOS_PERCPU_NEED_RESCHED) ::: "memory");
}

static __always_inline void clear_preempt_need_resched(void)
{
	asm volatile("movl $0, %%gs:" __stringify(RUSTOS_PERCPU_NEED_RESCHED) ::: "memory");
}

static __always_inline bool test_preempt_need_resched(void)
{
	int v;

	asm volatile("movl %%gs:" __stringify(RUSTOS_PERCPU_NEED_RESCHED) ", %0" : "=r"(v));
	return v != 0;
}

static __always_inline void __preempt_count_add(int val)
{
	asm volatile("lock addl %0, " __rustos_pc_ref : : "ir"(val) : "memory");
}

static __always_inline void __preempt_count_sub(int val)
{
	asm volatile("lock subl %0, " __rustos_pc_ref : : "ir"(val) : "memory");
}

static __always_inline bool __preempt_count_dec_and_test(void)
{
	bool zero;

	asm volatile("lock decl " __rustos_pc_ref "; sete %0"
		     : "=qm"(zero) : : "memory", "cc");
	return zero && test_preempt_need_resched();
}

static __always_inline bool should_resched(int preempt_offset)
{
	return preempt_count() == preempt_offset && test_preempt_need_resched();
}

/* CONFIG_PREEMPTION: preempt_enable() reschedules when the count drops to
 * zero with a reschedule pending (src/linuxkpi/c/sched.c). */
void preempt_schedule(void);
void preempt_schedule_notrace(void);
#define __preempt_schedule()		preempt_schedule()
#define __preempt_schedule_notrace()	preempt_schedule_notrace()

#endif /* __ASM_PREEMPT_H */
