#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(rustos::test_runner)]
#![reexport_test_harness_main = "test_main"]

use bootloader_api::{BootInfo, entry_point};
use core::panic::PanicInfo;
extern crate rustos;

entry_point!(main, config = &rustos::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    rustos::kernel_init(boot_info);
    rustos::vfs::init();
    test_main();
    loop {}
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    rustos::test_panic_handler(info)
}

/// Test that VFS can be initialized
#[test_case]
fn test_vfs_initialization() {
    let vfs = rustos::vfs::VFS.lock();
    assert!(vfs.is_some(), "VFS should be initialized");
}

/// Test reading a file from RamFS
#[test_case]
fn test_ramfs_read_file() {
    let mut vfs = rustos::vfs::VFS.lock();
    let vfs = vfs.as_mut().expect("VFS not initialized");

    let test_content = b"RamFS test content";
    let _ = vfs.write_file("/test.txt", test_content);

    let read_result = vfs.read_file("/test.txt");
    assert!(read_result.is_ok(), "Should read file from RamFS");
}

/// Test writing a file to RamFS
#[test_case]
fn test_ramfs_write_file() {
    let mut vfs = rustos::vfs::VFS.lock();
    let vfs = vfs.as_mut().expect("VFS not initialized");

    let test_path = "/write_test.txt";
    let test_data = b"Write test data";

    let result = vfs.write_file(test_path, test_data);
    assert!(result.is_ok(), "Should successfully write file");
}

/// Test file existence check
#[test_case]
fn test_file_exists() {
    let mut vfs = rustos::vfs::VFS.lock();
    let vfs = vfs.as_mut().expect("VFS not initialized");

    let test_path = "/exists_test.txt";

    let _ = vfs.write_file(test_path, b"content");
    let exists = vfs.exists(test_path);
    assert!(exists, "File should exist after creation");
}

/// Test directory type checking
#[test_case]
fn test_is_directory() {
    let mut vfs = rustos::vfs::VFS.lock();
    let vfs = vfs.as_mut().expect("VFS not initialized");

    let dir_path = "/is_dir_test";
    let file_path = "/is_file_test.txt";

    let _ = vfs.mkdir(dir_path);
    let _ = vfs.write_file(file_path, b"content");

    assert!(vfs.is_dir(dir_path), "Should recognize directory");
    assert!(
        !vfs.is_dir(file_path),
        "Should recognize file is not directory"
    );
}

/// Test file removal
#[test_case]
fn test_remove_file() {
    let mut vfs = rustos::vfs::VFS.lock();
    let vfs = vfs.as_mut().expect("VFS not initialized");

    let test_path = "/remove_test.txt";

    let _ = vfs.write_file(test_path, b"to remove");
    assert!(vfs.exists(test_path), "File should exist after creation");

    let result = vfs.remove(test_path);
    assert!(result.is_ok(), "Should remove file");
    assert!(
        !vfs.exists(test_path),
        "File should not exist after removal"
    );
}

/// Test error handling for invalid paths
#[test_case]
fn test_invalid_path_handling() {
    let mut vfs = rustos::vfs::VFS.lock();
    let vfs = vfs.as_mut().expect("VFS not initialized");

    let result = vfs.read_file("/nonexistent.txt");
    assert!(result.is_err(), "Should error on non-existent file");
}
