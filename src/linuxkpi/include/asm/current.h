/* SPDX-License-Identifier: GPL-2.0-or-later */
/*
 * RustOS replacement for arch/x86/include/asm/current.h: `current` is the
 * Linux task_struct shadow of the running RustOS thread, created on first
 * use (src/linuxkpi/c/task.c).
 */
#ifndef _ASM_X86_CURRENT_H
#define _ASM_X86_CURRENT_H

#include <linux/compiler.h>

#ifndef __ASSEMBLER__

struct task_struct;

struct task_struct *rustos_kpi_current(void) __attribute_const__;

static __always_inline struct task_struct *get_current(void)
{
	return rustos_kpi_current();
}

#define current get_current()

#endif /* __ASSEMBLER__ */
#endif /* _ASM_X86_CURRENT_H */
