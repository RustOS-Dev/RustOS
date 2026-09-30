//! Firmware command encoding for the iwlwifi "MVM" firmware API as spoken
//! by AX210 firmware (non-MLD station API).

use alloc::vec::Vec;
use wlan::chan::Band;

// Command groups.
pub const LEGACY: u8 = 0x0;
pub const SYSTEM: u8 = 0x2;
pub const MAC_CONF: u8 = 0x3;
pub const DATA_PATH: u8 = 0x5;
pub const REGULATORY_NVM: u8 = 0xC;

// Legacy group commands and notifications.
pub const ALIVE: u8 = 0x01;
pub const REPLY_ERROR: u8 = 0x02;
pub const INIT_COMPLETE: u8 = 0x04;
pub const PHY_CONTEXT: u8 = 0x08;
pub const SCAN_CFG: u8 = 0x0C;
pub const SCAN_REQ_UMAC: u8 = 0x0D;
pub const SCAN_ABORT_UMAC: u8 = 0x0E;
pub const SCAN_COMPLETE_UMAC: u8 = 0x0F;
pub const ADD_STA_KEY: u8 = 0x17;
pub const ADD_STA: u8 = 0x18;
pub const REMOVE_STA: u8 = 0x19;
pub const TX_CMD: u8 = 0x1C;
pub const SCD_QUEUE_CFG: u8 = 0x1D;
pub const MGMT_MCAST_KEY: u8 = 0x1F;
pub const MAC_CONTEXT: u8 = 0x28;
pub const BINDING: u8 = 0x2B;
pub const POWER_TABLE: u8 = 0x77;
pub const MAC_PM_POWER_TABLE: u8 = 0xA9;
pub const BEACON_FILTER_CONFIG: u8 = 0xD2;
pub const TX_ANT_CONFIG: u8 = 0x98;
pub const BT_CONFIG: u8 = 0x9B;
pub const MISSED_BEACONS: u8 = 0xA2;
pub const RX_MPDU: u8 = 0xC1;
pub const BAR_FRAME_RELEASE: u8 = 0xC2;
pub const FRAME_RELEASE: u8 = 0xC3;
pub const BA_NOTIF: u8 = 0xC5;
pub const MCC_UPDATE: u8 = 0xC8;
// System group.
pub const SOC_CONFIGURATION: u8 = 0x01;
pub const INIT_EXTENDED_CFG: u8 = 0x03;
// MAC config group.
pub const SESSION_PROTECTION: u8 = 0x05;
pub const SESSION_PROTECTION_NOTIF: u8 = 0xFB;
// Data path group.
pub const STA_HE_CTXT: u8 = 0x07;
pub const TLC_MNG_CONFIG: u8 = 0x0F;
pub const RX_BAID_ALLOCATION_CONFIG: u8 = 0x16;
pub const TLC_MNG_UPDATE_NOTIF: u8 = 0xF7;
// Regulatory/NVM group.
pub const NVM_ACCESS_COMPLETE: u8 = 0x00;
pub const PNVM_INIT_COMPLETE: u8 = 0xFE;

pub const FW_CTXT_ACTION_ADD: u32 = 1;
pub const FW_CTXT_ACTION_MODIFY: u32 = 2;
pub const FW_CTXT_ACTION_REMOVE: u32 = 3;
const FW_CTXT_INVALID: u32 = 0xFFFF_FFFF;
const FW_MAC_TYPE_BSS_STA: u32 = 5;
pub const PHY_BAND_5: u8 = 0;
pub const PHY_BAND_24: u8 = 1;

pub const MAC_ID: u32 = 0;
pub const PHY_ID: u32 = 0;
pub const AP_STA_ID: u8 = 0;
pub const MGMT_TID: u8 = 15;
/// Reorder-data BAID meaning "no block-ack session".
pub const INVALID_BAID: u8 = 0x7F;

/// Little-endian command builder.
#[derive(Default)]
pub struct Cmd(pub Vec<u8>);

impl Cmd {
    pub fn new() -> Cmd {
        Cmd(Vec::with_capacity(64))
    }
    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }
    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.0.extend_from_slice(b);
        self
    }
    pub fn zeros(&mut self, n: usize) -> &mut Self {
        self.0.resize(self.0.len() + n, 0);
        self
    }
    pub fn done(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.0)
    }
}

fn id_color(id: u32) -> u32 {
    id // color 0
}

pub const PHY_BAND_6: u8 = 2;

/// Firmware band code.
pub fn phy_band(b: Band) -> u8 {
    match b {
        Band::B2G => PHY_BAND_24,
        Band::B5G => PHY_BAND_5,
        Band::B6G => PHY_BAND_6,
    }
}

/// PHY_CONTEXT_CMD v3/v4 (UHB channel info). `channel` is the primary
/// channel; `width` 0-3 = 20/40/80/160 MHz; `ctrl_pos` the position of
/// the primary channel in the bandwidth (PHY_VHT_CTRL_POS_*).
pub fn phy_context(
    action: u32,
    band: Band,
    channel: u8,
    width: u8,
    ctrl_pos: u8,
    rx_ant: u32,
) -> Vec<u8> {
    let rxchain = (rx_ant << 1) | (2 << 10) | (2 << 12);
    Cmd::new()
        .u32(id_color(PHY_ID))
        .u32(action)
        .u32(channel as u32)
        .u8(phy_band(band))
        .u8(width)
        .u8(ctrl_pos)
        .u8(0)
        .u32(0) // lmac id (no CDB)
        .u32(rxchain)
        .u32(0)
        .u32(0)
        .done()
}

/// BINDING_CONTEXT_CMD v2 (with lmac id).
pub fn binding(action: u32) -> Vec<u8> {
    Cmd::new()
        .u32(id_color(PHY_ID))
        .u32(action)
        .u32(if action == FW_CTXT_ACTION_REMOVE {
            FW_CTXT_INVALID
        } else {
            id_color(MAC_ID)
        })
        .u32(FW_CTXT_INVALID)
        .u32(FW_CTXT_INVALID)
        .u32(id_color(PHY_ID))
        .u32(0)
        .done()
}

pub struct MacParams<'a> {
    pub action: u32,
    pub own: [u8; 6],
    pub bssid: [u8; 6],
    pub assoc: Option<(u16, u16, u8)>, // (aid, beacon interval, dtim period)
    pub qos: bool,
    pub band_5g: bool,
    pub short_slot: bool,
    pub short_preamble: bool,
    /// HT or better: HT protection mode from the AP's HT operation.
    pub ht: Option<u8>,
    /// Channel wider than 20 MHz.
    pub wide: bool,
    /// ERP (802.11g) protection.
    pub erp_protection: bool,
    pub he: bool,
    /// EDCA parameters per firmware AC (BK, BE, VI, VO).
    pub edca: &'a [wlan::caps::Edca; 4],
}

/// MAC_CONTEXT_CMD for a BSS station.
pub fn mac_context(p: &MacParams) -> Vec<u8> {
    const FILTER_ACCEPT_GRP: u32 = 1 << 2;
    const FILTER_IN_BEACON: u32 = 1 << 6;
    let mut c = Cmd::new();
    c.u32(id_color(MAC_ID))
        .u32(p.action)
        .u32(FW_MAC_TYPE_BSS_STA)
        .u32(0) // TSF id
        .bytes(&p.own)
        .u16(0)
        .bytes(&p.bssid)
        .u16(0)
        .u32(0xF) // CCK ACK rates 1..11
        .u32(0x15); // OFDM ACK rates 6, 12, 24
    // Protection: 11g (ERP) and HT (iwl_mvm_mac_ctxt_set_ht_flags).
    const PROT_TGG: u32 = 1 << 3;
    const PROT_TGN: u32 = 1 << 8;
    const PROT_HT: u32 = 1 << 23;
    const PROT_FAT: u32 = 1 << 24;
    let mut prot = if p.erp_protection { PROT_TGG } else { 0 };
    if let Some(mode) = p.ht {
        prot |= PROT_TGN;
        match mode {
            1 | 3 => prot |= PROT_HT | PROT_FAT,
            2 if p.wide => prot |= PROT_HT | PROT_FAT,
            _ => {}
        }
    }
    c.u32(prot)
        .u32(if p.short_preamble { 1 << 5 } else { 0 })
        .u32(if p.short_slot || p.band_5g { 1 << 4 } else { 0 });
    const FILTER_IN_11AX: u32 = 1 << 14;
    let mut filter = FILTER_ACCEPT_GRP;
    if p.assoc.is_none() {
        filter |= FILTER_IN_BEACON;
    }
    if p.he {
        filter |= FILTER_IN_11AX;
    }
    const QOS_UPDATE_EDCA: u32 = 1 << 0;
    const QOS_TGN: u32 = 1 << 1;
    let mut qos = if p.qos { QOS_UPDATE_EDCA } else { 0 };
    if p.ht.is_some() {
        qos |= QOS_TGN;
    }
    c.u32(filter).u32(qos);
    // EDCA per firmware AC (BK, BE, VI, VO) with the gen2 TX FIFO of that
    // AC (BK=1 .. VO=4), plus one unused entry.
    for (i, e) in p.edca.iter().enumerate() {
        c.u16(e.cw_min)
            .u16(e.cw_max)
            .u8(e.aifsn)
            .u8(1 << (i + 1))
            .u16(e.txop_us);
    }
    c.zeros(8);
    // iwl_mac_data_sta (44 bytes) inside a 48-byte union.
    let (aid, bi, dtim) = p.assoc.unwrap_or((0, 100, 1));
    c.u32(p.assoc.is_some() as u32)
        .u32(0) // dtim_time
        .u64(0) // dtim_tsf
        .u32(bi as u32)
        .u32(0)
        .u32(bi as u32 * dtim.max(1) as u32)
        .u32(0) // data policy
        .u32(10) // listen interval
        .u32(aid as u32)
        .u32(0)
        .zeros(4);
    c.done()
}

/// Station flags for ADD_STA from the negotiated link.
pub fn sta_flags(link: &wlan::caps::Link, tx_chains: u8) -> (u32, u32) {
    use wlan::caps::{Mode, Width};
    const RTS_MIMO_PROT: u32 = 1 << 17;
    const AGG_SIZE_SHIFT: u32 = 19;
    const AGG_SIZE_MSK: u32 = 0xF << 19;
    const DENS_SHIFT: u32 = 23;
    const DENS_MSK: u32 = 7 << 23;
    const FAT_MSK: u32 = 3 << 26;
    const MIMO_MSK: u32 = 3 << 28;
    let mut flags = 0;
    let mut mask = FAT_MSK | MIMO_MSK | RTS_MIMO_PROT;
    if link.mode == Mode::Legacy {
        return (flags, mask);
    }
    flags |= match link.width {
        Width::W20 => 0,
        Width::W40 => 1 << 26,
        Width::W80 => 2 << 26,
        Width::W160 => 3 << 26,
    };
    if link.nss >= 2 && tx_chains >= 2 {
        flags |= 1 << 28;
        if link.smps == 1 {
            flags |= RTS_MIMO_PROT;
        }
    }
    mask |= AGG_SIZE_MSK | DENS_MSK;
    flags |= ((link.ampdu_exp.min(9) as u32) << AGG_SIZE_SHIFT)
        | ((link.ampdu_density as u32) << DENS_SHIFT);
    (flags, mask)
}

/// ADD_STA v12 (iwl_mvm_add_sta_cmd) for the AP station: add, or modify
/// everything. `tid_disable` has a bit set for every TID without TX
/// aggregation.
pub fn add_sta(
    modify: bool,
    bssid: [u8; 6],
    aid: u16,
    flags: (u32, u32),
    tid_disable: u16,
    uapsd: bool,
) -> Vec<u8> {
    const MODIFY_UAPSD_ACS: u8 = 1 << 2;
    Cmd::new()
        .u8(modify as u8)
        .u8(0) // awake ACs
        .u16(tid_disable)
        .u32(id_color(MAC_ID))
        .bytes(&bssid)
        .u16(0)
        .u8(AP_STA_ID)
        .u8(if uapsd { MODIFY_UAPSD_ACS } else { 0 })
        .u16(0)
        .u32(flags.0)
        .u32(flags.1)
        .u8(0)
        .u8(0)
        .u16(0)
        .u16(0)
        .u8(0)
        .u8(0) // station type: link
        .u16(aid)
        .u16(0)
        .u32(0)
        .u16(0)
        .u8(0)
        .u8(0)
        .done()
}

/// ADD_STA modify of only the per-TID TX aggregation mask
/// (STA_MODIFY_TID_DISABLE_TX), as `iwl_mvm_sta_tx_agg` sends it.
pub fn add_sta_tid_disable(tid_disable: u16) -> Vec<u8> {
    let mut c = add_sta(true, [0; 6], 0, (0, 0), tid_disable, false);
    c[17] = 1 << 1; // modify mask
    c
}

/// ADD_STA modify starting (or stopping) a receive block-ack session
/// (`iwl_mvm_fw_baid_op_sta`). The response status carries the BAID.
pub fn add_sta_rx_ba(start: bool, tid: u8, ssn: u16, win: u16) -> Vec<u8> {
    const MODIFY_ADD_BA_TID: u8 = 1 << 3;
    const MODIFY_REMOVE_BA_TID: u8 = 1 << 4;
    let mut c = add_sta(true, [0; 6], 0, (0, 0), 0, false);
    // Offsets in iwl_mvm_add_sta_cmd: modify_mask 17, add_immediate_ba_tid
    // 28, remove_immediate_ba_tid 29, add_immediate_ba_ssn 30,
    // rx_ba_window 44.
    if start {
        c[17] = MODIFY_ADD_BA_TID;
        c[28] = tid;
        c[30..32].copy_from_slice(&ssn.to_le_bytes());
        c[44..46].copy_from_slice(&win.to_le_bytes());
    } else {
        c[17] = MODIFY_REMOVE_BA_TID;
        c[29] = tid;
    }
    c
}

/// BAID from an ADD_STA response status (valid bit 15, BAID bits 8-14).
pub fn add_sta_baid(status: u32) -> Option<u8> {
    (status & 0xFF == 1 && status & 0x8000 != 0).then_some(((status >> 8) & 0x7F) as u8)
}

const BAID_ACTION_ALLOC: u32 = 0;
const BAID_ACTION_REMOVE: u32 = 2;

/// RX_BAID_ALLOCATION_CONFIG_CMD alloc (`iwl_rx_baid_cfg_cmd`, 16 bytes):
/// action, sta_id_mask, tid, 3 reserved, ssn, win_size. The response is
/// the BAID as a u32.
pub fn rx_baid_alloc(tid: u8, ssn: u16, win: u16) -> Vec<u8> {
    Cmd::new()
        .u32(BAID_ACTION_ALLOC)
        .u32(1 << AP_STA_ID)
        .u8(tid)
        .zeros(3)
        .u16(ssn)
        .u16(win)
        .done()
}

/// RX_BAID_ALLOCATION_CONFIG_CMD remove: v1 names the BAID, v2 the
/// station mask and TID. Padded to the 16-byte command size.
pub fn rx_baid_remove(ver: u8, baid: u8, tid: u8) -> Vec<u8> {
    let mut c = Cmd::new();
    c.u32(BAID_ACTION_REMOVE);
    if ver >= 2 {
        c.u32(1 << AP_STA_ID).u32(tid as u32);
    } else {
        c.u32(baid as u32).zeros(4);
    }
    c.zeros(4).done()
}

/// BAID from an RX_BAID_ALLOCATION_CONFIG response.
pub fn rx_baid_resp(r: &[u8]) -> Option<u8> {
    let b = u32::from_le_bytes(r.get(0..4)?.try_into().ok()?);
    (b < INVALID_BAID as u32).then_some(b as u8)
}

/// BT_CONFIG (`iwl_bt_coex_cmd`): with coexistence the firmware shares
/// the antenna with the Bluetooth core of the same card (mode NW, the
/// Linux default modules: MPLUT, sync to SCO, high-band retention);
/// without, WiFi owns the antenna (mode WIFI).
pub fn bt_coex(enabled: bool) -> Vec<u8> {
    const MODE_NW: u32 = 1;
    const MODE_WIFI: u32 = 3;
    const MPLUT: u32 = 1 << 0;
    const SYNC2SCO: u32 = 1 << 2;
    const HIGH_BAND_RET: u32 = 1 << 4;
    if enabled {
        Cmd::new()
            .u32(MODE_NW)
            .u32(MPLUT | SYNC2SCO | HIGH_BAND_RET)
            .done()
    } else {
        Cmd::new().u32(MODE_WIFI).u32(0).done()
    }
}

/// POWER_TABLE_CMD (`iwl_device_power_cmd`): device-wide power save.
pub fn device_power(ps: bool) -> Vec<u8> {
    Cmd::new().u16(ps as u16).u16(0).done()
}

/// MAC_PM_POWER_TABLE (`iwl_mac_power_cmd`, 40 bytes) for the station
/// MAC, as `iwl_mvm_power_build_cmd` fills it in the "balanced" scheme:
/// power management always on, power save while associated, low-power RX,
/// and 100 ms of traffic keeping the radio awake. uAPSD stays off.
pub fn mac_power(ps: bool, beacon_int: u16, dtim: u8) -> Vec<u8> {
    const PM_ENA: u16 = 1 << 1;
    const PS_ENA: u16 = 1 << 0;
    const LPRX_ENA: u16 = 1 << 11;
    const ADVANCE_PM: u16 = 1 << 9;
    let mut flags = PM_ENA;
    if ps {
        flags |= PS_ENA | LPRX_ENA | ADVANCE_PM;
    }
    // Keep-alive: at least three DTIM periods, and at least 25 s.
    let dtim_ms = beacon_int as u32 * 1024 / 1000 * dtim.max(1) as u32;
    let keep_alive = (3 * dtim_ms).div_ceil(1000).max(25) as u16;
    let timeout_us = if ps { 100_000 } else { 0 };
    Cmd::new()
        .u32(id_color(MAC_ID))
        .u16(flags)
        .u16(keep_alive)
        .u32(timeout_us) // rx_data_timeout
        .u32(timeout_us) // tx_data_timeout
        .u32(0) // rx_data_timeout_uapsd
        .u32(0) // tx_data_timeout_uapsd
        .u8(if ps { 75 } else { 0 }) // lprx_rssi_threshold
        .u8(0) // skip_dtim_periods
        .u16(0) // snooze_interval
        .u16(0) // snooze_window
        .u8(0) // snooze_step
        .u8(0) // qndp_tid
        .u8(0) // uapsd_ac_flags
        .u8(0) // uapsd_max_sp
        .zeros(4) // heavy tx/rx thresholds
        .u8(0) // limited_ps_threshold
        .u8(0)
        .done()
}

/// BEACON_FILTER_CONFIG_CMD (`iwl_beacon_filter_cmd`) with the Linux
/// defaults: v3 is 11 words, v4 adds the absolute RSSI thresholds.
/// Beacons that change nothing we track are dropped by the firmware.
pub fn beacon_filter(ver: u8, enable: bool) -> Vec<u8> {
    let mut c = Cmd::new();
    c.u32(5) // bf_energy_delta
        .u32(1) // bf_roaming_energy_delta
        .u32(72) // bf_roaming_state
        .u32(112) // bf_temp_threshold
        .u32(1) // bf_temp_fast_filter
        .u32(5) // bf_temp_slow_filter
        .u32(enable as u32) // bf_enable_beacon_filter
        .u32(0) // bf_debug_flag
        .u32(50) // bf_escape_timer
        .u32(6) // ba_escape_timer
        .u32(enable as u32); // ba_enable_beacon_abort
    if ver >= 4 {
        c.zeros(16);
    }
    c.done()
}

pub fn remove_sta(sta_id: u8) -> Vec<u8> {
    Cmd::new().u8(sta_id).zeros(3).done()
}

/// SCD_QUEUE_CFG v2: allocate a TX queue (response: queue id + write ptr).
pub fn tx_queue_cfg(sta_id: u8, tid: u8, cb_size: u32, bc: u64, tfds: u64) -> Vec<u8> {
    Cmd::new()
        .u8(sta_id)
        .u8(tid)
        .u16(1) // enable
        .u32(cb_size)
        .u64(bc)
        .u64(tfds)
        .done()
}

/// SESSION_PROTECTION_CMD: stay on channel for association.
pub fn session_protection(action: u32, duration_tu: u32) -> Vec<u8> {
    Cmd::new()
        .u32(id_color(MAC_ID))
        .u32(action)
        .u32(0) // SESSION_PROTECT_CONF_ASSOC
        .u32(duration_tu)
        .u32(1)
        .u32(0)
        .done()
}

/// ADD_STA_KEY v3 (pairwise or group CCMP/GCMP key).
pub fn add_sta_key(
    key: &[u8],
    keyidx: u8,
    offset: u8,
    gcmp: bool,
    mcast: bool,
    mfp: bool,
    rx_pn: [u8; 6],
) -> Vec<u8> {
    const KEY_FLG_CCM: u16 = 2;
    const KEY_FLG_GCMP: u16 = 5;
    const KEY_FLG_WEP_KEY_MAP: u16 = 1 << 3;
    const KEY_32BYTES: u16 = 1 << 12;
    const KEY_MULTICAST: u16 = 1 << 14;
    const KEY_MFP: u16 = 1 << 15;
    let mut flags = ((keyidx as u16) << 8) & 0x300 | KEY_FLG_WEP_KEY_MAP;
    flags |= if gcmp { KEY_FLG_GCMP } else { KEY_FLG_CCM };
    if key.len() == 32 {
        flags |= KEY_32BYTES;
    }
    if mcast {
        flags |= KEY_MULTICAST;
    }
    if mfp {
        flags |= KEY_MFP;
    }
    let mut k = [0u8; 32];
    k[..key.len().min(32)].copy_from_slice(&key[..key.len().min(32)]);
    let mut seq = [0u8; 16];
    seq[..6].copy_from_slice(&rx_pn);
    Cmd::new()
        .u8(AP_STA_ID)
        .u8(offset)
        .u16(flags)
        .bytes(&k)
        .bytes(&seq)
        .u64(0)
        .u64(0)
        .u64(0)
        .done()
}

/// MGMT_MCAST_KEY: install the IGTK (BIP-CMAC-128) for PMF.
pub fn igtk(key: &[u8], keyidx: u16, ipn: [u8; 6]) -> Vec<u8> {
    let mut k = [0u8; 32];
    k[..key.len().min(32)].copy_from_slice(&key[..key.len().min(32)]);
    let mut rsc = 0u64;
    for (i, b) in ipn.iter().enumerate() {
        rsc |= (*b as u64) << (8 * i);
    }
    Cmd::new()
        .u32(2) // STA_KEY_FLG_CCM (BIP-CMAC)
        .bytes(&k)
        .u32(keyidx as u32)
        .u32(AP_STA_ID as u32)
        .u64(rsc)
        .done()
}

/// TLC_MNG_CONFIG_CMD v4: firmware rate scaling for the negotiated link.
pub fn tlc_config(link: &wlan::caps::Link, band_5g: bool, chains: u8) -> Vec<u8> {
    use wlan::caps::Mode;
    // Legacy rate bitmap: bits 0-3 CCK, 4-11 OFDM.
    let non_ht: u16 = if band_5g { 0xFF0 } else { 0xFFF };
    let mode = match link.mode {
        Mode::Legacy => 0,
        Mode::Ht => 1,
        Mode::Vht => 2,
        Mode::He => 3,
    };
    const FLAG_STBC: u16 = 1 << 0;
    const FLAG_LDPC: u16 = 1 << 1;
    let mut flags = 0;
    if link.stbc && chains.count_ones() > 1 {
        flags |= FLAG_STBC;
    }
    if link.ldpc {
        flags |= FLAG_LDPC;
    }
    let mut c = Cmd::new();
    c.u8(AP_STA_ID)
        .zeros(3)
        .u8(if link.mode == Mode::Legacy {
            0
        } else {
            link.width.code()
        })
        .u8(mode)
        .u8(chains)
        .u8(link.sgi)
        .u16(flags)
        .u16(non_ht);
    // ht_rates[nss][bw]: bw 0 = up to 80 MHz, 1 = 160 MHz, 2 = 320 MHz.
    for s in 0..2 {
        c.u16(link.mcs[s][0]).u16(link.mcs[s][1]).u16(0);
    }
    c.u16(if link.max_mpdu == 0 {
        3839
    } else {
        link.max_mpdu
    })
    .u16(0);
    c.done()
}

/// STA_HE_CTXT_CMD v2 (88 bytes) or v3 (92 bytes) for the AP station.
pub fn he_sta_context(version: u8, he: &wlan::caps::HeLink) -> Vec<u8> {
    const FLAG_BSS_COLOR_DIS: u32 = 1 << 5;
    const FLAG_PACKET_EXT: u32 = 1 << 8;
    const FLAG_ACK_ENABLED: u32 = 1 << 11;
    const FLAG_MU_EDCA_CW: u32 = 1 << 12;
    const HE_MAC2_ACK_EN: u8 = 1 << 1;
    let mut flags = 0;
    if he.color_disabled {
        flags |= FLAG_BSS_COLOR_DIS;
    }
    if he.pkt_ext.is_some() {
        flags |= FLAG_PACKET_EXT;
    }
    if he.mac[2] & HE_MAC2_ACK_EN != 0 {
        flags |= FLAG_ACK_ENABLED;
    }
    if he.mu_edca.is_some() {
        flags |= FLAG_MU_EDCA_CW;
    }
    let mut c = Cmd::new();
    c.u8(AP_STA_ID).u8(8).u8(0).u8(0).u32(flags);
    c.zeros(6).u16(0); // reference BSSID (no multiple BSSID)
    c.u32(0); // HTC flags
    c.u8(0).u8(0).u8(0).u8(0); // fragmentation off
    // Packet extension thresholds: [2 streams][4 or 5 widths][low, high].
    let widths = if version >= 3 { 5 } else { 4 };
    let pe = he
        .pkt_ext
        .unwrap_or([[[wlan::caps::PKT_EXT_NONE; 2]; 5]; 2]);
    for s in pe.iter() {
        for th in s.iter().take(widths) {
            c.u8(th[0]).u8(th[1]);
        }
    }
    c.u8(he.bss_color).u8(he.default_pe).u16(he.rts_threshold);
    c.u8(0).u8(0).u16(0); // random access parameters, puncturing
    // Trigger-based EDCA per firmware AC (BK, BE, VI, VO).
    for ac in 0..4 {
        match he.mu_edca {
            Some(m) => {
                let (aifsn, ecw_min, ecw_max, timer) = m[ac];
                c.u16(ecw_min as u16)
                    .u16(ecw_max as u16)
                    .u16(aifsn as u16)
                    .u16(timer as u16);
            }
            None => {
                c.zeros(8);
            }
        }
    }
    c.u8(0).u8(0).u8(0).u8(0).u8(0).zeros(3);
    c.done()
}

/// SCAN_CFG v5.
pub fn scan_config(tx_ant: u32, rx_ant: u32) -> Vec<u8> {
    Cmd::new()
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u32(tx_ant)
        .u32(rx_ant)
        .done()
}

/// UHB channel flags for 6 GHz channel entries: listen only (no probe
/// requests) unless an RNR told us which BSSIDs / short SSIDs to probe.
const UHB_CHAN_FORCE_PASSIVE: u32 = 1 << 26;

pub const SCAN_CHANNELS_24: &[u8] = &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13];
pub const SCAN_CHANNELS_5: &[u8] = &[
    36, 40, 44, 48, 52, 56, 60, 64, 100, 104, 108, 112, 116, 120, 124, 128, 132, 136, 140, 144,
    149, 153, 157, 161, 165,
];

/// SCAN_REQ_UMAC v15 (iwl_scan_req_umac_v17 layout) for a one-shot
/// active scan of `channels`, optionally directed at `ssid`. `caps24` and
/// `caps5` are extra probe request elements (HT/VHT/HE capabilities) for
/// each band.
/// 6 GHz discovery data for a scan: BSSIDs and short SSIDs learned from
/// Reduced Neighbor Reports (up to 16 and 8), and the 6 GHz probe
/// request elements.
#[derive(Default)]
pub struct Scan6g<'a> {
    pub bssids: &'a [(u8, [u8; 6])],
    pub short_ssids: &'a [(u8, u32)],
    pub caps: &'a [u8],
}

pub fn scan_request(
    own: [u8; 6],
    channels: &[(Band, u8)],
    ssid: &[u8],
    associated: bool,
    caps24: &[u8],
    caps5: &[u8],
    six: &Scan6g,
) -> Vec<u8> {
    const GEN_FLAGS_PASS_ALL: u16 = 1 << 1;
    const GEN_FLAGS_ADAPTIVE_DWELL: u16 = 1 << 7;
    const CHAN_FLAG_ENABLE_CHAN_ORDER: u8 = 1 << 5;
    let mut c = Cmd::new();
    c.u32(0) // uid
        .u32(6); // ooc priority: EXT_6
    // General parameters (v11, 36 bytes).
    let (max_out, suspend) = if associated { (120, 30) } else { (0, 0) };
    c.u16(GEN_FLAGS_PASS_ALL | GEN_FLAGS_ADAPTIVE_DWELL)
        .u8(0)
        .u8(MAC_ID as u8)
        .u8(10)
        .u8(10) // active dwell
        .u8(2)
        .u8(8)
        .u8(10) // adaptive dwell defaults (2 GHz, 5 GHz, social)
        .u8(0) // flags2
        .u16(if ssid.is_empty() { 300 } else { 100 })
        .u32(max_out)
        .u32(max_out)
        .u32(suspend)
        .u32(suspend)
        .u32(6) // priority
        .u8(110)
        .u8(110) // passive dwell
        .u8(0)
        .u8(0);
    // Channel parameters (v7): flags, count, n_aps_override[2], 67 entries.
    c.u8(CHAN_FLAG_ENABLE_CHAN_ORDER)
        .u8(channels.len().min(67) as u8)
        .u8(10)
        .u8(2);
    let bssids = &six.bssids[..six.bssids.len().min(16)];
    let short_ssids = &six.short_ssids[..six.short_ssids.len().min(8)];
    for i in 0..67 {
        match channels.get(i) {
            Some(&(Band::B6G, ch)) => {
                // Probe for the BSSIDs / short SSIDs reported on this
                // channel (bitmaps into the arrays below); listen only
                // when nothing was reported.
                let mut flags = 0u32;
                for (j, (c6, _)) in bssids.iter().enumerate() {
                    if *c6 == ch {
                        flags |= 1 << j;
                    }
                }
                for (j, (c6, _)) in short_ssids.iter().enumerate() {
                    if *c6 == ch {
                        flags |= 1 << (16 + j);
                    }
                }
                if flags == 0 {
                    flags = UHB_CHAN_FORCE_PASSIVE;
                }
                c.u32(flags).u8(ch).u8(PHY_BAND_6).u8(1).u8(0);
            }
            Some(&(band, ch)) => {
                // Probe with direct_scan[0] (the SSID, or wildcard).
                c.u32(1).u8(ch).u8(phy_band(band)).u8(1).u8(0);
            }
            None => {
                c.zeros(8);
            }
        }
    }
    // Periodic parameters: one iteration.
    c.u16(0).u8(1).u8(0).u16(0).u8(0).u8(0).u16(0).u16(0);
    // Probe request template (iwl_scan_probe_req).
    let mut buf = Vec::new();
    buf.extend_from_slice(&[0x40, 0x00, 0, 0]); // probe request, duration
    buf.extend_from_slice(&[0xFF; 6]);
    buf.extend_from_slice(&own);
    buf.extend_from_slice(&[0xFF; 6]);
    buf.extend_from_slice(&[0, 0]);
    buf.extend_from_slice(&[0, 0]); // wildcard SSID element
    let mac_len = buf.len();
    let b24 = buf.len();
    buf.extend_from_slice(&[1, 8, 0x82, 0x84, 0x8B, 0x96, 0x0C, 0x12, 0x18, 0x24]);
    buf.extend_from_slice(&[50, 4, 0x30, 0x48, 0x60, 0x6C]);
    buf.extend_from_slice(&[3, 1, 0]); // DS parameter set (firmware fills)
    buf.extend_from_slice(caps24);
    let b24_len = buf.len() - b24;
    let b5 = buf.len();
    buf.extend_from_slice(&[1, 8, 0x8C, 0x12, 0x98, 0x24, 0xB0, 0x48, 0x60, 0x6C]);
    buf.extend_from_slice(caps5);
    let b5_len = buf.len() - b5;
    let b6 = buf.len();
    if !six.caps.is_empty() {
        buf.extend_from_slice(&[1, 8, 0x8C, 0x12, 0x98, 0x24, 0xB0, 0x48, 0x60, 0x6C]);
        buf.extend_from_slice(six.caps);
    }
    let b6_len = buf.len() - b6;
    let common = buf.len();
    c.u16(0).u16(mac_len as u16);
    c.u16(b24 as u16).u16(b24_len as u16);
    c.u16(b5 as u16).u16(b5_len as u16);
    c.u16(b6 as u16).u16(b6_len as u16);
    c.u16(common as u16).u16(0); // common data
    buf.resize(512, 0);
    c.bytes(&buf[..512]);
    // short_ssid_num, bssid_num, reserved
    c.u8(short_ssids.len() as u8).u8(bssids.len() as u8).u16(0);
    // direct_scan[20] (id, len, ssid[32])
    for i in 0..20 {
        if i == 0 {
            let mut e = [0u8; 34];
            e[1] = ssid.len().min(32) as u8;
            e[2..2 + ssid.len().min(32)].copy_from_slice(&ssid[..ssid.len().min(32)]);
            c.bytes(&e);
        } else {
            c.zeros(34);
        }
    }
    for i in 0..8 {
        c.u32(short_ssids.get(i).map_or(0, |s| s.1));
    }
    for i in 0..16 {
        c.bytes(&bssids.get(i).map_or([0; 6], |b| b.1));
    }
    c.done()
}

/// Rate for management frames and frames sent before rate scaling is
/// configured: 1 Mb/s CCK on 2.4 GHz, 6 Mb/s OFDM on 5 GHz. `v2` selects
/// the rate_n_flags format of TX_CMD version 9 and later.
pub fn basic_rate(band_5g: bool, tx_ant: u32, v2: bool) -> u32 {
    let ant = if band_5g {
        1 << 14 // antenna A
    } else if tx_ant & 2 != 0 {
        2 << 14 // antenna B (not shared with Bluetooth)
    } else {
        1 << 14
    };
    match (v2, band_5g) {
        (true, true) => (1 << 8) | ant, // legacy OFDM, index 0 (6 Mb/s)
        (true, false) => ant,           // CCK, index 0 (1 Mb/s)
        (false, true) => 13 | ant,      // PLCP 6 Mb/s
        (false, false) => 10 | (1 << 9) | ant, // PLCP 1 Mb/s, CCK
    }
}

/// Describe a rate_n_flags value ("HE-MCS 11 2SS 80MHz", "54 Mb/s").
pub fn describe_rate(r: u32, v2: bool) -> alloc::string::String {
    use alloc::format;
    const CCK: [&str; 4] = ["1", "2", "5.5", "11"];
    const OFDM: [&str; 8] = ["6", "9", "12", "18", "24", "36", "48", "54"];
    const OFDM_PLCP: [u32; 8] = [13, 15, 5, 7, 9, 11, 1, 3];
    const CCK_PLCP: [u32; 4] = [10, 20, 55, 110];
    if v2 {
        let width = 20 << ((r >> 11) & 7);
        let nss = ((r >> 4) & 1) + 1;
        let idx = (r & 0xF) as usize;
        return match (r >> 8) & 7 {
            0 => format!("{} Mb/s", CCK.get(idx).unwrap_or(&"?")),
            1 => format!("{} Mb/s", OFDM.get(idx).unwrap_or(&"?")),
            2 => format!("HT-MCS {} {}MHz", (nss - 1) * 8 + (r & 7), width),
            3 => format!("VHT-MCS {} {}SS {}MHz", idx, nss, width),
            4 => format!("HE-MCS {} {}SS {}MHz", idx, nss, width),
            _ => format!("EHT-MCS {} {}SS {}MHz", idx, nss, width),
        };
    }
    let width = 20 << ((r >> 11) & 3);
    if r & (1 << 26) != 0 {
        return format!("VHT-MCS {} {}SS {}MHz", r & 0xF, ((r >> 4) & 3) + 1, width);
    }
    if r & (1 << 8) != 0 {
        return format!("HT-MCS {} {}MHz", r & 0x3F, width);
    }
    let plcp = r & 0xFF;
    if r & (1 << 9) != 0 {
        let i = CCK_PLCP.iter().position(|&p| p == plcp);
        return format!("{} Mb/s", i.map_or("?", |i| CCK[i]));
    }
    let i = OFDM_PLCP.iter().position(|&p| p == plcp);
    format!("{} Mb/s", i.map_or("?", |i| OFDM[i]))
}

/// Channels in the order of the firmware's NVM channel list (UHB devices:
/// 2.4 GHz, 5 GHz, then the 59 6 GHz channels 1, 5, ... 233).
pub const NVM_CHANNELS: &[u8] = &[
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 36, 40, 44, 48, 52, 56, 60, 64, 68, 72, 76, 80,
    84, 88, 92, 96, 100, 104, 108, 112, 116, 120, 124, 128, 132, 136, 140, 144, 149, 153, 157, 161,
    165, 169, 173, 177, 181,
];

/// Band and number of NVM channel list entry `i`.
pub fn nvm_channel(i: usize) -> Option<(Band, u8)> {
    match NVM_CHANNELS.get(i) {
        Some(&c) => Some((Band::of_legacy(c), c)),
        None => {
            let j = i - NVM_CHANNELS.len();
            (j < 59).then(|| (Band::B6G, 1 + 4 * j as u8))
        }
    }
}
pub const NVM_CHANNEL_VALID: u32 = 1 << 0;
pub const NVM_CHANNEL_ACTIVE: u32 = 1 << 3;

/// Parse an MCC_UPDATE response (v3, v4 or v8 layout, told apart by the
/// channel count matching the length): (country code, per-channel flags
/// for the 2.4/5 GHz channels in `NVM_CHANNELS`).
/// Band, channel number and its NVM flags.
pub type ChannelFlags = Vec<(Band, u8, u32)>;

pub fn parse_mcc_response(d: &[u8]) -> Option<([u8; 2], ChannelFlags)> {
    let le32 = |o: usize| {
        d.get(o..o + 4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
    };
    for off in [20usize, 16, 12] {
        let Some(n) = le32(off) else { continue };
        let n = n as usize;
        if n == 0 || n > 128 || off + 4 + 4 * n != d.len() {
            continue;
        }
        let mcc = [d[5], d[4]];
        let chans = (0..n)
            .filter_map(|i| {
                let (b, c) = nvm_channel(i)?;
                Some((b, c, le32(off + 4 + 4 * i).unwrap_or(0)))
            })
            .collect();
        return Some((mcc, chans));
    }
    None
}

/// Build the device TX command (cmd header + iwl_tx_cmd_gen3 + 802.11
/// header) for `frame`. Returns (buffer, TB1 end offset).
pub fn tx_command(
    frame: &[u8],
    hdrlen: usize,
    rate: Option<u32>,
    encrypt: bool,
    high_pri: bool,
) -> (Vec<u8>, usize) {
    const FLAGS_CMD_RATE: u16 = 1 << 0;
    const FLAGS_ENCRYPT_DIS: u16 = 1 << 1;
    const FLAGS_HIGH_PRI: u16 = 1 << 2;
    let mut flags = 0;
    if rate.is_some() {
        flags |= FLAGS_CMD_RATE;
    }
    if !encrypt {
        flags |= FLAGS_ENCRYPT_DIS;
    }
    if high_pri {
        flags |= FLAGS_HIGH_PRI;
    }
    let pad = !hdrlen.is_multiple_of(4);
    let mut offload = ((hdrlen as u32 / 2) & 0x1F) << 8;
    if pad {
        offload |= 1 << 13;
    }
    let mut c = Cmd::new();
    // iwl_cmd_header: cmd, group, sequence (filled by the transport).
    c.u8(TX_CMD).u8(0).u16(0);
    c.u16(frame.len() as u16)
        .u16(flags)
        .u32(offload)
        .zeros(8) // dram info
        .u32(rate.unwrap_or(0))
        .zeros(8)
        .bytes(&frame[..hdrlen]);
    let mut tb1_end = c.0.len();
    if pad {
        let aligned = (tb1_end - 20).next_multiple_of(4) + 20;
        c.zeros(aligned - tb1_end);
        tb1_end = aligned;
    }
    c.bytes(&frame[hdrlen..]);
    (c.done(), tb1_end)
}

/// 802.11 header length of a frame.
pub fn hdr_len(frame: &[u8]) -> usize {
    let fc = u16::from_le_bytes([frame[0], frame[1]]);
    let ty = (fc >> 2) & 3;
    let sub = (fc >> 4) & 0xF;
    let mut n = 24;
    if ty == 2 {
        if fc & 0x0300 == 0x0300 {
            n += 6;
        }
        if sub & 0x8 != 0 {
            n += 2;
        }
        if fc & 0x8000 != 0 && sub & 0x8 != 0 {
            n += 4; // HT control
        }
    } else if ty == 1 {
        n = 10;
    }
    n
}

/// Sanity checks of command sizes against the Linux iwlwifi structures
/// (run at boot in debug builds; the kernel is not host-testable).
pub fn layout_checks() {
    use wlan::caps::{DEFAULT_EDCA, HeLink, Link, Mode, Width};
    let link = Link {
        mode: Mode::He,
        width: Width::W80,
        primary: 36,
        center: 42,
        nss: 2,
        mcs: [[0xFFF, 0]; 2],
        sgi: 0,
        ldpc: true,
        stbc: true,
        ampdu_exp: 7,
        ampdu_density: 5,
        max_mpdu: 3895,
        smps: 3,
        ht_protection: 0,
        erp_protection: false,
        edca: DEFAULT_EDCA,
        qos: true,
        he: None,
    };
    let he = HeLink {
        bss_color: 1,
        color_disabled: false,
        rts_threshold: 1023,
        default_pe: 0,
        pkt_ext: None,
        mu_edca: None,
        mac: [0; 6],
    };
    // iwl_phy_context_cmd: 32 bytes.
    assert_eq!(phy_context(1, Band::B5G, 36, 2, 4, 3).len(), 32);
    assert_eq!(nvm_channel(51), Some((Band::B6G, 1)));
    assert_eq!(nvm_channel(109), Some((Band::B6G, 233)));
    assert_eq!(nvm_channel(110), None);
    // iwl_mvm_add_sta_cmd (ADD_STA_CMD_API_S_VER_12): 48 bytes.
    assert_eq!(
        add_sta(false, [0; 6], 0, sta_flags(&link, 2), 0xFFFF, false).len(),
        48
    );
    // iwl_rx_baid_cfg_cmd: action + 12-byte union.
    assert_eq!(rx_baid_alloc(3, 100, 64).len(), 16);
    assert_eq!(rx_baid_remove(1, 5, 3).len(), 16);
    assert_eq!(rx_baid_remove(2, 5, 3).len(), 16);
    // iwl_device_power_cmd 4, iwl_mac_power_cmd 40, beacon filter 44/60.
    assert_eq!(device_power(true).len(), 4);
    assert_eq!(mac_power(true, 100, 3).len(), 40);
    assert_eq!(beacon_filter(3, true).len(), 44);
    assert_eq!(beacon_filter(4, true).len(), 60);
    let ba = add_sta_rx_ba(true, 3, 100, 64);
    assert_eq!((ba[17], ba[28], ba[30], ba[44]), (1 << 3, 3, 100, 64));
    // iwl_tlc_config_cmd_v4: 12 header + 12 rates + 4 = 28 bytes.
    assert_eq!(tlc_config(&link, true, 3).len(), 28);
    // iwl_he_sta_context_cmd v2 / v3: 88 / 92 bytes.
    assert_eq!(he_sta_context(2, &he).len(), 88);
    assert_eq!(he_sta_context(3, &he).len(), 92);
    // iwl_mac_ctx_cmd: 60 common + 5 * 8 EDCA + 48 union = 148 bytes.
    let m = mac_context(&MacParams {
        action: 1,
        own: [0; 6],
        bssid: [0; 6],
        assoc: None,
        qos: true,
        band_5g: true,
        short_slot: true,
        short_preamble: false,
        ht: Some(0),
        wide: true,
        erp_protection: false,
        he: true,
        edca: &DEFAULT_EDCA,
    });
    assert_eq!(m.len(), 148);
    let _ = describe_rate(0, true);
}
