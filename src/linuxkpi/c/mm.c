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

void __iomem *ioremap_cache(resource_size_t offset, unsigned long size)
{
	return (void __iomem *)rustos_kpi_ioremap(offset, size, 2);
}

void iounmap(volatile void __iomem *addr)
{
	rustos_kpi_iounmap((void *)addr);
}

/* RAM goes through the direct map; anything else gets a mapping. */
void *memremap(resource_size_t offset, size_t size, unsigned long flags)
{
	if (PHYS_PFN(offset + size - 1) < max_pfn && (flags & MEMREMAP_WB))
		return __va(offset);
	return (void __force *)rustos_kpi_ioremap(offset, size, flags & MEMREMAP_WC ? 1 :
						  flags & MEMREMAP_WB ? 2 : 0);
}

void memunmap(void *addr)
{
	if (!virt_addr_valid(addr))
		rustos_kpi_iounmap(addr);
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

void *kmemdup_array(const void *src, size_t count, size_t element_size, gfp_t gfp)
{
	return kmemdup_noprof(src, size_mul(count, element_size), gfp);
}

void *__kmalloc_node_track_caller_noprof(DECL_BUCKET_PARAMS(size, b), gfp_t flags, int node,
					 unsigned long caller)
{
	return kpi_kmalloc(size, flags);
}

/* --------------------------------------------------------- slab caches */

/*
 * kmem_cache objects come from kmalloc: its size classes are powers of
 * two aligned to their size, so rounding the object size up to the
 * requested alignment gives aligned objects.
 */
struct kmem_cache {
	const char *name;
	unsigned int size;
	slab_flags_t flags;
	void (*ctor)(void *);
};

struct kmem_cache *__kmem_cache_create_args(const char *name, unsigned int object_size,
					    struct kmem_cache_args *args, slab_flags_t flags)
{
	struct kmem_cache *s = kzalloc(sizeof(*s), GFP_KERNEL);
	unsigned int align = args ? args->align : 0;

	if (!s)
		return NULL;
	if (flags & SLAB_HWCACHE_ALIGN)
		align = max_t(unsigned int, align, L1_CACHE_BYTES);
	s->name = name;
	s->size = align ? ALIGN(object_size, align) : object_size;
	s->flags = flags;
	s->ctor = args ? args->ctor : NULL;
	return s;
}

void kmem_cache_destroy(struct kmem_cache *s)
{
	kfree(s);
}

int kmem_cache_shrink(struct kmem_cache *s)
{
	return 0;
}

void *kmem_cache_alloc_noprof(struct kmem_cache *s, gfp_t flags)
{
	void *p = kpi_kmalloc(s->size, s->ctor ? flags & ~__GFP_ZERO : flags);

	if (p && s->ctor)
		s->ctor(p);
	return p;
}

void *kmem_cache_alloc_lru_noprof(struct kmem_cache *s, struct list_lru *lru, gfp_t flags)
{
	return kmem_cache_alloc_noprof(s, flags);
}

void *kmem_cache_alloc_node_noprof(struct kmem_cache *s, gfp_t flags, int node)
{
	return kmem_cache_alloc_noprof(s, flags);
}

void kmem_cache_free(struct kmem_cache *s, void *objp)
{
	kfree(objp);
}

/*
 * Sheaves: arrays of objects preallocated so later allocations cannot
 * fail (the maple tree reserves nodes this way).
 */
struct slab_sheaf {
	unsigned int capacity;
	unsigned int size;
	void *objects[];
};

static int kpi_sheaf_fill(struct kmem_cache *s, gfp_t gfp, struct slab_sheaf *sh,
			  unsigned int size)
{
	while (sh->size < size) {
		void *obj = kmem_cache_alloc_noprof(s, gfp);

		if (!obj)
			return -ENOMEM;
		sh->objects[sh->size++] = obj;
	}
	return 0;
}

struct slab_sheaf *kmem_cache_prefill_sheaf(struct kmem_cache *s, gfp_t gfp, unsigned int size)
{
	struct slab_sheaf *sh = kzalloc(struct_size(sh, objects, size), gfp);

	if (!sh)
		return NULL;
	sh->capacity = size;
	if (kpi_sheaf_fill(s, gfp, sh, size)) {
		kmem_cache_return_sheaf(s, gfp, sh);
		return NULL;
	}
	return sh;
}

int kmem_cache_refill_sheaf(struct kmem_cache *s, gfp_t gfp, struct slab_sheaf **sheafp,
			    unsigned int size)
{
	struct slab_sheaf *sh = *sheafp, *big;

	if (!sh)
		return -EINVAL;
	if (sh->size >= size)
		return 0;
	if (sh->capacity < size) {
		big = kzalloc(struct_size(big, objects, size), gfp);
		if (!big)
			return -ENOMEM;
		big->capacity = size;
		big->size = sh->size;
		memcpy(big->objects, sh->objects, sh->size * sizeof(void *));
		kfree(sh);
		*sheafp = sh = big;
	}
	return kpi_sheaf_fill(s, gfp, sh, size);
}

void kmem_cache_return_sheaf(struct kmem_cache *s, gfp_t gfp, struct slab_sheaf *sheaf)
{
	while (sheaf->size)
		kmem_cache_free(s, sheaf->objects[--sheaf->size]);
	kfree(sheaf);
}

void *kmem_cache_alloc_from_sheaf_noprof(struct kmem_cache *s, gfp_t gfp,
					 struct slab_sheaf *sheaf)
{
	void *obj;

	if (WARN_ON_ONCE(!sheaf->size))
		return NULL;
	obj = sheaf->objects[--sheaf->size];
	if (gfp & __GFP_ZERO)
		memset(obj, 0, s->size);
	return obj;
}

unsigned int kmem_cache_sheaf_size(struct slab_sheaf *sheaf)
{
	return sheaf->size;
}

void kmem_cache_free_bulk(struct kmem_cache *s, size_t size, void **p)
{
	for (size_t i = 0; i < size; i++)
		kfree(p[i]);
}

/* --------------------------------------------------------- user memory */

unsigned long _copy_from_user(void *to, const void __user *from, unsigned long n)
{
	unsigned long left;

	/* The kernel buffer of a read()/write() in progress (chrdev.c). */
	if (kpi_uaccess_kernel((const void __force *)from, n)) {
		memcpy(to, (const void __force *)from, n);
		return 0;
	}
	left = rustos_kpi_copy_from_user(to, (const void __force *)from, n);

	if (left)
		memset(to + (n - left), 0, left);
	return left;
}

unsigned long _copy_to_user(void __user *to, const void *from, unsigned long n)
{
	if (kpi_uaccess_kernel((const void __force *)to, n)) {
		memcpy((void __force *)to, from, n);
		return 0;
	}
	return rustos_kpi_copy_to_user((void __force *)to, from, n);
}

/* --------------------------------------------------- page protections */

/*
 * The PAT layout src/mm/mod.rs programs (Linux's): entry 0 WB, 1 WC,
 * 2 UC-, 3 UC, 4 WB, 5 WP, 6 UC-, 7 WT.
 */
uint16_t __cachemode2pte_tbl[_PAGE_CACHE_MODE_NUM] = {
	[_PAGE_CACHE_MODE_WB] = 0,
	[_PAGE_CACHE_MODE_WC] = _PAGE_PWT,
	[_PAGE_CACHE_MODE_UC_MINUS] = _PAGE_PCD,
	[_PAGE_CACHE_MODE_UC] = _PAGE_PCD | _PAGE_PWT,
	[_PAGE_CACHE_MODE_WT] = _PAGE_PCD | _PAGE_PWT | _PAGE_PAT,
	[_PAGE_CACHE_MODE_WP] = _PAGE_PWT | _PAGE_PAT,
};

uint8_t __pte2cachemode_tbl[8] = {
	[__pte2cm_idx(0)] = _PAGE_CACHE_MODE_WB,
	[__pte2cm_idx(_PAGE_PWT)] = _PAGE_CACHE_MODE_WC,
	[__pte2cm_idx(_PAGE_PCD)] = _PAGE_CACHE_MODE_UC_MINUS,
	[__pte2cm_idx(_PAGE_PWT | _PAGE_PCD)] = _PAGE_CACHE_MODE_UC,
	[__pte2cm_idx(_PAGE_PAT)] = _PAGE_CACHE_MODE_WB,
	[__pte2cm_idx(_PAGE_PWT | _PAGE_PAT)] = _PAGE_CACHE_MODE_WP,
	[__pte2cm_idx(_PAGE_PCD | _PAGE_PAT)] = _PAGE_CACHE_MODE_UC_MINUS,
	[__pte2cm_idx(_PAGE_PWT | _PAGE_PCD | _PAGE_PAT)] = _PAGE_CACHE_MODE_WT,
};

pgprot_t pgprot_writecombine(pgprot_t prot)
{
	return __pgprot((pgprot_val(prot) & ~_PAGE_CACHE_MASK) | _PAGE_PWT);
}

pgprot_t pgprot_writethrough(pgprot_t prot)
{
	return __pgprot((pgprot_val(prot) & ~_PAGE_CACHE_MASK) | _PAGE_PCD | _PAGE_PWT |
			_PAGE_PAT);
}

void *memdup_user_nul(const void __user *src, size_t len)
{
	char *p = kmalloc(len + 1, GFP_KERNEL);

	if (!p)
		return ERR_PTR(-ENOMEM);
	if (copy_from_user(p, src, len)) {
		kfree(p);
		return ERR_PTR(-EFAULT);
	}
	p[len] = '\0';
	return p;
}

void *memdup_user(const void __user *src, size_t len)
{
	void *p = kmalloc(len, GFP_KERNEL);

	if (!p)
		return ERR_PTR(-ENOMEM);
	if (copy_from_user(p, src, len)) {
		kfree(p);
		return ERR_PTR(-EFAULT);
	}
	return p;
}

/* From mm/slab_common.c. */
void kfree_sensitive(const void *p)
{
	size_t ks;
	void *mem = (void *)p;

	ks = ksize(mem);
	if (ks)
		memzero_explicit(mem, ks);
	kfree(mem);
}

/* The direct map covers all RAM; nothing else is a valid linear address. */
bool __virt_addr_valid(unsigned long x)
{
	return x >= page_offset_base && (x - page_offset_base) >> PAGE_SHIFT < max_pfn;
}

/* is_kernel_rodata(): no Linux object is read-only data here. */
char __start_rodata[0], __end_rodata[0];

/* Write combining comes from the PAT (ioremap_wc), not MTRRs. */
int arch_phys_wc_add(unsigned long base, unsigned long size)
{
	return 0;
}

void arch_phys_wc_del(int handle)
{
}

int arch_io_reserve_memtype_wc(resource_size_t start, resource_size_t size)
{
	return 0;
}

void arch_io_free_memtype_wc(resource_size_t start, resource_size_t size)
{
}

/* ------------------------------------------- vmalloc pages (videobuf2) */

void *vmalloc_user_noprof(unsigned long size)
{
	return rustos_kpi_vmalloc(size);	/* zeroed */
}

struct page *vmalloc_to_page(const void *addr)
{
	u64 phys = rustos_kpi_virt_to_phys((u64)addr);

	return phys ? pfn_to_page(phys >> PAGE_SHIFT) : NULL;
}

unsigned long vmalloc_to_pfn(const void *addr)
{
	return rustos_kpi_virt_to_phys((u64)addr) >> PAGE_SHIFT;
}

static void *kpi_vmap_pages(struct page **pages, unsigned int count)
{
	u64 *phys = kmalloc_array(count, sizeof(*phys), GFP_KERNEL);
	void *v;

	if (!phys)
		return NULL;
	for (unsigned int i = 0; i < count; i++)
		phys[i] = (u64)page_to_pfn(pages[i]) << PAGE_SHIFT;
	v = rustos_kpi_vmap(phys, count);
	kfree(phys);
	return v;
}

void *vm_map_ram(struct page **pages, unsigned int count, int node)
{
	return kpi_vmap_pages(pages, count);
}

void vm_unmap_ram(const void *mem, unsigned int count)
{
	rustos_kpi_vunmap(mem, count);
}

/* Without SPARSEMEM sections, pfn_valid() is false: user-pointer buffers
 * (pin_user_pages) are not supported. */
struct mem_section **mem_section;

int pin_user_pages_fast(unsigned long start, int nr_pages, unsigned int gup_flags,
			struct page **pages)
{
	return -EFAULT;
}

void unpin_user_pages(struct page **pages, unsigned long npages)
{
}

int set_page_dirty_lock(struct page *page)
{
	return 0;
}

/* Per-VMA locks: mappings are set up under the file's own locking. */
void __vma_start_write(struct vm_area_struct *vma, unsigned int mm_lock_seq)
{
}

char *strndup_user(const char __user *s, long n)
{
	char *p = kmalloc(n + 1, GFP_KERNEL);
	long len;

	if (!p)
		return ERR_PTR(-ENOMEM);
	if (copy_from_user(p, s, n)) {
		kfree(p);
		return ERR_PTR(-EFAULT);
	}
	p[n] = 0;
	len = strnlen(p, n);
	if (len == n) {
		kfree(p);
		return ERR_PTR(-EINVAL);
	}
	return p;
}

size_t memweight(const void *ptr, size_t bytes)
{
	const u8 *p = ptr;
	size_t w = 0;

	while (bytes--)
		w += hweight8(*p++);
	return w;
}

/* ----------------------------------------------- more memory (M34 sound) */

void *alloc_pages_exact_noprof(size_t size, gfp_t gfp_mask)
{
	struct page *page = alloc_pages(gfp_mask, get_order(size));

	return page ? page_address(page) : NULL;
}

void free_pages_exact(void *virt, size_t size)
{
	if (virt)
		free_pages((unsigned long)virt, get_order(size));
}

/* vmap()ed areas and their page counts, for vunmap(). */
struct kpi_vmap_area {
	struct list_head list;
	const void *addr;
	unsigned int count;
};

static LIST_HEAD(kpi_vmap_areas);
static DEFINE_SPINLOCK(kpi_vmap_lock);

void *vmap(struct page **pages, unsigned int count, unsigned long flags, pgprot_t prot)
{
	struct kpi_vmap_area *a = kmalloc(sizeof(*a), GFP_KERNEL);
	void *v;

	if (!a)
		return NULL;
	v = kpi_vmap_pages(pages, count);
	if (!v) {
		kfree(a);
		return NULL;
	}
	a->addr = v;
	a->count = count;
	spin_lock(&kpi_vmap_lock);
	list_add(&a->list, &kpi_vmap_areas);
	spin_unlock(&kpi_vmap_lock);
	return v;
}

void vunmap(const void *addr)
{
	struct kpi_vmap_area *a, *found = NULL;

	spin_lock(&kpi_vmap_lock);
	list_for_each_entry(a, &kpi_vmap_areas, list) {
		if (a->addr == addr) {
			list_del(&a->list);
			found = a;
			break;
		}
	}
	spin_unlock(&kpi_vmap_lock);
	if (found) {
		rustos_kpi_vunmap(addr, found->count);
		kfree(found);
	}
}

void *vmemdup_user(const void __user *src, size_t len)
{
	void *p = kvmalloc(len, GFP_USER);

	if (!p)
		return ERR_PTR(-ENOMEM);
	if (copy_from_user(p, src, len)) {
		kvfree(p);
		return ERR_PTR(-EFAULT);
	}
	return p;
}

pgprot_t vm_get_page_prot(vm_flags_t vm_flags)
{
	if (vm_flags & VM_SHARED)
		return (vm_flags & VM_WRITE) ? PAGE_SHARED : PAGE_READONLY;
	return (vm_flags & VM_WRITE) ? PAGE_COPY : PAGE_READONLY;
}

/* RAM stays write-back: page attributes of the direct map are not changed. */
int set_memory_wb(unsigned long addr, int numpages)
{
	return 0;
}

int set_memory_wc(unsigned long addr, int numpages)
{
	return 0;
}

pteval_t __default_kernel_pte_mask __read_mostly = ~0;

/* No special SRAM pools (ALSA's "IRAM" buffers fall back to normal pages). */
void *gen_pool_dma_alloc_align(struct gen_pool *pool, size_t size, dma_addr_t *dma, int align)
{
	return NULL;
}

void gen_pool_free_owner(struct gen_pool *pool, unsigned long addr, size_t size, void **owner)
{
}
