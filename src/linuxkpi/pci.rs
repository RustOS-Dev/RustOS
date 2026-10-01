//! PCI and interrupt services for the LinuxKPI C glue (src/linuxkpi/c/pci.c):
//! the RustOS PCI device list, config space, decoding, and interrupt
//! routing (INTx through ACPI `_PRT`, or MSI) to a C handler.

use crate::pci::{self, Bar, PciDevice};
use crate::sync::Mutex;
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ffi::{c_int, c_void};

/// Snapshot of the PCI bus taken when LinuxKPI starts; Linux `pci_dev`s
/// refer to devices by index into it.
static DEVICES: Mutex<Vec<PciDevice>> = Mutex::new(Vec::new());

pub fn init() {
    *DEVICES.lock() = pci::enumerate();
}

fn with_dev<R>(idx: u32, f: impl FnOnce(&PciDevice) -> R) -> Option<R> {
    DEVICES.lock().get(idx as usize).map(f)
}

/// Layout shared with `struct kpi_pci_info` in src/linuxkpi/c/kpi.h.
#[repr(C)]
pub struct KpiPciBar {
    start: u64,
    size: u64,
    /// 1 = memory, 2 = I/O, 4 = prefetchable, 8 = 64-bit.
    flags: u32,
}

#[repr(C)]
pub struct KpiPciInfo {
    segment: u16,
    bus: u8,
    dev: u8,
    func: u8,
    revision: u8,
    irq_pin: u8,
    vendor: u16,
    device: u16,
    subvendor: u16,
    subdevice: u16,
    class: u32,
    bars: [KpiPciBar; 6],
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_pci_count() -> u32 {
    DEVICES.lock().len() as u32
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_pci_get(idx: u32, out: *mut KpiPciInfo) -> c_int {
    let Some(info) = with_dev(idx, |d| {
        let bars = core::array::from_fn(|i| match d.bars[i] {
            Bar::Mmio {
                addr,
                size,
                prefetchable,
                is64,
            } => KpiPciBar {
                start: addr,
                size,
                flags: 1 | (prefetchable as u32) << 2 | (is64 as u32) << 3,
            },
            Bar::Io { port, size } => KpiPciBar {
                start: port as u64,
                size: size as u64,
                flags: 2,
            },
            Bar::None => KpiPciBar {
                start: 0,
                size: 0,
                flags: 0,
            },
        });
        KpiPciInfo {
            segment: d.segment,
            bus: d.bus,
            dev: d.dev,
            func: d.func,
            revision: d.revision,
            irq_pin: d.irq_pin,
            vendor: d.vendor_id,
            device: d.device_id,
            subvendor: d.subsys_vendor,
            subdevice: d.subsys_id,
            class: (d.class as u32) << 16 | (d.subclass as u32) << 8 | d.prog_if as u32,
            bars,
        }
    }) else {
        return -1;
    };
    unsafe { out.write(info) };
    0
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_pci_read(idx: u32, off: u32, size: u32) -> u32 {
    with_dev(idx, |d| match size {
        1 => d.read8(off as u16) as u32,
        2 => d.read16(off as u16) as u32,
        _ => d.read32(off as u16),
    })
    .unwrap_or(!0)
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_pci_write(idx: u32, off: u32, size: u32, val: u32) {
    with_dev(idx, |d| match size {
        1 => d.write8(off as u16, val as u8),
        2 => d.write16(off as u16, val as u16),
        _ => d.write32(off as u16, val),
    });
}

struct SendPtr(*mut c_void);
unsafe impl Send for SendPtr {}
unsafe impl Sync for SendPtr {}

/// Route the device's interrupt to `f(arg)` (called in interrupt context,
/// then EOI). `msi` selects MSI instead of INTx. Returns the vector, or -1.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_pci_irq(
    idx: u32,
    msi: c_int,
    f: extern "C" fn(*mut c_void),
    arg: *mut c_void,
) -> c_int {
    let arg = alloc::sync::Arc::new(SendPtr(arg));
    let handler = move || f(arg.0);
    with_dev(idx, |d| {
        if msi != 0 {
            d.enable_msi(Box::new(handler))
        } else {
            d.enable_intx(Box::new(handler))
        }
    })
    .flatten()
    .map(|v| v as c_int)
    .unwrap_or(-1)
}

/// Whether the device has an MSI capability.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_pci_has_msi(idx: u32) -> c_int {
    with_dev(idx, |d| d.find_capability(pci::CAP_MSI).is_some() as c_int).unwrap_or(0)
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_random_u64() -> u64 {
    crate::drivers::random::u64()
}

/// Whether a native RustOS driver drives device `idx` (Linux drivers then
/// do not bind it).
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_pci_claimed(idx: u32) -> c_int {
    with_dev(idx, |d| {
        crate::pci::claimed_by(d.bus, d.dev, d.func).is_some() as c_int
    })
    .unwrap_or(0)
}
