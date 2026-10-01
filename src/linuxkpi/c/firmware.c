// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * Firmware loading for LinuxKPI (the drivers/base/firmware_loader API) on
 * RustOS's firmware search path (src/firmware.rs: the initramfs'
 * /lib/firmware, /storage/lib/firmware, the EFI system partition).
 *
 * Linux drivers often probe before RustOS has mounted the boot drive (a
 * USB stick enumerates late), so a lookup that misses waits for storage,
 * up to a minute after boot; see rustos_kpi_firmware_load().
 */
#include <linux/device.h>
#include <linux/firmware.h>
#include <linux/slab.h>
#include <linux/string.h>
#include <linux/vmalloc.h>
#include <linux/workqueue.h>
#include "kpi.h"

/* Set in firmware->priv for buffers the caller supplied (into_buf). */
#define KPI_FW_CALLER_BUF ((void *)1)

static void *kpi_fw_alloc(size_t size)
{
	return vmalloc(size ? size : 1);
}

static int kpi_fw_get(const struct firmware **fw_p, const char *name, struct device *dev,
		      bool warn, void *buf, size_t buf_size)
{
	struct firmware *fw;
	void *data = NULL;
	size_t size = 0;
	int err;

	if (!fw_p)
		return -EINVAL;
	*fw_p = NULL;
	if (!name || !*name)
		return -EINVAL;
	fw = kzalloc(sizeof(*fw), GFP_KERNEL);
	if (!fw)
		return -ENOMEM;
	err = rustos_kpi_firmware_load(name, kpi_fw_alloc, &data, &size);
	if (err) {
		if (warn)
			dev_warn(dev, "Direct firmware load for %s failed with error %d\n", name,
				 err);
		kfree(fw);
		return err;
	}
	if (buf) {
		if (size > buf_size) {
			vfree(data);
			kfree(fw);
			return -EFBIG;
		}
		memcpy(buf, data, size);
		vfree(data);
		data = buf;
		fw->priv = KPI_FW_CALLER_BUF;
	}
	fw->data = data;
	fw->size = size;
	*fw_p = fw;
	return 0;
}

int request_firmware(const struct firmware **fw, const char *name, struct device *device)
{
	return kpi_fw_get(fw, name, device, true, NULL, 0);
}

int firmware_request_nowarn(const struct firmware **fw, const char *name,
			    struct device *device)
{
	return kpi_fw_get(fw, name, device, false, NULL, 0);
}

int request_firmware_direct(const struct firmware **fw, const char *name,
			    struct device *device)
{
	return kpi_fw_get(fw, name, device, false, NULL, 0);
}

int firmware_request_platform(const struct firmware **fw, const char *name,
			      struct device *device)
{
	return kpi_fw_get(fw, name, device, true, NULL, 0);
}

int request_firmware_into_buf(const struct firmware **fw, const char *name,
			      struct device *device, void *buf, size_t size)
{
	return kpi_fw_get(fw, name, device, true, buf, size);
}

void release_firmware(const struct firmware *fw)
{
	if (!fw)
		return;
	if (fw->priv != KPI_FW_CALLER_BUF)
		vfree(fw->data);
	kfree(fw);
}

/* ------------------------------------------------------- asynchronous */

struct kpi_fw_work {
	struct work_struct work;
	const char *name;
	struct device *dev;
	void *context;
	void (*cont)(const struct firmware *fw, void *context);
	bool warn;
};

static void kpi_fw_work_fn(struct work_struct *work)
{
	struct kpi_fw_work *w = container_of(work, struct kpi_fw_work, work);
	const struct firmware *fw;

	kpi_fw_get(&fw, w->name, w->dev, w->warn, NULL, 0);
	w->cont(fw, w->context);
	kfree_const(w->name);
	kfree(w);
}

static int kpi_fw_nowait(const char *name, struct device *device, gfp_t gfp, void *context,
			 void (*cont)(const struct firmware *fw, void *context), bool warn)
{
	struct kpi_fw_work *w = kzalloc(sizeof(*w), gfp);

	if (!w)
		return -ENOMEM;
	w->name = kstrdup_const(name, gfp);
	if (!w->name) {
		kfree(w);
		return -ENOMEM;
	}
	w->dev = device;
	w->context = context;
	w->cont = cont;
	w->warn = warn;
	INIT_WORK(&w->work, kpi_fw_work_fn);
	/* system_long_wq: the lookup may wait for storage to be mounted. */
	queue_work(system_long_wq, &w->work);
	return 0;
}

int request_firmware_nowait(struct module *module, bool uevent, const char *name,
			    struct device *device, gfp_t gfp, void *context,
			    void (*cont)(const struct firmware *fw, void *context))
{
	return kpi_fw_nowait(name, device, gfp, context, cont, true);
}

int firmware_request_nowait_nowarn(struct module *module, const char *name,
				   struct device *device, gfp_t gfp, void *context,
				   void (*cont)(const struct firmware *fw, void *context))
{
	return kpi_fw_nowait(name, device, gfp, context, cont, false);
}
