// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * /dev/kpi-test: a Linux misc driver, built with the linux-test feature,
 * that exercises LinuxKPI's character-device bridge from user space
 * (userland/musltest/kpitest.c):
 *  - read returns the last write ("linuxkpi\n" initially);
 *  - KPI_TEST_IOC_GET writes 42 + arg through a user pointer;
 *  - poll reports EPOLLIN once KPI_TEST_IOC_ARM's timer fires (50 ms);
 *  - mmap offset 0 maps a page with remap_pfn_range, offset 1 page
 *    through a vm_ops fault handler (vmf_insert_pfn).
 */
#include <linux/fs.h>
#include <linux/miscdevice.h>
#include <linux/mm.h>
#include <linux/module.h>
#include <linux/poll.h>
#include <linux/slab.h>
#include <linux/timer.h>
#include <linux/uaccess.h>

#define KPI_TEST_IOC_GET	_IOWR('k', 1, int)
#define KPI_TEST_IOC_ARM	_IO('k', 2)

static char msg[64] = "linuxkpi\n";
static size_t msg_len = 9;
static DEFINE_MUTEX(msg_lock);
static DECLARE_WAIT_QUEUE_HEAD(ready_wq);
static bool ready;
static struct timer_list ready_timer;
static struct page *map_pages[2];

static ssize_t kpi_test_read(struct file *f, char __user *buf, size_t len, loff_t *pos)
{
	ssize_t r;

	mutex_lock(&msg_lock);
	r = simple_read_from_buffer(buf, len, pos, msg, msg_len);
	mutex_unlock(&msg_lock);
	return r;
}

static ssize_t kpi_test_write(struct file *f, const char __user *buf, size_t len, loff_t *pos)
{
	len = min(len, sizeof(msg));
	mutex_lock(&msg_lock);
	if (copy_from_user(msg, buf, len)) {
		mutex_unlock(&msg_lock);
		return -EFAULT;
	}
	msg_len = len;
	mutex_unlock(&msg_lock);
	return len;
}

static void kpi_test_timer(struct timer_list *t)
{
	WRITE_ONCE(ready, true);
	wake_up_interruptible(&ready_wq);
}

static long kpi_test_ioctl(struct file *f, unsigned int cmd, unsigned long arg)
{
	int v;

	switch (cmd) {
	case KPI_TEST_IOC_GET:
		if (copy_from_user(&v, (int __user *)arg, sizeof(v)))
			return -EFAULT;
		v += 42;
		return copy_to_user((int __user *)arg, &v, sizeof(v)) ? -EFAULT : 0;
	case KPI_TEST_IOC_ARM:
		WRITE_ONCE(ready, false);
		mod_timer(&ready_timer, jiffies + msecs_to_jiffies(50));
		return 0;
	default:
		return -ENOTTY;
	}
}

static __poll_t kpi_test_poll(struct file *f, poll_table *wait)
{
	poll_wait(f, &ready_wq, wait);
	return READ_ONCE(ready) ? EPOLLIN | EPOLLRDNORM : 0;
}

static vm_fault_t kpi_test_fault(struct vm_fault *vmf)
{
	if (vmf->pgoff != 1)
		return VM_FAULT_SIGBUS;
	return vmf_insert_pfn(vmf->vma, vmf->address, page_to_pfn(map_pages[1]));
}

static const struct vm_operations_struct kpi_test_vm_ops = {
	.fault = kpi_test_fault,
};

static int kpi_test_mmap(struct file *f, struct vm_area_struct *vma)
{
	if (vma->vm_end - vma->vm_start != PAGE_SIZE)
		return -EINVAL;
	if (vma->vm_pgoff == 0)
		return remap_pfn_range(vma, vma->vm_start, page_to_pfn(map_pages[0]), PAGE_SIZE,
				       vma->vm_page_prot);
	vma->vm_ops = &kpi_test_vm_ops;
	return 0;
}

static const struct file_operations kpi_test_fops = {
	.owner = THIS_MODULE,
	.read = kpi_test_read,
	.write = kpi_test_write,
	.unlocked_ioctl = kpi_test_ioctl,
	.poll = kpi_test_poll,
	.mmap = kpi_test_mmap,
};

static struct miscdevice kpi_test_dev = {
	.minor = MISC_DYNAMIC_MINOR,
	.name = "kpi-test",
	.fops = &kpi_test_fops,
};

static int __init kpi_test_init(void)
{
	for (int i = 0; i < 2; i++) {
		map_pages[i] = alloc_page(GFP_KERNEL | __GFP_ZERO);
		if (!map_pages[i])
			return -ENOMEM;
		snprintf(page_address(map_pages[i]), 32, "kpi page %d", i);
	}
	timer_setup(&ready_timer, kpi_test_timer, 0);
	return misc_register(&kpi_test_dev);
}
module_init(kpi_test_init);
MODULE_LICENSE("GPL");
