//! Realtek Bluetooth (RTL8723/8761/8821/8822/8851/8852/8922 and their USB
//! adapters): patch firmware download, as Linux btrtl does.
//!
//! The controller is identified from HCI Read Local Version (LMP
//! subversion, HCI revision and version). Its patch file
//! (`rtl_bt/<chip>_fw.bin`) holds patches for several ROM versions: an
//! "epatch" v1 file has one patch per ROM version; a v2 file
//! ("RTBTCore") has sections of subsections, selected by ROM version and
//! security key and ordered by priority. The chosen patch, followed by the
//! chip's config file when there is one, goes to the controller with
//! vendor command 0xFC20 in 252-byte fragments.

use crate::{le16, le32};
use alloc::vec::Vec;

/// Download one fragment: index byte (bit 7 marks the last), then data.
pub const OP_DOWNLOAD: u16 = 0xFC20;
/// Read the ROM version: answer is status, version.
pub const OP_READ_ROM_VERSION: u16 = 0xFC6D;
/// Read a 16-bit chip register (vendor command parameters below).
pub const OP_READ_REG16: u16 = 0xFC61;
/// Drop the firmware a controller already runs (then identify again).
pub const OP_DROP_FW: u16 = 0xFC66;
/// Parameters of OP_READ_REG16 for the security project (key id).
pub const REG_SEC_PROJ: [u8; 5] = [0x10, 0xA4, 0xAD, 0x00, 0xB0];

/// Realtek's Bluetooth SIG company identifier (Read Local Version).
pub const MANUFACTURER: u16 = 93;

const FRAG_LEN: usize = 252;
const SIG_V1: &[u8; 8] = b"Realtech";
const SIG_V2: &[u8; 8] = b"RTBTCore";
const EXTENSION_SIG: [u8; 4] = [0x51, 0x04, 0xFD, 0x77];

const LMP_8723A: u16 = 0x1200;
const LMP_8723B: u16 = 0x8723;
const LMP_8821A: u16 = 0x8821;
const LMP_8761A: u16 = 0x8761;
const LMP_8703B: u16 = 0x8703;
const LMP_8822B: u16 = 0x8822;
const LMP_8852A: u16 = 0x8852;
const LMP_8851B: u16 = 0x8851;
const LMP_8922A: u16 = 0x8922;

/// A USB controller Linux btrtl knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chip {
    pub lmp_subver: u16,
    pub hci_rev: u16,
    /// 0 matches any HCI version.
    pub hci_ver: u8,
    pub config_needed: bool,
    pub has_rom_version: bool,
    /// Firmware file name without ".bin".
    pub fw: &'static str,
    /// Config file name without ".bin".
    pub cfg: Option<&'static str>,
}

const fn chip(
    lmp_subver: u16,
    hci_rev: u16,
    hci_ver: u8,
    config_needed: bool,
    fw: &'static str,
    cfg: &'static str,
) -> Chip {
    Chip {
        lmp_subver,
        hci_rev,
        hci_ver,
        config_needed,
        has_rom_version: true,
        fw,
        cfg: Some(cfg),
    }
}

/// Linux btrtl's ic_id_table, USB entries.
pub const CHIPS: &[Chip] = &[
    Chip {
        lmp_subver: LMP_8723A,
        hci_rev: 0xb,
        hci_ver: 0x6,
        config_needed: false,
        has_rom_version: false,
        fw: "rtl_bt/rtl8723a_fw",
        cfg: None,
    },
    chip(LMP_8723B, 0xb, 0x6, false, "rtl_bt/rtl8723b_fw", "rtl_bt/rtl8723b_config"),
    chip(LMP_8723B, 0xd, 0x8, true, "rtl_bt/rtl8723d_fw", "rtl_bt/rtl8723d_config"),
    chip(LMP_8821A, 0xa, 0x6, false, "rtl_bt/rtl8821a_fw", "rtl_bt/rtl8821a_config"),
    chip(LMP_8821A, 0xc, 0x8, false, "rtl_bt/rtl8821c_fw", "rtl_bt/rtl8821c_config"),
    chip(LMP_8761A, 0xa, 0x6, false, "rtl_bt/rtl8761a_fw", "rtl_bt/rtl8761a_config"),
    chip(LMP_8761A, 0xb, 0xa, false, "rtl_bt/rtl8761bu_fw", "rtl_bt/rtl8761bu_config"),
    chip(LMP_8761A, 0xe, 0, false, "rtl_bt/rtl8761cu_fw", "rtl_bt/rtl8761cu_config"),
    chip(LMP_8822B, 0xc, 0xa, false, "rtl_bt/rtl8822cu_fw", "rtl_bt/rtl8822cu_config"),
    chip(LMP_8822B, 0xb, 0x7, true, "rtl_bt/rtl8822b_fw", "rtl_bt/rtl8822b_config"),
    chip(LMP_8852A, 0xa, 0xb, false, "rtl_bt/rtl8852au_fw", "rtl_bt/rtl8852au_config"),
    chip(LMP_8852A, 0xb, 0xb, false, "rtl_bt/rtl8852bu_fw", "rtl_bt/rtl8852bu_config"),
    chip(LMP_8852A, 0xc, 0xc, false, "rtl_bt/rtl8852cu_fw", "rtl_bt/rtl8852cu_config"),
    chip(LMP_8851B, 0xb, 0xc, false, "rtl_bt/rtl8851bu_fw", "rtl_bt/rtl8851bu_config"),
    chip(LMP_8922A, 0xa, 0xc, false, "rtl_bt/rtl8922au_fw", "rtl_bt/rtl8922au_config"),
    chip(LMP_8852A, 0x87, 0xc, false, "rtl_bt/rtl8852btu_fw", "rtl_bt/rtl8852btu_config"),
];

/// The chip with this identity (HCI Read Local Version fields).
pub fn identify(lmp_subver: u16, hci_rev: u16, hci_ver: u8) -> Option<&'static Chip> {
    CHIPS.iter().find(|c| {
        c.lmp_subver == lmp_subver
            && c.hci_rev == hci_rev
            && (c.hci_ver == 0 || c.hci_ver == hci_ver)
    })
}

/// Firmware file names to try, in order (RTL8852C has a newer "_v2").
pub fn firmware_names(c: &Chip) -> Vec<alloc::string::String> {
    let mut v = Vec::new();
    if c.lmp_subver == LMP_8852A && c.hci_rev == 0xc {
        v.push(alloc::format!("{}_v2.bin", c.fw));
    }
    v.push(alloc::format!("{}.bin", c.fw));
    v
}

pub fn config_name(c: &Chip) -> Option<alloc::string::String> {
    c.cfg.map(|n| alloc::format!("{n}.bin"))
}

/// Project ids in the firmware's extension section, for the chip family
/// (LMP subversion) the file is for.
const PROJECTS: &[(u8, u16)] = &[
    (0, LMP_8723A),
    (1, LMP_8723B),
    (2, LMP_8821A),
    (3, LMP_8761A),
    (7, LMP_8703B),
    (8, LMP_8822B),
    (9, LMP_8723B),
    (10, LMP_8821A),
    (13, LMP_8822B),
    (14, LMP_8761A),
    (18, LMP_8852A),
    (20, LMP_8852A),
    (25, LMP_8852A),
    (36, LMP_8851B),
    (44, LMP_8922A),
    (47, LMP_8852A),
    (51, LMP_8761A),
];

/// Why a patch file was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Not an epatch file, or truncated.
    Format,
    /// The file is for another chip family.
    WrongChip,
    /// No patch for this ROM version (and key).
    NoPatch,
}

/// The project id from the extension section at the end of the file.
fn project_id(fw: &[u8], header_len: usize) -> Option<u8> {
    let mut p = fw.len().checked_sub(EXTENSION_SIG.len())?;
    if fw[p..] != EXTENSION_SIG {
        return None;
    }
    // Instructions run backwards from the signature: opcode, length, data.
    while p >= header_len + 3 {
        let (op, len, data) = (fw[p - 1], fw[p - 2], fw[p - 3]);
        p -= 3;
        if op == 0xff {
            break;
        }
        if len == 0 {
            return None;
        }
        if op == 0 && len == 1 {
            return Some(data);
        }
        p = p.checked_sub(len as usize)?;
    }
    None
}

/// The patch to download for a controller of family `lmp_subver` with
/// `rom_version` and security `key_id`, from patch file `fw` (without the
/// config file).
pub fn select_patch(fw: &[u8], lmp_subver: u16, rom_version: u8, key_id: u8) -> Result<Vec<u8>, Error> {
    let v1 = fw.starts_with(SIG_V1);
    let v2 = fw.starts_with(SIG_V2);
    if !v1 && !v2 {
        return Err(Error::Format);
    }
    let header_len = if v1 { 14 } else { 20 };
    if fw.len() < header_len + EXTENSION_SIG.len() + 3 {
        return Err(Error::Format);
    }
    let id = project_id(fw, header_len).ok_or(Error::Format)?;
    let family = PROJECTS
        .iter()
        .find(|&&(p, _)| p == id)
        .map(|&(_, f)| f)
        .ok_or(Error::WrongChip)?;
    if family != lmp_subver {
        return Err(Error::WrongChip);
    }
    if v2 {
        return select_v2(fw, rom_version, key_id);
    }
    let fw_version = &fw[8..12];
    let n = le16(fw, 12).ok_or(Error::Format)? as usize;
    if fw.len() < header_len + EXTENSION_SIG.len() + 3 + 8 * n {
        return Err(Error::Format);
    }
    let ids = header_len;
    let lens = ids + 2 * n;
    let offs = lens + 2 * n;
    let i = (0..n)
        .find(|&i| le16(fw, ids + 2 * i) == Some(rom_version as u16 + 1))
        .ok_or(Error::NoPatch)?;
    let len = le16(fw, lens + 2 * i).ok_or(Error::Format)? as usize;
    let off = le32(fw, offs + 4 * i).ok_or(Error::Format)? as usize;
    if off == 0 || len < 4 || off > fw.len() || len > fw.len() - off {
        return Err(Error::Format);
    }
    // The patch's last four bytes are replaced with the file's version.
    let mut out = Vec::from(&fw[off..off + len - 4]);
    out.extend_from_slice(fw_version);
    Ok(out)
}

const OP_SNIPPETS: u32 = 1;
const OP_DUMMY_HEADER: u32 = 2;
const OP_SECURITY_HEADER: u32 = 3;

fn select_v2(fw: &[u8], rom_version: u8, key_id: u8) -> Result<Vec<u8>, Error> {
    // The last seven bytes (extension instruction and signature) are cut.
    let body = &fw[..fw.len() - 7];
    let sections = le32(body, 16).ok_or(Error::Format)?;
    let mut p = 20;
    // (priority, data) of matching subsections, ordered by priority.
    let mut picked: Vec<(u8, &[u8])> = Vec::new();
    for _ in 0..sections {
        let (Some(op), Some(len)) = (le32(body, p), le32(body, p + 4)) else {
            break;
        };
        let len = len as usize;
        let Some(sec) = body.get(p + 8..p + 8 + len) else {
            break;
        };
        p += 8 + len;
        match op {
            OP_SNIPPETS | OP_DUMMY_HEADER => {}
            OP_SECURITY_HEADER if key_id != 0 => {}
            _ => continue,
        }
        let num = le16(sec, 0).ok_or(Error::Format)?;
        let mut q = 4;
        for _ in 0..num {
            let Some(hdr) = sec.get(q..q + 8) else {
                break;
            };
            let sub_len = le32(hdr, 4).unwrap_or(0) as usize;
            let Some(data) = sec.get(q + 8..q + 8 + sub_len) else {
                break;
            };
            q += 8 + sub_len;
            if hdr[0] != rom_version.wrapping_add(1) {
                continue;
            }
            if op == OP_SECURITY_HEADER && hdr[2] != key_id {
                continue;
            }
            // Insert before the first entry of equal or higher priority.
            let at = picked
                .iter()
                .position(|&(prio, _)| prio >= hdr[1])
                .unwrap_or(picked.len());
            picked.insert(at, (hdr[1], data));
        }
    }
    if picked.is_empty() {
        return Err(Error::NoPatch);
    }
    Ok(picked.into_iter().flat_map(|(_, d)| d.iter().copied()).collect())
}

/// The OP_DOWNLOAD parameters for `image` (patch plus config), in order.
pub fn download_commands(image: &[u8]) -> Vec<Vec<u8>> {
    let frags = image.len() / FRAG_LEN + 1;
    let mut out = Vec::with_capacity(frags);
    let mut j: u8 = 0;
    for i in 0..frags {
        let mut index = j;
        j = if j == 0x7f { 1 } else { j + 1 };
        let start = i * FRAG_LEN;
        let end = if i == frags - 1 {
            index |= 0x80;
            start + image.len() % FRAG_LEN
        } else {
            start + FRAG_LEN
        };
        let mut cmd = Vec::with_capacity(1 + end - start);
        cmd.push(index);
        cmd.extend_from_slice(&image[start..end]);
        out.push(cmd);
    }
    out
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use alloc::vec;

    /// Extension section naming project `id`: instruction (data, len, op)
    /// read backwards, then the signature.
    fn extension(id: u8) -> Vec<u8> {
        let mut v = vec![id, 1, 0];
        v.extend_from_slice(&EXTENSION_SIG);
        v
    }

    #[test]
    fn identifies_chips() {
        assert_eq!(identify(0x8822, 0xc, 0xa).unwrap().fw, "rtl_bt/rtl8822cu_fw");
        assert_eq!(identify(0x8852, 0xc, 0xc).unwrap().fw, "rtl_bt/rtl8852cu_fw");
        // hci_ver 0 in the table matches any version.
        assert_eq!(identify(0x8761, 0xe, 0x3).unwrap().fw, "rtl_bt/rtl8761cu_fw");
        assert!(identify(0x1234, 1, 1).is_none());
        let c = identify(0x8852, 0xc, 0xc).unwrap();
        assert_eq!(firmware_names(c), ["rtl_bt/rtl8852cu_fw_v2.bin", "rtl_bt/rtl8852cu_fw.bin"]);
        assert_eq!(config_name(c).unwrap(), "rtl_bt/rtl8852cu_config.bin");
    }

    #[test]
    fn v1_patch_for_rom_version() {
        // Two patches, for chip ids 1 and 2 (ROM versions 0 and 1).
        let mut fw = Vec::from(&SIG_V1[..]);
        fw.extend_from_slice(&0x1122_3344u32.to_le_bytes());
        fw.extend_from_slice(&2u16.to_le_bytes());
        let base = 14 + 16;
        for id in [1u16, 2] {
            fw.extend_from_slice(&id.to_le_bytes());
        }
        for len in [8u16, 6] {
            fw.extend_from_slice(&len.to_le_bytes());
        }
        for off in [base as u32, base as u32 + 8] {
            fw.extend_from_slice(&off.to_le_bytes());
        }
        fw.extend_from_slice(&[0xA1, 0xA2, 0xA3, 0xA4, 0, 0, 0, 0]);
        fw.extend_from_slice(&[0xB1, 0xB2, 0, 0, 0, 0]);
        fw.extend(extension(13)); // 8822C
        assert_eq!(
            select_patch(&fw, 0x8822, 1, 0).unwrap(),
            [0xB1, 0xB2, 0x44, 0x33, 0x22, 0x11]
        );
        assert_eq!(
            select_patch(&fw, 0x8822, 0, 0).unwrap(),
            [0xA1, 0xA2, 0xA3, 0xA4, 0x44, 0x33, 0x22, 0x11]
        );
        assert_eq!(select_patch(&fw, 0x8822, 5, 0), Err(Error::NoPatch));
        assert_eq!(select_patch(&fw, 0x8852, 0, 0), Err(Error::WrongChip));
        assert_eq!(select_patch(b"nonsense-file....", 0x8822, 0, 0), Err(Error::Format));
    }

    #[test]
    fn v2_subsections_by_priority_and_key() {
        let sub = |eco: u8, prio: u8, key: u8, data: &[u8]| {
            let mut v = vec![eco, prio, key, 0];
            v.extend_from_slice(&(data.len() as u32).to_le_bytes());
            v.extend_from_slice(data);
            v
        };
        let section = |op: u32, subs: &[Vec<u8>]| {
            let mut body = (subs.len() as u16).to_le_bytes().to_vec();
            body.extend_from_slice(&[0, 0]);
            for s in subs {
                body.extend_from_slice(s);
            }
            let mut v = op.to_le_bytes().to_vec();
            v.extend_from_slice(&(body.len() as u32).to_le_bytes());
            v.extend(body);
            v
        };
        let mut fw = Vec::from(&SIG_V2[..]);
        fw.extend_from_slice(&[0; 8]);
        fw.extend_from_slice(&3u32.to_le_bytes());
        // ROM version 1 (eco 2): priorities 5 and 1 in one section, a
        // subsection for another ROM, and a security header for key 7.
        fw.extend(section(OP_SNIPPETS, &[sub(2, 5, 0, b"late"), sub(2, 1, 0, b"early"), sub(3, 0, 0, b"other")]));
        fw.extend(section(OP_SECURITY_HEADER, &[sub(2, 3, 7, b"key7"), sub(2, 2, 8, b"key8")]));
        fw.extend(section(9, &[sub(2, 0, 0, b"unknown")]));
        fw.extend(extension(25)); // 8852C
        assert_eq!(select_patch(&fw, 0x8852, 1, 7).unwrap(), b"earlykey7late");
        // Key id 0: security headers are skipped.
        assert_eq!(select_patch(&fw, 0x8852, 1, 0).unwrap(), b"earlylate");
        assert_eq!(select_patch(&fw, 0x8852, 4, 0), Err(Error::NoPatch));
    }

    #[test]
    fn fragments_and_indices() {
        let image: Vec<u8> = (0..600u32).map(|i| i as u8).collect();
        let cmds = download_commands(&image);
        assert_eq!(cmds.len(), 3);
        assert_eq!((cmds[0][0], cmds[0].len()), (0, 253));
        assert_eq!((cmds[1][0], cmds[1].len()), (1, 253));
        assert_eq!((cmds[2][0], cmds[2].len()), (0x82, 1 + 600 - 504));
        assert_eq!(cmds[2][1], (504 % 256) as u8);
        // Indices wrap from 0x7f to 1.
        let big = vec![0u8; FRAG_LEN * 130];
        let cmds = download_commands(&big);
        assert_eq!(cmds[127][0], 0x7f);
        assert_eq!(cmds[128][0], 1);
        // An exact multiple ends with an empty last fragment, as in Linux.
        assert_eq!(cmds.last().unwrap(), &vec![0x80 | 3]);
    }
}
