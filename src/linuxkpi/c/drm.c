// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * What the DRM core and its helpers need around them (M35):
 *
 * - shmem files for GEM objects: a pseudo-filesystem file whose page
 *   cache is an xarray of zeroed pages, allocated as they are first read
 *   and freed with the file;
 * - the firmware framebuffer as an aperture owner: a platform device
 *   holds the GOP framebuffer's range, so a DRM driver taking the display
 *   (aperture_remove_conflicting_*) removes it, and RustOS stops drawing
 *   its console there (with simpledrm, sysfb.c's device does this);
 * - small pieces of the VFS, eventfd and x86 memory-type API.
 */
#include <linux/aperture.h>
#include <linux/acpi.h>
#include <linux/eventfd.h>
#include <linux/fs.h>
#include <linux/fs_context.h>
#include <linux/io.h>
#include <linux/irq_work.h>
#include <linux/mount.h>
#include <linux/pagemap.h>
#include <linux/pagevec.h>
#include <linux/platform_device.h>
#include <linux/pseudo_fs.h>
#include <linux/shmem_fs.h>
#include <linux/slab.h>
#include <linux/swap.h>
#include <linux/vgaarb.h>
#include <asm/set_memory.h>
#include <video/vga.h>
#include "kpi.h"

/* ----------------------------------------------------------- shmem files */

/* fs/inode.c */
void address_space_init_once(struct address_space *mapping)
{
	memset(mapping, 0, sizeof(*mapping));
	xa_init_flags(&mapping->i_pages, XA_FLAGS_LOCK_IRQ | XA_FLAGS_ACCOUNT);
	init_rwsem(&mapping->i_mmap_rwsem);
	init_rwsem(&mapping->invalidate_lock);
	spin_lock_init(&mapping->i_private_lock);
	INIT_LIST_HEAD(&mapping->i_private_list);
	mapping->i_mmap = RB_ROOT_CACHED;
}

static void kpi_shmem_release(struct dentry *d)
{
	struct address_space *mapping = d->d_inode->i_mapping;
	struct folio *folio;
	unsigned long index;

	xa_for_each(&mapping->i_pages, index, folio)
		folio_put(folio);
	xa_destroy(&mapping->i_pages);
}

static const struct dentry_operations kpi_shmem_dops = {
	.d_release = kpi_shmem_release,
};

static int kpi_shmem_init_fs_context(struct fs_context *fc)
{
	struct pseudo_fs_context *ctx = init_pseudo(fc, 0x01021994 /* TMPFS_MAGIC */);

	if (!ctx)
		return -ENOMEM;
	ctx->dops = &kpi_shmem_dops;
	return 0;
}

static struct file_system_type kpi_shmem_fs_type = {
	.name = "shmem",
	.init_fs_context = kpi_shmem_init_fs_context,
};

static struct vfsmount *kpi_shmem_mnt;
static DEFINE_MUTEX(kpi_shmem_lock);

static const struct file_operations kpi_shmem_fops = {
	.owner = THIS_MODULE,
};

struct file *shmem_file_setup_with_mnt(struct vfsmount *mnt, const char *name, loff_t size,
				       unsigned long flags)
{
	struct inode *inode;
	struct file *file;

	mutex_lock(&kpi_shmem_lock);
	if (!kpi_shmem_mnt)
		kpi_shmem_mnt = kern_mount(&kpi_shmem_fs_type);
	mutex_unlock(&kpi_shmem_lock);
	if (IS_ERR(kpi_shmem_mnt))
		return ERR_CAST(kpi_shmem_mnt);
	inode = alloc_anon_inode(kpi_shmem_mnt->mnt_sb);
	if (IS_ERR(inode))
		return ERR_CAST(inode);
	address_space_init_once(&inode->i_data);
	inode->i_data.host = inode;
	inode->i_mapping = &inode->i_data;
	inode->i_size = size;
	mapping_set_gfp_mask(inode->i_mapping, GFP_HIGHUSER);
	file = alloc_file_pseudo(inode, kpi_shmem_mnt, name, O_RDWR, &kpi_shmem_fops);
	if (IS_ERR(file)) {
		iput(inode);
		return file;
	}
	file->f_mapping = inode->i_mapping;
	return file;
}

struct file *shmem_file_setup(const char *name, loff_t size, unsigned long flags)
{
	return shmem_file_setup_with_mnt(NULL, name, size, flags);
}

/* Page @index of a shmem file, allocated zeroed on first use; the caller
 * gets a reference. */
struct folio *shmem_read_folio_gfp(struct address_space *mapping, pgoff_t index, gfp_t gfp)
{
	struct folio *folio, *old;
	struct page *page;

	if ((loff_t)index << PAGE_SHIFT >= i_size_read(mapping->host))
		return ERR_PTR(-EINVAL);
	folio = xa_load(&mapping->i_pages, index);
	if (folio) {
		folio_get(folio);
		return folio;
	}
	page = alloc_page((gfp & ~__GFP_HIGHMEM) | __GFP_ZERO);
	if (!page)
		return ERR_PTR(-ENOMEM);
	folio = page_folio(page);
	old = xa_cmpxchg(&mapping->i_pages, index, NULL, folio, GFP_KERNEL);
	if (old) {
		/* Raced with another reader, or out of memory. */
		folio_put(folio);
		if (xa_is_err(old))
			return ERR_PTR(xa_err(old));
		folio = old;
	} else {
		mapping->nrpages++;
	}
	folio_get(folio);
	return folio;
}

void __folio_batch_release(struct folio_batch *fbatch)
{
	for (unsigned int i = 0; i < folio_batch_count(fbatch); i++)
		folio_put(fbatch->folios[i]);
	folio_batch_reinit(fbatch);
}

/* No reclaim or writeback: these page-cache states do not matter. */
void check_move_unevictable_folios(struct folio_batch *fbatch)
{
}

void folio_mark_accessed(struct folio *folio)
{
}

bool folio_mark_dirty(struct folio *folio)
{
	return true;
}

/*
 * User mappings are not zapped when objects move: RustOS keeps the pages
 * a fault mapped until the mapping goes away (GEM objects do not move in
 * the drivers this build has).
 */
void unmap_mapping_range(struct address_space *mapping, loff_t const holebegin,
			 loff_t const holelen, int even_cows)
{
}

void vma_set_file(struct vm_area_struct *vma, struct file *file)
{
	get_file(file);
	swap(vma->vm_file, file);
	fput(file);
}

struct file *dentry_open(const struct path *path, int flags, const struct cred *cred)
{
	return ERR_PTR(-EOPNOTSUPP);
}

int simple_pin_fs(struct file_system_type *type, struct vfsmount **mount, int *count)
{
	if (!*mount) {
		struct vfsmount *m = kern_mount(type);

		if (IS_ERR(m))
			return PTR_ERR(m);
		*mount = m;
	}
	++*count;
	return 0;
}

void simple_release_fs(struct vfsmount **mount, int *count)
{
	--*count;
}

/* --------------------------------------------------------------- eventfd */

/* Syncobj eventfd notification (DRM_IOCTL_SYNCOBJ_EVENTFD) is unsupported. */
struct eventfd_ctx *eventfd_ctx_fdget(int fd)
{
	return ERR_PTR(-EINVAL);
}

void eventfd_ctx_put(struct eventfd_ctx *ctx)
{
}

void eventfd_signal_mask(struct eventfd_ctx *ctx, __poll_t mask)
{
}

/* ------------------------------------------------------------- odds */

int oops_in_progress;
int overflowuid = 65534;

void memcpy_toio(volatile void __iomem *dst, const void *src, size_t count)
{
	const u8 *s = src;

	for (size_t i = 0; i < count; i++)
		writeb(s[i], dst + i);
}

int register_acpi_bus_type(struct acpi_bus_type *type)
{
	return 0;
}

int unregister_acpi_bus_type(struct acpi_bus_type *type)
{
	return 0;
}

/* The direct map stays write-back; the PAT handles device mappings. */
int set_pages_array_wb(struct page **pages, int addrinarray)
{
	return 0;
}

int set_pages_array_wc(struct page **pages, int addrinarray)
{
	return 0;
}

void wbinvd_on_all_cpus(void)
{
	asm volatile("wbinvd" ::: "memory");
}

/* No VGA arbitration or legacy VGA console. */
struct pci_dev *vga_default_device(void)
{
	return NULL;
}

int vga_remove_vgacon(struct pci_dev *pdev)
{
	return 0;
}

bool video_is_primary_device(struct device *dev)
{
	return false;
}

/* ------------------------------------- the firmware framebuffer's owner */

static int kpi_fwfb_probe(struct platform_device *pdev)
{
	u64 len, phys = rustos_kpi_fb_phys(&len);

	if (!phys)
		return -ENODEV;
	return devm_aperture_acquire_for_platform_device(pdev, phys, len);
}

/* A display driver took the device (its aperture detached us). */
static void kpi_fwfb_remove(struct platform_device *pdev)
{
	rustos_kpi_fb_release();
}

static struct platform_driver kpi_fwfb_driver = {
	.driver = { .name = "rustos-fwfb" },
	.probe = kpi_fwfb_probe,
	.remove = kpi_fwfb_remove,
};

/* Overridden by sysfb.c in builds with simpledrm, which describes the
 * firmware framebuffer to it instead. */
bool __weak kpi_sysfb_register(void)
{
	return false;
}

static int __init kpi_fwfb_init(void)
{
	struct platform_device *pdev;
	u64 len;

	if (!rustos_kpi_fb_phys(&len) || kpi_sysfb_register())
		return 0;
	if (platform_driver_register(&kpi_fwfb_driver))
		return 0;
	pdev = platform_device_register_simple("rustos-fwfb", PLATFORM_DEVID_NONE, NULL, 0);
	return PTR_ERR_OR_ZERO(pdev);
}
/* Before DRM drivers (module_init) probe and evict it. */
subsys_initcall(kpi_fwfb_init);

int vga_get(struct pci_dev *pdev, unsigned int rsrc, int interruptible)
{
	return 0;
}

void vga_put(struct pci_dev *pdev, unsigned int rsrc)
{
}

/* PAT entries as RustOS programs them (src/mm): WB 0, WC PWT, UC- PCD,
 * UC PCD|PWT. */
unsigned long cachemode2protval(enum page_cache_mode pcm)
{
	switch (pcm) {
	case _PAGE_CACHE_MODE_WB:
		return 0;
	case _PAGE_CACHE_MODE_WC:
		return _PAGE_PWT;
	case _PAGE_CACHE_MODE_UC_MINUS:
		return _PAGE_PCD;
	default:
		return _PAGE_PCD | _PAGE_PWT;
	}
}
