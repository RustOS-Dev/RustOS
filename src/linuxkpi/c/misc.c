// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * LinuxKPI odds and ends: module parameter ops, system_state, random
 * numbers, linker-script symbols Linux code expects.
 */
#include <linux/kernel.h>
#include <linux/moduleparam.h>
#include <linux/random.h>
#include <linux/refcount.h>
#include <linux/string.h>
#include "kpi.h"

enum system_states system_state = SYSTEM_RUNNING;

/* kernel/locking/spinlock.c: in_lock_functions() compares against these. */
char __lock_text_start[0], __lock_text_end[0];

/*
 * Module parameters keep their compiled-in defaults for now; setting them
 * from kernel.conf (<module>.<param>=) is planned with the device core.
 */
static int kpi_param_set(const char *val, const struct kernel_param *kp)
{
	return 0;
}

static int kpi_param_get(char *buffer, const struct kernel_param *kp)
{
	return 0;
}

#define KPI_PARAM_OPS(name) \
	const struct kernel_param_ops param_ops_##name = { .set = kpi_param_set, .get = kpi_param_get }
KPI_PARAM_OPS(byte);
KPI_PARAM_OPS(short);
KPI_PARAM_OPS(ushort);
KPI_PARAM_OPS(int);
KPI_PARAM_OPS(uint);
KPI_PARAM_OPS(long);
KPI_PARAM_OPS(ulong);
KPI_PARAM_OPS(ullong);
KPI_PARAM_OPS(hexint);
KPI_PARAM_OPS(charp);
KPI_PARAM_OPS(bool);
KPI_PARAM_OPS(bool_enable_only);
KPI_PARAM_OPS(invbool);
KPI_PARAM_OPS(string);
const struct kernel_param_ops param_array_ops = { .set = kpi_param_set, .get = kpi_param_get };

void get_random_bytes(void *buf, size_t len)
{
	u8 *p = buf;

	while (len) {
		u64 r = rustos_kpi_random_u64();
		size_t n = min(len, sizeof(r));

		memcpy(p, &r, n);
		p += n;
		len -= n;
	}
}

u8 get_random_u8(void)
{
	return rustos_kpi_random_u64();
}

u16 get_random_u16(void)
{
	return rustos_kpi_random_u64();
}

u32 get_random_u32(void)
{
	return rustos_kpi_random_u64();
}

u64 get_random_u64(void)
{
	return rustos_kpi_random_u64();
}

u32 __get_random_u32_below(u32 ceil)
{
	return ((u64)get_random_u32() * ceil) >> 32;
}

/* From lib/refcount.c. */
void refcount_warn_saturate(refcount_t *r, enum refcount_saturation_type t)
{
	static const char *const what[] = {
		[REFCOUNT_ADD_NOT_ZERO_OVF] = "saturated; leaking memory",
		[REFCOUNT_ADD_OVF] = "saturated; leaking memory",
		[REFCOUNT_ADD_UAF] = "addition on 0; use-after-free",
		[REFCOUNT_SUB_UAF] = "underflow; use-after-free",
		[REFCOUNT_DEC_LEAK] = "decrement hit 0; leaking memory",
	};

	refcount_set(r, REFCOUNT_SATURATED);
	WARN(1, "refcount_t: %s\n",
	     t < ARRAY_SIZE(what) && what[t] ? what[t] : "unknown saturation event!?");
}
