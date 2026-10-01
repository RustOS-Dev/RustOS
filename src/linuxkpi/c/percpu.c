// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * LinuxKPI per-CPU areas. Every DEFINE_PER_CPU variable lives in the
 * `kpipcpu` section (see include/asm/percpu.h); each CPU gets a copy and
 * its offset is stored in the RustOS per-CPU block, which the generic
 * per-CPU accessors read. Until kpi_percpu_init() runs, the offset is 0
 * and every CPU shares the original section.
 */
#include <linux/percpu.h>
#include <linux/bitmap.h>
#include <linux/slab.h>
#include <linux/smp.h>
#include <linux/spinlock.h>
#include <linux/string.h>
#include <linux/vmalloc.h>
#include "kpi.h"

DEFINE_PER_CPU_READ_MOSTLY(int, cpu_number);
unsigned long __per_cpu_offset[NR_CPUS];
extern char __start_kpipcpu[], __stop_kpipcpu[];

unsigned int nr_cpu_ids = NR_CPUS;
struct cpumask __cpu_possible_mask, __cpu_online_mask, __cpu_present_mask,
	__cpu_active_mask, __cpu_dying_mask;
atomic_t __num_online_cpus;

/*
 * Each CPU's unit is its copy of the kpipcpu section followed by a dynamic
 * area for alloc_percpu(). The units are laid out at a fixed stride in one
 * vmalloc'd block, so a dynamic per-CPU pointer (an address just past the
 * section, never dereferenced directly) plus __per_cpu_offset[cpu] lands
 * in that CPU's dynamic area, as with the static variables.
 */
#define KPI_PCPU_DYN_SIZE	(256 * 1024)
#define KPI_PCPU_GRANULE	16
#define KPI_PCPU_GRANULES	(KPI_PCPU_DYN_SIZE / KPI_PCPU_GRANULE)

static size_t kpi_pcpu_static;		/* section size, rounded to a granule */
static size_t kpi_pcpu_unit;		/* stride between CPUs' units */
static DEFINE_SPINLOCK(kpi_pcpu_lock);
static DECLARE_BITMAP(kpi_pcpu_used, KPI_PCPU_GRANULES);
static u32 kpi_pcpu_len[KPI_PCPU_GRANULES];	/* granules, at each allocation start */

int kpi_percpu_init(void)
{
	unsigned int n = rustos_kpi_cpu_count();
	size_t size = __stop_kpipcpu - __start_kpipcpu;
	char *base;

	kpi_pcpu_static = ALIGN(size, KPI_PCPU_GRANULE);
	kpi_pcpu_unit = ALIGN(kpi_pcpu_static + KPI_PCPU_DYN_SIZE, PAGE_SIZE);
	base = vzalloc(kpi_pcpu_unit * n);
	if (!base)
		return -ENOMEM;
	nr_cpu_ids = n;
	atomic_set(&__num_online_cpus, n);
	for (unsigned int cpu = 0; cpu < n; cpu++) {
		char *area = base + cpu * kpi_pcpu_unit;

		memcpy(area, __start_kpipcpu, size);
		__per_cpu_offset[cpu] = area - __start_kpipcpu;
		per_cpu(cpu_number, cpu) = cpu;
		cpumask_set_cpu(cpu, &__cpu_possible_mask);
		cpumask_set_cpu(cpu, &__cpu_online_mask);
		cpumask_set_cpu(cpu, &__cpu_present_mask);
		cpumask_set_cpu(cpu, &__cpu_active_mask);
		rustos_kpi_set_cpu_offset(cpu, __per_cpu_offset[cpu]);
	}
	return 0;
}

void __percpu *pcpu_alloc_noprof(size_t size, size_t align, bool reserved, gfp_t gfp)
{
	unsigned long n = DIV_ROUND_UP(max_t(size_t, size, 1), KPI_PCPU_GRANULE);
	unsigned long step = max_t(unsigned long, align / KPI_PCPU_GRANULE, 1);
	unsigned long flags, start;
	char *p;
	int cpu;

	if (!kpi_pcpu_unit || n > KPI_PCPU_GRANULES)
		return NULL;
	spin_lock_irqsave(&kpi_pcpu_lock, flags);
	start = bitmap_find_next_zero_area(kpi_pcpu_used, KPI_PCPU_GRANULES, 0, n, step - 1);
	if (start >= KPI_PCPU_GRANULES) {
		spin_unlock_irqrestore(&kpi_pcpu_lock, flags);
		pr_warn("linuxkpi: per-CPU dynamic area full (%zu bytes requested)\n", size);
		return NULL;
	}
	bitmap_set(kpi_pcpu_used, start, n);
	kpi_pcpu_len[start] = n;
	spin_unlock_irqrestore(&kpi_pcpu_lock, flags);

	p = __start_kpipcpu + kpi_pcpu_static + start * KPI_PCPU_GRANULE;
	for_each_possible_cpu(cpu)
		memset(per_cpu_ptr((void __percpu *)p, cpu), 0, n * KPI_PCPU_GRANULE);
	return (void __percpu *)p;
}

void free_percpu(void __percpu *ptr)
{
	unsigned long start, flags;

	if (!ptr)
		return;
	start = ((char __force *)ptr - __start_kpipcpu - kpi_pcpu_static) / KPI_PCPU_GRANULE;
	if (WARN_ON(start >= KPI_PCPU_GRANULES || !kpi_pcpu_len[start]))
		return;
	spin_lock_irqsave(&kpi_pcpu_lock, flags);
	bitmap_clear(kpi_pcpu_used, start, kpi_pcpu_len[start]);
	kpi_pcpu_len[start] = 0;
	spin_unlock_irqrestore(&kpi_pcpu_lock, flags);
}

bool is_kernel_percpu_address(unsigned long addr)
{
	return addr >= (unsigned long)__start_kpipcpu &&
	       addr < (unsigned long)__start_kpipcpu + kpi_pcpu_unit;
}
