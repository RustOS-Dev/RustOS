// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * LinuxKPI logging: vsnprintf and friends, printk, dev_*()/netdev_*()
 * messages, WARN text. Output goes to the RustOS kernel log.
 *
 * vsnprintf is written here rather than imported: Linux's lib/vsprintf.c
 * pulls in kallsyms, siphash, dentry paths and page-flag tables. This one
 * handles the conversions drivers use, including the %p extensions
 * %pM %pm %pI4 %pi4 %pI6 %pI6c %pa %pad %pe %*ph[CDN] %pV %pS/%ps/%pB.
 */
#include <linux/ctype.h>
#include <linux/kernel.h>
#include <linux/device.h>
#include <linux/netdevice.h>
#include <linux/printk.h>
#include <linux/slab.h>
#include <linux/string.h>
#include <linux/ratelimit.h>
#include <linux/errname.h>
#include "kpi.h"

struct kpi_out {
	char *buf;
	size_t size;
	size_t len;		/* would-be length */
};

static void out_c(struct kpi_out *o, char c)
{
	if (o->len + 1 < o->size)
		o->buf[o->len] = c;
	o->len++;
}

static void out_s(struct kpi_out *o, const char *s, int width, int prec, bool left)
{
	int n = 0;

	if (!s)
		s = "(null)";
	while ((prec < 0 || n < prec) && s[n])
		n++;
	if (!left)
		while (width-- > n)
			out_c(o, ' ');
	for (int i = 0; i < n; i++)
		out_c(o, s[i]);
	if (left)
		while (width-- > n)
			out_c(o, ' ');
}

static void out_num(struct kpi_out *o, unsigned long long v, bool neg, int base,
		    bool upper, int width, int prec, bool left, bool zero, char sign,
		    bool alt)
{
	const char *digits = upper ? "0123456789ABCDEF" : "0123456789abcdef";
	char tmp[24];
	int n = 0, pad;
	int prefix = 0;

	if (v == 0 && prec != 0)
		tmp[n++] = '0';
	while (v) {
		tmp[n++] = digits[v % base];
		v /= base;
	}
	if (alt && base == 16)
		prefix = 2;
	else if (alt && base == 8 && (n == 0 || tmp[n - 1] != '0'))
		prefix = 1;
	pad = width - (prec > n ? prec : n) - prefix - (neg || sign ? 1 : 0);
	if (!left && !(zero && prec < 0))
		while (pad-- > 0)
			out_c(o, ' ');
	if (neg)
		out_c(o, '-');
	else if (sign)
		out_c(o, sign);
	if (prefix) {
		out_c(o, '0');
		if (base == 16)
			out_c(o, upper ? 'X' : 'x');
	}
	if (!left && zero && prec < 0)
		while (pad-- > 0)
			out_c(o, '0');
	for (int i = n; i < prec; i++)
		out_c(o, '0');
	while (n)
		out_c(o, tmp[--n]);
	if (left)
		while (pad-- > 0)
			out_c(o, ' ');
}

static void out_hex8(struct kpi_out *o, u8 b, bool upper)
{
	const char *d = upper ? "0123456789ABCDEF" : "0123456789abcdef";

	out_c(o, d[b >> 4]);
	out_c(o, d[b & 15]);
}

static void kpi_vformat(struct kpi_out *o, const char *fmt, va_list ap);

/* %p extensions. `fmt` points just past the 'p'; returns chars consumed. */
static int out_pointer(struct kpi_out *o, const char *fmt, void *p, int width)
{
	int used = 0;

	switch (fmt[0]) {
	case 'M': case 'm': {		/* MAC address */
		const u8 *a = p;
		bool colon = fmt[0] == 'M';

		used = 1;
		if (fmt[1] == 'F' || fmt[1] == 'R')
			used = 2;
		for (int i = 0; i < 6; i++) {
			if (i && colon)
				out_c(o, fmt[1] == 'F' ? '-' : ':');
			out_hex8(o, a[fmt[1] == 'R' ? 5 - i : i], false);
		}
		return used;
	}
	case 'I': case 'i':
		if (fmt[1] == '4') {
			const u8 *a = p;

			for (int i = 0; i < 4; i++) {
				if (i)
					out_c(o, '.');
				out_num(o, a[i], false, 10, false, 0, -1, false, false, 0, false);
			}
			return 2;
		}
		if (fmt[1] == '6') {
			const u8 *a = p;

			used = fmt[2] == 'c' ? 3 : 2;
			for (int i = 0; i < 16; i += 2) {
				if (i)
					out_c(o, ':');
				if (used == 3)
					out_num(o, (a[i] << 8) | a[i + 1], false, 16, false, 0, -1,
						false, false, 0, false);
				else {
					out_hex8(o, a[i], false);
					out_hex8(o, a[i + 1], false);
				}
			}
			return used;
		}
		break;
	case 'a':			/* phys_addr_t / dma_addr_t by reference */
		used = (fmt[1] == 'd' || fmt[1] == 'p') ? 2 : 1;
		out_c(o, '0');
		out_c(o, 'x');
		out_num(o, *(u64 *)p, false, 16, false, 16, -1, false, true, 0, false);
		return used;
	case 'e': {			/* error pointer */
		long err = PTR_ERR(p);
		const char *name = errname(err);

		if (name)
			out_s(o, name, 0, -1, false);
		else
			out_num(o, -err, true, 10, false, 0, -1, false, false, 0, false);
		return 1;
	}
	case 'h': {			/* hex buffer, width = length */
		const u8 *a = p;
		char sep = ' ';
		int n = width > 0 ? width : 1;

		used = 1;
		if (fmt[1] == 'C') { sep = ':'; used = 2; }
		else if (fmt[1] == 'D') { sep = '-'; used = 2; }
		else if (fmt[1] == 'N') { sep = 0; used = 2; }
		for (int i = 0; i < n && i < 64; i++) {
			if (i && sep)
				out_c(o, sep);
			out_hex8(o, a[i], false);
		}
		return used;
	}
	case 'V': {			/* struct va_format */
		struct va_format *vaf = p;
		va_list va;

		va_copy(va, *vaf->va);
		kpi_vformat(o, vaf->fmt, va);
		va_end(va);
		return 1;
	}
	case 'S': case 's': case 'B': case 'F': case 'f':
		used = 1;
		if (fmt[1] == 'R' || fmt[1] == 'b')
			used = 2;
		break;
	case 'K': case 'x':
		used = 1;
		break;
	case 'O': case 'C': case 'd': case 'D': case 'g': case 'G': case 'U': case 'N':
	case 'E': case 'r': case 'R': case 'b': case 't': case 'T':
		/* Not rendered: print the raw pointer, skip the specifier letters. */
		used = 1;
		while (isalnum(fmt[used]))
			used++;
		break;
	}
	out_c(o, '0');
	out_c(o, 'x');
	out_num(o, (unsigned long)p, false, 16, false, 0, -1, false, false, 0, false);
	return used;
}

static void kpi_vformat(struct kpi_out *o, const char *fmt, va_list ap)
{
	while (*fmt) {
		bool left = false, zero = false, alt = false;
		char sign = 0;
		int width = -1, prec = -1, lng = 0;
		char c = *fmt++;

		if (c != '%') {
			out_c(o, c);
			continue;
		}
		for (;; fmt++) {
			if (*fmt == '-') left = true;
			else if (*fmt == '0') zero = true;
			else if (*fmt == '+') sign = '+';
			else if (*fmt == ' ') { if (!sign) sign = ' '; }
			else if (*fmt == '#') alt = true;
			else break;
		}
		if (*fmt == '*') {
			width = va_arg(ap, int);
			if (width < 0) { left = true; width = -width; }
			fmt++;
		} else {
			while (isdigit(*fmt))
				width = (width < 0 ? 0 : width * 10) + (*fmt++ - '0');
		}
		if (*fmt == '.') {
			fmt++;
			prec = 0;
			if (*fmt == '*') {
				prec = va_arg(ap, int);
				fmt++;
			} else {
				while (isdigit(*fmt))
					prec = prec * 10 + (*fmt++ - '0');
			}
		}
		for (;; fmt++) {
			if (*fmt == 'l') lng++;
			else if (*fmt == 'h') lng--;
			else if (*fmt == 'z' || *fmt == 't' || *fmt == 'j' || *fmt == 'L') lng = 2;
			else break;
		}
		c = *fmt++;
		switch (c) {
		case 'd': case 'i': {
			long long v = lng >= 2 ? va_arg(ap, long long) :
				      lng == 1 ? va_arg(ap, long) : va_arg(ap, int);

			if (lng == -1) v = (short)v;
			if (lng <= -2) v = (signed char)v;
			out_num(o, v < 0 ? -(unsigned long long)v : v, v < 0, 10, false,
				width, prec, left, zero, sign, false);
			break;
		}
		case 'u': case 'x': case 'X': case 'o': {
			unsigned long long v = lng >= 2 ? va_arg(ap, unsigned long long) :
					       lng == 1 ? va_arg(ap, unsigned long) :
					       va_arg(ap, unsigned int);

			if (lng == -1) v = (unsigned short)v;
			if (lng <= -2) v = (unsigned char)v;
			out_num(o, v, false, c == 'u' ? 10 : c == 'o' ? 8 : 16, c == 'X',
				width, prec, left, zero, 0, alt);
			break;
		}
		case 'c':
			out_c(o, (char)va_arg(ap, int));
			break;
		case 's':
			out_s(o, va_arg(ap, const char *), width, prec, left);
			break;
		case 'p':
			fmt += out_pointer(o, fmt, va_arg(ap, void *), width);
			break;
		case '%':
			out_c(o, '%');
			break;
		case 0:
			return;
		default:
			out_c(o, '%');
			out_c(o, c);
		}
	}
}

int vsnprintf(char *buf, size_t size, const char *fmt, va_list args)
{
	struct kpi_out o = { .buf = buf, .size = size, .len = 0 };
	va_list ap;

	va_copy(ap, args);
	kpi_vformat(&o, fmt, ap);
	va_end(ap);
	if (size)
		buf[o.len < size ? o.len : size - 1] = 0;
	return o.len;
}

int vscnprintf(char *buf, size_t size, const char *fmt, va_list args)
{
	int n = vsnprintf(buf, size, fmt, args);

	if (n < (int)size)
		return n;
	return size ? size - 1 : 0;
}

int snprintf(char *buf, size_t size, const char *fmt, ...)
{
	va_list ap;
	int n;

	va_start(ap, fmt);
	n = vsnprintf(buf, size, fmt, ap);
	va_end(ap);
	return n;
}

int scnprintf(char *buf, size_t size, const char *fmt, ...)
{
	va_list ap;
	int n;

	va_start(ap, fmt);
	n = vscnprintf(buf, size, fmt, ap);
	va_end(ap);
	return n;
}

int vsprintf(char *buf, const char *fmt, va_list args)
{
	return vsnprintf(buf, INT_MAX, fmt, args);
}

int sprintf(char *buf, const char *fmt, ...)
{
	va_list ap;
	int n;

	va_start(ap, fmt);
	n = vsnprintf(buf, INT_MAX, fmt, ap);
	va_end(ap);
	return n;
}

char *kvasprintf(gfp_t gfp, const char *fmt, va_list ap)
{
	va_list aq;
	int n;
	char *p;

	va_copy(aq, ap);
	n = vsnprintf(NULL, 0, fmt, aq);
	va_end(aq);
	p = kmalloc(n + 1, gfp);
	if (p)
		vsnprintf(p, n + 1, fmt, ap);
	return p;
}

const char *kvasprintf_const(gfp_t gfp, const char *fmt, va_list ap)
{
	return kvasprintf(gfp, fmt, ap);
}

char *kasprintf(gfp_t gfp, const char *fmt, ...)
{
	va_list ap;
	char *p;

	va_start(ap, fmt);
	p = kvasprintf(gfp, fmt, ap);
	va_end(ap);
	return p;
}

/* ---------------------------------------------------------------- printk */

static int kpi_level(const char **fmt, int dflt)
{
	int level = dflt;

	while ((*fmt)[0] == KERN_SOH_ASCII && (*fmt)[1]) {
		char c = (*fmt)[1];

		if (c >= '0' && c <= '7')
			level = c - '0';
		*fmt += 2;
	}
	return level;
}

static void kpi_emit(int level, const char *prefix, const char *fmt, va_list ap)
{
	char buf[512];
	int n = 0;

	if (prefix)
		n = scnprintf(buf, sizeof(buf), "%s", prefix);
	n += vscnprintf(buf + n, sizeof(buf) - n, fmt, ap);
	if (n > 0 && buf[n - 1] == '\n')
		n--;
	rustos_kpi_log(level, buf, n);
}

int vprintk(const char *fmt, va_list args)
{
	int level = kpi_level(&fmt, LOGLEVEL_DEFAULT);

	kpi_emit(level, NULL, fmt, args);
	return 0;
}

int vprintk_emit(int facility, int level, const struct dev_printk_info *dev_info,
		 const char *fmt, va_list args)
{
	level = kpi_level(&fmt, level < 0 ? LOGLEVEL_DEFAULT : level);
	kpi_emit(level, NULL, fmt, args);
	return 0;
}

int _printk(const char *fmt, ...)
{
	va_list ap;

	va_start(ap, fmt);
	vprintk(fmt, ap);
	va_end(ap);
	return 0;
}

/* Logging never recurses into the scheduler here: deferral is not needed. */
int _printk_deferred(const char *fmt, ...)
{
	va_list ap;

	va_start(ap, fmt);
	vprintk(fmt, ap);
	va_end(ap);
	return 0;
}

int __printk_ratelimit(const char *func)
{
	return 1;
}

int net_ratelimit(void)
{
	static u64 window, count;
	u64 now = rustos_kpi_nanos();

	if (now - window > 5000000000ULL) {
		window = now;
		count = 0;
	}
	return ++count <= 10;
}

void __warn_printk(const char *fmt, ...)
{
	va_list ap;

	va_start(ap, fmt);
	kpi_emit(LOGLEVEL_WARNING, "WARNING: ", fmt, ap);
	va_end(ap);
}

void panic(const char *fmt, ...)
{
	static char buf[256];
	va_list ap;

	va_start(ap, fmt);
	vscnprintf(buf, sizeof(buf), fmt, ap);
	va_end(ap);
	rustos_kpi_panic(buf);
}

/* ------------------------------------------------------- device messages */

/* dev_printk() and friends come from drivers/base/core.c. */

static void kpi_netdev_emit(int level, const struct net_device *dev, const char *fmt,
			    va_list ap)
{
	char prefix[96];

	if (dev && dev->dev.parent)
		scnprintf(prefix, sizeof(prefix), "%s %s %s: ",
			  dev_driver_string(dev->dev.parent), dev_name(dev->dev.parent),
			  dev->name);
	else if (dev)
		scnprintf(prefix, sizeof(prefix), "%s: ", dev->name);
	else
		scnprintf(prefix, sizeof(prefix), "(NULL net_device): ");
	kpi_emit(level, prefix, fmt, ap);
}

#define KPI_NETDEV_LEVEL(name, level)						\
void name(const struct net_device *dev, const char *fmt, ...)			\
{										\
	va_list ap;								\
	va_start(ap, fmt);							\
	kpi_netdev_emit(level, dev, fmt, ap);					\
	va_end(ap);								\
}
KPI_NETDEV_LEVEL(netdev_emerg, LOGLEVEL_EMERG)
KPI_NETDEV_LEVEL(netdev_alert, LOGLEVEL_ALERT)
KPI_NETDEV_LEVEL(netdev_crit, LOGLEVEL_CRIT)
KPI_NETDEV_LEVEL(netdev_err, LOGLEVEL_ERR)
KPI_NETDEV_LEVEL(netdev_warn, LOGLEVEL_WARNING)
KPI_NETDEV_LEVEL(netdev_notice, LOGLEVEL_NOTICE)
KPI_NETDEV_LEVEL(netdev_info, LOGLEVEL_INFO)

void netdev_printk(const char *level, const struct net_device *dev, const char *fmt, ...)
{
	va_list ap;
	int lvl = kpi_level(&level, LOGLEVEL_DEFAULT);

	va_start(ap, fmt);
	kpi_netdev_emit(lvl, dev, fmt, ap);
	va_end(ap);
}

/* --------------------------------------------- simple_strto* (vsprintf.c) */

unsigned long long simple_strtoull(const char *cp, char **endp, unsigned int base)
{
	unsigned long long result = 0;

	if (!base)
		base = (cp[0] == '0' && (cp[1] | 0x20) == 'x' && isxdigit(cp[2])) ? 16 :
		       cp[0] == '0' ? 8 : 10;
	if (base == 16 && cp[0] == '0' && (cp[1] | 0x20) == 'x')
		cp += 2;
	for (;; cp++) {
		unsigned int v = isdigit(*cp) ? *cp - '0' :
				 isxdigit(*cp) ? (*cp | 0x20) - 'a' + 10 : base;

		if (v >= base)
			break;
		result = result * base + v;
	}
	if (endp)
		*endp = (char *)cp;
	return result;
}

unsigned long simple_strtoul(const char *cp, char **endp, unsigned int base)
{
	return simple_strtoull(cp, endp, base);
}

long long simple_strtoll(const char *cp, char **endp, unsigned int base)
{
	if (*cp == '-')
		return -simple_strtoull(cp + 1, endp, base);
	return simple_strtoull(cp, endp, base);
}

long simple_strtol(const char *cp, char **endp, unsigned int base)
{
	return simple_strtoll(cp, endp, base);
}
