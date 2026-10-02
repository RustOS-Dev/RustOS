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

fn id_string(v: &Value) -> Option<String> {
    match v {
        Value::Integer(i) => Some(acpi::eisa_id(*i)),
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// Every device in the namespace for Linux's ACPI device objects
/// (c/acpi.c): `cb(ctx, path, hid, cids, uid, sta, adr, has_adr)` per
/// device, parents before children. `cids` is `_CID` joined with commas;
/// strings are empty when the object is absent; `sta` is `_STA` (0xf when
/// absent).
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_acpi_for_each_device(
    cb: extern "C" fn(
        *mut c_void,
        *const c_char,
        *const c_char,
        *const c_char,
        *const c_char,
        u32,
        u64,
        c_int,
    ),
    ctx: *mut c_void,
) {
    let devices = DEVICES.call_once(acpi::devices);
    for d in devices {
        let cids = match acpi::eval(&alloc::format!("{}._CID", d.path), &[]) {
            Ok(Value::Package(p)) => p.iter().filter_map(id_string).collect::<Vec<_>>(),
            Ok(v) => id_string(&v).into_iter().collect(),
            Err(_) => Vec::new(),
        }
        .join(",");
        let uid = match acpi::eval(&alloc::format!("{}._UID", d.path), &[]) {
            Ok(Value::Integer(i)) => alloc::format!("{i}"),
            Ok(Value::String(s)) => s,
            _ => String::new(),
        };
        let sta = match acpi::eval(&alloc::format!("{}._STA", d.path), &[]) {
            Ok(Value::Integer(i)) => i as u32,
            _ => 0xf,
        };
        let z = |s: &str| {
            let mut v = Vec::from(s.as_bytes());
            v.retain(|&b| b != 0);
            v.push(0);
            v
        };
        let (path, hid, cids, uid) = (
            z(&d.path),
            z(d.hid.as_deref().unwrap_or("")),
            z(&cids),
            z(&uid),
        );
        cb(
            ctx,
            path.as_ptr() as *const c_char,
            hid.as_ptr() as *const c_char,
            cids.as_ptr() as *const c_char,
            uid.as_ptr() as *const c_char,
            sta,
            d.adr.unwrap_or(0),
            d.adr.is_some() as c_int,
        );
    }
}

struct SendPtr(*mut c_void);
unsafe impl Send for SendPtr {}
unsafe impl Sync for SendPtr {}

/// Route global system interrupt `gsi` to `f(arg)` (interrupt context),
/// sharing the line with other users. Returns the vector or -1.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_gsi_request(
    gsi: u32,
    level: c_int,
    active_low: c_int,
    f: extern "C" fn(*mut c_void),
    arg: *mut c_void,
) -> c_int {
    let arg = SendPtr(arg);
    crate::pci::request_gsi(
        gsi,
        level != 0,
        active_low != 0,
        alloc::boxed::Box::new(move || {
            let a = &arg;
            f(a.0)
        }),
    )
    .map(|v| v as c_int)
    .unwrap_or(-1)
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_gsi_mask(gsi: u32, masked: c_int) {
    crate::arch::x86_64::apic::set_gsi_masked(gsi, masked != 0);
}

/// The GSI and polarity/trigger for ISA IRQ `irq` (MADT source overrides).
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_isa_irq(irq: u32, gsi: *mut u32, level: *mut c_int, low: *mut c_int) {
    let ov = acpi::platform()
        .isa_overrides
        .iter()
        .find(|o| o.isa_irq as u32 == irq)
        .copied();
    let (g, l, a) = match ov {
        Some(o) => (o.gsi, o.level, o.active_low),
        None => (irq, false, false),
    };
    unsafe {
        *gsi = g;
        *level = l as c_int;
        *low = a as c_int;
    }
}
