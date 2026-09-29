//! AHCI (SATA) host controller driver.

use crate::block::{self, BlockDevice, DiskKind};
use crate::errno::*;
use crate::mm::dma::DmaBuffer;
use crate::pci::PciDevice;
use crate::sched::WaitQueue;
use crate::sched::mutex::Mutex;
use crate::time::Deadline;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

const CAP: usize = 0x00;
const GHC: usize = 0x04;
const IS: usize = 0x08;
const PI: usize = 0x0C;
const CAP2: usize = 0x24;
const BOHC: usize = 0x28;

const P_CLB: usize = 0x00;
const P_CLBU: usize = 0x04;
const P_FB: usize = 0x08;
const P_FBU: usize = 0x0C;
const P_IS: usize = 0x10;
const P_IE: usize = 0x14;
const P_CMD: usize = 0x18;
const P_TFD: usize = 0x20;
const P_SIG: usize = 0x24;
const P_SSTS: usize = 0x28;
const P_SERR: usize = 0x30;
const P_CI: usize = 0x38;

const CMD_ST: u32 = 1 << 0;
const CMD_SUD: u32 = 1 << 1;
const CMD_POD: u32 = 1 << 2;
const CMD_FRE: u32 = 1 << 4;
const CMD_FR: u32 = 1 << 14;
const CMD_CR: u32 = 1 << 15;

const ATA_READ_DMA_EXT: u8 = 0x25;
const ATA_WRITE_DMA_EXT: u8 = 0x35;
const ATA_FLUSH_CACHE_EXT: u8 = 0xEA;
const ATA_IDENTIFY: u8 = 0xEC;

/// Bounce buffer size per port (and max transfer per command).
const XFER: usize = 128 * 1024;

struct Hba {
    base: u64,
}

impl Hba {
    fn r(&self, off: usize) -> u32 {
        unsafe { core::ptr::read_volatile((self.base + off as u64) as *const u32) }
    }
    fn w(&self, off: usize, v: u32) {
        unsafe { core::ptr::write_volatile((self.base + off as u64) as *mut u32, v) }
    }
    fn pr(&self, port: usize, off: usize) -> u32 {
        self.r(0x100 + port * 0x80 + off)
    }
    fn pw(&self, port: usize, off: usize, v: u32) {
        self.w(0x100 + port * 0x80 + off, v)
    }
}

/// (port number, completion wait queue, pending flag) for the IRQ handler.
type PortWake = (usize, Arc<WaitQueue>, Arc<AtomicBool>);

struct PortState {
    /// Command list (32 headers), FIS receive area, one command table.
    clb: DmaBuffer,
    _fis: DmaBuffer,
    table: DmaBuffer,
    bounce: DmaBuffer,
}

pub struct AhciPort {
    hba: Arc<Hba>,
    port: usize,
    state: Mutex<PortState>,
    wq: Arc<WaitQueue>,
    irq: Arc<AtomicBool>,
    sectors: u64,
    sector_size: usize,
    model: String,
}

fn wait_clear(f: impl Fn() -> bool, ms: u64) -> bool {
    crate::time::wait_until(ms, || !f())
}

impl AhciPort {
    fn stop(hba: &Hba, p: usize) {
        let cmd = hba.pr(p, P_CMD);
        hba.pw(p, P_CMD, cmd & !CMD_ST);
        wait_clear(|| hba.pr(p, P_CMD) & CMD_CR != 0, 500);
        let cmd = hba.pr(p, P_CMD);
        hba.pw(p, P_CMD, cmd & !CMD_FRE);
        wait_clear(|| hba.pr(p, P_CMD) & CMD_FR != 0, 500);
    }

    fn start(hba: &Hba, p: usize) {
        wait_clear(|| hba.pr(p, P_CMD) & CMD_CR != 0, 500);
        let cmd = hba.pr(p, P_CMD);
        hba.pw(p, P_CMD, cmd | CMD_FRE | CMD_ST);
    }

    /// Issue one ATA command using slot 0. `data` is the transfer length in
    /// bytes (through the bounce buffer).
    fn command(
        &self,
        st: &mut PortState,
        cmd: u8,
        lba: u64,
        count: u16,
        bytes: usize,
        write: bool,
    ) -> KResult<()> {
        let hba = &self.hba;
        let p = self.port;
        // Wait for the device to be idle (BSY and DRQ clear).
        if !wait_clear(|| hba.pr(p, P_TFD) & 0x88 != 0, 2000) {
            return Err(EIO);
        }
        // Command header 0.
        let prdtl: u32 = if bytes > 0 { 1 } else { 0 };
        let flags = 5 | if write { 1 << 6 } else { 0 } | (prdtl << 16);
        st.clb.write::<u32>(0, flags);
        st.clb.write::<u32>(4, 0); // PRDBC
        st.clb.write::<u64>(8, st.table.phys());
        // Command FIS (Register H2D).
        st.table.zero();
        let fis: [u8; 20] = [
            0x27,
            0x80,
            cmd,
            0,
            lba as u8,
            (lba >> 8) as u8,
            (lba >> 16) as u8,
            0x40,
            (lba >> 24) as u8,
            (lba >> 32) as u8,
            (lba >> 40) as u8,
            0,
            count as u8,
            (count >> 8) as u8,
            0,
            0,
            0,
            0,
            0,
            0,
        ];
        for (i, b) in fis.iter().enumerate() {
            st.table.write::<u8>(i, *b);
        }
        if bytes > 0 {
            st.table.write::<u64>(0x80, st.bounce.phys());
            st.table.write::<u32>(0x88, 0);
            st.table.write::<u32>(0x8C, (bytes as u32 - 1) | (1 << 31));
        }
        hba.pw(p, P_SERR, u32::MAX);
        hba.pw(p, P_IS, u32::MAX);
        self.irq.store(false, Ordering::SeqCst);
        hba.pw(p, P_CI, 1);
        let deadline = Deadline::after_ms(10_000);
        loop {
            let done = hba.pr(p, P_CI) & 1 == 0;
            let err = hba.pr(p, P_IS) & (1 << 30) != 0;
            if err {
                crate::println!(
                    "[ahci] port {} error: TFD={:#x} SERR={:#x}",
                    p,
                    hba.pr(p, P_TFD),
                    hba.pr(p, P_SERR)
                );
                Self::stop(hba, p);
                hba.pw(p, P_SERR, u32::MAX);
                hba.pw(p, P_IS, u32::MAX);
                Self::start(hba, p);
                return Err(EIO);
            }
            if done {
                return if hba.pr(p, P_TFD) & 1 != 0 {
                    Err(EIO)
                } else {
                    Ok(())
                };
            }
            if deadline.expired() {
                crate::println!("[ahci] port {} command {:#x} timed out", p, cmd);
                return Err(EIO);
            }
            let irq = self.irq.clone();
            self.wq.wait_timeout(10, || {
                irq.load(Ordering::SeqCst) || hba.pr(p, P_CI) & 1 == 0
            });
        }
    }

    fn rw(&self, lba: u64, buf: &mut [u8], write_data: Option<&[u8]>) -> KResult<()> {
        let ss = self.sector_size;
        let total = buf.len().max(write_data.map_or(0, |d| d.len()));
        let mut done = 0;
        let mut st = self.state.lock();
        while done < total {
            let n = (total - done).min(XFER);
            let sectors = (n / ss) as u64;
            let lba_now = lba + (done / ss) as u64;
            if let Some(src) = write_data {
                st.bounce.as_mut_slice()[..n].copy_from_slice(&src[done..done + n]);
                self.command(&mut st, ATA_WRITE_DMA_EXT, lba_now, sectors as u16, n, true)?;
            } else {
                self.command(&mut st, ATA_READ_DMA_EXT, lba_now, sectors as u16, n, false)?;
                buf[done..done + n].copy_from_slice(&st.bounce.as_slice()[..n]);
            }
            done += n;
        }
        Ok(())
    }
}

impl BlockDevice for AhciPort {
    fn sector_size(&self) -> usize {
        self.sector_size
    }
    fn sector_count(&self) -> u64 {
        self.sectors
    }
    fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> KResult<()> {
        if lba + (buf.len() / self.sector_size) as u64 > self.sectors {
            return Err(EIO);
        }
        self.rw(lba, buf, None)
    }
    fn write_sectors(&self, lba: u64, buf: &[u8]) -> KResult<()> {
        if lba + (buf.len() / self.sector_size) as u64 > self.sectors {
            return Err(EIO);
        }
        let mut empty = [];
        self.rw(lba, &mut empty, Some(buf))
    }
    fn flush(&self) -> KResult<()> {
        let mut st = self.state.lock();
        self.command(&mut st, ATA_FLUSH_CACHE_EXT, 0, 0, 0, false)
    }
    fn model(&self) -> String {
        self.model.clone()
    }
}

fn ata_string(id: &[u8], from_word: usize, to_word: usize) -> String {
    let mut s = Vec::new();
    for w in from_word..to_word {
        s.push(id[w * 2 + 1]);
        s.push(id[w * 2]);
    }
    String::from_utf8_lossy(&s).trim().into()
}

pub fn probe(dev: &PciDevice) {
    dev.set_power_d0();
    dev.enable();
    let Some(base) = dev.map_bar(5) else {
        crate::println!("[ahci] {}: no ABAR", dev.name());
        return;
    };
    let hba = Arc::new(Hba { base });
    // BIOS/OS handoff.
    if hba.r(CAP2) & 1 != 0 {
        hba.w(BOHC, hba.r(BOHC) | 2);
        crate::time::wait_until(25, || hba.r(BOHC) & 1 == 0);
        crate::time::delay_us(10_000);
        if hba.r(BOHC) & 0x10 != 0 {
            crate::time::wait_until(2000, || hba.r(BOHC) & 0x10 == 0);
        }
    }
    hba.w(GHC, hba.r(GHC) | (1 << 31));
    let cap = hba.r(CAP);
    let pi = hba.r(PI);
    let staggered = cap & (1 << 27) != 0;

    // Interrupts: shared handler wakes every port's queue.
    let ports: Arc<crate::sync::Mutex<Vec<PortWake>>> =
        Arc::new(crate::sync::Mutex::new(Vec::new()));
    let (h2, p2) = (hba.clone(), ports.clone());
    let handler: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let is = h2.r(IS);
        for (p, wq, flag) in p2.lock().iter() {
            if is & (1 << p) != 0 {
                let pis = h2.pr(*p, P_IS);
                h2.pw(*p, P_IS, pis);
                flag.store(true, Ordering::SeqCst);
                wq.wake_all();
            }
        }
        h2.w(IS, is);
    });
    let irq_ok = dev.enable_irq(handler).is_some();
    if irq_ok {
        hba.w(GHC, hba.r(GHC) | 2);
    }

    for p in 0..32 {
        if pi & (1 << p) == 0 {
            continue;
        }
        if staggered {
            hba.pw(p, P_CMD, hba.pr(p, P_CMD) | CMD_SUD | CMD_POD);
        }
        let ssts = hba.pr(p, P_SSTS);
        if ssts & 0xF != 3 {
            continue;
        }
        let sig = hba.pr(p, P_SIG);
        if sig == 0xEB14_0101 {
            crate::println!(
                "[ahci] port {}: ATAPI device (optical drives are not supported)",
                p
            );
            continue;
        }
        if sig != 0x0000_0101 {
            continue;
        }
        AhciPort::stop(&hba, p);
        let (Some(clb), Some(fis), Some(table), Some(bounce)) = (
            DmaBuffer::new(1024),
            DmaBuffer::new(256),
            DmaBuffer::new(4096),
            DmaBuffer::new(XFER),
        ) else {
            continue;
        };
        hba.pw(p, P_CLB, clb.phys() as u32);
        hba.pw(p, P_CLBU, (clb.phys() >> 32) as u32);
        hba.pw(p, P_FB, fis.phys() as u32);
        hba.pw(p, P_FBU, (fis.phys() >> 32) as u32);
        hba.pw(p, P_SERR, u32::MAX);
        hba.pw(p, P_IS, u32::MAX);
        hba.pw(p, P_IE, if irq_ok { 0x7D80_00FF } else { 0 });
        AhciPort::start(&hba, p);

        let wq = Arc::new(WaitQueue::new());
        let flag = Arc::new(AtomicBool::new(false));
        x86_64::instructions::interrupts::without_interrupts(|| {
            ports.lock().push((p, wq.clone(), flag.clone()))
        });
        let mut port = AhciPort {
            hba: hba.clone(),
            port: p,
            state: Mutex::new(PortState {
                clb,
                _fis: fis,
                table,
                bounce,
            }),
            wq,
            irq: flag,
            sectors: 0,
            sector_size: 512,
            model: String::new(),
        };
        // IDENTIFY DEVICE.
        let id = {
            let mut st = port.state.lock();
            if port
                .command(&mut st, ATA_IDENTIFY, 0, 0, 512, false)
                .is_err()
            {
                crate::println!("[ahci] port {}: IDENTIFY failed", p);
                continue;
            }
            st.bounce.as_slice()[..512].to_vec()
        };
        let w = |i: usize| u16::from_le_bytes([id[i * 2], id[i * 2 + 1]]) as u64;
        let lba48 = w(83) & (1 << 10) != 0;
        port.sectors = if lba48 {
            w(100) | (w(101) << 16) | (w(102) << 32) | (w(103) << 48)
        } else {
            w(60) | (w(61) << 16)
        };
        if w(106) & 0xC000 == 0x4000 && w(106) & (1 << 12) != 0 {
            port.sector_size = ((w(117) | (w(118) << 16)) * 2) as usize;
        }
        port.model = ata_string(&id, 27, 47);
        if port.sectors == 0 {
            continue;
        }
        block::register_disk(Arc::new(port), DiskKind::Scsi, 0);
    }
}
