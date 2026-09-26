//! Wireless LAN drivers.

pub mod iwlwifi;

use crate::pci::PciDevice;

/// Probe `dev` with every WLAN driver. Returns true if one claimed it.
pub fn probe(dev: &PciDevice) -> bool {
    if iwlwifi::matches(dev) {
        iwlwifi::probe(dev);
        return true;
    }
    false
}
