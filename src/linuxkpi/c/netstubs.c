// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * Networking features RustOS does not offer, as Linux drivers see them:
 *
 * - XDP: no BPF programs can be attached (there is no bpf() syscall), so
 *   drivers always take their non-XDP paths. The rx-queue bookkeeping
 *   succeeds and the rest is never reached.
 * - tc flower offload (flow_block/flow_rule): no tc, so never configured.
 * - ethtool netlink extras (cable tests, MAC merge, self tests): reported
 *   as unsupported.
 *
 * Plus the ethtool string helpers drivers use for their statistics names.
 */
#include <linux/ethtool.h>
#include <linux/ethtool_netlink.h>
#include <linux/filter.h>
#include <linux/netdevice.h>
#include <linux/phy.h>
#include <net/flow_offload.h>
#include <net/selftests.h>
#include <net/xdp.h>
#include <net/xdp_sock_drv.h>
#include "kpi.h"

/* --------------------------------------------------------------- ethtool */

void ethtool_sprintf(u8 **data, const char *fmt, ...)
{
	va_list args;

	va_start(args, fmt);
	vsnprintf(*data, ETH_GSTRING_LEN, fmt, args);
	va_end(args);
	*data += ETH_GSTRING_LEN;
}

void ethtool_puts(u8 **data, const char *str)
{
	strscpy(*data, str, ETH_GSTRING_LEN);
	*data += ETH_GSTRING_LEN;
}

int ethnl_cable_test_alloc(struct phy_device *phydev, u8 cmd)
{
	return -EOPNOTSUPP;
}

void ethnl_cable_test_free(struct phy_device *phydev)
{
}

void ethnl_cable_test_finished(struct phy_device *phydev)
{
}

/* MAC merge (frame preemption) verification: never enabled. */
void ethtool_mmsv_init(struct ethtool_mmsv *mmsv, struct net_device *dev,
		       const struct ethtool_mmsv_ops *ops)
{
	mmsv->ops = ops;
	mmsv->dev = dev;
}

void ethtool_mmsv_stop(struct ethtool_mmsv *mmsv)
{
}

void ethtool_mmsv_link_state_handle(struct ethtool_mmsv *mmsv, bool up)
{
}

void ethtool_mmsv_event_handle(struct ethtool_mmsv *mmsv, enum ethtool_mmsv_event event)
{
}

void ethtool_mmsv_get_mm(struct ethtool_mmsv *mmsv, struct ethtool_mm_state *state)
{
	memset(state, 0, sizeof(*state));
}

void ethtool_mmsv_set_mm(struct ethtool_mmsv *mmsv, struct ethtool_mm_cfg *cfg)
{
}

int net_selftest_get_count(void)
{
	return 0;
}

void net_selftest_get_strings(u8 *data)
{
}

void net_selftest(struct net_device *ndev, struct ethtool_test *etest, u64 *buf)
{
}

/* ------------------------------------------------------ tc flow offload */

int flow_block_cb_setup_simple(struct flow_block_offload *f, struct list_head *driver_list,
			       flow_setup_cb_t *cb, void *cb_ident, void *cb_priv,
			       bool ingress_only)
{
	return -EOPNOTSUPP;
}

void flow_rule_match_basic(const struct flow_rule *rule, struct flow_match_basic *out)
{
	memset(out, 0, sizeof(*out));
}

void flow_rule_match_control(const struct flow_rule *rule, struct flow_match_control *out)
{
	memset(out, 0, sizeof(*out));
}

void flow_rule_match_eth_addrs(const struct flow_rule *rule, struct flow_match_eth_addrs *out)
{
	memset(out, 0, sizeof(*out));
}

void flow_rule_match_vlan(const struct flow_rule *rule, struct flow_match_vlan *out)
{
	memset(out, 0, sizeof(*out));
}

/* ------------------------------------------------------------------ XDP */

/* net/core/xdp.c's registration states. */
enum { KPI_XDP_UNUSED, KPI_XDP_NEW, KPI_XDP_REGISTERED, KPI_XDP_UNREGISTERED };

DEFINE_STATIC_KEY_FALSE(bpf_master_redirect_enabled_key);
DEFINE_STATIC_KEY_FALSE(bpf_stats_enabled_key);

int __xdp_rxq_info_reg(struct xdp_rxq_info *xdp_rxq, struct net_device *dev, u32 queue_index,
		       unsigned int napi_id, u32 frag_size)
{
	memset(xdp_rxq, 0, sizeof(*xdp_rxq));
	xdp_rxq->dev = dev;
	xdp_rxq->queue_index = queue_index;
	xdp_rxq->frag_size = frag_size;
	xdp_rxq->reg_state = KPI_XDP_REGISTERED;
	return 0;
}

void xdp_rxq_info_unreg(struct xdp_rxq_info *xdp_rxq)
{
	xdp_rxq->reg_state = KPI_XDP_UNREGISTERED;
	xdp_rxq->dev = NULL;
}

bool xdp_rxq_info_is_reg(struct xdp_rxq_info *xdp_rxq)
{
	return xdp_rxq->reg_state == KPI_XDP_REGISTERED;
}

int xdp_rxq_info_reg_mem_model(struct xdp_rxq_info *xdp_rxq, enum xdp_mem_type type,
			       void *allocator)
{
	xdp_rxq->mem.type = type;
	return 0;
}

void xdp_rxq_info_unreg_mem_model(struct xdp_rxq_info *xdp_rxq)
{
}

void xdp_features_set_redirect_target(struct net_device *dev, bool support_sg)
{
}

void xdp_features_clear_redirect_target(struct net_device *dev)
{
}

void xdp_warn(const char *msg, const char *func, const int line)
{
	pr_warn("XDP: %s (%s:%d)\n", msg, func, line);
}

void bpf_warn_invalid_xdp_action(const struct net_device *dev, const struct bpf_prog *prog,
				 u32 act)
{
}

int xdp_do_redirect(struct net_device *dev, struct xdp_buff *xdp, const struct bpf_prog *prog)
{
	return -EOPNOTSUPP;
}

void xdp_do_flush(void)
{
}

u32 xdp_master_redirect(struct xdp_buff *xdp)
{
	return XDP_ABORTED;
}

void xdp_return_frame(struct xdp_frame *xdpf)
{
	WARN_ONCE(1, "XDP frame without XDP\n");
}

void xdp_return_frame_rx_napi(struct xdp_frame *xdpf)
{
	WARN_ONCE(1, "XDP frame without XDP\n");
}

struct xdp_frame *xdp_convert_zc_to_xdp_frame(struct xdp_buff *xdp)
{
	return NULL;
}

struct sk_buff *xdp_build_skb_from_frame(struct xdp_frame *xdpf, struct net_device *dev)
{
	return NULL;
}
