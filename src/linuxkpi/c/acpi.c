// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * The ACPI API Linux drivers call (ACPICA's acpi_evaluate_object(),
 * acpi_get_handle(), acpi_get_table(), and drivers/acpi's
 * acpi_evaluate_integer(), _DSM helpers, companions), on RustOS's AML
 * interpreter (src/linuxkpi/acpi.rs).
 *
 * An acpi_handle points to a struct kpi_acpi_node naming an absolute AML
 * path; nodes are interned and never freed, so equal paths give equal
 * handles. Values cross to Rust in the tagged encoding described in
 * acpi.rs. Results are built in one allocation laid out like ACPICA's
 * (objects first, then strings and buffers), so callers free them with
 * kfree()/ACPI_FREE() as usual.
 */
#include <linux/acpi.h>
#include <linux/list.h>
#include <linux/mutex.h>
#include <linux/pci.h>
#include <linux/slab.h>
#include <linux/string.h>
#include "kpi.h"

struct kpi_acpi_node {
	struct list_head list;
	char path[];
};

static LIST_HEAD(kpi_acpi_nodes);
static DEFINE_MUTEX(kpi_acpi_lock);

/*
 * Canonical form of an absolute path: name segments padded to four
 * characters as the namespace stores them ("\_SB.I2CA" -> "\_SB_.I2CA").
 */
static char *kpi_acpi_normalize(const char *path)
{
	char *out = kmalloc(strlen(path) * 2 + 2, GFP_KERNEL), *o = out;
	const char *p = path;

	if (!out)
		return NULL;
	if (*p == '\\')
		*o++ = *p++;
	while (*p) {
		size_t n = strcspn(p, ".");

		memcpy(o, p, n);
		o += n;
		for (; n && n < 4; n++)
			*o++ = '_';
		p += strcspn(p, ".");
		if (*p == '.')
			*o++ = *p++;
	}
	*o = 0;
	return out;
}

acpi_handle kpi_acpi_intern(const char *path)
{
	struct kpi_acpi_node *n;
	char *norm = kpi_acpi_normalize(path);

	if (!norm)
		return NULL;
	mutex_lock(&kpi_acpi_lock);
	list_for_each_entry(n, &kpi_acpi_nodes, list) {
		if (!strcmp(n->path, norm))
			goto out;
	}
	n = kmalloc(sizeof(*n) + strlen(norm) + 1, GFP_KERNEL);
	if (n) {
		strcpy(n->path, norm);
		list_add(&n->list, &kpi_acpi_nodes);
	}
out:
	mutex_unlock(&kpi_acpi_lock);
	kfree(norm);
	return n;
}

const char *kpi_acpi_path(acpi_handle h)
{
	if (h == ACPI_ROOT_OBJECT)
		return "\\";
	return h ? ((struct kpi_acpi_node *)h)->path : NULL;
}

/* The scope containing @path ("\\" for top-level objects). */
static char *kpi_acpi_parent_path(const char *path)
{
	const char *dot = strrchr(path, '.');

	if (!dot)
		return kstrdup("\\", GFP_KERNEL);
	return kstrndup(path, dot - path, GFP_KERNEL);
}

/*
 * Absolute path for @name relative to @h, by ACPICA's rules: "\\" roots,
 * each "^" goes up a scope, and a single name segment is searched for in
 * the scope and then each enclosing one.
 */
static char *kpi_acpi_resolve(acpi_handle h, const char *name)
{
	char *scope, *up, *path;

	if (!name || !*name)
		return h ? kstrdup(kpi_acpi_path(h), GFP_KERNEL) : NULL;
	if (name[0] == '\\')
		return kstrdup(name, GFP_KERNEL);
	if (!h)
		return NULL;
	scope = kstrdup(kpi_acpi_path(h), GFP_KERNEL);
	while (scope && *name == '^') {
		up = kpi_acpi_parent_path(scope);
		kfree(scope);
		scope = up;
		name++;
	}
	if (!scope)
		return NULL;
	for (;;) {
		bool root = !strcmp(scope, "\\");

		path = kasprintf(GFP_KERNEL, root ? "%s%s" : "%s.%s", scope, name);
		if (!path || strchr(name, '.') || root || rustos_kpi_acpi_exists(path))
			break;
		/* Single segment not in this scope: try the enclosing one. */
		kfree(path);
		up = kpi_acpi_parent_path(scope);
		kfree(scope);
		scope = up;
		if (!scope)
			return NULL;
	}
	kfree(scope);
	return path;
}

acpi_status acpi_get_parent(acpi_handle object, acpi_handle *out_handle)
{
	char *p;

	if (!object || !strcmp(kpi_acpi_path(object), "\\"))
		return AE_NULL_ENTRY;
	p = kpi_acpi_parent_path(kpi_acpi_path(object));
	if (!p)
		return AE_NO_MEMORY;
	*out_handle = kpi_acpi_intern(p);
	kfree(p);
	return *out_handle ? AE_OK : AE_NO_MEMORY;
}

/* ------------------------------------------------------------ encoding */

struct kpi_enc {
	u8 *buf;
	size_t len, cap;
};

static void kpi_enc_bytes(struct kpi_enc *e, const void *p, size_t n)
{
	if (e->len + n > e->cap) {
		size_t cap = max(e->cap * 2, e->len + n + 64);
		u8 *b = krealloc(e->buf, cap, GFP_KERNEL);

		if (!b)
			return;
		e->buf = b;
		e->cap = cap;
	}
	memcpy(e->buf + e->len, p, n);
	e->len += n;
}

static void kpi_enc_obj(struct kpi_enc *e, const union acpi_object *o)
{
	u8 tag;
	u32 n;

	switch (o->type) {
	case ACPI_TYPE_INTEGER:
		tag = 1;
		kpi_enc_bytes(e, &tag, 1);
		kpi_enc_bytes(e, &o->integer.value, 8);
		break;
	case ACPI_TYPE_STRING:
		tag = 2;
		n = o->string.length;
		kpi_enc_bytes(e, &tag, 1);
		kpi_enc_bytes(e, &n, 4);
		kpi_enc_bytes(e, o->string.pointer, n);
		break;
	case ACPI_TYPE_BUFFER:
		tag = 3;
		n = o->buffer.length;
		kpi_enc_bytes(e, &tag, 1);
		kpi_enc_bytes(e, &n, 4);
		kpi_enc_bytes(e, o->buffer.pointer, n);
		break;
	case ACPI_TYPE_PACKAGE:
		tag = 4;
		n = o->package.count;
		kpi_enc_bytes(e, &tag, 1);
		kpi_enc_bytes(e, &n, 4);
		for (u32 i = 0; i < n; i++)
			kpi_enc_obj(e, &o->package.elements[i]);
		break;
	default:
		tag = 0;
		kpi_enc_bytes(e, &tag, 1);
	}
}

/* Size of the ACPICA form of the encoded value at *p; advances *p. */
static size_t kpi_dec_size(const u8 **p, const u8 *end, size_t *objs)
{
	u8 tag;
	u32 n;
	size_t extra = 0;

	if (*p >= end)
		return 0;
	tag = *(*p)++;
	(*objs)++;
	switch (tag) {
	case 1:
		*p += 8;
		return 0;
	case 2:
	case 3:
		memcpy(&n, *p, 4);
		*p += 4 + n;
		return n + 1;
	case 4:
		memcpy(&n, *p, 4);
		*p += 4;
		for (u32 i = 0; i < n; i++)
			extra += kpi_dec_size(p, end, objs);
		return extra;
	default:
		return 0;
	}
}

static void kpi_dec_fill(const u8 **p, union acpi_object *o, union acpi_object **next_obj,
			 u8 **data)
{
	u8 tag = *(*p)++;
	u32 n;

	switch (tag) {
	case 1:
		o->type = ACPI_TYPE_INTEGER;
		memcpy(&o->integer.value, *p, 8);
		*p += 8;
		break;
	case 2:
	case 3:
		memcpy(&n, *p, 4);
		*p += 4;
		memcpy(*data, *p, n);
		(*data)[n] = 0;
		if (tag == 2) {
			o->type = ACPI_TYPE_STRING;
			o->string.length = n;
			o->string.pointer = (char *)*data;
		} else {
			o->type = ACPI_TYPE_BUFFER;
			o->buffer.length = n;
			o->buffer.pointer = *data;
		}
		*data += n + 1;
		*p += n;
		break;
	case 4:
		memcpy(&n, *p, 4);
		*p += 4;
		o->type = ACPI_TYPE_PACKAGE;
		o->package.count = n;
		o->package.elements = *next_obj;
		*next_obj += n;
		for (u32 i = 0; i < n; i++)
			kpi_dec_fill(p, &o->package.elements[i], next_obj, data);
		break;
	default:
		o->type = ACPI_TYPE_ANY;
	}
}

static void *kpi_acpi_alloc(size_t n)
{
	return kmalloc(n ? n : 1, GFP_KERNEL);
}

static acpi_status kpi_status(int err)
{
	switch (err) {
	case 0:
		return AE_OK;
	case -ENOENT:
		return AE_NOT_FOUND;
	case -ENOMEM:
		return AE_NO_MEMORY;
	case -ENODEV:
		return AE_NOT_EXIST;
	default:
		return AE_ERROR;
	}
}

/*
 * Evaluate @path. With @ret, the result is written in ACPICA form: into a
 * new allocation if ret->length is ACPI_ALLOCATE_BUFFER, otherwise into
 * the caller's buffer if it is large enough.
 */
static acpi_status kpi_acpi_eval(const char *path, struct acpi_object_list *args,
				 struct acpi_buffer *ret)
{
	struct kpi_enc e = {};
	void *out = NULL;
	size_t out_len = 0, objs = 0, size;
	const u8 *p;
	union acpi_object *obj, *next;
	u8 *dp;
	int err;

	for (u32 i = 0; args && i < args->count; i++)
		kpi_enc_obj(&e, &args->pointer[i]);
	err = rustos_kpi_acpi_eval(path, e.buf, e.len, kpi_acpi_alloc, ret ? &out : NULL,
				   &out_len);
	kfree(e.buf);
	if (err)
		return kpi_status(err);
	if (!ret)
		return AE_OK;
	p = out;
	size = kpi_dec_size(&p, p + out_len, &objs);
	size += objs * sizeof(union acpi_object);
	if (ret->length == ACPI_ALLOCATE_BUFFER || ret->length == ACPI_ALLOCATE_LOCAL_BUFFER) {
		obj = kzalloc(size, GFP_KERNEL);
		if (!obj) {
			kfree(out);
			return AE_NO_MEMORY;
		}
	} else if (ret->length < size) {
		ret->length = size;
		kfree(out);
		return AE_BUFFER_OVERFLOW;
	} else {
		obj = ret->pointer;
		memset(obj, 0, size);
	}
	p = out;
	next = obj + 1;
	dp = (u8 *)(obj + objs);
	kpi_dec_fill(&p, obj, &next, &dp);
	kfree(out);
	ret->pointer = obj;
	ret->length = size;
	return AE_OK;
}

/* ----------------------------------------------------------- ACPICA API */

acpi_status acpi_evaluate_object(acpi_handle handle, acpi_string pathname,
				 struct acpi_object_list *args, struct acpi_buffer *ret)
{
	char *path = kpi_acpi_resolve(handle, pathname);
	acpi_status st;

	if (!path)
		return AE_BAD_PARAMETER;
	st = kpi_acpi_eval(path, args, ret);
	kfree(path);
	return st;
}

acpi_status acpi_evaluate_object_typed(acpi_handle handle, acpi_string pathname,
				       struct acpi_object_list *args, struct acpi_buffer *ret,
				       acpi_object_type type)
{
	bool allocated = ret && (ret->length == ACPI_ALLOCATE_BUFFER ||
				 ret->length == ACPI_ALLOCATE_LOCAL_BUFFER);
	acpi_status st = acpi_evaluate_object(handle, pathname, args, ret);

	if (ACPI_SUCCESS(st) && ret && type != ACPI_TYPE_ANY &&
	    ((union acpi_object *)ret->pointer)->type != type) {
		if (allocated) {
			kfree(ret->pointer);
			ret->pointer = NULL;
		}
		return AE_TYPE;
	}
	return st;
}

acpi_status acpi_get_handle(acpi_handle parent, const char *pathname, acpi_handle *ret_handle)
{
	char *path = kpi_acpi_resolve(parent, pathname);

	if (!path || !ret_handle) {
		kfree(path);
		return AE_BAD_PARAMETER;
	}
	if (!rustos_kpi_acpi_exists(path)) {
		kfree(path);
		return AE_NOT_FOUND;
	}
	*ret_handle = kpi_acpi_intern(path);
	kfree(path);
	return *ret_handle ? AE_OK : AE_NO_MEMORY;
}

acpi_status acpi_get_name(acpi_handle object, u32 name_type, struct acpi_buffer *ret)
{
	const char *path = kpi_acpi_path(object);
	const char *name;
	size_t len;

	if (!path || !ret)
		return AE_BAD_PARAMETER;
	name = name_type == ACPI_SINGLE_NAME && strrchr(path, '.') ? strrchr(path, '.') + 1 : path;
	len = strlen(name) + 1;
	if (ret->length == ACPI_ALLOCATE_BUFFER || ret->length == ACPI_ALLOCATE_LOCAL_BUFFER) {
		ret->pointer = kstrdup(name, GFP_KERNEL);
		if (!ret->pointer)
			return AE_NO_MEMORY;
	} else if (ret->length < len) {
		ret->length = len;
		return AE_BUFFER_OVERFLOW;
	} else {
		memcpy(ret->pointer, name, len);
	}
	ret->length = len;
	return AE_OK;
}

acpi_status acpi_get_table(acpi_string signature, u32 instance, struct acpi_table_header **out)
{
	u64 phys, len;

	if (!signature || !out || rustos_kpi_acpi_table((const u8 *)signature, instance, &phys,
							&len))
		return AE_NOT_FOUND;
	*out = __va(phys);
	return AE_OK;
}

void acpi_put_table(struct acpi_table_header *table)
{
}

const char *acpi_format_exception(acpi_status status)
{
	switch (status) {
	case AE_OK:
		return "AE_OK";
	case AE_NOT_FOUND:
		return "AE_NOT_FOUND";
	case AE_NO_MEMORY:
		return "AE_NO_MEMORY";
	case AE_BAD_PARAMETER:
		return "AE_BAD_PARAMETER";
	case AE_TYPE:
		return "AE_TYPE";
	case AE_BUFFER_OVERFLOW:
		return "AE_BUFFER_OVERFLOW";
	case AE_NOT_EXIST:
		return "AE_NOT_EXIST";
	default:
		return "AE_ERROR";
	}
}

/* ------------------------------------------------------ drivers/acpi API */

acpi_status acpi_evaluate_integer(acpi_handle handle, acpi_string pathname,
				  struct acpi_object_list *arguments, unsigned long long *data)
{
	struct acpi_buffer buf = { ACPI_ALLOCATE_BUFFER, NULL };
	union acpi_object *o;
	acpi_status st;

	if (!data)
		return AE_BAD_PARAMETER;
	st = acpi_evaluate_object(handle, pathname, arguments, &buf);
	if (ACPI_FAILURE(st))
		return st;
	o = buf.pointer;
	if (o->type != ACPI_TYPE_INTEGER) {
		kfree(o);
		return AE_BAD_DATA;
	}
	*data = o->integer.value;
	kfree(o);
	return AE_OK;
}

bool acpi_has_method(acpi_handle handle, char *name)
{
	char *path = kpi_acpi_resolve(handle, name);
	bool r = path && rustos_kpi_acpi_exists(path);

	kfree(path);
	return r;
}

acpi_status acpi_execute_simple_method(acpi_handle handle, char *method, u64 arg)
{
	union acpi_object obj = { .type = ACPI_TYPE_INTEGER };
	struct acpi_object_list args = { 1, &obj };

	obj.integer.value = arg;
	return acpi_evaluate_object(handle, method, &args, NULL);
}

union acpi_object *acpi_evaluate_dsm(acpi_handle handle, const guid_t *guid, u64 rev, u64 func,
				     union acpi_object *argv4)
{
	union acpi_object params[4], empty = { .type = ACPI_TYPE_PACKAGE };
	struct acpi_object_list input = { 4, params };
	struct acpi_buffer buf = { ACPI_ALLOCATE_BUFFER, NULL };

	if (!handle)
		return NULL;
	params[0].type = ACPI_TYPE_BUFFER;
	params[0].buffer.length = 16;
	params[0].buffer.pointer = (u8 *)guid;
	params[1].type = ACPI_TYPE_INTEGER;
	params[1].integer.value = rev;
	params[2].type = ACPI_TYPE_INTEGER;
	params[2].integer.value = func;
	params[3] = argv4 ? *argv4 : empty;
	if (ACPI_FAILURE(acpi_evaluate_object(handle, "_DSM", &input, &buf)))
		return NULL;
	return buf.pointer;
}

bool acpi_check_dsm(acpi_handle handle, const guid_t *guid, u64 rev, u64 funcs)
{
	union acpi_object *obj;
	u64 mask = 0;

	if (!funcs)
		return false;
	obj = acpi_evaluate_dsm(handle, guid, rev, 0, NULL);
	if (!obj)
		return false;
	if (obj->type == ACPI_TYPE_INTEGER) {
		mask = obj->integer.value;
	} else if (obj->type == ACPI_TYPE_BUFFER) {
		for (u32 i = 0; i < obj->buffer.length && i < 8; i++)
			mask |= (u64)obj->buffer.pointer[i] << (i * 8);
	}
	kfree(obj);
	/* Bit 0 says whether any function is supported at all. */
	return (mask & 1) && (mask & funcs) == funcs;
}

void acpi_handle_printk(const char *level, acpi_handle handle, const char *fmt, ...)
{
	struct va_format vaf;
	va_list args;

	va_start(args, fmt);
	vaf.fmt = fmt;
	vaf.va = &args;
	printk("%sACPI: %s: %pV", level, kpi_acpi_path(handle) ?: "<n/a>", &vaf);
	va_end(args);
}

/* ------------------------------------------------------------ companions */

bool is_acpi_device_node(const struct fwnode_handle *fwnode)
{
	return !IS_ERR_OR_NULL(fwnode) && fwnode->ops == &acpi_device_fwnode_ops;
}

/* Give a PCI device its ACPI companion (the device object with its _ADR). */
void kpi_acpi_pci_companion(struct pci_dev *pdev)
{
	char path[256];
	struct acpi_device *adev;

	if (rustos_kpi_acpi_pci_path(pdev->bus->number, PCI_SLOT(pdev->devfn),
				     PCI_FUNC(pdev->devfn), path, sizeof(path)))
		return;
	adev = kpi_acpi_device_at(path);
	if (adev)
		ACPI_COMPANION_SET(&pdev->dev, adev);
}

/* ACPI tables are not reloaded at run time (no SSDT overlays). */
int acpi_reconfig_notifier_register(struct notifier_block *nb)
{
	return 0;
}

int acpi_reconfig_notifier_unregister(struct notifier_block *nb)
{
	return 0;
}

void acpi_device_fix_up_power_extended(struct acpi_device *adev)
{
}

int acpi_device_fix_up_power(struct acpi_device *device)
{
	return 0;
}

int acpi_device_set_power(struct acpi_device *device, int state)
{
	return 0;
}
