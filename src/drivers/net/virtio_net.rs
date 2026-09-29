//! virtio network device (modern virtio-pci).

use crate::drivers::virtio::{VirtioPci, Virtqueue};
use crate::errno::*;
use crate::mm::dma::DmaBuffer;
use crate::net::{self, NetDevice};
use crate::pci::PciDevice;
use crate::sync::Mutex;
use alloc::sync::Arc;
use alloc::vec::Vec;

const F_MAC: u64 = 1 << 5;
const F_STATUS: u64 = 1 << 16;

/// virtio_net_hdr (with num_buffers, always present in virtio 1.0).
const HDR: usize = 12;
const BUF: usize = 2048;
const RX_BUFS: usize = 128;
const TX_BUFS: usize = 128;

struct Rx {
    vq: Virtqueue,
    mem: DmaBuffer,
    /// Descriptor head id -> buffer slot.
    slot_of: Vec<u16>,
}

struct Tx {
    vq: Virtqueue,
    mem: DmaBuffer,
    free: Vec<usize>,
    slot_of: Vec<u16>,
}

pub struct VirtioNet {
    pci: VirtioPci,
    mac: [u8; 6],
    status: bool,
    rx: Mutex<Rx>,
    tx: Mutex<Tx>,
}

impl VirtioNet {
    fn post_rx(rx: &mut Rx, slot: usize) {
        let phys = rx.mem.phys() + (slot * BUF) as u64;
        if let Some(id) = rx.vq.submit(&[(phys, BUF as u32, true)]) {
            rx.slot_of[id as usize] = slot as u16;
        }
    }

    fn reclaim_tx(tx: &mut Tx) {
        while let Some((id, _)) = tx.vq.pop_used() {
            let slot = tx.slot_of[id as usize] as usize;
            tx.free.push(slot);
        }
    }
}

impl NetDevice for VirtioNet {
    fn shutdown(&self) {
        self.pci.reset();
    }
    fn mac(&self) -> [u8; 6] {
        self.mac
    }

    fn link_up(&self) -> bool {
        !self.status || self.pci.cfg_r16(6) & 1 != 0
    }

    fn driver(&self) -> &'static str {
        "virtio-net"
    }

    fn transmit(&self, frame: &[u8]) -> KResult<()> {
        if frame.len() > BUF - HDR {
            return Err(EMSGSIZE);
        }
        let mut tx = self.tx.lock();
        Self::reclaim_tx(&mut tx);
        let slot = tx.free.pop().ok_or(EAGAIN)?;
        let off = slot * BUF;
        let buf = &mut tx.mem.as_mut_slice()[off..off + HDR + frame.len()];
        buf[..HDR].fill(0);
        buf[HDR..].copy_from_slice(frame);
        let phys = tx.mem.phys() + off as u64;
        let len = (HDR + frame.len()) as u32;
        match tx.vq.submit(&[(phys, len, false)]) {
            Some(id) => {
                tx.slot_of[id as usize] = slot as u16;
                tx.vq.notify();
                Ok(())
            }
            None => {
                tx.free.push(slot);
                Err(EAGAIN)
            }
        }
    }

    fn receive(&self) -> Option<Vec<u8>> {
        let mut rx = self.rx.lock();
        let (id, len) = rx.vq.pop_used()?;
        let slot = rx.slot_of[id as usize] as usize;
        let off = slot * BUF;
        let len = (len as usize).clamp(HDR, BUF);
        let frame = rx.mem.as_slice()[off + HDR..off + len].to_vec();
        Self::post_rx(&mut rx, slot);
        rx.vq.notify();
        Some(frame)
    }
}

pub fn probe(dev: &PciDevice) {
    let Some(v) = VirtioPci::new(dev) else {
        return;
    };
    let Some(features) = v.negotiate(F_MAC | F_STATUS) else {
        crate::println!("[virtio-net] {}: feature negotiation failed", dev.name());
        return;
    };
    let msix = if dev.msix_count().is_some() {
        v.set_config_msix(0);
        dev.enable_msix(alloc::vec![alloc::boxed::Box::new(net::kick)])
            .map(|_| 0u16)
    } else {
        dev.enable_irq(Arc::new(net::kick));
        None
    };
    let (Some(rxq), Some(txq)) = (
        v.setup_queue(0, RX_BUFS as u16, msix),
        v.setup_queue(1, TX_BUFS as u16, msix),
    ) else {
        return;
    };
    let (Some(rx_mem), Some(tx_mem)) =
        (DmaBuffer::new(RX_BUFS * BUF), DmaBuffer::new(TX_BUFS * BUF))
    else {
        return;
    };
    let mut mac = [0u8; 6];
    if features & F_MAC != 0 {
        for (i, m) in mac.iter_mut().enumerate() {
            *m = v.cfg_r8(i as u64);
        }
    } else {
        crate::drivers::random::fill(&mut mac);
        mac[0] = (mac[0] & 0xFE) | 0x02;
    }
    let rx_n = rxq.size as usize;
    let tx_n = txq.size as usize;
    let mut rx = Rx {
        vq: rxq,
        mem: rx_mem,
        slot_of: alloc::vec![0; rx_n],
    };
    for slot in 0..rx_n.min(RX_BUFS) {
        VirtioNet::post_rx(&mut rx, slot);
    }
    v.driver_ok();
    rx.vq.notify();
    let nic = Arc::new(VirtioNet {
        pci: v,
        mac,
        status: features & F_STATUS != 0,
        rx: Mutex::new(rx),
        tx: Mutex::new(Tx {
            vq: txq,
            mem: tx_mem,
            free: (0..tx_n.min(TX_BUFS)).collect(),
            slot_of: alloc::vec![0; tx_n],
        }),
    });
    net::register(nic);
}
