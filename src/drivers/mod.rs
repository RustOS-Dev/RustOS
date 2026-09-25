pub mod console;
pub mod fbdev;
pub mod framebuffer;
pub mod mouse;
pub mod ps2;
pub mod random;
pub mod serial;

/// Physical memory behind a mappable device (for mmap).
pub fn mmap_phys(obj: &dyn crate::vfs::FileLike) -> Option<(u64, u64)> {
    obj.as_any()
        .downcast_ref::<fbdev::FbDev>()
        .map(|fb| (fb.phys, fb.len as u64))
}

/// Probe every bus for devices with drivers (storage, USB, network, ...).
pub fn probe_all() {}
