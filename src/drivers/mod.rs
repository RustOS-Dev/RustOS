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

/// Quiesce devices before reboot or power-off: flush filesystems and
/// caches, then tell controllers to shut down cleanly.
pub fn shutdown() {
    crate::mm::pagecache::sync_all();
    crate::vfs::finish_all();
    crate::block::sync_all();
    block::nvme::shutdown_all();
    #[cfg(feature = "linuxkpi")]
    crate::linuxkpi::shutdown();
    crate::net::shutdown_devices();
    crate::usb::shutdown();
}

/// Native PCI drivers, in probe order. xHCI controllers are claimed by
/// `usb::init`.
const NATIVE_PCI_DRIVERS: &[crate::pci::Driver] = &[
    crate::pci::Driver {
        name: "ahci",
        probe: |d| {
            let m = (d.class, d.subclass, d.prog_if) == (0x01, 0x06, 0x01);
            if m {
                block::ahci::probe(d);
            }
            m
        },
    },
    crate::pci::Driver {
        name: "nvme",
        probe: |d| {
            let m = (d.class, d.subclass, d.prog_if) == (0x01, 0x08, 0x02);
            if m {
                block::nvme::probe(d);
            }
            m
        },
    },
    crate::pci::Driver {
        name: "virtio-blk",
        probe: |d| {
            let m = d.vendor_id == crate::pci::ids::VENDOR_REDHAT
                && matches!(d.device_id, 0x1001 | 0x1042);
            if m {
                block::virtio_blk::probe(d);
            }
            m
        },
    },
    crate::pci::Driver {
        name: "net",
        probe: |d| d.class == 0x02 && net::probe(d),
    },
    crate::pci::Driver {
        name: "wifi",
        probe: |d| d.class == 0x02 && wifi::probe(d),
    },
    crate::pci::Driver {
        name: "sound",
        probe: crate::sound::probe,
    },
];

/// Probe every bus for devices with drivers (storage, USB, network, ...).
pub fn probe_all() {
    #[cfg(feature = "linuxkpi")]
    crate::linuxkpi::init();
    // Firmware command layouts (cheap; catches encoding regressions in
    // QEMU runs where no Wi-Fi card exists).
    #[cfg(debug_assertions)]
    wifi::iwlwifi::self_test();
    crate::block::init();
    crate::pci::probe_drivers(NATIVE_PCI_DRIVERS);
    crate::usb::init();
    // Before the Linux drivers, which load firmware from these drives.
    crate::block::automount();
    // Linux drivers built in through LinuxKPI (module_init()).
    #[cfg(feature = "linuxkpi")]
    crate::linuxkpi::run_initcalls();
}
