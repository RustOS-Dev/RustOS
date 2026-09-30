//! H4 (UART) transport on a legacy serial port: `bt attach com2`, or
//! `bt.h4=com2` in kernel.conf. Each packet is a type byte followed by
//! the HCI packet. The port runs at 115200 8N1; its receive interrupt
//! (IRQ 3 for COM2) fills a buffer that a reader thread parses (COM3 and
//! COM4, which share their IRQs, are polled).

use super::{Hci, Transport};
use crate::errno::*;
use crate::sched::WaitQueue;
use crate::sync::Mutex;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use bt::hci;
use x86_64::instructions::port::Port;

struct Uart {
    base: u16,
    name: &'static str,
    tx: Mutex<()>,
    /// Bytes taken from the FIFO by the interrupt handler.
    rx: Mutex<VecDeque<u8>>,
    wq: WaitQueue,
}

static ATTACHED: Mutex<Vec<u16>> = Mutex::new(Vec::new());

/// (I/O base, name, IRQ if not shared with the console port).
fn port_of(name: &str) -> Option<(u16, &'static str, Option<u8>)> {
    Some(match name.trim_start_matches("/dev/") {
        "com2" | "ttyS1" => (0x2F8, "com2", Some(3)),
        "com3" | "ttyS2" => (0x3E8, "com3", None),
        "com4" | "ttyS3" => (0x2E8, "com4", None),
        _ => return None,
    })
}

impl Uart {
    fn reg(&self, off: u16) -> Port<u8> {
        Port::new(self.base + off)
    }

    fn init(&self) -> bool {
        unsafe {
            // Is anything there? The scratch register holds a value.
            self.reg(7).write(0x5A);
            if self.reg(7).read() != 0x5A {
                return false;
            }
            self.reg(1).write(0x00); // no interrupts
            self.reg(3).write(0x80); // DLAB
            self.reg(0).write(0x01); // 115200 baud
            self.reg(1).write(0x00);
            self.reg(3).write(0x03); // 8N1
            self.reg(2).write(0xC7); // FIFO on, cleared, 14-byte threshold
            self.reg(4).write(0x0B); // DTR, RTS, OUT2 (interrupt line)
        }
        true
    }

    /// Move whatever the FIFO holds into the receive buffer.
    fn drain(&self) {
        x86_64::instructions::interrupts::without_interrupts(|| {
            let mut rx = self.rx.lock();
            let mut guard = 0;
            while self.readable() && guard < 4096 {
                rx.push_back(self.read_byte());
                guard += 1;
            }
        });
    }

    fn readable(&self) -> bool {
        unsafe { self.reg(5).read() & 0x01 != 0 }
    }

    fn read_byte(&self) -> u8 {
        unsafe { self.reg(0).read() }
    }

    fn write_byte(&self, b: u8) {
        let deadline = crate::time::Deadline::after_ms(100);
        unsafe {
            while self.reg(5).read() & 0x20 == 0 {
                if deadline.expired() {
                    return;
                }
                core::hint::spin_loop();
            }
            self.reg(0).write(b);
        }
    }
}

impl Transport for Uart {
    fn send(&self, kind: u8, pkt: &[u8]) -> KResult<()> {
        let _g = self.tx.lock();
        self.write_byte(kind);
        for &b in pkt {
            self.write_byte(b);
        }
        Ok(())
    }

    fn name(&self) -> String {
        alloc::format!("UART {}", self.name)
    }
}

/// Start a controller on a serial port.
pub fn attach(port: &str) -> KResult<()> {
    let (base, name, irq) = port_of(port).ok_or(EINVAL)?;
    {
        let mut a = ATTACHED.lock();
        if a.contains(&base) {
            return Err(EBUSY);
        }
        a.push(base);
    }
    let u = Arc::new(Uart {
        base,
        name,
        tx: Mutex::new(()),
        rx: Mutex::new(VecDeque::new()),
        wq: WaitQueue::new(),
    });
    if !u.init() {
        ATTACHED.lock().retain(|b| *b != base);
        return Err(ENODEV);
    }
    if let Some(irq) = irq {
        let ui = u.clone();
        if let Some(v) = crate::arch::x86_64::idt::alloc_vector(move |_f| {
            ui.drain();
            ui.wq.wake_all();
            crate::arch::x86_64::apic::eoi();
        }) {
            unsafe { u.reg(1).write(0x01) }; // received-data interrupt
            crate::arch::x86_64::apic::route_isa_irq(irq, v);
        }
    }
    // Start reading before the controller is initialised (it answers the
    // first commands).
    let hci_slot: Arc<Mutex<Option<Arc<Hci>>>> = Arc::new(Mutex::new(None));
    let (u2, slot2) = (u.clone(), hci_slot.clone());
    crate::sched::spawn(&alloc::format!("h4-{}", name), move || reader(&u2, &slot2));
    let hci = super::register(u);
    *hci_slot.lock() = Some(hci);
    Ok(())
}

/// Parse the H4 byte stream into packets.
fn reader(u: &Uart, slot: &Mutex<Option<Arc<Hci>>>) {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        // The interrupt normally wakes us; polling covers a lost one (and
        // ports without an interrupt).
        u.wq.wait_timeout(50, || {
            x86_64::instructions::interrupts::without_interrupts(|| !u.rx.lock().is_empty())
        });
        u.drain();
        x86_64::instructions::interrupts::without_interrupts(|| buf.extend(u.rx.lock().drain(..)));
        while let Some(&kind) = buf.first() {
            let need = match kind {
                hci::H4_EVENT if buf.len() >= 3 => 3 + buf[2] as usize,
                hci::H4_ACL if buf.len() >= 5 => 5 + u16::from_le_bytes([buf[3], buf[4]]) as usize,
                hci::H4_EVENT | hci::H4_ACL => usize::MAX,
                _ => {
                    // Out of sync: drop a byte.
                    buf.remove(0);
                    continue;
                }
            };
            if buf.len() < need {
                break;
            }
            let pkt: Vec<u8> = buf.drain(..need).collect();
            if let Some(h) = slot.lock().clone() {
                h.on_packet(kind, &pkt[1..]);
            }
        }
    }
}
