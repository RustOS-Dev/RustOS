//! inline_data: small files and directories stored in the inode itself.
//!
//! The first 60 bytes live in `i_block`, the rest in the `system.data`
//! extended attribute in the inode body. Inline files and directories
//! are read in place; the first change converts them to ordinary
//! block-mapped (extent) files, which is always valid ext4.

use super::*;

const XATTR_MAGIC: u32 = 0xEA02_0000;
const INDEX_SYSTEM: u8 = 7;
const INLINE_SIZE: usize = 60;

/// One in-inode extended attribute: (name index, name, value, raw entry
/// header fields hash / value inode kept for round-tripping).
pub(super) struct IbodyAttr {
    pub index: u8,
    pub name: Vec<u8>,
    pub value: Vec<u8>,
    pub value_inum: u32,
    pub value_size: u32,
    pub hash: u32,
}

/// Start of the in-inode xattr area (`128 + i_extra_isize`), if any.
fn ibody_start(raw: &[u8]) -> Option<usize> {
    if raw.len() <= 128 + 4 {
        return None;
    }
    let s = 128 + u16le(raw, 128) as usize;
    (s + 4 <= raw.len()).then_some(s)
}

pub(super) fn parse_ibody(raw: &[u8]) -> Vec<IbodyAttr> {
    let mut out = Vec::new();
    let Some(s) = ibody_start(raw) else {
        return out;
    };
    if u32le(raw, s) != XATTR_MAGIC {
        return out;
    }
    let first = s + 4;
    let mut o = first;
    while o + 16 <= raw.len() && u32le(raw, o) != 0 {
        let nl = raw[o] as usize;
        let index = raw[o + 1];
        let voff = u16le(raw, o + 2) as usize;
        let value_inum = u32le(raw, o + 4);
        let vsize = u32le(raw, o + 8) as usize;
        let hash = u32le(raw, o + 12);
        if o + 16 + nl > raw.len() {
            break;
        }
        let name = raw[o + 16..o + 16 + nl].to_vec();
        let value = if value_inum == 0 {
            match raw.get(first + voff..first + voff + vsize) {
                Some(v) => v.to_vec(),
                None => break,
            }
        } else {
            Vec::new()
        };
        out.push(IbodyAttr {
            index,
            name,
            value,
            value_inum,
            value_size: vsize as u32,
            hash,
        });
        o += (16 + nl).next_multiple_of(4);
    }
    out
}

/// Rewrite the in-inode xattr area with `attrs` (entries from the start,
/// values packed at the end). Fails if they do not fit.
fn write_ibody(raw: &mut [u8], attrs: &[IbodyAttr]) -> KResult<()> {
    let Some(s) = ibody_start(raw) else {
        return if attrs.is_empty() {
            Ok(())
        } else {
            Err(ENOSPC)
        };
    };
    let first = s + 4;
    let end = raw.len();
    raw[s..end].fill(0);
    if attrs.is_empty() {
        return Ok(());
    }
    put32(raw, s, XATTR_MAGIC);
    let mut o = first;
    let mut vend = end;
    for a in attrs {
        let elen = (16 + a.name.len()).next_multiple_of(4);
        let vlen = a.value.len().next_multiple_of(4);
        if o + elen + 4 > vend - vlen {
            return Err(ENOSPC);
        }
        vend -= vlen;
        raw[o] = a.name.len() as u8;
        raw[o + 1] = a.index;
        put16(
            raw,
            o + 2,
            if a.value_inum == 0 {
                (vend - first) as u16
            } else {
                0
            },
        );
        put32(raw, o + 4, a.value_inum);
        put32(
            raw,
            o + 8,
            if a.value_inum == 0 {
                a.value.len() as u32
            } else {
                0
            },
        );
        put32(raw, o + 12, a.hash);
        raw[o + 16..o + 16 + a.name.len()].copy_from_slice(&a.name);
        if a.value_inum == 0 {
            raw[vend..vend + a.value.len()].copy_from_slice(&a.value);
        }
        o += elen;
    }
    Ok(())
}

fn is_data(a: &IbodyAttr) -> bool {
    a.index == INDEX_SYSTEM && a.name == b"data"
}

/// All inline bytes: `i_block` followed by the `system.data` value.
pub(super) fn inline_bytes(st: &RawInode) -> Vec<u8> {
    let mut v = st.block_bytes().to_vec();
    if let Some(a) = parse_ibody(&st.raw).into_iter().find(is_data) {
        v.extend_from_slice(&a.value);
    }
    v
}

/// Entries of an inline directory: "." and ".." first, then the records
/// in `i_block[4..60]` and in the `system.data` value. Logical block
/// `u64::MAX` marks them as inline (callers convert before writing).
pub(super) fn inline_dir_entries(ino: u32, st: &RawInode) -> Vec<RawDirEntry> {
    let data = inline_bytes(st);
    let mut out = alloc::vec![
        (String::from("."), ino, 2, u64::MAX, 0),
        (String::from(".."), u32le(&data, 0), 2, u64::MAX, 0),
    ];
    let mut parse = |area: &[u8], base: usize| {
        let mut o = 0;
        while o + 8 <= area.len() {
            let eino = u32le(area, o);
            let rec = u16le(area, o + 4) as usize;
            let nl = area[o + 6] as usize;
            if rec < 8 || o + rec > area.len() {
                break;
            }
            if eino != 0 && o + 8 + nl <= area.len() {
                let name = String::from_utf8_lossy(&area[o + 8..o + 8 + nl]).into_owned();
                out.push((name, eino, area[o + 7], u64::MAX, base + o));
            }
            o += rec;
        }
    };
    parse(&data[4..INLINE_SIZE.min(data.len())], 4);
    if data.len() > INLINE_SIZE {
        parse(&data[INLINE_SIZE..], INLINE_SIZE);
    }
    out
}

impl Ext2Inode {
    /// Convert an inline file or directory to a block-mapped one.
    pub(super) fn uninline(&self, st: &mut RawInode) -> KResult<()> {
        if st.flags & FL_INLINE == 0 {
            return Ok(());
        }
        if self.fs.read_only {
            return Err(EROFS);
        }
        let is_dir = st.kind() == FileType::Directory;
        let data = inline_bytes(st);
        let entries = if is_dir {
            inline_dir_entries(self.ino, st)
        } else {
            Vec::new()
        };
        // Drop system.data and switch to an (empty) extent tree.
        let attrs: Vec<IbodyAttr> = parse_ibody(&st.raw)
            .into_iter()
            .filter(|a| !is_data(a))
            .collect();
        write_ibody(&mut st.raw, &attrs)?;
        st.flags &= !FL_INLINE;
        if self.fs.extents {
            st.flags |= FL_EXTENTS;
            st.set_block_bytes(&extent::empty_root());
        } else {
            st.block = [0; 15];
        }
        st.blocks512 = 0;
        if is_dir {
            let bs = self.fs.block_size as usize;
            let end = self.dir_end();
            let mut buf = vec![0u8; bs];
            leaf_empty(&mut buf, end);
            let ft = |t: u8| if self.fs.filetype { t } else { 0 };
            for (name, ino, t, ..) in &entries {
                if !leaf_insert(&mut buf, end, name, *ino, ft(*t)) {
                    return Err(ENOSPC);
                }
            }
            let pb = self.bmap(st, 0, true)?.ok_or(EIO)?;
            st.size = bs as u64;
            self.write_dir_block(st, 0, pb, &mut buf)?;
            self.save(st)
        } else {
            let size = st.size as usize;
            self.save(st)?;
            let n = size.min(data.len());
            if n > 0 {
                self.write_data(st, 0, &data[..n])?;
            }
            st.size = size as u64;
            self.save(st)
        }
    }
}
