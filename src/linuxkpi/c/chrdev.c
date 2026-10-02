// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * Character devices and files for LinuxKPI: cdevs, register_chrdev,
 * misc devices, device nodes (devtmpfs_create_node), struct file with
 * read/write/ioctl/poll/mmap/release, and anon-inode files. RustOS's side
 * is src/linuxkpi/chrdev.rs: device nodes in devfs open through
 * kpi_chrdev_open(), and each open file is a RustOS FileLike wrapping
 * the struct file.
 *
 * read() and write() get RustOS's kernel bounce buffer: copy_to_user()
 * and copy_from_user() accept exactly that range for the duration of the
 * call (kpi_uaccess_*); pointers passed in ioctl arguments are checked
 * against the calling process as usual.
 */
#include <linux/anon_inodes.h>
#include <linux/cdev.h>
#include <linux/device.h>
#include <linux/file.h>
#include <linux/fs.h>
#include <linux/kdev_t.h>
#include <linux/miscdevice.h>
#include <linux/mm.h>
#include <linux/mman.h>
#include <linux/poll.h>
#include <linux/pseudo_fs.h>
#include <linux/fs_context.h>
#include <linux/mount.h>
#include <linux/slab.h>
#include <linux/uaccess.h>
#include "kpi.h"

/* ---------------------------------------------- kernel-buffer uaccess */

/*
 * The task's sigaltstack fields are unused by LinuxKPI tasks: they hold
 * the kernel buffer that copy_*_user() may touch during a read()/write().
 */
static void kpi_uaccess_begin(void *buf, size_t len)
{
	current->sas_ss_sp = (unsigned long)buf;
	current->sas_ss_size = len;
}

static void kpi_uaccess_end(void)
{
	current->sas_ss_sp = 0;
	current->sas_ss_size = 0;
}

/* Whether [addr, addr + n) lies in the current kernel buffer. */
bool kpi_uaccess_kernel(const void *addr, unsigned long n)
{
	unsigned long a = (unsigned long)addr, base = current->sas_ss_sp;

	return base && a >= base && n <= current->sas_ss_size &&
	       a - base <= current->sas_ss_size - n;
}

/* ------------------------------------------------------------- files */

struct kpi_file {
	struct file file;
	struct inode inode;
	spinlock_t lock;
	struct list_head polls;		/* struct kpi_poll_entry */
	void *waitq;			/* RustOS wait queue to wake on poll events */
};

struct kpi_poll_entry {
	struct list_head list;
	wait_queue_head_t *head;
	struct wait_queue_entry wait;
	struct kpi_file *kf;
};

static struct kpi_file *to_kf(struct file *f)
{
	return container_of(f, struct kpi_file, file);
}

static struct kpi_file *kpi_file_alloc(const struct file_operations *fops, unsigned int flags,
				       dev_t devt)
{
	struct kpi_file *kf = kzalloc(sizeof(*kf), GFP_KERNEL);

	if (!kf)
		return NULL;
	file_ref_init(&kf->file.f_ref, 1);
	spin_lock_init(&kf->file.f_lock);
	spin_lock_init(&kf->lock);
	INIT_LIST_HEAD(&kf->polls);
	kf->file.f_op = fops;
	kf->file.f_flags = flags;
	kf->file.f_mode = OPEN_FMODE(flags) | FMODE_LSEEK | FMODE_PREAD | FMODE_PWRITE;
	kf->file.f_inode = &kf->inode;
	kf->inode.i_rdev = devt;
	kf->inode.i_mode = S_IFCHR | 0666;
	mutex_init(&kf->file.f_pos_lock);
	return kf;
}

static void kpi_file_release(struct file *file)
{
	struct kpi_file *kf = to_kf(file);
	struct kpi_poll_entry *e, *n;

	list_for_each_entry_safe(e, n, &kf->polls, list) {
		remove_wait_queue(e->head, &e->wait);
		kfree(e);
	}
	if (file->f_op && file->f_op->release)
		file->f_op->release(file->f_inode, file);
	if (file->f_path.dentry) {
		/* alloc_file_pseudo(): the dentry and inode go with the file. */
		struct dentry *d = file->f_path.dentry;

		if (d->d_op && d->d_op->d_release)
			d->d_op->d_release(d);
		iput(d->d_inode);
		kfree(d);
	}
	kfree(kf);
}

bool __file_ref_put(file_ref_t *ref, unsigned long cnt)
{
	/* The last reference: mark the count dead so no get() revives it. */
	if (cnt == FILE_REF_NOREF)
		return atomic_long_try_cmpxchg_release(&ref->refcnt, &cnt, FILE_REF_DEAD);
	if (cnt > FILE_REF_MAXREF)
		atomic_long_set(&ref->refcnt, FILE_REF_DEAD);
	return false;
}

void fput(struct file *file)
{
	if (file && file_ref_put(&file->f_ref))
		kpi_file_release(file);
}

struct file *fget(unsigned int fd)
{
	struct file *f = rustos_kpi_fd_file(fd);

	return f ? get_file(f) : NULL;
}

/* ----------------------------------------- file helpers (fs/libfs.c etc.) */

ssize_t simple_read_from_buffer(void __user *to, size_t count, loff_t *ppos, const void *from,
				size_t available)
{
	loff_t pos = *ppos;
	size_t ret;

	if (pos < 0)
		return -EINVAL;
	if (pos >= available || !count)
		return 0;
	if (count > available - pos)
		count = available - pos;
	ret = copy_to_user(to, from + pos, count);
	if (ret == count)
		return -EFAULT;
	count -= ret;
	*ppos = pos + count;
	return count;
}

ssize_t simple_write_to_buffer(void *to, size_t available, loff_t *ppos,
			       const void __user *from, size_t count)
{
	loff_t pos = *ppos;
	size_t res;

	if (pos < 0)
		return -EINVAL;
	if (pos >= available || !count)
		return 0;
	if (count > available - pos)
		count = available - pos;
	res = copy_from_user(to + pos, from, count);
	if (res == count)
		return -EFAULT;
	count -= res;
	*ppos = pos + count;
	return count;
}

loff_t noop_llseek(struct file *file, loff_t offset, int whence)
{
	return file->f_pos;
}

loff_t default_llseek(struct file *file, loff_t offset, int whence)
{
	switch (whence) {
	case SEEK_SET:
		break;
	case SEEK_CUR:
		offset += file->f_pos;
		break;
	default:
		return -EINVAL;
	}
	if (offset < 0)
		return -EINVAL;
	file->f_pos = offset;
	return offset;
}

int nonseekable_open(struct inode *inode, struct file *filp)
{
	filp->f_mode &= ~(FMODE_LSEEK | FMODE_PREAD | FMODE_PWRITE);
	return 0;
}

int stream_open(struct inode *inode, struct file *filp)
{
	filp->f_mode &= ~(FMODE_LSEEK | FMODE_PREAD | FMODE_PWRITE | FMODE_ATOMIC_POS);
	filp->f_mode |= FMODE_STREAM;
	return 0;
}

/* ------------------------------------------------- file ops for RustOS */

ssize_t kpi_file_read(struct file *file, char *buf, size_t len, int nonblock)
{
	ssize_t r;

	if (!file->f_op->read)
		return -EINVAL;
	if (nonblock)
		file->f_flags |= O_NONBLOCK;
	else
		file->f_flags &= ~O_NONBLOCK;
	kpi_uaccess_begin(buf, len);
	r = file->f_op->read(file, (char __user *)buf, len, &file->f_pos);
	kpi_uaccess_end();
	return r;
}

ssize_t kpi_file_write(struct file *file, const char *buf, size_t len, int nonblock)
{
	ssize_t r;

	if (!file->f_op->write)
		return -EINVAL;
	if (nonblock)
		file->f_flags |= O_NONBLOCK;
	else
		file->f_flags &= ~O_NONBLOCK;
	kpi_uaccess_begin((void *)buf, len);
	r = file->f_op->write(file, (const char __user *)buf, len, &file->f_pos);
	kpi_uaccess_end();
	return r;
}

long kpi_file_ioctl(struct file *file, unsigned int cmd, unsigned long arg)
{
	if (file->f_op->unlocked_ioctl)
		return file->f_op->unlocked_ioctl(file, cmd, arg);
	return -ENOTTY;
}

static int kpi_poll_wake(struct wait_queue_entry *wait, unsigned int mode, int sync, void *key)
{
	struct kpi_poll_entry *e = container_of(wait, struct kpi_poll_entry, wait);

	rustos_kpi_waitq_wake(e->kf->waitq);
	return 0;
}

/* poll_wait(): hook the RustOS wait queue onto the driver's queue, once. */
static void kpi_poll_queue(struct file *file, wait_queue_head_t *head, poll_table *pt)
{
	struct kpi_file *kf = to_kf(file);
	struct kpi_poll_entry *e;
	unsigned long flags;

	spin_lock_irqsave(&kf->lock, flags);
	list_for_each_entry(e, &kf->polls, list) {
		if (e->head == head) {
			spin_unlock_irqrestore(&kf->lock, flags);
			return;
		}
	}
	spin_unlock_irqrestore(&kf->lock, flags);
	e = kzalloc(sizeof(*e), GFP_KERNEL);
	if (!e)
		return;
	e->head = head;
	e->kf = kf;
	init_waitqueue_func_entry(&e->wait, kpi_poll_wake);
	add_wait_queue(head, &e->wait);
	spin_lock_irqsave(&kf->lock, flags);
	list_add(&e->list, &kf->polls);
	spin_unlock_irqrestore(&kf->lock, flags);
}

/* Returns the EPOLL* mask; registers wake-ups for @waitq. */
unsigned int kpi_file_poll(struct file *file, void *waitq)
{
	poll_table pt;

	if (!file->f_op->poll)
		return EPOLLIN | EPOLLOUT | EPOLLRDNORM | EPOLLWRNORM;
	to_kf(file)->waitq = waitq;
	init_poll_funcptr(&pt, kpi_poll_queue);
	return file->f_op->poll(file, &pt);
}

void kpi_file_put(struct file *file)
{
	fput(file);
}

/* ----------------------------------------------------------------- mmap */

/*
 * A mapping in progress or alive: the vma handed to the driver. Drivers
 * either remap_pfn_range() a contiguous range, or install vm_ops with a
 * fault handler that inserts pages one at a time.
 */
struct kpi_vma {
	struct vm_area_struct vma;
	u64 pfn;		/* remap_pfn_range() base, or last inserted page */
	pgprot_t prot;
	bool remapped;
	bool inserted;
	void *vmalloc_base;	/* remap_vmalloc_range(): pages looked up on fault */
};

static struct kpi_vma *to_kv(struct vm_area_struct *vma)
{
	return container_of(vma, struct kpi_vma, vma);
}

/* RustOS memory type (0 UC, 1 WC, 2 WB) of a pgprot. */
static int kpi_prot_cache(pgprot_t prot)
{
	unsigned long v = pgprot_val(prot);

	if (v & _PAGE_PCD)
		return 0;
	if (v & _PAGE_PWT)
		return 1;
	return 2;
}

int remap_pfn_range(struct vm_area_struct *vma, unsigned long addr, unsigned long pfn,
		    unsigned long size, pgprot_t prot)
{
	struct kpi_vma *kv = to_kv(vma);

	if (addr != vma->vm_start || size < vma->vm_end - vma->vm_start)
		return -EINVAL;	/* only whole-vma remaps */
	kv->pfn = pfn;
	kv->prot = prot;
	kv->remapped = true;
	return 0;
}

/* vmalloc memory is physically scattered: its pages are mapped as user
 * space touches them (kpi_vma_fault). */
int remap_vmalloc_range(struct vm_area_struct *vma, void *addr, unsigned long pgoff)
{
	if (!is_vmalloc_addr(addr) || !PAGE_ALIGNED(addr))
		return -EINVAL;
	to_kv(vma)->vmalloc_base = addr + (pgoff << PAGE_SHIFT);
	vm_flags_set(vma, VM_DONTEXPAND | VM_DONTDUMP);
	return 0;
}

/* Pages for a whole vma: physically contiguous runs only (ALSA buffers
 * are allocated contiguous here). */
int vm_map_pages(struct vm_area_struct *vma, struct page **pages, unsigned long num)
{
	unsigned long count = vma_pages(vma), off = vma->vm_pgoff;

	if (off >= num || count > num - off)
		return -ENXIO;
	for (unsigned long i = 1; i < count; i++)
		if (page_to_pfn(pages[off + i]) != page_to_pfn(pages[off]) + i)
			return -EINVAL;
	return remap_pfn_range(vma, vma->vm_start, page_to_pfn(pages[off]),
			       vma->vm_end - vma->vm_start, vma->vm_page_prot);
}

int vm_iomap_memory(struct vm_area_struct *vma, phys_addr_t start, unsigned long len)
{
	unsigned long off = vma->vm_pgoff << PAGE_SHIFT;

	if (off + (vma->vm_end - vma->vm_start) > PAGE_ALIGN(len + offset_in_page(start)))
		return -EINVAL;
	return remap_pfn_range(vma, vma->vm_start, (start >> PAGE_SHIFT) + vma->vm_pgoff,
			       vma->vm_end - vma->vm_start, vma->vm_page_prot);
}

vm_fault_t vmf_insert_pfn_prot(struct vm_area_struct *vma, unsigned long addr,
			       unsigned long pfn, pgprot_t pgprot)
{
	struct kpi_vma *kv = to_kv(vma);

	kv->pfn = pfn;
	kv->prot = pgprot;
	kv->inserted = true;
	return VM_FAULT_NOPAGE;
}

vm_fault_t vmf_insert_pfn(struct vm_area_struct *vma, unsigned long addr, unsigned long pfn)
{
	return vmf_insert_pfn_prot(vma, addr, pfn, vma->vm_page_prot);
}

vm_fault_t vmf_insert_mixed(struct vm_area_struct *vma, unsigned long addr, unsigned long pfn)
{
	return vmf_insert_pfn(vma, addr, pfn);
}

int vm_insert_page(struct vm_area_struct *vma, unsigned long addr, struct page *page)
{
	vmf_insert_pfn(vma, addr, page_to_pfn(page));
	return 0;
}

/*
 * Map @len bytes at file offset @off. Returns 0 with *kind 1 (contiguous:
 * *phys, *cache) or 2 (faulting: *handle for kpi_vma_fault/close), or a
 * negative errno.
 */
/*
 * The address space every driver-visible vma claims (there is no Linux
 * mm). Its mmap lock reads as write-held and its lock sequence matches
 * new vmas', so vm_flags_set() and friends find the vma write-locked.
 */
static struct mm_struct kpi_vma_mm = {
	.mmap_lock = { .count = ATOMIC_LONG_INIT(RWSEM_WRITER_LOCKED) },
};

int kpi_file_mmap(struct file *file, u64 off, u64 len, u32 prot, int *kind, u64 *phys,
		  int *cache, void **handle)
{
	struct kpi_vma *kv;
	int err;

	if (!file->f_op->mmap)
		return -ENODEV;
	kv = kzalloc(sizeof(*kv), GFP_KERNEL);
	if (!kv)
		return -ENOMEM;
	kv->vma.vm_mm = &kpi_vma_mm;
	kv->vma.vm_start = 0x100000000000UL;	/* a nominal user address */
	kv->vma.vm_end = kv->vma.vm_start + PAGE_ALIGN(len);
	kv->vma.vm_pgoff = off >> PAGE_SHIFT;
	vm_flags_init(&kv->vma, VM_SHARED | VM_MAYSHARE | (prot & PROT_READ ? VM_READ | VM_MAYREAD : 0) |
			   (prot & PROT_WRITE ? VM_WRITE | VM_MAYWRITE : 0));
	kv->vma.vm_page_prot = PAGE_SHARED;
	kv->vma.vm_file = get_file(file);
	err = file->f_op->mmap(file, &kv->vma);
	if (err) {
		fput(file);
		kfree(kv);
		return err;
	}
	if (kv->remapped) {
		*kind = 1;
		*phys = (u64)kv->pfn << PAGE_SHIFT;
		*cache = kpi_prot_cache(kv->prot);
		if (kv->vma.vm_ops && kv->vma.vm_ops->close)
			kv->vma.vm_ops->close(&kv->vma);
		fput(file);
		kfree(kv);
		return 0;
	}
	if (!kv->vmalloc_base && (!kv->vma.vm_ops || !kv->vma.vm_ops->fault)) {
		if (kv->vma.vm_ops && kv->vma.vm_ops->close)
			kv->vma.vm_ops->close(&kv->vma);
		fput(file);
		kfree(kv);
		return -ENODEV;
	}
	*kind = 2;
	*handle = kv;
	return 0;
}

/* Page @pgoff (file page) of a faulting mapping: 0 and *phys/*cache. */
int kpi_vma_fault(void *handle, u64 pgoff, int write, u64 *phys, int *cache)
{
	struct kpi_vma *kv = handle;
	struct vm_fault vmf = {
		.vma = &kv->vma,
		.pgoff = pgoff,
		.address = kv->vma.vm_start + ((pgoff - kv->vma.vm_pgoff) << PAGE_SHIFT),
		.flags = write ? FAULT_FLAG_WRITE : 0,
	};
	vm_fault_t r;

	if (kv->vmalloc_base) {
		u64 off = (pgoff - kv->vma.vm_pgoff) << PAGE_SHIFT;

		if (off >= kv->vma.vm_end - kv->vma.vm_start)
			return -EFAULT;
		*phys = rustos_kpi_virt_to_phys((u64)kv->vmalloc_base + off);
		*cache = 2;
		return *phys ? 0 : -EFAULT;
	}
	kv->inserted = false;
	r = kv->vma.vm_ops->fault(&vmf);
	if (r & VM_FAULT_ERROR)
		return -EFAULT;
	if (!kv->inserted && vmf.page) {
		kv->pfn = page_to_pfn(vmf.page);
		kv->prot = kv->vma.vm_page_prot;
		kv->inserted = true;
		put_page(vmf.page);	/* the reference fault() took */
	}
	if (!kv->inserted)
		return -EFAULT;
	*phys = (u64)kv->pfn << PAGE_SHIFT;
	*cache = kpi_prot_cache(kv->prot);
	return 0;
}

void kpi_vma_close(void *handle)
{
	struct kpi_vma *kv = handle;

	if (kv->vma.vm_ops && kv->vma.vm_ops->close)
		kv->vma.vm_ops->close(&kv->vma);
	fput(kv->vma.vm_file);
	kfree(kv);
}

/* ----------------------------------------------------- cdevs and majors */

static LIST_HEAD(kpi_cdevs);
static DEFINE_MUTEX(kpi_cdev_lock);
static unsigned int kpi_next_major = 511;

void cdev_init(struct cdev *cdev, const struct file_operations *fops)
{
	memset(cdev, 0, sizeof(*cdev));
	INIT_LIST_HEAD(&cdev->list);
	cdev->ops = fops;
}

struct cdev *cdev_alloc(void)
{
	struct cdev *p = kzalloc(sizeof(*p), GFP_KERNEL);

	if (p)
		INIT_LIST_HEAD(&p->list);
	return p;
}

int cdev_add(struct cdev *p, dev_t dev, unsigned int count)
{
	p->dev = dev;
	p->count = count;
	mutex_lock(&kpi_cdev_lock);
	list_add(&p->list, &kpi_cdevs);
	mutex_unlock(&kpi_cdev_lock);
	return 0;
}

void cdev_del(struct cdev *p)
{
	mutex_lock(&kpi_cdev_lock);
	list_del_init(&p->list);
	mutex_unlock(&kpi_cdev_lock);
}

void cdev_set_parent(struct cdev *p, struct kobject *kobj)
{
}

int cdev_device_add(struct cdev *cdev, struct device *dev)
{
	int err = 0;

	if (dev->devt) {
		err = cdev_add(cdev, dev->devt, 1);
		if (err)
			return err;
	}
	err = device_add(dev);
	if (err && dev->devt)
		cdev_del(cdev);
	return err;
}

void cdev_device_del(struct cdev *cdev, struct device *dev)
{
	device_del(dev);
	if (dev->devt)
		cdev_del(cdev);
}

static struct cdev *kpi_cdev_lookup(dev_t devt)
{
	struct cdev *c, *found = NULL;

	mutex_lock(&kpi_cdev_lock);
	list_for_each_entry(c, &kpi_cdevs, list) {
		if (devt >= c->dev && devt < c->dev + c->count) {
			found = c;
			break;
		}
	}
	mutex_unlock(&kpi_cdev_lock);
	return found;
}

int register_chrdev_region(dev_t from, unsigned int count, const char *name)
{
	return 0;
}

int alloc_chrdev_region(dev_t *dev, unsigned int baseminor, unsigned int count,
			const char *name)
{
	mutex_lock(&kpi_cdev_lock);
	*dev = MKDEV(kpi_next_major--, baseminor);
	mutex_unlock(&kpi_cdev_lock);
	return 0;
}

void unregister_chrdev_region(dev_t from, unsigned int count)
{
}

int __register_chrdev(unsigned int major, unsigned int baseminor, unsigned int count,
		      const char *name, const struct file_operations *fops)
{
	struct cdev *cdev = cdev_alloc();
	bool dynamic = !major;
	dev_t dev;

	if (!cdev)
		return -ENOMEM;
	if (dynamic) {
		alloc_chrdev_region(&dev, baseminor, count, name);
		major = MAJOR(dev);
	}
	cdev->ops = fops;
	cdev_add(cdev, MKDEV(major, baseminor), count);
	/* As Linux: the new major when one was allocated, else 0. */
	return dynamic ? major : 0;
}

void __unregister_chrdev(unsigned int major, unsigned int baseminor, unsigned int count,
			 const char *name)
{
	struct cdev *c = kpi_cdev_lookup(MKDEV(major, baseminor));

	if (c) {
		cdev_del(c);
		kfree(c);
	}
}

/* Open device @devt: a new struct file, through the driver's open(). */
int kpi_chrdev_open(u32 devt, unsigned int flags, struct file **out)
{
	struct cdev *cdev = kpi_cdev_lookup(devt);
	struct kpi_file *kf;
	int err = 0;

	if (!cdev || !cdev->ops)
		return -ENXIO;
	kf = kpi_file_alloc(cdev->ops, flags, devt);
	if (!kf)
		return -ENOMEM;
	kf->inode.i_cdev = cdev;
	if (cdev->ops->open)
		err = cdev->ops->open(&kf->inode, &kf->file);
	if (err) {
		kfree(kf);
		return err;
	}
	*out = &kf->file;
	return 0;
}

/* ---------------------------------------------------------- device nodes */

/* drivers/base/core.c (declared in its private base.h). */
const char *device_get_devnode(const struct device *dev, umode_t *mode, kuid_t *uid,
			       kgid_t *gid, const char **tmp);

int __init devtmpfs_init(void)
{
	return 0;
}

int devtmpfs_create_node(struct device *dev)
{
	const char *tmp = NULL, *name;
	umode_t mode = 0;
	kuid_t uid;
	kgid_t gid;
	int err;

	name = device_get_devnode(dev, &mode, &uid, &gid, &tmp);
	if (!name)
		return -ENOMEM;
	err = rustos_kpi_devnode_add(name, dev->devt, dev->class && !strcmp(dev->class->name,
									      "block"));
	kfree(tmp);
	return err;
}

int devtmpfs_delete_node(struct device *dev)
{
	const char *tmp = NULL, *name;
	umode_t mode;
	kuid_t uid;
	kgid_t gid;

	name = device_get_devnode(dev, &mode, &uid, &gid, &tmp);
	if (name)
		rustos_kpi_devnode_remove(name);
	kfree(tmp);
	return 0;
}

/* ------------------------------------------------------- misc devices */

#define KPI_MISC_MAJOR 10
static LIST_HEAD(kpi_misc_list);
static DEFINE_MUTEX(kpi_misc_lock);
static DECLARE_BITMAP(kpi_misc_minors, 256);

static char *kpi_misc_devnode(const struct device *dev, umode_t *mode)
{
	struct miscdevice *c = dev_get_drvdata(dev);

	if (mode && c->mode)
		*mode = c->mode;
	if (c->nodename)
		return kstrdup(c->nodename, GFP_KERNEL);
	return NULL;
}

static const struct class kpi_misc_class = {
	.name = "misc",
	.devnode = kpi_misc_devnode,
};

static int kpi_misc_open(struct inode *inode, struct file *file)
{
	struct miscdevice *c, *found = NULL;
	int err = -ENODEV;

	mutex_lock(&kpi_misc_lock);
	list_for_each_entry(c, &kpi_misc_list, list) {
		if (c->minor == iminor(inode)) {
			found = c;
			break;
		}
	}
	mutex_unlock(&kpi_misc_lock);
	if (!found || !found->fops)
		return err;
	/* As drivers/char/misc.c: the misc device's fops take over. */
	file->private_data = found;
	file->f_op = found->fops;
	err = file->f_op->open ? file->f_op->open(inode, file) : 0;
	return err;
}

static const struct file_operations kpi_misc_fops = {
	.owner = THIS_MODULE,
	.open = kpi_misc_open,
};

int misc_register(struct miscdevice *misc)
{
	dev_t dev;

	mutex_lock(&kpi_misc_lock);
	if (misc->minor == MISC_DYNAMIC_MINOR) {
		int i = find_next_zero_bit(kpi_misc_minors, 256, 64);

		if (i >= 256) {
			mutex_unlock(&kpi_misc_lock);
			return -EBUSY;
		}
		misc->minor = i;
	}
	set_bit(misc->minor & 255, kpi_misc_minors);
	list_add(&misc->list, &kpi_misc_list);
	mutex_unlock(&kpi_misc_lock);
	dev = MKDEV(KPI_MISC_MAJOR, misc->minor);
	misc->this_device = device_create_with_groups(&kpi_misc_class, misc->parent, dev, misc,
						      misc->groups, "%s", misc->name);
	if (IS_ERR(misc->this_device)) {
		int err = PTR_ERR(misc->this_device);

		mutex_lock(&kpi_misc_lock);
		list_del(&misc->list);
		clear_bit(misc->minor & 255, kpi_misc_minors);
		mutex_unlock(&kpi_misc_lock);
		return err;
	}
	return 0;
}

void misc_deregister(struct miscdevice *misc)
{
	device_destroy(&kpi_misc_class, MKDEV(KPI_MISC_MAJOR, misc->minor));
	mutex_lock(&kpi_misc_lock);
	list_del(&misc->list);
	clear_bit(misc->minor & 255, kpi_misc_minors);
	mutex_unlock(&kpi_misc_lock);
}

/* --------------------------------------------------------- anon inodes */

struct file *anon_inode_getfile(const char *name, const struct file_operations *fops,
				void *priv, int flags)
{
	struct kpi_file *kf = kpi_file_alloc(fops, flags, 0);

	if (!kf)
		return ERR_PTR(-ENOMEM);
	kf->file.private_data = priv;
	kf->inode.i_mode = S_IFREG | 0600;
	return &kf->file;
}

int anon_inode_getfd(const char *name, const struct file_operations *fops, void *priv,
		     int flags)
{
	struct file *f = anon_inode_getfile(name, fops, priv, flags);
	int fd;

	if (IS_ERR(f))
		return PTR_ERR(f);
	/* Consumes the reference, also on failure. */
	fd = rustos_kpi_fd_install(f, flags & O_CLOEXEC, -1);
	return fd;
}

int get_unused_fd_flags(unsigned int flags)
{
	return rustos_kpi_fd_reserve(flags & O_CLOEXEC);
}

void put_unused_fd(unsigned int fd)
{
	rustos_kpi_fd_unreserve(fd);
}

/* Takes over the caller's reference to @file. */
void fd_install(unsigned int fd, struct file *file)
{
	rustos_kpi_fd_install(file, 0, fd);
}

int kpi_chrdev_init(void)
{
	int err = class_register(&kpi_misc_class);

	if (err)
		return err;
	return __register_chrdev(KPI_MISC_MAJOR, 0, 256, "misc", &kpi_misc_fops) < 0 ? -EBUSY : 0;
}

/* fs/libfs.c attribute files: only debugfs uses them, and debugfs is off,
 * so these are never reached through a file. */
int simple_attr_open(struct inode *inode, struct file *file, int (*get)(void *, u64 *),
		     int (*set)(void *, u64), const char *fmt)
{
	return -ENODEV;
}

int simple_attr_release(struct inode *inode, struct file *file)
{
	return 0;
}

/* -------------------------------------------- pseudo filesystems (dma-buf) */

struct kpi_pseudo {
	struct super_block sb;
	struct vfsmount mnt;
	struct pseudo_fs_context ctx;
};

struct pseudo_fs_context *init_pseudo(struct fs_context *fc, unsigned long magic)
{
	struct kpi_pseudo *p = fc->fs_private;

	p->ctx.magic = magic;
	return &p->ctx;
}

/* An internal filesystem's mount: its superblock carries the default
 * dentry operations of its files. */
struct vfsmount *kern_mount(struct file_system_type *type)
{
	struct kpi_pseudo *p = kzalloc(sizeof(*p), GFP_KERNEL);
	struct fs_context fc = { .fs_type = type };

	if (!p)
		return ERR_PTR(-ENOMEM);
	fc.fs_private = p;
	if (type->init_fs_context && type->init_fs_context(&fc)) {
		kfree(p);
		return ERR_PTR(-ENOMEM);
	}
	p->sb.s_type = type;
	p->sb.s_magic = p->ctx.magic;
	p->sb.__s_d_op = p->ctx.dops;
	p->mnt.mnt_sb = &p->sb;
	return &p->mnt;
}

void kill_anon_super(struct super_block *sb)
{
}

struct inode *alloc_anon_inode(struct super_block *sb)
{
	struct inode *inode = kzalloc(sizeof(*inode), GFP_KERNEL);

	if (!inode)
		return ERR_PTR(-ENOMEM);
	inode->i_sb = sb;
	inode->i_mode = S_IFREG | 0600;
	atomic_set(&inode->i_count, 1);
	return inode;
}

void iput(struct inode *inode)
{
	if (inode && atomic_dec_and_test(&inode->i_count))
		kfree(inode);
}

void inode_set_bytes(struct inode *inode, loff_t bytes)
{
	inode->i_blocks = bytes >> 9;
	inode->i_bytes = bytes & 511;
}

/* Takes over the caller's reference to @inode. */
struct file *alloc_file_pseudo(struct inode *inode, struct vfsmount *mnt, const char *name,
			       int flags, const struct file_operations *fops)
{
	struct kpi_file *kf = kpi_file_alloc(fops, flags, 0);
	struct dentry *d = kzalloc(sizeof(*d), GFP_KERNEL);

	if (!kf || !d) {
		kfree(kf);
		kfree(d);
		return ERR_PTR(-ENOMEM);
	}
	d->d_inode = inode;
	d->d_sb = mnt->mnt_sb;
	d->d_op = mnt->mnt_sb->__s_d_op;
	kf->file.f_inode = inode;
	kf->file.__f_path.mnt = mnt;
	kf->file.__f_path.dentry = d;
	return &kf->file;
}

char *dynamic_dname(char *buffer, int buflen, const char *fmt, ...)
{
	char temp[64];
	va_list args;
	int sz;

	va_start(args, fmt);
	sz = vsnprintf(temp, sizeof(temp), fmt, args) + 1;
	va_end(args);
	if (sz > sizeof(temp) || sz > buflen)
		return ERR_PTR(-ENAMETOOLONG);
	buffer += buflen - sz;
	return memcpy(buffer, temp, sz);
}

struct fd fdget(unsigned int fd)
{
	struct file *f = fget(fd);

	return (struct fd){ (unsigned long)f | (f ? FDPUT_FPUT : 0) };
}
