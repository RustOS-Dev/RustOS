// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * LinuxKPI odds and ends: module parameter ops, system_state, random
 * numbers, linker-script symbols Linux code expects.
 */
#include <linux/capability.h>
#include <linux/ctype.h>
#include <linux/crc32.h>
#include <linux/kernel.h>
#include <linux/limits.h>
#include <linux/moduleparam.h>
#include <linux/random.h>
#include <linux/refcount.h>
#include <linux/string.h>
#include <net/dropreason.h>
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

/* ------------------------------------------------------------------ CRC32 */

/* Table-driven CRC32 (IEEE 802.3), little- and big-endian bit order, as
 * lib/crc/crc32-main.c defines them (no pre/post inversion). */
static u32 kpi_crc32_le_tab[256], kpi_crc32_be_tab[256];

static void kpi_crc32_init(void)
{
	for (u32 i = 0; i < 256; i++) {
		u32 le = i, be = i << 24;

		for (int k = 0; k < 8; k++) {
			le = (le >> 1) ^ (le & 1 ? 0xedb88320 : 0);
			be = (be << 1) ^ (be & 0x80000000 ? 0x04c11db7 : 0);
		}
		kpi_crc32_le_tab[i] = le;
		kpi_crc32_be_tab[i] = be;
	}
}

u32 crc32_le(u32 crc, const void *p, size_t len)
{
	const u8 *b = p;

	if (unlikely(!kpi_crc32_le_tab[1]))
		kpi_crc32_init();
	while (len--)
		crc = (crc >> 8) ^ kpi_crc32_le_tab[(crc & 0xff) ^ *b++];
	return crc;
}

u32 crc32_be(u32 crc, const void *p, size_t len)
{
	const u8 *b = p;

	if (unlikely(!kpi_crc32_be_tab[1]))
		kpi_crc32_init();
	while (len--)
		crc = (crc << 8) ^ kpi_crc32_be_tab[(crc >> 24) ^ *b++];
	return crc;
}

/* ----------------------------------------------------------------- sscanf */

/*
 * vsscanf for the conversions kernel code uses: %d %i %u %x %X %o with
 * hh/h/l/ll/z/j modifiers, %s, %c, %n, %%, field widths and '*'.
 */
int vsscanf(const char *buf, const char *fmt, va_list args)
{
	const char *str = buf;
	int num = 0;

	while (*fmt) {
		int width = -1, qual = 0, base = 10;
		bool skip = false, is_signed = false;
		unsigned long long val;
		char *end;

		if (isspace(*fmt)) {
			fmt = skip_spaces(fmt);
			str = skip_spaces(str);
			continue;
		}
		if (*fmt != '%' || fmt[1] == '%') {
			if (*fmt == '%')
				fmt++;
			if (*fmt++ != *str++)
				break;
			continue;
		}
		fmt++;
		if (*fmt == '*') {
			skip = true;
			fmt++;
		}
		if (isdigit(*fmt))
			width = simple_strtoul(fmt, (char **)&fmt, 10);
		switch (*fmt) {
		case 'h':
			qual = 'h';
			if (*++fmt == 'h') {
				qual = 'H';
				fmt++;
			}
			break;
		case 'l':
			qual = 'l';
			if (*++fmt == 'l') {
				qual = 'L';
				fmt++;
			}
			break;
		case 'z':
		case 'j':
		case 'L':
			qual = 'L';
			fmt++;
			break;
		}
		if (!*fmt)
			break;
		if (*fmt == 'n') {
			if (!skip)
				*va_arg(args, int *) = str - buf;
			fmt++;
			continue;
		}
		if (!*str)
			break;
		switch (*fmt++) {
		case 'c':
			if (width < 0)
				width = 1;
			if (skip) {
				while (width-- > 0 && *str)
					str++;
			} else {
				char *s = va_arg(args, char *);

				while (width-- > 0 && *str)
					*s++ = *str++;
				num++;
			}
			continue;
		case 's': {
			char *s = skip ? NULL : va_arg(args, char *);

			if (width < 0)
				width = INT_MAX;
			str = skip_spaces(str);
			while (*str && !isspace(*str) && width-- > 0) {
				if (s)
					*s++ = *str;
				str++;
			}
			if (s) {
				*s = '\0';
				num++;
			}
			continue;
		}
		case 'd':
			is_signed = true;
			break;
		case 'i':
			is_signed = true;
			base = 0;
			break;
		case 'u':
			break;
		case 'x':
		case 'X':
			base = 16;
			break;
		case 'o':
			base = 8;
			break;
		default:
			return num;
		}
		str = skip_spaces(str);
		{
			/* Honour the field width by scanning a bounded copy. */
			char tmp[32];
			const char *src = str;
			int n = 0;

			while (src[n] && n < (int)sizeof(tmp) - 1 && (width < 0 || n < width))
				n++;
			memcpy(tmp, src, n);
			tmp[n] = '\0';
			if (is_signed && tmp[0] == '-') {
				val = -(long long)simple_strtoull(tmp + 1, &end, base);
				if (end == tmp + 1)
					return num;
			} else {
				val = simple_strtoull(tmp[0] == '+' ? tmp + 1 : tmp, &end, base);
				if (end == tmp || (tmp[0] == '+' && end == tmp + 1))
					return num;
			}
			str += end - tmp;
		}
		if (skip)
			continue;
		switch (qual) {
		case 'H':
			*va_arg(args, u8 *) = val;
			break;
		case 'h':
			*va_arg(args, u16 *) = val;
			break;
		case 'l':
			*va_arg(args, unsigned long *) = val;
			break;
		case 'L':
			*va_arg(args, unsigned long long *) = val;
			break;
		default:
			*va_arg(args, unsigned int *) = val;
		}
		num++;
	}
	return num;
}

int sscanf(const char *buf, const char *fmt, ...)
{
	va_list args;
	int i;

	va_start(args, fmt);
	i = vsscanf(buf, fmt, args);
	va_end(args);
	return i;
}

/* ------------------------------------------------------------- odds/ends */

/* Drop reasons are only names for tracing, which is off. */
void drop_reasons_register_subsys(enum skb_drop_reason_subsys subsys,
				  const struct drop_reason_list *list)
{
}

void drop_reasons_unregister_subsys(enum skb_drop_reason_subsys subsys)
{
}

/* Netlink extended-ack tracepoint (tracing is off). */
void do_trace_netlink_extack(const char *msg)
{
}

/* No capabilities in RustOS: root has them all. */
bool ns_capable(struct user_namespace *ns, int cap)
{
	return rustos_kpi_current_uid() == 0;
}

bool ns_capable_noaudit(struct user_namespace *ns, int cap)
{
	return ns_capable(ns, cap);
}

bool capable(int cap)
{
	return ns_capable(NULL, cap);
}

bool file_ns_capable(const struct file *file, struct user_namespace *ns, int cap)
{
	return ns_capable(ns, cap);
}

/* Module parameters are set once at boot (src/params.rs). */
void kernel_param_lock(struct module *mod)
{
}

void kernel_param_unlock(struct module *mod)
{
}
