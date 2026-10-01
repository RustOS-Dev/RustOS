//! ACPI services for LinuxKPI's ACPI API (src/linuxkpi/c/acpi.c), on
//! RustOS's AML interpreter (`crate::arch::x86_64::acpi`).
//!
//! Values cross the boundary in a small tagged encoding:
//! 0 other, 1 integer (u64), 2 string (u32 length + bytes),
//! 3 buffer (u32 length + bytes), 4 package (u32 count + elements);
//! all little-endian.

use crate::arch::x86_64::acpi::{self, EvalError, Value};
use crate::errno::*;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::{CStr, c_char, c_int, c_void};

fn encode(v: &Value, out: &mut Vec<u8>) {
    match v {
        Value::Integer(i) => {
            out.push(1);
            out.extend_from_slice(&i.to_le_bytes());
        }
        Value::String(s) => {
            out.push(2);
            out.extend_from_slice(&(s.len() as u32).to_le_bytes());
            out.extend_from_slice(s.as_bytes());
        }
        Value::Buffer(b) => {
            out.push(3);
            out.extend_from_slice(&(b.len() as u32).to_le_bytes());
            out.extend_from_slice(b);
        }
        Value::Package(p) => {
            out.push(4);
            out.extend_from_slice(&(p.len() as u32).to_le_bytes());
            for e in p {
                encode(e, out);
            }
        }
        Value::Other => out.push(0),
    }
}

fn decode(data: &mut &[u8]) -> Option<Value> {
    fn take<'a>(d: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
        if d.len() < n {
            return None;
        }
        let (a, b) = d.split_at(n);
        *d = b;
        Some(a)
    }
    let tag = take(data, 1)?[0];
    Some(match tag {
        1 => Value::Integer(u64::from_le_bytes(take(data, 8)?.try_into().ok()?)),
        2..=4 => {
            let n = u32::from_le_bytes(take(data, 4)?.try_into().ok()?) as usize;
            match tag {
                2 => Value::String(String::from_utf8_lossy(take(data, n)?).into()),
                3 => Value::Buffer(take(data, n)?.to_vec()),
                _ => Value::Package((0..n).map(|_| decode(data)).collect::<Option<_>>()?),
            }
        }
        _ => Value::Other,
    })
}

fn cstr(p: *const c_char) -> String {
    String::from(unsafe { CStr::from_ptr(p) }.to_string_lossy())
}

/// Evaluate `path` with the encoded `args` (a sequence of values). The
/// encoded result goes into memory from `alloc`. Returns 0 or -errno
/// (-ENOENT: no such object, -ENODEV: no interpreter, -EIO: failed).
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_acpi_eval(
    path: *const c_char,
    args: *const u8,
    args_len: usize,
    alloc: extern "C" fn(usize) -> *mut c_void,
    out: *mut *mut c_void,
    out_len: *mut usize,
) -> c_int {
    let mut data = if args.is_null() {
        &[][..]
    } else {
        unsafe { core::slice::from_raw_parts(args, args_len) }
    };
    let mut argv = Vec::new();
    while !data.is_empty() {
        match decode(&mut data) {
            Some(v) => argv.push(v),
            None => return -EINVAL.0,
        }
    }
    let v = match acpi::eval(&cstr(path), &argv) {
        Ok(v) => v,
        Err(EvalError::NotFound) => return -ENOENT.0,
        Err(EvalError::NoInterpreter) => return -ENODEV.0,
        Err(EvalError::Failed) => return -EIO.0,
    };
    if out.is_null() {
        return 0;
    }
    let mut enc = Vec::new();
    encode(&v, &mut enc);
    let p = alloc(enc.len());
    if p.is_null() {
        return -ENOMEM.0;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(enc.as_ptr(), p as *mut u8, enc.len());
        *out = p;
        *out_len = enc.len();
    }
    0
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_acpi_exists(path: *const c_char) -> c_int {
    acpi::exists(&cstr(path)) as c_int
}

fn copy_out(s: &str, buf: *mut c_char, len: usize) -> c_int {
    if s.len() + 1 > len {
        return -ENAMETOOLONG.0;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(s.as_ptr(), buf as *mut u8, s.len());
        *buf.add(s.len()) = 0;
    }
    0
}

/// The PCI-to-PCI bridge whose secondary bus is `bus`.
fn bridge_for_bus(bus: u8) -> Option<(u8, u8, u8)> {
    crate::pci::enumerate()
        .into_iter()
        .find(|d| d.header_type & 0x7f == 1 && d.read8(0x19) == bus && bus != 0)
        .map(|d| (d.bus, d.dev, d.func))
}

fn pci_path(bus: u8, dev: u8, func: u8, devices: &[acpi::Device], depth: u32) -> Option<String> {
    let want = ((dev as u64) << 16) | func as u64;
    let parents: Vec<String> = if bus == 0 {
        devices
            .iter()
            .filter(|d| matches!(d.hid.as_deref(), Some("PNP0A03" | "PNP0A08")))
            .map(|d| d.path.clone())
            .collect()
    } else {
        let (pb, pd, pf) = bridge_for_bus(bus)?;
        if depth > 8 {
            return None;
        }
        alloc::vec![pci_path(pb, pd, pf, devices, depth + 1)?]
    };
    devices
        .iter()
        .find(|d| {
            d.adr == Some(want)
                && parents.iter().any(|p| {
                    d.path
                        .strip_prefix(p.as_str())
                        .is_some_and(|rest| rest.starts_with('.') && !rest[1..].contains('.'))
                })
        })
        .map(|d| d.path.clone())
}

static DEVICES: spin::Once<Vec<acpi::Device>> = spin::Once::new();

/// The ACPI path of PCI device `bus:dev.func` (following bridges), into
/// `buf`. Returns 0, -ENOENT or -ENAMETOOLONG.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_acpi_pci_path(
    bus: u8,
    dev: u8,
    func: u8,
    buf: *mut c_char,
    len: usize,
) -> c_int {
    let devices = DEVICES.call_once(acpi::devices);
    match pci_path(bus, dev, func, devices, 0) {
        Some(p) => copy_out(&p, buf, len),
        None => -ENOENT.0,
    }
}

/// Physical address and length of ACPI table `sig` (4 bytes), `instance`
/// counting from 1.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_acpi_table(
    sig: *const u8,
    instance: u32,
    phys: *mut u64,
    len: *mut u64,
) -> c_int {
    let sig: [u8; 4] = unsafe { core::slice::from_raw_parts(sig, 4) }
        .try_into()
        .unwrap();
    match acpi::table(&sig, instance.max(1) as usize) {
        Some((p, l)) => {
            unsafe {
                *phys = p;
                *len = l as u64;
            }
            0
        }
        None => -ENOENT.0,
    }
}
