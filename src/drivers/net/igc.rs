//! Intel I225/I226 2.5 GbE (igc), one queue pair with advanced descriptors.

use crate::errno::*;
use crate::mm::dma::DmaBuffer;
use crate::net::{self, NetDevice};
use crate::pci::PciDevice;
use crate::sync::Mutex;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

const CTRL: u64 = 0x0000;
const STATUS: u64 = 0x0008;
const CTRL_EXT: u64 = 0x0018;
const RCTL: u64 = 0x0100;
const TCTL: u64 = 0x0400;
const ICR: u64 = 0x1500;
const IMS: u64 = 0x1508;
const IMC: u64 = 0x150C;
const EIMC: u64 = 0x1528;
const RAL0: u64 = 0x5400;
const RAH0: u64 = 0x5404;
const MTA: u64 = 0x5200;
const RDBAL: u64 = 0xC000;
const RDBAH: u64 = 0xC004;
const RDLEN: u64 = 0xC008;
const SRRCTL: u64 = 0xC00C;
const RDH: u64 = 0xC010;
const RDT: u64 = 0xC018;
const RXDCTL: u64 = 0xC028;
const TDBAL: u64 = 0xE000;
const TDBAH: u64 = 0xE004;
const TDLEN: u64 = 0xE008;
const TDH: u64 = 0xE010;
const TDT: u64 = 0xE018;
const TXDCTL: u64 = 0xE028;

const CTRL_SLU: u32 = 1 << 6;
const CTRL_DEV_RST: u32 = 1 << 29;
const CTRL_EXT_DRV_LOAD: u32 = 1 << 28;
const STATUS_LU: u32 = 1 << 1;
const STATUS_2500: u32 = 1 << 22;
const QUEUE_ENABLE: u32 = 1 << 25;

const RX_N: usize = 256;
const TX_N: usize = 256;
const BUF: usize = 2048;

struct Rings {
    rx: DmaBuffer,
    rx_bufs: DmaBuffer,
    rx_next: usize,
    tx: DmaBuffer,
    tx_bufs: DmaBuffer,
    tx_next: usize,
    tx_clean: usize,
}

pub struct Igc {
    mmio: u64,
    mac: [u8; 6],
    rings: Mutex<Rings>,
}

fn r32(a: u64) -> u32 {
    unsafe { core::ptr::read_volatile(a as *const u32) }
}
fn w32(a: u64, v: u32) {
    unsafe { core::ptr::write_volatile(a as *mut u32, v) }
}

impl Igc {
    fn r(&self, reg: u64) -> u32 {
        r32(self.mmio + reg)
    }
    fn w(&self, reg: u64, v: u32) {
        w32(self.mmio + reg, v)
    }
}

impl NetDevice for Igc {
    fn shutdown(&self) {
        self.w(RCTL, 0);
        self.w(TCTL, 0);
        self.w(CTRL, self.r(CTRL) | CTRL_DEV_RST);
    }
    fn mac(&self) -> [u8; 6] {
        self.mac
    }
    fn link_up(&self) -> bool {
        self.r(STATUS) & STATUS_LU != 0
    }
    fn speed(&self) -> Option<u32> {
        let st = self.r(STATUS);
        if st & STATUS_LU == 0 {
            return None;
        }
        Some(if st & STATUS_2500 != 0 {
            2500
        } else {
            match (st >> 6) & 3 {
                0 => 10,
                1 => 100,
                _ => 1000,
            }
        })
    }
    fn driver(&self) -> &'static str {
        "igc"
    }

    fn transmit(&self, frame: &[u8]) -> KResult<()> {
        if frame.len() > BUF {
            return Err(EMSGSIZE);
        }
        let mut r = self.rings.lock();
        while r.tx_clean != r.tx_next && r.tx.read::<u32>(r.tx_clean * 16 + 12) & 1 != 0 {
            r.tx_clean = (r.tx_clean + 1) % TX_N;
        }
        let i = r.tx_next;
        if (i + 1) % TX_N == r.tx_clean {
            return Err(EAGAIN);
        }
        let off = i * BUF;
        r.tx_bufs.as_mut_slice()[off..off + frame.len()].copy_from_slice(frame);
        let len = frame.len().max(60) as u32;
        let d = i * 16;
        let addr = r.tx_bufs.phys() + off as u64;
        r.tx.write::<u64>(d, addr);
        // DTYP=data, DEXT, EOP, IFCS, RS.
        r.tx.write::<u32>(
            d + 8,
            len | (3 << 20) | (1 << 29) | (1 << 24) | (1 << 25) | (1 << 27),
        );
        r.tx.write::<u32>(d + 12, len << 14);
        r.tx_next = (i + 1) % TX_N;
        core::sync::atomic::fence(Ordering::SeqCst);
        self.w(TDT, r.tx_next as u32);
        Ok(())
    }

    fn receive(&self) -> Option<Vec<u8>> {
        let mut r = self.rings.lock();
        loop {
            let i = r.rx_next;
            let d = i * 16;
            let status = r.rx.read::<u32>(d + 8);
            if status & 1 == 0 {
                return None;
            }
            let len = r.rx.read::<u16>(d + 12) as usize;
            let eop = status & 2 != 0;
            let frame = (eop && len >= 14).then(|| {
                let off = i * BUF;
                r.rx_bufs.as_slice()[off..off + len.min(BUF)].to_vec()
            });
            // Re-arm (write-back overwrote the read-format fields).
            let addr = r.rx_bufs.phys() + (i * BUF) as u64;
            r.rx.write::<u64>(d, addr);
            r.rx.write::<u64>(d + 8, 0);
            r.rx_next = (i + 1) % RX_N;
            self.w(RDT, i as u32);
            if frame.is_some() {
                return frame;
            }
        }
    }
}

pub fn matches(dev: &PciDevice) -> bool {
    dev.vendor_id == 0x8086
        && matches!(
            dev.device_id,
            0x15F2
                | 0x15F3
                | 0x15F7
                | 0x15F8
                | 0x15FD
                | 0x0D9F
                | 0x125B
                | 0x125C
                | 0x125D
                | 0x125E
                | 0x125F
                | 0x3100
                | 0x3101
                | 0x3102
                | 0x5502
                | 0x5503
        )
}

pub fn probe(dev: &PciDevice) {
    dev.set_power_d0();
    dev.enable();
    let Some(mmio) = dev.map_bar(0) else { return };
    w32(mmio + IMC, 0xFFFF_FFFF);
    w32(mmio + EIMC, 0xFFFF_FFFF);
    w32(mmio + RCTL, 0);
    w32(mmio + TCTL, 0);
    crate::time::sleep_ms(10);
    w32(mmio + CTRL, r32(mmio + CTRL) | CTRL_DEV_RST);
    crate::time::sleep_ms(20);
    crate::time::wait_until(200, || r32(mmio + CTRL) & CTRL_DEV_RST == 0);
    w32(mmio + IMC, 0xFFFF_FFFF);
    let _ = r32(mmio + ICR);
    w32(mmio + CTRL_EXT, r32(mmio + CTRL_EXT) | CTRL_EXT_DRV_LOAD);

    let ral = r32(mmio + RAL0);
    let rah = r32(mmio + RAH0);
    let mut mac = [0u8; 6];
    mac[..4].copy_from_slice(&ral.to_le_bytes());
    mac[4..].copy_from_slice(&(rah as u16).to_le_bytes());
    if mac == [0; 6] || mac == [0xFF; 6] {
        crate::drivers::random::fill(&mut mac);
        mac[0] = (mac[0] & 0xFE) | 0x02;
        w32(
            mmio + RAL0,
            u32::from_le_bytes([mac[0], mac[1], mac[2], mac[3]]),
        );
    }
    w32(
        mmio + RAH0,
        u16::from_le_bytes([mac[4], mac[5]]) as u32 | (1 << 31),
    );
    for i in 0..128 {
        w32(mmio + MTA + i * 4, 0);
    }
    w32(mmio + CTRL, r32(mmio + CTRL) | CTRL_SLU);

    let (Some(rx), Some(rx_bufs), Some(tx), Some(tx_bufs)) = (
        DmaBuffer::new(RX_N * 16),
        DmaBuffer::new(RX_N * BUF),
        DmaBuffer::new(TX_N * 16),
        DmaBuffer::new(TX_N * BUF),
    ) else {
        return;
    };
    for i in 0..RX_N {
        rx.write::<u64>(i * 16, rx_bufs.phys() + (i * BUF) as u64);
    }
    w32(mmio + RDBAL, rx.phys() as u32);
    w32(mmio + RDBAH, (rx.phys() >> 32) as u32);
    w32(mmio + RDLEN, (RX_N * 16) as u32);
    // 2 KiB buffers, advanced one-buffer descriptors, drop when full.
    w32(mmio + SRRCTL, 2 | (1 << 25) | (1 << 31));
    w32(mmio + RDH, 0);
    w32(mmio + RDT, 0);
    w32(mmio + RXDCTL, QUEUE_ENABLE | 8 | (8 << 8) | (1 << 16));
    crate::time::wait_until(10, || r32(mmio + RXDCTL) & QUEUE_ENABLE != 0);
    w32(mmio + RDT, (RX_N - 1) as u32);
    w32(mmio + TDBAL, tx.phys() as u32);
    w32(mmio + TDBAH, (tx.phys() >> 32) as u32);
    w32(mmio + TDLEN, (TX_N * 16) as u32);
    w32(mmio + TDH, 0);
    w32(mmio + TDT, 0);
    w32(mmio + TXDCTL, QUEUE_ENABLE | 8 | (1 << 8) | (1 << 16));
    crate::time::wait_until(10, || r32(mmio + TXDCTL) & QUEUE_ENABLE != 0);
    w32(mmio + TCTL, (1 << 1) | (1 << 3) | (0x0F << 4));
    w32(mmio + RCTL, (1 << 1) | (1 << 15) | (1 << 26));

    let nic = Arc::new(Igc {
        mmio,
        mac,
        rings: Mutex::new(Rings {
            rx,
            rx_bufs,
            rx_next: 0,
            tx,
            tx_bufs,
            tx_next: 0,
            tx_clean: 0,
        }),
    });
    let handler: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let _ = r32(mmio + ICR);
        net::kick();
    });
    if dev.enable_msi_or_intx(handler).is_some() {
        // RXDW | RXDMT0 | LSC | TXDW
        w32(mmio + IMS, (1 << 7) | (1 << 4) | (1 << 2) | 1);
    }
    net::register(nic);
}
