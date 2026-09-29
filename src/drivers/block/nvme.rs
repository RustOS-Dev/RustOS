//! NVMe controller driver.
//!
//! One admin queue pair and one I/O queue pair per controller. Commands are
//! issued one at a time under a mutex; completions are signalled by MSI-X
//! (or MSI/INTx) and detected by the CQ phase bit, with polling as a
//! fallback. Transfers go through a DMA bounce buffer described by a PRP
//! list.

use crate::block::{self, BlockDevice, DiskKind};
use crate::errno::*;
use crate::mm::dma::DmaBuffer;
use crate::pci::PciDevice;
use crate::sched::WaitQueue;
use crate::sched::mutex::Mutex;
use crate::time::Deadline;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

const REG_CAP: usize = 0x00;
const REG_VS: usize = 0x08;
const REG_CC: usize = 0x14;
const REG_CSTS: usize = 0x1C;
const REG_AQA: usize = 0x24;
const REG_ASQ: usize = 0x28;
const REG_ACQ: usize = 0x30;

const ADMIN_CREATE_SQ: u8 = 0x01;
const ADMIN_CREATE_CQ: u8 = 0x05;
const ADMIN_IDENTIFY: u8 = 0x06;
const ADMIN_SET_FEATURES: u8 = 0x09;
const IO_FLUSH: u8 = 0x00;
const IO_WRITE: u8 = 0x01;
const IO_READ: u8 = 0x02;

const QUEUE_DEPTH: u16 = 64;
const XFER: usize = 128 * 1024;

struct Queue {
    sq: DmaBuffer,
    cq: DmaBuffer,
    size: u16,
    sq_tail: u16,
    cq_head: u16,
    phase: bool,
    qid: u16,
    cid: u16,
}

struct Io {
    admin: Queue,
    io: Option<Queue>,
    bounce: DmaBuffer,
    prp_list: DmaBuffer,
}

pub struct Controller {
    base: u64,
    dstrd: u32,
    io: Mutex<Io>,
    wq: Arc<WaitQueue>,
    timeout_ms: u64,
}

impl Controller {
    fn r32(&self, off: usize) -> u32 {
        unsafe { core::ptr::read_volatile((self.base + off as u64) as *const u32) }
    }
    fn w32(&self, off: usize, v: u32) {
        unsafe { core::ptr::write_volatile((self.base + off as u64) as *mut u32, v) }
    }
    fn r64(&self, off: usize) -> u64 {
        self.r32(off) as u64 | ((self.r32(off + 4) as u64) << 32)
    }
    fn w64(&self, off: usize, v: u64) {
        self.w32(off, v as u32);
        self.w32(off + 4, (v >> 32) as u32);
    }
    fn sq_doorbell(&self, qid: u16) -> usize {
        0x1000 + (2 * qid as usize) * (4 << self.dstrd)
    }
    fn cq_doorbell(&self, qid: u16) -> usize {
        0x1000 + (2 * qid as usize + 1) * (4 << self.dstrd)
    }

    /// Submit a 64-byte command and wait for its completion. Returns
    /// (status, dword0).
    fn submit(&self, q: &mut Queue, mut cmd: [u32; 16]) -> KResult<u32> {
        q.cid = q.cid.wrapping_add(1);
        cmd[0] = (cmd[0] & 0xFFFF) | ((q.cid as u32) << 16);
        let slot = q.sq_tail as usize * 64;
        for (i, w) in cmd.iter().enumerate() {
            q.sq.write::<u32>(slot + i * 4, *w);
        }
        q.sq_tail = (q.sq_tail + 1) % q.size;
        self.w32(self.sq_doorbell(q.qid), q.sq_tail as u32);
        let deadline = Deadline::after_ms(self.timeout_ms);
        loop {
            let e = q.cq_head as usize * 16;
            let status = q.cq.read::<u32>(e + 12);
            if (status >> 16) & 1 == q.phase as u32 {
                let dw0 = q.cq.read::<u32>(e);
                q.cq_head = (q.cq_head + 1) % q.size;
                if q.cq_head == 0 {
                    q.phase = !q.phase;
                }
                self.w32(self.cq_doorbell(q.qid), q.cq_head as u32);
                let sc = (status >> 17) & 0x7FF;
                if sc != 0 {
                    return Err(EIO);
                }
                return Ok(dw0);
            }
            if deadline.expired() {
                crate::println!("[nvme] command {:#x} timed out", cmd[0] & 0xFF);
                return Err(EIO);
            }
            let (cq, head, phase) = (&q.cq, q.cq_head, q.phase);
            self.wq.wait_timeout(5, || {
                (cq.read::<u32>(head as usize * 16 + 12) >> 16) & 1 == phase as u32
            });
        }
    }

    fn admin(&self, io: &mut Io, cmd: [u32; 16]) -> KResult<u32> {
        self.submit(&mut io.admin, cmd)
    }

    /// Fill PRP entries for `len` bytes of the bounce buffer.
    fn prps(io: &Io, len: usize) -> (u64, u64) {
        let base = io.bounce.phys();
        let pages = len.div_ceil(4096);
        match pages {
            0 | 1 => (base, 0),
            2 => (base, base + 4096),
            _ => {
                for i in 1..pages {
                    io.prp_list
                        .write::<u64>((i - 1) * 8, base + i as u64 * 4096);
                }
                (base, io.prp_list.phys())
            }
        }
    }
}

fn new_queue(qid: u16, size: u16) -> Option<Queue> {
    Some(Queue {
        sq: DmaBuffer::new(size as usize * 64)?,
        cq: DmaBuffer::new(size as usize * 16)?,
        size,
        sq_tail: 0,
        cq_head: 0,
        phase: true,
        qid,
        cid: 0,
    })
}

pub struct Namespace {
    ctrl: Arc<Controller>,
    nsid: u32,
    sectors: u64,
    sector_size: usize,
    model: String,
}

impl Namespace {
    fn rw(&self, write: bool, lba: u64, buf: &mut [u8], src: Option<&[u8]>) -> KResult<()> {
        let ss = self.sector_size;
        let total = if write {
            src.map_or(0, |s| s.len())
        } else {
            buf.len()
        };
        let mut io = self.ctrl.io.lock();
        let mut done = 0;
        while done < total {
            let n = (total - done).min(XFER);
            let nlb = (n / ss) as u32;
            let slba = lba + (done / ss) as u64;
            if let Some(s) = src {
                io.bounce.as_mut_slice()[..n].copy_from_slice(&s[done..done + n]);
            }
            let (p1, p2) = Controller::prps(&io, n);
            let mut cmd = [0u32; 16];
            cmd[0] = if write { IO_WRITE } else { IO_READ } as u32;
            cmd[1] = self.nsid;
            cmd[6] = p1 as u32;
            cmd[7] = (p1 >> 32) as u32;
            cmd[8] = p2 as u32;
            cmd[9] = (p2 >> 32) as u32;
            cmd[10] = slba as u32;
            cmd[11] = (slba >> 32) as u32;
            cmd[12] = nlb - 1;
            let io_ref = &mut *io;
            let q = io_ref.io.as_mut().ok_or(EIO)?;
            self.ctrl.submit(q, cmd)?;
            if !write {
                buf[done..done + n].copy_from_slice(&io.bounce.as_slice()[..n]);
            }
            done += n;
        }
        Ok(())
    }
}

impl BlockDevice for Namespace {
    fn sector_size(&self) -> usize {
        self.sector_size
    }
    fn sector_count(&self) -> u64 {
        self.sectors
    }
    fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> KResult<()> {
        self.rw(false, lba, buf, None)
    }
    fn write_sectors(&self, lba: u64, buf: &[u8]) -> KResult<()> {
        let mut empty = [];
        self.rw(true, lba, &mut empty, Some(buf))
    }
    fn flush(&self) -> KResult<()> {
        let mut io = self.ctrl.io.lock();
        let mut cmd = [0u32; 16];
        cmd[0] = IO_FLUSH as u32;
        cmd[1] = self.nsid;
        let q = io.io.as_mut().ok_or(EIO)?;
        self.ctrl.submit(q, cmd).map(|_| ())
    }
    fn model(&self) -> String {
        self.model.clone()
    }
}

static CONTROLLERS: crate::sync::Mutex<Vec<Arc<Controller>>> = crate::sync::Mutex::new(Vec::new());

/// Notify every controller of an orderly shutdown (CC.SHN = normal).
pub fn shutdown_all() {
    for c in CONTROLLERS.lock().iter() {
        let cc = c.r32(REG_CC);
        c.w32(REG_CC, (cc & !(3 << 14)) | (1 << 14));
        crate::time::wait_until(1000, || (c.r32(REG_CSTS) >> 2) & 3 == 2);
    }
}

pub fn probe(dev: &PciDevice) {
    dev.set_power_d0();
    dev.enable();
    let Some(base) = dev.map_bar(0) else {
        return;
    };
    let wq = Arc::new(WaitQueue::new());
    let mut ctrl = Controller {
        base,
        dstrd: 0,
        io: Mutex::new(Io {
            admin: match new_queue(0, 32) {
                Some(q) => q,
                None => return,
            },
            io: None,
            bounce: match DmaBuffer::new(XFER) {
                Some(b) => b,
                None => return,
            },
            prp_list: match DmaBuffer::new(4096) {
                Some(b) => b,
                None => return,
            },
        }),
        wq: wq.clone(),
        timeout_ms: 5000,
    };
    let cap = ctrl.r64(REG_CAP);
    ctrl.dstrd = ((cap >> 32) & 0xF) as u32;
    let mqes = (cap & 0xFFFF) as u16 + 1;
    let to_ms = ((cap >> 24) & 0xFF) * 500;
    ctrl.timeout_ms = to_ms.max(5000);
    let vs = ctrl.r32(REG_VS);

    // Reset.
    ctrl.w32(REG_CC, ctrl.r32(REG_CC) & !1);
    if !crate::time::wait_until(to_ms.max(1000), || ctrl.r32(REG_CSTS) & 1 == 0) {
        crate::println!("[nvme] {}: reset timed out", dev.name());
        return;
    }
    {
        let io = ctrl.io.lock();
        let a = &io.admin;
        ctrl.w32(REG_AQA, ((a.size as u32 - 1) << 16) | (a.size as u32 - 1));
        ctrl.w64(REG_ASQ, a.sq.phys());
        ctrl.w64(REG_ACQ, a.cq.phys());
    }
    // 4 KiB pages, NVM command set, 64-byte SQEs, 16-byte CQEs, enable.
    ctrl.w32(REG_CC, (6 << 16) | (4 << 20) | 1);
    if !crate::time::wait_until(to_ms.max(1000), || ctrl.r32(REG_CSTS) & 1 == 1) {
        crate::println!(
            "[nvme] {}: enable timed out (CSTS={:#x})",
            dev.name(),
            ctrl.r32(REG_CSTS)
        );
        return;
    }

    let w = wq.clone();
    let irq = dev.enable_irq(Arc::new(move || w.wake_all()));

    let ctrl = Arc::new(ctrl);
    let (model, nn) = {
        let mut io = ctrl.io.lock();
        let mut cmd = [0u32; 16];
        cmd[0] = ADMIN_IDENTIFY as u32;
        cmd[6] = io.bounce.phys() as u32;
        cmd[7] = (io.bounce.phys() >> 32) as u32;
        cmd[10] = 1; // identify controller
        if ctrl.admin(&mut io, cmd).is_err() {
            crate::println!("[nvme] {}: identify failed", dev.name());
            return;
        }
        let id = io.bounce.as_slice()[..4096].to_vec();
        let model: String = String::from_utf8_lossy(&id[24..64]).trim().into();
        let nn = u32::from_le_bytes(id[516..520].try_into().unwrap());

        // One I/O queue pair.
        let mut sf = [0u32; 16];
        sf[0] = ADMIN_SET_FEATURES as u32;
        sf[10] = 7;
        sf[11] = 0;
        let _ = ctrl.admin(&mut io, sf);
        let depth = QUEUE_DEPTH.min(mqes);
        let Some(q) = new_queue(1, depth) else { return };
        let mut ccq = [0u32; 16];
        ccq[0] = ADMIN_CREATE_CQ as u32;
        ccq[6] = q.cq.phys() as u32;
        ccq[7] = (q.cq.phys() >> 32) as u32;
        ccq[10] = ((depth as u32 - 1) << 16) | 1;
        ccq[11] = 1 | if irq.is_some() { 2 } else { 0 };
        if ctrl.admin(&mut io, ccq).is_err() {
            crate::println!("[nvme] {}: create CQ failed", dev.name());
            return;
        }
        let mut csq = [0u32; 16];
        csq[0] = ADMIN_CREATE_SQ as u32;
        csq[6] = q.sq.phys() as u32;
        csq[7] = (q.sq.phys() >> 32) as u32;
        csq[10] = ((depth as u32 - 1) << 16) | 1;
        csq[11] = (1 << 16) | 1;
        if ctrl.admin(&mut io, csq).is_err() {
            crate::println!("[nvme] {}: create SQ failed", dev.name());
            return;
        }
        io.io = Some(q);
        (model, nn)
    };
    crate::println!(
        "[nvme] {}: {} (NVMe {}.{}, {} namespace(s))",
        dev.name(),
        model,
        vs >> 16,
        (vs >> 8) & 0xFF,
        nn
    );
    CONTROLLERS.lock().push(ctrl.clone());
    let index = block::next_nvme_index();
    for nsid in 1..=nn.min(16) {
        let ns = {
            let mut io = ctrl.io.lock();
            let mut cmd = [0u32; 16];
            cmd[0] = ADMIN_IDENTIFY as u32;
            cmd[1] = nsid;
            cmd[6] = io.bounce.phys() as u32;
            cmd[7] = (io.bounce.phys() >> 32) as u32;
            cmd[10] = 0; // identify namespace
            if ctrl.admin(&mut io, cmd).is_err() {
                continue;
            }
            let id = io.bounce.as_slice()[..4096].to_vec();
            let nsze = u64::from_le_bytes(id[0..8].try_into().unwrap());
            let flbas = (id[26] & 0xF) as usize;
            let lbaf = u32::from_le_bytes(id[128 + flbas * 4..132 + flbas * 4].try_into().unwrap());
            let lbads = (lbaf >> 16) & 0xFF;
            (nsze, 1usize << lbads)
        };
        if ns.0 == 0 || ns.1 < 512 {
            continue;
        }
        let dev = Namespace {
            ctrl: ctrl.clone(),
            nsid,
            sectors: ns.0,
            sector_size: ns.1,
            model: format!("{} ns{}", model, nsid),
        };
        block::register_disk(Arc::new(dev), DiskKind::Nvme(index), nsid);
    }
}
