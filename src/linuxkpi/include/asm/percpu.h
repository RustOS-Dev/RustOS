/* SPDX-License-Identifier: GPL-2.0-or-later */
/*
 * RustOS replacement for arch/x86/include/asm/percpu.h.
 *
 * Linux reaches per-CPU variables as %gs:(&var), with GS based at the
 * CPU's per-CPU offset. RustOS's GS base points at its own per-CPU block,
 * so Linux code uses the generic per-CPU implementation instead:
 * &var + offset, where the offset is read from the RustOS block. All
 * per-CPU variables go into one section, `kpipcpu`, that LinuxKPI copies
 * for each CPU at boot (src/linuxkpi/percpu.rs).
 */
#ifndef _ASM_X86_PERCPU_H
#define _ASM_X86_PERCPU_H

#ifndef __ASSEMBLER__

#include <linux/args.h>
#include <linux/bits.h>
#include <linux/build_bug.h>
#include <linux/stringify.h>
#include <rustos/percpu-layout.h>

#define __percpu_seg_override
#define __force_percpu_prefix	""
#define __percpu_prefix		""

static __always_inline unsigned long __rustos_my_cpu_offset(void)
{
	unsigned long off;

	asm volatile("movq %%gs:" __stringify(RUSTOS_PERCPU_LINUX_OFFSET) ", %0"
		     : "=r"(off));
	return off;
}
#define __my_cpu_offset __rustos_my_cpu_offset()

#include <asm-generic/percpu.h>

/* One section for every per-CPU variable (a C identifier, so the linker
 * provides __start_kpipcpu/__stop_kpipcpu). */
#undef __PCPU_ATTRS
#define __PCPU_ATTRS(sec)						\
	__percpu __attribute__((section("kpipcpu")))			\
	PER_CPU_ATTRIBUTES

#define this_cpu_read_stable(pcp)	(*raw_cpu_ptr(&(pcp)))
#define this_cpu_read_const(pcp)	(*raw_cpu_ptr(&(pcp)))
#define raw_cpu_read_long(pcp)		raw_cpu_read(pcp)
#define x86_this_cpu_test_bit(_nr, _var) test_bit(_nr, raw_cpu_ptr(&(_var)))

#define DECLARE_EARLY_PER_CPU(_type, _name)			\
	DECLARE_PER_CPU(_type, _name);				\
	extern __typeof__(_type) *_name##_early_ptr;		\
	extern __typeof__(_type)  _name##_early_map[]
#define DECLARE_EARLY_PER_CPU_READ_MOSTLY(_type, _name)		\
	DECLARE_PER_CPU_READ_MOSTLY(_type, _name);		\
	extern __typeof__(_type) *_name##_early_ptr;		\
	extern __typeof__(_type)  _name##_early_map[]
#define DEFINE_EARLY_PER_CPU_READ_MOSTLY(_type, _name, _initvalue) \
	DEFINE_PER_CPU_READ_MOSTLY(_type, _name) = _initvalue;	\
	__typeof__(_type) _name##_early_map[NR_CPUS] __initdata = \
				{ [0 ... NR_CPUS-1] = _initvalue }; \
	__typeof__(_type) *_name##_early_ptr __refdata = _name##_early_map
#define EXPORT_EARLY_PER_CPU_SYMBOL(_name)
#define early_per_cpu_ptr(_name)	(_name##_early_ptr)
#define early_per_cpu_map(_name, _idx)	(_name##_early_map[_idx])
#define early_per_cpu(_name, _cpu)				\
	*(early_per_cpu_ptr(_name) ?				\
		&early_per_cpu_ptr(_name)[_cpu] :		\
		&per_cpu(_name, _cpu))

#endif /* __ASSEMBLER__ */
#endif /* _ASM_X86_PERCPU_H */
