//! Vendor and device IDs for devices RustOS has drivers for.

pub const VENDOR_INTEL: u16 = 0x8086;
pub const VENDOR_REALTEK: u16 = 0x10EC;
pub const VENDOR_REDHAT: u16 = 0x1AF4; // virtio
pub const VENDOR_QEMU: u16 = 0x1B36;
pub const VENDOR_AMD: u16 = 0x1022;

pub fn vendor_name(v: u16) -> &'static str {
    match v {
        VENDOR_INTEL => "Intel",
        VENDOR_REALTEK => "Realtek",
        VENDOR_REDHAT => "Red Hat (virtio)",
        VENDOR_QEMU => "QEMU",
        VENDOR_AMD => "AMD",
        0x1002 => "AMD/ATI",
        0x10DE => "NVIDIA",
        0x144D => "Samsung",
        0x15B7 => "SanDisk/WD",
        0x1987 => "Phison",
        0x1C5C => "SK hynix",
        0x14E4 => "Broadcom",
        0x168C => "Qualcomm Atheros",
        0x17CB => "Qualcomm",
        0x1234 => "Bochs/QEMU",
        _ => "Unknown vendor",
    }
}
