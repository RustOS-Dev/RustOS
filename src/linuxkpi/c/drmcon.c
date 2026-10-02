// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * RustOS's text console on a DRM device: the in-kernel client drivers
 * start with drm_client_setup() (in place of Linux's fbdev emulation and
 * drm_log). It sets the preferred mode with a buffer of its own, gives
 * that buffer to the console (src/drivers/framebuffer.rs), and flushes
 * what the console draws at 30 Hz, since bochs and virtio-gpu copy from
 * shadow buffers on damage. When the last user-space DRM client closes,
 * the console's mode is restored, as Linux restores fbcon's.
 *
 * The first DRM device to call drm_client_setup() gets the console.
 */
#include <drm/clients/drm_client_setup.h>
#include <drm/drm_client.h>
#include <drm/drm_device.h>
#include <drm/drm_fourcc.h>
#include <drm/drm_framebuffer.h>
#include <drm/drm_modes.h>
#include <drm/drm_print.h>
#include <linux/slab.h>
#include <linux/workqueue.h>
#include "kpi.h"

struct kpi_con {
	struct drm_client_dev client;
	struct drm_client_buffer *buffer;
	struct delayed_work flush;
	u32 width, height;
};

static struct kpi_con *kpi_console;	/* the device that has the console */
static DEFINE_MUTEX(kpi_con_lock);

static void kpi_con_flush(struct work_struct *work)
{
	struct kpi_con *c = container_of(work, struct kpi_con, flush.work);
	u32 lo, hi;

	if (rustos_kpi_console_damage(&lo, &hi)) {
		struct drm_rect r = DRM_RECT_INIT(0, lo, c->width,
						  min(hi, c->height - 1) - lo + 1);

		drm_client_framebuffer_flush(c->buffer, &r);
	}
	schedule_delayed_work(&c->flush, msecs_to_jiffies(33));
}

/* Probe outputs, set the preferred mode with the console's buffer. */
static int kpi_con_start(struct kpi_con *c)
{
	struct drm_mode_set *modeset;
	struct iosys_map map;
	u32 w = 0, h = 0;
	int ret;

	ret = drm_client_modeset_probe(&c->client, 0, 0);
	if (ret)
		return ret;
	mutex_lock(&c->client.modeset_mutex);
	drm_client_for_each_modeset(modeset, &c->client) {
		if (modeset->mode) {
			w = modeset->mode->hdisplay;
			h = modeset->mode->vdisplay;
			break;
		}
	}
	mutex_unlock(&c->client.modeset_mutex);
	if (!w || !h)
		return -ENODEV;
	c->buffer = drm_client_framebuffer_create(&c->client, w, h, DRM_FORMAT_XRGB8888);
	if (IS_ERR(c->buffer)) {
		ret = PTR_ERR(c->buffer);
		c->buffer = NULL;
		return ret;
	}
	ret = drm_client_buffer_vmap(c->buffer, &map);
	if (ret)
		goto delete;
	mutex_lock(&c->client.modeset_mutex);
	drm_client_for_each_modeset(modeset, &c->client)
		modeset->fb = c->buffer->fb;
	mutex_unlock(&c->client.modeset_mutex);
	ret = drm_client_modeset_commit(&c->client);
	if (ret)
		goto unmap;
	c->width = w;
	c->height = h;
	rustos_kpi_console_attach(map.vaddr, (u64)c->buffer->fb->pitches[0] * h, w, h,
				  c->buffer->fb->pitches[0]);
	schedule_delayed_work(&c->flush, msecs_to_jiffies(33));
	return 0;
unmap:
	drm_client_buffer_vunmap(c->buffer);
delete:
	drm_client_buffer_delete(c->buffer);
	c->buffer = NULL;
	return ret;
}

static int kpi_con_hotplug(struct drm_client_dev *client)
{
	struct kpi_con *c = container_of(client, struct kpi_con, client);
	struct drm_mode_set *ms;

	if (!c->buffer)
		return kpi_con_start(c);
	/* Outputs changed: set the console's mode on what is connected now. */
	drm_client_modeset_probe(client, c->width, c->height);
	mutex_lock(&client->modeset_mutex);
	drm_client_for_each_modeset(ms, client)
		ms->fb = c->buffer->fb;
	mutex_unlock(&client->modeset_mutex);
	return drm_client_modeset_commit(client);
}

/* The last user-space client closed: show the console again. */
static int kpi_con_restore(struct drm_client_dev *client)
{
	struct kpi_con *c = container_of(client, struct kpi_con, client);

	if (!c->buffer)
		return 0;
	return drm_client_modeset_commit(client);
}

static void kpi_con_unregister(struct drm_client_dev *client)
{
	struct kpi_con *c = container_of(client, struct kpi_con, client);

	cancel_delayed_work_sync(&c->flush);
	mutex_lock(&kpi_con_lock);
	if (kpi_console == c) {
		kpi_console = NULL;
		rustos_kpi_fb_release();
	}
	mutex_unlock(&kpi_con_lock);
	if (c->buffer) {
		drm_client_buffer_vunmap(c->buffer);
		drm_client_buffer_delete(c->buffer);
	}
	drm_client_release(client);
	kfree(c);
}

static const struct drm_client_funcs kpi_con_funcs = {
	.owner = THIS_MODULE,
	.unregister = kpi_con_unregister,
	.restore = kpi_con_restore,
	.hotplug = kpi_con_hotplug,
};

void drm_client_setup(struct drm_device *dev, const struct drm_format_info *format)
{
	struct kpi_con *c;
	int ret;

	mutex_lock(&kpi_con_lock);
	if (kpi_console) {
		mutex_unlock(&kpi_con_lock);
		return;
	}
	c = kzalloc(sizeof(*c), GFP_KERNEL);
	if (!c) {
		mutex_unlock(&kpi_con_lock);
		return;
	}
	INIT_DELAYED_WORK(&c->flush, kpi_con_flush);
	ret = drm_client_init(dev, &c->client, "rustos-console", &kpi_con_funcs);
	if (ret) {
		drm_err(dev, "console client: %d\n", ret);
		kfree(c);
		mutex_unlock(&kpi_con_lock);
		return;
	}
	kpi_console = c;
	mutex_unlock(&kpi_con_lock);
	drm_client_register(&c->client);
}

void drm_client_setup_with_fourcc(struct drm_device *dev, u32 fourcc)
{
	drm_client_setup(dev, drm_format_info(fourcc));
}

void drm_client_setup_with_color_mode(struct drm_device *dev, unsigned int color_mode)
{
	drm_client_setup(dev, NULL);
}
