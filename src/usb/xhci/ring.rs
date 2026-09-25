//! XHCI ring management: Command Ring, Event Ring, and Transfer Rings.
//!
//! All ring memory is DMA-allocated (physically contiguous within one page).
//! Rings use a producer/consumer cycle-bit protocol to distinguish full from
//! empty and to wrap around without a separate head/tail counter.

use super::trb::{Trb, link_trb};
use crate::mm::dma_alloc;

/// Number of TRBs per ring segment (excluding the Link TRB for producer rings).
pub const RING_SIZE: usize = 64;

// ---------------------------------------------------------------------------
// Command Ring  (producer: software, consumer: xHC)
// ---------------------------------------------------------------------------

pub struct CommandRing {
    trbs: *mut Trb,
    pub phys: u64,
    enqueue: usize,
    cycle_bit: bool,
}

unsafe impl Send for CommandRing {}

impl CommandRing {
    pub fn new() -> Self {
        let size = (RING_SIZE + 1) * core::mem::size_of::<Trb>(); // +1 for Link
        let (virt, phys) = dma_alloc(size, 64);
        let trbs = virt as *mut Trb;

        // Write Link TRB at index RING_SIZE pointing back to start
        unsafe {
            let link = link_trb(phys, true); // toggle cycle
            trbs.add(RING_SIZE).write_volatile(link);
        }

        CommandRing {
            trbs,
            phys,
            enqueue: 0,
            cycle_bit: true,
        }
    }

    /// Write a TRB to the ring and advance the enqueue pointer.
    /// Returns the physical address of the slot written (for doorbell tracking).
    pub fn push(&mut self, mut trb: Trb) -> u64 {
        // Set cycle bit
        trb.set_cycle(self.cycle_bit);
        let slot_phys = self.phys + (self.enqueue * core::mem::size_of::<Trb>()) as u64;
        unsafe {
            self.trbs.add(self.enqueue).write_volatile(trb);
        }
        self.enqueue += 1;

        // Wrap around
        if self.enqueue == RING_SIZE {
            // Update Link TRB cycle bit to match current producer cycle
            unsafe {
                let link = self.trbs.add(RING_SIZE);
                let mut lt = link.read_volatile();
                lt.set_cycle(self.cycle_bit);
                link.write_volatile(lt);
            }
            self.enqueue = 0;
            self.cycle_bit = !self.cycle_bit;
        }
        slot_phys
    }
}

impl Default for CommandRing {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Event Ring  (producer: xHC, consumer: software)
// ---------------------------------------------------------------------------

/// Event Ring Segment Table Entry (16 bytes, spec section 6.5)
#[repr(C, align(64))]
pub struct ErstEntry {
    pub base_addr: u64,
    pub size: u16,
    _pad: [u16; 3],
}

pub struct EventRing {
    trbs: *mut Trb,
    pub phys: u64,
    #[allow(dead_code)]
    erst: *mut ErstEntry,
    pub erst_phys: u64,
    dequeue: usize,
    cycle_bit: bool,
}

unsafe impl Send for EventRing {}

impl EventRing {
    pub fn new() -> Self {
        let seg_size = RING_SIZE * core::mem::size_of::<Trb>();
        let (seg_virt, seg_phys) = dma_alloc(seg_size, 64);

        let (erst_virt, erst_phys) = dma_alloc(core::mem::size_of::<ErstEntry>(), 64);
        let erst = erst_virt as *mut ErstEntry;
        unsafe {
            (*erst).base_addr = seg_phys;
            (*erst).size = RING_SIZE as u16;
        }

        EventRing {
            trbs: seg_virt as *mut Trb,
            phys: seg_phys,
            erst,
            erst_phys,
            dequeue: 0,
            cycle_bit: true,
        }
    }

    /// Pop one event TRB if available.  Returns `None` if the ring is empty.
    pub fn pop(&mut self) -> Option<Trb> {
        let trb = unsafe { self.trbs.add(self.dequeue).read_volatile() };
        if trb.cycle_bit() != self.cycle_bit {
            return None; // no new event
        }
        self.dequeue += 1;
        if self.dequeue >= RING_SIZE {
            self.dequeue = 0;
            self.cycle_bit = !self.cycle_bit;
        }
        Some(trb)
    }

    /// Physical address the xHC should use to update ERDP (dequeue pointer).
    pub fn dequeue_phys(&self) -> u64 {
        self.phys + (self.dequeue * core::mem::size_of::<Trb>()) as u64
    }
}

impl Default for EventRing {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Transfer Ring  (one per endpoint)
// ---------------------------------------------------------------------------

pub struct TransferRing {
    trbs: *mut Trb,
    pub phys: u64,
    enqueue: usize,
    cycle_bit: bool,
}

unsafe impl Send for TransferRing {}

impl TransferRing {
    pub fn new() -> Self {
        let size = (RING_SIZE + 1) * core::mem::size_of::<Trb>();
        let (virt, phys) = dma_alloc(size, 64);
        let trbs = virt as *mut Trb;
        unsafe {
            let link = link_trb(phys, true);
            trbs.add(RING_SIZE).write_volatile(link);
        }
        TransferRing {
            trbs,
            phys,
            enqueue: 0,
            cycle_bit: true,
        }
    }

    pub fn push(&mut self, mut trb: Trb) {
        trb.set_cycle(self.cycle_bit);
        unsafe {
            self.trbs.add(self.enqueue).write_volatile(trb);
        }
        self.enqueue += 1;
        if self.enqueue == RING_SIZE {
            unsafe {
                let link = self.trbs.add(RING_SIZE);
                let mut lt = link.read_volatile();
                lt.set_cycle(self.cycle_bit);
                link.write_volatile(lt);
            }
            self.enqueue = 0;
            self.cycle_bit = !self.cycle_bit;
        }
    }
}

impl Default for TransferRing {
    fn default() -> Self {
        Self::new()
    }
}
