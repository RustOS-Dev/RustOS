// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * LinuxKPI memory: the direct map and struct page array Linux's address
 * macros expect, page allocation, kmalloc, vmalloc and ioremap.
 *
 * - page_offset_base is RustOS's physical-memory offset, so __va()/__pa()
 *   work for every allocation made here (all come from physical frames).
 * - vmemmap_base points at a zeroed struct page array covering every frame
 *   the RustOS frame allocator manages.
 * - kmalloc uses power-of-two size classes carved from single pages, with
 *   the class recorded in the page's `private` field; larger requests get
 *   their own contiguous pages.
 */
#include <linux/gfp.h>
#include <linux/io.h>
#include <linux/mm.h>
#include <linux/slab.h>
#include <linux/string.h>
#include <linux/vmalloc.h>
#include "kpi.h"

#define KPI_VMEMMAP_BASE	0xfffff00000000000UL

unsigned long page_offset_base;
unsigned long vmemmap_base = KPI_VMEMMAP_BASE;
/* The RustOS kernel image is not in Linux's __START_KERNEL_map window. */
unsigned long phys_base;
unsigned long max_pfn;

kmem_buckets kmalloc_caches[NR_KMALLOC_TYPES];

/* `page->private` tags for pages this allocator owns. */
#define KPI_TAG_MASK		0xffff000000000000UL
#define KPI_TAG_PAGES		0x4b50000000000000UL	/* "KP": order in low bits */
#define KPI_TAG_SLAB		0x4b53000000000000UL	/* "KS": class in low bits */

int kpi_mm_init(void)
{
	page_offset_base = rustos_kpi_page_offset();
	max_pfn = rustos_kpi_max_pfn();
	return rustos_kpi_map_zeroed(KPI_VMEMMAP_BASE,
				     PAGE_ALIGN(max_pfn * sizeof(struct page))) ? -ENOMEM : 0;
}

/* ---------------------------------------------------------------- pages */

static struct page *kpi_alloc_pages(gfp_t gfp, unsigned int order)
{
	unsigned long n = 1UL << order, i;
	u64 phys = rustos_kpi_alloc_frames(n, n * PAGE_SIZE,
					   !!(gfp & (__GFP_DMA | __GFP_DMA32)));
	struct page *page;

	if (!phys)
		return NULL;
	page = pfn_to_page(phys >> PAGE_SHIFT);
	for (i = 0; i < n; i++)
		memset(&page[i], 0, sizeof(struct page));
	set_page_count(page, 1);
	set_page_private(page, KPI_TAG_PAGES | order);
	if (gfp & __GFP_ZERO)
		memset(page_address(page), 0, n * PAGE_SIZE);
	return page;
}

static void kpi_free_page_block(struct page *page)
{
	unsigned long tag = page_private(page);
	unsigned int order = 0;

	if ((tag & KPI_TAG_MASK) == KPI_TAG_PAGES)
		order = tag & 0xff;
	set_page_private(page, 0);
	rustos_kpi_free_frames(page_to_phys(page), 1UL << order);
}

struct page *__alloc_pages_noprof(gfp_t gfp, unsigned int order, int preferred_nid,
				  nodemask_t *nodemask)
{
	return kpi_alloc_pages(gfp, order);
}

void __free_pages(struct page *page, unsigned int order)
{
	if (put_page_testzero(page))
		kpi_free_page_block(page);
}

void free_pages(unsigned long addr, unsigned int order)
{
	if (addr)
		__free_pages(virt_to_page((void *)addr), order);
}

unsigned long get_free_pages_noprof(gfp_t gfp, unsigned int order)
{
	struct page *page = kpi_alloc_pages(gfp, order);

	return page ? (unsigned long)page_address(page) : 0;
}

unsigned long get_zeroed_page_noprof(gfp_t gfp)
{
	return get_free_pages_noprof(gfp | __GFP_ZERO, 0);
}

void __folio_put(struct folio *folio)
{
	kpi_free_page_block(&folio->page);
}

void page_frag_free(void *addr)
{
	put_page(virt_to_page(addr));
}

/* -------------------------------------------------------------- kmalloc */

#define KPI_MIN_SHIFT	4			/* 16 bytes */
#define KPI_MAX_SHIFT	11			/* 2048 bytes; larger use pages */
#define KPI_CLASSES	(KPI_MAX_SHIFT - KPI_MIN_SHIFT + 1)

struct kpi_free { struct kpi_free *next; };
static struct kpi_free *kpi_free_list[KPI_CLASSES];
static int kpi_slab_lock;

static unsigned long kpi_lock(void)
{
	unsigned long flags;

	local_irq_save(flags);
	while (__atomic_exchange_n(&kpi_slab_lock, 1, __ATOMIC_ACQUIRE))
		cpu_relax();
	return flags;
}

static void kpi_unlock(unsigned long flags)
{
	__atomic_store_n(&kpi_slab_lock, 0, __ATOMIC_RELEASE);
	local_irq_restore(flags);
}

static int kpi_class(size_t size)
{
	int shift = KPI_MIN_SHIFT;

	while ((1UL << shift) < size)
		shift++;
	return shift - KPI_MIN_SHIFT;
}

static void *kpi_kmalloc(size_t size, gfp_t flags)
{
	unsigned long irq;
	struct kpi_free *obj;
	int c;

	if (!size)
		return ZERO_SIZE_PTR;
	if (size > (1UL << KPI_MAX_SHIFT)) {
		struct page *page = kpi_alloc_pages(flags, get_order(size));

		return page ? page_address(page) : NULL;
	}
	c = kpi_class(size);
	irq = kpi_lock();
	obj = kpi_free_list[c];
	if (!obj) {
		/* Carve a new page into objects of this class. */
		size_t osz = 1UL << (c + KPI_MIN_SHIFT);
		struct page *page;
		char *base;
		size_t off;

		kpi_unlock(irq);
		page = kpi_alloc_pages(flags & ~__GFP_ZERO, 0);
		if (!page)
			return NULL;
		set_page_private(page, KPI_TAG_SLAB | c);
		base = page_address(page);
		irq = kpi_lock();
		for (off = 0; off < PAGE_SIZE; off += osz) {
			struct kpi_free *f = (struct kpi_free *)(base + off);

			f->next = kpi_free_list[c];
			kpi_free_list[c] = f;
		}
		obj = kpi_free_list[c];
	}
	kpi_free_list[c] = obj->next;
	kpi_unlock(irq);
	if (flags & __GFP_ZERO)
		memset(obj, 0, 1UL << (c + KPI_MIN_SHIFT));
	return obj;
}

size_t ksize(const void *objp)
{
	struct page *page;
	unsigned long tag;

	if (ZERO_OR_NULL_PTR(objp))
		return 0;
	page = virt_to_page(objp);
	tag = page_private(page);
	if ((tag & KPI_TAG_MASK) == KPI_TAG_SLAB)
		return 1UL << ((tag & 0xff) + KPI_MIN_SHIFT);
	return PAGE_SIZE << (tag & 0xff);
}

size_t kmalloc_size_roundup(size_t size)
{
	if (size <= (1UL << KPI_MAX_SHIFT))
		return size ? 1UL << (kpi_class(size) + KPI_MIN_SHIFT) : 0;
	return PAGE_SIZE << get_order(size);
}

void kfree(const void *objp)
{
	struct page *page;
	unsigned long tag, irq;
	int c;

	if (ZERO_OR_NULL_PTR(objp))
		return;
	if (is_vmalloc_addr(objp)) {
		vfree(objp);
		return;
	}
	page = virt_to_page(objp);
	tag = page_private(page);
	if ((tag & KPI_TAG_MASK) != KPI_TAG_SLAB) {
		__free_pages(page, tag & 0xff);
		return;
	}
	c = tag & 0xff;
	irq = kpi_lock();
	((struct kpi_free *)objp)->next = kpi_free_list[c];
	kpi_free_list[c] = (struct kpi_free *)objp;
	kpi_unlock(irq);
}

void *__kmalloc_noprof(size_t size, gfp_t flags)
{
	return kpi_kmalloc(size, flags);
}

void *__kmalloc_cache_noprof(struct kmem_cache *s, gfp_t flags, size_t size)
{
	return kpi_kmalloc(size, flags);
}

void *__kmalloc_node_noprof(DECL_BUCKET_PARAMS(size, b), gfp_t flags, int node)
{
	return kpi_kmalloc(size, flags);
}

void *__kmalloc_cache_node_noprof(struct kmem_cache *s, gfp_t flags, int node, size_t size)
{
	return kpi_kmalloc(size, flags);
}

void *__kmalloc_large_noprof(size_t size, gfp_t flags)
{
	return kpi_kmalloc(size, flags);
}

void *__kmalloc_large_node_noprof(size_t size, gfp_t flags, int node)
{
	return kpi_kmalloc(size, flags);
}

void *krealloc_node_align_noprof(const void *p, size_t new_size, unsigned long align,
				 gfp_t flags, int nid)
{
	size_t old = ksize(p);
	void *n;

	if (!new_size) {
		kfree(p);
		return ZERO_SIZE_PTR;
	}
	if (p && !ZERO_OR_NULL_PTR(p) && new_size <= old)
		return (void *)p;
	n = kpi_kmalloc(new_size, flags);
	if (n && !ZERO_OR_NULL_PTR(p)) {
		memcpy(n, p, min(old, new_size));
		kfree(p);
	}
	return n;
}

void kvfree(const void *addr)
{
	if (is_vmalloc_addr(addr))
		vfree(addr);
	else
		kfree(addr);
}

void *__kvmalloc_node_noprof(DECL_BUCKET_PARAMS(size, b), unsigned long align,
			     gfp_t flags, int node)
{
	if (size <= PAGE_SIZE * 8)
		return kpi_kmalloc(size, flags);
	return rustos_kpi_vmalloc(size);	/* zeroed */
}

/* -------------------------------------------------------------- vmalloc */

void *vmalloc_noprof(unsigned long size)
{
	return rustos_kpi_vmalloc(size);
}

void *vzalloc_noprof(unsigned long size)
{
	return rustos_kpi_vmalloc(size);	/* always zeroed */
}

void vfree(const void *addr)
{
	rustos_kpi_vfree(addr);
}

bool is_vmalloc_addr(const void *x)
{
	return rustos_kpi_is_vmalloc(x);
}

/* --------------------------------------------------------------- ioremap */

void __iomem *ioremap(resource_size_t offset, unsigned long size)
{
	return (void __iomem *)rustos_kpi_ioremap(offset, size, 0);
}

void __iomem *ioremap_wc(resource_size_t offset, unsigned long size)
{
	return (void __iomem *)rustos_kpi_ioremap(offset, size, 1);
}

void __iomem *ioremap_uc(resource_size_t offset, unsigned long size)
{
	return (void __iomem *)rustos_kpi_ioremap(offset, size, 0);
}

void iounmap(volatile void __iomem *addr)
{
	rustos_kpi_iounmap((void *)addr);
}

/* ------------------------------------------------------- string copies */

/*
 * The mm/util.c helpers. kstrdup_const() always copies here (and
 * kvasprintf_const() in printk.c always allocates), so kfree_const() is
 * kfree().
 */
void *kmemdup_noprof(const void *src, size_t len, gfp_t gfp)
{
	void *p = kmalloc(len, gfp);

	if (p)
		memcpy(p, src, len);
	return p;
}

char *kmemdup_nul(const char *s, size_t len, gfp_t gfp)
{
	char *p;

	if (!s)
		return NULL;
	p = kmalloc(len + 1, gfp);
	if (p) {
		memcpy(p, s, len);
		p[len] = '\0';
	}
	return p;
}

char *kstrdup(const char *s, gfp_t gfp)
{
	return s ? kmemdup_nul(s, strlen(s), gfp) : NULL;
}

char *kstrndup(const char *s, size_t max, gfp_t gfp)
{
	return s ? kmemdup_nul(s, strnlen(s, max), gfp) : NULL;
}

const char *kstrdup_const(const char *s, gfp_t gfp)
{
	return kstrdup(s, gfp);
}

void kfree_const(const void *x)
{
	kfree(x);
}
