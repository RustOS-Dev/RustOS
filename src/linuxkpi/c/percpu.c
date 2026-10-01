// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * LinuxKPI per-CPU areas. Every DEFINE_PER_CPU variable lives in the
 * `kpipcpu` section (see include/asm/percpu.h); each CPU gets a copy and
 * its offset is stored in the RustOS per-CPU block, which the generic
 * per-CPU accessors read. Until kpi_percpu_init() runs, the offset is 0
 * and every CPU shares the original section.
 */
#include <linux/percpu.h>
#include <linux/slab.h>
#include <linux/smp.h>
#include <linux/string.h>
#include "kpi.h"

DEFINE_PER_CPU_READ_MOSTLY(int, cpu_number);
unsigned long __per_cpu_offset[NR_CPUS];
extern char __start_kpipcpu[], __stop_kpipcpu[];

unsigned int nr_cpu_ids = NR_CPUS;
struct cpumask __cpu_possible_mask, __cpu_online_mask, __cpu_present_mask,
	__cpu_active_mask, __cpu_dying_mask;
atomic_t __num_online_cpus;

int kpi_percpu_init(void)
{
	unsigned int n = rustos_kpi_cpu_count();
	size_t size = __stop_kpipcpu - __start_kpipcpu;

	nr_cpu_ids = n;
	atomic_set(&__num_online_cpus, n);
	for (unsigned int cpu = 0; cpu < n; cpu++) {
		char *area = kmalloc(max_t(size_t, size, 64), GFP_KERNEL);

		if (!area)
			return -ENOMEM;
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
