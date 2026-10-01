/* SPDX-License-Identifier: GPL-2.0-or-later */
/*
 * RustOS replacement for arch/x86/include/asm/bug.h. Linux implements
 * BUG()/WARN() as a ud2 trap plus a bug table its trap handler reads;
 * here they are calls: BUG() panics RustOS, WARN() logs a backtrace and
 * continues (src/linuxkpi/mod.rs).
 */
#ifndef _ASM_X86_BUG_H
#define _ASM_X86_BUG_H

#include <linux/stringify.h>

#define ASM_UD2		"ud2"
#define INSN_UD2	0x0b0f
#define LEN_UD2		2
#define BUG_NONE	0xffff
#define BUG_UD2		0xfffe
#define BUG_UD1		0xfffd
#define BUG_UD1_UBSAN	0xfffc
#define BUG_UDB		0xffd6
#define BUG_LOCK	0xfff0

#ifndef __ASSEMBLER__
void rustos_kpi_bug(const char *file, int line) __attribute__((noreturn, cold));
void rustos_kpi_warn(const char *file, int line) __attribute__((cold));

#define HAVE_ARCH_BUG
#define BUG() rustos_kpi_bug(__FILE__, __LINE__)
#define __WARN_FLAGS(flags) rustos_kpi_warn(__FILE__, __LINE__)
#endif

#include <asm-generic/bug.h>

#endif /* _ASM_X86_BUG_H */
