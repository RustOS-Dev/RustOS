//! crc32c (Castagnoli) and crc16 plus the ext4 metadata checksum rules.
//! crc32c here is the raw Linux `crc32c_le` (no pre/post inversion): ext4
//! passes `!0` as the seed where it wants one.

const fn crc32c_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0x82F6_3B78
            } else {
                c >> 1
            };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

static CRC32C: [u32; 256] = crc32c_table();

pub fn crc32c(mut crc: u32, data: &[u8]) -> u32 {
    for &b in data {
        crc = CRC32C[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc
}

/// crc16 (poly 0x8005, reflected), as used for group descriptors without
/// metadata_csum (`uninit_bg`/`gdt_csum`).
pub fn crc16(mut crc: u16, data: &[u8]) -> u16 {
    for &b in data {
        crc ^= b as u16;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xA001
            } else {
                crc >> 1
            };
        }
    }
    crc
}

fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

/// Superblock byte offsets used here.
pub mod sb {
    pub const UUID: usize = 0x68;
    pub const CHECKSUM_SEED: usize = 0x270;
    pub const CHECKSUM: usize = 0x3FC;
}

/// Checksum seed of a filesystem (`s_csum_seed`).
pub fn fs_seed(superblock: &[u8], csum_seed_feature: bool) -> u32 {
    if csum_seed_feature {
        u32::from_le_bytes(
            superblock[sb::CHECKSUM_SEED..sb::CHECKSUM_SEED + 4]
                .try_into()
                .unwrap(),
        )
    } else {
        crc32c(!0, &superblock[sb::UUID..sb::UUID + 16])
    }
}

/// Superblock checksum (`s_checksum`).
pub fn superblock(superblock: &[u8]) -> u32 {
    crc32c(!0, &superblock[..sb::CHECKSUM])
}

/// Group descriptor checksum (`bg_checksum` at 0x1E) with metadata_csum.
pub fn group_desc(seed: u32, group: u32, desc: &[u8]) -> u16 {
    let mut c = crc32c(seed, &group.to_le_bytes());
    c = crc32c(c, &desc[..0x1E]);
    c = crc32c(c, &[0, 0]);
    c = crc32c(c, &desc[0x20..]);
    c as u16
}

/// Group descriptor checksum with the older `gdt_csum` feature.
pub fn group_desc_crc16(uuid: &[u8], group: u32, desc: &[u8]) -> u16 {
    let mut c = crc16(!0, uuid);
    c = crc16(c, &group.to_le_bytes());
    c = crc16(c, &desc[..0x1E]);
    if desc.len() > 0x20 {
        c = crc16(c, &desc[0x20..]);
    }
    c
}

/// Block or inode bitmap checksum (`bg_*_bitmap_csum_lo/hi`).
pub fn bitmap(seed: u32, bitmap: &[u8]) -> u32 {
    crc32c(seed, bitmap)
}

/// Per-inode seed: used for the inode itself and its extent, directory
/// and index blocks.
pub fn inode_seed(fs_seed: u32, ino: u32, generation: u32) -> u32 {
    crc32c(
        crc32c(fs_seed, &ino.to_le_bytes()),
        &generation.to_le_bytes(),
    )
}

/// Inode offsets.
pub const INODE_CSUM_LO: usize = 0x7C;
pub const INODE_EXTRA_ISIZE: usize = 0x80;
pub const INODE_CSUM_HI: usize = 0x82;
pub const INODE_GENERATION: usize = 0x64;

/// Whether the on-disk inode has room for `i_checksum_hi`.
pub fn inode_has_csum_hi(raw: &[u8]) -> bool {
    raw.len() > 128 && le16(raw, INODE_EXTRA_ISIZE) as usize >= 4
}

/// Inode checksum over the whole on-disk record (`raw`, inode_size bytes)
/// with the checksum fields taken as zero.
pub fn inode(iseed: u32, raw: &[u8]) -> u32 {
    let mut c = crc32c(iseed, &raw[..INODE_CSUM_LO]);
    c = crc32c(c, &[0, 0]);
    let hi = inode_has_csum_hi(raw);
    if raw.len() <= 128 {
        return crc32c(c, &raw[INODE_CSUM_LO + 2..]);
    }
    c = crc32c(c, &raw[INODE_CSUM_LO + 2..INODE_CSUM_HI]);
    if hi {
        c = crc32c(c, &[0, 0]);
        crc32c(c, &raw[INODE_CSUM_HI + 2..])
    } else {
        crc32c(c, &raw[INODE_CSUM_HI..])
    }
}

/// Store the inode checksum into `raw`.
pub fn set_inode(iseed: u32, raw: &mut [u8]) {
    let c = inode(iseed, raw);
    raw[INODE_CSUM_LO..INODE_CSUM_LO + 2].copy_from_slice(&(c as u16).to_le_bytes());
    if inode_has_csum_hi(raw) {
        raw[INODE_CSUM_HI..INODE_CSUM_HI + 2].copy_from_slice(&((c >> 16) as u16).to_le_bytes());
    }
}

/// Stored inode checksum (lo, and hi when present).
pub fn stored_inode(raw: &[u8]) -> u32 {
    let lo = le16(raw, INODE_CSUM_LO) as u32;
    if inode_has_csum_hi(raw) {
        lo | (le16(raw, INODE_CSUM_HI) as u32) << 16
    } else {
        lo
    }
}

/// Extent block tail: checksum of the block up to the tail (after
/// `eh_max` entries).
pub fn extent_block(iseed: u32, block: &[u8]) -> Option<(usize, u32)> {
    let max = le16(block, 4) as usize;
    let off = 12 + 12 * max;
    (off + 4 <= block.len()).then(|| (off, crc32c(iseed, &block[..off])))
}

/// Store an extent block's tail checksum.
pub fn set_extent_block(iseed: u32, block: &mut [u8]) {
    if let Some((off, c)) = extent_block(iseed, block) {
        block[off..off + 4].copy_from_slice(&c.to_le_bytes());
    }
}

/// Directory leaf block with a checksum tail (the last 12 bytes form a
/// fake entry: inode 0, rec_len 12, name_len 0, type 0xDE).
pub const DIRENT_TAIL: usize = 12;

pub fn has_dirent_tail(block: &[u8]) -> bool {
    let t = block.len() - DIRENT_TAIL;
    block[t..t + 4] == [0; 4]
        && le16(block, t + 4) == 12
        && block[t + 6] == 0
        && block[t + 7] == 0xDE
}

/// Write the tail entry and its checksum.
pub fn set_dirent_tail(iseed: u32, block: &mut [u8]) {
    let t = block.len() - DIRENT_TAIL;
    block[t..t + 4].fill(0);
    block[t + 4..t + 6].copy_from_slice(&12u16.to_le_bytes());
    block[t + 6] = 0;
    block[t + 7] = 0xDE;
    let c = crc32c(iseed, &block[..t]);
    block[t + 8..t + 12].copy_from_slice(&c.to_le_bytes());
}

pub fn dirent_tail(iseed: u32, block: &[u8]) -> u32 {
    crc32c(iseed, &block[..block.len() - DIRENT_TAIL])
}

/// htree node checksum. `count_offset` is where the (limit, count) pair
/// starts (0x20 in the root, 8 in interior nodes); the 8-byte tail
/// follows `limit` entries.
pub fn dx_node(iseed: u32, block: &[u8], count_offset: usize) -> Option<(usize, u32)> {
    let limit = le16(block, count_offset) as usize;
    let count = le16(block, count_offset + 2) as usize;
    let tail = count_offset + limit * 8;
    if tail + 8 > block.len() || count > limit {
        return None;
    }
    let mut c = crc32c(iseed, &block[..count_offset + count * 8]);
    c = crc32c(c, &block[tail..tail + 4]);
    c = crc32c(c, &[0; 4]);
    Some((tail + 4, c))
}

pub fn set_dx_node(iseed: u32, block: &mut [u8], count_offset: usize) {
    if let Some((off, c)) = dx_node(iseed, block, count_offset) {
        block[off..off + 4].copy_from_slice(&c.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_vectors() {
        // Standard CRC-32C check value (with the usual inversions).
        assert_eq!(!crc32c(!0, b"123456789"), 0xE306_9283);
        // CRC-16/ARC check value.
        assert_eq!(crc16(0, b"123456789"), 0xBB3D);
    }
}
