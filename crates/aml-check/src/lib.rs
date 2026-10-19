//! Run ACPI tables through RustOS's AML interpreter (third_party/acpi) on
//! the host: load the DSDT and SSDTs as the kernel does, then evaluate the
//! devices' identification and resource methods. The hardware is a stand-in:
//! memory and PCI configuration read as zero (all ones for PCI), I/O ports
//! are plain storage, except an index/data pair at 0x72/0x73 (as on AMD
//! platforms) backed by a 256-byte table.

use acpi::aml::namespace::{AmlName, NamespaceLevelKind};
use acpi::aml::object::Object;
use acpi::aml::{AmlError, Interpreter};
use acpi::registers::FixedRegisters;
use acpi::sdt::fadt::Fadt;
use acpi::{Handle, Handler, PciAddress, PhysicalMapping};
use std::ptr::NonNull;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

/// The stand-in hardware's state.
pub struct State {
    pub io: Vec<u8>,
    /// Index register and table of the 0x72/0x73 pair.
    pub index: u8,
    pub table: [u8; 256],
}

#[derive(Clone)]
pub struct Machine(pub Arc<Mutex<State>>);

impl Machine {
    pub fn new() -> Machine {
        let mut table = [0u8; 256];
        for (i, b) in table.iter_mut().enumerate() {
            *b = 0x10u8.wrapping_add(i as u8);
        }
        Machine(Arc::new(Mutex::new(State {
            io: vec![0; 0x10000],
            index: 0,
            table,
        })))
    }

    fn io_read(&self, port: u16) -> u8 {
        let s = self.0.lock().unwrap();
        match port {
            0x72 => s.index,
            0x73 => s.table[s.index as usize],
            p => s.io[p as usize],
        }
    }

    fn io_write(&self, port: u16, v: u8) {
        let mut s = self.0.lock().unwrap();
        match port {
            0x72 => s.index = v,
            0x73 => {
                let i = s.index as usize;
                s.table[i] = v;
            }
            p => s.io[p as usize] = v,
        }
    }
}

impl Default for Machine {
    fn default() -> Self {
        Self::new()
    }
}

impl Handler for Machine {
    unsafe fn map_physical_region<T>(&self, phys: usize, size: usize) -> PhysicalMapping<Self, T> {
        // Only the FADT the caller passes is ever mapped: its address is a host pointer.
        PhysicalMapping {
            physical_start: phys,
            virtual_start: NonNull::new(phys as *mut T).unwrap(),
            region_length: size,
            mapped_length: size,
            handler: self.clone(),
        }
    }
    fn unmap_physical_region<T>(_r: &PhysicalMapping<Self, T>) {}
    fn read_u8(&self, _a: usize) -> u8 {
        0
    }
    fn read_u16(&self, _a: usize) -> u16 {
        0
    }
    fn read_u32(&self, _a: usize) -> u32 {
        0
    }
    fn read_u64(&self, _a: usize) -> u64 {
        0
    }
    fn write_u8(&self, _a: usize, _v: u8) {}
    fn write_u16(&self, _a: usize, _v: u16) {}
    fn write_u32(&self, _a: usize, _v: u32) {}
    fn write_u64(&self, _a: usize, _v: u64) {}
    fn read_io_u8(&self, p: u16) -> u8 {
        self.io_read(p)
    }
    fn read_io_u16(&self, p: u16) -> u16 {
        u16::from_le_bytes([self.io_read(p), self.io_read(p.wrapping_add(1))])
    }
    fn read_io_u32(&self, p: u16) -> u32 {
        u32::from_le_bytes([0, 1, 2, 3].map(|i| self.io_read(p.wrapping_add(i))))
    }
    fn write_io_u8(&self, p: u16, v: u8) {
        self.io_write(p, v)
    }
    fn write_io_u16(&self, p: u16, v: u16) {
        for (i, b) in v.to_le_bytes().into_iter().enumerate() {
            self.io_write(p.wrapping_add(i as u16), b);
        }
    }
    fn write_io_u32(&self, p: u16, v: u32) {
        for (i, b) in v.to_le_bytes().into_iter().enumerate() {
            self.io_write(p.wrapping_add(i as u16), b);
        }
    }
    fn read_pci_u8(&self, _a: PciAddress, _o: u16) -> u8 {
        0xff
    }
    fn read_pci_u16(&self, _a: PciAddress, _o: u16) -> u16 {
        0xffff
    }
    fn read_pci_u32(&self, _a: PciAddress, _o: u16) -> u32 {
        0xffff_ffff
    }
    fn write_pci_u8(&self, _a: PciAddress, _o: u16, _v: u8) {}
    fn write_pci_u16(&self, _a: PciAddress, _o: u16, _v: u16) {}
    fn write_pci_u32(&self, _a: PciAddress, _o: u16, _v: u32) {}
    fn nanos_since_boot(&self) -> u64 {
        0
    }
    fn stall(&self, _us: u64) {}
    fn sleep(&self, _ms: u64) {}
    fn create_mutex(&self) -> Handle {
        Handle(0)
    }
    fn acquire(&self, _m: Handle, _t: u16) -> Result<(), AmlError> {
        Ok(())
    }
    fn release(&self, _m: Handle) {}
}

/// A FADT with PM1a event and control blocks in I/O space, for tables that
/// come without the machine's own.
fn stand_in_fadt() -> Vec<u8> {
    let mut f = vec![0u8; 276];
    f[0..4].copy_from_slice(b"FACP");
    f[4..8].copy_from_slice(&276u32.to_le_bytes());
    f[8] = 1;
    f[56..60].copy_from_slice(&0x600u32.to_le_bytes());
    f[64..68].copy_from_slice(&0x604u32.to_le_bytes());
    f[88] = 4;
    f[89] = 2;
    f
}

/// An interpreter for a DSDT of revision `dsdt_revision`, with the
/// machine's FADT if given.
pub fn interpreter(
    machine: &Machine,
    fadt: Option<&[u8]>,
    dsdt_revision: u8,
) -> Interpreter<Machine> {
    let bytes: &'static [u8] = Box::leak(
        fadt.map_or_else(stand_in_fadt, <[u8]>::to_vec)
            .into_boxed_slice(),
    );
    // SAFETY: the bytes are a FADT (at least its fixed part) and live for ever.
    let fadt = unsafe { &*(bytes.as_ptr() as *const Fadt) };
    // The interpreter takes the registers in an Arc; this tool is single-threaded.
    #[allow(clippy::arc_with_non_send_sync)]
    let regs = Arc::new(FixedRegisters::new(fadt, machine.clone()).expect("FADT PM1 blocks"));
    Interpreter::new(machine.clone(), dsdt_revision, regs, None)
}

/// Load one table (header included); `Err` if the interpreter could not run all of it.
pub fn load(interp: &Interpreter<Machine>, table: &[u8]) -> Result<(), AmlError> {
    interp.load_table(&table[36..])
}

/// Evaluate `path` (no arguments).
pub fn eval(interp: &Interpreter<Machine>, path: &str) -> Result<Object, AmlError> {
    let name = AmlName::from_str(path)?;
    interp.evaluate(name, Vec::new()).map(|o| (*o).clone())
}

/// Every device's path in the namespace.
pub fn devices(interp: &Interpreter<Machine>) -> Vec<String> {
    let mut out = Vec::new();
    let _ = interp.namespace.lock().traverse(|name, level| {
        if matches!(level.kind, NamespaceLevelKind::Device) {
            out.push(name.as_string());
        }
        Ok(true)
    });
    out
}

/// Route every slot (0-31) and pin of the `_PRT` at `path` (link devices'
/// `_CRS` evaluated): `Ok(routes)`, or the first error other than a slot
/// or pin the table does not list. `Ok(None)` if there is no such `_PRT`.
pub fn prt_routes(interp: &Interpreter<Machine>, path: &str) -> Result<Option<usize>, AmlError> {
    use acpi::aml::pci_routing::{PciRoutingTable, Pin};
    let table = match PciRoutingTable::from_prt_path(AmlName::from_str(path)?, interp) {
        Ok(t) => t,
        Err(e) if format!("{e:?}").contains("DoesNotExist") => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut routes = 0;
    for device in 0..32 {
        for pin in [Pin::IntA, Pin::IntB, Pin::IntC, Pin::IntD] {
            match table.route(device, 0, pin, interp) {
                Ok(_) => routes += 1,
                Err(AmlError::PrtNoEntry) => {}
                Err(e) => return Err(e),
            }
        }
    }
    Ok(Some(routes))
}
