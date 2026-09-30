//! Intel Bluetooth controllers with TLV version information (AX200 and
//! newer, e.g. AX210 as USB 8087:0032): version parsing, firmware file
//! names, and turning an `.sfi` image into secure-send fragments and a
//! `.ddc` file into Write DDC commands, as Linux btintel does. The
//! controller boots into a bootloader that accepts a signed image; after
//! the download an Intel Reset starts the operational firmware.

use alloc::string::String;
use alloc::vec::Vec;

pub const OP_RESET: u16 = 0xFC01;
pub const OP_READ_VERSION: u16 = 0xFC05;
pub const OP_SECURE_SEND: u16 = 0xFC09;
pub const OP_WRITE_DDC: u16 = 0xFC8B;
/// Memory write inside the image: its address is the boot address.
const OP_MEMORY_WRITE: u16 = 0xFC0E;

pub const IMAGE_BOOTLOADER: u8 = 0x01;
pub const IMAGE_OPERATIONAL: u8 = 0x03;

/// Vendor event subtypes (first byte of a 0xFF event).
pub const EVT_BOOTUP: u8 = 0x02;
pub const EVT_SECURE_SEND_RESULT: u8 = 0x06;

/// Secure-send fragment types.
const FRAG_INIT: u8 = 0x00;
const FRAG_DATA: u8 = 0x01;
const FRAG_SIGN: u8 = 0x02;
const FRAG_PKEY: u8 = 0x03;

/// Version TLVs (Read Version with parameter 0xFF).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Version {
    pub cnvi_top: u32,
    pub cnvr_top: u32,
    pub cnvi_bt: u32,
    pub image_type: u8,
    pub build_num: u32,
    /// Secure boot engine: 0 RSA, 1 ECDSA.
    pub sbe_type: u8,
    pub otp_bdaddr: Option<crate::Addr>,
}

/// Parse the Read Version (TLV) return parameters.
pub fn parse_version(ret: &[u8]) -> Option<Version> {
    if *ret.first()? != 0 {
        return None;
    }
    let mut v = Version::default();
    let mut o = 1;
    let mut any = false;
    while o + 2 <= ret.len() {
        let (t, l) = (ret[o], ret[o + 1] as usize);
        let d = ret.get(o + 2..o + 2 + l)?;
        let u32v = || {
            let mut b = [0u8; 4];
            b[..d.len().min(4)].copy_from_slice(&d[..d.len().min(4)]);
            u32::from_le_bytes(b)
        };
        match t {
            0x10 => v.cnvi_top = u32v(),
            0x11 => v.cnvr_top = u32v(),
            0x12 => v.cnvi_bt = u32v(),
            0x1C => v.image_type = *d.first()?,
            0x1F => v.build_num = u32v(),
            0x2F => v.sbe_type = *d.first()?,
            0x30 => v.otp_bdaddr = crate::Addr::from_slice(d),
            _ => {}
        }
        any = true;
        o += 2 + l;
    }
    any.then_some(v)
}

fn pack_swab(top: u32) -> u16 {
    let t = (top & 0xFFF) as u16;
    let s = ((top & 0x0F00_0000) >> 24) as u16;
    ((t << 4) | s).swap_bytes()
}

/// "intel/ibt-XXXX-YYYY.<ext>" for this controller (ext: "sfi", "ddc").
pub fn firmware_name(v: &Version, ext: &str) -> String {
    alloc::format!(
        "intel/ibt-{:04x}-{:04x}.{}",
        pack_swab(v.cnvi_top),
        pack_swab(v.cnvr_top),
        ext
    )
}

/// The secure-send commands for an `.sfi` image, in order, plus the boot
/// address for the Intel Reset. Each entry is the parameter block of one
/// OP_SECURE_SEND command (fragment type, then at most 252 bytes).
pub fn secure_send_plan(fw: &[u8], sbe_type: u8) -> Option<(Vec<Vec<u8>>, u32)> {
    let mut out = Vec::new();
    let mut push = |ty: u8, data: &[u8]| {
        for c in data.chunks(252) {
            let mut p = alloc::vec![ty];
            p.extend_from_slice(c);
            out.push(p);
        }
    };
    // RSA: CSS header 128, public key 256, (4 reserved), signature 256;
    // commands from 644. ECDSA: header, key and signature after the RSA
    // block, commands from 964.
    let start = if sbe_type == 1 {
        let h = fw.get(644..644 + 128)?;
        push(FRAG_INIT, h);
        push(FRAG_PKEY, fw.get(644 + 128..644 + 224)?);
        push(FRAG_SIGN, fw.get(644 + 224..644 + 320)?);
        964
    } else {
        push(FRAG_INIT, fw.get(..128)?);
        push(FRAG_PKEY, fw.get(128..384)?);
        push(FRAG_SIGN, fw.get(388..644)?);
        644
    };
    // The payload is a list of HCI commands, sent in groups whose length is
    // a multiple of four.
    let mut boot = 0u32;
    let mut o = start;
    let mut frag = start;
    while o + 3 <= fw.len() {
        let opcode = u16::from_le_bytes([fw[o], fw[o + 1]]);
        let plen = fw[o + 2] as usize;
        if opcode == OP_MEMORY_WRITE && plen >= 4 {
            boot = u32::from_le_bytes(fw.get(o + 3..o + 7)?.try_into().ok()?);
        }
        o += 3 + plen;
        if o > fw.len() {
            return None;
        }
        if (o - frag) % 4 == 0 {
            push(FRAG_DATA, &fw[frag..o]);
            frag = o;
        }
    }
    (frag == fw.len()).then_some((out, boot))
}

/// Intel Reset parameters: boot the downloaded image at `boot`.
pub fn reset_params(boot: u32) -> Vec<u8> {
    let mut p = alloc::vec![0x00, 0x01, 0x00, 0x01];
    p.extend_from_slice(&boot.to_le_bytes());
    p
}

/// Write DDC commands for a `.ddc` file: records of `length, id(2),
/// data`, each sent whole (with its length byte).
pub fn ddc_commands(ddc: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut o = 0;
    while o < ddc.len() {
        let len = ddc[o] as usize + 1;
        let Some(rec) = ddc.get(o..o + len) else {
            break;
        };
        out.push(rec.to_vec());
        o += len;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_and_names() {
        let ret = [
            0, 0x10, 4, 0x10, 0x04, 0x40, 0x00, 0x11, 4, 0x10, 0x04, 0x40, 0x00, 0x1C, 1, 0x01,
            0x2F, 1, 0x00,
        ];
        let v = parse_version(&ret).unwrap();
        assert_eq!(v.image_type, IMAGE_BOOTLOADER);
        assert_eq!(firmware_name(&v, "sfi"), "intel/ibt-0041-0041.sfi");
    }

    #[test]
    fn secure_send() {
        let mut fw = alloc::vec![0xAA; 644];
        // Two commands (7 + 5 bytes: 12, a multiple of 4), then one of 8.
        fw.extend_from_slice(&[0x0E, 0xFC, 4, 0x78, 0x56, 0x34, 0x12]);
        fw.extend_from_slice(&[0x01, 0xFC, 2, 0, 0]);
        fw.extend_from_slice(&[0x02, 0xFC, 5, 1, 2, 3, 4, 5]);
        let (cmds, boot) = secure_send_plan(&fw, 0).unwrap();
        assert_eq!(boot, 0x1234_5678);
        // Header 128 (1), key 256 (2: 252 + 4), signature 256 (2), data 2.
        assert_eq!(cmds.len(), 7);
        assert_eq!(cmds[0][0], 0x00);
        assert_eq!(cmds[0].len(), 129);
        assert_eq!(cmds[5], [&[1u8][..], &fw[644..656]].concat());
        assert_eq!(cmds[6][1..], fw[656..]);
        // A truncated command is refused.
        assert!(secure_send_plan(&fw[..fw.len() - 1], 0).is_none());
        assert_eq!(
            reset_params(0x1234_5678),
            [0, 1, 0, 1, 0x78, 0x56, 0x34, 0x12]
        );
        assert_eq!(
            ddc_commands(&[3, 1, 2, 9, 2, 5, 6]),
            [alloc::vec![3, 1, 2, 9], alloc::vec![2, 5, 6]]
        );
    }
}
