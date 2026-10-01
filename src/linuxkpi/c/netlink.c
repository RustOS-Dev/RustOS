// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * LinuxKPI kernel netlink sockets, the kernel half of net/netlink/
 * af_netlink.c, on RustOS's AF_NETLINK (src/net/netlink.rs): user sockets
 * live in Rust; a protocol with a Linux kernel socket (generic netlink)
 * gets the datagrams users send as skbs through cfg->input, and its
 * unicasts and multicasts are copied out to the Rust sockets.
 *
 * Dumps run to completion when requested (Linux runs the dump callback
 * again each time the reader drains its queue), each skb up to
 * NLMSG_GOODSIZE (or min_dump_alloc), then NLMSG_DONE.
 */
#include <linux/netlink.h>
#include <linux/notifier.h>
#include <linux/skbuff.h>
#include <linux/slab.h>
#include <net/net_namespace.h>
#include <net/netlink.h>
#include <net/sock.h>
#include "kpi.h"

#define KPI_NETLINK_UNITS 32

struct kpi_nl_kernel {
	struct sock *sk;
	void (*input)(struct sk_buff *skb);
};

static struct kpi_nl_kernel kpi_nl_units[KPI_NETLINK_UNITS];
static ATOMIC_NOTIFIER_HEAD(netlink_chain);

/* Stands in for the sending user socket (skb->sk, NETLINK_CB(skb).sk):
 * code only takes the namespace from it. */
static struct sock kpi_nl_user_sk;

static void kpi_netlink_input(u32 proto, u32 portid, const void *data, size_t len);
static void kpi_netlink_release(u32 proto, u32 portid);

struct sock *__netlink_kernel_create(struct net *net, int unit, struct module *module,
				     struct netlink_kernel_cfg *cfg)
{
	struct sock *sk;

	if (unit < 0 || unit >= KPI_NETLINK_UNITS || kpi_nl_units[unit].sk)
		return NULL;
	sk = kzalloc(sizeof(*sk), GFP_KERNEL);
	if (!sk)
		return NULL;
	sock_net_set(sk, net);
	sock_net_set(&kpi_nl_user_sk, &init_net);
	sk->sk_protocol = unit;
	refcount_set(&sk->sk_refcnt, 1);
	kpi_nl_units[unit].sk = sk;
	kpi_nl_units[unit].input = cfg ? cfg->input : NULL;
	rustos_kpi_netlink_register(unit, kpi_netlink_input, kpi_netlink_release);
	return sk;
}

void netlink_kernel_release(struct sock *sk)
{
	if (!sk)
		return;
	if (sk->sk_protocol < KPI_NETLINK_UNITS && kpi_nl_units[sk->sk_protocol].sk == sk) {
		rustos_kpi_netlink_register(sk->sk_protocol, NULL, NULL);
		kpi_nl_units[sk->sk_protocol].sk = NULL;
		kpi_nl_units[sk->sk_protocol].input = NULL;
	}
	kfree(sk);
}

/* A datagram from user socket portid (called from its sendmsg). */
static void kpi_netlink_input(u32 proto, u32 portid, const void *data, size_t len)
{
	struct kpi_nl_kernel *k = proto < KPI_NETLINK_UNITS ? &kpi_nl_units[proto] : NULL;
	struct sk_buff *skb;

	if (!k || !k->input)
		return;
	skb = alloc_skb(len, GFP_KERNEL);
	if (!skb)
		return;
	skb_put_data(skb, data, len);
	skb->sk = &kpi_nl_user_sk;
	skb->protocol = proto;
	NETLINK_CB(skb).portid = portid;
	NETLINK_CB(skb).dst_group = 0;
	NETLINK_CB(skb).sk = &kpi_nl_user_sk;
	NETLINK_CB(skb).creds.uid = make_kuid(&init_user_ns, rustos_kpi_current_uid());
	k->input(skb);
	skb->sk = NULL;
	kfree_skb(skb);
}

/* A user socket closed: NETLINK_URELEASE (nl80211 drops what it owned). */
static void kpi_netlink_release(u32 proto, u32 portid)
{
	struct netlink_notify n = {
		.net = &init_net,
		.portid = portid,
		.protocol = proto,
	};

	atomic_notifier_call_chain(&netlink_chain, NETLINK_URELEASE, &n);
}

int netlink_register_notifier(struct notifier_block *nb)
{
	return atomic_notifier_chain_register(&netlink_chain, nb);
}

int netlink_unregister_notifier(struct notifier_block *nb)
{
	return atomic_notifier_chain_unregister(&netlink_chain, nb);
}

/* Copy the skb out for RustOS: linear data. Returns 0 or -errno. */
static int kpi_skb_linear(struct sk_buff *skb)
{
	return skb_linearize(skb) ? -ENOMEM : 0;
}

int netlink_unicast(struct sock *ssk, struct sk_buff *skb, u32 portid, int nonblock)
{
	int len = skb->len, err;

	err = kpi_skb_linear(skb);
	if (!err)
		err = rustos_kpi_netlink_unicast(ssk->sk_protocol, portid, skb->data, skb->len);
	kfree_skb(skb);
	return err ?: len;
}

int netlink_broadcast_filtered(struct sock *ssk, struct sk_buff *skb, u32 portid, u32 group,
			       gfp_t allocation, netlink_filter_fn filter, void *filter_data)
{
	int delivered = 0;

	if (!kpi_skb_linear(skb))
		delivered = rustos_kpi_netlink_multicast(ssk->sk_protocol, group, portid,
							 skb->data, skb->len);
	consume_skb(skb);
	return delivered ? 0 : -ESRCH;
}

int netlink_broadcast(struct sock *ssk, struct sk_buff *skb, u32 portid, u32 group,
		      gfp_t allocation)
{
	return netlink_broadcast_filtered(ssk, skb, portid, group, allocation, NULL, NULL);
}

int netlink_has_listeners(struct sock *sk, unsigned int group)
{
	return rustos_kpi_netlink_has_listeners(sk->sk_protocol, group);
}

int netlink_set_err(struct sock *ssk, u32 portid, u32 group, int code)
{
	return 0;
}

bool netlink_strict_get_check(struct sk_buff *skb)
{
	return false;
}

/* Multicast groups exist as numbers only: RustOS sockets join any. */
int __netlink_change_ngroups(struct sock *sk, unsigned int groups)
{
	return 0;
}

int netlink_change_ngroups(struct sock *sk, unsigned int groups)
{
	return 0;
}

void __netlink_clear_multicast_users(struct sock *sk, unsigned int group)
{
}

void netlink_table_grab(void)
{
}

void netlink_table_ungrab(void)
{
}

/* Senders were checked when they sent: root only, there being no
 * capabilities in RustOS. */
bool netlink_capable(const struct sk_buff *skb, int cap)
{
	return uid_eq(NETLINK_CB(skb).creds.uid, GLOBAL_ROOT_UID);
}

bool netlink_ns_capable(const struct sk_buff *skb, struct user_namespace *ns, int cap)
{
	return netlink_capable(skb, cap);
}

bool netlink_net_capable(const struct sk_buff *skb, int cap)
{
	return netlink_capable(skb, cap);
}

/* From net/netlink/af_netlink.c. */
struct nlmsghdr *__nlmsg_put(struct sk_buff *skb, u32 portid, u32 seq, int type, int len,
			     int flags)
{
	struct nlmsghdr *nlh;
	int size = nlmsg_msg_size(len);

	nlh = skb_put(skb, NLMSG_ALIGN(size));
	nlh->nlmsg_type = type;
	nlh->nlmsg_len = size;
	nlh->nlmsg_flags = flags;
	nlh->nlmsg_pid = portid;
	nlh->nlmsg_seq = seq;
	if (NLMSG_ALIGN(size) - size != 0)
		memset(nlmsg_data(nlh) + len, 0, NLMSG_ALIGN(size) - size);
	return nlh;
}

int nlmsg_notify(struct sock *sk, struct sk_buff *skb, u32 portid, unsigned int group,
		 int report, gfp_t flags)
{
	int err = 0;

	if (group) {
		int exclude_portid = 0;

		if (report) {
			refcount_inc(&skb->users);
			exclude_portid = portid;
		}
		err = nlmsg_multicast(sk, skb, exclude_portid, group, flags);
		if (err == -ESRCH)
			err = 0;
	}
	if (report) {
		int err2 = nlmsg_unicast(sk, skb, portid);

		if (!err)
			err = err2;
	}
	return err;
}

/*
 * The error/ACK message: always capped (the request header, not its
 * payload), with the extended-ACK message string when there is one.
 */
void netlink_ack(struct sk_buff *in_skb, struct nlmsghdr *nlh, int err,
		 const struct netlink_ext_ack *extack)
{
	/* kpi_netlink_input() keeps the unit in skb->protocol. */
	struct sock *ssk = in_skb->protocol < KPI_NETLINK_UNITS ?
			   kpi_nl_units[in_skb->protocol].sk : NULL;
	size_t payload = sizeof(struct nlmsgerr), tlvlen = 0;
	unsigned int flags = NLM_F_CAPPED;
	struct nlmsghdr *rep;
	struct nlmsgerr *errmsg;
	struct sk_buff *skb;

	if (!ssk)
		return;
	if (extack && extack->_msg)
		tlvlen = nla_total_size(strlen(extack->_msg) + 1);
	if (tlvlen)
		flags |= NLM_F_ACK_TLVS;
	skb = nlmsg_new(payload + tlvlen, GFP_KERNEL);
	if (!skb)
		return;
	rep = nlmsg_put(skb, NETLINK_CB(in_skb).portid, nlh->nlmsg_seq, NLMSG_ERROR,
			payload, flags);
	if (!rep) {
		kfree_skb(skb);
		return;
	}
	errmsg = nlmsg_data(rep);
	errmsg->error = err;
	errmsg->msg = *nlh;
	if (tlvlen && nla_put_string(skb, NLMSGERR_ATTR_MSG, extack->_msg)) {
		kfree_skb(skb);
		return;
	}
	nlmsg_end(skb, rep);
	nlmsg_unicast(ssk, skb, NETLINK_CB(in_skb).portid);
}

int netlink_rcv_skb(struct sk_buff *skb,
		    int (*cb)(struct sk_buff *, struct nlmsghdr *, struct netlink_ext_ack *))
{
	struct netlink_ext_ack extack;
	struct nlmsghdr *nlh;
	int err;

	while (skb->len >= nlmsg_total_size(0)) {
		int msglen;

		memset(&extack, 0, sizeof(extack));
		nlh = nlmsg_hdr(skb);
		err = 0;
		if (nlh->nlmsg_len < NLMSG_HDRLEN || skb->len < nlh->nlmsg_len)
			return 0;
		/* Only requests are handled by the kernel; skip control messages. */
		if (!(nlh->nlmsg_flags & NLM_F_REQUEST) || nlh->nlmsg_type < NLMSG_MIN_TYPE)
			goto ack;
		err = cb(skb, nlh, &extack);
		if (err == -EINTR)
			goto skip;
ack:
		if (nlh->nlmsg_flags & NLM_F_ACK || err)
			netlink_ack(skb, nlh, err, &extack);
skip:
		msglen = NLMSG_ALIGN(nlh->nlmsg_len);
		if (msglen > skb->len)
			msglen = skb->len;
		skb_pull(skb, msglen);
	}
	return 0;
}

int __netlink_dump_start(struct sock *ssk, struct sk_buff *skb, const struct nlmsghdr *nlh,
			 struct netlink_dump_control *control)
{
	u32 portid = NETLINK_CB(skb).portid;
	struct netlink_callback *cb;
	int len, ret = 0;

	cb = kzalloc(sizeof(*cb), GFP_KERNEL);
	if (!cb)
		return -ENOMEM;
	cb->dump = control->dump;
	cb->done = control->done;
	cb->nlh = nlh;
	cb->data = control->data;
	cb->module = control->module;
	cb->min_dump_alloc = control->min_dump_alloc;
	cb->flags = control->flags;
	cb->skb = skb;
	if (control->start) {
		cb->extack = control->extack;
		ret = control->start(cb);
		cb->extack = NULL;
		if (ret) {
			kfree(cb);
			return ret;
		}
	}
	for (;;) {
		size_t size = max_t(size_t, cb->min_dump_alloc, NLMSG_GOODSIZE);
		struct sk_buff *out = alloc_skb(size, GFP_KERNEL);
		struct nlmsghdr *done;

		if (!out) {
			len = -ENOMEM;
			break;
		}
		out->sk = &kpi_nl_user_sk;
		NETLINK_CB(out).portid = portid;
		len = cb->dump(out, cb);
		if (len > 0 && out->len) {
			netlink_unicast(ssk, out, portid, MSG_DONTWAIT);
			continue;
		}
		if (len > 0)
			len = 0;
		/* NLMSG_DONE carries the dump's final status. */
		if (skb_tailroom(out) < nlmsg_total_size(sizeof(len))) {
			netlink_unicast(ssk, out, portid, MSG_DONTWAIT);
			out = alloc_skb(nlmsg_total_size(sizeof(len)), GFP_KERNEL);
			if (!out)
				break;
		}
		done = nlmsg_put_answer(out, cb, NLMSG_DONE, sizeof(len),
					NLM_F_MULTI | cb->answer_flags);
		if (done)
			memcpy(nlmsg_data(done), &len, sizeof(len));
		netlink_unicast(ssk, out, portid, MSG_DONTWAIT);
		break;
	}
	if (cb->done)
		cb->done(cb);
	kfree(cb);
	/* Started (and here finished): no ACK, as in Linux. */
	return -EINTR;
}
