//! Network interface card drivers.

pub mod e1000;
pub mod igc;
pub mod r8169;
pub mod virtio_net;

use crate::pci::PciDevice;

/// Probe `dev` with every NIC driver. Returns true if one claimed it.
pub fn probe(dev: &PciDevice) -> bool {
    if dev.vendor_id == crate::pci::ids::VENDOR_REDHAT && matches!(dev.device_id, 0x1000 | 0x1041) {
        virtio_net::probe(dev);
    } else if e1000::matches(dev) {
        e1000::probe(dev);
    } else if igc::matches(dev) {
        igc::probe(dev);
    } else if r8169::matches(dev) {
        r8169::probe(dev);
    } else {
        return false;
    }
    true
}
