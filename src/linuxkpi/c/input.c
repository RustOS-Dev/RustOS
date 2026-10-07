// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * Linux input devices as RustOS input devices.
 *
 * Linux's input core (drivers/input/input.c, imported) runs as in Linux;
 * this handler, in place of evdev, connects to every input device. Each
 * one becomes a RustOS input device (/dev/input/eventN,
 * src/drivers/input.rs) with the same capabilities, and its events go
 * there. Keyboards also feed the console (src/linuxkpi/input.rs), and the
 * console's lock-key LEDs come back as EV_LED events.
 */
#include <linux/input.h>
#include <linux/module.h>
#include <linux/slab.h>
#include "kpi.h"

struct kpi_input {
	struct input_handle handle;
	u64 rid;	/* RustOS device */
};

/* Capabilities in the layout src/linuxkpi/input.rs reads. */
struct kpi_input_caps {
	const unsigned long *key, *rel, *abs, *led, *prop;
	const struct input_absinfo *absinfo;
	u16 id[4];
};

static int kpi_input_connect(struct input_handler *handler, struct input_dev *dev,
			     const struct input_device_id *id)
{
	struct kpi_input *k = kzalloc(sizeof(*k), GFP_KERNEL);
	struct kpi_input_caps caps;
	int err;

	if (!k)
		return -ENOMEM;
	k->handle.dev = dev;
	k->handle.handler = handler;
	k->handle.name = "rustos";
	err = input_register_handle(&k->handle);
	if (err)
		goto free;
	err = input_open_device(&k->handle);
	if (err)
		goto unregister;
	caps.key = dev->keybit;
	caps.rel = dev->relbit;
	caps.abs = dev->absbit;
	caps.led = dev->ledbit;
	caps.prop = dev->propbit;
	caps.absinfo = dev->absinfo;
	caps.id[0] = dev->id.bustype;
	caps.id[1] = dev->id.vendor;
	caps.id[2] = dev->id.product;
	caps.id[3] = dev->id.version;
	k->rid = rustos_kpi_input_add(dev->name ?: "input", dev->phys ?: "", &caps,
				      &k->handle);
	return 0;
unregister:
	input_unregister_handle(&k->handle);
free:
	kfree(k);
	return err;
}

static void kpi_input_disconnect(struct input_handle *handle)
{
	struct kpi_input *k = container_of(handle, struct kpi_input, handle);

	rustos_kpi_input_remove(k->rid);
	input_close_device(handle);
	input_unregister_handle(handle);
	kfree(k);
}

static unsigned int kpi_input_events(struct input_handle *handle, struct input_value *vals,
				     unsigned int count)
{
	struct kpi_input *k = container_of(handle, struct kpi_input, handle);

	for (unsigned int i = 0; i < count; i++)
		rustos_kpi_input_event(k->rid, vals[i].type, vals[i].code, vals[i].value);
	return count;
}

/* The console's lock keys changed (bit 0 num, 1 caps, 2 scroll lock). */
void kpi_input_set_leds(struct input_handle *handle, u32 bits)
{
	struct input_dev *dev = handle->dev;

	if (!test_bit(EV_LED, dev->evbit))
		return;
	input_inject_event(handle, EV_LED, LED_NUML, !!(bits & 1));
	input_inject_event(handle, EV_LED, LED_CAPSL, !!(bits & 2));
	input_inject_event(handle, EV_LED, LED_SCROLLL, !!(bits & 4));
	input_inject_event(handle, EV_SYN, SYN_REPORT, 0);
}

static const struct input_device_id kpi_input_ids[] = {
	/* Every device: all report EV_SYN. */
	{ .flags = INPUT_DEVICE_ID_MATCH_EVBIT, .evbit = { BIT_MASK(EV_SYN) } },
	{ },
};

static struct input_handler kpi_input_handler = {
	.events = kpi_input_events,
	.connect = kpi_input_connect,
	.disconnect = kpi_input_disconnect,
	.name = "rustos",
	.id_table = kpi_input_ids,
};

static int __init kpi_input_init(void)
{
	return input_register_handler(&kpi_input_handler);
}
/* Before the drivers' module_init()s register their devices. */
subsys_initcall(kpi_input_init);
