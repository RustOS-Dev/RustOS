//! MediaTek Bluetooth (MT7921/MT7922/MT7925 and relatives, e.g. the
//! MT7921AU combo adapter): the WMT ("wireless management task") protocol
//! that downloads the controller's patch firmware, as Linux btmtk does.
//!
//! WMT commands travel as HCI vendor command 0xFC6F. Their answers come back
//! as vendor event 0xE4. On USB, these events arrive through a vendor
//! control-IN request rather than the interrupt endpoint. The patch file
//! (`BT_RAM_CODE_MT*_hdr.bin`) holds a header, a global descriptor and a
//! section map. Each section with data is announced with its map entry,
//! then sent in 250-byte blocks.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// HCI vendor command carrying WMT commands.
pub const OP_WMT: u16 = 0xFC6F;
/// HCI event code of WMT answers.
pub const EVT_WMT: u8 = 0xE4;

pub const WMT_PATCH_DWNLD: u8 = 0x01;
pub const WMT_FUNC_CTRL: u8 = 0x06;
pub const WMT_SEMAPHORE: u8 = 0x17;

/// Chip registers read over USB (vendor request 0x63) at setup.
pub const REG_DEV_ID_OLD: u32 = 0x8000_0008;
pub const REG_DEV_ID: u32 = 0x7001_0200;
pub const REG_FW_VERSION: u32 = 0x8002_1004;
pub const REG_FW_FLAVOR: u32 = 0x7001_0020;
/// Endpoint reset option register (written with vendor request 0x02).
pub const REG_EP_RST_OPT: u32 = 0x7401_1890;
pub const EP_RST_IN_OUT_OPT: u32 = 0x0001_0001;

const HEADER_SIZE: usize = 32;
const GLOBAL_DESC_SIZE: usize = 64;
const SECTION_MAP_SIZE: usize = 64;
/// Bytes of a section map entry before the part sent to the controller.
const SEC_MAP_COMMON_SIZE: usize = 12;
/// Bytes of the entry the controller gets with the section announcement.
const SEC_MAP_NEED_SEND_SIZE: usize = 52;
/// Largest block of patch data per WMT command.
const BLOCK: usize = 250;

/// USB IDs of MediaTek controllers sold under other vendors' IDs (Linux
/// btusb's BTUSB_MEDIATEK entries); MediaTek's own 0e8d IDs are all theirs.
pub const OEM_USB_IDS: &[(u16, u16)] = &[
    (0x043e, 0x3109),
    (0x043e, 0x310c),
    (0x0489, 0xe0c8),
    (0x0489, 0xe0cd),
    (0x0489, 0xe0d8),
    (0x0489, 0xe0d9),
    (0x0489, 0xe0e0),
    (0x0489, 0xe0e2),
    (0x0489, 0xe0e4),
    (0x0489, 0xe0f1),
    (0x0489, 0xe0f2),
    (0x0489, 0xe0f5),
    (0x0489, 0xe0f6),
    (0x0489, 0xe102),
    (0x0489, 0xe111),
    (0x0489, 0xe113),
    (0x0489, 0xe118),
    (0x0489, 0xe11e),
    (0x0489, 0xe124),
    (0x0489, 0xe134),
    (0x0489, 0xe135),
    (0x0489, 0xe139),
    (0x0489, 0xe14e),
    (0x0489, 0xe14f),
    (0x0489, 0xe150),
    (0x0489, 0xe151),
    (0x0489, 0xe152),
    (0x0489, 0xe153),
    (0x0489, 0xe158),
    (0x0489, 0xe170),
    (0x04ca, 0x3801),
    (0x04ca, 0x3802),
    (0x04ca, 0x3804),
    (0x04ca, 0x38e4),
    (0x13d3, 0x3560),
    (0x13d3, 0x3563),
    (0x13d3, 0x3564),
    (0x13d3, 0x3567),
    (0x13d3, 0x3568),
    (0x13d3, 0x3576),
    (0x13d3, 0x3578),
    (0x13d3, 0x3583),
    (0x13d3, 0x3584),
    (0x13d3, 0x3585),
    (0x13d3, 0x3602),
    (0x13d3, 0x3603),
    (0x13d3, 0x3604),
    (0x13d3, 0x3605),
    (0x13d3, 0x3606),
    (0x13d3, 0x3607),
    (0x13d3, 0x3608),
    (0x13d3, 0x3610),
    (0x13d3, 0x3613),
    (0x13d3, 0x3614),
    (0x13d3, 0x3615),
    (0x13d3, 0x3620),
    (0x13d3, 0x3621),
    (0x13d3, 0x3622),
    (0x13d3, 0x3627),
    (0x13d3, 0x3628),
    (0x13d3, 0x3630),
    (0x13d3, 0x3633),
    (0x2c7c, 0x7009),
    (0x35f5, 0x7922),
];

/// Whether a USB Bluetooth controller is a MediaTek one.
pub fn is_mediatek(vendor: u16, product: u16) -> bool {
    vendor == 0x0e8d || OEM_USB_IDS.contains(&(vendor, product))
}

/// Chips handled by the 79xx patch download.
pub fn supported(dev_id: u32) -> bool {
    matches!(dev_id, 0x7961 | 0x7922 | 0x7925 | 0x7902 | 0x6639)
}

/// Patch file name for a chip (btmtk_fw_get_filename()).
pub fn firmware_name(dev_id: u32, fw_version: u32, fw_flavor: u32) -> String {
    let id = dev_id & 0xffff;
    let v = (fw_version & 0xff) + 1;
    match dev_id {
        0x6639 => format!("mediatek/mt7927/BT_RAM_CODE_MT{:04x}_2_{:x}_hdr.bin", id, v),
        0x7925 => format!(
            "mediatek/mt{0:04x}/BT_RAM_CODE_MT{0:04x}_1_{1:x}_hdr.bin",
            id, v
        ),
        0x7961 if fw_flavor != 0 => {
            format!("mediatek/BT_RAM_CODE_MT{:04x}_1a_{:x}_hdr.bin", id, v)
        }
        _ => format!("mediatek/BT_RAM_CODE_MT{:04x}_1_{:x}_hdr.bin", id, v),
    }
}

/// Parameters of an HCI 0xFC6F command for WMT operation `op`.
pub fn wmt_cmd(op: u8, flag: u8, data: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(5 + data.len());
    v.push(1); // direction: host to controller
    v.push(op);
    v.extend_from_slice(&((data.len() + 1) as u16).to_le_bytes());
    v.push(flag);
    v.extend_from_slice(data);
    v
}

/// A WMT answer: the vendor event's operation, flag and (for function
/// control) its status word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WmtEvent {
    pub op: u8,
    pub flag: u8,
    pub status: Option<u16>,
}

/// Parse an HCI event (code, length, parameters) as a WMT answer.
pub fn parse_event(evt: &[u8]) -> Option<WmtEvent> {
    if evt.len() < 7 || evt[0] != EVT_WMT {
        return None;
    }
    let plen = (evt[1] as usize).min(evt.len() - 2);
    let p = &evt[2..2 + plen];
    if p.len() < 5 {
        return None;
    }
    Some(WmtEvent {
        op: p[1],
        flag: p[4],
        status: (p.len() >= 7).then(|| u16::from_be_bytes([p[5], p[6]])),
    })
}

/// What a WMT answer says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    PatchUndone,
    PatchProgress,
    PatchDone,
    OnUndone,
    OnDone,
    OnProgress,
}

/// Interpret an answer to `op` (btmtk_usb_hci_wmt_sync()).
pub fn status(e: &WmtEvent) -> Option<Status> {
    Some(match e.op {
        WMT_SEMAPHORE if e.flag == 2 => Status::PatchUndone,
        WMT_SEMAPHORE => Status::PatchDone,
        WMT_FUNC_CTRL => match e.status {
            None if e.flag != 0 => Status::OnUndone,
            None => Status::OnDone,
            Some(0x404) => Status::OnDone,
            Some(0x420) => Status::OnProgress,
            Some(_) => Status::OnUndone,
        },
        WMT_PATCH_DWNLD => match e.flag {
            2 => Status::PatchDone,
            1 => Status::PatchProgress,
            _ => Status::PatchUndone,
        },
        _ => return None,
    })
}

/// One section of a patch: the announcement (answered with a status) and
/// the data blocks with their flags (1 first, 2 middle, 3 last).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub announce: Vec<u8>,
    pub blocks: Vec<(u8, Vec<u8>)>,
}

/// The patch's version line for the log: hardware/software version and
/// build time.
pub fn describe(fw: &[u8]) -> Option<String> {
    if fw.len() < HEADER_SIZE {
        return None;
    }
    let date: String = fw[..16]
        .iter()
        .take_while(|&&b| b != 0)
        .map(|&b| b as char)
        .collect();
    let hw = u16::from_le_bytes([fw[20], fw[21]]);
    let sw = u16::from_le_bytes([fw[22], fw[23]]);
    Some(format!("{:04x}{:04x}, built {}", hw, sw, date.trim()))
}

fn le32(b: &[u8], off: usize) -> Option<u32> {
    b.get(off..off + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// Split a patch file into its sections (btmtk_setup_firmware_79xx()).
/// Sections without data are skipped; for MT6639 only those whose
/// download mode is 1.
pub fn sections(fw: &[u8], dev_id: u32) -> Option<Vec<Section>> {
    let count = le32(fw, HEADER_SIZE + 12)? as usize;
    if count > 64 {
        return None;
    }
    let mut out = Vec::new();
    for i in 0..count {
        let map = HEADER_SIZE + GLOBAL_DESC_SIZE + SECTION_MAP_SIZE * i;
        let offset = le32(fw, map + 4)? as usize;
        // bin_info_spec: dlAddr, dlsize, seckeyidx, alignlen, sectype,
        // dlmodecrctype, ...
        let size = le32(fw, map + 16)? as usize;
        let mode = le32(fw, map + 32)?;
        if size == 0 || (dev_id == 0x6639 && mode & 0xff != 1) {
            continue;
        }
        let spec = map + SEC_MAP_COMMON_SIZE;
        let mut announce = Vec::with_capacity(SEC_MAP_NEED_SEND_SIZE + 1);
        announce.push(0); // legacy download mode
        announce.extend_from_slice(fw.get(spec..spec + SEC_MAP_NEED_SEND_SIZE)?);
        let data = fw.get(offset..offset + size)?;
        let n = data.len().div_ceil(BLOCK);
        let blocks = data
            .chunks(BLOCK)
            .enumerate()
            .map(|(j, c)| {
                let flag = if j == 0 {
                    1
                } else if j + 1 == n {
                    3
                } else {
                    2
                };
                (flag, c.to_vec())
            })
            .collect();
        out.push(Section { announce, blocks });
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use alloc::vec;

    /// A patch with two data sections (600 and 100 bytes) and an empty one.
    fn patch() -> Vec<u8> {
        let sizes = [600usize, 0, 100];
        let data_start = HEADER_SIZE + GLOBAL_DESC_SIZE + SECTION_MAP_SIZE * sizes.len();
        let mut fw = vec![0u8; data_start];
        fw[..14].copy_from_slice(b"20250101123456");
        fw[20..22].copy_from_slice(&0x8a10u16.to_le_bytes());
        fw[22..24].copy_from_slice(&0x8a10u16.to_le_bytes());
        fw[HEADER_SIZE + 12..HEADER_SIZE + 16].copy_from_slice(&3u32.to_le_bytes());
        let mut off = data_start;
        for (i, &sz) in sizes.iter().enumerate() {
            let m = HEADER_SIZE + GLOBAL_DESC_SIZE + SECTION_MAP_SIZE * i;
            fw[m + 4..m + 8].copy_from_slice(&(off as u32).to_le_bytes());
            fw[m + 8..m + 12].copy_from_slice(&(sz as u32).to_le_bytes());
            fw[m + 16..m + 20].copy_from_slice(&(sz as u32).to_le_bytes());
            fw[m + 12] = 0xA0 + i as u8; // dlAddr low byte: marks the entry
            off += sz;
        }
        for (i, &sz) in sizes.iter().enumerate() {
            fw.extend(core::iter::repeat_n(i as u8 + 1, sz));
        }
        fw
    }

    #[test]
    fn names() {
        assert_eq!(
            firmware_name(0x7961, 1, 0),
            "mediatek/BT_RAM_CODE_MT7961_1_2_hdr.bin"
        );
        assert_eq!(
            firmware_name(0x7961, 1, 1),
            "mediatek/BT_RAM_CODE_MT7961_1a_2_hdr.bin"
        );
        assert_eq!(
            firmware_name(0x7922, 0, 0),
            "mediatek/BT_RAM_CODE_MT7922_1_1_hdr.bin"
        );
        assert_eq!(
            firmware_name(0x7925, 0, 0),
            "mediatek/mt7925/BT_RAM_CODE_MT7925_1_1_hdr.bin"
        );
    }

    #[test]
    fn commands_and_events() {
        assert_eq!(wmt_cmd(WMT_FUNC_CTRL, 0, &[1]), [1, 6, 2, 0, 0, 1]);
        // e4 len | dir op dlen flag | status (be)
        let e = parse_event(&[0xE4, 7, 2, 6, 3, 0, 0, 0x04, 0x04]).unwrap();
        assert_eq!(e.status, Some(0x404));
        assert_eq!(status(&e), Some(Status::OnDone));
        let e = parse_event(&[0xE4, 5, 2, 1, 1, 0, 1]).unwrap();
        assert_eq!(status(&e), Some(Status::PatchProgress));
        let e = parse_event(&[0xE4, 5, 2, 1, 1, 0, 0]).unwrap();
        assert_eq!(status(&e), Some(Status::PatchUndone));
        let e = parse_event(&[0xE4, 5, 2, 0x17, 1, 0, 2]).unwrap();
        assert_eq!(status(&e), Some(Status::PatchUndone));
        assert!(parse_event(&[0x0E, 4, 1, 0x6F, 0xFC, 0]).is_none());
    }

    #[test]
    fn section_plan() {
        let fw = patch();
        assert_eq!(describe(&fw).unwrap(), "8a108a10, built 20250101123456");
        let s = sections(&fw, 0x7961).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].announce.len(), 53);
        assert_eq!(s[0].announce[0], 0);
        assert_eq!(s[0].announce[1], 0xA0);
        let flags: Vec<u8> = s[0].blocks.iter().map(|b| b.0).collect();
        assert_eq!(flags, [1, 2, 3]);
        let lens: Vec<usize> = s[0].blocks.iter().map(|b| b.1.len()).collect();
        assert_eq!(lens, [250, 250, 100]);
        assert!(s[0].blocks.iter().all(|b| b.1.iter().all(|&x| x == 1)));
        assert_eq!(s[1].announce[1], 0xA2);
        assert_eq!(s[1].blocks, [(1, vec![3u8; 100])]);
        // Truncated files are refused.
        assert!(sections(&fw[..fw.len() - 1], 0x7961).is_none());
    }

    /// The real patch files, when tools/fetch-firmware.sh has fetched them.
    #[test]
    fn linux_firmware_files() {
        for (name, id) in [
            ("BT_RAM_CODE_MT7961_1_2_hdr.bin", 0x7961),
            ("BT_RAM_CODE_MT7922_1_1_hdr.bin", 0x7922),
        ] {
            let path = format!(
                "{}/../../target/firmware/mediatek/{}",
                env!("CARGO_MANIFEST_DIR"),
                name
            );
            let Ok(fw) = std::fs::read(&path) else {
                continue;
            };
            let s = sections(&fw, id).expect(name);
            assert!(!s.is_empty(), "{name}");
            let total: usize = s
                .iter()
                .flat_map(|s| s.blocks.iter())
                .map(|b| b.1.len())
                .sum();
            assert!(total > 100_000 && total < fw.len(), "{name}: {total}");
        }
    }
}
