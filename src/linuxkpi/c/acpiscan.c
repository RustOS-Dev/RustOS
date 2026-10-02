// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * ACPI device objects and resources, in place of drivers/acpi/scan.c and
 * ACPICA's resource manager.
 *
 * Every Device in the namespace becomes a struct acpi_device with its
 * _HID/_CID ids, _UID and _STA (src/linuxkpi/acpi.rs walks the
 * namespace). Devices with a _HID become platform devices (as Linux's
 * default enumeration does, through the imported acpi_platform.c),
 * except I2C/SPI/UART slaves: their bus controller's driver enumerates
 * them (i2c-core-acpi). acpi_walk_resources() turns the raw _CRS buffers
 * into ACPICA's struct acpi_resource for drivers/acpi/resource.c,
 * i2c-core-acpi and gpiolib-acpi.
 *
 * Device objects are created once and never removed (no hotplug or
 * table loading at run time).
 */
#include <linux/acpi.h>
#include <linux/device.h>
#include <linux/list.h>
#include <linux/mutex.h>
#include <linux/platform_device.h>
#include <linux/slab.h>
#include <linux/string.h>
#include <linux/unaligned.h>
#include "kpi.h"

static LIST_HEAD(kpi_adevs);	/* linked by acpi_device.del_list, in namespace order */
static DEFINE_MUTEX(kpi_scan_lock);
static bool kpi_scanned;

#define for_each_kpi_adev(adev) list_for_each_entry(adev, &kpi_adevs, del_list)

/* ------------------------------------------------------------ resources */

static u16 kpi_u16(const u8 *p)
{
	return get_unaligned_le16(p);
}

static u32 kpi_u32(const u8 *p)
{
	return get_unaligned_le32(p);
}

/* Copy a resource source (index byte, then a name) into the extra area. */
static void kpi_res_source(struct acpi_resource_source *rs, const u8 *idx, const u8 *str,
			   const u8 *end, char **extra)
{
	size_t n;

	memset(rs, 0, sizeof(*rs));
	if (!str || str >= end)
		return;
	rs->index = idx ? *idx : 0;
	n = strnlen((const char *)str, end - str);
	memcpy(*extra, str, n);
	(*extra)[n] = 0;
	rs->string_ptr = *extra;
	rs->string_length = n + 1;
	*extra += n + 1;
}

static void kpi_res_address(struct acpi_resource *r, const u8 *d, size_t dlen, int width,
			    char **extra)
{
	struct acpi_resource_address64 *a = &r->data.address64;
	u8 rtype = d[3], gflags = d[4], tflags = d[5];
	u64 v[5];
	int i;

	for (i = 0; i < 5; i++) {
		const u8 *p = d + 6 + i * width;

		v[i] = width == 2 ? kpi_u16(p) : width == 4 ? kpi_u32(p) : get_unaligned_le64(p);
	}
	/* The common fields have the same layout in all three structs. */
	a->resource_type = rtype;
	a->producer_consumer = gflags & 1 ? ACPI_CONSUMER : ACPI_PRODUCER;
	a->decode = gflags & 2 ? ACPI_SUB_DECODE : ACPI_POS_DECODE;
	a->min_address_fixed = !!(gflags & 4);
	a->max_address_fixed = !!(gflags & 8);
	if (rtype == ACPI_MEMORY_RANGE) {
		a->info.mem.write_protect = tflags & 1;
		a->info.mem.caching = (tflags >> 1) & 3;
		a->info.mem.range_type = (tflags >> 3) & 3;
		a->info.mem.translation = (tflags >> 5) & 1;
	} else if (rtype == ACPI_IO_RANGE) {
		a->info.io.range_type = tflags & 3;
		a->info.io.translation = (tflags >> 4) & 1;
		a->info.io.translation_type = (tflags >> 5) & 1;
	} else {
		a->info.type_specific = tflags;
	}
	switch (width) {
	case 2: {
		struct acpi_resource_address16 *a16 = &r->data.address16;

		r->type = ACPI_RESOURCE_TYPE_ADDRESS16;
		a16->address.granularity = v[0];
		a16->address.minimum = v[1];
		a16->address.maximum = v[2];
		a16->address.translation_offset = v[3];
		a16->address.address_length = v[4];
		kpi_res_source(&a16->resource_source, d + 16, d + 17, d + dlen, extra);
		break;
	}
	case 4: {
		struct acpi_resource_address32 *a32 = &r->data.address32;

		r->type = ACPI_RESOURCE_TYPE_ADDRESS32;
		a32->address.granularity = v[0];
		a32->address.minimum = v[1];
		a32->address.maximum = v[2];
		a32->address.translation_offset = v[3];
		a32->address.address_length = v[4];
		kpi_res_source(&a32->resource_source, d + 26, d + 27, d + dlen, extra);
		break;
	}
	default:
		r->type = ACPI_RESOURCE_TYPE_ADDRESS64;
		a->address.granularity = v[0];
		a->address.minimum = v[1];
		a->address.maximum = v[2];
		a->address.translation_offset = v[3];
		a->address.address_length = v[4];
		kpi_res_source(&a->resource_source, d + 46, d + 47, d + dlen, extra);
		break;
	}
}

static void kpi_res_gpio(struct acpi_resource *r, const u8 *d, size_t dlen, char **extra)
{
	struct acpi_resource_gpio *g = &r->data.gpio;
	u16 iflags = kpi_u16(d + 7), pin_off = kpi_u16(d + 14), rs_off = kpi_u16(d + 17);
	u16 vend_off = kpi_u16(d + 19), vend_len = kpi_u16(d + 21);
	int i;

	r->type = ACPI_RESOURCE_TYPE_GPIO;
	g->revision_id = d[3];
	g->connection_type = d[4];
	g->producer_consumer = kpi_u16(d + 5) & 1 ? ACPI_CONSUMER : ACPI_PRODUCER;
	if (g->connection_type == ACPI_RESOURCE_GPIO_TYPE_INT) {
		g->triggering = iflags & 1 ? ACPI_EDGE_SENSITIVE : ACPI_LEVEL_SENSITIVE;
		g->polarity = (iflags >> 1) & 3;
		g->wake_capable = (iflags >> 4) & 1;
	} else {
		g->io_restriction = iflags & 3;
	}
	g->shareable = (iflags >> 3) & 1;
	g->pin_config = d[9];
	g->drive_strength = kpi_u16(d + 10);
	g->debounce_timeout = kpi_u16(d + 12);
	g->pin_table_length = rs_off > pin_off && rs_off <= dlen ? (rs_off - pin_off) / 2 : 0;
	g->pin_table = (u16 *)PTR_ALIGN(*extra, 2);
	for (i = 0; i < g->pin_table_length; i++)
		g->pin_table[i] = kpi_u16(d + pin_off + 2 * i);
	*extra = (char *)(g->pin_table + g->pin_table_length);
	kpi_res_source(&g->resource_source, d + 16, d + rs_off,
		       d + (vend_off > rs_off && vend_off <= dlen ? vend_off : dlen), extra);
	if (vend_len && vend_off + vend_len <= dlen) {
		g->vendor_length = vend_len;
		g->vendor_data = (u8 *)*extra;
		memcpy(*extra, d + vend_off, vend_len);
		*extra += vend_len;
	}
}

static void kpi_res_serial(struct acpi_resource *r, const u8 *d, size_t dlen, char **extra)
{
	struct acpi_resource_common_serialbus *sb = &r->data.common_serial_bus;
	u16 tflags = kpi_u16(d + 7), tdl = kpi_u16(d + 10);
	unsigned int min = 0;

	r->type = ACPI_RESOURCE_TYPE_SERIAL_BUS;
	sb->revision_id = d[3];
	sb->type = d[5];
	sb->slave_mode = d[6] & 1;
	sb->producer_consumer = d[6] & 2 ? ACPI_CONSUMER : ACPI_PRODUCER;
	sb->connection_sharing = (d[6] >> 2) & 1;
	sb->type_revision_id = d[9];
	sb->type_data_length = tdl;
	switch (sb->type) {
	case ACPI_RESOURCE_SERIAL_TYPE_I2C: {
		struct acpi_resource_i2c_serialbus *i2c = &r->data.i2c_serial_bus;

		i2c->access_mode = tflags & 1;
		i2c->connection_speed = kpi_u32(d + 12);
		i2c->slave_address = kpi_u16(d + 16);
		min = 6;
		break;
	}
	case ACPI_RESOURCE_SERIAL_TYPE_SPI: {
		struct acpi_resource_spi_serialbus *spi = &r->data.spi_serial_bus;

		spi->wire_mode = tflags & 1;
		spi->device_polarity = (tflags >> 1) & 1;
		spi->connection_speed = kpi_u32(d + 12);
		spi->data_bit_length = d[16];
		spi->clock_phase = d[17];
		spi->clock_polarity = d[18];
		spi->device_selection = kpi_u16(d + 19);
		min = 9;
		break;
	}
	case ACPI_RESOURCE_SERIAL_TYPE_UART: {
		struct acpi_resource_uart_serialbus *uart = &r->data.uart_serial_bus;

		uart->flow_control = tflags & 3;
		uart->stop_bits = (tflags >> 2) & 3;
		uart->data_bits = (tflags >> 4) & 7;
		uart->endian = (tflags >> 7) & 1;
		uart->default_baud_rate = kpi_u32(d + 12);
		uart->rx_fifo_size = kpi_u16(d + 16);
		uart->tx_fifo_size = kpi_u16(d + 18);
		uart->parity = d[20];
		uart->lines_enabled = d[21];
		min = 10;
		break;
	}
	}
	if (tdl > min && 12 + tdl <= dlen) {
		sb->vendor_length = tdl - min;
		sb->vendor_data = (u8 *)*extra;
		memcpy(*extra, d + 12 + min, sb->vendor_length);
		*extra += sb->vendor_length;
	}
	kpi_res_source(&sb->resource_source, d + 4, d + 12 + tdl, d + dlen, extra);
}

/*
 * Convert the descriptor at @d (@dlen bytes) into @r, whose trailing
 * space (at least 2 * @dlen + 64 bytes) holds tables and strings.
 * Returns false for descriptors drivers do not use (skipped).
 */
static bool kpi_res_convert(const u8 *d, size_t dlen, struct acpi_resource *r)
{
	char *extra = (char *)(r + 1) + 64;	/* past flexible interrupt arrays */
	int i;

	memset(r, 0, sizeof(*r));
	r->length = sizeof(*r);
	if (!(d[0] & 0x80)) {
		unsigned int len = d[0] & 7;

		switch ((d[0] >> 3) & 0xf) {
		case 0x04: {	/* IRQ */
			struct acpi_resource_irq *irq = &r->data.irq;
			u16 mask = kpi_u16(d + 1);
			u8 flags = len >= 3 ? d[3] : 0x01;

			r->type = ACPI_RESOURCE_TYPE_IRQ;
			irq->descriptor_length = len;
			irq->triggering = flags & 1 ? ACPI_EDGE_SENSITIVE : ACPI_LEVEL_SENSITIVE;
			irq->polarity = (flags >> 3) & 1;
			irq->shareable = (flags >> 4) & 1;
			irq->wake_capable = (flags >> 5) & 1;
			for (i = 0; i < 16; i++)
				if (mask & BIT(i))
					irq->interrupts[irq->interrupt_count++] = i;
			return true;
		}
		case 0x05: {	/* DMA */
			struct acpi_resource_dma *dma = &r->data.dma;

			r->type = ACPI_RESOURCE_TYPE_DMA;
			dma->transfer = d[2] & 3;
			dma->bus_master = (d[2] >> 2) & 1;
			dma->type = (d[2] >> 5) & 3;
			for (i = 0; i < 8; i++)
				if (d[1] & BIT(i))
					dma->channels[dma->channel_count++] = i;
			return true;
		}
		case 0x06:
			r->type = ACPI_RESOURCE_TYPE_START_DEPENDENT;
			return true;
		case 0x07:
			r->type = ACPI_RESOURCE_TYPE_END_DEPENDENT;
			return true;
		case 0x08: {	/* I/O port */
			struct acpi_resource_io *io = &r->data.io;

			r->type = ACPI_RESOURCE_TYPE_IO;
			io->io_decode = d[1] & 1;
			io->minimum = kpi_u16(d + 2);
			io->maximum = kpi_u16(d + 4);
			io->alignment = d[6];
			io->address_length = d[7];
			return true;
		}
		case 0x09:	/* fixed I/O port */
			r->type = ACPI_RESOURCE_TYPE_FIXED_IO;
			r->data.fixed_io.address = kpi_u16(d + 1) & 0x3ff;
			r->data.fixed_io.address_length = d[3];
			return true;
		case 0x0a:	/* fixed DMA */
			r->type = ACPI_RESOURCE_TYPE_FIXED_DMA;
			r->data.fixed_dma.request_lines = kpi_u16(d + 1);
			r->data.fixed_dma.channels = kpi_u16(d + 3);
			r->data.fixed_dma.width = d[5];
			return true;
		case 0x0f:
			r->type = ACPI_RESOURCE_TYPE_END_TAG;
			return true;
		}
		return false;
	}

	switch (d[0] & 0x7f) {
	case 0x01: {	/* 24-bit memory */
		struct acpi_resource_memory24 *m = &r->data.memory24;

		r->type = ACPI_RESOURCE_TYPE_MEMORY24;
		m->write_protect = d[3] & 1;
		m->minimum = kpi_u16(d + 4);
		m->maximum = kpi_u16(d + 6);
		m->alignment = kpi_u16(d + 8);
		m->address_length = kpi_u16(d + 10);
		return true;
	}
	case 0x05: {	/* 32-bit memory */
		struct acpi_resource_memory32 *m = &r->data.memory32;

		r->type = ACPI_RESOURCE_TYPE_MEMORY32;
		m->write_protect = d[3] & 1;
		m->minimum = kpi_u32(d + 4);
		m->maximum = kpi_u32(d + 8);
		m->alignment = kpi_u32(d + 12);
		m->address_length = kpi_u32(d + 16);
		return true;
	}
	case 0x06: {	/* 32-bit fixed memory */
		struct acpi_resource_fixed_memory32 *m = &r->data.fixed_memory32;

		r->type = ACPI_RESOURCE_TYPE_FIXED_MEMORY32;
		m->write_protect = d[3] & 1;
		m->address = kpi_u32(d + 4);
		m->address_length = kpi_u32(d + 8);
		return true;
	}
	case 0x07:	/* DWord address space */
		if (dlen < 26)
			return false;
		kpi_res_address(r, d, dlen, 4, &extra);
		return true;
	case 0x08:	/* Word address space */
		if (dlen < 16)
			return false;
		kpi_res_address(r, d, dlen, 2, &extra);
		return true;
	case 0x0a:	/* QWord address space */
		if (dlen < 46)
			return false;
		kpi_res_address(r, d, dlen, 8, &extra);
		return true;
	case 0x09: {	/* extended interrupt */
		struct acpi_resource_extended_irq *x = &r->data.extended_irq;
		u8 flags = d[3];

		r->type = ACPI_RESOURCE_TYPE_EXTENDED_IRQ;
		x->producer_consumer = flags & 1 ? ACPI_CONSUMER : ACPI_PRODUCER;
		x->triggering = flags & 2 ? ACPI_EDGE_SENSITIVE : ACPI_LEVEL_SENSITIVE;
		x->polarity = (flags >> 2) & 1;
		x->shareable = (flags >> 3) & 1;
		x->wake_capable = (flags >> 4) & 1;
		x->interrupt_count = min_t(size_t, d[4], (dlen - 5) / 4);
		for (i = 0; i < x->interrupt_count; i++)
			x->interrupts[i] = kpi_u32(d + 5 + 4 * i);
		extra = (char *)(x->interrupts + x->interrupt_count) + 8;
		kpi_res_source(&x->resource_source, d + 5 + 4 * x->interrupt_count,
			       d + 6 + 4 * x->interrupt_count, d + dlen, &extra);
		return true;
	}
	case 0x0c:	/* GPIO connection */
		if (dlen < 23)
			return false;
		kpi_res_gpio(r, d, dlen, &extra);
		return true;
	case 0x0e:	/* serial bus connection */
		if (dlen < 12)
			return false;
		kpi_res_serial(r, d, dlen, &extra);
		return true;
	}
	return false;
}

acpi_status acpi_walk_resource_buffer(struct acpi_buffer *buffer,
				      acpi_walk_resource_callback cb, void *ctx)
{
	const u8 *p = buffer->pointer, *end = p + buffer->length;
	acpi_status status = AE_OK;

	while (p < end) {
		size_t dlen = p[0] & 0x80 ? (p + 3 <= end ? 3 + kpi_u16(p + 1) : 0) : 1 + (p[0] & 7);
		struct acpi_resource *r;
		bool end_tag;

		if (!dlen || p + dlen > end)
			return AE_AML_BAD_RESOURCE_LENGTH;
		r = kzalloc(sizeof(*r) + 2 * dlen + 128, GFP_KERNEL);
		if (!r)
			return AE_NO_MEMORY;
		if (!kpi_res_convert(p, dlen, r)) {
			kfree(r);
			p += dlen;
			continue;
		}
		end_tag = r->type == ACPI_RESOURCE_TYPE_END_TAG;
		status = cb(r, ctx);
		kfree(r);
		if (ACPI_FAILURE(status)) {
			if (status == AE_CTRL_TERMINATE)
				status = AE_OK;
			break;
		}
		if (end_tag)
			break;
		p += dlen;
	}
	return status;
}

acpi_status acpi_walk_resources(acpi_handle handle, char *name,
				acpi_walk_resource_callback cb, void *ctx)
{
	struct acpi_buffer buf = { ACPI_ALLOCATE_BUFFER, NULL };
	union acpi_object *obj;
	struct acpi_buffer raw;
	acpi_status status;

	status = acpi_evaluate_object(handle, name, NULL, &buf);
	if (ACPI_FAILURE(status))
		return status;
	obj = buf.pointer;
	if (obj->type != ACPI_TYPE_BUFFER) {
		kfree(obj);
		return AE_TYPE;
	}
	raw.length = obj->buffer.length;
	raw.pointer = obj->buffer.pointer;
	status = acpi_walk_resource_buffer(&raw, cb, ctx);
	kfree(obj);
	return status;
}

acpi_status acpi_resource_to_address64(struct acpi_resource *r,
				       struct acpi_resource_address64 *out)
{
	switch (r->type) {
	case ACPI_RESOURCE_TYPE_ADDRESS16: {
		struct acpi_resource_address16 *a = &r->data.address16;

		memcpy(out, a, offsetof(struct acpi_resource_address16, address));
		out->address.granularity = a->address.granularity;
		out->address.minimum = a->address.minimum;
		out->address.maximum = a->address.maximum;
		out->address.translation_offset = a->address.translation_offset;
		out->address.address_length = a->address.address_length;
		out->resource_source = a->resource_source;
		return AE_OK;
	}
	case ACPI_RESOURCE_TYPE_ADDRESS32: {
		struct acpi_resource_address32 *a = &r->data.address32;

		memcpy(out, a, offsetof(struct acpi_resource_address32, address));
		out->address.granularity = a->address.granularity;
		out->address.minimum = a->address.minimum;
		out->address.maximum = a->address.maximum;
		out->address.translation_offset = a->address.translation_offset;
		out->address.address_length = a->address.address_length;
		out->resource_source = a->resource_source;
		return AE_OK;
	}
	case ACPI_RESOURCE_TYPE_ADDRESS64:
		*out = r->data.address64;
		return AE_OK;
	}
	return AE_BAD_PARAMETER;
}

/* --------------------------------------------------------- device objects */

static void kpi_adev_release(struct device *dev)
{
}

static void kpi_add_id(struct acpi_device *adev, const char *id, size_t n)
{
	struct acpi_hardware_id *hwid = kzalloc(sizeof(*hwid), GFP_KERNEL);

	if (!hwid)
		return;
	hwid->id = kstrndup(id, n, GFP_KERNEL);
	if (!hwid->id) {
		kfree(hwid);
		return;
	}
	list_add_tail(&hwid->list, &adev->pnp.ids);
	adev->pnp.type.hardware_id = 1;
}

static int kpi_instance_no(const char *hid)
{
	struct acpi_device *adev;
	int n = 0;

	for_each_kpi_adev(adev)
		if (!strcmp(acpi_device_hid(adev), hid))
			n++;
	return n;
}

static void kpi_scan_one(void *ctx, const char *path, const char *hid, const char *cids,
			 const char *uid, u32 sta, u64 adr, int has_adr)
{
	struct acpi_device *adev = kzalloc(sizeof(*adev), GFP_KERNEL);
	const char *seg;
	int i;

	if (!adev)
		return;
	adev->handle = kpi_acpi_intern(path);
	fwnode_init(&adev->fwnode, &acpi_device_fwnode_ops);
	INIT_LIST_HEAD(&adev->pnp.ids);
	INIT_LIST_HEAD(&adev->wakeup_list);
	INIT_LIST_HEAD(&adev->physical_node_list);
	mutex_init(&adev->physical_node_lock);
	acpi_set_device_status(adev, sta);
	if (*hid) {
		kpi_add_id(adev, hid, strlen(hid));
		adev->pnp.type.platform_id = 1;
	}
	while (*cids) {
		size_t n = strcspn(cids, ",");

		if (n)
			kpi_add_id(adev, cids, n);
		cids += n + (cids[n] == ',');
	}
	if (has_adr) {
		adev->pnp.bus_address = adr;
		adev->pnp.type.bus_address = 1;
	}
	if (*uid)
		adev->pnp.unique_id = kstrdup(uid, GFP_KERNEL);
	seg = strrchr(path, '.');
	seg = seg ? seg + 1 : path + (*path == '\\');
	strscpy(adev->pnp.bus_id, seg, sizeof(adev->pnp.bus_id));
	for (i = 3; i > 1; i--) {
		if (adev->pnp.bus_id[i] != '_')
			break;
		adev->pnp.bus_id[i] = 0;
	}
	adev->flags.initialized = 1;
	adev->flags.match_driver = 1;
	adev->pnp.instance_no = kpi_instance_no(acpi_device_hid(adev));
	device_initialize(&adev->dev);
	adev->dev.release = kpi_adev_release;
	dev_set_name(&adev->dev, "%s:%02x", acpi_device_hid(adev), adev->pnp.instance_no);
	list_add_tail(&adev->del_list, &kpi_adevs);
}

struct acpi_device *acpi_fetch_acpi_dev(acpi_handle handle)
{
	struct acpi_device *adev;

	if (!handle)
		return NULL;
	if (handle == ACPI_ROOT_OBJECT)
		handle = kpi_acpi_intern("\\");
	kpi_acpi_scan_devices();
	for_each_kpi_adev(adev)
		if (adev->handle == handle)
			return adev;
	return NULL;
}

struct acpi_device *acpi_get_acpi_dev(acpi_handle handle)
{
	struct acpi_device *adev = acpi_fetch_acpi_dev(handle);

	if (adev)
		get_device(&adev->dev);
	return adev;
}

/* The nearest enclosing Device of @adev's namespace node. */
static struct acpi_device *kpi_find_parent(struct acpi_device *adev)
{
	acpi_handle h = adev->handle, up;
	struct acpi_device *p;

	while (ACPI_SUCCESS(acpi_get_parent(h, &up))) {
		for_each_kpi_adev(p)
			if (p->handle == up)
				return p;
		h = up;
	}
	return NULL;
}

/* Serial bus slaves that are not enumerated by their controller. */
static const struct acpi_device_id kpi_ignore_serial_bus_ids[] = {
	{"AKM9911", 0}, {"AKM9915", 0}, {"BCM4752", 0}, {"BCM4752E", 0},
	{"BCM47531", 0}, {"BSG1160", 0}, {"BSG2150", 0}, {"CSC3551", 0},
	{"CSC3554", 0}, {"CSC3556", 0}, {"CSC3557", 0}, {"INT33FE", 0},
	{"INT3515", 0}, {"TXNW2781", 0}, {"MSHW0028", 0}, {"BCM2E8E", 0},
	{"BCM2E3A", 0}, {"BCM2E7C", 0}, {"BCM2E7E", 0}, {"BCM2E95", 0},
	{"BCM2EA1", 0}, {"BCM2EB1", 0}, {"BCM2EB5", 0}, {"INT33A1", 0},
	{ }
};

static acpi_status kpi_check_serial_slave(struct acpi_resource *r, void *ctx)
{
	bool *slave = ctx;

	if (r->type == ACPI_RESOURCE_TYPE_SERIAL_BUS &&
	    r->data.common_serial_bus.type >= ACPI_RESOURCE_SERIAL_TYPE_I2C &&
	    r->data.common_serial_bus.type <= ACPI_RESOURCE_SERIAL_TYPE_UART) {
		*slave = true;
		return AE_CTRL_TERMINATE;
	}
	return AE_OK;
}

/* Create the device objects (once; also called from PCI companion lookup). */
void kpi_acpi_scan_devices(void)
{
	struct acpi_device *adev;

	mutex_lock(&kpi_scan_lock);
	if (kpi_scanned) {
		mutex_unlock(&kpi_scan_lock);
		return;
	}
	rustos_kpi_acpi_for_each_device(kpi_scan_one, NULL);
	for_each_kpi_adev(adev) {
		struct acpi_device *parent = kpi_find_parent(adev);
		bool slave = false;

		if (parent)
			adev->dev.parent = &parent->dev;
		if (adev->pnp.type.platform_id &&
		    acpi_match_device_ids(adev, kpi_ignore_serial_bus_ids)) {
			acpi_walk_resources(adev->handle, METHOD_NAME__CRS, kpi_check_serial_slave,
					    &slave);
			adev->flags.enumeration_by_parent = slave;
		}
	}
	kpi_scanned = true;
	mutex_unlock(&kpi_scan_lock);
}

/* ACPI ids that are not platform devices (buses, processors, ACPI-only). */
static const struct acpi_device_id kpi_not_platform_ids[] = {
	{"PNP0A03", 0}, {"PNP0A08", 0}, {"ACPI0007", 0}, {"ACPI0010", 0},
	{"LNXSYSTM", 0}, {"LNXSYBUS", 0}, {"LNXCPU", 0}, {"PNP0C0F", 0},
	{"ACPI0004", 0}, {"PNP0A05", 0}, {"PNP0A06", 0},
	{ }
};

/*
 * Platform devices for ACPI devices with a _HID (Linux's default
 * enumeration). Runs after the device core and platform bus are up;
 * drivers registering later bind as they come.
 */
static int __init kpi_acpi_platform_scan(void)
{
	struct acpi_device *adev;
	int n = 0;

	kpi_acpi_scan_devices();
	for_each_kpi_adev(adev) {
		struct platform_device *pdev;

		if (!adev->pnp.type.platform_id || adev->flags.enumeration_by_parent ||
		    adev->pnp.type.bus_address || !acpi_device_is_present(adev) ||
		    !adev->status.enabled ||
		    !acpi_match_device_ids(adev, kpi_not_platform_ids))
			continue;
		pdev = acpi_create_platform_device(adev, NULL);
		if (!IS_ERR_OR_NULL(pdev))
			n++;
		acpi_device_set_enumerated(adev);
	}
	pr_info("ACPI: %d platform devices\n", n);
	return 0;
}
subsys_initcall(kpi_acpi_platform_scan);

bool acpi_device_is_present(const struct acpi_device *adev)
{
	return adev->status.present || adev->status.functional;
}

bool acpi_dev_ready_for_enumeration(const struct acpi_device *device)
{
	if (device->flags.honor_deps && device->dep_unmet)
		return false;
	return acpi_device_is_present(device);
}

int acpi_bus_get_status(struct acpi_device *adev)
{
	unsigned long long sta;

	if (ACPI_SUCCESS(acpi_evaluate_integer(adev->handle, "_STA", NULL, &sta)))
		acpi_set_device_status(adev, sta);
	return 0;
}

const char *acpi_device_hid(struct acpi_device *device)
{
	struct acpi_hardware_id *hid;

	if (!device || list_empty(&device->pnp.ids))
		return "device";
	hid = list_first_entry(&device->pnp.ids, struct acpi_hardware_id, list);
	return hid->id;
}

/* ----------------------------------------------------------- namespace */

static int kpi_depth(const char *path)
{
	int d = 0;

	if (!strcmp(path, "\\"))
		return 0;
	for (d = 1; *path; path++)
		d += *path == '.';
	return d;
}

static bool kpi_under(const char *path, const char *scope)
{
	size_t n = strlen(scope);

	if (!strcmp(scope, "\\"))
		return strcmp(path, "\\");
	return !strncmp(path, scope, n) && path[n] == '.';
}

/*
 * Devices under @start (other object types are not walked: drivers walk
 * for devices), depth-first in namespace order, descending callbacks
 * only. AE_CTRL_DEPTH skips a device's children.
 */
acpi_status acpi_walk_namespace(acpi_object_type type, acpi_handle start, u32 max_depth,
				acpi_walk_callback desc_cb, acpi_walk_callback asc_cb,
				void *ctx, void **ret)
{
	const char *scope = kpi_acpi_path(start ?: ACPI_ROOT_OBJECT), *skip = NULL;
	int base = kpi_depth(scope);
	struct acpi_device *adev;

	if (type != ACPI_TYPE_DEVICE && type != ACPI_TYPE_ANY)
		return AE_OK;
	kpi_acpi_scan_devices();
	for_each_kpi_adev(adev) {
		const char *path = kpi_acpi_path(adev->handle);
		int depth = kpi_depth(path) - base;
		acpi_status s;

		if (!kpi_under(path, scope) || depth > max_depth)
			continue;
		if (skip && kpi_under(path, skip))
			continue;
		skip = NULL;
		if (!desc_cb)
			continue;
		s = desc_cb(adev->handle, depth, ctx, ret);
		if (s == AE_CTRL_TERMINATE)
			return AE_OK;
		if (s == AE_CTRL_DEPTH)
			skip = path;
		else if (ACPI_FAILURE(s))
			return s;
	}
	return AE_OK;
}

acpi_status acpi_get_devices(const char *hid, acpi_walk_callback cb, void *ctx, void **ret)
{
	struct acpi_device *adev;

	kpi_acpi_scan_devices();
	for_each_kpi_adev(adev) {
		struct acpi_hardware_id *id;
		acpi_status s;

		if (hid) {
			bool found = false;

			list_for_each_entry(id, &adev->pnp.ids, list)
				found |= !strcmp(id->id, hid);
			if (!found)
				continue;
		}
		if (!acpi_device_is_present(adev))
			continue;
		s = cb(adev->handle, 1, ctx, ret);
		if (s == AE_CTRL_TERMINATE)
			break;
		if (ACPI_FAILURE(s) && s != AE_CTRL_DEPTH)
			return s;
	}
	return AE_OK;
}

acpi_status acpi_get_type(acpi_handle handle, acpi_object_type *ret_type)
{
	*ret_type = acpi_fetch_acpi_dev(handle) ? ACPI_TYPE_DEVICE : ACPI_TYPE_ANY;
	return AE_OK;
}

/* ------------------------------------------------------------- matching */

static const struct acpi_device_id *kpi_match_ids(struct acpi_device *adev,
						  const struct acpi_device_id *ids)
{
	const struct acpi_device_id *id;
	struct acpi_hardware_id *hwid;

	if (!adev || !ids || !adev->status.present)
		return NULL;
	for (id = ids; id->id[0] || id->cls; id++) {
		if (!id->id[0])
			continue;
		list_for_each_entry(hwid, &adev->pnp.ids, list)
			if (!strcmp((const char *)id->id, hwid->id))
				return id;
	}
	return NULL;
}

int acpi_match_device_ids(struct acpi_device *device, const struct acpi_device_id *ids)
{
	return kpi_match_ids(device, ids) ? 0 : -ENOENT;
}

const struct acpi_device_id *acpi_match_acpi_device(const struct acpi_device_id *ids,
						    const struct acpi_device *adev)
{
	return kpi_match_ids((struct acpi_device *)adev, ids);
}

const struct acpi_device_id *acpi_match_device(const struct acpi_device_id *ids,
					       const struct device *dev)
{
	return kpi_match_ids(ACPI_COMPANION(dev), ids);
}

const void *acpi_device_get_match_data(const struct device *dev)
{
	const struct acpi_device_id *match;

	if (!dev->driver)
		return NULL;
	match = acpi_match_device(dev->driver->acpi_match_table, dev);
	return match ? (const void *)match->driver_data : NULL;
}

bool acpi_driver_match_device(struct device *dev, const struct device_driver *drv)
{
	return acpi_match_device(drv->acpi_match_table, dev) != NULL;
}

static struct acpi_device *kpi_first_match(const char *hid, const char *uid, s64 hrv,
					   struct acpi_device *from)
{
	struct acpi_device *adev;
	bool started = !from;

	kpi_acpi_scan_devices();
	for_each_kpi_adev(adev) {
		struct acpi_hardware_id *id;
		bool found = false;

		if (!started) {
			started = adev == from;
			continue;
		}
		list_for_each_entry(id, &adev->pnp.ids, list)
			found |= !strcmp(id->id, hid);
		if (!found)
			continue;
		if (uid && (!acpi_device_uid(adev) || strcmp(acpi_device_uid(adev), uid)))
			continue;
		if (!acpi_device_is_present(adev))
			continue;
		return adev;
	}
	return NULL;
}

struct acpi_device *acpi_dev_get_next_match_dev(struct acpi_device *adev, const char *hid,
						const char *uid, s64 hrv)
{
	struct acpi_device *next = kpi_first_match(hid, uid, hrv, adev);

	if (next)
		get_device(&next->dev);
	if (adev)
		put_device(&adev->dev);
	return next;
}

struct acpi_device *acpi_dev_get_first_match_dev(const char *hid, const char *uid, s64 hrv)
{
	return acpi_dev_get_next_match_dev(NULL, hid, uid, hrv);
}

bool acpi_dev_present(const char *hid, const char *uid, s64 hrv)
{
	return kpi_first_match(hid, uid, hrv, NULL) != NULL;
}

struct acpi_device *acpi_find_child_device(struct acpi_device *parent, u64 address,
					   bool check_children)
{
	struct acpi_device *adev;

	if (!parent)
		return NULL;
	for_each_kpi_adev(adev)
		if (adev->dev.parent == &parent->dev && adev->pnp.type.bus_address &&
		    adev->pnp.bus_address == address)
			return adev;
	return NULL;
}

/* PCI functions' companions: the device object with their _ADR. */
struct acpi_device *kpi_acpi_device_at(const char *path)
{
	return acpi_fetch_acpi_dev(kpi_acpi_intern(path));
}

/* ------------------------------------------------------- physical nodes */

void acpi_device_notify(struct device *dev)
{
	struct acpi_device *adev = ACPI_COMPANION(dev);
	struct acpi_device_physical_node *pn;

	if (!adev)
		return;
	pn = kzalloc(sizeof(*pn), GFP_KERNEL);
	if (!pn)
		return;
	pn->dev = get_device(dev);
	mutex_lock(&adev->physical_node_lock);
	pn->node_id = adev->physical_node_count++;
	list_add_tail(&pn->node, &adev->physical_node_list);
	mutex_unlock(&adev->physical_node_lock);
}

void acpi_device_notify_remove(struct device *dev)
{
	struct acpi_device *adev = ACPI_COMPANION(dev);
	struct acpi_device_physical_node *pn, *tmp;

	if (!adev)
		return;
	mutex_lock(&adev->physical_node_lock);
	list_for_each_entry_safe(pn, tmp, &adev->physical_node_list, node) {
		if (pn->dev == dev) {
			list_del(&pn->node);
			adev->physical_node_count--;
			put_device(dev);
			kfree(pn);
			break;
		}
	}
	mutex_unlock(&adev->physical_node_lock);
}

struct device *acpi_get_first_physical_node(struct acpi_device *adev)
{
	struct acpi_device_physical_node *pn;
	struct device *dev = NULL;

	if (!adev)
		return NULL;
	mutex_lock(&adev->physical_node_lock);
	pn = list_first_entry_or_null(&adev->physical_node_list,
				      struct acpi_device_physical_node, node);
	if (pn)
		dev = pn->dev;
	mutex_unlock(&adev->physical_node_lock);
	return dev;
}

/* --------------------------------------------------------- fwnode ops */

static bool kpi_fw_available(const struct fwnode_handle *fwnode)
{
	return acpi_device_is_present(to_acpi_device_node(fwnode));
}

static const void *kpi_fw_match_data(const struct fwnode_handle *fwnode,
				     const struct device *dev)
{
	return acpi_device_get_match_data(dev);
}

static bool kpi_fw_dma_supported(const struct fwnode_handle *fwnode)
{
	return true;
}

static enum dev_dma_attr kpi_fw_dma_attr(const struct fwnode_handle *fwnode)
{
	return DEV_DMA_COHERENT;
}

/* No _DSD device properties: drivers use their defaults. */
static bool kpi_fw_property_present(const struct fwnode_handle *fwnode, const char *propname)
{
	return false;
}

static int kpi_fw_read_int_array(const struct fwnode_handle *fwnode, const char *propname,
				 unsigned int elem_size, void *val, size_t nval)
{
	return -EINVAL;
}

static int kpi_fw_read_string_array(const struct fwnode_handle *fwnode, const char *propname,
				    const char **val, size_t nval)
{
	return -EINVAL;
}

static const char *kpi_fw_get_name(const struct fwnode_handle *fwnode)
{
	return to_acpi_device_node(fwnode)->pnp.bus_id;
}

static struct fwnode_handle *kpi_fw_get_parent(const struct fwnode_handle *fwnode)
{
	struct acpi_device *parent = acpi_dev_parent(to_acpi_device_node(fwnode));

	return parent ? acpi_fwnode_handle(parent) : NULL;
}

static struct fwnode_handle *kpi_fw_next_child(const struct fwnode_handle *fwnode,
					       struct fwnode_handle *child)
{
	struct acpi_device *adev = to_acpi_device_node(fwnode), *c;
	bool started = !child;

	for_each_kpi_adev(c) {
		if (!started) {
			started = acpi_fwnode_handle(c) == child;
			continue;
		}
		if (c->dev.parent == &adev->dev)
			return acpi_fwnode_handle(c);
	}
	return NULL;
}

static int kpi_fw_reference_args(const struct fwnode_handle *fwnode, const char *prop,
				 const char *nargs_prop, unsigned int nargs, unsigned int index,
				 struct fwnode_reference_args *args)
{
	return -ENOENT;
}

const struct fwnode_operations acpi_device_fwnode_ops = {
	.device_is_available = kpi_fw_available,
	.device_get_match_data = kpi_fw_match_data,
	.device_dma_supported = kpi_fw_dma_supported,
	.device_get_dma_attr = kpi_fw_dma_attr,
	.property_present = kpi_fw_property_present,
	.property_read_int_array = kpi_fw_read_int_array,
	.property_read_string_array = kpi_fw_read_string_array,
	.get_name = kpi_fw_get_name,
	.get_parent = kpi_fw_get_parent,
	.get_next_child_node = kpi_fw_next_child,
	.get_reference_args = kpi_fw_reference_args,
};

/* ------------------------------------------------------- odds and ends */

/* MADT interrupt source overrides of ISA IRQs (drivers/acpi/resource.c). */
bool acpi_int_src_ovr[NR_IRQS_LEGACY];
struct acpi_table_fadt acpi_gbl_FADT;

static int __init kpi_acpi_globals(void)
{
	struct acpi_table_header *fadt;
	u32 gsi;
	int i, level, low;

	for (i = 0; i < NR_IRQS_LEGACY; i++) {
		rustos_kpi_isa_irq(i, &gsi, &level, &low);
		acpi_int_src_ovr[i] = gsi != i || level || low;
	}
	if (ACPI_SUCCESS(acpi_get_table(ACPI_SIG_FADT, 1, &fadt))) {
		memcpy(&acpi_gbl_FADT, fadt, min_t(u32, fadt->length, sizeof(acpi_gbl_FADT)));
		acpi_put_table(fadt);
	}
	return 0;
}
core_initcall(kpi_acpi_globals);

/* Data attached to namespace nodes (ACPICA) and to device objects. */
struct kpi_acpi_data {
	struct list_head list;
	acpi_handle handle;
	acpi_object_handler handler;
	void *data;
};

static LIST_HEAD(kpi_acpi_data_list);
static DEFINE_SPINLOCK(kpi_acpi_data_lock);

acpi_status acpi_attach_data(acpi_handle handle, acpi_object_handler handler, void *data)
{
	struct kpi_acpi_data *d, *e;

	d = kzalloc(sizeof(*d), GFP_KERNEL);
	if (!d)
		return AE_NO_MEMORY;
	d->handle = handle;
	d->handler = handler;
	d->data = data;
	spin_lock(&kpi_acpi_data_lock);
	list_for_each_entry(e, &kpi_acpi_data_list, list) {
		if (e->handle == handle && e->handler == handler) {
			spin_unlock(&kpi_acpi_data_lock);
			kfree(d);
			return AE_ALREADY_EXISTS;
		}
	}
	list_add(&d->list, &kpi_acpi_data_list);
	spin_unlock(&kpi_acpi_data_lock);
	return AE_OK;
}

static struct kpi_acpi_data *kpi_find_data(acpi_handle handle, acpi_object_handler handler)
{
	struct kpi_acpi_data *e;

	list_for_each_entry(e, &kpi_acpi_data_list, list)
		if (e->handle == handle && e->handler == handler)
			return e;
	return NULL;
}

acpi_status acpi_detach_data(acpi_handle handle, acpi_object_handler handler)
{
	struct kpi_acpi_data *e;

	spin_lock(&kpi_acpi_data_lock);
	e = kpi_find_data(handle, handler);
	if (e)
		list_del(&e->list);
	spin_unlock(&kpi_acpi_data_lock);
	kfree(e);
	return e ? AE_OK : AE_NOT_FOUND;
}

acpi_status acpi_get_data(acpi_handle handle, acpi_object_handler handler, void **data)
{
	struct kpi_acpi_data *e;

	spin_lock(&kpi_acpi_data_lock);
	e = kpi_find_data(handle, handler);
	if (e)
		*data = e->data;
	spin_unlock(&kpi_acpi_data_lock);
	return e ? AE_OK : AE_NOT_FOUND;
}

static void kpi_private_data_handler(acpi_handle handle, void *context)
{
}

int acpi_bus_attach_private_data(acpi_handle handle, void *data)
{
	return ACPI_SUCCESS(acpi_attach_data(handle, kpi_private_data_handler, data)) ? 0 : -EINVAL;
}

int acpi_bus_get_private_data(acpi_handle handle, void **data)
{
	return ACPI_SUCCESS(acpi_get_data(handle, kpi_private_data_handler, data)) ? 0 : -ENODEV;
}

void acpi_bus_detach_private_data(acpi_handle handle)
{
	acpi_detach_data(handle, kpi_private_data_handler);
}

/* One resource descriptor in AML form (GPIO operation region connections). */
acpi_status acpi_buffer_to_resource(u8 *aml_buffer, u16 aml_buffer_length,
				    struct acpi_resource **resource_ptr)
{
	struct acpi_resource *r;

	if (!aml_buffer_length)
		return AE_BAD_PARAMETER;
	r = kzalloc(sizeof(*r) + 2 * aml_buffer_length + 128, GFP_KERNEL);
	if (!r)
		return AE_NO_MEMORY;
	if (!kpi_res_convert(aml_buffer, aml_buffer_length, r)) {
		kfree(r);
		return AE_AML_INVALID_RESOURCE_TYPE;
	}
	*resource_ptr = r;
	return AE_OK;
}

/* AML access to GPIO and GenericSerialBus operation regions is not routed
 * to Linux drivers: RustOS's interpreter handles the regions it knows. */
acpi_status acpi_install_address_space_handler(acpi_handle device, acpi_adr_space_type space_id,
					       acpi_adr_space_handler handler,
					       acpi_adr_space_setup setup, void *context)
{
	return AE_OK;
}

acpi_status acpi_remove_address_space_handler(acpi_handle device, acpi_adr_space_type space_id,
					      acpi_adr_space_handler handler)
{
	return AE_OK;
}

void acpi_dev_clear_dependencies(struct acpi_device *supplier)
{
}

bool acpi_dma_supported(const struct acpi_device *adev)
{
	return true;
}

void acpi_set_modalias(struct acpi_device *adev, const char *default_id, char *modalias,
		       size_t len)
{
	strscpy(modalias, default_id, len);
}

int acpi_unbind_one(struct device *dev)
{
	acpi_device_notify_remove(dev);
	return 0;
}

/* No _DSD properties or data nodes. */
int __acpi_node_get_property_reference(const struct fwnode_handle *fwnode, const char *propname,
				       size_t index, size_t num_args,
				       struct fwnode_reference_args *args)
{
	return -ENOENT;
}

bool is_acpi_data_node(const struct fwnode_handle *fwnode)
{
	return false;
}

/* Wakeup from suspend: there is no suspend. */
int acpi_register_wakeup_handler(int wake_irq, bool (*wakeup)(void *context), void *context)
{
	return 0;
}

void acpi_unregister_wakeup_handler(bool (*wakeup)(void *context), void *context)
{
}
