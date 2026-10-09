//! QEMU's firmware configuration device (fw_cfg, I/O ports 0x510/0x511),
//! through which the host hands files to the guest:
//! `-fw_cfg name=opt/rustos/kernel.conf,string=desktop=none`. Probed only
//! under a hypervisor (CPUID.1:ECX bit 31): on real machines the ports
//! belong to nothing.

use alloc::vec;
use alloc::vec::Vec;
use x86_64::instructions::port::Port;

const SELECTOR: u16 = 0x510;
const DATA: u16 = 0x511;
const KEY_SIGNATURE: u16 = 0x0000;
const KEY_FILE_DIR: u16 = 0x0019;

fn hypervisor() -> bool {
    let r = core::arch::x86_64::__cpuid(1);
    r.ecx & (1 << 31) != 0
}

fn select(key: u16) {
    unsafe { Port::<u16>::new(SELECTOR).write(key) };
}

fn read(buf: &mut [u8]) {
    let mut port = Port::<u8>::new(DATA);
    for b in buf {
        *b = unsafe { port.read() };
    }
}

/// True when running on QEMU with fw_cfg.
pub fn present() -> bool {
    if !hypervisor() {
        return false;
    }
    select(KEY_SIGNATURE);
    let mut sig = [0u8; 4];
    read(&mut sig);
    &sig == b"QEMU"
}

/// The contents of the named fw_cfg file (`opt/...`), if the host gave one.
pub fn read_file(name: &str) -> Option<Vec<u8>> {
    if !present() {
        return None;
    }
    select(KEY_FILE_DIR);
    let mut count = [0u8; 4];
    read(&mut count);
    // Directory entries: size (BE u32), selector (BE u16), reserved, name[56].
    for _ in 0..u32::from_be_bytes(count).min(4096) {
        let mut e = [0u8; 64];
        read(&mut e);
        let n = &e[8..];
        let len = n.iter().position(|&b| b == 0).unwrap_or(n.len());
        if &n[..len] == name.as_bytes() {
            let size = u32::from_be_bytes([e[0], e[1], e[2], e[3]]) as usize;
            select(u16::from_be_bytes([e[4], e[5]]));
            let mut data = vec![0u8; size.min(64 * 1024)];
            read(&mut data);
            return Some(data);
        }
    }
    None
}
