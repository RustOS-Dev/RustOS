//! Local APIC (xAPIC or x2APIC), I/O APIC and legacy 8259 masking.

use super::acpi;
use crate::sync::Mutex;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use x86_64::registers::model_specific::Msr;

const IA32_APIC_BASE: u32 = 0x1B;
const APIC_BASE_ENABLE: u64 = 1 << 11;
const APIC_BASE_X2: u64 = 1 << 10;

// Register offsets (xAPIC MMIO byte offsets; x2APIC MSR = 0x800 + off/16).
pub const REG_ID: u32 = 0x20;
pub const REG_VERSION: u32 = 0x30;
pub const REG_TPR: u32 = 0x80;
pub const REG_EOI: u32 = 0xB0;
pub const REG_SVR: u32 = 0xF0;
pub const REG_ESR: u32 = 0x280;
pub const REG_ICR_LOW: u32 = 0x300;
pub const REG_ICR_HIGH: u32 = 0x310;
pub const REG_LVT_TIMER: u32 = 0x320;
pub const REG_LVT_LINT0: u32 = 0x350;
pub const REG_LVT_LINT1: u32 = 0x360;
pub const REG_LVT_ERROR: u32 = 0x370;
pub const REG_TIMER_INIT: u32 = 0x380;
pub const REG_TIMER_CURRENT: u32 = 0x390;
pub const REG_TIMER_DIV: u32 = 0x3E0;

pub const LVT_MASKED: u32 = 1 << 16;
pub const LVT_TIMER_PERIODIC: u32 = 1 << 17;

static X2APIC: AtomicBool = AtomicBool::new(false);

/// Whether the local APIC runs in x2APIC (MSR) mode.
pub fn is_x2apic() -> bool {
    X2APIC.load(Ordering::Relaxed)
}
static XAPIC_BASE: AtomicU64 = AtomicU64::new(0);
static ENABLED: AtomicBool = AtomicBool::new(false);

fn x2apic_supported() -> bool {
    let r = core::arch::x86_64::__cpuid(1);
    r.ecx & (1 << 21) != 0
}

pub fn read(reg: u32) -> u32 {
    if X2APIC.load(Ordering::Relaxed) {
        unsafe { Msr::new(0x800 + (reg >> 4)).read() as u32 }
    } else {
        let base = XAPIC_BASE.load(Ordering::Relaxed);
        unsafe { core::ptr::read_volatile((base + reg as u64) as *const u32) }
    }
}

pub fn write(reg: u32, val: u32) {
    if X2APIC.load(Ordering::Relaxed) {
        unsafe { Msr::new(0x800 + (reg >> 4)).write(val as u64) }
    } else {
        let base = XAPIC_BASE.load(Ordering::Relaxed);
        unsafe { core::ptr::write_volatile((base + reg as u64) as *mut u32, val) }
    }
}

/// This CPU's local APIC ID.
pub fn id() -> u32 {
    if X2APIC.load(Ordering::Relaxed) {
        read(REG_ID)
    } else {
        read(REG_ID) >> 24
    }
}

pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Signal end-of-interrupt to the local APIC.
#[inline]
pub fn eoi() {
    if ENABLED.load(Ordering::Relaxed) {
        write(REG_EOI, 0);
    }
}

/// Enable the local APIC on the current CPU. The first call (BSP) also
/// selects x2APIC vs xAPIC mode and maps the xAPIC MMIO page.
pub fn init_local() {
    if !ENABLED.load(Ordering::SeqCst) {
        if x2apic_supported() {
            X2APIC.store(true, Ordering::SeqCst);
        } else {
            let phys = match acpi::platform().lapic_address {
                0 => (unsafe { Msr::new(IA32_APIC_BASE).read() }) & 0xFFFF_F000,
                a => a,
            };
            XAPIC_BASE.store(crate::mm::map_mmio(phys, 4096), Ordering::SeqCst);
        }
    }
    unsafe {
        let mut base = Msr::new(IA32_APIC_BASE);
        let mut v = base.read() | APIC_BASE_ENABLE;
        if X2APIC.load(Ordering::SeqCst) {
            v |= APIC_BASE_X2;
        }
        base.write(v);
    }
    write(REG_TPR, 0);
    write(REG_LVT_LINT0, LVT_MASKED);
    write(REG_LVT_LINT1, LVT_MASKED | (4 << 8)); // NMI delivery, masked
    write(REG_LVT_ERROR, super::idt::VEC_APIC_ERROR as u32);
    write(REG_ESR, 0);
    write(REG_ESR, 0);
    write(REG_SVR, 0x100 | super::idt::VEC_SPURIOUS as u32);
    ENABLED.store(true, Ordering::SeqCst);
    eoi();
}

pub fn apic_error_handler(_f: &mut super::idt::TrapFrame) {
    write(REG_ESR, 0);
    let esr = read(REG_ESR);
    crate::serial_println!("[apic] error ESR={:#x}", esr);
    eoi();
}

/// Send an IPI. `shorthand`: 0 = none, 1 = self, 2 = all incl. self, 3 = all but self.
pub fn send_ipi_raw(dest: u32, low: u32) {
    if X2APIC.load(Ordering::Relaxed) {
        unsafe { Msr::new(0x830).write(((dest as u64) << 32) | low as u64) };
    } else {
        // ICR high/low are two writes: an interrupt in between whose
        // handler sends an IPI would redirect this one.
        x86_64::instructions::interrupts::without_interrupts(|| {
            while read(REG_ICR_LOW) & (1 << 12) != 0 {
                core::hint::spin_loop();
            }
            write(REG_ICR_HIGH, dest << 24);
            write(REG_ICR_LOW, low);
            while read(REG_ICR_LOW) & (1 << 12) != 0 {
                core::hint::spin_loop();
            }
        });
    }
}

/// Fixed-delivery IPI to one CPU.
pub fn send_ipi(dest_apic: u32, vector: u8) {
    send_ipi_raw(dest_apic, vector as u32 | (1 << 14));
}

/// Fixed-delivery IPI to every other CPU.
pub fn send_ipi_all_but_self(vector: u8) {
    send_ipi_raw(0, vector as u32 | (1 << 14) | (3 << 18));
}

pub fn send_init(dest_apic: u32) {
    send_ipi_raw(dest_apic, (5 << 8) | (1 << 14) | (1 << 15));
    crate::time::delay_us(200);
    send_ipi_raw(dest_apic, (5 << 8) | (1 << 15));
}

pub fn send_sipi(dest_apic: u32, page: u8) {
    send_ipi_raw(dest_apic, (6 << 8) | page as u32 | (1 << 14));
}

// ---------------------------------------------------------------------------
// I/O APIC
// ---------------------------------------------------------------------------

struct IoApic {
    base: u64,
    gsi_base: u32,
    entries: u32,
}

impl IoApic {
    fn read(&self, reg: u32) -> u32 {
        unsafe {
            core::ptr::write_volatile(self.base as *mut u32, reg);
            core::ptr::read_volatile((self.base + 0x10) as *const u32)
        }
    }
    fn write(&self, reg: u32, val: u32) {
        unsafe {
            core::ptr::write_volatile(self.base as *mut u32, reg);
            core::ptr::write_volatile((self.base + 0x10) as *mut u32, val);
        }
    }
    fn set_entry(&self, idx: u32, low: u32, high: u32) {
        self.write(0x10 + idx * 2 + 1, high);
        self.write(0x10 + idx * 2, low);
    }
}

static IOAPICS: Mutex<Vec<IoApic>> = Mutex::new(Vec::new());

/// Map every I/O APIC, mask all inputs and mask the legacy 8259s.
pub fn init_io() {
    disable_legacy_pic();
    let mut list = IOAPICS.lock();
    for info in acpi::platform().io_apics.iter() {
        let base = crate::mm::map_mmio(info.address, 0x20);
        let mut io = IoApic {
            base,
            gsi_base: info.gsi_base,
            entries: 0,
        };
        io.entries = ((io.read(1) >> 16) & 0xFF) + 1;
        for i in 0..io.entries {
            io.set_entry(i, 1 << 16, 0);
        }
        list.push(io);
    }
    if list.is_empty() {
        // No MADT: assume the standard single I/O APIC.
        let base = crate::mm::map_mmio(0xFEC0_0000, 0x20);
        let mut io = IoApic {
            base,
            gsi_base: 0,
            entries: 0,
        };
        io.entries = ((io.read(1) >> 16) & 0xFF) + 1;
        for i in 0..io.entries {
            io.set_entry(i, 1 << 16, 0);
        }
        list.push(io);
    }
}

/// Route a global system interrupt to `vector` on CPU `dest_apic`.
pub fn route_gsi(gsi: u32, vector: u8, dest_apic: u32, level: bool, active_low: bool) -> bool {
    let list = IOAPICS.lock();
    for io in list.iter() {
        if gsi >= io.gsi_base && gsi < io.gsi_base + io.entries {
            let mut low = vector as u32;
            if active_low {
                low |= 1 << 13;
            }
            if level {
                low |= 1 << 15;
            }
            io.set_entry(gsi - io.gsi_base, low, dest_apic << 24);
            return true;
        }
    }
    false
}

pub fn mask_gsi(gsi: u32) {
    let list = IOAPICS.lock();
    for io in list.iter() {
        if gsi >= io.gsi_base && gsi < io.gsi_base + io.entries {
            io.set_entry(gsi - io.gsi_base, 1 << 16, 0);
        }
    }
}

/// Route an ISA IRQ (0–15), honouring MADT source overrides.
pub fn route_isa_irq(irq: u8, vector: u8) -> bool {
    let ov = acpi::platform()
        .isa_overrides
        .iter()
        .find(|o| o.isa_irq == irq)
        .copied();
    let (gsi, level, low) = match ov {
        Some(o) => (o.gsi, o.level, o.active_low),
        None => (irq as u32, false, false),
    };
    route_gsi(gsi, vector, id(), level, low)
}

fn disable_legacy_pic() {
    use x86_64::instructions::port::Port;
    unsafe {
        let mut pic1_cmd = Port::<u8>::new(0x20);
        let mut pic1_data = Port::<u8>::new(0x21);
        let mut pic2_cmd = Port::<u8>::new(0xA0);
        let mut pic2_data = Port::<u8>::new(0xA1);
        // Re-initialise with vectors 0x20–0x2F so any spurious interrupt lands
        // on a known vector, then mask everything.
        pic1_cmd.write(0x11);
        pic2_cmd.write(0x11);
        pic1_data.write(0x20);
        pic2_data.write(0x28);
        pic1_data.write(4);
        pic2_data.write(2);
        pic1_data.write(1);
        pic2_data.write(1);
        pic1_data.write(0xFF);
        pic2_data.write(0xFF);
    }
}
