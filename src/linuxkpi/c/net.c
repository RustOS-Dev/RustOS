// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * LinuxKPI networking: net_device registration bridged to RustOS
 * interfaces (src/linuxkpi/net.rs), NAPI polled from the LinuxKPI softirq
 * thread, and the sk_buff core drivers use.
 *
 * Linux's net/core is not compiled: RustOS has its own stack (smoltcp),
 * so a received skb is copied out as an Ethernet frame for it, and a
 * frame RustOS sends becomes a fresh linear skb for ndo_start_xmit().
 * The skb functions follow net/core/skbuff.c for linear skbs with page
 * fragments; clones and frag lists are not supported.
 */
#include <linux/etherdevice.h>
#include <linux/ethtool.h>
#include <linux/if_arp.h>
#include <linux/if_ether.h>
#include <linux/netdevice.h>
#include <linux/rtnetlink.h>
#include <linux/skbuff.h>
#include <linux/slab.h>
#include <net/checksum.h>
#include "kpi.h"

DEFINE_PER_CPU_ALIGNED(struct softnet_data, softnet_data);

static DEFINE_MUTEX(rtnl_mutex);

void rtnl_lock(void)
{
	mutex_lock(&rtnl_mutex);
}

void rtnl_unlock(void)
{
	mutex_unlock(&rtnl_mutex);
}

int rtnl_trylock(void)
{
	return mutex_trylock(&rtnl_mutex);
}

int rtnl_is_locked(void)
{
	return mutex_is_locked(&rtnl_mutex);
}

/* ---------------------------------------------------------- sk_buff core */

static void kpi_skb_init(struct sk_buff *skb, void *data, unsigned int size)
{
	struct skb_shared_info *shinfo;

	size -= SKB_DATA_ALIGN(sizeof(struct skb_shared_info));
	memset(skb, 0, offsetof(struct sk_buff, tail));
	skb->truesize = SKB_TRUESIZE(size);
	refcount_set(&skb->users, 1);
	skb->head = data;
	skb->data = data;
	skb_reset_tail_pointer(skb);
	skb_set_end_offset(skb, size);
	skb->mac_header = (typeof(skb->mac_header))~0U;
	skb->transport_header = (typeof(skb->transport_header))~0U;
	skb->alloc_cpu = raw_smp_processor_id();
	shinfo = skb_shinfo(skb);
	memset(shinfo, 0, offsetof(struct skb_shared_info, dataref));
	atomic_set(&shinfo->dataref, 1);
}

struct sk_buff *__alloc_skb(unsigned int size, gfp_t gfp, int flags, int node)
{
	struct sk_buff *skb = kmalloc(sizeof(*skb), gfp & ~__GFP_DMA);
	void *data;

	if (!skb)
		return NULL;
	size = SKB_DATA_ALIGN(size) + SKB_DATA_ALIGN(sizeof(struct skb_shared_info));
	size = kmalloc_size_roundup(size);
	data = kmalloc(size, gfp);
	if (!data) {
		kfree(skb);
		return NULL;
	}
	kpi_skb_init(skb, data, size);
	return skb;
}

struct sk_buff *napi_build_skb(void *data, unsigned int frag_size)
{
	struct sk_buff *skb = kmalloc(sizeof(*skb), GFP_ATOMIC);

	if (!skb)
		return NULL;
	kpi_skb_init(skb, data, frag_size ?: ksize(data));
	skb->head_frag = frag_size != 0;
	return skb;
}

struct sk_buff *build_skb(void *data, unsigned int frag_size)
{
	return napi_build_skb(data, frag_size);
}

static void kpi_skb_free_head(struct sk_buff *skb)
{
	if (skb->head_frag)
		page_frag_free(skb->head);
	else
		kfree(skb->head);
}

static void kpi_skb_release(struct sk_buff *skb)
{
	struct skb_shared_info *sh = skb_shinfo(skb);

	if (skb->destructor)
		skb->destructor(skb);
	if (skb->cloned && atomic_dec_return(&sh->dataref))
		return;
	for (int i = 0; i < sh->nr_frags; i++)
		put_page(skb_frag_page(&sh->frags[i]));
	kpi_skb_free_head(skb);
}

static void kpi_kfree_skb(struct sk_buff *skb)
{
	if (!skb || !skb_unref(skb))
		return;
	kpi_skb_release(skb);
	kfree(skb);
}

void __fix_address sk_skb_reason_drop(struct sock *sk, struct sk_buff *skb,
				      enum skb_drop_reason reason)
{
	kpi_kfree_skb(skb);
}

#ifdef CONFIG_TRACEPOINTS
void consume_skb(struct sk_buff *skb)
{
	kpi_kfree_skb(skb);
}
#endif

void dev_kfree_skb_any_reason(struct sk_buff *skb, enum skb_drop_reason reason)
{
	kpi_kfree_skb(skb);
}

void dev_kfree_skb_irq_reason(struct sk_buff *skb, enum skb_drop_reason reason)
{
	kpi_kfree_skb(skb);
}

void napi_consume_skb(struct sk_buff *skb, int budget)
{
	kpi_kfree_skb(skb);
}

void skb_tstamp_tx(struct sk_buff *orig_skb, struct skb_shared_hwtstamps *hwtstamps)
{
}

void *skb_put(struct sk_buff *skb, unsigned int len)
{
	void *tmp = skb_tail_pointer(skb);

	SKB_LINEAR_ASSERT(skb);
	skb->tail += len;
	skb->len += len;
	if (unlikely(skb->tail > skb->end))
		panic("skb_put: over end (len %u)", len);
	return tmp;
}

void *skb_push(struct sk_buff *skb, unsigned int len)
{
	skb->data -= len;
	skb->len += len;
	if (unlikely(skb->data < skb->head))
		panic("skb_push: under head (len %u)", len);
	return skb->data;
}

void *skb_pull(struct sk_buff *skb, unsigned int len)
{
	return skb_pull_inline(skb, len);
}

void skb_trim(struct sk_buff *skb, unsigned int len)
{
	if (skb->len > len)
		__skb_trim(skb, len);
}

int ___pskb_trim(struct sk_buff *skb, unsigned int len)
{
	struct skb_shared_info *sh = skb_shinfo(skb);
	unsigned int headlen = skb_headlen(skb), offset = headlen;
	int i;

	if (len <= headlen) {
		for (i = 0; i < sh->nr_frags; i++)
			put_page(skb_frag_page(&sh->frags[i]));
		sh->nr_frags = 0;
		skb->data_len = 0;
		skb->len = len;
		skb_set_tail_pointer(skb, len);
		return 0;
	}
	for (i = 0; i < sh->nr_frags; i++) {
		unsigned int end = offset + skb_frag_size(&sh->frags[i]);

		if (end < len) {
			offset = end;
			continue;
		}
		skb_frag_size_set(&sh->frags[i++], len - offset);
		break;
	}
	for (int j = i; j < sh->nr_frags; j++)
		put_page(skb_frag_page(&sh->frags[j]));
	sh->nr_frags = i;
	skb->data_len = len - headlen;
	skb->len = len;
	return 0;
}

int skb_copy_bits(const struct sk_buff *skb, int offset, void *to, int len)
{
	int start = skb_headlen(skb), copy;
	struct skb_shared_info *sh = skb_shinfo(skb);

	if (offset > (int)skb->len - len)
		return -EFAULT;
	copy = start - offset;
	if (copy > 0) {
		copy = min(copy, len);
		memcpy(to, skb->data + offset, copy);
		len -= copy;
		offset += copy;
		to += copy;
	}
	for (int i = 0; i < sh->nr_frags && len > 0; i++) {
		skb_frag_t *f = &sh->frags[i];
		int end = start + skb_frag_size(f);

		copy = end - offset;
		if (copy > 0) {
			copy = min(copy, len);
			memcpy(to, page_address(skb_frag_page(f)) + skb_frag_off(f) + offset - start,
			       copy);
			len -= copy;
			offset += copy;
			to += copy;
		}
		start = end;
	}
	return len ? -EFAULT : 0;
}

/* From net/core/skbuff.c. */
void skb_headers_offset_update(struct sk_buff *skb, int off)
{
	/* Only adjust this if it actually is csum_start rather than csum */
	if (skb->ip_summed == CHECKSUM_PARTIAL)
		skb->csum_start += off;
	/* {transport,network,mac}_header and tail are relative to skb->head */
	skb->transport_header += off;
	skb->network_header += off;
	if (skb_mac_header_was_set(skb))
		skb->mac_header += off;
	skb->inner_transport_header += off;
	skb->inner_network_header += off;
	skb->inner_mac_header += off;
}

int pskb_expand_head(struct sk_buff *skb, int nhead, int ntail, gfp_t gfp)
{
	unsigned int osize = skb_end_offset(skb);
	unsigned int size = osize + nhead + ntail;
	struct skb_shared_info *sh = skb_shinfo(skb);
	long off;
	u8 *data;

	size = SKB_DATA_ALIGN(size);
	size = kmalloc_size_roundup(size + SKB_DATA_ALIGN(sizeof(struct skb_shared_info)));
	data = kmalloc(size, gfp);
	if (!data)
		return -ENOMEM;
	size = SKB_WITH_OVERHEAD(size);
	memcpy(data + nhead, skb->head, skb_tail_pointer(skb) - skb->head);
	memcpy((struct skb_shared_info *)(data + size), sh,
	       offsetof(struct skb_shared_info, frags[sh->nr_frags]));
	kpi_skb_free_head(skb);
	off = (data + nhead) - skb->head;
	skb->head = data;
	skb->head_frag = 0;
	skb->data += off;
	skb_set_end_offset(skb, size);
	skb->tail += nhead;
	skb_headers_offset_update(skb, nhead);
	skb->cloned = 0;
	skb->hdr_len = 0;
	skb->nohdr = 0;
	atomic_set(&skb_shinfo(skb)->dataref, 1);
	return 0;
}

void *__pskb_pull_tail(struct sk_buff *skb, int delta)
{
	struct skb_shared_info *sh;
	int eat = (skb->tail + delta) - skb->end;
	int i, k;

	if (eat > 0 && pskb_expand_head(skb, 0, eat + 128, GFP_ATOMIC))
		return NULL;
	if (skb_copy_bits(skb, skb_headlen(skb), skb_tail_pointer(skb), delta))
		return NULL;
	sh = skb_shinfo(skb);
	eat = delta;
	for (i = k = 0; i < sh->nr_frags; i++) {
		int size = skb_frag_size(&sh->frags[i]);

		if (size <= eat) {
			put_page(skb_frag_page(&sh->frags[i]));
			eat -= size;
		} else {
			sh->frags[k] = sh->frags[i];
			if (eat) {
				skb_frag_off_add(&sh->frags[k], eat);
				skb_frag_size_sub(&sh->frags[k], eat);
				eat = 0;
			}
			k++;
		}
	}
	sh->nr_frags = k;
	skb->tail += delta;
	skb->data_len -= delta;
	return skb_tail_pointer(skb);
}

int __skb_pad(struct sk_buff *skb, int pad, bool free_on_error)
{
	int err, ntail;

	if (!skb_cloned(skb) && skb_tailroom(skb) >= pad) {
		memset(skb->data + skb->len, 0, pad);
		return 0;
	}
	ntail = skb->data_len + pad - (skb->end - skb->tail);
	if (ntail > 0) {
		err = pskb_expand_head(skb, 0, ntail, GFP_ATOMIC);
		if (err)
			goto free;
	}
	err = skb_linearize(skb);
	if (err)
		goto free;
	memset(skb->data + skb->len, 0, pad);
	return 0;
free:
	if (free_on_error)
		kpi_kfree_skb(skb);
	return err;
}

/* Page fragments for receive buffers: each fragment holds a page reference;
 * the cache holds one more until it moves on to a new page. */
static DEFINE_SPINLOCK(kpi_frag_lock);
static struct page *kpi_frag_page;
static unsigned int kpi_frag_offset;

void *__netdev_alloc_frag_align(unsigned int fragsz, unsigned int align_mask)
{
	unsigned long flags;
	void *addr;

	fragsz = SKB_DATA_ALIGN(fragsz);
	if (fragsz > PAGE_SIZE)
		return NULL;
	spin_lock_irqsave(&kpi_frag_lock, flags);
	kpi_frag_offset = (kpi_frag_offset + ~align_mask) & align_mask;
	if (!kpi_frag_page || kpi_frag_offset + fragsz > PAGE_SIZE) {
		if (kpi_frag_page)
			put_page(kpi_frag_page);
		kpi_frag_page = alloc_page(GFP_ATOMIC);
		kpi_frag_offset = 0;
		if (!kpi_frag_page) {
			spin_unlock_irqrestore(&kpi_frag_lock, flags);
			return NULL;
		}
	}
	addr = page_address(kpi_frag_page) + kpi_frag_offset;
	kpi_frag_offset += fragsz;
	get_page(kpi_frag_page);
	spin_unlock_irqrestore(&kpi_frag_lock, flags);
	return addr;
}

void *__napi_alloc_frag_align(unsigned int fragsz, unsigned int align_mask)
{
	return __netdev_alloc_frag_align(fragsz, align_mask);
}

struct sk_buff *__netdev_alloc_skb(struct net_device *dev, unsigned int len, gfp_t gfp)
{
	struct sk_buff *skb = __alloc_skb(len + NET_SKB_PAD, gfp, 0, NUMA_NO_NODE);

	if (skb) {
		skb_reserve(skb, NET_SKB_PAD);
		skb->dev = dev;
	}
	return skb;
}

struct sk_buff *napi_alloc_skb(struct napi_struct *napi, unsigned int length)
{
	struct sk_buff *skb = __alloc_skb(length + NET_SKB_PAD + NET_IP_ALIGN, GFP_ATOMIC, 0,
					  NUMA_NO_NODE);

	if (skb) {
		skb_reserve(skb, NET_SKB_PAD + NET_IP_ALIGN);
		skb->dev = napi->dev;
	}
	return skb;
}

/* --------------------------------------------------------- net_device */

#define KPI_MAX_NETDEVS 16
static struct { struct net_device *dev; u64 handle; bool opened; } kpi_netdevs[KPI_MAX_NETDEVS];

static u64 kpi_netdev_handle(const struct net_device *dev)
{
	for (int i = 0; i < KPI_MAX_NETDEVS; i++)
		if (kpi_netdevs[i].dev == dev)
			return kpi_netdevs[i].handle;
	return 0;
}

struct net_device *alloc_etherdev_mqs(int sizeof_priv, unsigned int txqs, unsigned int rxqs)
{
	struct net_device *dev = kzalloc(struct_size(dev, priv, sizeof_priv), GFP_KERNEL);

	if (!dev)
		return NULL;
	dev->priv_len = sizeof_priv;
	dev->_tx = kcalloc(txqs, sizeof(struct netdev_queue), GFP_KERNEL);
	dev->dev_addr = kzalloc(MAX_ADDR_LEN, GFP_KERNEL);
	if (!dev->_tx || !dev->dev_addr) {
		kfree(dev->_tx);
		kfree(dev->dev_addr);
		kfree(dev);
		return NULL;
	}
	dev->num_tx_queues = dev->real_num_tx_queues = txqs;
	dev->num_rx_queues = dev->real_num_rx_queues = rxqs;
	for (unsigned int i = 0; i < txqs; i++) {
		struct netdev_queue *q = &dev->_tx[i];

		q->dev = dev;
		spin_lock_init(&q->_xmit_lock);
		q->xmit_lock_owner = -1;
#ifdef CONFIG_BQL
		dql_init(&q->dql, HZ);
#endif
	}
	/* ether_setup() */
	dev->type = ARPHRD_ETHER;
	dev->hard_header_len = ETH_HLEN;
	dev->min_header_len = ETH_HLEN;
	dev->mtu = ETH_DATA_LEN;
	dev->min_mtu = ETH_MIN_MTU;
	dev->max_mtu = ETH_DATA_LEN;
	dev->addr_len = ETH_ALEN;
	dev->tx_queue_len = 1000;	/* DEFAULT_TX_QUEUE_LEN */
	dev->flags = IFF_BROADCAST | IFF_MULTICAST;
	dev->priv_flags |= IFF_TX_SKB_SHARING;
	eth_broadcast_addr(dev->broadcast);

	strscpy(dev->name, "eth%d", sizeof(dev->name));
	INIT_LIST_HEAD(&dev->napi_list);
	INIT_LIST_HEAD(&dev->uc.list);
	INIT_LIST_HEAD(&dev->mc.list);
	set_bit(__LINK_STATE_PRESENT, &dev->state);
	return dev;
}

void free_netdev(struct net_device *dev)
{
	if (!dev)
		return;
	kfree(dev->_tx);
	kfree(dev->dev_addr);
	kfree(dev);
}

void dev_addr_mod(struct net_device *dev, unsigned int offset, const void *addr, size_t len)
{
	memcpy((u8 *)dev->dev_addr + offset, addr, len);
}

int eth_validate_addr(struct net_device *dev)
{
	return is_valid_ether_addr(dev->dev_addr) ? 0 : -EADDRNOTAVAIL;
}

static bool kpi_netdev_is_wireless(struct net_device *dev)
{
#if IS_ENABLED(CONFIG_CFG80211)
	return dev->ieee80211_ptr;
#else
	return false;
#endif
}

void netif_tx_stop_all_queues(struct net_device *dev)
{
	for (unsigned int i = 0; i < dev->num_tx_queues; i++)
		netif_tx_stop_queue(netdev_get_tx_queue(dev, i));
}

int register_netdev(struct net_device *dev)
{
	const char *drv = dev->dev.parent ? dev_driver_string(dev->dev.parent) : "linux";
	u64 handle;

	rtnl_lock();
	dev->reg_state = NETREG_REGISTERED;
	handle = rustos_kpi_netdev_register(dev, dev->dev_addr, dev->mtu, kpi_netdev_is_wireless(dev),
					    drv, dev->name, sizeof(dev->name));
	for (int i = 0; i < KPI_MAX_NETDEVS; i++) {
		if (!kpi_netdevs[i].dev) {
			kpi_netdevs[i].dev = dev;
			kpi_netdevs[i].handle = handle;
			kpi_netdevs[i].opened = false;
			break;
		}
	}
	/* No carrier until opened (kpi_netdev_open_pending). */
	rustos_kpi_netdev_carrier(handle, 0);
	rtnl_unlock();
	return 0;
}

/*
 * RustOS interfaces are up once registered, so open them, but only after
 * the registering probe (or initcall) has returned: drivers still set up
 * state after register_netdev() (e1000 calls netif_carrier_off(), for one),
 * as in Linux, where user space opens interfaces later.
 */
void kpi_netdev_open_pending(void)
{
	rtnl_lock();
	for (int i = 0; i < KPI_MAX_NETDEVS; i++) {
		struct net_device *dev = kpi_netdevs[i].dev;
		int err = 0;

		if (!dev || kpi_netdevs[i].opened)
			continue;
		kpi_netdevs[i].opened = true;
		if (dev->netdev_ops->ndo_open)
			err = dev->netdev_ops->ndo_open(dev);
		if (err) {
			netdev_err(dev, "open failed: %d\n", err);
			continue;
		}
		set_bit(__LINK_STATE_START, &dev->state);
		dev->flags |= IFF_UP;
		rustos_kpi_netdev_carrier(kpi_netdevs[i].handle, netif_carrier_ok(dev));
	}
	rtnl_unlock();
}

void unregister_netdev(struct net_device *dev)
{
	rtnl_lock();
	if (test_and_clear_bit(__LINK_STATE_START, &dev->state) && dev->netdev_ops->ndo_stop)
		dev->netdev_ops->ndo_stop(dev);
	dev->reg_state = NETREG_UNREGISTERED;
	rtnl_unlock();
}

/* RustOS shutdown path (src/linuxkpi/net.rs). */
void kpi_netdev_stop(struct net_device *dev)
{
	if (test_and_clear_bit(__LINK_STATE_START, &dev->state) && dev->netdev_ops->ndo_stop)
		dev->netdev_ops->ndo_stop(dev);
}

void netif_carrier_on(struct net_device *dev)
{
	if (test_and_clear_bit(__LINK_STATE_NOCARRIER, &dev->state) &&
	    dev->reg_state == NETREG_REGISTERED)
		rustos_kpi_netdev_carrier(kpi_netdev_handle(dev), 1);
}

void netif_carrier_off(struct net_device *dev)
{
	if (!test_and_set_bit(__LINK_STATE_NOCARRIER, &dev->state) &&
	    dev->reg_state == NETREG_REGISTERED)
		rustos_kpi_netdev_carrier(kpi_netdev_handle(dev), 0);
}

void netif_device_detach(struct net_device *dev)
{
	if (test_and_clear_bit(__LINK_STATE_PRESENT, &dev->state) && netif_running(dev))
		netif_tx_stop_all_queues(dev);
}

void netif_device_attach(struct net_device *dev)
{
	if (!test_and_set_bit(__LINK_STATE_PRESENT, &dev->state) && netif_running(dev))
		netif_tx_wake_all_queues(dev);
}

void netif_tx_wake_queue(struct netdev_queue *dev_queue)
{
	clear_bit(__QUEUE_STATE_DRV_XOFF, &dev_queue->state);
}

void netif_schedule_queue(struct netdev_queue *txq)
{
}

void netif_queue_set_napi(struct net_device *dev, unsigned int queue_index,
			  enum netdev_queue_type type, struct napi_struct *napi)
{
}

/* Frames from RustOS (src/linuxkpi/net.rs): a new linear skb each. */
int kpi_netdev_xmit(struct net_device *dev, const u8 *data, u32 len)
{
	struct netdev_queue *txq = netdev_get_tx_queue(dev, 0);
	struct sk_buff *skb;
	netdev_tx_t rc = NETDEV_TX_BUSY;

	if (!netif_running(dev) || !netif_carrier_ok(dev))
		return -ENETDOWN;
	skb = __netdev_alloc_skb(dev, len, GFP_ATOMIC);
	if (!skb)
		return -ENOMEM;
	skb_put_data(skb, data, len);
	skb_reset_mac_header(skb);
	skb_set_network_header(skb, ETH_HLEN);
	skb->protocol = ((const struct ethhdr *)data)->h_proto;
	skb->ip_summed = CHECKSUM_NONE;
	skb_set_queue_mapping(skb, 0);

	local_bh_disable();
	__netif_tx_lock(txq, smp_processor_id());
	if (!netif_xmit_frozen_or_stopped(txq))
		rc = dev->netdev_ops->ndo_start_xmit(skb, dev);
	__netif_tx_unlock(txq);
	local_bh_enable();
	if (rc != NETDEV_TX_OK) {
		kpi_kfree_skb(skb);
		return -EBUSY;
	}
	return 0;
}

/* Hand a received skb to RustOS as an Ethernet frame and free it. */
static void kpi_netif_deliver(struct sk_buff *skb)
{
	struct net_device *dev = skb->dev;
	unsigned char *mac;
	unsigned int len;

	if (!dev || skb_linearize(skb)) {
		kpi_kfree_skb(skb);
		return;
	}
	mac = skb_mac_header_was_set(skb) ? skb_mac_header(skb) : skb->data;
	len = skb->len + (skb->data - mac);
	rustos_kpi_netdev_rx(kpi_netdev_handle(dev), mac, len);
	kpi_kfree_skb(skb);
}

gro_result_t gro_receive_skb(struct gro_node *gro, struct sk_buff *skb)
{
	kpi_netif_deliver(skb);
	return GRO_NORMAL;
}

int netif_receive_skb(struct sk_buff *skb)
{
	kpi_netif_deliver(skb);
	return NET_RX_SUCCESS;
}

int netif_rx(struct sk_buff *skb)
{
	kpi_netif_deliver(skb);
	return NET_RX_SUCCESS;
}

__be16 eth_type_trans(struct sk_buff *skb, struct net_device *dev)
{
	const struct ethhdr *eth;

	skb->dev = dev;
	skb_reset_mac_header(skb);
	eth = (struct ethhdr *)skb->data;
	skb_pull_inline(skb, ETH_HLEN);
	if (unlikely(!ether_addr_equal(eth->h_dest, dev->dev_addr))) {
		if (is_multicast_ether_addr(eth->h_dest))
			skb->pkt_type = ether_addr_equal(eth->h_dest, dev->broadcast) ?
					PACKET_BROADCAST : PACKET_MULTICAST;
		else
			skb->pkt_type = PACKET_OTHERHOST;
	}
	if (likely(eth_proto_is_802_3(eth->h_proto)))
		return eth->h_proto;
	if (unlikely(*(unsigned short *)skb->data == 0xFFFF))
		return htons(ETH_P_802_3);
	return htons(ETH_P_802_2);
}

/* -------------------------------------------------------------------- NAPI */

static DEFINE_SPINLOCK(kpi_napi_lock);
static LIST_HEAD(kpi_napi_list);

static void kpi_napi_run(void)
{
	for (int round = 0; round < 16; round++) {
		struct napi_struct *n;
		unsigned long flags;
		int work;

		spin_lock_irqsave(&kpi_napi_lock, flags);
		n = list_first_entry_or_null(&kpi_napi_list, struct napi_struct, poll_list);
		if (n)
			list_del_init(&n->poll_list);
		spin_unlock_irqrestore(&kpi_napi_lock, flags);
		if (!n)
			return;
		work = n->poll(n, n->weight);
		/* A poll that used its whole budget stays scheduled. */
		if (work >= n->weight && test_bit(NAPI_STATE_SCHED, &n->state)) {
			spin_lock_irqsave(&kpi_napi_lock, flags);
			if (list_empty(&n->poll_list))
				list_add_tail(&n->poll_list, &kpi_napi_list);
			spin_unlock_irqrestore(&kpi_napi_lock, flags);
		}
	}
	/* More work left: run again. */
	if (!list_empty(&kpi_napi_list))
		rustos_kpi_softirq_raise();
}

void kpi_softirq_register(void (*fn)(void));

int kpi_net_init(void)
{
	kpi_softirq_register(kpi_napi_run);
	return 0;
}

void netif_napi_add_weight_locked(struct net_device *dev, struct napi_struct *napi,
				  int (*poll)(struct napi_struct *, int), int weight)
{
	INIT_LIST_HEAD(&napi->poll_list);
	napi->poll = poll;
	napi->weight = weight;
	napi->dev = dev;
	/* Created disabled, as in Linux. */
	set_bit(NAPI_STATE_SCHED, &napi->state);
	set_bit(NAPI_STATE_NPSVC, &napi->state);
	list_add_rcu(&napi->dev_list, &dev->napi_list);
}

void netif_napi_set_irq_locked(struct napi_struct *napi, int irq)
{
	napi->irq = irq;
}

void __netif_napi_del_locked(struct napi_struct *napi)
{
	list_del_init(&napi->dev_list);
}

void napi_enable(struct napi_struct *n)
{
	clear_bit(NAPI_STATE_NPSVC, &n->state);
	smp_mb__before_atomic();
	clear_bit(NAPI_STATE_SCHED, &n->state);
}

void napi_disable(struct napi_struct *n)
{
	set_bit(NAPI_STATE_DISABLE, &n->state);
	while (test_and_set_bit(NAPI_STATE_SCHED, &n->state))
		msleep(1);
	set_bit(NAPI_STATE_NPSVC, &n->state);
	clear_bit(NAPI_STATE_DISABLE, &n->state);
}

bool napi_schedule_prep(struct napi_struct *n)
{
	unsigned long new, val = READ_ONCE(n->state);

	do {
		if (unlikely(val & NAPIF_STATE_DISABLE))
			return false;
		new = val | NAPIF_STATE_SCHED;
		/* Already scheduled: remember to poll again (MISSED). */
		new |= (val & NAPIF_STATE_SCHED) / NAPIF_STATE_SCHED * NAPIF_STATE_MISSED;
	} while (!try_cmpxchg(&n->state, &val, new));
	return !(val & NAPIF_STATE_SCHED);
}

void __napi_schedule(struct napi_struct *n)
{
	unsigned long flags;

	spin_lock_irqsave(&kpi_napi_lock, flags);
	if (list_empty(&n->poll_list))
		list_add_tail(&n->poll_list, &kpi_napi_list);
	spin_unlock_irqrestore(&kpi_napi_lock, flags);
	rustos_kpi_softirq_raise();
}

void __napi_schedule_irqoff(struct napi_struct *n)
{
	__napi_schedule(n);
}

bool napi_complete_done(struct napi_struct *n, int work_done)
{
	unsigned long new, val = READ_ONCE(n->state);

	do {
		new = val & ~(NAPIF_STATE_MISSED | NAPIF_STATE_SCHED);
		/* MISSED set: stay scheduled. */
		new |= (val & NAPIF_STATE_MISSED) / NAPIF_STATE_MISSED * NAPIF_STATE_SCHED;
	} while (!try_cmpxchg(&n->state, &val, new));
	if (unlikely(val & NAPIF_STATE_MISSED)) {
		__napi_schedule(n);
		return false;
	}
	return true;
}

struct sk_buff *napi_get_frags(struct napi_struct *napi)
{
	if (!napi->skb)
		napi->skb = napi_alloc_skb(napi, MAX_HEADER + 128)	/* GRO_MAX_HEAD */;
	return napi->skb;
}

gro_result_t napi_gro_frags(struct napi_struct *napi)
{
	struct sk_buff *skb = napi->skb;

	napi->skb = NULL;
	if (!skb)
		return GRO_CONSUMED;
	skb->protocol = eth_type_trans(skb, napi->dev);
	kpi_netif_deliver(skb);
	return GRO_NORMAL;
}

/* ------------------------------------------------------------- helpers */

void ethtool_convert_legacy_u32_to_link_mode(unsigned long *dst, u32 legacy_u32)
{
	bitmap_zero(dst, __ETHTOOL_LINK_MODE_MASK_NBITS);
	dst[0] = legacy_u32;
}

bool ethtool_convert_link_mode_to_legacy_u32(u32 *legacy_u32, const unsigned long *src)
{
	bool ok = find_next_bit(src, __ETHTOOL_LINK_MODE_MASK_NBITS, 32) ==
		  __ETHTOOL_LINK_MODE_MASK_NBITS;

	*legacy_u32 = src[0];
	return ok;
}

int ethtool_op_get_ts_info(struct net_device *dev, struct kernel_ethtool_ts_info *info)
{
	info->so_timestamping = SOF_TIMESTAMPING_TX_SOFTWARE | SOF_TIMESTAMPING_RX_SOFTWARE |
				SOF_TIMESTAMPING_SOFTWARE;
	info->phc_index = -1;
	return 0;
}

u32 ethtool_op_get_link(struct net_device *dev)
{
	return netif_carrier_ok(dev);
}

__sum16 csum_ipv6_magic(const struct in6_addr *saddr, const struct in6_addr *daddr, __u32 len,
			__u8 proto, __wsum csum)
{
	u64 sum = (__force u32)csum;

	for (int i = 0; i < 4; i++) {
		sum += (__force u32)saddr->s6_addr32[i];
		sum += (__force u32)daddr->s6_addr32[i];
	}
	sum += (__force u32)htonl(len);
	sum += (__force u32)htonl(proto);
	sum = (sum & 0xffffffff) + (sum >> 32);
	sum = (sum & 0xffffffff) + (sum >> 32);
	return csum_fold((__force __wsum)(u32)sum);
}
