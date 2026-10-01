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
#include <linux/jhash.h>
#include <net/netdev_queues.h>
#include <net/netdev_rx_queue.h>
#include <net/checksum.h>
#include <net/dst.h>
#include <linux/utsname.h>
#include <net/net_namespace.h>
#include <net/netns/generic.h>
#include "kpi.h"

DEFINE_PER_CPU_ALIGNED(struct softnet_data, softnet_data);

static DEFINE_MUTEX(rtnl_mutex);

void rtnl_lock(void)
{
	mutex_lock(&rtnl_mutex);
}

static LIST_HEAD(kpi_net_todo);
static void kpi_netdev_run_todo(struct list_head *list);

void rtnl_unlock(void)
{
	LIST_HEAD(todo);

	list_splice_init(&kpi_net_todo, &todo);
	mutex_unlock(&rtnl_mutex);
	/* Finish unregistrations outside the lock, as netdev_run_todo(). */
	kpi_netdev_run_todo(&todo);
}

int rtnl_trylock(void)
{
	return mutex_trylock(&rtnl_mutex);
}

int rtnl_is_locked(void)
{
	return mutex_is_locked(&rtnl_mutex);
}

/* ------------------------------------------------------------ namespaces */

/*
 * One network namespace, init_net. Per-namespace state (pernet_operations)
 * is created for it when registered and torn down when unregistered.
 */
LIST_HEAD(net_namespace_list);
struct net init_net;
static DEFINE_MUTEX(kpi_pernet_lock);
/* From net/core/net_namespace.c: ids index net_generic.ptr past its header. */
#define MIN_PERNET_OPS_ID ((sizeof(struct net_generic) + sizeof(void *) - 1) / sizeof(void *))
static unsigned int kpi_pernet_next_id = MIN_PERNET_OPS_ID;
#define KPI_PERNET_IDS 64

static const void *kpi_net_initial_ns(void)
{
	return &init_net;
}

static void *kpi_net_grab_current_ns(void)
{
	return &init_net;
}

static const void *kpi_net_netlink_ns(struct sock *sk)
{
	return &init_net;
}

static void kpi_net_drop_ns(void *p)
{
}

const struct kobj_ns_type_operations net_ns_type_operations = {
	.type = KOBJ_NS_TYPE_NET,
	.grab_current_ns = kpi_net_grab_current_ns,
	.netlink_ns = kpi_net_netlink_ns,
	.initial_ns = kpi_net_initial_ns,
	.drop_ns = kpi_net_drop_ns,
};

struct uts_namespace init_uts_ns = {
	.name = {
		.sysname = "Linux",
		.nodename = "rustos",
		.release = "6.18.54-rustos",
		.version = "#1 SMP",
		.machine = "x86_64",
		.domainname = "(none)",
	},
};

static int kpi_netns_init(void)
{
	struct net_generic *ng = kzalloc(struct_size(ng, ptr, KPI_PERNET_IDS), GFP_KERNEL);

	if (!ng)
		return -ENOMEM;
	ng->s.len = KPI_PERNET_IDS;
	rcu_assign_pointer(init_net.gen, ng);
	refcount_set(&init_net.ns.__ns_ref, 1);
	refcount_set(&init_net.passive, 1);
	INIT_LIST_HEAD(&init_net.dev_base_head);
	INIT_LIST_HEAD(&init_net.exit_list);
	list_add_tail_rcu(&init_net.list, &net_namespace_list);
	/* As net_ns_init(): classes tagged with network namespaces (ieee80211,
	 * net) need the type registered. */
	return kobj_ns_type_register(&net_ns_type_operations);
}

static int kpi_register_pernet(struct pernet_operations *ops)
{
	struct net_generic *ng = rcu_dereference_protected(init_net.gen, true);
	unsigned int id = 0;
	int err = 0;

	mutex_lock(&kpi_pernet_lock);
	if (ops->id) {
		if (kpi_pernet_next_id >= KPI_PERNET_IDS) {
			err = -ENOSPC;
			goto out;
		}
		id = *ops->id = kpi_pernet_next_id++;
		if (ops->size) {
			ng->ptr[id] = kzalloc(ops->size, GFP_KERNEL);
			if (!ng->ptr[id]) {
				err = -ENOMEM;
				goto out;
			}
		}
	}
	if (ops->init)
		err = ops->init(&init_net);
	if (err && id && ops->size) {
		kfree(ng->ptr[id]);
		ng->ptr[id] = NULL;
	}
out:
	mutex_unlock(&kpi_pernet_lock);
	return err;
}

static void kpi_unregister_pernet(struct pernet_operations *ops)
{
	struct net_generic *ng = rcu_dereference_protected(init_net.gen, true);
	LIST_HEAD(nets);

	mutex_lock(&kpi_pernet_lock);
	list_add(&init_net.exit_list, &nets);
	if (ops->pre_exit)
		ops->pre_exit(&init_net);
	if (ops->exit_rtnl) {
		LIST_HEAD(kill);

		rtnl_lock();
		ops->exit_rtnl(&init_net, &kill);
		unregister_netdevice_many(&kill);
		rtnl_unlock();
	}
	if (ops->exit)
		ops->exit(&init_net);
	if (ops->exit_batch)
		ops->exit_batch(&nets);
	list_del_init(&init_net.exit_list);
	if (ops->id && ops->size) {
		kfree(ng->ptr[*ops->id]);
		ng->ptr[*ops->id] = NULL;
	}
	mutex_unlock(&kpi_pernet_lock);
}

int register_pernet_subsys(struct pernet_operations *ops)
{
	return kpi_register_pernet(ops);
}

void unregister_pernet_subsys(struct pernet_operations *ops)
{
	kpi_unregister_pernet(ops);
}

int register_pernet_device(struct pernet_operations *ops)
{
	return kpi_register_pernet(ops);
}

void unregister_pernet_device(struct pernet_operations *ops)
{
	kpi_unregister_pernet(ops);
}

/* init_net is never freed. */
void __put_net(struct net *net)
{
}

struct net *get_net_ns_by_fd(int fd)
{
	return ERR_PTR(-EINVAL);
}

struct net *get_net_ns_by_pid(pid_t pid)
{
	return ERR_PTR(-ESRCH);
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

/* Drop this skb's hold on its data (shared with clones). */
static void kpi_skb_release_data(struct sk_buff *skb)
{
	struct skb_shared_info *sh = skb_shinfo(skb);

	if (skb->cloned && atomic_dec_return(&sh->dataref))
		return;
	for (int i = 0; i < sh->nr_frags; i++)
		put_page(skb_frag_page(&sh->frags[i]));
	if (sh->frag_list)
		kfree_skb_list(sh->frag_list);
	kpi_skb_free_head(skb);
}

static void kpi_skb_release(struct sk_buff *skb)
{
	if (skb->destructor)
		skb->destructor(skb);
	kpi_skb_release_data(skb);
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
	if (skb_cloned(skb)) {
		/* The clones keep the old data: the copy takes its own
		 * references on the fragments. */
		for (int i = 0; i < sh->nr_frags; i++)
			get_page(skb_frag_page(&sh->frags[i]));
		for (struct sk_buff *f = sh->frag_list; f; f = f->next)
			skb_get(f);
		kpi_skb_release_data(skb);
	} else {
		kpi_skb_free_head(skb);
	}
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

void kfree_skb_list_reason(struct sk_buff *segs, enum skb_drop_reason reason)
{
	while (segs) {
		struct sk_buff *next = segs->next;

		kpi_kfree_skb(segs);
		segs = next;
	}
}

/* From net/core/skbuff.c, without fclones, skb extensions or dst. */
static void kpi_copy_skb_header(struct sk_buff *new, const struct sk_buff *old)
{
	new->tstamp = old->tstamp;
	new->dev = old->dev;
	memcpy(new->cb, old->cb, sizeof(old->cb));
	new->queue_mapping = old->queue_mapping;
	memcpy(&new->headers, &old->headers, sizeof(new->headers));
}

struct sk_buff *skb_clone(struct sk_buff *skb, gfp_t gfp)
{
	struct sk_buff *n = kmalloc(sizeof(*n), gfp & ~__GFP_DMA);

	if (!n)
		return NULL;
	memset(n, 0, offsetof(struct sk_buff, tail));
	n->next = n->prev = NULL;
	n->sk = NULL;
	kpi_copy_skb_header(n, skb);
	n->len = skb->len;
	n->data_len = skb->data_len;
	n->mac_len = skb->mac_len;
	n->hdr_len = skb->nohdr ? skb_headroom(skb) : skb->hdr_len;
	n->cloned = 1;
	n->nohdr = 0;
	n->peeked = 0;
	n->destructor = NULL;
	n->tail = skb->tail;
	n->end = skb->end;
	n->head = skb->head;
	n->head_frag = skb->head_frag;
	n->data = skb->data;
	n->truesize = skb->truesize;
	n->fclone = SKB_FCLONE_UNAVAILABLE;
	refcount_set(&n->users, 1);
	atomic_inc(&skb_shinfo(skb)->dataref);
	skb->cloned = 1;
	return n;
}

void skb_copy_header(struct sk_buff *new, const struct sk_buff *old)
{
	kpi_copy_skb_header(new, old);
	skb_shinfo(new)->gso_size = skb_shinfo(old)->gso_size;
	skb_shinfo(new)->gso_segs = skb_shinfo(old)->gso_segs;
	skb_shinfo(new)->gso_type = skb_shinfo(old)->gso_type;
}

struct sk_buff *skb_copy(const struct sk_buff *skb, gfp_t gfp)
{
	int headerlen = skb_headroom(skb);
	unsigned int size = skb_end_offset(skb) + skb->data_len;
	struct sk_buff *n = __alloc_skb(size, gfp, 0, NUMA_NO_NODE);

	if (!n)
		return NULL;
	skb_reserve(n, headerlen);
	skb_put(n, skb->len);
	BUG_ON(skb_copy_bits(skb, -headerlen, n->head, headerlen + skb->len));
	skb_copy_header(n, skb);
	return n;
}

struct sk_buff *skb_copy_expand(const struct sk_buff *skb, int newheadroom, int newtailroom,
				gfp_t gfp)
{
	int oldheadroom = skb_headroom(skb), head_copy_len, head_copy_off = 0;
	struct sk_buff *n = __alloc_skb(newheadroom + skb->len + newtailroom, gfp, 0,
					NUMA_NO_NODE);

	if (!n)
		return NULL;
	skb_reserve(n, newheadroom);
	skb_put(n, skb->len);
	head_copy_len = oldheadroom;
	if (newheadroom <= head_copy_len)
		head_copy_len = newheadroom;
	else
		head_copy_off = newheadroom - head_copy_len;
	BUG_ON(skb_copy_bits(skb, -head_copy_len, n->head + head_copy_off,
			     skb->len + head_copy_len));
	skb_copy_header(n, skb);
	skb_headers_offset_update(n, newheadroom - oldheadroom);
	return n;
}

int skb_ensure_writable(struct sk_buff *skb, unsigned int write_len)
{
	if (!pskb_may_pull(skb, write_len))
		return -ENOMEM;
	if (!skb_cloned(skb) || skb_clone_writable(skb, write_len))
		return 0;
	return pskb_expand_head(skb, 0, 0, GFP_ATOMIC);
}

/* Transmit status for sockets (SO_WIFI_STATUS): no RustOS socket asks for
 * it, and skbs from RustOS have no socket. */
struct sk_buff *skb_clone_sk(struct sk_buff *skb)
{
	return NULL;
}

void skb_complete_wifi_ack(struct sk_buff *skb, bool acked)
{
	kpi_kfree_skb(skb);
}

struct sk_buff *skb_dequeue(struct sk_buff_head *list)
{
	unsigned long flags;
	struct sk_buff *result;

	spin_lock_irqsave(&list->lock, flags);
	result = __skb_dequeue(list);
	spin_unlock_irqrestore(&list->lock, flags);
	return result;
}

struct sk_buff *skb_dequeue_tail(struct sk_buff_head *list)
{
	unsigned long flags;
	struct sk_buff *result;

	spin_lock_irqsave(&list->lock, flags);
	result = __skb_dequeue_tail(list);
	spin_unlock_irqrestore(&list->lock, flags);
	return result;
}

void skb_queue_tail(struct sk_buff_head *list, struct sk_buff *newsk)
{
	unsigned long flags;

	spin_lock_irqsave(&list->lock, flags);
	__skb_queue_tail(list, newsk);
	spin_unlock_irqrestore(&list->lock, flags);
}

void skb_queue_head(struct sk_buff_head *list, struct sk_buff *newsk)
{
	unsigned long flags;

	spin_lock_irqsave(&list->lock, flags);
	__skb_queue_head(list, newsk);
	spin_unlock_irqrestore(&list->lock, flags);
}

void skb_unlink(struct sk_buff *skb, struct sk_buff_head *list)
{
	unsigned long flags;

	spin_lock_irqsave(&list->lock, flags);
	__skb_unlink(skb, list);
	spin_unlock_irqrestore(&list->lock, flags);
}

void skb_queue_purge_reason(struct sk_buff_head *list, enum skb_drop_reason reason)
{
	struct sk_buff_head tmp;
	unsigned long flags;
	struct sk_buff *skb;

	if (skb_queue_empty_lockless(list))
		return;
	__skb_queue_head_init(&tmp);
	spin_lock_irqsave(&list->lock, flags);
	skb_queue_splice_init(list, &tmp);
	spin_unlock_irqrestore(&list->lock, flags);
	while ((skb = __skb_dequeue(&tmp)))
		kpi_kfree_skb(skb);
}

void skb_add_rx_frag_netmem(struct sk_buff *skb, int i, netmem_ref netmem, int off, int size,
			    unsigned int truesize)
{
	skb_fill_netmem_desc(skb, i, netmem, off, size);
	skb->len += size;
	skb->data_len += size;
	skb->truesize += truesize;
}

/* Flow hash for queue selection (mac80211's fq): a hash of the frame's
 * first bytes is as good as the flow dissector for spreading flows. */
void __skb_get_hash_net(const struct net *net, struct sk_buff *skb)
{
	u8 buf[48];
	unsigned int n = min_t(unsigned int, skb->len, sizeof(buf));

	if (skb_copy_bits(skb, 0, buf, n))
		n = 0;
	__skb_set_sw_hash(skb, jhash(buf, n, 0x52757374) ?: 1, false);
}

/* No GSO: RustOS's stack never builds frames larger than the MTU. */
struct sk_buff *__skb_gso_segment(struct sk_buff *skb, netdev_features_t features,
				  bool tx_path)
{
	return ERR_PTR(-EPROTONOSUPPORT);
}

int skb_csum_hwoffload_help(struct sk_buff *skb, const netdev_features_t features)
{
	return 0;
}

void dst_release(struct dst_entry *dst)
{
}

/* From lib/checksum.c (generic do_csum). */
static unsigned int kpi_do_csum(const unsigned char *buff, int len)
{
	u64 sum = 0;
	bool odd = (unsigned long)buff & 1;

	if (odd && len > 0) {
		sum += *buff++ << 8;
		len--;
	}
	while (len > 1) {
		sum += *(const u16 *)buff;
		buff += 2;
		len -= 2;
	}
	if (len > 0)
		sum += *buff;
	while (sum >> 16)
		sum = (sum & 0xffff) + (sum >> 16);
	if (odd)
		sum = ((sum >> 8) & 0xff) | ((sum & 0xff) << 8);
	return sum;
}

__wsum csum_partial(const void *buff, int len, __wsum wsum)
{
	unsigned int sum = (__force unsigned int)wsum;
	unsigned int result = kpi_do_csum(buff, len);

	result += sum;
	if (sum > result)
		result += 1;
	return (__force __wsum)result;
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

/*
 * Every registered net_device is also a RustOS interface (net.rs); the
 * table below maps one to the other. RustOS interface indexes are Linux
 * ifindexes. Registration follows net/core/dev.c without qdiscs, XDP,
 * namespaces other than init_net, or the netdev instance lock: the
 * netdevice notifier chain sees NETDEV_REGISTER/UP/GOING_DOWN/DOWN/
 * UNREGISTER as in Linux, and freeing (needs_free_netdev) waits for
 * rtnl_unlock() as Linux's netdev_run_todo() does.
 */
#define KPI_MAX_NETDEVS 32
static struct { struct net_device *dev; u64 handle; bool opened; } kpi_netdevs[KPI_MAX_NETDEVS];
static RAW_NOTIFIER_HEAD(netdev_chain);

static u64 kpi_netdev_handle(const struct net_device *dev)
{
	for (int i = 0; i < KPI_MAX_NETDEVS; i++)
		if (kpi_netdevs[i].dev == dev)
			return kpi_netdevs[i].handle;
	return 0;
}

int call_netdevice_notifiers_info(unsigned long val, struct netdev_notifier_info *info)
{
	return raw_notifier_call_chain(&netdev_chain, val, info);
}

int call_netdevice_notifiers(unsigned long val, struct net_device *dev)
{
	struct netdev_notifier_info info = { .dev = dev };

	return call_netdevice_notifiers_info(val, &info);
}

int register_netdevice_notifier(struct notifier_block *nb)
{
	int err;

	rtnl_lock();
	err = raw_notifier_chain_register(&netdev_chain, nb);
	/* Replay the devices that exist, as Linux does. */
	for (int i = 0; !err && i < KPI_MAX_NETDEVS; i++) {
		struct net_device *dev = kpi_netdevs[i].dev;
		struct netdev_notifier_info info = { .dev = dev };

		if (!dev)
			continue;
		nb->notifier_call(nb, NETDEV_REGISTER, &info);
		if (dev->flags & IFF_UP)
			nb->notifier_call(nb, NETDEV_UP, &info);
	}
	rtnl_unlock();
	return err;
}

int unregister_netdevice_notifier(struct notifier_block *nb)
{
	int err;

	rtnl_lock();
	err = raw_notifier_chain_unregister(&netdev_chain, nb);
	rtnl_unlock();
	return err;
}

/* IPv4 addresses live in RustOS's stack; nobody hears about them. */
int register_inetaddr_notifier(struct notifier_block *nb)
{
	return 0;
}

int unregister_inetaddr_notifier(struct notifier_block *nb)
{
	return 0;
}

static const struct ethtool_ops default_ethtool_ops;

void netdev_set_default_ethtool_ops(struct net_device *dev, const struct ethtool_ops *ops)
{
	if (dev->ethtool_ops == &default_ethtool_ops)
		dev->ethtool_ops = ops;
}

/* From net/ethernet/eth.c. */
void ether_setup(struct net_device *dev)
{
	dev->header_ops = NULL;
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
}

int eth_mac_addr(struct net_device *dev, void *p)
{
	struct sockaddr *addr = p;

	if (!(dev->priv_flags & IFF_LIVE_ADDR_CHANGE) && netif_running(dev))
		return -EBUSY;
	if (!is_valid_ether_addr(addr->sa_data))
		return -EADDRNOTAVAIL;
	eth_hw_addr_set(dev, addr->sa_data);
	return 0;
}

struct net_device *alloc_netdev_mqs(int sizeof_priv, const char *name,
				    unsigned char name_assign_type,
				    void (*setup)(struct net_device *), unsigned int txqs,
				    unsigned int rxqs)
{
	struct net_device *dev;

	if (!txqs || !rxqs)
		return NULL;
	dev = kzalloc(struct_size(dev, priv, sizeof_priv), GFP_KERNEL);
	if (!dev)
		return NULL;
	dev->priv_len = sizeof_priv;
	dev->pcpu_refcnt = alloc_percpu(int);
	dev->dev_addr = kzalloc(MAX_ADDR_LEN, GFP_KERNEL);
	dev->_tx = kcalloc(txqs, sizeof(struct netdev_queue), GFP_KERNEL);
	dev->_rx = kcalloc(rxqs, sizeof(struct netdev_rx_queue), GFP_KERNEL);
	dev->ethtool = kzalloc(sizeof(*dev->ethtool), GFP_KERNEL);
	dev->cfg = kzalloc(sizeof(*dev->cfg), GFP_KERNEL);
	dev->napi_config = kcalloc(max(txqs, rxqs), sizeof(*dev->napi_config), GFP_KERNEL);
	if (!dev->pcpu_refcnt || !dev->dev_addr || !dev->_tx || !dev->_rx || !dev->ethtool ||
	    !dev->cfg || !dev->napi_config) {
		free_netdev(dev);
		return NULL;
	}
	__dev_hold(dev);
	dev_net_set(dev, &init_net);
	dev->gso_max_size = GSO_LEGACY_MAX_SIZE;
	dev->gso_max_segs = GSO_MAX_SEGS;
	dev->gro_max_size = GRO_LEGACY_MAX_SIZE;
	dev->tso_max_size = TSO_LEGACY_MAX_SIZE;
	dev->tso_max_segs = TSO_MAX_SEGS;
	dev->upper_level = 1;
	dev->lower_level = 1;
	INIT_LIST_HEAD(&dev->napi_list);
	INIT_LIST_HEAD(&dev->unreg_list);
	INIT_LIST_HEAD(&dev->close_list);
	INIT_LIST_HEAD(&dev->link_watch_list);
	INIT_LIST_HEAD(&dev->adj_list.upper);
	INIT_LIST_HEAD(&dev->adj_list.lower);
	INIT_LIST_HEAD(&dev->ptype_all);
	INIT_LIST_HEAD(&dev->ptype_specific);
	INIT_LIST_HEAD(&dev->net_notifier_list);
	INIT_LIST_HEAD(&dev->todo_list);
	INIT_LIST_HEAD(&dev->dev_list);
	INIT_LIST_HEAD(&dev->uc.list);
	INIT_LIST_HEAD(&dev->mc.list);
	mutex_init(&dev->lock);
	dev->priv_flags = IFF_XMIT_DST_RELEASE | IFF_XMIT_DST_RELEASE_PERM;
	setup(dev);
	if (!dev->tx_queue_len) {
		dev->priv_flags |= IFF_NO_QUEUE;
		dev->tx_queue_len = 1000;
	}
	dev->num_tx_queues = dev->real_num_tx_queues = txqs;
	for (unsigned int i = 0; i < txqs; i++) {
		struct netdev_queue *q = &dev->_tx[i];

		q->dev = dev;
		spin_lock_init(&q->_xmit_lock);
		q->xmit_lock_owner = -1;
#ifdef CONFIG_BQL
		dql_init(&q->dql, HZ);
#endif
	}
	dev->num_rx_queues = dev->real_num_rx_queues = rxqs;
	for (unsigned int i = 0; i < rxqs; i++)
		dev->_rx[i].dev = dev;
	dev->cfg_pending = dev->cfg;
	dev->num_napi_configs = max(txqs, rxqs);
	strscpy(dev->name, name, sizeof(dev->name));
	dev->name_assign_type = name_assign_type;
	if (!dev->ethtool_ops)
		dev->ethtool_ops = &default_ethtool_ops;
	set_bit(__LINK_STATE_PRESENT, &dev->state);
	return dev;
}

struct net_device *alloc_etherdev_mqs(int sizeof_priv, unsigned int txqs, unsigned int rxqs)
{
	return alloc_netdev_mqs(sizeof_priv, "eth%d", NET_NAME_ENUM, ether_setup, txqs, rxqs);
}

static void kpi_netdev_free_mem(struct net_device *dev)
{
	free_percpu(dev->pcpu_refcnt);
	kfree(dev->dev_addr);
	kfree(dev->_tx);
	kfree(dev->_rx);
	kfree(dev->ethtool);
	kfree(dev->cfg);
	kfree(dev->napi_config);
	kfree(dev);
}

/* The struct device of a registered net_device frees it on release. */
static void kpi_netdev_release(struct device *d)
{
	kpi_netdev_free_mem(container_of(d, struct net_device, dev));
}

static const struct class kpi_net_class = {
	.name = "net",
	.dev_release = kpi_netdev_release,
};

void free_netdev(struct net_device *dev)
{
	if (!dev)
		return;
	if (dev->reg_state == NETREG_UNINITIALIZED) {
		kpi_netdev_free_mem(dev);
		return;
	}
	dev->reg_state = NETREG_RELEASED;
	put_device(&dev->dev);
}

void dev_addr_mod(struct net_device *dev, unsigned int offset, const void *addr, size_t len)
{
	memcpy((u8 *)dev->dev_addr + offset, addr, len);
	if (dev->reg_state == NETREG_REGISTERED && dev->addr_len == ETH_ALEN)
		rustos_kpi_netdev_set_mac(kpi_netdev_handle(dev), dev->dev_addr);
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

/* Interface names: an exact name, or a template with %d that takes the
 * lowest free number. Free means free in RustOS (native NICs too). */
int dev_alloc_name(struct net_device *dev, const char *name)
{
	char buf[IFNAMSIZ];

	if (!strchr(name, '%')) {
		if (!rustos_kpi_ifname_free(name))
			return -EEXIST;
		strscpy(dev->name, name, sizeof(dev->name));
		return 0;
	}
	for (int i = 0; i < 1000; i++) {
		snprintf(buf, sizeof(buf), name, i);
		if (rustos_kpi_ifname_free(buf)) {
			strscpy(dev->name, buf, sizeof(dev->name));
			return i;
		}
	}
	return -ENFILE;
}

int register_netdevice(struct net_device *dev)
{
	const char *drv = dev->dev.parent ? dev_driver_string(dev->dev.parent) : "linux";
	int slot = -1, err;
	u64 handle;

	ASSERT_RTNL();
	for (int i = 0; i < KPI_MAX_NETDEVS; i++)
		if (!kpi_netdevs[i].dev) {
			slot = i;
			break;
		}
	if (slot < 0)
		return -ENFILE;
	err = dev_alloc_name(dev, dev->name);
	if (err < 0)
		return err;
	if (dev->netdev_ops->ndo_init) {
		err = dev->netdev_ops->ndo_init(dev);
		if (err)
			return err > 0 ? -EIO : err;
	}
	if (dev->pcpu_stat_type == NETDEV_PCPU_STAT_TSTATS) {
		dev->tstats = netdev_alloc_pcpu_stats(struct pcpu_sw_netstats);
		if (!dev->tstats) {
			err = -ENOMEM;
			goto uninit;
		}
	} else if (dev->pcpu_stat_type == NETDEV_PCPU_STAT_DSTATS) {
		dev->dstats = netdev_alloc_pcpu_stats(struct pcpu_dstats);
		if (!dev->dstats) {
			err = -ENOMEM;
			goto uninit;
		}
	}
	err = notifier_to_errno(call_netdevice_notifiers(NETDEV_POST_INIT, dev));
	if (err)
		goto uninit;
	handle = rustos_kpi_netdev_register(dev, dev->dev_addr, dev->mtu,
					    kpi_netdev_is_wireless(dev), dev->type == ARPHRD_ETHER,
					    drv, dev->name);
	dev->ifindex = rustos_kpi_netdev_ifindex(handle);
	kpi_netdevs[slot].dev = dev;
	kpi_netdevs[slot].handle = handle;
	kpi_netdevs[slot].opened = false;

	device_initialize(&dev->dev);
	dev->dev.class = &kpi_net_class;
	dev->dev.platform_data = dev;
	dev_set_name(&dev->dev, "%s", dev->name);
	err = device_add(&dev->dev);
	if (err)
		netdev_warn(dev, "sysfs registration failed: %d\n", err);

	list_add_tail_rcu(&dev->dev_list, &init_net.dev_base_head);
	dev->reg_state = NETREG_REGISTERED;
	/* No carrier until opened (dev_open). */
	rustos_kpi_netdev_carrier(handle, 0);
	call_netdevice_notifiers(NETDEV_REGISTER, dev);
	return 0;

uninit:
	if (dev->netdev_ops->ndo_uninit)
		dev->netdev_ops->ndo_uninit(dev);
	return err;
}

int register_netdev(struct net_device *dev)
{
	int err;

	rtnl_lock();
	err = register_netdevice(dev);
	rtnl_unlock();
	return err;
}

int dev_open(struct net_device *dev, struct netlink_ext_ack *extack)
{
	int err;

	ASSERT_RTNL();
	if (dev->flags & IFF_UP)
		return 0;
	if (!netif_device_present(dev))
		return -ENODEV;
	err = notifier_to_errno(call_netdevice_notifiers(NETDEV_PRE_UP, dev));
	if (err)
		return err;
	set_bit(__LINK_STATE_START, &dev->state);
	if (dev->netdev_ops->ndo_open)
		err = dev->netdev_ops->ndo_open(dev);
	if (err) {
		clear_bit(__LINK_STATE_START, &dev->state);
		return err;
	}
	dev->flags |= IFF_UP;
	call_netdevice_notifiers(NETDEV_UP, dev);
	rustos_kpi_netdev_carrier(kpi_netdev_handle(dev), netif_carrier_ok(dev));
	return 0;
}

void dev_close(struct net_device *dev)
{
	ASSERT_RTNL();
	if (!(dev->flags & IFF_UP))
		return;
	call_netdevice_notifiers(NETDEV_GOING_DOWN, dev);
	clear_bit(__LINK_STATE_START, &dev->state);
	smp_mb__after_atomic();
	if (dev->netdev_ops->ndo_stop)
		dev->netdev_ops->ndo_stop(dev);
	dev->flags &= ~IFF_UP;
	call_netdevice_notifiers(NETDEV_DOWN, dev);
	rustos_kpi_netdev_carrier(kpi_netdev_handle(dev), 0);
}

/* `ip link set up/down` and SIOCSIFFLAGS from RustOS (net.rs). */
int kpi_netdev_set_up(struct net_device *dev, int up)
{
	int err = 0;

	rtnl_lock();
	if (up)
		err = dev_open(dev, NULL);
	else
		dev_close(dev);
	rtnl_unlock();
	return err;
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
		int err;

		if (!dev || kpi_netdevs[i].opened)
			continue;
		kpi_netdevs[i].opened = true;
		err = dev_open(dev, NULL);
		if (err)
			netdev_err(dev, "open failed: %d\n", err);
	}
	rtnl_unlock();
}

static void kpi_unregister_one(struct net_device *dev)
{
	int i;

	dev_close(dev);
	call_netdevice_notifiers(NETDEV_UNREGISTER, dev);
	list_del_rcu(&dev->dev_list);
	for (i = 0; i < KPI_MAX_NETDEVS; i++)
		if (kpi_netdevs[i].dev == dev)
			break;
	if (i < KPI_MAX_NETDEVS) {
		rustos_kpi_netdev_unregister(kpi_netdevs[i].handle);
		kpi_netdevs[i].dev = NULL;
	}
	if (dev->netdev_ops->ndo_uninit)
		dev->netdev_ops->ndo_uninit(dev);
	device_del(&dev->dev);
	dev->reg_state = NETREG_UNREGISTERING;
	list_add_tail(&dev->todo_list, &kpi_net_todo);
}

void unregister_netdevice_many(struct list_head *head)
{
	struct net_device *dev, *tmp;

	list_for_each_entry_safe(dev, tmp, head, unreg_list) {
		list_del_init(&dev->unreg_list);
		kpi_unregister_one(dev);
	}
}

void unregister_netdevice_queue(struct net_device *dev, struct list_head *head)
{
	ASSERT_RTNL();
	if (head) {
		list_move_tail(&dev->unreg_list, head);
		return;
	}
	kpi_unregister_one(dev);
}

void unregister_netdev(struct net_device *dev)
{
	rtnl_lock();
	unregister_netdevice(dev);
	rtnl_unlock();
}

/* rtnl_unlock(): finish unregistrations (net/core/dev.c netdev_run_todo). */
static void kpi_netdev_run_todo(struct list_head *list)
{
	struct net_device *dev, *tmp;

	if (list_empty(list))
		return;
	synchronize_rcu();
	list_for_each_entry_safe(dev, tmp, list, todo_list) {
		list_del_init(&dev->todo_list);
		dev->reg_state = NETREG_UNREGISTERED;
		free_percpu(dev->tstats);
		dev->tstats = NULL;
		if (dev->priv_destructor)
			dev->priv_destructor(dev);
		if (dev->needs_free_netdev)
			free_netdev(dev);
	}
}

/* RustOS shutdown path (src/linuxkpi/net.rs). */
void kpi_netdev_stop(struct net_device *dev)
{
	if (test_and_clear_bit(__LINK_STATE_START, &dev->state) && dev->netdev_ops->ndo_stop)
		dev->netdev_ops->ndo_stop(dev);
}

int dev_change_net_namespace(struct net_device *dev, struct net *net, const char *pat)
{
	return net == dev_net(dev) ? 0 : -EOPNOTSUPP;
}

struct net_device *__dev_get_by_index(struct net *net, int ifindex)
{
	for (int i = 0; i < KPI_MAX_NETDEVS; i++) {
		struct net_device *dev = READ_ONCE(kpi_netdevs[i].dev);

		if (dev && dev->ifindex == ifindex)
			return dev;
	}
	return NULL;
}

struct net_device *dev_get_by_index_rcu(struct net *net, int ifindex)
{
	return __dev_get_by_index(net, ifindex);
}

struct net_device *dev_get_by_index(struct net *net, int ifindex)
{
	struct net_device *dev = __dev_get_by_index(net, ifindex);

	dev_hold(dev);
	return dev;
}

struct net_device *__dev_get_by_name(struct net *net, const char *name)
{
	for (int i = 0; i < KPI_MAX_NETDEVS; i++) {
		struct net_device *dev = READ_ONCE(kpi_netdevs[i].dev);

		if (dev && !strncmp(dev->name, name, IFNAMSIZ))
			return dev;
	}
	return NULL;
}

void synchronize_net(void)
{
	synchronize_rcu();
}

/* Unicast/multicast address lists: RustOS's stack filters nothing in
 * hardware, so the lists stay empty and syncing them is a no-op. */
void __hw_addr_init(struct netdev_hw_addr_list *list)
{
	INIT_LIST_HEAD(&list->list);
	list->count = 0;
	list->tree = RB_ROOT;
}

int __hw_addr_sync(struct netdev_hw_addr_list *to_list, struct netdev_hw_addr_list *from_list,
		   int addr_len)
{
	return 0;
}

void __hw_addr_unsync(struct netdev_hw_addr_list *to_list,
		      struct netdev_hw_addr_list *from_list, int addr_len)
{
}

void netif_carrier_on(struct net_device *dev)
{
	if (test_and_clear_bit(__LINK_STATE_NOCARRIER, &dev->state) &&
	    dev->reg_state == NETREG_REGISTERED && netif_running(dev))
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

/*
 * Transmit without qdiscs: pick the queue (ndo_select_queue), then hand
 * the skb to the driver unless that queue is stopped, in which case it is
 * dropped (RustOS's stack retransmits).
 */
int __dev_queue_xmit(struct sk_buff *skb, struct net_device *sb_dev)
{
	struct net_device *dev = skb->dev;
	struct netdev_queue *txq;
	netdev_tx_t rc = NETDEV_TX_BUSY;
	u16 q = 0;

	skb_reset_mac_header(skb);
	if (dev->netdev_ops->ndo_select_queue)
		q = dev->netdev_ops->ndo_select_queue(dev, skb, sb_dev);
	if (q >= dev->real_num_tx_queues)
		q = 0;
	skb_set_queue_mapping(skb, q);
	txq = netdev_get_tx_queue(dev, q);

	local_bh_disable();
	if (!dev->lltx)
		__netif_tx_lock(txq, smp_processor_id());
	if (netif_running(dev) && !netif_xmit_frozen_or_stopped(txq))
		rc = dev->netdev_ops->ndo_start_xmit(skb, dev);
	if (!dev->lltx)
		__netif_tx_unlock(txq);
	local_bh_enable();
	if (rc != NETDEV_TX_OK) {
		kpi_kfree_skb(skb);
		return NET_XMIT_DROP;
	}
	return NET_XMIT_SUCCESS;
}

/* Frames from RustOS (src/linuxkpi/net.rs): a new linear skb each. */
int kpi_netdev_xmit(struct net_device *dev, const u8 *data, u32 len)
{
	struct sk_buff *skb;

	if (!netif_running(dev) || !netif_carrier_ok(dev))
		return -ENETDOWN;
	skb = __netdev_alloc_skb(dev, len + dev->needed_headroom + dev->needed_tailroom,
				 GFP_ATOMIC);
	if (!skb)
		return -ENOMEM;
	skb_reserve(skb, dev->needed_headroom);
	skb_put_data(skb, data, len);
	skb_reset_mac_header(skb);
	skb_set_network_header(skb, ETH_HLEN);
	skb->protocol = ((const struct ethhdr *)data)->h_proto;
	skb->ip_summed = CHECKSUM_NONE;
	return __dev_queue_xmit(skb, NULL) == NET_XMIT_SUCCESS ? 0 : -EBUSY;
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

void netif_receive_skb_list(struct list_head *head)
{
	struct sk_buff *skb, *next;

	list_for_each_entry_safe(skb, next, head, list) {
		skb_list_del_init(skb);
		kpi_netif_deliver(skb);
	}
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
	return kpi_netns_init();
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
