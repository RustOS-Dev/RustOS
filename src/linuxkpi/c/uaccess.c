// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * x86's get_user()/put_user() out-of-line helpers (arch/x86/lib/getuser.S,
 * putuser.S), on the shim's copy_from_user()/copy_to_user().
 *
 * They have a register interface, since inline assembly calls them:
 * __get_user_N takes the address in %rax and returns the error in %rax and
 * the value in %rdx; __put_user_N takes the value in %rax and the address
 * in %rcx and returns the error in %rcx. Nothing else may change, so the
 * thunks save every register a C call clobbers.
 */
#include <linux/linkage.h>
#include <linux/uaccess.h>
#include "kpi.h"

int kpi_get_user(const void __user *uptr, u64 *val, unsigned long n);
int kpi_put_user(void __user *uptr, const u64 *val, unsigned long n);

int kpi_get_user(const void __user *uptr, u64 *val, unsigned long n)
{
	*val = 0;
	if (copy_from_user(val, uptr, n)) {
		*val = 0;
		return -EFAULT;
	}
	return 0;
}

int kpi_put_user(void __user *uptr, const u64 *val, unsigned long n)
{
	return copy_to_user(uptr, val, n) ? -EFAULT : 0;
}

/* On entry %rsp is 8 modulo 16: seven pushes and a 16-byte slot keep the
 * call aligned. */
#define KPI_GET_USER(name, size)					\
	asm(".text\n.globl " #name "\n.type " #name ",@function\n"	\
	    #name ":\n"							\
	    "push %rcx\npush %rsi\npush %rdi\npush %r8\n"		\
	    "push %r9\npush %r10\npush %r11\n"				\
	    "sub $16, %rsp\n"						\
	    "mov %rax, %rdi\nmov %rsp, %rsi\nmov $" #size ", %edx\n"	\
	    "call kpi_get_user\n"					\
	    "movslq %eax, %rax\nmov (%rsp), %rdx\n"			\
	    "add $16, %rsp\n"						\
	    "pop %r11\npop %r10\npop %r9\npop %r8\n"			\
	    "pop %rdi\npop %rsi\npop %rcx\nret\n"			\
	    ".size " #name ", .-" #name "\n")

/* Eight pushes and an 8-byte slot. */
#define KPI_PUT_USER(name, size)					\
	asm(".text\n.globl " #name "\n.type " #name ",@function\n"	\
	    #name ":\n"							\
	    "push %rax\npush %rdx\npush %rsi\npush %rdi\n"		\
	    "push %r8\npush %r9\npush %r10\npush %r11\n"		\
	    "sub $8, %rsp\nmov %rax, (%rsp)\n"				\
	    "mov %rcx, %rdi\nmov %rsp, %rsi\nmov $" #size ", %edx\n"	\
	    "call kpi_put_user\n"					\
	    "movslq %eax, %rcx\n"					\
	    "add $8, %rsp\n"						\
	    "pop %r11\npop %r10\npop %r9\npop %r8\n"			\
	    "pop %rdi\npop %rsi\npop %rdx\npop %rax\nret\n"		\
	    ".size " #name ", .-" #name "\n")

KPI_GET_USER(__get_user_1, 1);
KPI_GET_USER(__get_user_2, 2);
KPI_GET_USER(__get_user_4, 4);
KPI_GET_USER(__get_user_8, 8);
KPI_GET_USER(__get_user_nocheck_1, 1);
KPI_GET_USER(__get_user_nocheck_2, 2);
KPI_GET_USER(__get_user_nocheck_4, 4);
KPI_GET_USER(__get_user_nocheck_8, 8);
KPI_PUT_USER(__put_user_1, 1);
KPI_PUT_USER(__put_user_2, 2);
KPI_PUT_USER(__put_user_4, 4);
KPI_PUT_USER(__put_user_8, 8);
KPI_PUT_USER(__put_user_nocheck_1, 1);
KPI_PUT_USER(__put_user_nocheck_2, 2);
KPI_PUT_USER(__put_user_nocheck_4, 4);
KPI_PUT_USER(__put_user_nocheck_8, 8);

void __copy_overflow(int size, unsigned long count)
{
	WARN(1, "Buffer overflow detected (%d < %lu)!\n", size, count);
}
