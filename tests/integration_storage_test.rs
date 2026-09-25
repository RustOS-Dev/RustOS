#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(rustos::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader_api::{BootInfo, entry_point};
use core::panic::PanicInfo;
use rustos::errno::*;
use rustos::vfs;

entry_point!(main, config = &rustos::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    rustos::kernel_init(boot_info);
    test_main();
    loop {}
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    rustos::test_panic_handler(info)
}

#[test_case]
fn test_root_and_pseudo_filesystems_mounted() {
    let mounts = vfs::mounts();
    for m in ["/", "/dev", "/proc"] {
        assert!(mounts.iter().any(|(p, _, _)| p == m), "{} not mounted", m);
    }
    assert!(vfs::is_dir("/tmp"));
}

#[test_case]
fn test_write_read_file() {
    vfs::write_all("/tmp/a.txt", b"hello vfs").unwrap();
    assert_eq!(vfs::read_all("/tmp/a.txt").unwrap(), b"hello vfs");
    assert_eq!(vfs::stat("/tmp/a.txt").unwrap().size, 9);
}

#[test_case]
fn test_open_file_offsets_and_append() {
    let f = vfs::open(
        "/tmp/b.txt",
        vfs::O_RDWR | vfs::O_CREAT | vfs::O_TRUNC,
        0o644,
    )
    .unwrap();
    f.write(b"0123456789").unwrap();
    f.seek(2, 0).unwrap();
    let mut buf = [0u8; 3];
    assert_eq!(f.read(&mut buf).unwrap(), 3);
    assert_eq!(&buf, b"234");
    let a = vfs::open("/tmp/b.txt", vfs::O_WRONLY | vfs::O_APPEND, 0).unwrap();
    a.write(b"XY").unwrap();
    assert_eq!(vfs::read_all("/tmp/b.txt").unwrap(), b"0123456789XY");
}

#[test_case]
fn test_directories_rename_unlink() {
    vfs::mkdir_p("/tmp/d1/d2").unwrap();
    vfs::write_all("/tmp/d1/d2/f", b"x").unwrap();
    assert_eq!(vfs::rmdir("/tmp/d1"), Err(ENOTEMPTY));
    vfs::rename("/tmp/d1/d2/f", "/tmp/d1/g").unwrap();
    assert!(!vfs::exists("/tmp/d1/d2/f"));
    assert!(vfs::exists("/tmp/d1/g"));
    vfs::unlink("/tmp/d1/g").unwrap();
    vfs::rmdir("/tmp/d1/d2").unwrap();
    vfs::rmdir("/tmp/d1").unwrap();
    assert!(!vfs::exists("/tmp/d1"));
}

#[test_case]
fn test_symlinks_and_dotdot() {
    vfs::mkdir_p("/tmp/s/real").unwrap();
    vfs::write_all("/tmp/s/real/file", b"via link").unwrap();
    vfs::symlink("real", "/tmp/s/link").unwrap();
    assert_eq!(vfs::read_all("/tmp/s/link/file").unwrap(), b"via link");
    assert_eq!(
        vfs::read_all("/tmp/s/link/../real/file").unwrap(),
        b"via link"
    );
    assert_eq!(
        vfs::lookup_nofollow("/tmp/s/link")
            .unwrap()
            .readlink()
            .unwrap(),
        "real"
    );
}

#[test_case]
fn test_devices_and_proc() {
    let z = vfs::open("/dev/zero", vfs::O_RDONLY, 0).unwrap();
    let mut buf = [1u8; 8];
    z.read(&mut buf).unwrap();
    assert_eq!(buf, [0; 8]);
    let n = vfs::open("/dev/null", vfs::O_WRONLY, 0).unwrap();
    assert_eq!(n.write(b"discard").unwrap(), 7);
    let meminfo = vfs::read_all("/proc/meminfo").unwrap();
    assert!(core::str::from_utf8(&meminfo).unwrap().contains("MemTotal"));
}

#[test_case]
fn test_pipe_roundtrip() {
    let (r, w) = vfs::pipe::pipe();
    w.write(b"through a pipe", false).unwrap();
    let mut buf = [0u8; 32];
    let n = r.read(&mut buf, false).unwrap();
    assert_eq!(&buf[..n], b"through a pipe");
    assert_eq!(r.read(&mut buf, true), Err(EAGAIN));
}

#[test_case]
fn test_initramfs_unpack() {
    // A tiny cpio archive with one directory and one file.
    let mut ar = alloc::vec::Vec::new();
    let mut add = |name: &str, mode: u32, data: &[u8]| {
        let hdr = alloc::format!(
            "070701{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}",
            1,
            mode,
            0,
            0,
            1,
            0,
            data.len(),
            0,
            0,
            0,
            0,
            name.len() + 1,
            0
        );
        ar.extend_from_slice(hdr.as_bytes());
        ar.extend_from_slice(name.as_bytes());
        ar.push(0);
        while ar.len() % 4 != 0 {
            ar.push(0);
        }
        ar.extend_from_slice(data);
        while ar.len() % 4 != 0 {
            ar.push(0);
        }
    };
    add("tmp/cpio", 0o040755, b"");
    add("tmp/cpio/f", 0o100644, b"cpio data");
    add("TRAILER!!!", 0, b"");
    assert_eq!(rustos::initramfs::unpack_archive(&ar), 2);
    assert_eq!(vfs::read_all("/tmp/cpio/f").unwrap(), b"cpio data");
}
