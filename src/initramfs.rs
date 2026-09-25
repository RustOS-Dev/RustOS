//! The embedded initial RAM filesystem (a cpio "newc" archive built from
//! `userland/` by build.rs), unpacked into the root tmpfs at boot.

use crate::vfs;

static ARCHIVE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/initramfs.cpio"));

fn hex(b: &[u8]) -> Option<u32> {
    u32::from_str_radix(core::str::from_utf8(b).ok()?, 16).ok()
}

/// Unpack the archive into the VFS root. Returns the number of entries.
pub fn unpack() -> usize {
    unpack_archive(ARCHIVE)
}

pub fn unpack_archive(data: &[u8]) -> usize {
    let mut off = 0usize;
    let mut count = 0;
    while off + 110 <= data.len() {
        let h = &data[off..off + 110];
        if &h[..6] != b"070701" {
            break;
        }
        let field = |i: usize| hex(&h[6 + i * 8..6 + (i + 1) * 8]).unwrap_or(0);
        let mode = field(1);
        let filesize = field(6) as usize;
        let namesize = field(11) as usize;
        let name_start = off + 110;
        let name_end = name_start + namesize.saturating_sub(1);
        if name_end > data.len() {
            break;
        }
        let name = core::str::from_utf8(&data[name_start..name_end]).unwrap_or("");
        let data_start = (name_start + namesize).next_multiple_of(4);
        let data_end = data_start + filesize;
        if data_end > data.len() {
            break;
        }
        if name == "TRAILER!!!" {
            break;
        }
        let path = alloc::format!("/{}", name.trim_start_matches("./").trim_start_matches('/'));
        let body = &data[data_start..data_end];
        match mode & 0o170000 {
            0o040000 => {
                let _ = vfs::mkdir_p(&path);
            }
            0o120000 => {
                if let Ok(target) = core::str::from_utf8(body) {
                    let _ = vfs::unlink(&path);
                    let _ = vfs::symlink(target, &path);
                }
            }
            0o100000 => {
                if let Some(parent) = path.rfind('/').map(|i| &path[..i])
                    && !parent.is_empty()
                {
                    let _ = vfs::mkdir_p(parent);
                }
                if vfs::write_all(&path, body).is_ok()
                    && let Ok(i) = vfs::lookup(&path)
                {
                    let _ = i.chmod(mode & 0o7777);
                }
            }
            _ => {}
        }
        count += 1;
        off = data_end.next_multiple_of(4);
    }
    count
}
