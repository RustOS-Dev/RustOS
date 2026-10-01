//! ACPI: firmware tables, power off, reset, and PCI interrupt routing.
//!
//! Static tables (MADT, MCFG, HPET, FADT) are parsed at boot. The AML
//! interpreter is only created on first use (shutdown, `_PRT` lookups) so
//! firmware bytecode the interpreter cannot handle never blocks boot.

use crate::sync::Mutex;
use acpi::aml::{Interpreter, namespace::AmlName, object::Object};
use acpi::platform::AcpiPlatform;
use acpi::platform::interrupt::{InterruptModel, Polarity, TriggerMode};
use acpi::sdt::{fadt::Fadt, mcfg::Mcfg};
use acpi::{AcpiTables, Handle, Handler, HpetInfo, PciAddress, PhysicalMapping};
use alloc::string::String;
use alloc::vec::Vec;
use core::ptr::NonNull;
use core::str::FromStr;
use spin::Once;
use x86_64::instructions::port::Port;

#[derive(Clone, Copy, Debug)]
pub struct KernelAcpi;

impl Handler for KernelAcpi {
    unsafe fn map_physical_region<T>(&self, phys: usize, size: usize) -> PhysicalMapping<Self, T> {
        crate::mm::ensure_phys_mapped(phys as u64, size);
        PhysicalMapping {
            physical_start: phys,
            virtual_start: NonNull::new(crate::mm::phys_ptr::<T>(phys as u64)).unwrap(),
            region_length: size,
            mapped_length: size,
            handler: *self,
        }
    }

    fn unmap_physical_region<T>(_region: &PhysicalMapping<Self, T>) {}

    fn read_u8(&self, a: usize) -> u8 {
        mem_read(a)
    }
    fn read_u16(&self, a: usize) -> u16 {
        mem_read(a)
    }
    fn read_u32(&self, a: usize) -> u32 {
        mem_read(a)
    }
    fn read_u64(&self, a: usize) -> u64 {
        mem_read(a)
    }
    fn write_u8(&self, a: usize, v: u8) {
        mem_write(a, v)
    }
    fn write_u16(&self, a: usize, v: u16) {
        mem_write(a, v)
    }
    fn write_u32(&self, a: usize, v: u32) {
        mem_write(a, v)
    }
    fn write_u64(&self, a: usize, v: u64) {
        mem_write(a, v)
    }

    fn read_io_u8(&self, port: u16) -> u8 {
        unsafe { Port::new(port).read() }
    }
    fn read_io_u16(&self, port: u16) -> u16 {
        unsafe { Port::new(port).read() }
    }
    fn read_io_u32(&self, port: u16) -> u32 {
        unsafe { Port::new(port).read() }
    }
    fn write_io_u8(&self, port: u16, v: u8) {
        unsafe { Port::new(port).write(v) }
    }
    fn write_io_u16(&self, port: u16, v: u16) {
        unsafe { Port::new(port).write(v) }
    }
    fn write_io_u32(&self, port: u16, v: u32) {
        unsafe { Port::new(port).write(v) }
    }

    fn read_pci_u8(&self, a: PciAddress, off: u16) -> u8 {
        (crate::pci::config_read32(a.segment(), a.bus(), a.device(), a.function(), off & !3)
            >> ((off & 3) * 8)) as u8
    }
    fn read_pci_u16(&self, a: PciAddress, off: u16) -> u16 {
        (crate::pci::config_read32(a.segment(), a.bus(), a.device(), a.function(), off & !3)
            >> ((off & 2) * 8)) as u16
    }
    fn read_pci_u32(&self, a: PciAddress, off: u16) -> u32 {
        crate::pci::config_read32(a.segment(), a.bus(), a.device(), a.function(), off)
    }
    fn write_pci_u8(&self, a: PciAddress, off: u16, v: u8) {
        let (s, b, d, f) = (a.segment(), a.bus(), a.device(), a.function());
        let shift = (off & 3) * 8;
        let old = crate::pci::config_read32(s, b, d, f, off & !3);
        let new = (old & !(0xff << shift)) | ((v as u32) << shift);
        crate::pci::config_write32(s, b, d, f, off & !3, new);
    }
    fn write_pci_u16(&self, a: PciAddress, off: u16, v: u16) {
        let (s, b, d, f) = (a.segment(), a.bus(), a.device(), a.function());
        let shift = (off & 2) * 8;
        let old = crate::pci::config_read32(s, b, d, f, off & !3);
        let new = (old & !(0xffff << shift)) | ((v as u32) << shift);
        crate::pci::config_write32(s, b, d, f, off & !3, new);
    }
    fn write_pci_u32(&self, a: PciAddress, off: u16, v: u32) {
        crate::pci::config_write32(a.segment(), a.bus(), a.device(), a.function(), off, v)
    }

    fn nanos_since_boot(&self) -> u64 {
        crate::time::nanos()
    }
    fn stall(&self, us: u64) {
        crate::time::delay_us(us)
    }
    fn sleep(&self, ms: u64) {
        crate::time::sleep_ms(ms)
    }

    fn create_mutex(&self) -> Handle {
        Handle(0)
    }
    fn acquire(&self, _m: Handle, _timeout: u16) -> Result<(), acpi::aml::AmlError> {
        // AML is only evaluated under the INTERPRETER lock.
        Ok(())
    }
    fn release(&self, _m: Handle) {}
}

fn mem_read<T: Copy>(phys: usize) -> T {
    crate::mm::ensure_phys_mapped(phys as u64, core::mem::size_of::<T>());
    unsafe { core::ptr::read_volatile(crate::mm::phys_ptr::<T>(phys as u64)) }
}

fn mem_write<T: Copy>(phys: usize, v: T) {
    crate::mm::ensure_phys_mapped(phys as u64, core::mem::size_of::<T>());
    unsafe { core::ptr::write_volatile(crate::mm::phys_ptr::<T>(phys as u64), v) }
}

// ---------------------------------------------------------------------------
// Parsed platform information
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct IoApicInfo {
    pub id: u8,
    pub address: u64,
    pub gsi_base: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct IsaOverride {
    pub isa_irq: u8,
    pub gsi: u32,
    pub active_low: bool,
    pub level: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct EcamRegion {
    pub base: u64,
    pub segment: u16,
    pub bus_start: u8,
    pub bus_end: u8,
}

#[derive(Debug, Clone, Copy)]
pub struct GenericReg {
    pub space: u8,
    pub address: u64,
}

#[derive(Debug, Default)]
pub struct PlatformInfo {
    pub lapic_address: u64,
    pub io_apics: Vec<IoApicInfo>,
    pub isa_overrides: Vec<IsaOverride>,
    pub has_legacy_pic: bool,
    /// Local APIC IDs of usable processors (BSP first).
    pub cpus: Vec<u32>,
    pub ecam: Vec<EcamRegion>,
    pub hpet_base: Option<u64>,
    pub pm_timer_port: Option<u16>,
    pub pm1a_cnt: Option<GenericReg>,
    pub pm1b_cnt: Option<GenericReg>,
    pub reset_reg: Option<(GenericReg, u8)>,
    pub sci_irq: u16,
    pub century_reg: u8,
}

static PLATFORM: Once<PlatformInfo> = Once::new();
static ACPI: Mutex<Option<AcpiPlatform<KernelAcpi>>> = Mutex::new(None);
static INTERPRETER: Mutex<Option<Interpreter<KernelAcpi>>> = Mutex::new(None);
static AML_FAILED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Parsed platform info. Returns defaults when ACPI is unavailable.
pub fn platform() -> &'static PlatformInfo {
    PLATFORM.call_once(PlatformInfo::default)
}

/// Parse ACPI tables starting from the RSDP physical address.
pub fn init(rsdp: Option<u64>) {
    let rsdp = match rsdp {
        Some(r) => r,
        None => {
            crate::println!("[acpi] no RSDP from bootloader; running without ACPI");
            platform();
            return;
        }
    };
    let tables = match unsafe { AcpiTables::from_rsdp(KernelAcpi, rsdp as usize) } {
        Ok(t) => t,
        Err(e) => {
            crate::println!("[acpi] bad RSDP: {:?}", e);
            platform();
            return;
        }
    };

    let mut info = PlatformInfo::default();

    if let Some(mcfg) = tables.find_table::<Mcfg>() {
        for e in mcfg.get().entries() {
            info.ecam.push(EcamRegion {
                base: e.base_address,
                segment: e.pci_segment_group,
                bus_start: e.bus_number_start,
                bus_end: e.bus_number_end,
            });
        }
    }
    if let Ok(hpet) = HpetInfo::new(&tables) {
        info.hpet_base = Some(hpet.base_address as u64);
    }
    if let Some(fadt) = tables.find_table::<Fadt>() {
        info.sci_irq = fadt.sci_interrupt;
        info.century_reg = fadt.century;
        let conv = |g: acpi::address::GenericAddress| GenericReg {
            space: match g.address_space {
                acpi::address::AddressSpace::SystemIo => 1,
                _ => 0,
            },
            address: g.address,
        };
        if let Ok(g) = fadt.pm1a_control_block() {
            info.pm1a_cnt = Some(conv(g));
        }
        if let Ok(Some(g)) = fadt.pm1b_control_block() {
            info.pm1b_cnt = Some(conv(g));
        }
        if let Ok(Some(g)) = fadt.pm_timer_block()
            && matches!(g.address_space, acpi::address::AddressSpace::SystemIo)
        {
            info.pm_timer_port = Some(g.address as u16);
        }
        let flags = { fadt.flags };
        if flags.supports_system_reset_via_fadt()
            && let Ok(g) = fadt.reset_register()
        {
            info.reset_reg = Some((conv(g), fadt.reset_value));
        }
    }

    match AcpiPlatform::new(tables, KernelAcpi) {
        Ok(p) => {
            if let InterruptModel::Apic(apic) = &p.interrupt_model {
                info.lapic_address = apic.local_apic_address;
                info.has_legacy_pic = apic.also_has_legacy_pics;
                for io in apic.io_apics.iter() {
                    info.io_apics.push(IoApicInfo {
                        id: io.id,
                        address: io.address as u64,
                        gsi_base: io.global_system_interrupt_base,
                    });
                }
                for o in apic.interrupt_source_overrides.iter() {
                    info.isa_overrides.push(IsaOverride {
                        isa_irq: o.isa_source,
                        gsi: o.global_system_interrupt,
                        active_low: matches!(o.polarity, Polarity::ActiveLow),
                        level: matches!(o.trigger_mode, TriggerMode::Level),
                    });
                }
            }
            if let Some(pi) = &p.processor_info {
                info.cpus.push(pi.boot_processor.local_apic_id);
                for ap in pi.application_processors.iter() {
                    if ap.state != acpi::platform::ProcessorState::Disabled {
                        info.cpus.push(ap.local_apic_id);
                    }
                }
            }
            *ACPI.lock() = Some(p);
        }
        Err(e) => crate::println!("[acpi] platform parse failed: {:?}", e),
    }

    crate::println!(
        "[acpi] {} CPU(s), {} IOAPIC(s), ECAM regions: {}, HPET: {}",
        info.cpus.len(),
        info.io_apics.len(),
        info.ecam.len(),
        if info.hpet_base.is_some() {
            "yes"
        } else {
            "no"
        }
    );
    PLATFORM.call_once(|| info);
}

/// Run `f` with the AML interpreter, creating it on first use.
fn with_interpreter<R>(f: impl FnOnce(&Interpreter<KernelAcpi>) -> R) -> Option<R> {
    if AML_FAILED.load(core::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    let mut guard = INTERPRETER.lock();
    if guard.is_none() {
        let acpi = ACPI.lock();
        let platform = acpi.as_ref()?;
        match Interpreter::new_from_platform(platform) {
            Ok(i) => {
                i.initialize_namespace();
                // Tell the firmware interrupts go through the IOAPIC, as
                // Linux does: `_PRT` methods then return APIC routing
                // (GSIs) instead of the legacy-PIC link routing.
                if let Ok(pic) = AmlName::from_str("\\_PIC") {
                    let _ = i.evaluate_if_present(pic, alloc::vec![Object::Integer(1).wrap()]);
                }
                *guard = Some(i);
            }
            Err(e) => {
                crate::println!("[acpi] AML interpreter unavailable: {:?}", e);
                AML_FAILED.store(true, core::sync::atomic::Ordering::Relaxed);
                return None;
            }
        }
    }
    guard.as_ref().map(f)
}

fn eval_integer_package(path: &str) -> Option<Vec<u64>> {
    with_interpreter(|i| {
        let name = AmlName::from_str(path).ok()?;
        let obj = i.evaluate(name, Vec::new()).ok()?;
        match &*obj {
            Object::Package(elems) => Some(
                elems
                    .iter()
                    .map(|e| match **e {
                        Object::Integer(v) => v,
                        _ => 0,
                    })
                    .collect(),
            ),
            _ => None,
        }
    })
    .flatten()
}

fn write_reg(reg: GenericReg, value: u16) {
    if reg.space == 1 {
        unsafe { Port::<u16>::new(reg.address as u16).write(value) };
    } else {
        mem_write(reg.address as usize, value);
    }
}

/// Enter ACPI S5 (soft off). Falls back to emulator-specific ports.
pub fn shutdown() -> ! {
    let info = platform();
    let s5 = eval_integer_package("\\_S5");
    if let Ok(pts) = AmlName::from_str("\\_PTS") {
        let _ = with_interpreter(|i| {
            i.evaluate_if_present(pts, alloc::vec![Object::Integer(5).wrap()])
        });
    }
    x86_64::instructions::interrupts::disable();
    super::smp::halt_others();
    const SLP_EN: u16 = 1 << 13;
    match (s5, info.pm1a_cnt) {
        (Some(v), Some(pm1a)) if !v.is_empty() => {
            let a = v[0] as u16 & 7;
            let b = v.get(1).copied().unwrap_or(v[0]) as u16 & 7;
            write_reg(pm1a, (a << 10) | SLP_EN);
            if let Some(pm1b) = info.pm1b_cnt {
                write_reg(pm1b, (b << 10) | SLP_EN);
            }
        }
        (_, Some(pm1a)) => {
            // No usable _S5: try the common SLP_TYP encodings.
            for typ in [5u16, 7, 0] {
                write_reg(pm1a, (typ << 10) | SLP_EN);
                crate::time::delay_us(10_000);
            }
        }
        _ => {}
    }
    unsafe {
        Port::<u16>::new(0x604).write(0x2000); // QEMU
        Port::<u16>::new(0xB004).write(0x2000); // Bochs / old QEMU
        Port::<u16>::new(0x4004).write(0x3400); // VirtualBox
    }
    crate::hlt_loop();
}

/// Reset the machine: FADT reset register, then 0xCF9, then the 8042, then a
/// triple fault.
pub fn reboot() -> ! {
    x86_64::instructions::interrupts::disable();
    super::smp::halt_others();
    if let Some((reg, val)) = platform().reset_reg {
        match reg.space {
            1 => unsafe { Port::<u8>::new(reg.address as u16).write(val) },
            0 => mem_write(reg.address as usize, val),
            _ => {}
        }
        crate::time::delay_us(50_000);
    }
    unsafe {
        let mut cf9 = Port::<u8>::new(0xCF9);
        cf9.write(0x02);
        crate::time::delay_us(10);
        cf9.write(0x06);
    }
    crate::time::delay_us(50_000);
    unsafe {
        let mut status = Port::<u8>::new(0x64);
        for _ in 0..100_000 {
            if status.read() & 2 == 0 {
                break;
            }
        }
        status.write(0xFE);
    }
    crate::time::delay_us(50_000);
    // Triple fault: load an empty IDT and trap.
    unsafe {
        let idt = x86_64::structures::DescriptorTablePointer {
            limit: 0,
            base: x86_64::VirtAddr::new(0),
        };
        x86_64::instructions::tables::lidt(&idt);
        core::arch::asm!("int3", options(noreturn));
    }
}

/// Resolve the GSI for a legacy PCI interrupt pin via `_PRT`.
/// `pin` is 1-based (1 = INTA) as in config space.
pub fn pci_irq_route(device: u8, function: u8, pin: u8) -> Option<u32> {
    use acpi::aml::pci_routing::{PciRoutingTable, Pin};
    let pin = match pin {
        1 => Pin::IntA,
        2 => Pin::IntB,
        3 => Pin::IntC,
        4 => Pin::IntD,
        _ => return None,
    };
    with_interpreter(|i| {
        for root in ["\\_SB.PCI0._PRT", "\\_SB.PC00._PRT", "\\_SB.PCI1._PRT"] {
            let Ok(path) = AmlName::from_str(root) else {
                continue;
            };
            if let Ok(table) = PciRoutingTable::from_prt_path(path, i)
                && let Ok(irq) = table.route(device as u16, function as u16, pin, i)
            {
                return Some(irq.irq);
            }
        }
        None
    })
    .flatten()
}

/// True if the machine runs on battery: some AC adapter's `_PSR`
/// reports offline. `None` when there is no AC adapter (a desktop) or
/// the AML interpreter is unavailable.
pub fn on_battery() -> Option<bool> {
    use acpi::aml::namespace::NameSeg;
    let psr = NameSeg::from_bytes(*b"_PSR").ok()?;
    with_interpreter(|i| {
        let mut paths = Vec::new();
        let _ = i.namespace.lock().traverse(|name, level| {
            if level.values.contains_key(&psr)
                && let Ok(p) = AmlName::from_name_seg(psr).resolve(name)
            {
                paths.push(p);
            }
            Ok(true)
        });
        let mut online = None;
        for p in paths {
            if let Ok(obj) = i.evaluate(p, Vec::new())
                && let Object::Integer(v) = *obj
            {
                online = Some(online.unwrap_or(false) || v != 0);
            }
        }
        online.map(|o| !o)
    })
    .flatten()
}

// ---------------------------------------------------------------------------
// General AML evaluation (for LinuxKPI's ACPI API)
// ---------------------------------------------------------------------------

/// A value passed to or returned from AML.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Integer(u64),
    String(String),
    Buffer(Vec<u8>),
    Package(Vec<Value>),
    /// A reference or other object with no plain-data form.
    Other,
}

fn to_value(obj: &Object) -> Value {
    match obj {
        Object::Integer(v) => Value::Integer(*v),
        Object::String(s) => Value::String(s.clone()),
        Object::Buffer(b) => Value::Buffer(b.clone()),
        Object::Package(elems) => Value::Package(elems.iter().map(|e| to_value(e)).collect()),
        _ => Value::Other,
    }
}

fn from_value(v: &Value) -> acpi::aml::object::WrappedObject {
    match v {
        Value::Integer(i) => Object::Integer(*i).wrap(),
        Value::String(s) => Object::String(s.clone()).wrap(),
        Value::Buffer(b) => Object::Buffer(b.clone()).wrap(),
        Value::Package(p) => Object::Package(p.iter().map(from_value).collect()).wrap(),
        Value::Other => Object::Uninitialized.wrap(),
    }
}

/// Why an evaluation failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvalError {
    /// No AML interpreter (no ACPI, or the tables failed to load).
    NoInterpreter,
    /// The path does not name an object.
    NotFound,
    /// The method ran and failed.
    Failed,
}

/// Evaluate `path` (an absolute AML path such as `\_SB.PCI0._PRT`) with
/// `args`: run it if it is a method, otherwise return its value.
pub fn eval(path: &str, args: &[Value]) -> Result<Value, EvalError> {
    let name = AmlName::from_str(path).map_err(|_| EvalError::NotFound)?;
    with_interpreter(
        |i| match i.evaluate(name, args.iter().map(from_value).collect()) {
            Ok(obj) => Ok(to_value(&obj)),
            Err(acpi::aml::AmlError::ObjectDoesNotExist(_)) => Err(EvalError::NotFound),
            Err(e) => {
                crate::println!("[acpi] {path}: {e:?}");
                Err(EvalError::Failed)
            }
        },
    )
    .unwrap_or(Err(EvalError::NoInterpreter))
}

/// Whether `path` names an object.
pub fn exists(path: &str) -> bool {
    let Ok(name) = AmlName::from_str(path) else {
        return false;
    };
    with_interpreter(|i| {
        let mut ns = i.namespace.lock();
        // Objects, or scopes and devices (namespace levels).
        ns.level_exists(&name) || ns.get(name).is_ok()
    })
    .unwrap_or(false)
}

/// A device in the ACPI namespace.
#[derive(Clone, Debug)]
pub struct Device {
    pub path: String,
    /// `_ADR`, evaluated (PCI: device << 16 | function).
    pub adr: Option<u64>,
    /// `_HID` as a string (EISA ids decoded, e.g. `PNP0C50`).
    pub hid: Option<String>,
}

fn eisa_id(v: u64) -> String {
    let v = (v as u32).swap_bytes();
    let c = |s: u32| (b'@' + ((v >> s) & 0x1f) as u8) as char;
    alloc::format!("{}{}{}{:04X}", c(26), c(21), c(16), v & 0xffff)
}

/// Every device in the namespace, with `_ADR` and `_HID` evaluated.
pub fn devices() -> Vec<Device> {
    use acpi::aml::namespace::NamespaceLevelKind;
    let paths: Vec<String> = with_interpreter(|i| {
        let mut out = Vec::new();
        let _ = i.namespace.lock().traverse(|name, level| {
            if matches!(level.kind, NamespaceLevelKind::Device) {
                out.push(name.as_string());
            }
            Ok(true)
        });
        out
    })
    .unwrap_or_default();
    paths
        .into_iter()
        .map(|path| {
            let adr = match eval(&alloc::format!("{path}._ADR"), &[]) {
                Ok(Value::Integer(a)) => Some(a),
                _ => None,
            };
            let hid = match eval(&alloc::format!("{path}._HID"), &[]) {
                Ok(Value::Integer(v)) => Some(eisa_id(v)),
                Ok(Value::String(s)) => Some(s),
                _ => None,
            };
            Device { path, adr, hid }
        })
        .collect()
}

/// The ACPI device for PCI device `dev.func` on the root bus: the device
/// under the PCI root bridge (`PNP0A03`/`PNP0A08`) whose `_ADR` matches.
pub fn pci_device_path(dev: u8, func: u8) -> Option<String> {
    let all = devices();
    let roots: Vec<&Device> = all
        .iter()
        .filter(|d| matches!(d.hid.as_deref(), Some("PNP0A03" | "PNP0A08")))
        .collect();
    let want = ((dev as u64) << 16) | func as u64;
    all.iter()
        .find(|d| {
            d.adr == Some(want)
                && roots.iter().any(|r| {
                    d.path
                        .strip_prefix(r.path.as_str())
                        .is_some_and(|rest| rest.starts_with('.') && !rest[1..].contains('.'))
                })
        })
        .map(|d| d.path.clone())
}

/// The physical address and length of the first ACPI table with
/// signature `sig` (e.g. `*b"MCFG"`), `instance` counting from 1.
pub fn table(sig: &[u8; 4], instance: usize) -> Option<(u64, usize)> {
    let acpi = ACPI.lock();
    let platform = acpi.as_ref()?;
    platform
        .tables
        .table_headers()
        .filter(|(_, h)| h.signature.as_str().as_bytes() == sig)
        .nth(instance.saturating_sub(1))
        .map(|(phys, h)| (phys as u64, h.length as usize))
}
