// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * LinuxKPI USB core: Linux USB drivers on RustOS's xHCI driver.
 *
 * RustOS enumerates devices and selects their configuration
 * (src/usb/mod.rs). Interfaces that no RustOS driver claims are offered
 * here (src/linuxkpi/usb.rs): the first offer builds the struct usb_device
 * from the device's descriptors (Linux's drivers/usb/core/config.c parses
 * them) and the interfaces of the active configuration; each offered
 * interface is then added to the device core on the "usb" bus, which
 * matches usb_driver ID tables and probes.
 *
 * URBs (drivers/usb/core/urb.c, imported) reach the host controller
 * through usb_hcd_submit_urb()/usb_hcd_unlink_urb() below: RustOS runs the
 * transfers of each endpoint in order on a worker thread and gives each
 * URB back with kpi_usb_complete(), which calls its completion with
 * bottom halves disabled, as Linux's HCD giveback does.
 */
#include <linux/dma-mapping.h>
#include <linux/io.h>
#include <linux/module.h>
#include <linux/slab.h>
#include <linux/usb.h>
#include <linux/usb/cdc.h>
#include <linux/usb/hcd.h>
#include <linux/usb/quirks.h>
#include <linux/unaligned.h>
#include "kpi.h"

/* drivers/usb/core/config.c */
int usb_get_configuration(struct usb_device *dev);
void usb_destroy_configuration(struct usb_device *dev);
void usb_release_interface_cache(struct kref *ref);

#define KPI_USB_BUSES	8

struct kpi_usb_dev {
	struct usb_device udev;
	u64 handle;
};

static struct usb_bus *kpi_usb_bus[KPI_USB_BUSES];
static struct device *kpi_usb_root;
static DEFINE_SPINLOCK(kpi_urb_lock);
DECLARE_WAIT_QUEUE_HEAD(usb_kill_urb_queue);

static u64 kpi_handle(const struct usb_device *udev)
{
	return container_of(udev, struct kpi_usb_dev, udev)->handle;
}

const char *usb_speed_string(enum usb_device_speed speed)
{
	static const char *const names[] = {
		[USB_SPEED_UNKNOWN] = "UNKNOWN",
		[USB_SPEED_LOW] = "low-speed",
		[USB_SPEED_FULL] = "full-speed",
		[USB_SPEED_HIGH] = "high-speed",
		[USB_SPEED_WIRELESS] = "wireless",
		[USB_SPEED_SUPER] = "super-speed",
		[USB_SPEED_SUPER_PLUS] = "super-speed-plus",
	};

	if (speed < 0 || speed >= ARRAY_SIZE(names))
		speed = USB_SPEED_UNKNOWN;
	return names[speed];
}
EXPORT_SYMBOL_GPL(usb_speed_string);

/* --------------------------------------------------------- control messages */

int usb_control_msg(struct usb_device *dev, unsigned int pipe, __u8 request,
		    __u8 requesttype, __u16 value, __u16 index, void *data,
		    __u16 size, int timeout)
{
	u8 setup[8];

	setup[0] = requesttype;
	setup[1] = request;
	put_unaligned_le16(value, &setup[2]);
	put_unaligned_le16(index, &setup[4]);
	put_unaligned_le16(size, &setup[6]);
	if (dev->state == USB_STATE_NOTATTACHED)
		return -ENODEV;
	return rustos_kpi_usb_control(kpi_handle(dev), setup, data, timeout);
}
EXPORT_SYMBOL_GPL(usb_control_msg);

int usb_control_msg_send(struct usb_device *dev, __u8 endpoint, __u8 request,
			 __u8 requesttype, __u16 value, __u16 index,
			 const void *driver_data, __u16 size, int timeout,
			 gfp_t memflags)
{
	u8 *data = NULL;
	int ret;

	if (size) {
		data = kmemdup(driver_data, size, memflags);
		if (!data)
			return -ENOMEM;
	}
	ret = usb_control_msg(dev, usb_sndctrlpipe(dev, endpoint), request, requesttype,
			      value, index, data, size, timeout);
	kfree(data);
	return ret < 0 ? ret : 0;
}
EXPORT_SYMBOL_GPL(usb_control_msg_send);

int usb_control_msg_recv(struct usb_device *dev, __u8 endpoint, __u8 request,
			 __u8 requesttype, __u16 value, __u16 index,
			 void *driver_data, __u16 size, int timeout,
			 gfp_t memflags)
{
	u8 *data;
	int ret;

	if (!size || !driver_data)
		return -EINVAL;
	data = kmalloc(size, memflags);
	if (!data)
		return -ENOMEM;
	ret = usb_control_msg(dev, usb_rcvctrlpipe(dev, endpoint), request, requesttype,
			      value, index, data, size, timeout);
	if (ret == size) {
		memcpy(driver_data, data, size);
		ret = 0;
	} else if (ret >= 0) {
		ret = -EREMOTEIO;
	}
	kfree(data);
	return ret;
}
EXPORT_SYMBOL_GPL(usb_control_msg_recv);

int usb_get_descriptor(struct usb_device *dev, unsigned char type,
		       unsigned char index, void *buf, int size)
{
	int result = -EIO;

	memset(buf, 0, size);
	for (int i = 0; i < 3; i++) {
		result = usb_control_msg(dev, usb_rcvctrlpipe(dev, 0), USB_REQ_GET_DESCRIPTOR,
					 USB_DIR_IN, (type << 8) + index, 0, buf, size,
					 USB_CTRL_GET_TIMEOUT);
		if (result <= 0 && result != -ETIMEDOUT)
			continue;
		if (result > 1 && ((u8 *)buf)[1] != type) {
			result = -ENODATA;
			continue;
		}
		break;
	}
	return result;
}
EXPORT_SYMBOL_GPL(usb_get_descriptor);

int usb_get_status(struct usb_device *dev, int recip, int type, int target, void *data)
{
	__le16 *status = kmalloc(2, GFP_KERNEL);
	int ret;

	if (!status)
		return -ENOMEM;
	ret = usb_control_msg(dev, usb_rcvctrlpipe(dev, 0), USB_REQ_GET_STATUS,
			      USB_DIR_IN | recip, USB_STATUS_TYPE_STANDARD, target, status, 2,
			      USB_CTRL_GET_TIMEOUT);
	if (ret == 2) {
		*(u16 *)data = le16_to_cpu(*status);
		ret = 0;
	} else if (ret >= 0) {
		ret = -EIO;
	}
	kfree(status);
	return ret;
}
EXPORT_SYMBOL_GPL(usb_get_status);

/* UTF-16LE string descriptor to UTF-8 (characters beyond U+07FF as '?'). */
int usb_string(struct usb_device *dev, int index, char *buf, size_t size)
{
	u8 *tbuf;
	int err, n = 0;

	if (size <= 0 || !buf)
		return -EINVAL;
	buf[0] = 0;
	if (index <= 0 || index >= 256)
		return -EINVAL;
	tbuf = kmalloc(256, GFP_NOIO);
	if (!tbuf)
		return -ENOMEM;
	if (!dev->have_langid) {
		err = usb_get_descriptor(dev, USB_DT_STRING, 0, tbuf, 255);
		dev->string_langid = err >= 4 ? get_unaligned_le16(&tbuf[2]) : 0x0409;
		dev->have_langid = 1;
	}
	err = usb_control_msg(dev, usb_rcvctrlpipe(dev, 0), USB_REQ_GET_DESCRIPTOR, USB_DIR_IN,
			      (USB_DT_STRING << 8) + index, dev->string_langid, tbuf, 255,
			      USB_CTRL_GET_TIMEOUT);
	if (err < 2 || tbuf[1] != USB_DT_STRING) {
		kfree(tbuf);
		return err < 0 ? err : -EINVAL;
	}
	err = min_t(int, err, tbuf[0]);
	for (int i = 2; i + 1 < err; i += 2) {
		u16 c = get_unaligned_le16(&tbuf[i]);

		if (c < 0x80 && n + 1 < size) {
			buf[n++] = c;
		} else if (c < 0x800 && n + 2 < size) {
			buf[n++] = 0xc0 | (c >> 6);
			buf[n++] = 0x80 | (c & 0x3f);
		} else if (c >= 0x800 && n + 1 < size) {
			buf[n++] = '?';
		}
	}
	buf[n] = 0;
	kfree(tbuf);
	return n;
}
EXPORT_SYMBOL_GPL(usb_string);

char *usb_cache_string(struct usb_device *udev, int index)
{
	char *buf, *smallbuf = NULL;
	int len;

	if (index <= 0)
		return NULL;
	buf = kmalloc(256, GFP_NOIO);
	if (!buf)
		return NULL;
	len = usb_string(udev, index, buf, 256);
	if (len > 0) {
		smallbuf = kmalloc(++len, GFP_NOIO);
		if (smallbuf)
			memcpy(smallbuf, buf, len);
	}
	kfree(buf);
	return smallbuf;
}
EXPORT_SYMBOL_GPL(usb_cache_string);

/* ---------------------------------------------------- synchronous transfers */

struct kpi_api_context {
	struct completion done;
	int status;
};

static void kpi_api_blocking_completion(struct urb *urb)
{
	struct kpi_api_context *ctx = urb->context;

	ctx->status = urb->status;
	complete(&ctx->done);
}

/* drivers/usb/core/message.c usb_start_wait_urb() */
static int kpi_start_wait_urb(struct urb *urb, int timeout, int *actual_length)
{
	struct kpi_api_context ctx;
	unsigned long expire;
	int retval;
	long rc;

	init_completion(&ctx.done);
	urb->context = &ctx;
	urb->actual_length = 0;
	retval = usb_submit_urb(urb, GFP_NOIO);
	if (unlikely(retval))
		goto out;
	if (timeout <= 0 || timeout > USB_MAX_SYNCHRONOUS_TIMEOUT)
		timeout = USB_MAX_SYNCHRONOUS_TIMEOUT;
	expire = msecs_to_jiffies(timeout);
	rc = wait_for_completion_timeout(&ctx.done, expire);
	if (rc <= 0) {
		usb_kill_urb(urb);
		retval = ctx.status != -ENOENT ? ctx.status : -ETIMEDOUT;
	} else {
		retval = ctx.status;
	}
out:
	if (actual_length)
		*actual_length = urb->actual_length;
	usb_free_urb(urb);
	return retval;
}

int usb_bulk_msg(struct usb_device *usb_dev, unsigned int pipe, void *data,
		 int len, int *actual_length, int timeout)
{
	struct usb_host_endpoint *ep = usb_pipe_endpoint(usb_dev, pipe);
	struct urb *urb;

	if (!ep)
		return -EINVAL;
	urb = usb_alloc_urb(0, GFP_KERNEL);
	if (!urb)
		return -ENOMEM;
	if (usb_endpoint_xfer_int(&ep->desc)) {
		pipe = (pipe & ~(3 << 30)) | (PIPE_INTERRUPT << 30);
		usb_fill_int_urb(urb, usb_dev, pipe, data, len, kpi_api_blocking_completion,
				 NULL, ep->desc.bInterval);
	} else {
		usb_fill_bulk_urb(urb, usb_dev, pipe, data, len, kpi_api_blocking_completion,
				  NULL);
	}
	return kpi_start_wait_urb(urb, timeout, actual_length);
}
EXPORT_SYMBOL_GPL(usb_bulk_msg);

int usb_interrupt_msg(struct usb_device *usb_dev, unsigned int pipe, void *data,
		      int len, int *actual_length, int timeout)
{
	return usb_bulk_msg(usb_dev, pipe, data, len, actual_length, timeout);
}
EXPORT_SYMBOL_GPL(usb_interrupt_msg);

/* ------------------------------------------------------------- the "HCD" */

static u8 kpi_urb_epaddr(const struct urb *urb)
{
	if (usb_pipecontrol(urb->pipe))
		return 0;
	return usb_pipeendpoint(urb->pipe) | (usb_pipein(urb->pipe) ? USB_DIR_IN : 0);
}

/* Called by usb_submit_urb() (urb.c) once the URB checked out. */
int usb_hcd_submit_urb(struct urb *urb, gfp_t mem_flags)
{
	struct usb_device *udev = urb->dev;
	unsigned long flags;
	int status;

	usb_get_urb(urb);
	atomic_inc(&urb->use_count);
	atomic_inc(&udev->urbnum);
	spin_lock_irqsave(&kpi_urb_lock, flags);
	if (unlikely(atomic_read(&urb->reject)))
		status = -EPERM;
	else if (udev->state == USB_STATE_NOTATTACHED)
		status = -ESHUTDOWN;
	else if (urb->num_sgs)
		status = -EOPNOTSUPP;
	else
		status = 0;
	if (!status) {
		urb->unlinked = 0;
		urb->hcpriv = urb;
	}
	spin_unlock_irqrestore(&kpi_urb_lock, flags);
	if (!status)
		status = rustos_kpi_usb_submit(kpi_handle(udev), kpi_urb_epaddr(urb),
					       urb->transfer_buffer,
					       urb->transfer_buffer_length,
					       usb_pipecontrol(urb->pipe) ? urb->setup_packet : NULL,
					       !!(urb->transfer_flags & URB_ZERO_PACKET),
					       usb_pipeisoc(urb->pipe) ? urb->iso_frame_desc : NULL,
					       usb_pipeisoc(urb->pipe) ? urb->number_of_packets : 0,
					       urb);
	if (unlikely(status)) {
		urb->hcpriv = NULL;
		INIT_LIST_HEAD(&urb->urb_list);
		atomic_dec(&urb->use_count);
		atomic_dec(&udev->urbnum);
		if (atomic_read(&urb->reject))
			wake_up(&usb_kill_urb_queue);
		usb_put_urb(urb);
	}
	return status;
}

/* Called by usb_unlink_urb()/usb_kill_urb(): ask RustOS to stop the URB;
 * it is given back with `status`. */
int usb_hcd_unlink_urb(struct urb *urb, int status)
{
	unsigned long flags;

	if (!urb->dev)
		return -ENODEV;
	spin_lock_irqsave(&kpi_urb_lock, flags);
	if (!urb->hcpriv) {
		spin_unlock_irqrestore(&kpi_urb_lock, flags);
		return -EIDRM;
	}
	if (urb->unlinked) {
		spin_unlock_irqrestore(&kpi_urb_lock, flags);
		return -EBUSY;
	}
	urb->unlinked = status;
	spin_unlock_irqrestore(&kpi_urb_lock, flags);
	rustos_kpi_usb_cancel(kpi_handle(urb->dev), kpi_urb_epaddr(urb), urb);
	return -EINPROGRESS;
}

/* RustOS finished (or abandoned) a URB: __usb_hcd_giveback_urb(). */
void kpi_usb_complete(void *ctx, int status, u32 actual)
{
	struct urb *urb = ctx;
	struct usb_anchor *anchor = urb->anchor;
	unsigned long flags;

	spin_lock_irqsave(&kpi_urb_lock, flags);
	urb->hcpriv = NULL;
	if (usb_pipeisoc(urb->pipe)) {
		urb->error_count = 0;
		for (int i = 0; i < urb->number_of_packets; i++)
			urb->error_count += urb->iso_frame_desc[i].status != 0;
		urb->start_frame = 0;
	}
	if (urb->unlinked)
		status = urb->unlinked;
	else if (!status && (urb->transfer_flags & URB_SHORT_NOT_OK) && !usb_pipeisoc(urb->pipe) &&
		 actual < urb->transfer_buffer_length && usb_pipein(urb->pipe))
		status = -EREMOTEIO;
	spin_unlock_irqrestore(&kpi_urb_lock, flags);
	urb->actual_length = actual;
	urb->status = status;
	usb_anchor_suspend_wakeups(anchor);
	usb_unanchor_urb(urb);
	local_bh_disable();
	urb->complete(urb);
	local_bh_enable();
	usb_anchor_resume_wakeups(anchor);
	atomic_dec(&urb->use_count);
	atomic_dec(&urb->dev->urbnum);
	if (unlikely(atomic_read(&urb->reject)))
		wake_up(&usb_kill_urb_queue);
	usb_put_urb(urb);
}

void *usb_alloc_coherent(struct usb_device *dev, size_t size, gfp_t mem_flags,
			 dma_addr_t *dma)
{
	void *p = kmalloc(size, mem_flags);

	if (p && dma)
		*dma = virt_to_phys(p);
	return p;
}
EXPORT_SYMBOL_GPL(usb_alloc_coherent);

void usb_free_coherent(struct usb_device *dev, size_t size, void *addr, dma_addr_t dma)
{
	kfree(addr);
}
EXPORT_SYMBOL_GPL(usb_free_coherent);

/* Streaming buffers (uvcvideo): plain memory with a one-entry table; no
 * IOMMU or cache maintenance is needed on x86. */
void *usb_alloc_noncoherent(struct usb_device *dev, size_t size, gfp_t mem_flags,
			    dma_addr_t *dma_handle, enum dma_data_direction dir,
			    struct sg_table **table)
{
	struct sg_table *sgt = kzalloc(sizeof(*sgt), mem_flags);
	void *p = kmalloc(size, mem_flags);

	if (!sgt || !p || sg_alloc_table(sgt, 1, mem_flags)) {
		kfree(sgt);
		kfree(p);
		return NULL;
	}
	sg_set_buf(sgt->sgl, p, size);
	sg_dma_address(sgt->sgl) = virt_to_phys(p);
	sg_dma_len(sgt->sgl) = size;
	sgt->nents = 1;
	if (dma_handle)
		*dma_handle = virt_to_phys(p);
	*table = sgt;
	return p;
}
EXPORT_SYMBOL_GPL(usb_alloc_noncoherent);

void usb_free_noncoherent(struct usb_device *dev, size_t size, void *addr,
			  enum dma_data_direction dir, struct sg_table *table)
{
	if (table) {
		sg_free_table(table);
		kfree(table);
	}
	kfree(addr);
}
EXPORT_SYMBOL_GPL(usb_free_noncoherent);

/* The (micro)frame counter as the bus would show it: milliseconds. */
int usb_get_current_frame_number(struct usb_device *usb_dev)
{
	return jiffies_to_msecs(jiffies) & 0x7ff;
}
EXPORT_SYMBOL_GPL(usb_get_current_frame_number);

u32 usb_endpoint_max_periodic_payload(struct usb_device *udev,
				      const struct usb_host_endpoint *ep)
{
	if (!usb_endpoint_xfer_isoc(&ep->desc) && !usb_endpoint_xfer_int(&ep->desc))
		return 0;
	switch (udev->speed) {
	case USB_SPEED_SUPER_PLUS:
		if (USB_SS_SSP_ISOC_COMP(ep->ss_ep_comp.bmAttributes))
			return le32_to_cpu(ep->ssp_isoc_ep_comp.dwBytesPerInterval);
		fallthrough;
	case USB_SPEED_SUPER:
		return le16_to_cpu(ep->ss_ep_comp.wBytesPerInterval);
	default:
		if (usb_endpoint_is_hs_isoc_double(udev, ep))
			return le32_to_cpu(ep->eusb2_isoc_ep_comp.dwBytesPerInterval);
		return usb_endpoint_maxp(&ep->desc) * usb_endpoint_maxp_mult(&ep->desc);
	}
}
EXPORT_SYMBOL_GPL(usb_endpoint_max_periodic_payload);

/* drivers/usb/core/message.c */
int cdc_parse_cdc_header(struct usb_cdc_parsed_header *hdr, struct usb_interface *intf,
			 u8 *buffer, int buflen)
{
	/* duplicates are ignored */
	struct usb_cdc_union_desc *union_header = NULL;
	/* duplicates are not tolerated */
	struct usb_cdc_header_desc *header = NULL;
	struct usb_cdc_ether_desc *ether = NULL;
	struct usb_cdc_mdlm_detail_desc *detail = NULL;
	struct usb_cdc_mdlm_desc *desc = NULL;
	unsigned int elength;
	int cnt = 0;

	memset(hdr, 0x00, sizeof(struct usb_cdc_parsed_header));
	hdr->phonet_magic_present = false;
	while (buflen > 0) {
		elength = buffer[0];
		if (!elength) {
			dev_err(&intf->dev, "skipping garbage byte\n");
			elength = 1;
			goto next_desc;
		}
		if ((buflen < elength) || (elength < 3)) {
			dev_err(&intf->dev, "invalid descriptor buffer length\n");
			break;
		}
		if (buffer[1] != USB_DT_CS_INTERFACE) {
			dev_err(&intf->dev, "skipping garbage\n");
			goto next_desc;
		}
		switch (buffer[2]) {
		case USB_CDC_UNION_TYPE:
			if (elength < sizeof(struct usb_cdc_union_desc))
				goto next_desc;
			if (union_header) {
				dev_err(&intf->dev, "More than one union descriptor, skipping ...\n");
				goto next_desc;
			}
			union_header = (struct usb_cdc_union_desc *)buffer;
			break;
		case USB_CDC_COUNTRY_TYPE:
			if (elength < sizeof(struct usb_cdc_country_functional_desc))
				goto next_desc;
			hdr->usb_cdc_country_functional_desc =
				(struct usb_cdc_country_functional_desc *)buffer;
			break;
		case USB_CDC_HEADER_TYPE:
			if (elength != sizeof(struct usb_cdc_header_desc))
				goto next_desc;
			if (header)
				return -EINVAL;
			header = (struct usb_cdc_header_desc *)buffer;
			break;
		case USB_CDC_ACM_TYPE:
			if (elength < sizeof(struct usb_cdc_acm_descriptor))
				goto next_desc;
			hdr->usb_cdc_acm_descriptor = (struct usb_cdc_acm_descriptor *)buffer;
			break;
		case USB_CDC_ETHERNET_TYPE:
			if (elength != sizeof(struct usb_cdc_ether_desc))
				goto next_desc;
			if (ether)
				return -EINVAL;
			ether = (struct usb_cdc_ether_desc *)buffer;
			break;
		case USB_CDC_CALL_MANAGEMENT_TYPE:
			if (elength < sizeof(struct usb_cdc_call_mgmt_descriptor))
				goto next_desc;
			hdr->usb_cdc_call_mgmt_descriptor =
				(struct usb_cdc_call_mgmt_descriptor *)buffer;
			break;
		case USB_CDC_DMM_TYPE:
			if (elength < sizeof(struct usb_cdc_dmm_desc))
				goto next_desc;
			hdr->usb_cdc_dmm_desc = (struct usb_cdc_dmm_desc *)buffer;
			break;
		case USB_CDC_MDLM_TYPE:
			if (elength < sizeof(struct usb_cdc_mdlm_desc))
				goto next_desc;
			if (desc)
				return -EINVAL;
			desc = (struct usb_cdc_mdlm_desc *)buffer;
			break;
		case USB_CDC_MDLM_DETAIL_TYPE:
			if (elength < sizeof(struct usb_cdc_mdlm_detail_desc))
				goto next_desc;
			if (detail)
				return -EINVAL;
			detail = (struct usb_cdc_mdlm_detail_desc *)buffer;
			break;
		case USB_CDC_NCM_TYPE:
			if (elength < sizeof(struct usb_cdc_ncm_desc))
				goto next_desc;
			hdr->usb_cdc_ncm_desc = (struct usb_cdc_ncm_desc *)buffer;
			break;
		case USB_CDC_MBIM_TYPE:
			if (elength < sizeof(struct usb_cdc_mbim_desc))
				goto next_desc;
			hdr->usb_cdc_mbim_desc = (struct usb_cdc_mbim_desc *)buffer;
			break;
		case USB_CDC_MBIM_EXTENDED_TYPE:
			if (elength < sizeof(struct usb_cdc_mbim_extended_desc))
				goto next_desc;
			hdr->usb_cdc_mbim_extended_desc =
				(struct usb_cdc_mbim_extended_desc *)buffer;
			break;
		case CDC_PHONET_MAGIC_NUMBER:
			hdr->phonet_magic_present = true;
			break;
		default:
			dev_dbg(&intf->dev, "Ignoring descriptor: type %02x, length %ud\n",
				buffer[2], elength);
			goto next_desc;
		}
		cnt++;
next_desc:
		buflen -= elength;
		buffer += elength;
	}
	hdr->usb_cdc_union_desc = union_header;
	hdr->usb_cdc_header_desc = header;
	hdr->usb_cdc_mdlm_detail_desc = detail;
	hdr->usb_cdc_mdlm_desc = desc;
	hdr->usb_cdc_ether_desc = ether;
	return cnt;
}
EXPORT_SYMBOL(cdc_parse_cdc_header);

/* ------------------------------------------------- endpoints and settings */

static void kpi_enable_interface(struct usb_device *dev, struct usb_interface *intf)
{
	struct usb_host_interface *alt = intf->cur_altsetting;

	for (int i = 0; i < alt->desc.bNumEndpoints; i++) {
		struct usb_host_endpoint *ep = &alt->endpoint[i];
		int n = usb_endpoint_num(&ep->desc);

		if (usb_endpoint_dir_in(&ep->desc))
			dev->ep_in[n] = ep;
		else
			dev->ep_out[n] = ep;
		ep->enabled = 1;
	}
}

static void kpi_disable_interface(struct usb_device *dev, struct usb_interface *intf)
{
	struct usb_host_interface *alt = intf->cur_altsetting;

	for (int i = 0; i < alt->desc.bNumEndpoints; i++) {
		struct usb_host_endpoint *ep = &alt->endpoint[i];
		int n = usb_endpoint_num(&ep->desc);

		if (usb_endpoint_dir_in(&ep->desc)) {
			if (dev->ep_in[n] == ep)
				dev->ep_in[n] = NULL;
		} else if (dev->ep_out[n] == ep) {
			dev->ep_out[n] = NULL;
		}
		ep->enabled = 0;
	}
}

struct usb_interface *usb_ifnum_to_if(const struct usb_device *dev, unsigned ifnum)
{
	struct usb_host_config *config = dev->actconfig;

	if (!config)
		return NULL;
	for (int i = 0; i < config->desc.bNumInterfaces; i++)
		if (config->interface[i] &&
		    config->interface[i]->altsetting[0].desc.bInterfaceNumber == ifnum)
			return config->interface[i];
	return NULL;
}
EXPORT_SYMBOL_GPL(usb_ifnum_to_if);

struct usb_host_interface *usb_altnum_to_altsetting(const struct usb_interface *intf,
						    unsigned int altnum)
{
	for (int i = 0; i < intf->num_altsetting; i++)
		if (intf->altsetting[i].desc.bAlternateSetting == altnum)
			return &intf->altsetting[i];
	return NULL;
}
EXPORT_SYMBOL_GPL(usb_altnum_to_altsetting);

struct usb_host_interface *usb_find_alt_setting(struct usb_host_config *config,
						unsigned int iface_num, unsigned int alt_num)
{
	struct usb_interface_cache *intf_cache = NULL;

	if (!config)
		return NULL;
	for (int i = 0; i < config->desc.bNumInterfaces; i++)
		if (config->intf_cache[i]->altsetting[0].desc.bInterfaceNumber == iface_num) {
			intf_cache = config->intf_cache[i];
			break;
		}
	if (!intf_cache)
		return NULL;
	for (int i = 0; i < intf_cache->num_altsetting; i++)
		if (intf_cache->altsetting[i].desc.bAlternateSetting == alt_num)
			return &intf_cache->altsetting[i];
	return NULL;
}
EXPORT_SYMBOL_GPL(usb_find_alt_setting);

static bool kpi_match_endpoint(struct usb_endpoint_descriptor *epd,
			       struct usb_endpoint_descriptor **bulk_in,
			       struct usb_endpoint_descriptor **bulk_out,
			       struct usb_endpoint_descriptor **int_in,
			       struct usb_endpoint_descriptor **int_out)
{
	switch (usb_endpoint_type(epd)) {
	case USB_ENDPOINT_XFER_BULK:
		if (usb_endpoint_dir_in(epd)) {
			if (bulk_in && !*bulk_in) {
				*bulk_in = epd;
				break;
			}
		} else if (bulk_out && !*bulk_out) {
			*bulk_out = epd;
			break;
		}
		return false;
	case USB_ENDPOINT_XFER_INT:
		if (usb_endpoint_dir_in(epd)) {
			if (int_in && !*int_in) {
				*int_in = epd;
				break;
			}
		} else if (int_out && !*int_out) {
			*int_out = epd;
			break;
		}
		return false;
	default:
		return false;
	}
	return (!bulk_in || *bulk_in) && (!bulk_out || *bulk_out) &&
	       (!int_in || *int_in) && (!int_out || *int_out);
}

int usb_find_common_endpoints(struct usb_host_interface *alt,
				struct usb_endpoint_descriptor **bulk_in,
				struct usb_endpoint_descriptor **bulk_out,
				struct usb_endpoint_descriptor **int_in,
				struct usb_endpoint_descriptor **int_out)
{
	if (bulk_in)
		*bulk_in = NULL;
	if (bulk_out)
		*bulk_out = NULL;
	if (int_in)
		*int_in = NULL;
	if (int_out)
		*int_out = NULL;
	for (int i = 0; i < alt->desc.bNumEndpoints; i++)
		if (kpi_match_endpoint(&alt->endpoint[i].desc, bulk_in, bulk_out, int_in, int_out))
			return 0;
	return -ENXIO;
}
EXPORT_SYMBOL_GPL(usb_find_common_endpoints);

int usb_find_common_endpoints_reverse(struct usb_host_interface *alt,
					struct usb_endpoint_descriptor **bulk_in,
					struct usb_endpoint_descriptor **bulk_out,
					struct usb_endpoint_descriptor **int_in,
					struct usb_endpoint_descriptor **int_out)
{
	if (bulk_in)
		*bulk_in = NULL;
	if (bulk_out)
		*bulk_out = NULL;
	if (int_in)
		*int_in = NULL;
	if (int_out)
		*int_out = NULL;
	for (int i = alt->desc.bNumEndpoints - 1; i >= 0; i--)
		if (kpi_match_endpoint(&alt->endpoint[i].desc, bulk_in, bulk_out, int_in, int_out))
			return 0;
	return -ENXIO;
}
EXPORT_SYMBOL_GPL(usb_find_common_endpoints_reverse);

static const struct usb_host_endpoint *kpi_find_endpoint(const struct usb_interface *intf,
							 unsigned int ep_addr)
{
	const struct usb_host_interface *alt = intf->cur_altsetting;

	for (int n = 0; n < alt->desc.bNumEndpoints; n++)
		if (alt->endpoint[n].desc.bEndpointAddress == ep_addr)
			return &alt->endpoint[n];
	return NULL;
}

bool usb_check_bulk_endpoints(const struct usb_interface *intf, const u8 *ep_addrs)
{
	for (; *ep_addrs; ++ep_addrs) {
		const struct usb_host_endpoint *ep = kpi_find_endpoint(intf, *ep_addrs);

		if (!ep || !usb_endpoint_xfer_bulk(&ep->desc))
			return false;
	}
	return true;
}
EXPORT_SYMBOL_GPL(usb_check_bulk_endpoints);

bool usb_check_int_endpoints(const struct usb_interface *intf, const u8 *ep_addrs)
{
	for (; *ep_addrs; ++ep_addrs) {
		const struct usb_host_endpoint *ep = kpi_find_endpoint(intf, *ep_addrs);

		if (!ep || !usb_endpoint_xfer_int(&ep->desc))
			return false;
	}
	return true;
}
EXPORT_SYMBOL_GPL(usb_check_int_endpoints);

bool usb_endpoint_is_hs_isoc_double(struct usb_device *udev, const struct usb_host_endpoint *ep)
{
	return ep->eusb2_isoc_ep_comp.bDescriptorType &&
	       le16_to_cpu(udev->descriptor.bcdUSB) == 0x220 &&
	       usb_endpoint_is_isoc_in(&ep->desc) && !le16_to_cpu(ep->desc.wMaxPacketSize);
}
EXPORT_SYMBOL_GPL(usb_endpoint_is_hs_isoc_double);

/* drivers/usb/core/quirks.c: no endpoint quirks. */
bool usb_endpoint_is_ignored(struct usb_device *udev, struct usb_host_interface *intf,
			     struct usb_endpoint_descriptor *epd)
{
	return false;
}

int usb_set_interface(struct usb_device *dev, int ifnum, int alternate)
{
	struct usb_interface *iface = usb_ifnum_to_if(dev, ifnum);
	struct usb_host_interface *alt;
	int ret;

	if (dev->state == USB_STATE_NOTATTACHED)
		return -ENODEV;
	if (!iface)
		return -EINVAL;
	alt = usb_altnum_to_altsetting(iface, alternate);
	if (!alt)
		return -EINVAL;
	ret = rustos_kpi_usb_set_interface(kpi_handle(dev), ifnum, alternate, 1);
	if (ret < 0)
		return ret;
	kpi_disable_interface(dev, iface);
	iface->cur_altsetting = alt;
	kpi_enable_interface(dev, iface);
	return 0;
}
EXPORT_SYMBOL_GPL(usb_set_interface);

int usb_clear_halt(struct usb_device *dev, int pipe)
{
	u8 ep = usb_pipeendpoint(pipe) | (usb_pipein(pipe) ? USB_DIR_IN : 0);

	return rustos_kpi_usb_clear_halt(kpi_handle(dev), ep);
}
EXPORT_SYMBOL_GPL(usb_clear_halt);

void usb_reset_endpoint(struct usb_device *dev, unsigned int epaddr)
{
}
EXPORT_SYMBOL_GPL(usb_reset_endpoint);

int usb_reset_configuration(struct usb_device *dev)
{
	return 0;
}
EXPORT_SYMBOL_GPL(usb_reset_configuration);

/* Port resets are RustOS's hub driver's business: report success, so a
 * driver's recovery path carries on with the device as it is. */
int usb_reset_device(struct usb_device *udev)
{
	dev_info(&udev->dev, "device reset requested (not supported; continuing)\n");
	return 0;
}
EXPORT_SYMBOL_GPL(usb_reset_device);

void usb_queue_reset_device(struct usb_interface *iface)
{
}
EXPORT_SYMBOL_GPL(usb_queue_reset_device);

int usb_lock_device_for_reset(struct usb_device *udev, const struct usb_interface *iface)
{
	if (udev->state == USB_STATE_NOTATTACHED)
		return -ENODEV;
	device_lock(&udev->dev);
	return 0;
}
EXPORT_SYMBOL_GPL(usb_lock_device_for_reset);

int usb_driver_set_configuration(struct usb_device *udev, int config)
{
	return -EOPNOTSUPP;
}
EXPORT_SYMBOL_GPL(usb_driver_set_configuration);

/* Runtime power management (usb_autopm_*) is configured out: CONFIG_PM
 * is off, so the header's no-op versions apply. */

/* ------------------------------------------------------- reference counts */

struct usb_device *usb_get_dev(struct usb_device *dev)
{
	if (dev)
		get_device(&dev->dev);
	return dev;
}
EXPORT_SYMBOL_GPL(usb_get_dev);

void usb_put_dev(struct usb_device *dev)
{
	if (dev)
		put_device(&dev->dev);
}
EXPORT_SYMBOL_GPL(usb_put_dev);

struct usb_interface *usb_get_intf(struct usb_interface *intf)
{
	if (intf)
		get_device(&intf->dev);
	return intf;
}
EXPORT_SYMBOL_GPL(usb_get_intf);

void usb_put_intf(struct usb_interface *intf)
{
	if (intf)
		put_device(&intf->dev);
}
EXPORT_SYMBOL_GPL(usb_put_intf);

/* -------------------------------------------------------------- matching */

int usb_match_device(struct usb_device *dev, const struct usb_device_id *id)
{
	const struct usb_device_descriptor *d = &dev->descriptor;

	if ((id->match_flags & USB_DEVICE_ID_MATCH_VENDOR) &&
	    id->idVendor != le16_to_cpu(d->idVendor))
		return 0;
	if ((id->match_flags & USB_DEVICE_ID_MATCH_PRODUCT) &&
	    id->idProduct != le16_to_cpu(d->idProduct))
		return 0;
	if ((id->match_flags & USB_DEVICE_ID_MATCH_DEV_LO) &&
	    id->bcdDevice_lo > le16_to_cpu(d->bcdDevice))
		return 0;
	if ((id->match_flags & USB_DEVICE_ID_MATCH_DEV_HI) &&
	    id->bcdDevice_hi < le16_to_cpu(d->bcdDevice))
		return 0;
	if ((id->match_flags & USB_DEVICE_ID_MATCH_DEV_CLASS) &&
	    id->bDeviceClass != d->bDeviceClass)
		return 0;
	if ((id->match_flags & USB_DEVICE_ID_MATCH_DEV_SUBCLASS) &&
	    id->bDeviceSubClass != d->bDeviceSubClass)
		return 0;
	if ((id->match_flags & USB_DEVICE_ID_MATCH_DEV_PROTOCOL) &&
	    id->bDeviceProtocol != d->bDeviceProtocol)
		return 0;
	return 1;
}

int usb_match_one_id_intf(struct usb_device *dev, struct usb_host_interface *intf,
			  const struct usb_device_id *id)
{
	/* Interface fields of a vendor-specific device only count when the
	 * entry names the vendor (drivers/usb/core/driver.c). */
	if (dev->descriptor.bDeviceClass == USB_CLASS_VENDOR_SPEC &&
	    !(id->match_flags & USB_DEVICE_ID_MATCH_VENDOR) &&
	    (id->match_flags & (USB_DEVICE_ID_MATCH_INT_INFO | USB_DEVICE_ID_MATCH_INT_NUMBER)))
		return 0;
	if ((id->match_flags & USB_DEVICE_ID_MATCH_INT_CLASS) &&
	    id->bInterfaceClass != intf->desc.bInterfaceClass)
		return 0;
	if ((id->match_flags & USB_DEVICE_ID_MATCH_INT_SUBCLASS) &&
	    id->bInterfaceSubClass != intf->desc.bInterfaceSubClass)
		return 0;
	if ((id->match_flags & USB_DEVICE_ID_MATCH_INT_PROTOCOL) &&
	    id->bInterfaceProtocol != intf->desc.bInterfaceProtocol)
		return 0;
	if ((id->match_flags & USB_DEVICE_ID_MATCH_INT_NUMBER) &&
	    id->bInterfaceNumber != intf->desc.bInterfaceNumber)
		return 0;
	return 1;
}

int usb_match_one_id(struct usb_interface *interface, const struct usb_device_id *id)
{
	struct usb_device *dev;

	if (!id)
		return 0;
	dev = interface_to_usbdev(interface);
	if (!usb_match_device(dev, id))
		return 0;
	return usb_match_one_id_intf(dev, interface->cur_altsetting, id);
}
EXPORT_SYMBOL_GPL(usb_match_one_id);

const struct usb_device_id *usb_match_id(struct usb_interface *interface,
					 const struct usb_device_id *id)
{
	if (!id)
		return NULL;
	for (; id->idVendor || id->idProduct || id->bDeviceClass || id->bInterfaceClass ||
	       id->driver_info; id++)
		if (usb_match_one_id(interface, id))
			return id;
	return NULL;
}
EXPORT_SYMBOL_GPL(usb_match_id);

/* ------------------------------------------------------------- the bus */

static void kpi_usb_release_dev(struct device *dev)
{
	struct usb_device *udev = to_usb_device(dev);

	usb_destroy_configuration(udev);
	kfree(udev->product);
	kfree(udev->manufacturer);
	kfree(udev->serial);
	kfree(container_of(udev, struct kpi_usb_dev, udev));
}

static void kpi_usb_release_intf(struct device *dev)
{
	struct usb_interface *intf = to_usb_interface(dev);
	struct usb_interface_cache *intfc = altsetting_to_usb_interface_cache(intf->altsetting);

	kref_put(&intfc->ref, usb_release_interface_cache);
	usb_put_dev(interface_to_usbdev(intf));
	kfree(intf);
}

static int kpi_usb_dev_uevent(const struct device *dev, struct kobj_uevent_env *env)
{
	const struct usb_device *udev = to_usb_device(dev);

	return add_uevent_var(env, "PRODUCT=%x/%x/%x", le16_to_cpu(udev->descriptor.idVendor),
			      le16_to_cpu(udev->descriptor.idProduct),
			      le16_to_cpu(udev->descriptor.bcdDevice));
}

const struct device_type usb_device_type = {
	.name = "usb_device",
	.release = kpi_usb_release_dev,
	.uevent = kpi_usb_dev_uevent,
};

static int kpi_usb_if_uevent(const struct device *dev, struct kobj_uevent_env *env)
{
	const struct usb_interface *intf = to_usb_interface(dev);
	const struct usb_device *udev = interface_to_usbdev(intf);
	const struct usb_host_interface *alt = intf->cur_altsetting;

	return add_uevent_var(env, "MODALIAS=usb:v%04Xp%04Xd%04Xdc%02Xdsc%02Xdp%02Xic%02Xisc%02Xip%02Xin%02X",
			      le16_to_cpu(udev->descriptor.idVendor),
			      le16_to_cpu(udev->descriptor.idProduct),
			      le16_to_cpu(udev->descriptor.bcdDevice),
			      udev->descriptor.bDeviceClass, udev->descriptor.bDeviceSubClass,
			      udev->descriptor.bDeviceProtocol, alt->desc.bInterfaceClass,
			      alt->desc.bInterfaceSubClass, alt->desc.bInterfaceProtocol,
			      alt->desc.bInterfaceNumber);
}

const struct device_type usb_if_device_type = {
	.name = "usb_interface",
	.release = kpi_usb_release_intf,
	.uevent = kpi_usb_if_uevent,
};

static int kpi_usb_bus_match(struct device *dev, const struct device_driver *drv)
{
	struct usb_interface *intf;
	struct usb_driver *udrv;

	/* Only interface drivers (usb_register_driver()) exist here. */
	if (dev->type != &usb_if_device_type)
		return 0;
	intf = to_usb_interface(dev);
	udrv = to_usb_driver(drv);
	return usb_match_id(intf, udrv->id_table) != NULL;
}

static int kpi_usb_bus_probe(struct device *dev)
{
	struct usb_driver *driver = to_usb_driver(dev->driver);
	struct usb_interface *intf = to_usb_interface(dev);
	struct usb_device *udev = interface_to_usbdev(intf);
	const struct usb_device_id *id;
	int err;

	if (udev->state == USB_STATE_NOTATTACHED)
		return -ENODEV;
	id = usb_match_id(intf, driver->id_table);
	if (!id)
		return -ENODEV;
	intf->condition = USB_INTERFACE_BINDING;
	if (intf->needs_altsetting0) {
		err = usb_set_interface(udev, intf->altsetting[0].desc.bInterfaceNumber, 0);
		if (err < 0)
			goto fail;
		intf->needs_altsetting0 = 0;
	}
	err = driver->probe(intf, id);
	if (err)
		goto fail;
	intf->condition = USB_INTERFACE_BOUND;
	return 0;
fail:
	usb_set_intfdata(intf, NULL);
	intf->needs_remote_wakeup = 0;
	intf->condition = USB_INTERFACE_UNBOUND;
	return err;
}

static void kpi_usb_bus_remove(struct device *dev)
{
	struct usb_driver *driver = to_usb_driver(dev->driver);
	struct usb_interface *intf = to_usb_interface(dev);

	intf->condition = USB_INTERFACE_UNBINDING;
	if (driver->disconnect)
		driver->disconnect(intf);
	usb_set_intfdata(intf, NULL);
	intf->condition = USB_INTERFACE_UNBOUND;
	intf->needs_remote_wakeup = 0;
	if (intf->cur_altsetting->desc.bAlternateSetting != 0)
		intf->needs_altsetting0 = 1;
}

static void kpi_usb_bus_shutdown(struct device *dev)
{
	struct usb_interface *intf;
	struct usb_driver *driver;

	if (dev->type != &usb_if_device_type || !dev->driver)
		return;
	intf = to_usb_interface(dev);
	driver = to_usb_driver(dev->driver);
	if (driver->shutdown)
		driver->shutdown(intf);
}

const struct bus_type usb_bus_type = {
	.name = "usb",
	.match = kpi_usb_bus_match,
	.probe = kpi_usb_bus_probe,
	.remove = kpi_usb_bus_remove,
	.shutdown = kpi_usb_bus_shutdown,
};
EXPORT_SYMBOL_GPL(usb_bus_type);

int usb_register_driver(struct usb_driver *new_driver, struct module *owner,
			const char *mod_name)
{
	int err;

	new_driver->driver.name = new_driver->name;
	new_driver->driver.bus = &usb_bus_type;
	new_driver->driver.owner = owner;
	new_driver->driver.mod_name = mod_name;
	err = driver_register(&new_driver->driver);
	if (!err)
		pr_info("usbcore: registered new interface driver %s\n", new_driver->name);
	return err;
}
EXPORT_SYMBOL_GPL(usb_register_driver);

/* USB is never disabled on the command line; IDs are not added at run
 * time (no new_id sysfs writes). */
int usb_disabled(void)
{
	return 0;
}
EXPORT_SYMBOL_GPL(usb_disabled);

DEFINE_MUTEX(usb_dynids_lock);

ssize_t usb_show_dynids(struct usb_dynids *dynids, char *buf)
{
	return 0;
}
EXPORT_SYMBOL_GPL(usb_show_dynids);

ssize_t usb_store_new_id(struct usb_dynids *dynids, const struct usb_device_id *id_table,
			 struct device_driver *driver, const char *buf, size_t count)
{
	return -EOPNOTSUPP;
}
EXPORT_SYMBOL_GPL(usb_store_new_id);

/* drivers/usb/core/usb.c */
int __usb_get_extra_descriptor(char *buffer, unsigned size, unsigned char type, void **ptr,
			       size_t minsize)
{
	struct usb_descriptor_header *header;

	while (size >= sizeof(struct usb_descriptor_header)) {
		header = (struct usb_descriptor_header *)buffer;
		if (header->bLength < 2 || header->bLength > size)
			return -1;
		if (header->bDescriptorType == type && header->bLength >= minsize) {
			*ptr = header;
			return 0;
		}
		buffer += header->bLength;
		size -= header->bLength;
	}
	return -1;
}
EXPORT_SYMBOL_GPL(__usb_get_extra_descriptor);

/* Device-level drivers (r8152's configuration selector) are not run:
 * RustOS selects the configuration (src/usb/mod.rs prefers the vendor one
 * of Realtek adapters in linux-usbnet builds, as r8152's selector does). */
int usb_register_device_driver(struct usb_device_driver *new_udriver, struct module *owner)
{
	return 0;
}
EXPORT_SYMBOL_GPL(usb_register_device_driver);

void usb_deregister_device_driver(struct usb_device_driver *udriver)
{
}
EXPORT_SYMBOL_GPL(usb_deregister_device_driver);

/* Link power management stays as the controller set it. */
void usb_enable_lpm(struct usb_device *udev)
{
}
EXPORT_SYMBOL_GPL(usb_enable_lpm);

void usb_deregister(struct usb_driver *driver)
{
	driver_unregister(&driver->driver);
}
EXPORT_SYMBOL_GPL(usb_deregister);

int usb_driver_claim_interface(struct usb_driver *driver, struct usb_interface *iface,
			       void *data)
{
	struct device *dev = &iface->dev;
	struct usb_device *udev = interface_to_usbdev(iface);
	int ret = 0;

	if (dev->driver)
		return -EBUSY;
	/* The RustOS side must not use it either; enable its endpoints. */
	if (!rustos_kpi_usb_claim(kpi_handle(udev), iface->altsetting[0].desc.bInterfaceNumber))
		return -EBUSY;
	dev->driver = &driver->driver;
	usb_set_intfdata(iface, data);
	iface->needs_binding = 0;
	iface->condition = USB_INTERFACE_BOUND;
	if (device_is_registered(dev))
		ret = device_bind_driver(dev);
	if (ret) {
		dev->driver = NULL;
		usb_set_intfdata(iface, NULL);
		iface->condition = USB_INTERFACE_UNBOUND;
	}
	return ret;
}
EXPORT_SYMBOL_GPL(usb_driver_claim_interface);

void usb_driver_release_interface(struct usb_driver *driver, struct usb_interface *iface)
{
	struct device *dev = &iface->dev;

	if (!dev->driver || dev->driver != &driver->driver)
		return;
	if (iface->condition != USB_INTERFACE_BOUND)
		return;
	iface->condition = USB_INTERFACE_UNBINDING;
	if (device_is_registered(dev)) {
		device_release_driver(dev);
	} else {
		device_lock(dev);
		kpi_usb_bus_remove(dev);
		dev->driver = NULL;
		device_unlock(dev);
	}
}
EXPORT_SYMBOL_GPL(usb_driver_release_interface);

/* ------------------------------------------------- devices from RustOS */

static struct usb_bus *kpi_bus(u32 busnum)
{
	struct usb_bus *bus;

	if (!busnum || busnum > KPI_USB_BUSES)
		return NULL;
	bus = kpi_usb_bus[busnum - 1];
	if (bus)
		return bus;
	bus = kzalloc(sizeof(*bus), GFP_KERNEL);
	if (!bus)
		return NULL;
	bus->busnum = busnum;
	bus->bus_name = kasprintf(GFP_KERNEL, "xhci-%u", busnum);
	/* Transfers are bounced through RustOS DMA buffers: no SG lists. */
	bus->sg_tablesize = 0;
	bus->no_sg_constraint = 0;
	kpi_usb_bus[busnum - 1] = bus;
	return bus;
}

static struct usb_device *kpi_find(u64 handle)
{
	return (struct usb_device *)rustos_kpi_usb_cookie(handle);
}

/* A device RustOS enumerated and configured with configuration `cfgval`. */
int kpi_usb_device_add(u64 handle, u32 busnum, u32 devnum, u32 speed, u32 port, u32 cfgval)
{
	struct kpi_usb_dev *k;
	struct usb_device *udev;
	struct usb_device_descriptor *desc;
	int err;

	if (!kpi_usb_root)
		return -ENODEV;
	k = kzalloc(sizeof(*k), GFP_KERNEL);
	if (!k)
		return -ENOMEM;
	k->handle = handle;
	udev = &k->udev;
	device_initialize(&udev->dev);
	udev->dev.bus = &usb_bus_type;
	udev->dev.type = &usb_device_type;
	udev->dev.parent = kpi_usb_root;
	udev->dev.dma_mask = &udev->dev.coherent_dma_mask;
	udev->dev.coherent_dma_mask = DMA_BIT_MASK(64);
	udev->bus = kpi_bus(busnum);
	udev->devnum = devnum;
	udev->speed = speed;
	udev->portnum = port;
	udev->level = 1;
	udev->route = port;
	snprintf(udev->devpath, sizeof(udev->devpath), "%u", port);
	udev->state = USB_STATE_CONFIGURED;
	udev->authorized = 1;
	udev->can_submit = 1;
	udev->slot_id = devnum;
	INIT_LIST_HEAD(&udev->ep0.urb_list);
	udev->ep0.desc.bLength = USB_DT_ENDPOINT_SIZE;
	udev->ep0.desc.bDescriptorType = USB_DT_ENDPOINT;
	udev->ep0.desc.wMaxPacketSize = cpu_to_le16(speed >= USB_SPEED_SUPER ? 512 : 64);
	udev->ep0.enabled = 1;
	udev->ep_in[0] = udev->ep_out[0] = &udev->ep0;
	dev_set_name(&udev->dev, "%u-%s", busnum, udev->devpath);
	rustos_kpi_usb_set_cookie(handle, udev);

	desc = kmalloc(sizeof(*desc), GFP_KERNEL);
	err = desc ? usb_get_descriptor(udev, USB_DT_DEVICE, 0, desc, sizeof(*desc)) : -ENOMEM;
	if (err == sizeof(*desc)) {
		udev->descriptor = *desc;
		err = 0;
	} else if (err >= 0) {
		err = -EIO;
	}
	kfree(desc);
	if (!err) {
		udev->ep0.desc.wMaxPacketSize = cpu_to_le16(
			speed >= USB_SPEED_SUPER ? 1 << udev->descriptor.bMaxPacketSize0
						 : udev->descriptor.bMaxPacketSize0);
		err = usb_get_configuration(udev);
	}
	if (!err) {
		for (int i = 0; i < udev->descriptor.bNumConfigurations; i++)
			if (udev->config[i].desc.bConfigurationValue == cfgval)
				udev->actconfig = &udev->config[i];
		if (!udev->actconfig)
			err = -EINVAL;
	}
	if (err) {
		rustos_kpi_usb_set_cookie(handle, NULL);
		put_device(&udev->dev);
		return err;
	}
	udev->product = usb_cache_string(udev, udev->descriptor.iProduct);
	udev->manufacturer = usb_cache_string(udev, udev->descriptor.iManufacturer);
	udev->serial = usb_cache_string(udev, udev->descriptor.iSerialNumber);

	for (int i = 0; i < udev->actconfig->desc.bNumInterfaces; i++) {
		struct usb_interface_cache *intfc = udev->actconfig->intf_cache[i];
		struct usb_interface *intf = kzalloc(sizeof(*intf), GFP_KERNEL);
		struct usb_host_interface *alt;
		int ifnum;

		udev->actconfig->interface[i] = intf;
		if (!intf)
			continue;
		kref_get(&intfc->ref);
		intf->altsetting = intfc->altsetting;
		intf->num_altsetting = intfc->num_altsetting;
		ifnum = intf->altsetting[0].desc.bInterfaceNumber;
		for (int j = 0; j < USB_MAXIADS && udev->actconfig->intf_assoc[j]; j++) {
			struct usb_interface_assoc_descriptor *iad = udev->actconfig->intf_assoc[j];

			if (ifnum >= iad->bFirstInterface &&
			    ifnum < iad->bFirstInterface + iad->bInterfaceCount) {
				intf->intf_assoc = iad;
				break;
			}
		}
		alt = usb_altnum_to_altsetting(intf, 0);
		intf->cur_altsetting = alt ? alt : &intf->altsetting[0];
		intf->minor = -1;
		intf->authorized = 1;
		intf->dev.parent = &udev->dev;
		intf->dev.bus = &usb_bus_type;
		intf->dev.type = &usb_if_device_type;
		intf->dev.dma_mask = udev->dev.dma_mask;
		intf->dev.coherent_dma_mask = udev->dev.coherent_dma_mask;
		intf->usb_dev = &udev->dev;
		device_initialize(&intf->dev);
		usb_get_dev(udev);
		dev_set_name(&intf->dev, "%u-%s:%d.%d", busnum, udev->devpath, cfgval, ifnum);
		kpi_enable_interface(udev, intf);
	}
	err = device_add(&udev->dev);
	if (err) {
		dev_err(&udev->dev, "cannot add to the device core: %d\n", err);
		return err;
	}
	dev_info(&udev->dev, "%04x:%04x %s %s (%s)\n", le16_to_cpu(udev->descriptor.idVendor),
		 le16_to_cpu(udev->descriptor.idProduct),
		 udev->manufacturer ? udev->manufacturer : "",
		 udev->product ? udev->product : "", usb_speed_string(udev->speed));
	return 0;
}

/* Offer interface `ifnum` to Linux drivers: 1 if one is bound to it. */
int kpi_usb_probe_interface(u64 handle, u32 ifnum)
{
	struct usb_device *udev = kpi_find(handle);
	struct usb_interface *intf = udev ? usb_ifnum_to_if(udev, ifnum) : NULL;

	if (!intf)
		return 0;
	if (!device_is_registered(&intf->dev)) {
		/* A claimed interface (usb_driver_claim_interface()) is set up. */
		if (!intf->dev.driver)
			rustos_kpi_usb_set_interface(handle, ifnum,
						     intf->cur_altsetting->desc.bAlternateSetting, 0);
		if (device_add(&intf->dev)) {
			dev_err(&intf->dev, "cannot add to the device core\n");
			return 0;
		}
	} else if (!intf->dev.driver) {
		if (device_attach(&intf->dev) < 0)
			return 0;
	}
	if (!intf->dev.driver)
		return 0;
	/* Network interfaces the driver registered come up like boot-time
	 * ones (hot-plugged adapters included). */
	kpi_netdev_open_pending();
	return 1;
}

/* Unplugged: unbind the drivers (their URBs fail with -ESHUTDOWN). */
void kpi_usb_device_remove(u64 handle)
{
	struct usb_device *udev = kpi_find(handle);
	struct usb_host_config *config;

	if (!udev)
		return;
	udev->state = USB_STATE_NOTATTACHED;
	config = udev->actconfig;
	for (int i = 0; config && i < config->desc.bNumInterfaces; i++) {
		struct usb_interface *intf = config->interface[i];

		if (!intf)
			continue;
		if (device_is_registered(&intf->dev))
			device_del(&intf->dev);
		else if (intf->dev.driver)
			usb_driver_release_interface(to_usb_driver(intf->dev.driver), intf);
		config->interface[i] = NULL;
		put_device(&intf->dev);
	}
	rustos_kpi_usb_set_cookie(handle, NULL);
	device_del(&udev->dev);
	put_device(&udev->dev);
}

int kpi_usb_bus_init(void)
{
	int err = bus_register(&usb_bus_type);

	if (err)
		return err;
	kpi_usb_root = root_device_register("usb");
	if (IS_ERR(kpi_usb_root)) {
		err = PTR_ERR(kpi_usb_root);
		kpi_usb_root = NULL;
	}
	return err;
}
