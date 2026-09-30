//! ext4 fast commits: the compact logical records ext4 writes to the
//! fast-commit area at the end of the journal between full commits. After
//! the regular journal replay, the records of the transaction that
//! follows the last full commit are applied, up to the last tail whose
//! transaction id and checksum match (linux/fs/ext4/fast_commit.c).

use crate::csum::crc32c;
use alloc::vec::Vec;

pub const TAG_ADD_RANGE: u16 = 1;
pub const TAG_DEL_RANGE: u16 = 2;
pub const TAG_CREAT: u16 = 3;
pub const TAG_LINK: u16 = 4;
pub const TAG_UNLINK: u16 = 5;
pub const TAG_INODE: u16 = 6;
pub const TAG_PAD: u16 = 7;
pub const TAG_TAIL: u16 = 8;
pub const TAG_HEAD: u16 = 9;

/// One logical change to replay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tag {
    /// Map `len` blocks from logical `lblk` to physical `pblk` (an
    /// uninitialized extent when `uninit`).
    AddRange {
        ino: u32,
        lblk: u32,
        len: u32,
        pblk: u64,
        uninit: bool,
    },
    /// Unmap `len` blocks from logical `lblk`.
    DelRange { ino: u32, lblk: u32, len: u32 },
    Create {
        parent: u32,
        ino: u32,
        name: Vec<u8>,
    },
    Link {
        parent: u32,
        ino: u32,
        name: Vec<u8>,
    },
    Unlink {
        parent: u32,
        ino: u32,
        name: Vec<u8>,
    },
    /// The whole on-disk inode.
    Inode { ino: u32, raw: Vec<u8> },
}

fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn value_ok(tag: u16, len: usize, block: usize) -> bool {
    match tag {
        TAG_ADD_RANGE => len == 16,
        TAG_DEL_RANGE => len == 12,
        TAG_CREAT | TAG_LINK | TAG_UNLINK => len > 8,
        TAG_INODE => len > 4 && len <= block,
        TAG_PAD => true,
        TAG_TAIL => len >= 8,
        TAG_HEAD => len == 8,
        _ => false,
    }
}

fn decode(tag: u16, v: &[u8]) -> Option<Tag> {
    let dentry = || (le32(v, 0), le32(v, 4), v[8..].to_vec());
    Some(match tag {
        TAG_ADD_RANGE => {
            // struct ext4_extent: ee_block, ee_len, ee_start_hi, ee_start_lo
            let e = &v[4..16];
            let raw_len = le16(e, 4) as u32;
            let (len, uninit) = if raw_len > 32768 {
                (raw_len - 32768, true)
            } else {
                (raw_len, false)
            };
            Tag::AddRange {
                ino: le32(v, 0),
                lblk: le32(e, 0),
                len,
                pblk: (le16(e, 6) as u64) << 32 | le32(e, 8) as u64,
                uninit,
            }
        }
        TAG_DEL_RANGE => Tag::DelRange {
            ino: le32(v, 0),
            lblk: le32(v, 4),
            len: le32(v, 8),
        },
        TAG_CREAT => {
            let (parent, ino, name) = dentry();
            Tag::Create { parent, ino, name }
        }
        TAG_LINK => {
            let (parent, ino, name) = dentry();
            Tag::Link { parent, ino, name }
        }
        TAG_UNLINK => {
            let (parent, ino, name) = dentry();
            Tag::Unlink { parent, ino, name }
        }
        TAG_INODE => Tag::Inode {
            ino: le32(v, 0),
            raw: v[4..].to_vec(),
        },
        _ => return None,
    })
}

/// The replayable records in fast-commit blocks `blocks` (in order) for
/// transaction `tid`: everything up to the last valid tail. Blocks after
/// the first one that does not continue the area are ignored.
pub fn scan(blocks: &[Vec<u8>], tid: u32) -> Vec<Tag> {
    let mut valid: Vec<Tag> = Vec::new();
    let mut pending: Vec<Tag> = Vec::new();
    let mut crc = 0u32;
    'blocks: for (bi, b) in blocks.iter().enumerate() {
        if bi == 0 && (b.len() < 4 || le16(b, 0) != TAG_HEAD) {
            break;
        }
        let mut o = 0;
        while o + 4 <= b.len() {
            let tag = le16(b, o);
            let len = le16(b, o + 2) as usize;
            let v0 = o + 4;
            if len > b.len() - v0 || !value_ok(tag, len, b.len()) {
                break 'blocks;
            }
            let v = &b[v0..v0 + len];
            match tag {
                TAG_HEAD => {
                    if le32(v, 4) != tid {
                        break 'blocks;
                    }
                    crc = crc32c(crc, &b[o..v0 + len]);
                }
                TAG_TAIL => {
                    crc = crc32c(crc, &b[o..v0 + 4]);
                    if le32(v, 0) == tid && le32(v, 4) == crc {
                        valid.append(&mut pending);
                    } else {
                        break 'blocks;
                    }
                    crc = 0;
                }
                TAG_PAD => crc = crc32c(crc, &b[o..v0 + len]),
                _ => {
                    crc = crc32c(crc, &b[o..v0 + len]);
                    match decode(tag, v) {
                        Some(t) => pending.push(t),
                        None => break 'blocks,
                    }
                }
            }
            o = v0 + len;
        }
    }
    valid
}

/// Encoding helpers (for tests and tools): a tag with its value.
pub fn tlv(tag: u16, value: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(4 + value.len());
    v.extend_from_slice(&tag.to_le_bytes());
    v.extend_from_slice(&(value.len() as u16).to_le_bytes());
    v.extend_from_slice(value);
    v
}

/// A tail for transaction `tid` closing the records in `before` (the
/// bytes since the previous tail, head included).
pub fn tail(tid: u32, before: &[u8]) -> Vec<u8> {
    let mut t = tlv(TAG_TAIL, &[0u8; 8]);
    t[4..8].copy_from_slice(&tid.to_le_bytes());
    let crc = crc32c(crc32c(0, before), &t[..8]);
    t[8..12].copy_from_slice(&crc.to_le_bytes());
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(tid: u32) -> Vec<u8> {
        let mut v = [0u8; 8];
        v[4..8].copy_from_slice(&tid.to_le_bytes());
        tlv(TAG_HEAD, &v)
    }

    fn dentry(parent: u32, ino: u32, name: &[u8]) -> Vec<u8> {
        let mut v = parent.to_le_bytes().to_vec();
        v.extend_from_slice(&ino.to_le_bytes());
        v.extend_from_slice(name);
        v
    }

    #[test]
    fn scan_stops_at_last_good_tail() {
        let mut body = head(7);
        let mut ext = 12u32.to_le_bytes().to_vec();
        ext.extend_from_slice(&3u32.to_le_bytes()); // ee_block
        ext.extend_from_slice(&2u16.to_le_bytes()); // ee_len
        ext.extend_from_slice(&0u16.to_le_bytes()); // start hi
        ext.extend_from_slice(&5000u32.to_le_bytes());
        body.extend(tlv(TAG_ADD_RANGE, &ext));
        body.extend(tlv(TAG_LINK, &dentry(2, 12, b"x")));
        let t = tail(7, &body);
        body.extend(t);
        // A second, torn commit (bad checksum).
        let second = tlv(TAG_UNLINK, &dentry(2, 12, b"x"));
        let mut bad = tail(7, &second);
        bad[8] ^= 1;
        let mut blk = body.clone();
        blk.extend(second);
        blk.extend(bad);
        blk.resize(4096, 0);
        let tags = scan(&[blk.clone()], 7);
        assert_eq!(tags.len(), 2);
        assert_eq!(
            tags[0],
            Tag::AddRange {
                ino: 12,
                lblk: 3,
                len: 2,
                pblk: 5000,
                uninit: false
            }
        );
        assert!(matches!(&tags[1], Tag::Link { name, .. } if name == b"x"));
        // Another transaction id: nothing.
        assert!(scan(&[blk], 8).is_empty());
    }
}
