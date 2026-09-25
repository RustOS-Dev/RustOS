//! virtio block device driver.

use crate::block::{self, BlockDevice, DiskKind};
use crate::drivers::virtio::{VirtioPci, Virtqueue};
use crate::errno::*;
use crate::mm::dma::DmaBuffer;
use crate::pci::PciDevice;
use crate::sched::WaitQueue;
use crate::sched::mutex::Mutex;
use crate::time::Deadline;
use alloc::string::String;
use alloc::sync::Arc;

const F_RO: u64 = 1 << 5;
const F_BLK_SIZE: u64 = 1 << 6;
const F_FLUSH: u64 = 1 << 9;

const T_IN: u32 = 0;
const T_OUT: u32 = 1;
const T_FLUSH: u32 = 4;

const XFER: usize = 128 * 1024;

struct State {
    vq: Virtqueue,
    /// header (16 bytes) | status (1 byte) at 512 | data at 4096
    buf: DmaBuffer,
}

pub struct VirtioBlk {
    state: Mutex<State>,
    wq: Arc<WaitQueue>,
    sectors: u64,
    sector_size: usize,
    read_only: bool,
    flush: bool,
    _pci: VirtioPci,
}

impl VirtioBlk {
    fn request(&self, st: &mut State, kind: u32, sector: u64, len: usize) -> KResult<()> {
        st.buf.write::<u32>(0, kind);
        st.buf.write::<u32>(4, 0);
        st.buf.write::<u64>(8, sector);
        st.buf.write::<u8>(512, 0xFF);
        let phys = st.buf.phys();
        let mut chain: [(u64, u32, bool); 3] = [
            (phys, 16, false),
            (phys + 4096, len as u32, kind == T_IN),
            (phys + 512, 1, true),
        ];
        let bufs: &[(u64, u32, bool)] = if len == 0 {
            chain[1] = chain[2];
            &chain[..2]
        } else {
            &chain
        };
        st.vq.submit(bufs).ok_or(EIO)?;
        st.vq.notify();
        let deadline = Deadline::after_ms(10_000);
        while !st.vq.has_used() {
            if deadline.expired() {
                return Err(EIO);
            }
            let vq = &st.vq;
            self.wq.wait_timeout(5, || vq.has_used());
        }
        st.vq.pop_used();
        match st.buf.read::<u8>(512) {
            0 => Ok(()),
            2 => Err(EOPNOTSUPP),
            _ => Err(EIO),
        }
    }
}

impl BlockDevice for VirtioBlk {
    fn sector_size(&self) -> usize {
        self.sector_size
    }
    fn sector_count(&self) -> u64 {
        self.sectors
    }
    fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> KResult<()> {
        let mut st = self.state.lock();
        let mut done = 0;
        while done < buf.len() {
            let n = (buf.len() - done).min(XFER);
            // virtio sectors are always 512 bytes.
            let sector = (lba * self.sector_size as u64 + done as u64) / 512;
            self.request(&mut st, T_IN, sector, n)?;
            buf[done..done + n].copy_from_slice(&st.buf.as_slice()[4096..4096 + n]);
            done += n;
        }
        Ok(())
    }
    fn write_sectors(&self, lba: u64, buf: &[u8]) -> KResult<()> {
        if self.read_only {
            return Err(EROFS);
        }
        let mut st = self.state.lock();
        let mut done = 0;
        while done < buf.len() {
            let n = (buf.len() - done).min(XFER);
            let sector = (lba * self.sector_size as u64 + done as u64) / 512;
            st.buf.as_mut_slice()[4096..4096 + n].copy_from_slice(&buf[done..done + n]);
            self.request(&mut st, T_OUT, sector, n)?;
            done += n;
        }
        Ok(())
    }
    fn flush(&self) -> KResult<()> {
        if !self.flush {
            return Ok(());
        }
        let mut st = self.state.lock();
        self.request(&mut st, T_FLUSH, 0, 0)
    }
    fn read_only(&self) -> bool {
        self.read_only
    }
    fn model(&self) -> String {
        String::from("virtio block device")
    }
}

pub fn probe(dev: &PciDevice) {
    let Some(v) = VirtioPci::new(dev) else {
        crate::println!("[virtio-blk] {}: no modern virtio interface", dev.name());
        return;
    };
    let Some(features) = v.negotiate(F_RO | F_BLK_SIZE | F_FLUSH) else {
        crate::println!("[virtio-blk] {}: feature negotiation failed", dev.name());
        return;
    };
    let wq = Arc::new(WaitQueue::new());
    let w = wq.clone();
    let msix = if dev.msix_count().is_some() {
        v.set_config_msix(0xFFFF);
        dev.enable_msix(alloc::vec![alloc::boxed::Box::new(move || w.wake_all())])
            .map(|_| 0u16)
    } else {
        dev.enable_irq(Arc::new(move || w.wake_all()));
        None
    };
    let Some(vq) = v.setup_queue(0, 128, msix) else {
        return;
    };
    let Some(buf) = DmaBuffer::new(4096 + XFER) else {
        return;
    };
    v.driver_ok();
    let sectors512 = v.cfg_r64(0);
    let sector_size = if features & F_BLK_SIZE != 0 {
        (v.cfg_r32(20) as usize).max(512)
    } else {
        512
    };
    let blk = VirtioBlk {
        state: Mutex::new(State { vq, buf }),
        wq,
        sectors: sectors512 * 512 / sector_size as u64,
        sector_size,
        read_only: features & F_RO != 0,
        flush: features & F_FLUSH != 0,
        _pci: v,
    };
    block::register_disk(Arc::new(blk), DiskKind::Virtio, 0);
}
