//! Realtek RTL8169/8168/8111/8101 and RTL8125 (2.5 GbE) Ethernet.
//!
//! Generic bring-up only: the chip-revision specific PHY tuning tables and
//! firmware patches of the Linux driver are not applied, which most boards
//! tolerate (link comes up, possibly with reduced power efficiency).

use crate::errno::*;
use crate::mm::dma::DmaBuffer;
use crate::net::{self, NetDevice};
use crate::pci::{Bar, PciDevice};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;
use spin::Mutex;

const IDR0: u64 = 0x00;
const MAR0: u64 = 0x08;
const TX_DESC_LO: u64 = 0x20;
const TX_DESC_HI: u64 = 0x24;
const CHIP_CMD: u64 = 0x37;
const TX_POLL_8169: u64 = 0x38;
const INTR_MASK_8169: u64 = 0x3C;
const INTR_STATUS_8169: u64 = 0x3E;
const INTR_MASK_8125: u64 = 0x38;
const INTR_STATUS_8125: u64 = 0x3C;
const TX_POLL_8125: u64 = 0x90;
const TX_CONFIG: u64 = 0x40;
const RX_CONFIG: u64 = 0x44;
const CFG9346: u64 = 0x50;
const PHY_STATUS: u64 = 0x6C;
const RX_MAX_SIZE: u64 = 0xDA;
const CPLUS_CMD: u64 = 0xE0;
const RX_DESC_LO: u64 = 0xE4;
const RX_DESC_HI: u64 = 0xE8;
const MAX_TX_SIZE: u64 = 0xEC;

const CMD_RESET: u8 = 0x10;
const CMD_RX_EN: u8 = 0x08;
const CMD_TX_EN: u8 = 0x04;

const OWN: u32 = 1 << 31;
const EOR: u32 = 1 << 30;
const FS: u32 = 1 << 29;
const LS: u32 = 1 << 28;

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

pub struct Rtl {
    mmio: u64,
    mac: [u8; 6],
    is_8125: bool,
    rings: Mutex<Rings>,
}

fn r8(a: u64) -> u8 {
    unsafe { core::ptr::read_volatile(a as *const u8) }
}
fn w8(a: u64, v: u8) {
    unsafe { core::ptr::write_volatile(a as *mut u8, v) }
}
fn r16(a: u64) -> u16 {
    unsafe { core::ptr::read_volatile(a as *const u16) }
}
fn w16(a: u64, v: u16) {
    unsafe { core::ptr::write_volatile(a as *mut u16, v) }
}
fn r32(a: u64) -> u32 {
    unsafe { core::ptr::read_volatile(a as *const u32) }
}
fn w32(a: u64, v: u32) {
    unsafe { core::ptr::write_volatile(a as *mut u32, v) }
}

impl NetDevice for Rtl {
    fn shutdown(&self) {
        w8(self.mmio + CHIP_CMD, CMD_RESET);
    }
    fn mac(&self) -> [u8; 6] {
        self.mac
    }
    fn link_up(&self) -> bool {
        r16(self.mmio + PHY_STATUS) & 0x02 != 0
    }
    fn speed(&self) -> Option<u32> {
        let s = r16(self.mmio + PHY_STATUS);
        if s & 0x02 == 0 {
            return None;
        }
        Some(if self.is_8125 && s & 0x400 != 0 {
            2500
        } else if s & 0x10 != 0 {
            1000
        } else if s & 0x08 != 0 {
            100
        } else {
            10
        })
    }
    fn driver(&self) -> &'static str {
        if self.is_8125 { "r8125" } else { "r8169" }
    }

    fn transmit(&self, frame: &[u8]) -> KResult<()> {
        if frame.len() > BUF {
            return Err(EMSGSIZE);
        }
        let mut r = self.rings.lock();
        while r.tx_clean != r.tx_next && r.tx.read::<u32>(r.tx_clean * 16) & OWN == 0 {
            r.tx_clean = (r.tx_clean + 1) % TX_N;
        }
        let i = r.tx_next;
        if (i + 1) % TX_N == r.tx_clean {
            return Err(EAGAIN);
        }
        let off = i * BUF;
        let len = frame.len().max(60);
        r.tx_bufs.as_mut_slice()[off..off + frame.len()].copy_from_slice(frame);
        r.tx_bufs.as_mut_slice()[off + frame.len()..off + len].fill(0);
        let d = i * 16;
        let addr = r.tx_bufs.phys() + off as u64;
        r.tx.write::<u64>(d + 8, addr);
        r.tx.write::<u32>(d + 4, 0);
        let eor = if i == TX_N - 1 { EOR } else { 0 };
        core::sync::atomic::fence(Ordering::SeqCst);
        r.tx.write::<u32>(d, OWN | eor | FS | LS | len as u32);
        r.tx_next = (i + 1) % TX_N;
        core::sync::atomic::fence(Ordering::SeqCst);
        if self.is_8125 {
            w16(self.mmio + TX_POLL_8125, 1);
        } else {
            w8(self.mmio + TX_POLL_8169, 0x40);
        }
        Ok(())
    }

    fn receive(&self) -> Option<Vec<u8>> {
        let mut r = self.rings.lock();
        loop {
            let i = r.rx_next;
            let d = i * 16;
            let opts1 = r.rx.read::<u32>(d);
            if opts1 & OWN != 0 {
                return None;
            }
            let whole = opts1 & (FS | LS) == (FS | LS);
            let errors = opts1 & (1 << 21) != 0; // RES
            let len = (opts1 & 0x3FFF) as usize;
            let frame = (whole && !errors && len > 18).then(|| {
                let off = i * BUF;
                r.rx_bufs.as_slice()[off..off + (len - 4).min(BUF)].to_vec() // strip CRC
            });
            let eor = if i == RX_N - 1 { EOR } else { 0 };
            r.rx.write::<u32>(d + 4, 0);
            core::sync::atomic::fence(Ordering::SeqCst);
            r.rx.write::<u32>(d, OWN | eor | BUF as u32);
            r.rx_next = (i + 1) % RX_N;
            if frame.is_some() {
                return frame;
            }
        }
    }
}

pub fn matches(dev: &PciDevice) -> bool {
    matches!(
        (dev.vendor_id, dev.device_id),
        (
            0x10EC,
            0x8161 | 0x8168 | 0x8169 | 0x8136 | 0x8167 | 0x8125 | 0x3000 | 0x8162
        ) | (0x1186, 0x4300 | 0x4302)
            | (0x1737, 0x1032)
    )
}

pub fn probe(dev: &PciDevice) {
    dev.set_power_d0();
    dev.enable();
    // The register window is the first memory BAR (BAR0 is usually I/O).
    let Some(bar) = (0..6).find(|&i| matches!(dev.bars[i], Bar::Mmio { .. })) else {
        return;
    };
    let Some(mmio) = dev.map_bar(bar) else { return };
    let is_8125 = matches!(dev.device_id, 0x8125 | 0x3000 | 0x8162);
    let (imask, istatus) = if is_8125 {
        (INTR_MASK_8125, INTR_STATUS_8125)
    } else {
        (INTR_MASK_8169, INTR_STATUS_8169)
    };
    if is_8125 {
        w32(mmio + imask, 0);
    } else {
        w16(mmio + imask, 0);
    }
    w8(mmio + CHIP_CMD, CMD_RESET);
    if !crate::time::wait_until(100, || r8(mmio + CHIP_CMD) & CMD_RESET == 0) {
        crate::println!("[r8169] {}: reset timed out", dev.name());
        return;
    }
    let mut mac = [0u8; 6];
    for (i, m) in mac.iter_mut().enumerate() {
        *m = r8(mmio + IDR0 + i as u64);
    }
    let (Some(rx), Some(rx_bufs), Some(tx), Some(tx_bufs)) = (
        DmaBuffer::new(RX_N * 16),
        DmaBuffer::new(RX_N * BUF),
        DmaBuffer::new(TX_N * 16),
        DmaBuffer::new(TX_N * BUF),
    ) else {
        return;
    };
    for i in 0..RX_N {
        let d = i * 16;
        rx.write::<u64>(d + 8, rx_bufs.phys() + (i * BUF) as u64);
        rx.write::<u32>(d + 4, 0);
        let eor = if i == RX_N - 1 { EOR } else { 0 };
        rx.write::<u32>(d, OWN | eor | BUF as u32);
    }
    w8(mmio + CFG9346, 0xC0); // unlock config registers
    w16(mmio + CPLUS_CMD, r16(mmio + CPLUS_CMD) | (1 << 3)); // PCI multiple R/W
    w16(mmio + RX_MAX_SIZE, BUF as u16);
    if !is_8125 {
        w8(mmio + MAX_TX_SIZE, 0x3B);
    }
    w32(mmio + TX_DESC_LO, tx.phys() as u32);
    w32(mmio + TX_DESC_HI, (tx.phys() >> 32) as u32);
    w32(mmio + RX_DESC_LO, rx.phys() as u32);
    w32(mmio + RX_DESC_HI, (rx.phys() >> 32) as u32);
    w8(mmio + CHIP_CMD, CMD_RX_EN | CMD_TX_EN);
    w32(mmio + TX_CONFIG, (3 << 24) | (7 << 8));
    // Accept broadcast, multicast and our own address; unlimited DMA burst.
    w32(mmio + RX_CONFIG, (7 << 8) | (7 << 13) | 0x0E);
    w32(mmio + MAR0, 0xFFFF_FFFF);
    w32(mmio + MAR0 + 4, 0xFFFF_FFFF);
    w8(mmio + CFG9346, 0x00);

    let nic = Arc::new(Rtl {
        mmio,
        mac,
        is_8125,
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
        if is_8125 {
            let s = r32(mmio + istatus);
            w32(mmio + istatus, s);
        } else {
            let s = r16(mmio + istatus);
            w16(mmio + istatus, s);
        }
        net::kick();
    });
    let irq = dev.enable_msi_or_intx(handler).is_some();
    if irq {
        // RxOK RxErr TxOK TxErr RxOverflow LinkChg RxFIFOOver
        if is_8125 {
            w32(mmio + imask, 0x7F);
        } else {
            w16(mmio + imask, 0x7F);
        }
    }
    net::register(nic);
}
