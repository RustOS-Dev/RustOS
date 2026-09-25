//! Disk filesystems and the mount dispatcher.

use crate::errno::*;
use alloc::sync::Arc;

/// Mount `source` at `target` as filesystem type `fstype` ("auto" probes).
pub fn mount_by_type(fstype: &str, source: &str, target: &str) -> KResult<()> {
    match fstype {
        "tmpfs" | "ramfs" => crate::vfs::mount(target, crate::vfs::tmpfs::TmpFs::new(), "tmpfs"),
        "proc" => crate::vfs::mount(target, crate::vfs::procfs::ProcFs::new(), "proc"),
        "devtmpfs" | "devfs" => {
            crate::vfs::mount(target, crate::vfs::devfs::DevFs::new(), "devtmpfs")
        }
        _ => {
            let fs = probe_block_fs(fstype, source)?;
            crate::vfs::mount(target, fs, source)
        }
    }
}

fn probe_block_fs(_fstype: &str, _source: &str) -> KResult<Arc<dyn crate::vfs::FileSystem>> {
    Err(ENODEV)
}
