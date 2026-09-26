//! iwlwifi firmware image (`.ucode`) and platform NVM (`.pnvm`) parsing.
//!
//! Both files are TLV streams. The ucode carries the image loader (IML),
//! the runtime sections for the LMAC, UMAC and paging CPUs (separated by
//! marker sections), capability/API bitmaps and the command version table.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

const MAGIC: u32 = 0x0a4c_5749; // "IWL\n"
const HEADER_LEN: usize = 4 + 4 + 64 + 4 + 4 + 8;

const TLV_PHY_SKU: u32 = 23;
const TLV_SEC_RT: u32 = 19;
const TLV_ENABLED_CAPABILITIES: u32 = 30;
const TLV_CMD_VERSIONS: u32 = 48;
const TLV_IML: u32 = 52;
const TLV_HW_TYPE: u32 = 58;
const TLV_PNVM_VERSION: u32 = 62;
const TLV_PNVM_SKU: u32 = 64;

const CPU1_CPU2_SEPARATOR: u32 = 0xFFFF_CCCC;
const PAGING_SEPARATOR: u32 = 0xAAAA_BBBB;

pub struct Section {
    pub data: Vec<u8>,
}

pub struct Firmware {
    pub version: String,
    pub iml: Vec<u8>,
    pub lmac: Vec<Section>,
    pub umac: Vec<Section>,
    pub paging: Vec<Section>,
    pub phy_sku: u32,
    capa: [u32; 8],
    cmd_versions: BTreeMap<(u8, u8), (u8, u8)>,
}

fn tlvs(mut d: &[u8]) -> impl Iterator<Item = (u32, &[u8])> {
    core::iter::from_fn(move || {
        if d.len() < 8 {
            return None;
        }
        let t = u32::from_le_bytes(d[0..4].try_into().unwrap());
        let l = u32::from_le_bytes(d[4..8].try_into().unwrap()) as usize;
        if d.len() - 8 < l {
            return None;
        }
        let v = &d[8..8 + l];
        d = &d[(8 + l.next_multiple_of(4)).min(d.len())..];
        Some((t, v))
    })
}

fn le32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}

impl Firmware {
    pub fn parse(d: &[u8]) -> Result<Firmware, &'static str> {
        if d.len() < HEADER_LEN || le32(d, 0) != 0 || le32(d, 4) != MAGIC {
            return Err("not an iwlwifi TLV firmware file");
        }
        let human = &d[8..72];
        let end = human.iter().position(|&c| c == 0).unwrap_or(64);
        let mut fw = Firmware {
            version: String::from_utf8_lossy(&human[..end]).into(),
            iml: Vec::new(),
            lmac: Vec::new(),
            umac: Vec::new(),
            paging: Vec::new(),
            phy_sku: 0,
            capa: [0; 8],
            cmd_versions: BTreeMap::new(),
        };
        // 0 = LMAC, 1 = UMAC, 2 = paging.
        let mut part = 0;
        for (t, v) in tlvs(&d[HEADER_LEN..]) {
            match t {
                TLV_SEC_RT if v.len() >= 4 => {
                    let offset = le32(v, 0);
                    match offset {
                        CPU1_CPU2_SEPARATOR => part = 1,
                        PAGING_SEPARATOR => part = 2,
                        _ => {
                            let s = Section {
                                data: v[4..].to_vec(),
                            };
                            match part {
                                0 => fw.lmac.push(s),
                                1 => fw.umac.push(s),
                                _ => fw.paging.push(s),
                            }
                        }
                    }
                }
                TLV_IML => fw.iml = v.to_vec(),
                TLV_PHY_SKU if v.len() >= 4 => fw.phy_sku = le32(v, 0),
                TLV_ENABLED_CAPABILITIES if v.len() >= 8 => {
                    let idx = le32(v, 0) as usize;
                    if idx < fw.capa.len() {
                        fw.capa[idx] = le32(v, 4);
                    }
                }
                TLV_CMD_VERSIONS => {
                    for e in v.chunks_exact(4) {
                        fw.cmd_versions.insert((e[1], e[0]), (e[2], e[3]));
                    }
                }
                _ => {}
            }
        }
        if fw.iml.is_empty() || fw.lmac.is_empty() || fw.umac.is_empty() {
            return Err("firmware has no image loader or runtime sections");
        }
        if fw.lmac.len() > 64 || fw.umac.len() > 64 || fw.paging.len() > 64 {
            return Err("firmware has too many sections");
        }
        Ok(fw)
    }

    pub fn has_capa(&self, bit: usize) -> bool {
        self.capa
            .get(bit / 32)
            .is_some_and(|w| w & (1 << (bit % 32)) != 0)
    }

    /// Command version the firmware implements (group 0 and 1 are the
    /// same "legacy" namespace).
    pub fn cmd_version(&self, group: u8, cmd: u8) -> Option<u8> {
        let v = self
            .cmd_versions
            .get(&(group, cmd))
            .or_else(|| match group {
                0 => self.cmd_versions.get(&(1, cmd)),
                1 => self.cmd_versions.get(&(0, cmd)),
                _ => None,
            })?;
        (v.0 != 99 && v.0 != 0).then_some(v.0)
    }

    /// Valid TX/RX antenna masks from the PHY SKU.
    pub fn valid_tx_ant(&self) -> u32 {
        match (self.phy_sku >> 16) & 0xF {
            0 => 3,
            a => a,
        }
    }
    pub fn valid_rx_ant(&self) -> u32 {
        match (self.phy_sku >> 20) & 0xF {
            0 => 3,
            a => a,
        }
    }
}

/// Find the PNVM payload for this device: the SKU section matching the
/// ALIVE-reported `sku` whose HW_TYPE matches the MAC/RF type. The
/// payload chunks are concatenated into one DRAM block.
pub fn pnvm_payload(d: &[u8], sku: [u32; 3], mac_type: u16, rf_id: u16) -> Option<(u32, Vec<u8>)> {
    let mut it = tlvs(d).peekable();
    while let Some((t, v)) = it.next() {
        if t != TLV_PNVM_SKU || v.len() < 12 {
            continue;
        }
        let this = [le32(v, 0), le32(v, 4), le32(v, 8)];
        let mut hw_match = false;
        let mut version = 0;
        let mut payload = Vec::new();
        while let Some(&(t2, v2)) = it.peek() {
            if t2 == TLV_PNVM_SKU {
                break;
            }
            it.next();
            match t2 {
                TLV_PNVM_VERSION if v2.len() >= 4 => version = le32(v2, 0),
                TLV_HW_TYPE if v2.len() >= 4 => {
                    let mt = u16::from_le_bytes([v2[0], v2[1]]);
                    let rf = u16::from_le_bytes([v2[2], v2[3]]);
                    if mt == mac_type && rf == rf_id {
                        hw_match = true;
                    }
                }
                TLV_SEC_RT if v2.len() >= 4 && le32(v2, 0) != 0xddddeeee => {
                    payload.extend_from_slice(&v2[4..]);
                }
                _ => {}
            }
        }
        if this == sku && hw_match && !payload.is_empty() {
            return Some((version, payload));
        }
    }
    None
}
