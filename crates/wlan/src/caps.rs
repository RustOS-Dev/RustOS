//! 802.11n/ac/ax (HT/VHT/HE) capabilities: the elements a station
//! advertises, parsing of the AP's capability and operation elements, and
//! the negotiated link parameters a driver programs into its firmware
//! (channel width and centre, streams, MCS maps, guard interval, coding,
//! A-MPDU limits, protection, EDCA).

use crate::ie;
use alloc::vec::Vec;

pub const HT_CAPS: u8 = 45;
pub const HT_OP: u8 = 61;
pub const VHT_CAPS: u8 = 191;
pub const VHT_OP: u8 = 192;
pub const ERP: u8 = 42;
/// Extension element ids (element 255).
pub const EXT_HE_CAPS: u8 = 35;
pub const EXT_HE_OP: u8 = 36;
pub const EXT_MU_EDCA: u8 = 38;

/// Operating mode, in increasing order of capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Mode {
    Legacy,
    Ht,
    Vht,
    He,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Legacy => "legacy",
            Mode::Ht => "HT",
            Mode::Vht => "VHT",
            Mode::He => "HE",
        }
    }
    /// 802.11 amendment name ("802.11ax").
    pub fn standard(self, band_2g: bool) -> &'static str {
        match self {
            Mode::Legacy if band_2g => "802.11g",
            Mode::Legacy => "802.11a",
            Mode::Ht => "802.11n",
            Mode::Vht => "802.11ac",
            Mode::He => "802.11ax",
        }
    }
}

/// Channel width in MHz.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Width {
    W20,
    W40,
    W80,
    W160,
}

impl Width {
    pub fn mhz(self) -> u32 {
        match self {
            Width::W20 => 20,
            Width::W40 => 40,
            Width::W80 => 80,
            Width::W160 => 160,
        }
    }
    /// Firmware encoding (iwlwifi PHY_VHT_CHANNEL_MODE / TLC width /
    /// rate_n_flags width): 0 = 20, 1 = 40, 2 = 80, 3 = 160.
    pub fn code(self) -> u8 {
        self as u8
    }
    pub fn from_mhz(m: u32) -> Width {
        match m {
            0..=20 => Width::W20,
            21..=40 => Width::W40,
            41..=80 => Width::W80,
            _ => Width::W160,
        }
    }
}

/// What this station supports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Profile {
    /// Spatial streams (transmit and receive chains).
    pub nss: u8,
    /// Highest mode to use.
    pub max_mode: Mode,
    /// Widest channel to use.
    pub max_width: Width,
    pub ldpc: bool,
}

impl Profile {
    /// Intel AX210/AX211: 2x2, 802.11ax, 160 MHz, LDPC.
    pub const AX210: Profile = Profile {
        nss: 2,
        max_mode: Mode::He,
        max_width: Width::W160,
        ldpc: true,
    };
    pub const LEGACY: Profile = Profile {
        nss: 1,
        max_mode: Mode::Legacy,
        max_width: Width::W20,
        ldpc: false,
    };

    fn nss_mcs_map(&self, supported: u16) -> u16 {
        // Two bits per stream; 3 = not supported.
        let mut m = 0xFFFFu16;
        for i in 0..self.nss.min(8) {
            m &= !(3 << (2 * i));
            m |= supported << (2 * i);
        }
        m
    }
}

// HT capability info bits.
pub const HT_CAP_LDPC: u16 = 1 << 0;
pub const HT_CAP_40MHZ: u16 = 1 << 1;
pub const HT_CAP_SMPS_MASK: u16 = 3 << 2;
pub const HT_CAP_SGI20: u16 = 1 << 5;
pub const HT_CAP_SGI40: u16 = 1 << 6;
pub const HT_CAP_TX_STBC: u16 = 1 << 7;
pub const HT_CAP_RX_STBC_MASK: u16 = 3 << 8;
pub const HT_CAP_MAX_AMSDU_7935: u16 = 1 << 11;
pub const HT_CAP_DSSS_CCK40: u16 = 1 << 12;

// VHT capability info bits.
pub const VHT_CAP_MAX_MPDU_MASK: u32 = 3;
pub const VHT_CAP_WIDTH_160: u32 = 1 << 2;
pub const VHT_CAP_WIDTH_160_80P80: u32 = 2 << 2;
pub const VHT_CAP_RXLDPC: u32 = 1 << 4;
pub const VHT_CAP_SGI80: u32 = 1 << 5;
pub const VHT_CAP_SGI160: u32 = 1 << 6;
pub const VHT_CAP_TX_STBC: u32 = 1 << 7;
pub const VHT_CAP_RX_STBC_MASK: u32 = 7 << 8;
pub const VHT_CAP_AMPDU_EXP_SHIFT: u32 = 23;
pub const VHT_CAP_RX_ANT_PATTERN: u32 = 1 << 28;
pub const VHT_CAP_TX_ANT_PATTERN: u32 = 1 << 29;

// HE PHY capability bits used here (byte, mask).
const HE_PHY0_40_IN_2G: u8 = 1 << 1;
const HE_PHY0_40_80_IN_5G: u8 = 1 << 2;
const HE_PHY0_160_IN_5G: u8 = 1 << 3;
pub const HE_PHY1_LDPC: u8 = 1 << 5;
const HE_PHY1_DEVICE_CLASS_A: u8 = 1 << 4;
const HE_PHY2_STBC_TX_80: u8 = 1 << 2;
pub const HE_PHY2_STBC_RX_80: u8 = 1 << 3;
pub const HE_PHY6_PPE_PRESENT: u8 = 1 << 7;
const HE_PHY9_NOMINAL_PAD_SHIFT: u8 = 6;
// HE MAC capability bits.
const HE_MAC1_TF_PAD_16US: u8 = 2 << 2;
pub const HE_MAC3_AMPDU_EXP_EXT_MASK: u8 = 3 << 3;

/// Maximum A-MSDU / MPDU length we accept. Kept at the smallest value so a
/// received MPDU always fits one 4 KiB receive buffer even when the
/// hardware does not split A-MSDUs.
const OUR_MAX_MPDU_VHT: u32 = 0; // 3895
const OUR_AMPDU_DENSITY: u8 = 5; // 4 us

/// HT Capabilities element body (26 bytes).
pub fn ht_caps(p: &Profile) -> [u8; 26] {
    let mut info =
        HT_CAP_40MHZ | (3 << 2) | HT_CAP_SGI20 | HT_CAP_SGI40 | (1 << 8) | HT_CAP_DSSS_CCK40;
    if p.ldpc {
        info |= HT_CAP_LDPC;
    }
    if p.nss > 1 {
        info |= HT_CAP_TX_STBC;
    }
    if p.max_width == Width::W20 {
        info &= !(HT_CAP_40MHZ | HT_CAP_SGI40 | HT_CAP_DSSS_CCK40);
    }
    let mut b = [0u8; 26];
    b[0..2].copy_from_slice(&info.to_le_bytes());
    b[2] = 3 | (OUR_AMPDU_DENSITY << 2); // 64 KiB A-MPDU, density
    for i in 0..p.nss.min(4) as usize {
        b[3 + i] = 0xFF; // MCS 0-7 per stream
    }
    b[3 + 12] = 1; // TX MCS set defined (= RX set)
    b
}

/// VHT Capabilities element body (12 bytes).
pub fn vht_caps(p: &Profile) -> [u8; 12] {
    let mut info = OUR_MAX_MPDU_VHT | VHT_CAP_SGI80 | (1 << 8) | (7 << VHT_CAP_AMPDU_EXP_SHIFT);
    if p.max_width >= Width::W160 {
        info |= VHT_CAP_WIDTH_160 | VHT_CAP_SGI160;
    }
    if p.ldpc {
        info |= VHT_CAP_RXLDPC;
    }
    if p.nss > 1 {
        info |= VHT_CAP_TX_STBC;
    } else {
        info |= VHT_CAP_TX_ANT_PATTERN | VHT_CAP_RX_ANT_PATTERN;
    }
    let map = p.nss_mcs_map(2); // MCS 0-9
    let mut b = [0u8; 12];
    b[0..4].copy_from_slice(&info.to_le_bytes());
    b[4..6].copy_from_slice(&map.to_le_bytes());
    b[8..10].copy_from_slice(&map.to_le_bytes());
    b
}

/// HE Capabilities extension element body (after the extension id).
pub fn he_caps(p: &Profile, band_2g: bool) -> Vec<u8> {
    let mut mac = [0u8; 6];
    mac[1] = HE_MAC1_TF_PAD_16US;
    let mut phy = [0u8; 11];
    if band_2g {
        if p.max_width >= Width::W40 {
            phy[0] |= HE_PHY0_40_IN_2G;
        }
    } else {
        if p.max_width >= Width::W40 {
            phy[0] |= HE_PHY0_40_80_IN_5G;
        }
        if p.max_width >= Width::W160 {
            phy[0] |= HE_PHY0_160_IN_5G;
        }
    }
    phy[1] = HE_PHY1_DEVICE_CLASS_A;
    if p.ldpc {
        phy[1] |= HE_PHY1_LDPC;
    }
    phy[2] = HE_PHY2_STBC_RX_80;
    if p.nss > 1 {
        phy[2] |= HE_PHY2_STBC_TX_80;
    }
    // No PPE thresholds: ask for the nominal 16 us packet padding.
    phy[9] = 2 << HE_PHY9_NOMINAL_PAD_SHIFT;
    let map = p.nss_mcs_map(2); // MCS 0-11
    let mut b = Vec::with_capacity(26);
    b.extend_from_slice(&mac);
    b.extend_from_slice(&phy);
    b.extend_from_slice(&map.to_le_bytes());
    b.extend_from_slice(&map.to_le_bytes());
    if phy[0] & HE_PHY0_160_IN_5G != 0 {
        b.extend_from_slice(&map.to_le_bytes());
        b.extend_from_slice(&map.to_le_bytes());
    }
    b
}

fn push_ext(out: &mut Vec<u8>, ext_id: u8, body: &[u8]) {
    out.push(ie::EXTENSION);
    out.push(body.len() as u8 + 1);
    out.push(ext_id);
    out.extend_from_slice(body);
}

/// Find an extension element body (without the extension id).
pub fn find_ext(ies: &[u8], ext_id: u8) -> Option<&[u8]> {
    ie::iter(ies)
        .find(|(id, b)| *id == ie::EXTENSION && b.first() == Some(&ext_id))
        .map(|(_, b)| &b[1..])
}

/// Capability elements for an association request to an AP whose beacon
/// elements are `ap_ies`: only what the AP supports, limited by `p`.
pub fn assoc_elements(p: &Profile, ap_ies: &[u8], band_2g: bool) -> Vec<u8> {
    let mut out = Vec::new();
    let ap_ht = ie::find(ap_ies, HT_CAPS).is_some();
    if p.max_mode >= Mode::Ht && ap_ht {
        ie::push(&mut out, HT_CAPS, &ht_caps(p));
    }
    if p.max_mode >= Mode::Vht && !band_2g && ap_ht && ie::find(ap_ies, VHT_CAPS).is_some() {
        ie::push(&mut out, VHT_CAPS, &vht_caps(p));
    }
    if p.max_mode >= Mode::He && ap_ht && find_ext(ap_ies, EXT_HE_CAPS).is_some() {
        push_ext(&mut out, EXT_HE_CAPS, &he_caps(p, band_2g));
    }
    out
}

/// Capability elements for probe requests on one band.
pub fn probe_elements(p: &Profile, band_2g: bool) -> Vec<u8> {
    let mut out = Vec::new();
    if p.max_mode >= Mode::Ht {
        ie::push(&mut out, HT_CAPS, &ht_caps(p));
    }
    if p.max_mode >= Mode::Vht && !band_2g {
        ie::push(&mut out, VHT_CAPS, &vht_caps(p));
    }
    if p.max_mode >= Mode::He {
        push_ext(&mut out, EXT_HE_CAPS, &he_caps(p, band_2g));
    }
    out
}

/// Parsed HT Capabilities.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HtCaps {
    pub info: u16,
    pub ampdu_params: u8,
    pub rx_mcs: [u8; 4],
}

impl HtCaps {
    pub fn parse(b: &[u8]) -> Option<HtCaps> {
        if b.len() < 26 {
            return None;
        }
        Some(HtCaps {
            info: u16::from_le_bytes([b[0], b[1]]),
            ampdu_params: b[2],
            rx_mcs: [b[3], b[4], b[5], b[6]],
        })
    }
    /// Spatial streams the peer can receive.
    pub fn nss(&self) -> u8 {
        self.rx_mcs.iter().take_while(|&&m| m != 0).count() as u8
    }
    /// SM power save: 0 static, 1 dynamic, 3 disabled.
    pub fn smps(&self) -> u8 {
        ((self.info & HT_CAP_SMPS_MASK) >> 2) as u8
    }
}

/// Parsed HT Operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HtOp {
    pub primary: u8,
    /// Secondary channel offset: 1 above, 3 below, 0 none.
    pub secondary: u8,
    pub any_width: bool,
    /// HT protection mode (0 none, 1 non-member, 2 20 MHz, 3 non-HT mixed).
    pub protection: u8,
}

impl HtOp {
    pub fn parse(b: &[u8]) -> Option<HtOp> {
        if b.len() < 22 {
            return None;
        }
        Some(HtOp {
            primary: b[0],
            secondary: b[1] & 3,
            any_width: b[1] & 4 != 0,
            protection: b[2] & 3,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VhtCaps {
    pub info: u32,
    pub rx_mcs_map: u16,
    pub tx_mcs_map: u16,
}

impl VhtCaps {
    pub fn parse(b: &[u8]) -> Option<VhtCaps> {
        if b.len() < 12 {
            return None;
        }
        Some(VhtCaps {
            info: u32::from_le_bytes(b[0..4].try_into().unwrap()),
            rx_mcs_map: u16::from_le_bytes([b[4], b[5]]),
            tx_mcs_map: u16::from_le_bytes([b[8], b[9]]),
        })
    }
}

/// VHT operation information (also carried in the HE Operation element).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VhtOp {
    /// 0: 20/40, 1: 80/160/80+80, 2: 160 (deprecated), 3: 80+80 (deprecated).
    pub width: u8,
    pub ccfs0: u8,
    pub ccfs1: u8,
}

impl VhtOp {
    pub fn parse(b: &[u8]) -> Option<VhtOp> {
        if b.len() < 3 {
            return None;
        }
        Some(VhtOp {
            width: b[0],
            ccfs0: b[1],
            ccfs1: b[2],
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeCaps {
    pub mac: [u8; 6],
    pub phy: [u8; 11],
    pub rx_mcs_80: u16,
    pub tx_mcs_80: u16,
    pub rx_mcs_160: u16,
    pub tx_mcs_160: u16,
    /// PPE thresholds field, if present.
    pub ppe: Vec<u8>,
}

impl HeCaps {
    /// Parse the body after the extension id.
    pub fn parse(b: &[u8]) -> Option<HeCaps> {
        if b.len() < 21 {
            return None;
        }
        let mac: [u8; 6] = b[0..6].try_into().unwrap();
        let phy: [u8; 11] = b[6..17].try_into().unwrap();
        let le = |o: usize| b.get(o..o + 2).map(|s| u16::from_le_bytes([s[0], s[1]]));
        let mut o = 17;
        let rx_mcs_80 = le(o)?;
        let tx_mcs_80 = le(o + 2)?;
        o += 4;
        let (mut rx_mcs_160, mut tx_mcs_160) = (0xFFFF, 0xFFFF);
        if phy[0] & HE_PHY0_160_IN_5G != 0 {
            rx_mcs_160 = le(o)?;
            tx_mcs_160 = le(o + 2)?;
            o += 4;
        }
        if phy[0] & (1 << 4) != 0 {
            o += 4; // 80+80
        }
        let ppe = if phy[6] & HE_PHY6_PPE_PRESENT != 0 {
            b.get(o..).unwrap_or(&[]).to_vec()
        } else {
            Vec::new()
        };
        Some(HeCaps {
            mac,
            phy,
            rx_mcs_80,
            tx_mcs_80,
            rx_mcs_160,
            tx_mcs_160,
            ppe,
        })
    }
    /// Nominal packet padding in microseconds (when no PPE thresholds).
    pub fn nominal_padding_us(&self) -> u8 {
        match self.phy[9] >> HE_PHY9_NOMINAL_PAD_SHIFT {
            0 => 0,
            1 => 8,
            _ => 16,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeOp {
    pub params: u32,
    pub bss_color: u8,
    pub color_disabled: bool,
    pub vht_op: Option<VhtOp>,
}

impl HeOp {
    pub fn parse(b: &[u8]) -> Option<HeOp> {
        if b.len() < 6 {
            return None;
        }
        let params = u32::from_le_bytes([b[0], b[1], b[2], 0]);
        let color = b[3];
        let vht_op = if params & (1 << 14) != 0 {
            VhtOp::parse(b.get(6..9)?)
        } else {
            None
        };
        Some(HeOp {
            params,
            bss_color: color & 0x3F,
            color_disabled: color & 0x80 != 0,
            vht_op,
        })
    }
    /// TXOP duration RTS threshold (units of 32 us; 1023 = disabled).
    pub fn rts_threshold(&self) -> u16 {
        ((self.params >> 4) & 0x3FF) as u16
    }
    pub fn default_pe(&self) -> u8 {
        (self.params & 7) as u8
    }
}

/// EDCA parameters of one access category.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edca {
    pub aifsn: u8,
    pub cw_min: u16,
    pub cw_max: u16,
    /// TXOP limit in microseconds.
    pub txop_us: u16,
}

/// Access categories, indexed by `Ac as usize`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ac {
    Bk = 0,
    Be = 1,
    Vi = 2,
    Vo = 3,
}

impl Ac {
    pub const ALL: [Ac; 4] = [Ac::Bk, Ac::Be, Ac::Vi, Ac::Vo];
    /// 802.1D user priority / TID to access category.
    pub fn from_tid(tid: u8) -> Ac {
        match tid & 7 {
            1 | 2 => Ac::Bk,
            0 | 3 => Ac::Be,
            4 | 5 => Ac::Vi,
            _ => Ac::Vo,
        }
    }
    /// The TID used for this category's traffic.
    pub fn tid(self) -> u8 {
        match self {
            Ac::Bk => 1,
            Ac::Be => 0,
            Ac::Vi => 5,
            Ac::Vo => 6,
        }
    }
}

/// Default EDCA parameters (802.11 Table 9-155, non-AP STA).
pub const DEFAULT_EDCA: [Edca; 4] = [
    Edca {
        aifsn: 7,
        cw_min: 15,
        cw_max: 1023,
        txop_us: 0,
    },
    Edca {
        aifsn: 3,
        cw_min: 15,
        cw_max: 1023,
        txop_us: 0,
    },
    Edca {
        aifsn: 2,
        cw_min: 7,
        cw_max: 15,
        txop_us: 3008,
    },
    Edca {
        aifsn: 2,
        cw_min: 3,
        cw_max: 7,
        txop_us: 1504,
    },
];

fn aci_to_ac(aci: u8) -> Ac {
    match aci & 3 {
        0 => Ac::Be,
        1 => Ac::Bk,
        2 => Ac::Vi,
        _ => Ac::Vo,
    }
}

/// Parse the WMM Parameter element (vendor 00:50:F2 type 2 subtype 1).
pub fn wmm_params(ies: &[u8]) -> Option<[Edca; 4]> {
    let b = ie::iter(ies)
        .find(|(id, b)| {
            *id == ie::VENDOR && b.len() >= 24 && b[..5] == [0x00, 0x50, 0xF2, 0x02, 0x01]
        })
        .map(|(_, b)| b)?;
    let mut out = DEFAULT_EDCA;
    for rec in b[8..24].chunks_exact(4) {
        let ac = aci_to_ac(rec[0] >> 5);
        out[ac as usize] = Edca {
            aifsn: rec[0] & 0xF,
            cw_min: (1u16 << (rec[1] & 0xF)) - 1,
            cw_max: (1u16 << (rec[1] >> 4)) - 1,
            txop_us: u16::from_le_bytes([rec[2], rec[3]]).saturating_mul(32),
        };
    }
    Some(out)
}

/// MU EDCA parameters (HE): per AC (aifsn, ecw_min, ecw_max, timer in
/// units of 8 TU), indexed by `Ac`.
pub fn mu_edca(ies: &[u8]) -> Option<[(u8, u8, u8, u8); 4]> {
    let b = find_ext(ies, EXT_MU_EDCA)?;
    if b.len() < 13 {
        return None;
    }
    let mut out = [(0, 0, 0, 0); 4];
    for rec in b[1..13].chunks_exact(3) {
        let ac = aci_to_ac(rec[0] >> 5);
        out[ac as usize] = (rec[0] & 0xF, rec[1] & 0xF, rec[1] >> 4, rec[2]);
    }
    Some(out)
}

/// Firmware packet-extension thresholds: per stream (2) and bandwidth
/// index (20, 40, 80, 160, 320): (low, high) constellation index, 7 = none.
pub type PktExt = [[[u8; 2]; 5]; 2];

pub const PKT_EXT_NONE: u8 = 7;
pub const PKT_EXT_BPSK: u8 = 0;

fn ppe_bits(ppe: &[u8], pos: usize) -> u8 {
    let byte = pos / 8;
    let bit = pos % 8;
    let lo = *ppe.get(byte).unwrap_or(&0) as u16;
    let hi = *ppe.get(byte + 1).unwrap_or(&0) as u16;
    (((hi << 8 | lo) >> bit) & 7) as u8
}

/// Packet extension the AP needs for frames we send, from its PPE
/// thresholds or its nominal packet padding.
pub fn pkt_ext(ap: &HeCaps) -> Option<PktExt> {
    let mut pe = [[[PKT_EXT_NONE; 2]; 5]; 2];
    if !ap.ppe.is_empty() {
        let nss = ((ap.ppe[0] & 7) + 1).min(2) as usize;
        let ru_mask = (ap.ppe[0] >> 3) & 0xF;
        let mut pos = 7;
        for s in pe.iter_mut().take(nss) {
            for (bw, th) in s.iter_mut().enumerate().take(4) {
                if ru_mask & (1 << bw) == 0 {
                    continue;
                }
                let ppet16 = ppe_bits(&ap.ppe, pos);
                let ppet8 = ppe_bits(&ap.ppe, pos + 3);
                pos += 6;
                *th = [ppet8, ppet16];
            }
        }
        return Some(pe);
    }
    let (low, high) = match ap.nominal_padding_us() {
        0 => return None,
        8 => (PKT_EXT_BPSK, PKT_EXT_NONE),
        _ => (PKT_EXT_NONE, PKT_EXT_BPSK),
    };
    for s in pe.iter_mut() {
        for th in s.iter_mut() {
            *th = [low, high];
        }
    }
    Some(pe)
}

/// Link parameters agreed with the AP.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub mode: Mode,
    pub width: Width,
    /// Primary (control) channel.
    pub primary: u8,
    /// Centre channel of the whole bandwidth.
    pub center: u8,
    /// Streams to use towards the AP.
    pub nss: u8,
    /// Per stream (index 0 = 1 stream): transmit MCS bitmap for <= 80 MHz
    /// and for 160 MHz (HT: MCS 0-7 of that stream).
    pub mcs: [[u16; 2]; 2],
    /// Short guard interval allowed per width (bit = Width code). HT/VHT.
    pub sgi: u8,
    pub ldpc: bool,
    pub stbc: bool,
    /// Maximum A-MPDU length exponent of the AP (firmware scale, 0 = 8K).
    pub ampdu_exp: u8,
    /// Minimum MPDU start spacing the AP needs (HT encoding).
    pub ampdu_density: u8,
    /// Maximum MPDU length the AP accepts.
    pub max_mpdu: u16,
    /// AP SM power save: 0 static, 1 dynamic, 3 disabled.
    pub smps: u8,
    pub ht_protection: u8,
    /// ERP "use protection" (11g protection on 2.4 GHz).
    pub erp_protection: bool,
    pub edca: [Edca; 4],
    pub qos: bool,
    pub he: Option<HeLink>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeLink {
    pub bss_color: u8,
    pub color_disabled: bool,
    pub rts_threshold: u16,
    /// Default PE duration (trigger-based packet extension).
    pub default_pe: u8,
    pub pkt_ext: Option<PktExt>,
    pub mu_edca: Option<[(u8, u8, u8, u8); 4]>,
    /// AP HE MAC capabilities.
    pub mac: [u8; 6],
}

/// Channel offset of the primary channel from the centre of the
/// bandwidth, in MHz (e.g. -30 = primary is the second 20 MHz channel
/// below the centre).
pub fn ctrl_offset_mhz(l: &Link) -> i32 {
    (l.primary as i32 - l.center as i32) * 5
}

/// iwlwifi control channel position: 0-3 below the centre (1..4 channels),
/// 4-7 above.
pub fn ctrl_pos(l: &Link) -> u8 {
    match ctrl_offset_mhz(l) {
        -70 => 3,
        -50 => 2,
        -30 => 1,
        10 => 4,
        30 => 5,
        50 => 6,
        70 => 7,
        _ => 0,
    }
}

fn vht_mcs_mask(max: u16) -> u16 {
    match max {
        0 => 0x0FF,
        1 => 0x1FF,
        2 => 0x3FF,
        _ => 0,
    }
}

fn he_mcs_mask(max: u16) -> u16 {
    match max {
        0 => 0x0FF,
        1 => 0x3FF,
        2 => 0xFFF,
        _ => 0,
    }
}

fn min_mcs(a: u16, b: u16) -> u16 {
    if a == 3 || b == 3 { 3 } else { a.min(b) }
}

/// Negotiate the link with an AP on `channel`. `ies` are the AP's
/// elements (association response merged with the beacon).
pub fn negotiate(p: &Profile, ies: &[u8], channel: u8) -> Link {
    let band_2g = channel <= 14;
    let ht_caps = ie::find(ies, HT_CAPS).and_then(HtCaps::parse);
    let ht_op = ie::find(ies, HT_OP).and_then(HtOp::parse);
    let vht_caps = ie::find(ies, VHT_CAPS).and_then(VhtCaps::parse);
    let he_caps = find_ext(ies, EXT_HE_CAPS).and_then(HeCaps::parse);
    let he_op = find_ext(ies, EXT_HE_OP).and_then(HeOp::parse);
    let vht_op = ie::find(ies, VHT_OP)
        .and_then(VhtOp::parse)
        .or(he_op.and_then(|h| h.vht_op));
    let wmm = wmm_params(ies);
    let erp_protection = ie::find(ies, ERP).is_some_and(|e| e.first().is_some_and(|b| b & 2 != 0));

    let mut mode = Mode::Legacy;
    if p.max_mode >= Mode::Ht && ht_caps.is_some() && ht_op.is_some() {
        mode = Mode::Ht;
        if p.max_mode >= Mode::Vht && !band_2g && vht_caps.is_some() {
            mode = Mode::Vht;
        }
        if p.max_mode >= Mode::He && he_caps.is_some() && he_op.is_some() {
            mode = Mode::He;
        }
    }
    let mut l = Link {
        mode,
        width: Width::W20,
        primary: channel,
        center: channel,
        nss: 1,
        mcs: [[0; 2]; 2],
        sgi: 0,
        ldpc: false,
        stbc: false,
        ampdu_exp: 0,
        ampdu_density: 0,
        max_mpdu: 3839,
        smps: 3,
        ht_protection: 0,
        erp_protection,
        edca: wmm.unwrap_or(DEFAULT_EDCA),
        qos: wmm.is_some() || mode >= Mode::Ht,
        he: None,
    };
    if mode == Mode::Legacy {
        l.max_mpdu = 0;
        return l;
    }
    let ht = ht_caps.unwrap();
    let op = ht_op.unwrap();

    // Width and centre.
    let ap_40 = ht.info & HT_CAP_40MHZ != 0 && op.any_width && op.secondary != 0;
    if ap_40 && p.max_width >= Width::W40 {
        let center = if op.secondary == 1 {
            channel + 2
        } else {
            channel.saturating_sub(2)
        };
        l.width = Width::W40;
        l.center = center;
    }
    if l.width == Width::W40
        && mode >= Mode::Vht
        && let Some(v) = vht_op
        && v.width >= 1
        && p.max_width >= Width::W80
    {
        let (c0, c1) = (v.ccfs0, v.ccfs1);
        let diff = c1.abs_diff(c0);
        let ap_160 = v.width == 2 || (v.width == 1 && c1 != 0 && diff == 8);
        let we_160 = p.max_width >= Width::W160
            && match mode {
                Mode::He => he_caps
                    .as_ref()
                    .is_some_and(|h| h.phy[0] & HE_PHY0_160_IN_5G != 0),
                _ => vht_caps
                    .is_some_and(|v| v.info & (VHT_CAP_WIDTH_160 | VHT_CAP_WIDTH_160_80P80) != 0),
            };
        if ap_160 && we_160 {
            l.width = Width::W160;
            l.center = if v.width == 2 { c0 } else { c1 };
        } else {
            l.width = Width::W80;
            // With 160 MHz signalled, CCFS0 is the centre of the primary
            // 80 MHz segment.
            l.center = c0;
        }
        // The primary channel must lie in the bandwidth.
        let half = (l.width.mhz() / 10) as i32 - 2;
        if (channel as i32 - l.center as i32).abs() > half {
            l.width = Width::W40;
            l.center = if op.secondary == 1 {
                channel + 2
            } else {
                channel.saturating_sub(2)
            };
        }
    }

    // Streams and rates (what we may transmit to the AP).
    let our_tx_streams = p.nss.max(1);
    l.smps = ht.smps();
    let ap_nss = match mode {
        Mode::He => he_caps.as_ref().map_or(1, |h| {
            (0..8)
                .take_while(|i| (h.rx_mcs_80 >> (2 * i)) & 3 != 3)
                .count() as u8
        }),
        Mode::Vht => vht_caps.map_or(1, |v| {
            (0..8)
                .take_while(|i| (v.rx_mcs_map >> (2 * i)) & 3 != 3)
                .count() as u8
        }),
        _ => ht.nss(),
    };
    l.nss = ap_nss.min(our_tx_streams).clamp(1, 2);
    if l.smps == 0 {
        l.nss = 1;
    }
    for s in 0..l.nss as usize {
        match mode {
            Mode::Ht => l.mcs[s][0] = ht.rx_mcs[s] as u16,
            Mode::Vht => {
                let v = vht_caps.unwrap();
                let m = min_mcs((v.rx_mcs_map >> (2 * s)) & 3, 2);
                let mut mask = vht_mcs_mask(m);
                if l.width == Width::W20 && s != 2 {
                    mask &= !(1 << 9); // VHT MCS 9 is invalid at 20 MHz (1-2 streams)
                }
                l.mcs[s][0] = mask;
                if l.width == Width::W160 {
                    l.mcs[s][1] = mask;
                }
            }
            Mode::He => {
                let h = he_caps.as_ref().unwrap();
                l.mcs[s][0] = he_mcs_mask(min_mcs((h.rx_mcs_80 >> (2 * s)) & 3, 2));
                if l.width == Width::W160 {
                    l.mcs[s][1] = he_mcs_mask(min_mcs((h.rx_mcs_160 >> (2 * s)) & 3, 2));
                }
            }
            Mode::Legacy => {}
        }
    }

    // Guard interval, coding, STBC.
    if mode != Mode::He {
        if ht.info & HT_CAP_SGI20 != 0 {
            l.sgi |= 1 << Width::W20.code();
        }
        if ht.info & HT_CAP_SGI40 != 0 {
            l.sgi |= 1 << Width::W40.code();
        }
        if let Some(v) = vht_caps.filter(|_| mode == Mode::Vht) {
            if v.info & VHT_CAP_SGI80 != 0 {
                l.sgi |= 1 << Width::W80.code();
            }
            if v.info & VHT_CAP_SGI160 != 0 {
                l.sgi |= 1 << Width::W160.code();
            }
        }
    }
    l.ldpc = p.ldpc
        && (ht.info & HT_CAP_LDPC != 0
            || vht_caps.is_some_and(|v| v.info & VHT_CAP_RXLDPC != 0)
            || (mode == Mode::He
                && he_caps
                    .as_ref()
                    .is_some_and(|h| h.phy[1] & HE_PHY1_LDPC != 0)));
    l.stbc = p.nss > 1
        && match mode {
            Mode::He => he_caps
                .as_ref()
                .is_some_and(|h| h.phy[2] & HE_PHY2_STBC_RX_80 != 0),
            Mode::Vht => vht_caps.is_some_and(|v| v.info & VHT_CAP_RX_STBC_MASK != 0),
            _ => ht.info & HT_CAP_RX_STBC_MASK != 0,
        };

    // Aggregation limits.
    l.ampdu_density = (ht.ampdu_params >> 2) & 7;
    l.ampdu_exp = ht.ampdu_params & 3;
    if !band_2g && let Some(v) = vht_caps {
        l.ampdu_exp = ((v.info >> VHT_CAP_AMPDU_EXP_SHIFT) & 7) as u8;
    }
    if mode == Mode::He
        && let Some(h) = he_caps.as_ref()
    {
        l.ampdu_exp += (h.mac[3] & HE_MAC3_AMPDU_EXP_EXT_MASK) >> 3;
    }
    l.ampdu_exp = l.ampdu_exp.min(9);
    l.max_mpdu = match vht_caps.filter(|_| !band_2g) {
        Some(v) => match v.info & VHT_CAP_MAX_MPDU_MASK {
            2 => 11454,
            1 => 7991,
            _ => 3895,
        },
        None if ht.info & HT_CAP_MAX_AMSDU_7935 != 0 => 7935,
        None => 3839,
    };
    l.ht_protection = op.protection;

    if mode == Mode::He {
        let h = he_caps.unwrap();
        let o = he_op.unwrap();
        l.he = Some(HeLink {
            bss_color: o.bss_color,
            color_disabled: o.color_disabled,
            rts_threshold: o.rts_threshold(),
            default_pe: o.default_pe(),
            pkt_ext: pkt_ext(&h),
            mu_edca: mu_edca(ies),
            mac: h.mac,
        });
    }
    l
}

/// Human-readable summary ("802.11ax 80 MHz 2x2").
pub fn describe(l: &Link, band_2g: bool) -> alloc::string::String {
    alloc::format!(
        "{} {} MHz {}x{}",
        l.mode.standard(band_2g),
        l.width.mhz(),
        l.nss,
        l.nss
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ht_elements(primary: u8, secondary: u8) -> Vec<u8> {
        let mut ies = Vec::new();
        let caps = ht_caps(&Profile::AX210);
        ie::push(&mut ies, HT_CAPS, &caps);
        let mut op = [0u8; 22];
        op[0] = primary;
        op[1] = secondary | if secondary != 0 { 4 } else { 0 };
        ie::push(&mut ies, HT_OP, &op);
        ies
    }

    fn wmm() -> Vec<u8> {
        let mut b = alloc::vec![0x00, 0x50, 0xF2, 0x02, 0x01, 0x01, 0x80, 0x00];
        // BE, BK, VI, VO records (ACI in bits 5-6).
        b.extend_from_slice(&[0x03, 0xA4, 0, 0]);
        b.extend_from_slice(&[0x27, 0xA4, 0, 0]);
        b.extend_from_slice(&[0x42, 0x43, 94, 0]);
        b.extend_from_slice(&[0x62, 0x32, 47, 0]);
        let mut e = Vec::new();
        ie::push(&mut e, ie::VENDOR, &b);
        e
    }

    #[test]
    fn element_sizes_and_bits() {
        let p = Profile::AX210;
        let ht = ht_caps(&p);
        let info = u16::from_le_bytes([ht[0], ht[1]]);
        assert!(info & HT_CAP_40MHZ != 0 && info & HT_CAP_LDPC != 0);
        assert_eq!(&ht[3..5], &[0xFF, 0xFF]);
        assert_eq!(ht[5], 0);
        let v = vht_caps(&p);
        assert_eq!(u16::from_le_bytes([v[4], v[5]]), 0xFFFA);
        assert_eq!(he_caps(&p, true).len(), 6 + 11 + 4);
        assert_eq!(he_caps(&p, false).len(), 6 + 11 + 8);
        let parsed = HeCaps::parse(&he_caps(&p, false)).unwrap();
        assert_eq!(parsed.rx_mcs_160, 0xFFFA);
        assert!(parsed.ppe.is_empty());
        assert_eq!(parsed.nominal_padding_us(), 16);
        // Legacy APs get no HT elements.
        assert!(assoc_elements(&p, &[], true).is_empty());
        let e = assoc_elements(&p, &ht_elements(6, 0), true);
        assert!(ie::find(&e, HT_CAPS).is_some());
        assert!(ie::find(&e, VHT_CAPS).is_none());
    }

    #[test]
    fn ht40_on_2g() {
        let mut ies = ht_elements(1, 1);
        ies.extend_from_slice(&wmm());
        let l = negotiate(&Profile::AX210, &ies, 1);
        assert_eq!(l.mode, Mode::Ht);
        assert_eq!(l.width, Width::W40);
        assert_eq!(l.center, 3);
        assert_eq!(ctrl_pos(&l), 0); // 1 below the centre
        assert_eq!(l.nss, 2);
        assert_eq!(l.mcs[1][0], 0xFF);
        assert!(l.qos && l.ldpc && l.stbc);
        assert_eq!(l.sgi, 0b11);
        assert_eq!(
            l.edca[Ac::Vo as usize],
            Edca {
                aifsn: 2,
                cw_min: 3,
                cw_max: 7,
                txop_us: 1504
            }
        );
        assert_eq!(l.edca[Ac::Bk as usize].aifsn, 7);
        // Secondary below.
        let l = negotiate(&Profile::AX210, &ht_elements(11, 3), 11);
        assert_eq!((l.width, l.center, ctrl_pos(&l)), (Width::W40, 9, 4));
        // A legacy profile ignores HT.
        assert_eq!(negotiate(&Profile::LEGACY, &ies, 1).mode, Mode::Legacy);
    }

    #[test]
    fn vht80_and_160() {
        let mut ies = ht_elements(44, 1);
        ie::push(&mut ies, VHT_CAPS, &vht_caps(&Profile::AX210));
        ie::push(&mut ies, VHT_OP, &[1, 42, 0, 0xFC, 0xFF]);
        let l = negotiate(&Profile::AX210, &ies, 44);
        assert_eq!((l.mode, l.width, l.center), (Mode::Vht, Width::W80, 42));
        assert_eq!(ctrl_offset_mhz(&l), 10);
        assert_eq!(ctrl_pos(&l), 4);
        assert_eq!(l.mcs[0][0], 0x3FF);
        assert_eq!(l.ampdu_exp, 7);
        assert_eq!(l.max_mpdu, 3895);
        // 160 MHz: CCFS0 = primary 80 centre, CCFS1 = 160 centre.
        let mut ies = ht_elements(36, 1);
        ie::push(&mut ies, VHT_CAPS, &vht_caps(&Profile::AX210));
        ie::push(&mut ies, VHT_OP, &[1, 42, 50, 0xFC, 0xFF]);
        let l = negotiate(&Profile::AX210, &ies, 36);
        assert_eq!((l.width, l.center, ctrl_pos(&l)), (Width::W160, 50, 3));
        assert_eq!(l.mcs[1][1], 0x3FF);
        // Limited to 80 by the profile.
        let p = Profile {
            max_width: Width::W80,
            ..Profile::AX210
        };
        let l = negotiate(&p, &ies, 36);
        assert_eq!((l.width, l.center, ctrl_pos(&l)), (Width::W80, 42, 1));
    }

    #[test]
    fn he_with_ppe_and_mu_edca() {
        let mut ies = ht_elements(149, 1);
        ie::push(&mut ies, VHT_CAPS, &vht_caps(&Profile::AX210));
        ie::push(&mut ies, VHT_OP, &[1, 155, 0, 0xFC, 0xFF]);
        let mut he = he_caps(&Profile::AX210, false);
        he[6 + 6] |= HE_PHY6_PPE_PRESENT;
        he[3] |= 1 << 3; // A-MPDU exponent extension 1
        // PPE: NSTS=1 (2 streams), RU mask 0b0011; PPET16/8 pairs.
        he.extend_from_slice(&[0x19, 0x1c, 0xc7, 0x71]);
        push_ext(&mut ies, EXT_HE_CAPS, &he);
        push_ext(&mut ies, EXT_HE_OP, &[0xF4, 0x3F, 0x00, 0x21, 0xFC, 0xFF]);
        push_ext(
            &mut ies,
            EXT_MU_EDCA,
            &[
                0, 0x08, 0xA4, 2, 0x28, 0xA4, 2, 0x48, 0x43, 2, 0x68, 0x32, 2,
            ],
        );
        let l = negotiate(&Profile::AX210, &ies, 149);
        assert_eq!(l.mode, Mode::He);
        assert_eq!(l.width, Width::W80);
        assert_eq!(l.mcs[1][0], 0xFFF);
        assert_eq!(l.sgi, 0);
        assert_eq!(l.ampdu_exp, 8);
        let h = l.he.clone().unwrap();
        assert_eq!(h.bss_color, 0x21);
        assert!(!h.color_disabled);
        assert_eq!(h.rts_threshold, 1023);
        let pe = h.pkt_ext.unwrap();
        // 0x19 = NSTS 1, RU mask 3; bits from position 7.
        assert_eq!(pe[0][0], [ppe_bits(&he_ppe(), 10), ppe_bits(&he_ppe(), 7)]);
        assert_eq!(pe[0][2], [PKT_EXT_NONE, PKT_EXT_NONE]);
        let mu = h.mu_edca.unwrap();
        assert_eq!(mu[Ac::Be as usize], (8, 4, 10, 2));
        assert_eq!(mu[Ac::Vo as usize], (8, 2, 3, 2));
        assert_eq!(describe(&l, false), "802.11ax 80 MHz 2x2");
    }

    fn he_ppe() -> Vec<u8> {
        alloc::vec![0x19, 0x1c, 0xc7, 0x71]
    }

    #[test]
    fn nominal_padding() {
        let mut h = HeCaps::parse(&he_caps(&Profile::AX210, true)).unwrap();
        let pe = pkt_ext(&h).unwrap();
        assert_eq!(pe[1][4], [PKT_EXT_NONE, PKT_EXT_BPSK]);
        h.phy[9] = 0;
        assert_eq!(pkt_ext(&h), None);
    }
}
