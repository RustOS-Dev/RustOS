//! PCIe transport for AX210-family ("gen3") iwlwifi devices.
//!
//! The firmware boots itself from host memory described by a context-info
//! structure: we hand it the image loader, the runtime sections and the
//! addresses of the RX rings and the command queue, then kick it and wait
//! for the ALIVE interrupt. Afterwards all communication goes through the
//! command queue (host commands) and the RX ring (responses,
//! notifications and received frames); data queues are allocated at
//! runtime with a firmware command.

use super::fw::Firmware;
use crate::errno::*;
use crate::mm::dma::DmaBuffer;
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::sync::atomic::{Ordering, fence};

// CSR registers.
pub const CSR_HW_IF_CONFIG_REG: u64 = 0x000;
const CSR_INT_COALESCING: u64 = 0x004;
pub const CSR_INT: u64 = 0x008;
pub const CSR_INT_MASK: u64 = 0x00C;
pub const CSR_FH_INT_STATUS: u64 = 0x010;
pub const CSR_RESET: u64 = 0x020;
pub const CSR_GP_CNTRL: u64 = 0x024;
pub const CSR_HW_REV: u64 = 0x028;
const CSR_GIO_REG: u64 = 0x03C;
const CSR_UCODE_DRV_GP1_CLR: u64 = 0x05C;
const CSR_MBOX_SET_REG: u64 = 0x088;
pub const CSR_HW_RF_ID: u64 = 0x09C;
const CSR_MAC_SHADOW_REG_CTRL: u64 = 0x0A8;
const CSR_LTR_LONG_VAL_AD: u64 = 0x0D4;
const CSR_GIO_CHICKEN_BITS: u64 = 0x100;
const CSR_CTXT_INFO_ADDR: u64 = 0x118;
const CSR_IML_DATA_ADDR: u64 = 0x120;
const CSR_IML_SIZE_ADDR: u64 = 0x128;
const CSR_DBG_HPET_MEM_REG: u64 = 0x240;
const CSR_DBG_LINK_PWR_MGMT_REG: u64 = 0x250;
const CSR_MAC_ADDR_BASE: u64 = 0x380;
const HBUS_TARG_PRPH_WADDR: u64 = 0x444;
const HBUS_TARG_PRPH_RADDR: u64 = 0x448;
const HBUS_TARG_PRPH_WDAT: u64 = 0x44C;
const HBUS_TARG_PRPH_RDAT: u64 = 0x450;
const HBUS_TARG_WRPTR: u64 = 0x460;
const HBUS_TARG_MEM_RADDR: u64 = 0x40C;
const HBUS_TARG_MEM_RDAT: u64 = 0x41C;
const RFH_Q0_FRBDCB_WIDX_TRG: u64 = 0x1C80;
const CSR_MSIX_HW_INT_CAUSES_AD: u64 = 0x2808;

const HW_IF_NIC_READY: u32 = 0x0040_0000;
const HW_IF_PREPARE: u32 = 0x0800_0000;
const HW_IF_HAP_WAKE_L1A: u32 = 0x0008_0000;
const CTXT_INFO_AUTO_FUNC_BOOT_ENA: u32 = 1 << 1;
const RESET_SW_RESET: u32 = 0x80;
const GP_CNTRL_MAC_CLOCK_READY: u32 = 0x01;
const GP_CNTRL_INIT_DONE: u32 = 0x04;
const GP_CNTRL_MAC_ACCESS_REQ: u32 = 0x08;
pub const GP_CNTRL_HW_RF_KILL_SW: u32 = 0x0800_0000;
const MBOX_OS_ALIVE: u32 = 1 << 5;
const GIO_L0S_DISABLED: u32 = 0x02;
const GIO_CHICKEN_L1A_NO_L0S_RX: u32 = 0x0080_0000;
const DBG_HPET_MEM_VAL: u32 = 0xFFFF_0000;
const LINK_PWR_MGMT_DISABLED: u32 = 0x8000_0000;
const UCODE_SW_BIT_RFKILL: u32 = 0x02;
const UCODE_DRV_GP1_BIT_CMD_BLOCKED: u32 = 0x04;
const MSIX_HW_INT_IML: u32 = 1 << 1;

pub const INT_ALIVE: u32 = 1 << 0;
pub const INT_RF_KILL: u32 = 1 << 7;
pub const INT_SW_ERR: u32 = 1 << 25;
pub const INT_FH_TX: u32 = 1 << 27;
pub const INT_HW_ERR: u32 = 1 << 29;
pub const INT_FH_RX: u32 = 1 << 31;
const INT_SW_RX: u32 = 1 << 3;

// Periphery registers (UMAC ones live at +0x300000 on AX210).
const UMAC_PRPH_OFFSET: u32 = 0x30_0000;
const UREG_CPU_INIT_RUN: u32 = 0xA0_5C44;
const UREG_DOORBELL_TO_ISR6: u32 = 0xA0_5C04;
const UREG_DOORBELL_TO_ISR6_PNVM: u32 = 1 << 20;
const UMAG_SB_CPU_1_STATUS: u32 = 0xA0_38C0;
const UMAG_SB_CPU_2_STATUS: u32 = 0xA0_38C4;

/// RX ring size (free and used rings) and the number of buffers posted.
pub const RX_RING: usize = 512;
const RX_BUFS: usize = RX_RING - 8;
const RB_SIZE: usize = 4096;
const CMD_QUEUE: usize = 32;
const CMD_SLOT: usize = 4096;
const FIRST_TB: usize = 20;
const TFD_SIZE: usize = 256;
/// Hardware index space of TFD write pointers.
const TFD_INDEX_MASK: u32 = 0xFFFF;
const MAX_DRAM_ENTRY: usize = 64;

// Context info (gen3) layout.
const CTXT_INFO_SIZE: usize = 0x68;
/// prph_scratch: control config (0x54 bytes), 10 reserved words, then the
/// DRAM section tables (UMAC, LMAC, paging).
const PRPH_SCRATCH_DRAM: usize = 0x54 + 40;
const PRPH_SCRATCH_SIZE: usize = PRPH_SCRATCH_DRAM + 3 * MAX_DRAM_ENTRY * 8;
const PRPH_SCRATCH_RB_SIZE_4K: u32 = 1 << 16;
const PRPH_SCRATCH_MTR_MODE: u32 = 1 << 17;
const PRPH_MTR_FORMAT_256B: u32 = 0xC0000;

/// A received firmware packet: header fields plus payload.
pub struct Packet {
    pub cmd: u8,
    pub group: u8,
    pub seq: u16,
    pub data: Vec<u8>,
}

impl Packet {
    pub fn is_notification(&self) -> bool {
        self.seq & 0x8000 != 0
    }
}

/// A TFD ring with a byte-count table and one bounce buffer per slot.
pub struct TxQueue {
    pub id: u16,
    size: usize,
    tfds: DmaBuffer,
    bc: DmaBuffer,
    first_tb: DmaBuffer,
    bufs: DmaBuffer,
    slot: usize,
    write: u32,
    read: u32,
}

fn put(buf: &DmaBuffer, off: usize, data: &[u8]) {
    assert!(off + data.len() <= buf.len());
    unsafe {
        core::ptr::copy_nonoverlapping(data.as_ptr(), (buf.virt() as *mut u8).add(off), data.len())
    }
}

fn put64(buf: &DmaBuffer, off: usize, v: u64) {
    put(buf, off, &v.to_le_bytes())
}

impl TxQueue {
    pub fn new(size: usize, slot: usize) -> Option<TxQueue> {
        Some(TxQueue {
            id: 0,
            size,
            tfds: DmaBuffer::new(size * TFD_SIZE)?,
            bc: DmaBuffer::new(size.max(1024) * 2)?,
            first_tb: DmaBuffer::new(size * 64)?,
            bufs: DmaBuffer::new(size * slot)?,
            slot,
            write: 0,
            read: 0,
        })
    }

    pub fn tfd_phys(&self) -> u64 {
        self.tfds.phys()
    }
    pub fn bc_phys(&self) -> u64 {
        self.bc.phys()
    }
    /// log2(size) - 3, the ring size encoding used by the firmware.
    pub fn cb_size(&self) -> u32 {
        self.size.trailing_zeros() - 3
    }

    fn used(&self) -> usize {
        (self.write.wrapping_sub(self.read) & TFD_INDEX_MASK) as usize
    }

    pub fn space(&self) -> usize {
        self.size - 1 - self.used()
    }

    pub fn set_start(&mut self, ptr: u32) {
        self.write = ptr & TFD_INDEX_MASK;
        self.read = self.write;
    }

    /// Mark everything up to (not including) `idx` as done.
    pub fn reclaim_to(&mut self, idx: u32) {
        let mask = self.size as u32 - 1;
        while self.read != self.write && (self.read & mask) != (idx & mask) {
            self.read = (self.read + 1) & TFD_INDEX_MASK;
        }
    }

    pub fn reclaim_one(&mut self) {
        if self.read != self.write {
            self.read = (self.read + 1) & TFD_INDEX_MASK;
        }
    }

    pub fn next_index(&self) -> usize {
        (self.write as usize) & (self.size - 1)
    }

    /// Queue `parts` (concatenated) as one TFD. The first 20 bytes go in
    /// TB0 (bidirectional scratch), bytes up to `split` in TB1, the rest in
    /// TB2. Returns the TFD count used for the byte-count table.
    fn enqueue(&mut self, data: &[u8], split: usize, byte_cnt: Option<u16>) -> KResult<()> {
        if self.space() < 2 {
            return Err(EAGAIN);
        }
        if data.len() > self.slot {
            return Err(EMSGSIZE);
        }
        let idx = self.next_index();
        let off = idx * self.slot;
        put(&self.bufs, off, data);
        let tb0 = data.len().min(FIRST_TB);
        put(&self.first_tb, idx * 64, &data[..tb0]);
        let mut tbs: Vec<(u64, u16)> =
            alloc::vec![(self.first_tb.phys() + (idx * 64) as u64, tb0 as u16)];
        let split = split.clamp(tb0, data.len());
        if split > tb0 {
            tbs.push((self.bufs.phys() + (off + tb0) as u64, (split - tb0) as u16));
        }
        if data.len() > split {
            tbs.push((
                self.bufs.phys() + (off + split) as u64,
                (data.len() - split) as u16,
            ));
        }
        let t = idx * TFD_SIZE;
        put(&self.tfds, t, &[0u8; TFD_SIZE]);
        put(&self.tfds, t, &(tbs.len() as u16).to_le_bytes());
        for (i, (addr, len)) in tbs.iter().enumerate() {
            put(&self.tfds, t + 2 + i * 10, &len.to_le_bytes());
            put(&self.tfds, t + 4 + i * 10, &addr.to_le_bytes());
        }
        if let Some(len) = byte_cnt {
            let filled = 2 + tbs.len() * 10;
            let chunks = filled.div_ceil(64) as u16 - 1;
            put(
                &self.bc,
                idx * 2,
                &((len & 0x3FFF) | (chunks << 14)).to_le_bytes(),
            );
        }
        fence(Ordering::SeqCst);
        self.write = (self.write + 1) & TFD_INDEX_MASK;
        Ok(())
    }
}

pub struct Trans {
    pub mmio: u64,
    pub hw_rev: u32,
    pub hw_rf_id: u32,
    pub integrated: bool,
    rx_free: DmaBuffer,
    rx_used: DmaBuffer,
    rb_stts: DmaBuffer,
    rx_bufs: DmaBuffer,
    rx_read: usize,
    rx_write: usize,
    rx_stocked: bool,
    pub cmdq: TxQueue,
    // Boot structures (kept for the lifetime of the firmware run).
    ctxt: Option<DmaBuffer>,
    prph_scratch: Option<DmaBuffer>,
    prph_info: Option<DmaBuffer>,
    fw_dram: Vec<DmaBuffer>,
    pnvm: Option<DmaBuffer>,
    /// Packets received while waiting for a command response.
    pub pending: VecDeque<Packet>,
}

fn r32(a: u64) -> u32 {
    unsafe { core::ptr::read_volatile(a as *const u32) }
}
fn w32(a: u64, v: u32) {
    unsafe { core::ptr::write_volatile(a as *mut u32, v) }
}

impl Trans {
    pub fn new(mmio: u64, integrated: bool) -> Option<Trans> {
        Some(Trans {
            mmio,
            hw_rev: r32(mmio + CSR_HW_REV),
            hw_rf_id: r32(mmio + CSR_HW_RF_ID),
            integrated,
            rx_free: DmaBuffer::new(RX_RING * 16)?,
            rx_used: DmaBuffer::new(RX_RING * 32)?,
            rb_stts: DmaBuffer::new(4096)?,
            rx_bufs: DmaBuffer::new(RX_BUFS * RB_SIZE)?,
            rx_read: 0,
            rx_write: 0,
            rx_stocked: false,
            cmdq: TxQueue::new(CMD_QUEUE, CMD_SLOT)?,
            ctxt: None,
            prph_scratch: None,
            prph_info: None,
            fw_dram: Vec::new(),
            pnvm: None,
            pending: VecDeque::new(),
        })
    }

    pub fn r(&self, reg: u64) -> u32 {
        r32(self.mmio + reg)
    }
    pub fn w(&self, reg: u64, v: u32) {
        w32(self.mmio + reg, v)
    }
    fn w64(&self, reg: u64, v: u64) {
        self.w(reg, v as u32);
        self.w(reg + 4, (v >> 32) as u32);
    }
    fn set(&self, reg: u64, bits: u32) {
        self.w(reg, self.r(reg) | bits);
    }
    fn clear(&self, reg: u64, bits: u32) {
        self.w(reg, self.r(reg) & !bits);
    }
    fn poll(&self, reg: u64, mask: u32, ms: u64) -> bool {
        crate::time::wait_until(ms, || self.r(reg) & mask == mask)
    }

    fn grab_nic(&self) -> bool {
        self.set(CSR_GP_CNTRL, GP_CNTRL_MAC_ACCESS_REQ);
        crate::time::wait_until(15, || {
            self.r(CSR_GP_CNTRL) & (GP_CNTRL_MAC_CLOCK_READY | 0x10) == GP_CNTRL_MAC_CLOCK_READY
        })
    }
    fn release_nic(&self) {
        self.clear(CSR_GP_CNTRL, GP_CNTRL_MAC_ACCESS_REQ);
    }

    pub fn write_prph(&self, addr: u32, v: u32) {
        let ok = self.grab_nic();
        self.w(HBUS_TARG_PRPH_WADDR, (addr & 0xFF_FFFF) | (3 << 24));
        self.w(HBUS_TARG_PRPH_WDAT, v);
        if ok {
            self.release_nic();
        }
    }
    pub fn read_prph(&self, addr: u32) -> u32 {
        let ok = self.grab_nic();
        self.w(HBUS_TARG_PRPH_RADDR, (addr & 0xFF_FFFF) | (3 << 24));
        let v = self.r(HBUS_TARG_PRPH_RDAT);
        if ok {
            self.release_nic();
        }
        v
    }
    /// Read `words` 32-bit words of device SRAM (auto-incrementing).
    pub fn read_mem(&self, addr: u32, words: usize) -> alloc::vec::Vec<u32> {
        let ok = self.grab_nic();
        self.w(HBUS_TARG_MEM_RADDR, addr);
        let v = (0..words).map(|_| self.r(HBUS_TARG_MEM_RDAT)).collect();
        if ok {
            self.release_nic();
        }
        v
    }

    pub fn write_umac_prph(&self, addr: u32, v: u32) {
        self.write_prph(addr + UMAC_PRPH_OFFSET, v)
    }
    pub fn read_umac_prph(&self, addr: u32) -> u32 {
        self.read_prph(addr + UMAC_PRPH_OFFSET)
    }

    pub fn rfkill(&self) -> bool {
        self.r(CSR_GP_CNTRL) & GP_CNTRL_HW_RF_KILL_SW == 0
    }

    /// Permanent MAC address (OEM strap, else OTP).
    pub fn mac_address(&self) -> [u8; 6] {
        let flip = |a0: u32, a1: u32| {
            let a = a0.to_le_bytes();
            let b = a1.to_le_bytes();
            [a[3], a[2], a[1], a[0], b[1], b[0]]
        };
        let valid = |m: &[u8; 6]| m[0] & 1 == 0 && *m != [0; 6];
        let strap = flip(
            self.r(CSR_MAC_ADDR_BASE + 8),
            self.r(CSR_MAC_ADDR_BASE + 12),
        );
        if valid(&strap) {
            return strap;
        }
        flip(self.r(CSR_MAC_ADDR_BASE), self.r(CSR_MAC_ADDR_BASE + 4))
    }

    fn set_hw_ready(&self) -> bool {
        self.set(CSR_HW_IF_CONFIG_REG, HW_IF_NIC_READY);
        let ok = self.poll(CSR_HW_IF_CONFIG_REG, HW_IF_NIC_READY, 1);
        if ok {
            self.set(CSR_MBOX_SET_REG, MBOX_OS_ALIVE);
        }
        ok
    }

    /// Take ownership of the device from the platform (ME/CSME).
    fn prepare_card_hw(&self) -> bool {
        if self.set_hw_ready() {
            return true;
        }
        self.set(CSR_DBG_LINK_PWR_MGMT_REG, LINK_PWR_MGMT_DISABLED);
        crate::time::sleep_ms(2);
        for _ in 0..10 {
            self.set(CSR_HW_IF_CONFIG_REG, HW_IF_PREPARE);
            let t = crate::time::Deadline::after_ms(150);
            while !t.expired() {
                if self.set_hw_ready() {
                    return true;
                }
                crate::time::delay_us(200);
            }
            crate::time::sleep_ms(25);
        }
        false
    }

    fn sw_reset(&self) {
        self.set(CSR_RESET, RESET_SW_RESET);
        crate::time::sleep_ms(6);
    }

    fn apm_init(&self) -> KResult<()> {
        self.set(CSR_GIO_CHICKEN_BITS, GIO_CHICKEN_L1A_NO_L0S_RX);
        self.set(CSR_DBG_HPET_MEM_REG, DBG_HPET_MEM_VAL);
        self.set(CSR_HW_IF_CONFIG_REG, HW_IF_HAP_WAKE_L1A);
        self.set(CSR_GIO_REG, GIO_L0S_DISABLED);
        self.set(CSR_GP_CNTRL, GP_CNTRL_INIT_DONE);
        if !self.poll(CSR_GP_CNTRL, GP_CNTRL_MAC_CLOCK_READY, 25) {
            return Err(ETIMEDOUT);
        }
        Ok(())
    }

    /// Stop DMA and reset the device (also used on shutdown).
    pub fn stop(&mut self) {
        self.w(CSR_INT_MASK, 0);
        self.w(CSR_INT, 0xFFFF_FFFF);
        self.w(CSR_FH_INT_STATUS, 0xFFFF_FFFF);
        self.sw_reset();
        self.clear(CSR_GP_CNTRL, GP_CNTRL_INIT_DONE);
        self.ctxt = None;
        self.prph_scratch = None;
        self.prph_info = None;
        self.fw_dram.clear();
        self.pnvm = None;
    }

    /// Wake the NIC, lay out the boot structures and start the firmware.
    /// Returns once the ALIVE interrupt fired (the ALIVE packet itself
    /// follows on the RX ring).
    pub fn start_fw(&mut self, fw: &Firmware) -> KResult<()> {
        if !self.prepare_card_hw() {
            crate::println!("[iwlwifi] device not ready (owned by the platform?)");
            return Err(EIO);
        }
        self.sw_reset();
        self.apm_init()?;
        self.w(CSR_INT, 0xFFFF_FFFF);
        self.w(CSR_UCODE_DRV_GP1_CLR, UCODE_SW_BIT_RFKILL);
        self.w(CSR_UCODE_DRV_GP1_CLR, UCODE_DRV_GP1_BIT_CMD_BLOCKED);
        unsafe { core::ptr::write_volatile((self.mmio + CSR_INT_COALESCING) as *mut u8, 0x40) };

        // RX: reset rings; buffers are posted after ALIVE.
        self.rx_free.zero();
        self.rx_used.zero();
        self.rb_stts.zero();
        self.rx_read = 0;
        self.rx_write = 0;
        self.rx_stocked = false;
        // Command queue.
        self.cmdq.tfds.zero();
        self.cmdq.set_start(0);
        self.set(CSR_MAC_SHADOW_REG_CTRL, 0x800F_FFFF);

        // Firmware sections in DRAM.
        let mut dram = Vec::new();
        let scratch = DmaBuffer::new(PRPH_SCRATCH_SIZE).ok_or(ENOMEM)?;
        let groups: [(&[super::fw::Section], usize); 3] =
            [(&fw.umac, 0), (&fw.lmac, 1), (&fw.paging, 2)];
        for (secs, which) in groups {
            for (i, s) in secs.iter().enumerate() {
                let b = DmaBuffer::new(s.data.len()).ok_or(ENOMEM)?;
                put(&b, 0, &s.data);
                put64(
                    &scratch,
                    PRPH_SCRATCH_DRAM + (which * MAX_DRAM_ENTRY + i) * 8,
                    b.phys(),
                );
                dram.push(b);
            }
        }
        // prph_scratch control config.
        scratch.write::<u16>(0, self.hw_rev as u16); // mac_id
        scratch.write::<u16>(2, 0); // version
        scratch.write::<u16>(4, (PRPH_SCRATCH_SIZE / 4) as u16);
        let control = PRPH_SCRATCH_RB_SIZE_4K | PRPH_SCRATCH_MTR_MODE | PRPH_MTR_FORMAT_256B;
        scratch.write::<u32>(8, control);
        // pnvm_cfg at 0x10 (filled by load_pnvm), hwm_cfg at 0x20,
        // rbd_cfg at 0x30.
        put64(&scratch, 0x30, self.rx_free.phys());

        let info = DmaBuffer::new(4096).ok_or(ENOMEM)?;
        let ctxt = DmaBuffer::new(CTXT_INFO_SIZE).ok_or(ENOMEM)?;
        put64(&ctxt, 0x08, info.phys()); // prph_info_base_addr
        put64(&ctxt, 0x10, self.rb_stts.phys()); // cr_head_idx_arr
        put64(&ctxt, 0x18, info.phys() + 2048); // tr_tail_idx_arr
        put64(&ctxt, 0x20, info.phys() + 3072); // cr_tail_idx_arr
        put64(&ctxt, 0x34, self.cmdq.tfd_phys()); // mtr_base_addr
        put64(&ctxt, 0x3C, self.rx_used.phys()); // mcr_base_addr
        put(&ctxt, 0x44, &(self.cmdq.cb_size() as u16).to_le_bytes()); // mtr_size
        put(
            &ctxt,
            0x46,
            &(RX_RING.trailing_zeros() as u16).to_le_bytes(),
        ); // mcr_size
        put64(&ctxt, 0x58, scratch.phys()); // prph_scratch_base_addr
        put(&ctxt, 0x60, &(PRPH_SCRATCH_SIZE as u32).to_le_bytes());

        let iml = DmaBuffer::new(fw.iml.len()).ok_or(ENOMEM)?;
        put(&iml, 0, &fw.iml);

        // Only ALIVE and RX until the firmware is up.
        self.w(CSR_INT_MASK, INT_ALIVE | INT_FH_RX);
        self.w64(CSR_CTXT_INFO_ADDR, ctxt.phys());
        self.w64(CSR_IML_DATA_ADDR, iml.phys());
        self.w(CSR_IML_SIZE_ADDR, fw.iml.len() as u32);
        self.set(CSR_HW_IF_CONFIG_REG, CTXT_INFO_AUTO_FUNC_BOOT_ENA);

        let keep_busy = if self.integrated {
            self.w(CSR_MSIX_HW_INT_CAUSES_AD, MSIX_HW_INT_IML);
            true
        } else {
            // LTR ~250 us (boot ROM workaround for discrete parts).
            let ltr = 0x8000_0000 | (2 << 26) | (250 << 16) | 0x8000 | (2 << 10) | 250;
            self.w(CSR_LTR_LONG_VAL_AD, ltr);
            false
        };
        self.write_umac_prph(UREG_CPU_INIT_RUN, 1);
        if keep_busy {
            crate::time::wait_until(100, || {
                self.r(CSR_MSIX_HW_INT_CAUSES_AD) & MSIX_HW_INT_IML != 0
            });
        }

        let alive = crate::time::wait_until(1000, || {
            self.r(CSR_INT) & (INT_ALIVE | INT_SW_ERR | INT_HW_ERR) != 0
        });
        let ints = self.r(CSR_INT);
        self.w(CSR_INT, ints);
        drop(iml);
        self.ctxt = Some(ctxt);
        self.prph_scratch = Some(scratch);
        self.prph_info = Some(info);
        self.fw_dram = dram;
        if !alive || ints & INT_ALIVE == 0 {
            crate::println!(
                "[iwlwifi] no ALIVE interrupt (CSR_INT {:#x}, GP_CNTRL {:#x}, SecBoot CPU1 {:#x} CPU2 {:#x})",
                ints,
                self.r(CSR_GP_CNTRL),
                self.read_umac_prph(UMAG_SB_CPU_1_STATUS),
                self.read_umac_prph(UMAG_SB_CPU_2_STATUS)
            );
            return Err(ETIMEDOUT);
        }
        // The firmware configured the RX DMA engine; post buffers.
        self.restock_all();
        Ok(())
    }

    /// Hand the PNVM to the firmware and ring its doorbell.
    pub fn load_pnvm(&mut self, data: &[u8]) -> KResult<()> {
        let b = DmaBuffer::new(data.len()).ok_or(ENOMEM)?;
        put(&b, 0, data);
        let scratch = self.prph_scratch.as_ref().ok_or(EIO)?;
        put64(scratch, 0x10, b.phys());
        scratch.write::<u32>(0x18, data.len() as u32);
        self.pnvm = Some(b);
        self.write_umac_prph(UREG_DOORBELL_TO_ISR6, UREG_DOORBELL_TO_ISR6_PNVM);
        Ok(())
    }

    /// Firmware is fully up: release the image copies and enable the
    /// interrupts the driver handles.
    pub fn fw_alive(&mut self) {
        self.fw_dram.clear();
        self.w(CSR_INT, 0xFFFF_FFFF);
        self.unmask();
    }

    /// Re-enable interrupts (the handler masks them until serviced).
    pub fn unmask(&self) {
        self.w(
            CSR_INT_MASK,
            INT_FH_RX | INT_SW_RX | INT_FH_TX | INT_SW_ERR | INT_HW_ERR | INT_RF_KILL | INT_ALIVE,
        );
    }

    fn restock_all(&mut self) {
        for i in 0..RX_BUFS {
            self.post_rb(i as u16 + 1);
        }
        self.rx_stocked = true;
        self.update_rx_wptr();
    }

    fn post_rb(&mut self, vid: u16) {
        let d = self.rx_write * 16;
        self.rx_free.write::<u16>(d, vid);
        put64(
            &self.rx_free,
            d + 8,
            self.rx_bufs.phys() + (vid as u64 - 1) * RB_SIZE as u64,
        );
        self.rx_write = (self.rx_write + 1) % RX_RING;
    }

    fn update_rx_wptr(&self) {
        fence(Ordering::SeqCst);
        self.w(RFH_Q0_FRBDCB_WIDX_TRG, (self.rx_write & !7) as u32);
    }

    /// Acknowledge interrupt causes; returns them.
    pub fn ack_interrupts(&self) -> u32 {
        let i = self.r(CSR_INT);
        if i == 0xFFFF_FFFF || i & 0xFFFF_FFF0 == 0xA5A5_A5A0 {
            return 0; // device gone / not powered
        }
        self.w(CSR_INT, i);
        if i & (INT_FH_RX | INT_SW_RX) != 0 {
            self.w(CSR_FH_INT_STATUS, 0x0003_0000 | (1 << 30));
        }
        if i & INT_FH_TX != 0 {
            self.w(CSR_FH_INT_STATUS, 0x3);
        }
        i
    }

    /// Collect every completed RX buffer into `pending`.
    pub fn rx(&mut self) {
        if !self.rx_stocked {
            return;
        }
        let closed = (self.rb_stts.read::<u16>(0) as usize & 0xFFF) % RX_RING;
        let mut n = 0;
        while self.rx_read != closed {
            let cd = self.rx_read * 32;
            let vid = self.rx_used.read::<u16>(cd + 4);
            if vid == 0 || vid as usize > RX_BUFS {
                crate::println!("[iwlwifi] bad RX buffer id {}", vid);
            } else {
                let base = (vid as usize - 1) * RB_SIZE;
                let buf = &self.rx_bufs.as_slice()[base..base + RB_SIZE];
                let len_n_flags = u32::from_le_bytes(buf[0..4].try_into().unwrap());
                let len = (len_n_flags & 0x3FFF) as usize;
                if len_n_flags != 0x5555_0000 && len >= 4 && len + 4 <= RB_SIZE {
                    self.pending.push_back(Packet {
                        cmd: buf[4],
                        group: buf[5],
                        seq: u16::from_le_bytes([buf[6], buf[7]]),
                        data: buf[8..4 + len].to_vec(),
                    });
                }
                self.post_rb(vid);
                n += 1;
            }
            self.rx_read = (self.rx_read + 1) % RX_RING;
        }
        if n > 0 {
            self.update_rx_wptr();
        }
    }

    /// Send a host command and wait for its response. Notifications that
    /// arrive meanwhile stay in `pending`.
    pub fn send_cmd(
        &mut self,
        group: u8,
        cmd: u8,
        data: &[u8],
        timeout_ms: u64,
    ) -> KResult<Vec<u8>> {
        let idx = self.cmdq.next_index();
        let seq = (idx & 0xFF) as u16;
        if crate::params::IWL_DEBUG.load(core::sync::atomic::Ordering::Relaxed) {
            crate::println!(
                "[iwlwifi] cmd {:#04x}:{:#04x} seq {} len {} {:02x?}",
                group,
                cmd,
                seq,
                data.len(),
                &data[..data.len().min(32)]
            );
        }
        let mut buf = Vec::with_capacity(8 + data.len());
        buf.push(cmd);
        buf.push(group);
        buf.extend_from_slice(&seq.to_le_bytes());
        buf.extend_from_slice(&(data.len() as u16).to_le_bytes());
        buf.push(0);
        buf.push(0);
        buf.extend_from_slice(data);
        let split = buf.len();
        self.cmdq.enqueue(&buf, split, None)?;
        self.w(HBUS_TARG_WRPTR, self.cmdq.write);
        let deadline = crate::time::Deadline::after_ms(timeout_ms);
        loop {
            self.rx();
            if let Some(pos) = self.pending.iter().position(|p| {
                !p.is_notification()
                    && p.cmd == cmd
                    && (p.seq >> 8) & 0x1F == 0
                    && p.seq & 0xFF == seq
            }) {
                let p = self.pending.remove(pos).unwrap();
                self.cmdq.reclaim_one();
                return Ok(p.data);
            }
            if let Some(pos) = self
                .pending
                .iter()
                .position(|p| p.cmd == 0x2 && p.group == 0)
            {
                let p = self.pending.remove(pos).unwrap();
                self.cmdq.reclaim_one();
                let le = |o: usize| {
                    p.data
                        .get(o..o + 4)
                        .map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()))
                };
                crate::println!(
                    "[iwlwifi] command {:02x}.{:02x} rejected: error {:#x} (cmd {:#x}, seq {:#x})",
                    group,
                    cmd,
                    le(0),
                    le(4),
                    le(12)
                );
                return Err(EIO);
            }
            if deadline.expired() {
                self.cmdq.reclaim_one();
                crate::println!("[iwlwifi] command {:02x}.{:02x} timed out", group, cmd);
                return Err(ETIMEDOUT);
            }
            if self.r(CSR_INT) & (INT_SW_ERR | INT_HW_ERR) != 0 {
                crate::println!(
                    "[iwlwifi] firmware error while waiting for {:02x}.{:02x}",
                    group,
                    cmd
                );
                return Err(EIO);
            }
            crate::time::delay_us(100);
        }
    }

    /// Wait for a notification (group, cmd); other packets stay queued.
    pub fn wait_notif(&mut self, group: u8, cmd: u8, timeout_ms: u64) -> KResult<Vec<u8>> {
        let deadline = crate::time::Deadline::after_ms(timeout_ms);
        loop {
            self.rx();
            if let Some(pos) = self
                .pending
                .iter()
                .position(|p| p.group == group && p.cmd == cmd)
            {
                return Ok(self.pending.remove(pos).unwrap().data);
            }
            if deadline.expired() {
                return Err(ETIMEDOUT);
            }
            if self.r(CSR_INT) & (INT_SW_ERR | INT_HW_ERR) != 0 {
                return Err(EIO);
            }
            crate::time::delay_us(200);
        }
    }

    /// Queue a frame on a data queue. `cmd` is the device TX command
    /// followed by the frame body; TB1 ends at `tb1_end` (the padded end
    /// of the 802.11 header), the body goes in TB2.
    pub fn tx(
        q: &mut TxQueue,
        mmio: u64,
        cmd: &[u8],
        tb1_end: usize,
        frame_len: u16,
    ) -> KResult<()> {
        // The command header carries the queue/index pair the firmware
        // echoes back in the TX response.
        let seq = ((q.id & 0x1F) << 8) | (q.next_index() as u16 & 0xFF);
        let mut c = cmd.to_vec();
        c[2..4].copy_from_slice(&seq.to_le_bytes());
        q.enqueue(&c, tb1_end, Some(frame_len))?;
        w32(mmio + HBUS_TARG_WRPTR, q.write | ((q.id as u32) << 16));
        Ok(())
    }
}
