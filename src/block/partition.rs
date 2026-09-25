//! GPT and MBR partition tables.

use super::BlockDevice;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Debug, Clone)]
pub struct Partition {
    /// 1-based partition number.
    pub index: u32,
    pub start_lba: u64,
    pub sector_count: u64,
    pub type_name: String,
    pub label: String,
    pub is_esp: bool,
}

const GPT_ESP: [u8; 16] = guid_bytes("C12A7328-F81F-11D2-BA4B-00A0C93EC93B");
const GPT_BASIC_DATA: [u8; 16] = guid_bytes("EBD0A0A2-B9E5-4433-87C0-68B6B72699C7");
const GPT_LINUX: [u8; 16] = guid_bytes("0FC63DAF-8483-4772-8E79-3D69D8477DE4");
const GPT_LINUX_SWAP: [u8; 16] = guid_bytes("0657FD6D-A4AB-43C4-84E5-0933C84B4F4F");
const GPT_MS_RESERVED: [u8; 16] = guid_bytes("E3C9E316-0B5C-4DB8-817D-F92DF00215AE");
const GPT_BIOS_BOOT: [u8; 16] = guid_bytes("21686148-6449-6E6F-744E-656564454649");

/// Parse a textual GUID into its on-disk (mixed-endian) byte order.
pub const fn guid_bytes(s: &str) -> [u8; 16] {
    const fn hex(c: u8) -> u8 {
        match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            _ => 0,
        }
    }
    let b = s.as_bytes();
    let mut raw = [0u8; 16];
    let mut i = 0;
    let mut j = 0;
    while i < b.len() {
        if b[i] == b'-' {
            i += 1;
            continue;
        }
        raw[j] = hex(b[i]) << 4 | hex(b[i + 1]);
        j += 1;
        i += 2;
    }
    // First three fields are little-endian on disk.
    [
        raw[3], raw[2], raw[1], raw[0], raw[5], raw[4], raw[7], raw[6], raw[8], raw[9], raw[10],
        raw[11], raw[12], raw[13], raw[14], raw[15],
    ]
}

fn gpt_type_name(t: &[u8]) -> String {
    let n = if t == GPT_ESP {
        "EFI System"
    } else if t == GPT_BASIC_DATA {
        "Microsoft basic data"
    } else if t == GPT_LINUX {
        "Linux filesystem"
    } else if t == GPT_LINUX_SWAP {
        "Linux swap"
    } else if t == GPT_MS_RESERVED {
        "Microsoft reserved"
    } else if t == GPT_BIOS_BOOT {
        "BIOS boot"
    } else {
        "unknown"
    };
    String::from(n)
}

fn mbr_type_name(t: u8) -> &'static str {
    match t {
        0x01 => "FAT12",
        0x04 | 0x06 | 0x0E => "FAT16",
        0x0B | 0x0C => "FAT32",
        0x07 => "NTFS/exFAT",
        0x82 => "Linux swap",
        0x83 => "Linux",
        0xEF => "EFI System",
        _ => "unknown",
    }
}

fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn u64le(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

fn read(dev: &dyn BlockDevice, lba: u64, sectors: usize) -> Option<Vec<u8>> {
    let mut v = alloc::vec![0u8; sectors * dev.sector_size()];
    dev.read_sectors(lba, &mut v).ok()?;
    Some(v)
}

/// Scan `dev` for a partition table.
pub fn scan(dev: &dyn BlockDevice) -> Vec<Partition> {
    let ss = dev.sector_size();
    let Some(lba0) = read(dev, 0, 1) else {
        return Vec::new();
    };
    if lba0[510] != 0x55 || lba0[511] != 0xAA {
        return Vec::new();
    }
    // Protective MBR -> GPT.
    if (0..4).any(|i| lba0[446 + i * 16 + 4] == 0xEE)
        && let Some(v) = parse_gpt(dev, ss)
    {
        return v;
    }
    parse_mbr(dev, &lba0)
}

pub fn parse_gpt(dev: &dyn BlockDevice, ss: usize) -> Option<Vec<Partition>> {
    let hdr = read(dev, 1, 1)?;
    if &hdr[0..8] != b"EFI PART" {
        return None;
    }
    let entries_lba = u64le(&hdr, 72);
    let count = u32le(&hdr, 80) as usize;
    let esize = u32le(&hdr, 84) as usize;
    if esize < 128 || count == 0 || count > 1024 {
        return None;
    }
    let sectors = (count * esize).div_ceil(ss);
    let table = read(dev, entries_lba, sectors)?;
    let mut out = Vec::new();
    for i in 0..count {
        let e = &table[i * esize..(i + 1) * esize];
        let ty = &e[0..16];
        if ty.iter().all(|&b| b == 0) {
            continue;
        }
        let first = u64le(e, 32);
        let last = u64le(e, 40);
        if last < first || last >= dev.sector_count() {
            continue;
        }
        let name_u16: Vec<u16> = e[56..128]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&c| c != 0)
            .collect();
        out.push(Partition {
            index: i as u32 + 1,
            start_lba: first,
            sector_count: last - first + 1,
            type_name: gpt_type_name(ty),
            label: String::from_utf16_lossy(&name_u16),
            is_esp: ty == GPT_ESP,
        });
    }
    Some(out)
}

pub fn parse_mbr(dev: &dyn BlockDevice, lba0: &[u8]) -> Vec<Partition> {
    let mut out = Vec::new();
    for i in 0..4 {
        let e = &lba0[446 + i * 16..446 + (i + 1) * 16];
        let ty = e[4];
        let start = u32le(e, 8) as u64;
        let count = u32le(e, 12) as u64;
        if ty == 0 || count == 0 {
            continue;
        }
        if ty == 0x05 || ty == 0x0F || ty == 0x85 {
            // Extended partition: walk the EBR chain (logical partitions 5+).
            let mut ebr_lba = start;
            let mut idx = 5;
            for _ in 0..64 {
                let Some(ebr) = read(dev, ebr_lba, 1) else {
                    break;
                };
                if ebr[510] != 0x55 || ebr[511] != 0xAA {
                    break;
                }
                let l = &ebr[446..462];
                let lcount = u32le(l, 12) as u64;
                if l[4] != 0 && lcount > 0 {
                    out.push(Partition {
                        index: idx,
                        start_lba: ebr_lba + u32le(l, 8) as u64,
                        sector_count: lcount,
                        type_name: format!("{} (logical)", mbr_type_name(l[4])),
                        label: String::new(),
                        is_esp: false,
                    });
                    idx += 1;
                }
                let next = &ebr[462..478];
                if next[4] == 0 {
                    break;
                }
                ebr_lba = start + u32le(next, 8) as u64;
            }
            continue;
        }
        if start + count > dev.sector_count() {
            continue;
        }
        out.push(Partition {
            index: i as u32 + 1,
            start_lba: start,
            sector_count: count,
            type_name: String::from(mbr_type_name(ty)),
            label: String::new(),
            is_esp: ty == 0xEF,
        });
    }
    out
}
