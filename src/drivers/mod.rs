pub mod block;
pub mod console;
pub mod fbdev;
pub mod framebuffer;
pub mod input;
pub mod mouse;
pub mod net;
pub mod ps2;
pub mod random;
pub mod serial;
pub mod virtio;
pub mod wifi;

/// Physical memory behind a mappable device (for mmap).
pub fn mmap_phys(obj: &dyn crate::vfs::FileLike) -> Option<(u64, u64)> {
    obj.as_any()
        .downcast_ref::<fbdev::FbDev>()
        .map(|fb| (fb.phys, fb.len as u64))
}

/// Quiesce devices before reboot or power-off: flush filesystems and
/// caches, then tell controllers to shut down cleanly.
pub fn shutdown() {
    crate::mm::pagecache::sync_all();
    crate::vfs::finish_all();
    crate::block::sync_all();
    block::nvme::shutdown_all();
    crate::net::shutdown_devices();
    crate::usb::shutdown();
}

/// Probe every bus for devices with drivers (storage, USB, network, ...).
pub fn probe_all() {
    #[cfg(feature = "linuxkpi")]
    crate::linuxkpi::init();
    // Firmware command layouts (cheap; catches encoding regressions in
    // QEMU runs where no Wi-Fi card exists).
    #[cfg(debug_assertions)]
    wifi::iwlwifi::self_test();
    crate::block::init();
    for dev in crate::pci::enumerate() {
        match (dev.class, dev.subclass, dev.prog_if) {
            (0x01, 0x06, 0x01) => block::ahci::probe(&dev),
            (0x01, 0x08, 0x02) => block::nvme::probe(&dev),
            _ => {}
        }
        if dev.vendor_id == crate::pci::ids::VENDOR_REDHAT
            && matches!(dev.device_id, 0x1001 | 0x1042)
        {
            block::virtio_blk::probe(&dev);
        }
        if dev.class == 0x02 && !net::probe(&dev) {
            wifi::probe(&dev);
        }
        crate::sound::probe(&dev);
    }
    crate::usb::init();
    crate::block::automount();
}
