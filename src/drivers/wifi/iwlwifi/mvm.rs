//! Firmware command encoding for the iwlwifi "MVM" firmware API as spoken
//! by AX210 firmware (non-MLD station API).

use alloc::vec::Vec;

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
pub const TX_ANT_CONFIG: u8 = 0x98;
pub const BT_CONFIG: u8 = 0x9B;
pub const MISSED_BEACONS: u8 = 0xA2;
pub const RX_MPDU: u8 = 0xC1;
pub const MCC_UPDATE: u8 = 0xC8;
// System group.
pub const SOC_CONFIGURATION: u8 = 0x01;
pub const INIT_EXTENDED_CFG: u8 = 0x03;
// MAC config group.
pub const SESSION_PROTECTION: u8 = 0x05;
pub const SESSION_PROTECTION_NOTIF: u8 = 0xFB;
// Data path group.
pub const TLC_MNG_CONFIG: u8 = 0x0F;
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

pub fn band_of(channel: u8) -> u8 {
    if channel <= 14 {
        PHY_BAND_24
    } else {
        PHY_BAND_5
    }
}

/// PHY_CONTEXT_CMD v3/v4 (UHB channel info).
pub fn phy_context(action: u32, channel: u8, rx_ant: u32) -> Vec<u8> {
    let rxchain = (rx_ant << 1) | (2 << 10) | (2 << 12);
    Cmd::new()
        .u32(id_color(PHY_ID))
        .u32(action)
        .u32(channel as u32)
        .u8(band_of(channel))
        .u8(0) // 20 MHz
        .u8(0) // control channel position
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
    pub _p: core::marker::PhantomData<&'a ()>,
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
        .u32(0x15) // OFDM ACK rates 6, 12, 24
        .u32(0) // protection
        .u32(if p.short_preamble { 1 << 5 } else { 0 })
        .u32(if p.short_slot || p.band_5g { 1 << 4 } else { 0 });
    let mut filter = FILTER_ACCEPT_GRP;
    if p.assoc.is_none() {
        filter |= FILTER_IN_BEACON;
    }
    c.u32(filter).u32(if p.qos { 1 } else { 0 });
    // EDCA parameters per firmware AC (BK, BE, VI, VO) + one spare:
    // (cw_min, cw_max, aifsn, fifo, txop in usec).
    let acs: [(u16, u16, u8, u8, u16); 5] = [
        (15, 1023, 7, 1, 0),
        (15, 1023, 3, 2, 0),
        (7, 15, 2, 3, 3008),
        (3, 7, 2, 4, 1504),
        (0, 0, 0, 0, 0),
    ];
    for (cw_min, cw_max, aifsn, fifo, txop) in acs {
        c.u16(cw_min)
            .u16(cw_max)
            .u8(aifsn)
            .u8(if fifo == 0 { 0 } else { 1 << fifo })
            .u16(txop);
    }
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

/// ADD_STA v12 for the AP station.
pub fn add_sta(modify: bool, bssid: [u8; 6], aid: u16) -> Vec<u8> {
    const STA_FLG_FAT_EN_MSK: u32 = 3 << 26;
    const STA_FLG_MIMO_EN_MSK: u32 = 3 << 28;
    const STA_FLG_RTS_MIMO_PROT: u32 = 1 << 17;
    Cmd::new()
        .u8(modify as u8)
        .u8(0) // awake ACs
        .u16(0xFFFF) // no aggregation on any TID
        .u32(id_color(MAC_ID))
        .bytes(&bssid)
        .u16(0)
        .u8(AP_STA_ID)
        .u8(0) // modify mask
        .u16(0)
        .u32(0) // SISO, 20 MHz
        .u32(STA_FLG_FAT_EN_MSK | STA_FLG_MIMO_EN_MSK | STA_FLG_RTS_MIMO_PROT)
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

/// TLC_MNG_CONFIG_CMD v4: firmware rate scaling with legacy rates.
pub fn tlc_config(band_5g: bool, chains: u8) -> Vec<u8> {
    // Legacy rate bitmap: bits 0-3 CCK, 4-11 OFDM.
    let non_ht: u16 = if band_5g { 0xFF0 } else { 0xFFF };
    let mut c = Cmd::new();
    c.u8(AP_STA_ID)
        .zeros(3)
        .u8(0) // 20 MHz
        .u8(0) // non-HT mode
        .u8(chains)
        .u8(0)
        .u16(0)
        .u16(non_ht)
        .zeros(2 * 3 * 2)
        .u16(3839) // max MPDU length
        .u16(0);
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

pub const SCAN_CHANNELS_24: &[u8] = &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13];
pub const SCAN_CHANNELS_5: &[u8] = &[
    36, 40, 44, 48, 52, 56, 60, 64, 100, 104, 108, 112, 116, 120, 124, 128, 132, 136, 140, 144,
    149, 153, 157, 161, 165,
];

/// SCAN_REQ_UMAC v15 (iwl_scan_req_umac_v17 layout) for a one-shot
/// active scan of `channels`, optionally directed at `ssid`.
pub fn scan_request(own: [u8; 6], channels: &[u8], ssid: &[u8], associated: bool) -> Vec<u8> {
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
    for i in 0..67 {
        match channels.get(i) {
            Some(&ch) => {
                // Probe with direct_scan[0] (the SSID, or wildcard).
                c.u32(1).u8(ch).u8(band_of(ch)).u8(1).u8(0);
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
    let b24_len = buf.len() - b24;
    let b5 = buf.len();
    buf.extend_from_slice(&[1, 8, 0x8C, 0x12, 0x98, 0x24, 0xB0, 0x48, 0x60, 0x6C]);
    let b5_len = buf.len() - b5;
    let common = buf.len();
    c.u16(0).u16(mac_len as u16);
    c.u16(b24 as u16).u16(b24_len as u16);
    c.u16(b5 as u16).u16(b5_len as u16);
    c.u16(common as u16).u16(0); // 6 GHz: none
    c.u16(common as u16).u16(0); // common data
    buf.resize(512, 0);
    c.bytes(&buf);
    // short_ssid_num, bssid_num, reserved
    c.u8(0).u8(0).u16(0);
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
    c.zeros(8 * 4); // short SSIDs
    c.zeros(16 * 6); // BSSIDs
    c.done()
}

/// Rate for management frames and frames sent before rate scaling is
/// configured: 1 Mb/s CCK on 2.4 GHz, 6 Mb/s OFDM on 5 GHz.
pub fn basic_rate(band_5g: bool, tx_ant: u32) -> u32 {
    let ant = if band_5g {
        1 << 14 // antenna A
    } else if tx_ant & 2 != 0 {
        2 << 14 // antenna B (not shared with Bluetooth)
    } else {
        1 << 14
    };
    if band_5g { (1 << 8) | ant } else { ant }
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
