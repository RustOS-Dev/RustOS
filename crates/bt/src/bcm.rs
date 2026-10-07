//! Broadcom Bluetooth (BCM20702, BCM4335, BCM4350 and relatives on USB):
//! patch RAM download, as Linux btbcm does.
//!
//! The controller is reset and identified from HCI Read Local Version
//! (LMP subversion) and its USB vendor/product ids (vendor command
//! 0xFC5A). The patch (`brcm/<chip>-<vid>-<pid>.hcd`, or
//! `brcm/BCM-<vid>-<pid>.hcd`) is a list of HCI commands, replayed after
//! "download minidriver" (0xFC2E); the controller then runs the new code
//! and is reset again.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

pub const OP_DOWNLOAD_MINIDRV: u16 = 0xFC2E;
/// Read USB product: answer is status, vid, pid (little-endian).
pub const OP_READ_USB_PRODUCT: u16 = 0xFC5A;

/// Broadcom's Bluetooth SIG company identifier (Read Local Version).
pub const MANUFACTURER: u16 = 15;

/// Linux btbcm's bcm_usb_subver_table.
const USB_CHIPS: &[(u16, &str)] = &[
    (0x2105, "BCM20703A1"),
    (0x210b, "BCM43142A0"),
    (0x2112, "BCM4314A0"),
    (0x2118, "BCM20702A0"),
    (0x2126, "BCM4335A0"),
    (0x220e, "BCM20702A1"),
    (0x230f, "BCM4356A2"),
    (0x4106, "BCM4335B0"),
    (0x410e, "BCM20702B0"),
    (0x6109, "BCM4335C0"),
    (0x610c, "BCM4354"),
    (0x6607, "BCM4350C5"),
];

/// The chip name for an LMP subversion.
pub fn chip_name(lmp_subver: u16) -> Option<&'static str> {
    USB_CHIPS.iter().find(|&&(s, _)| s == lmp_subver).map(|&(_, n)| n)
}

/// Patch file names to try, in order.
pub fn firmware_names(lmp_subver: u16, vid: u16, pid: u16) -> Vec<String> {
    let postfix = format!("-{vid:04x}-{pid:04x}");
    let mut v = Vec::new();
    if let Some(n) = chip_name(lmp_subver) {
        v.push(format!("brcm/{n}{postfix}.hcd"));
    }
    v.push(format!("brcm/BCM{postfix}.hcd"));
    v
}

/// The (opcode, parameters) HCI commands of a .hcd patch; None if a
/// record is truncated.
pub fn patch_commands(hcd: &[u8]) -> Option<Vec<(u16, &[u8])>> {
    let mut out = Vec::new();
    let mut p = hcd;
    while p.len() >= 3 {
        let op = u16::from_le_bytes([p[0], p[1]]);
        let len = p[2] as usize;
        let params = p.get(3..3 + len)?;
        out.push((op, params));
        p = &p[3 + len..];
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn names() {
        assert_eq!(
            firmware_names(0x2118, 0x0a5c, 0x21e8),
            ["brcm/BCM20702A0-0a5c-21e8.hcd", "brcm/BCM-0a5c-21e8.hcd"]
        );
        assert_eq!(firmware_names(0x9999, 0x0a5c, 0x1), ["brcm/BCM-0a5c-0001.hcd"]);
    }

    #[test]
    fn records() {
        let hcd = [0x4c, 0xfc, 0x02, 0xaa, 0xbb, 0x4e, 0xfc, 0x00];
        let cmds = patch_commands(&hcd).unwrap();
        assert_eq!(cmds, [(0xfc4c, &[0xaa, 0xbb][..]), (0xfc4e, &[][..])]);
        assert!(patch_commands(&[0x4c, 0xfc, 0x05, 1]).is_none());
    }
}
