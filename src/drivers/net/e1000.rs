//! Intel PRO/1000 family: 8254x (e1000), 82574L (e1000e) and the I217/
//! I218/I219 PCH-integrated LAN controllers, using legacy descriptors.
//!
//! PCH parts (I217+) get the extra bring-up steps they need when no
//! management engine has initialised the PHY: LANPHYPC toggling to force
//! the PHY out of power-gated/SMBus mode, and ULP (ultra-low-power) exit.

use crate::errno::*;
use crate::mm::dma::DmaBuffer;
use crate::net::{self, NetDevice, RxQueue};
use crate::pci::PciDevice;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use spin::Mutex;

const CTRL: u64 = 0x0000;
const STATUS: u64 = 0x0008;
const EERD: u64 = 0x0014;
const CTRL_EXT: u64 = 0x0018;
const MDIC: u64 = 0x0020;
const FEXTNVM3: u64 = 0x003C;
const ICR: u64 = 0x00C0;
const ITR: u64 = 0x00C4;
const IMS: u64 = 0x00D0;
const IMC: u64 = 0x00D8;
const RCTL: u64 = 0x0100;
const TCTL: u64 = 0x0400;
const TIPG: u64 = 0x0410;
const RDBAL: u64 = 0x2800;
const RDBAH: u64 = 0x2804;
const RDLEN: u64 = 0x2808;
const RDH: u64 = 0x2810;
const RDT: u64 = 0x2818;
const RXDCTL: u64 = 0x2828;
const TDBAL: u64 = 0x3800;
const TDBAH: u64 = 0x3804;
const TDLEN: u64 = 0x3808;
const TDH: u64 = 0x3810;
const TDT: u64 = 0x3818;
const TXDCTL: u64 = 0x3828;
const MTA: u64 = 0x5200;
const RAL0: u64 = 0x5400;
const RAH0: u64 = 0x5404;
const FWSM: u64 = 0x5B54;
const H2ME: u64 = 0x5B50;

const CTRL_SLU: u32 = 1 << 6;
const CTRL_ASDE: u32 = 1 << 5;
const CTRL_RST: u32 = 1 << 26;
const CTRL_PHY_RST: u32 = 1 << 31;
const CTRL_LANPHYPC_OVERRIDE: u32 = 1 << 16;
const CTRL_LANPHYPC_VALUE: u32 = 1 << 17;
const CTRL_EXT_DRV_LOAD: u32 = 1 << 28;
const STATUS_LU: u32 = 1 << 1;

const RCTL_EN: u32 = 1 << 1;
const RCTL_BAM: u32 = 1 << 15;
const RCTL_SECRC: u32 = 1 << 26;
const TCTL_EN: u32 = 1 << 1;
const TCTL_PSP: u32 = 1 << 3;

const INT_TXDW: u32 = 1 << 0;
const INT_LSC: u32 = 1 << 2;
const INT_RXDMT0: u32 = 1 << 4;
const INT_RXO: u32 = 1 << 6;
const INT_RXT0: u32 = 1 << 7;

const RX_N: usize = 256;
const TX_N: usize = 256;
const BUF: usize = 2048;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Family {
    /// 8254x: EEPROM MAC via EERD with 8-bit address shift.
    Classic,
    /// 82571/82572/82573/82574/82583.
    E1000e,
    /// I217/I218/I219 (PCH LAN).
    Pch,
}

struct Rings {
    rx: DmaBuffer,
    rx_bufs: DmaBuffer,
    rx_next: usize,
    tx: DmaBuffer,
    tx_bufs: DmaBuffer,
    tx_next: usize,
    tx_clean: usize,
}

pub struct E1000 {
    mmio: u64,
    mac: [u8; 6],
    family: Family,
    name: &'static str,
    rings: Mutex<Rings>,
    rxq: RxQueue,
    link: AtomicBool,
    speed: AtomicU32,
}

fn r32(a: u64) -> u32 {
    unsafe { core::ptr::read_volatile(a as *const u32) }
}
fn w32(a: u64, v: u32) {
    unsafe { core::ptr::write_volatile(a as *mut u32, v) }
}

impl E1000 {
    fn r(&self, reg: u64) -> u32 {
        r32(self.mmio + reg)
    }
    fn w(&self, reg: u64, v: u32) {
        w32(self.mmio + reg, v)
    }

    fn update_link(&self) {
        let st = self.r(STATUS);
        let up = st & STATUS_LU != 0;
        self.link.store(up, Ordering::SeqCst);
        self.speed.store(
            match (st >> 6) & 3 {
                0 => 10,
                1 => 100,
                _ => 1000,
            },
            Ordering::SeqCst,
        );
    }

    /// Move completed receive descriptors into the software queue.
    fn harvest_rx(&self) {
        let mut r = self.rings.lock();
        loop {
            let i = r.rx_next;
            let d = i * 16;
            let status = r.rx.read::<u8>(d + 12);
            if status & 1 == 0 {
                break;
            }
            let len = r.rx.read::<u16>(d + 8) as usize;
            let errors = r.rx.read::<u8>(d + 13);
            if status & 2 != 0 && errors == 0 && len >= 14 {
                let off = i * BUF;
                self.rxq
                    .push(r.rx_bufs.as_slice()[off..off + len.min(BUF)].to_vec());
            }
            r.rx.write::<u8>(d + 12, 0);
            r.rx_next = (i + 1) % RX_N;
            // Give the descriptor back to hardware.
            self.w(RDT, i as u32);
        }
    }

    /// Interrupt handler: acknowledge and let the network thread do the
    /// work (the rings are only touched from thread context).
    fn interrupt(&self) {
        let icr = self.r(ICR);
        if icr & INT_LSC != 0 {
            self.update_link();
        }
        if icr != 0 {
            net::kick();
        }
    }
}

impl NetDevice for E1000 {
    fn mac(&self) -> [u8; 6] {
        self.mac
    }
    fn link_up(&self) -> bool {
        self.link.load(Ordering::SeqCst)
    }
    fn speed(&self) -> Option<u32> {
        self.link_up().then(|| self.speed.load(Ordering::SeqCst))
    }
    fn driver(&self) -> &'static str {
        self.name
    }

    fn transmit(&self, frame: &[u8]) -> KResult<()> {
        if frame.len() > BUF {
            return Err(EMSGSIZE);
        }
        let mut r = self.rings.lock();
        // Reclaim finished descriptors.
        while r.tx_clean != r.tx_next && r.tx.read::<u8>(r.tx_clean * 16 + 12) & 1 != 0 {
            r.tx_clean = (r.tx_clean + 1) % TX_N;
        }
        let i = r.tx_next;
        if (i + 1) % TX_N == r.tx_clean {
            return Err(EAGAIN);
        }
        let off = i * BUF;
        r.tx_bufs.as_mut_slice()[off..off + frame.len()].copy_from_slice(frame);
        let d = i * 16;
        let addr = r.tx_bufs.phys() + off as u64;
        r.tx.write::<u64>(d, addr);
        r.tx.write::<u16>(d + 8, frame.len().max(60) as u16);
        r.tx.write::<u8>(d + 10, 0);
        r.tx.write::<u8>(d + 11, 0x01 | 0x02 | 0x08); // EOP | IFCS | RS
        r.tx.write::<u8>(d + 12, 0);
        r.tx_next = (i + 1) % TX_N;
        core::sync::atomic::fence(Ordering::SeqCst);
        self.w(TDT, r.tx_next as u32);
        Ok(())
    }

    fn receive(&self) -> Option<Vec<u8>> {
        if let Some(f) = self.rxq.pop() {
            return Some(f);
        }
        // Polling fallback (interrupts may be unavailable or coalesced).
        self.harvest_rx();
        if !self.link_up() {
            self.update_link();
        }
        self.rxq.pop()
    }
}

fn family_of(dev: &PciDevice) -> Option<(Family, &'static str)> {
    let id = dev.device_id;
    Some(match id {
        0x100E | 0x100F | 0x1004 | 0x1008 | 0x1009 | 0x100C | 0x100D | 0x1010 | 0x1011 | 0x1012
        | 0x1013 | 0x1015 | 0x1016 | 0x1017 | 0x1018 | 0x1019 | 0x101A | 0x101D | 0x101E
        | 0x1026 | 0x1027 | 0x1028 | 0x1075 | 0x1076 | 0x1077 | 0x1078 | 0x1079 | 0x107A
        | 0x107B | 0x107C | 0x108A | 0x1099 | 0x10B5 => (Family::Classic, "e1000"),
        0x105E | 0x105F | 0x1060 | 0x107D | 0x107E | 0x107F | 0x108B | 0x108C | 0x109A | 0x10D3
        | 0x10F6 | 0x150C | 0x10BC | 0x10A4 | 0x10A5 | 0x10D5 | 0x10D9 | 0x10DA => {
            (Family::E1000e, "e1000e")
        }
        // I217, I218, I219 (LM/V variants across PCH generations).
        0x153A | 0x153B | 0x155A | 0x1559 | 0x15A0 | 0x15A1 | 0x15A2 | 0x15A3 | 0x156F | 0x1570
        | 0x15B7 | 0x15B8 | 0x15B9 | 0x15BB | 0x15BC | 0x15BD | 0x15BE | 0x15D6 | 0x15D7
        | 0x15D8 | 0x15E3 | 0x15DF | 0x15E0 | 0x15E1 | 0x15E2 | 0x0D4E | 0x0D4F | 0x0D4C
        | 0x0D4D | 0x0D53 | 0x0D55 | 0x15F4 | 0x15F5 | 0x15F9 | 0x15FA | 0x15FB | 0x15FC
        | 0x1A1C | 0x1A1D | 0x1A1E | 0x1A1F | 0x0DC5 | 0x0DC6 | 0x0DC7 | 0x0DC8 | 0x550A
        | 0x550B | 0x550C | 0x550D | 0x550E | 0x550F | 0x5510 | 0x5511 | 0x57A0 | 0x57A1
        | 0x57B3 | 0x57B4 | 0x57B5 | 0x57B6 => (Family::Pch, "e1000e-pch"),
        _ => return None,
    })
}

pub fn matches(dev: &PciDevice) -> bool {
    dev.vendor_id == 0x8086 && family_of(dev).is_some()
}

/// Read a word from the EEPROM/NVM via EERD.
fn eeprom_read(mmio: u64, family: Family, addr: u16) -> Option<u16> {
    let (shift, done) = match family {
        Family::Classic => (8, 1 << 4),
        _ => (2, 1 << 1),
    };
    w32(mmio + EERD, 1 | ((addr as u32) << shift));
    if !crate::time::wait_until(10, || r32(mmio + EERD) & done != 0) {
        return None;
    }
    Some((r32(mmio + EERD) >> 16) as u16)
}

/// PCH: force the PHY into a usable state (mirrors the essential parts of
/// Linux's e1000_init_phy_workarounds_pchlan for I217/I218/I219).
fn pch_phy_power_up(mmio: u64) {
    const CTRL_EXT_LPCD: u32 = 1 << 2;
    const CTRL_EXT_FORCE_SMBUS: u32 = 1 << 11;
    const FEXTNVM3_PHY_CFG_COUNTER_MASK: u32 = 0x0C00_0000;
    const FEXTNVM3_PHY_CFG_COUNTER_50MS: u32 = 0x0800_0000;
    const FWSM_FW_VALID: u32 = 1 << 15;
    const FWSM_ULP_CFG_DONE: u32 = 1 << 10;
    const H2ME_ULP: u32 = 1 << 11;
    const H2ME_ENFORCE_SETTINGS: u32 = 1 << 12;

    let fw_valid = r32(mmio + FWSM) & FWSM_FW_VALID != 0;
    // Leave ULP (ultra low power) mode.
    if fw_valid {
        // The ME owns ULP: ask it to exit and wait for it to finish.
        let h2me = r32(mmio + H2ME);
        w32(mmio + H2ME, (h2me & !H2ME_ULP) | H2ME_ENFORCE_SETTINGS);
        crate::time::wait_until(30, || r32(mmio + FWSM) & FWSM_ULP_CFG_DONE == 0);
        let h2me = r32(mmio + H2ME);
        w32(mmio + H2ME, h2me & !H2ME_ENFORCE_SETTINGS);
    }
    // Shorten the PHY config counter so the toggle completes quickly.
    let f3 = r32(mmio + FEXTNVM3);
    w32(
        mmio + FEXTNVM3,
        (f3 & !FEXTNVM3_PHY_CFG_COUNTER_MASK) | FEXTNVM3_PHY_CFG_COUNTER_50MS,
    );
    // Toggle LANPHYPC to power-cycle the PHY out of SMBus/power-gated mode.
    let ctrl = r32(mmio + CTRL);
    w32(
        mmio + CTRL,
        (ctrl | CTRL_LANPHYPC_OVERRIDE) & !CTRL_LANPHYPC_VALUE,
    );
    let _ = r32(mmio + STATUS);
    crate::time::delay_us(1000);
    w32(mmio + CTRL, r32(mmio + CTRL) & !CTRL_LANPHYPC_OVERRIDE);
    let _ = r32(mmio + STATUS);
    crate::time::wait_until(100, || r32(mmio + CTRL_EXT) & CTRL_EXT_LPCD != 0);
    crate::time::sleep_ms(30);
    // Talk to the PHY over MDIO rather than SMBus from now on.
    w32(
        mmio + CTRL_EXT,
        r32(mmio + CTRL_EXT) & !CTRL_EXT_FORCE_SMBUS,
    );
}

fn mdic_write(mmio: u64, phy: u32, reg: u32, val: u16) {
    w32(
        mmio + MDIC,
        (val as u32) | (reg << 16) | (phy << 21) | (1 << 26),
    );
    crate::time::wait_until(10, || r32(mmio + MDIC) & (1 << 28) != 0);
}

pub fn probe(dev: &PciDevice) {
    let Some((family, name)) = family_of(dev) else {
        return;
    };
    dev.set_power_d0();
    dev.enable();
    let Some(mmio) = dev.map_bar(0) else {
        return;
    };
    if family == Family::Pch {
        pch_phy_power_up(mmio);
    }
    // Mask interrupts, stop RX/TX, reset.
    w32(mmio + IMC, 0xFFFF_FFFF);
    w32(mmio + RCTL, 0);
    w32(mmio + TCTL, TCTL_PSP);
    crate::time::sleep_ms(10);
    let mut ctrl = r32(mmio + CTRL) | CTRL_RST;
    if family == Family::Pch {
        ctrl |= CTRL_PHY_RST;
    }
    w32(mmio + CTRL, ctrl);
    crate::time::sleep_ms(20);
    crate::time::wait_until(100, || r32(mmio + CTRL) & CTRL_RST == 0);
    w32(mmio + IMC, 0xFFFF_FFFF);
    let _ = r32(mmio + ICR);
    if family != Family::Classic {
        w32(mmio + CTRL_EXT, r32(mmio + CTRL_EXT) | CTRL_EXT_DRV_LOAD);
    }

    // MAC address: receive address register if firmware set it, else NVM.
    let mut mac = [0u8; 6];
    let ral = r32(mmio + RAL0);
    let rah = r32(mmio + RAH0);
    if rah & (1 << 31) != 0 && (ral != 0 || rah & 0xFFFF != 0) {
        mac[..4].copy_from_slice(&ral.to_le_bytes());
        mac[4..].copy_from_slice(&(rah as u16).to_le_bytes());
    } else {
        for i in 0..3u16 {
            let w = eeprom_read(mmio, family, i).unwrap_or(0);
            mac[i as usize * 2..i as usize * 2 + 2].copy_from_slice(&w.to_le_bytes());
        }
        if mac == [0; 6] || mac == [0xFF; 6] {
            crate::drivers::random::fill(&mut mac);
            mac[0] = (mac[0] & 0xFE) | 0x02;
        }
    }
    w32(
        mmio + RAL0,
        u32::from_le_bytes([mac[0], mac[1], mac[2], mac[3]]),
    );
    w32(
        mmio + RAH0,
        u16::from_le_bytes([mac[4], mac[5]]) as u32 | (1 << 31),
    );
    for i in 0..128 {
        w32(mmio + MTA + i * 4, 0);
    }

    // Link: auto-negotiate everything.
    w32(
        mmio + CTRL,
        (r32(mmio + CTRL) | CTRL_SLU | CTRL_ASDE) & !(1 << 3) & !(1 << 7),
    );
    if family == Family::Pch {
        // Restart autonegotiation on PHY address 2 (BMCR: ANENABLE|RESTART).
        mdic_write(mmio, 2, 0, 0x1200);
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
        rx.write::<u64>(i * 16, rx_bufs.phys() + (i * BUF) as u64);
    }
    w32(mmio + RDBAL, rx.phys() as u32);
    w32(mmio + RDBAH, (rx.phys() >> 32) as u32);
    w32(mmio + RDLEN, (RX_N * 16) as u32);
    w32(mmio + RDH, 0);
    w32(mmio + RDT, (RX_N - 1) as u32);
    w32(mmio + TDBAL, tx.phys() as u32);
    w32(mmio + TDBAH, (tx.phys() >> 32) as u32);
    w32(mmio + TDLEN, (TX_N * 16) as u32);
    w32(mmio + TDH, 0);
    w32(mmio + TDT, 0);
    if family != Family::Classic {
        // Descriptor write-back granularity; queue enable on newer parts.
        w32(mmio + RXDCTL, r32(mmio + RXDCTL) | (1 << 24) | (1 << 25));
        w32(mmio + TXDCTL, r32(mmio + TXDCTL) | (1 << 24) | (1 << 25));
    }
    w32(mmio + TIPG, 10 | (8 << 10) | (6 << 20));
    w32(mmio + TCTL, TCTL_EN | TCTL_PSP | (0x0F << 4) | (0x3F << 12));
    w32(mmio + RCTL, RCTL_EN | RCTL_BAM | RCTL_SECRC);
    w32(mmio + ITR, 1000); // ~4000 interrupts/s max

    let nic = Arc::new(E1000 {
        mmio,
        mac,
        family,
        name,
        rings: Mutex::new(Rings {
            rx,
            rx_bufs,
            rx_next: 0,
            tx,
            tx_bufs,
            tx_next: 0,
            tx_clean: 0,
        }),
        rxq: RxQueue::new(),
        link: AtomicBool::new(false),
        speed: AtomicU32::new(0),
    });
    let n = nic.clone();
    // MSI (single vector) avoids the 82574's IVAR setup for MSI-X.
    let irq = dev.enable_msi_or_intx(Arc::new(move || n.interrupt()));
    w32(
        mmio + IMS,
        INT_RXT0 | INT_RXO | INT_RXDMT0 | INT_LSC | INT_TXDW,
    );
    nic.update_link();
    if irq.is_none() {
        crate::println!("[{}] {}: no interrupt, polling", name, dev.name());
    }
    let _ = nic.family;
    net::register(nic);
}
