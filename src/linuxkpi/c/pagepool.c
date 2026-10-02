// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * LinuxKPI page pools (net/core/page_pool.c), simplified: every
 * allocation is a fresh page (or compound page) with a pool reference
 * count of 1, so it is freed either by the driver returning it to the pool
 * or by the skb that took it (put_page in the skb free path). There is no
 * IOMMU, so a page's DMA address is its physical address and DMA syncs are
 * no-ops.
 */
#include <linux/dma-mapping.h>
#include <linux/mm.h>
#include <linux/slab.h>
#include <net/page_pool/helpers.h>
#include <net/page_pool/types.h>

struct page_pool *page_pool_create(const struct page_pool_params *params)
{
	struct page_pool *pool = kzalloc(sizeof(*pool), GFP_KERNEL);

	if (!pool)
		return ERR_PTR(-ENOMEM);
	pool->p = params->fast;
	pool->slow = params->slow;
	pool->dma_map = !!(params->slow.flags & PP_FLAG_DMA_MAP);
	return pool;
}

void page_pool_destroy(struct page_pool *pool)
{
	if (!IS_ERR_OR_NULL(pool))
		kfree(pool);
}

static struct page *kpi_pp_alloc(struct page_pool *pool, unsigned int order, gfp_t gfp)
{
	struct page *page = alloc_pages(gfp | (order ? __GFP_COMP : 0), order);

	if (!page)
		return NULL;
	page->pp_magic = PP_SIGNATURE;
	page->pp = pool;
	page->dma_addr = page_to_phys(page);
	atomic_long_set(&page->pp_ref_count, 1);
	return page;
}

struct page *page_pool_alloc_pages(struct page_pool *pool, gfp_t gfp)
{
	return kpi_pp_alloc(pool, pool->p.order, gfp);
}

netmem_ref page_pool_alloc_netmems(struct page_pool *pool, gfp_t gfp)
{
	struct page *page = page_pool_alloc_pages(pool, gfp);

	return page ? page_to_netmem(page) : 0;
}

struct page *page_pool_alloc_frag(struct page_pool *pool, unsigned int *offset,
				  unsigned int size, gfp_t gfp)
{
	unsigned int order = max_t(unsigned int, pool->p.order, get_order(size));

	*offset = 0;
	return kpi_pp_alloc(pool, order, gfp);
}

netmem_ref page_pool_alloc_frag_netmem(struct page_pool *pool, unsigned int *offset,
				       unsigned int size, gfp_t gfp)
{
	struct page *page = page_pool_alloc_frag(pool, offset, size, gfp);

	return page ? page_to_netmem(page) : 0;
}

void page_pool_put_unrefed_netmem(struct page_pool *pool, netmem_ref netmem,
				  unsigned int dma_sync_size, bool allow_direct)
{
	struct page *page = netmem_to_page(netmem);

	page->pp_magic = 0;
	page->pp = NULL;
	put_page(page);
}

void page_pool_put_unrefed_page(struct page_pool *pool, struct page *page,
				unsigned int dma_sync_size, bool allow_direct)
{
	page_pool_put_unrefed_netmem(pool, page_to_netmem(page), dma_sync_size, allow_direct);
}
