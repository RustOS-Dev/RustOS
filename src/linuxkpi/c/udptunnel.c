// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * LinuxKPI UDP tunnel sockets (net/ipv4/udp_tunnel_core.c's API) on
 * RustOS's UDP (src/linuxkpi/udp.rs), with what a tunnel driver needs
 * from Linux's IPv4 routing: WireGuard (M43).
 *
 * RustOS's stack (smoltcp) owns IP: a tunnel socket is a RustOS UDP socket
 * whose datagrams a kernel thread hands to the encap_rcv() callback as
 * skbs with an IPv4 and UDP header in front, as udp_queue_rcv_one_skb()
 * does (in softirq context: bottom halves off). udp_tunnel_xmit_skb()
 * queues the payload for RustOS to send; RustOS picks the source address
 * and builds the headers. Routes are RustOS's too: ip_route_output_flow()
 * answers with one shared route (the "default route") whose device is a
 * placeholder, and the dst cache caches nothing.
 */
#include <linux/inetdevice.h>
#include <linux/ip.h>
#include <linux/netdevice.h>
#include <linux/skbuff.h>
#include <linux/slab.h>
#include <linux/udp.h>
#include <net/dst_cache.h>
#include <net/icmp.h>
#include <net/ip_tunnels.h>
#include <net/route.h>
#include <net/sock.h>
#include <net/udp.h>
#include <net/udp_tunnel.h>
#include "kpi.h"

/* A tunnel socket: Linux's struct socket and udp_sock, and RustOS's. */
struct kpi_udp_tunnel {
	struct socket sock;
	struct udp_sock usk;
	u64 handle;
};

static struct kpi_udp_tunnel *kpi_tunnel_of(const struct sock *sk)
{
	return container_of(udp_sk(sk), struct kpi_udp_tunnel, usk);
}

int udp_sock_create4(struct net *net, struct udp_port_cfg *cfg, struct socket **sockp)
{
	struct kpi_udp_tunnel *t;
	struct sock *sk;
	u16 bound;
	int err;

	*sockp = NULL;
	if (cfg->family != AF_INET)
		return -EAFNOSUPPORT;
	t = kzalloc(sizeof(*t), GFP_KERNEL);
	if (!t)
		return -ENOMEM;
	sk = &t->usk.inet.sk;
	err = rustos_kpi_udp_open(cfg->local_ip.s_addr, ntohs(cfg->local_udp_port), sk,
				  &t->handle, &bound);
	if (err) {
		kfree(t);
		return err;
	}
	sock_net_set(sk, net);
	sk->sk_family = AF_INET;
	sk->sk_protocol = IPPROTO_UDP;
	sk->sk_type = SOCK_DGRAM;
	refcount_set(&sk->sk_refcnt, 1);
	sk->sk_socket = &t->sock;
	inet_sk(sk)->inet_sport = htons(bound);
	inet_sk(sk)->inet_num = bound;
	inet_sk(sk)->inet_saddr = cfg->local_ip.s_addr;
	inet_sk(sk)->inet_rcv_saddr = cfg->local_ip.s_addr;
	t->sock.sk = sk;
	t->sock.type = SOCK_DGRAM;
	t->sock.state = SS_UNCONNECTED;
	*sockp = &t->sock;
	return 0;
}

void setup_udp_tunnel_sock(struct net *net, struct socket *sock,
			   struct udp_tunnel_sock_cfg *cfg)
{
	struct sock *sk = sock->sk;
	struct udp_sock *up = udp_sk(sk);

	rcu_assign_sk_user_data(sk, cfg->sk_user_data);
	WRITE_ONCE(up->encap_type, cfg->encap_type);
	WRITE_ONCE(up->encap_rcv, cfg->encap_rcv);
	WRITE_ONCE(up->encap_err_rcv, cfg->encap_err_rcv);
	WRITE_ONCE(up->encap_err_lookup, cfg->encap_err_lookup);
	WRITE_ONCE(up->encap_destroy, cfg->encap_destroy);
	WRITE_ONCE(up->gro_receive, cfg->gro_receive);
	WRITE_ONCE(up->gro_complete, cfg->gro_complete);
}

void udp_tunnel_sock_release(struct socket *sock)
{
	struct sock *sk = sock->sk;
	struct kpi_udp_tunnel *t = kpi_tunnel_of(sk);
	void (*destroy)(struct sock *) = READ_ONCE(udp_sk(sk)->encap_destroy);

	/* No callback runs once this returns. */
	rustos_kpi_udp_close(t->handle);
	if (destroy)
		destroy(sk);
	rcu_assign_sk_user_data(sk, NULL);
	synchronize_rcu();
	kfree(t);
}

/*
 * A datagram for socket `ctx`, on its RustOS thread. As Linux's receive
 * path leaves it: skb->data at the UDP header, the IPv4 header before it
 * (network header), bottom halves off. `daddr` is 0 when RustOS does not
 * know which of its addresses the datagram was sent to.
 */
void kpi_udp_rx(void *ctx, u32 saddr, u16 sport, u32 daddr, u16 dport, const u8 *data, u32 len)
{
	struct sock *sk = ctx;
	int (*encap_rcv)(struct sock *, struct sk_buff *);
	struct sk_buff *skb;
	struct udphdr *uh;
	struct iphdr *iph;

	skb = alloc_skb(len + sizeof(*iph) + sizeof(*uh) + NET_SKB_PAD, GFP_KERNEL);
	if (!skb)
		return;
	skb_reserve(skb, NET_SKB_PAD + sizeof(*iph) + sizeof(*uh));
	skb_put_data(skb, data, len);
	uh = skb_push(skb, sizeof(*uh));
	uh->source = htons(sport);
	uh->dest = htons(dport);
	uh->len = htons(len + sizeof(*uh));
	uh->check = 0;
	skb_reset_transport_header(skb);
	iph = skb_push(skb, sizeof(*iph));
	memset(iph, 0, sizeof(*iph));
	iph->version = 4;
	iph->ihl = 5;
	iph->tot_len = htons(skb->len);
	iph->ttl = 64;
	iph->protocol = IPPROTO_UDP;
	iph->saddr = saddr;
	iph->daddr = daddr;
	iph->check = ip_fast_csum((u8 *)iph, iph->ihl);
	skb_reset_network_header(skb);
	skb_reset_mac_header(skb);
	__skb_pull(skb, sizeof(*iph));
	skb->protocol = htons(ETH_P_IP);
	skb->pkt_type = PACKET_HOST;
	/* RustOS checked the UDP checksum. */
	skb->ip_summed = CHECKSUM_UNNECESSARY;

	local_bh_disable();
	rcu_read_lock();
	encap_rcv = READ_ONCE(udp_sk(sk)->encap_rcv);
	if (encap_rcv && encap_rcv(sk, skb) <= 0)
		skb = NULL;	/* consumed */
	rcu_read_unlock();
	local_bh_enable();
	kfree_skb(skb);
}

void udp_tunnel_xmit_skb(struct rtable *rt, struct sock *sk, struct sk_buff *skb,
			 __be32 src, __be32 dst, __u8 tos, __u8 ttl, __be16 df, __be16 src_port,
			 __be16 dst_port, bool xnet, bool nocheck, u16 ipcb_flags)
{
	struct kpi_udp_tunnel *t = kpi_tunnel_of(sk);
	struct net_device *dev = skb->dev;
	unsigned int len = skb->len;

	if (skb_linearize(skb) ||
	    rustos_kpi_udp_send(t->handle, src, dst, ntohs(dst_port), skb->data, len)) {
		if (dev)
			DEV_STATS_INC(dev, tx_dropped);
		kfree_skb(skb);
		return;
	}
	consume_skb(skb);
}

/* ------------------------------------------------------------ routing */

static struct rtable kpi_route;
static u32 kpi_route_metrics[RTAX_MAX];

static int __init kpi_route_init(void)
{
	/* Hop limit as RustOS sends (smoltcp's default). */
	kpi_route_metrics[RTAX_HOPLIMIT - 1] = 64;
	kpi_route.dst.dev = alloc_netdev_dummy(0);
	if (!kpi_route.dst.dev)
		return -ENOMEM;
	kpi_route.dst._metrics = (unsigned long)kpi_route_metrics | DST_METRICS_READ_ONLY;
	rcuref_init(&kpi_route.dst.__rcuref, 1);
	kpi_route.rt_type = RTN_UNICAST;
	return 0;
}
core_initcall(kpi_route_init);

/* RustOS routes the datagram when it sends it: every destination has the
 * same route here, and the source address is left to RustOS. */
struct rtable *ip_route_output_flow(struct net *net, struct flowi4 *flp, const struct sock *sk)
{
	if (!kpi_route.dst.dev)
		return ERR_PTR(-ENETUNREACH);
	return &kpi_route;
}

/* Source addresses are RustOS's choice (see above): none is "confirmed",
 * so a tunnel forgets the one it learned and sends from any address. */
__be32 inet_confirm_addr(struct net *net, struct in_device *in_dev, __be32 dst, __be32 local,
			 int scope)
{
	return 0;
}

/* The dst cache caches nothing: every lookup asks ip_route_output_flow(). */
int dst_cache_init(struct dst_cache *dst_cache, gfp_t gfp)
{
	memset(dst_cache, 0, sizeof(*dst_cache));
	return 0;
}

void dst_cache_destroy(struct dst_cache *dst_cache)
{
}

void dst_cache_reset_now(struct dst_cache *dst_cache)
{
}

struct rtable *dst_cache_get_ip4(struct dst_cache *dst_cache, __be32 *saddr)
{
	return NULL;
}

void dst_cache_set_ip4(struct dst_cache *dst_cache, struct dst_entry *dst, __be32 saddr)
{
}

/* Tunnels report unreachable inner destinations with ICMP; RustOS's
 * stack sends no ICMP errors for them. */
void __icmp_send(struct sk_buff *skb_in, int type, int code, __be32 info,
		 const struct inet_skb_parm *opt)
{
}

/* Socket memory reserves (swap over the network): nothing to reserve. */
void sk_set_memalloc(struct sock *sk)
{
	sock_set_flag(sk, SOCK_MEMALLOC);
}

void sk_clear_memalloc(struct sock *sk)
{
	sock_reset_flag(sk, SOCK_MEMALLOC);
}

/* From net/ipv4/ip_tunnel_core.c. */
__be16 ip_tunnel_parse_protocol(const struct sk_buff *skb)
{
	if (skb_network_header(skb) >= skb->head &&
	    (skb_network_header(skb) + sizeof(struct iphdr)) <= skb_tail_pointer(skb) &&
	    ip_hdr(skb)->version == 4)
		return htons(ETH_P_IP);
	if (skb_network_header(skb) >= skb->head &&
	    (skb_network_header(skb) + sizeof(struct ipv6hdr)) <= skb_tail_pointer(skb) &&
	    ipv6_hdr(skb)->version == 6)
		return htons(ETH_P_IPV6);
	return 0;
}

const struct header_ops ip_tunnel_header_ops = { .parse_protocol = ip_tunnel_parse_protocol };
