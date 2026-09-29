use crate::sync::Mutex;
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, Ordering};
use x86_64::instructions::port::Port;

const COM1_PORT: u16 = 0x3f8;
const DATA_OFFSET: u16 = 0;
const INTERRUPT_ENABLE_OFFSET: u16 = 1;
const FIFO_CONTROL_OFFSET: u16 = 2;
const LINE_CONTROL_OFFSET: u16 = 3;
const MODEM_CONTROL_OFFSET: u16 = 4;
const LINE_STATUS_OFFSET: u16 = 5;
const TRANSMIT_EMPTY_BIT: u8 = 0x20;
// Bounded COM1 waits use a tiny best-effort spin window: enough for normal UARTs
// to become ready, but short enough not to stall boot on laptops without COM1.
const SERIAL_SPIN_LIMIT: usize = 10_000;

static SERIAL_LOCK: Mutex<()> = Mutex::new(());
static SERIAL_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Best-effort COM1 initialization.
///
/// Modern laptops often have no legacy UART at COM1. Port I/O is still safe in
/// ring 0, but probing libraries can fail or block on absent hardware. Keep this
/// path non-panicking so early boot never depends on QEMU-style serial hardware.
pub fn init() {
    if SERIAL_INITIALIZED.swap(true, Ordering::AcqRel) {
        return;
    }

    unsafe {
        let mut data = Port::<u8>::new(COM1_PORT + DATA_OFFSET);
        let mut interrupt_enable = Port::<u8>::new(COM1_PORT + INTERRUPT_ENABLE_OFFSET);
        let mut fifo_control = Port::<u8>::new(COM1_PORT + FIFO_CONTROL_OFFSET);
        let mut line_control = Port::<u8>::new(COM1_PORT + LINE_CONTROL_OFFSET);
        let mut modem_control = Port::<u8>::new(COM1_PORT + MODEM_CONTROL_OFFSET);

        interrupt_enable.write(0x00); // Disable interrupts
        line_control.write(0x80); // Enable DLAB
        data.write(0x03); // 38400 baud divisor low
        interrupt_enable.write(0x00); // divisor high
        line_control.write(0x03); // 8N1
        fifo_control.write(0xc7); // Enable FIFO
        modem_control.write(0x0b); // IRQs disabled, RTS/DSR set
    }
}

#[doc(hidden)]
pub fn _print(args: ::core::fmt::Arguments) {
    use x86_64::instructions::interrupts;

    interrupts::without_interrupts(|| {
        let _guard = SERIAL_LOCK.lock();
        init();
        let _ = SerialWriter.write_fmt(args);
    });
}

/// Write bytes exactly as given (terminal output already has CR/LF).
pub fn write_raw(bytes: &[u8]) {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| {
        let _guard = SERIAL_LOCK.lock();
        init();
        for &b in bytes {
            write_byte(b);
        }
    });
}

/// Write without taking the serial lock (NMI handlers: the interrupted
/// code may hold it). Output may interleave with other writers.
pub fn write_unlocked(bytes: &[u8]) {
    for &b in bytes {
        if b == b'\n' {
            write_byte(b'\r');
        }
        write_byte(b);
    }
}

pub fn is_locked() -> bool {
    SERIAL_LOCK.is_locked()
}

static RX_LOCK: Mutex<()> = Mutex::new(());
/// Set when a BREAK arrives on COM1 (the "SysRq" debug dump request).
pub static SYSRQ: AtomicBool = AtomicBool::new(false);
/// Uptime (ms) when the pending SysRq request arrived (0: none).
pub static SYSRQ_SINCE: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static RX_ENABLED: AtomicBool = AtomicBool::new(false);

/// Move received bytes from the UART to the TTY. Called from the IRQ 4
/// handler and, as a safety net, periodically by the TTY input thread: an
/// edge-triggered interrupt that is missed would otherwise leave data in
/// the FIFO (and the UART's interrupt line asserted) forever.
pub fn poll_rx() {
    if !RX_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    x86_64::instructions::interrupts::without_interrupts(|| {
        let _g = RX_LOCK.lock();
        unsafe {
            let mut lsr = Port::<u8>::new(COM1_PORT + LINE_STATUS_OFFSET);
            let mut data = Port::<u8>::new(COM1_PORT + DATA_OFFSET);
            let mut guard = 0;
            loop {
                let status = lsr.read();
                if status & 0x10 != 0 {
                    // Break condition: request a state dump (the break
                    // itself arrives as a NUL byte, which is dropped).
                    SYSRQ.store(true, Ordering::Relaxed);
                    let _ = SYSRQ_SINCE.compare_exchange(
                        0,
                        crate::time::millis().max(1),
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    );
                    crate::tty::input_available();
                    if status & 1 != 0 {
                        let _ = data.read();
                    }
                    continue;
                }
                if status & 1 == 0 || guard >= 64 {
                    break;
                }
                let b = data.read();
                crate::tty::serial_input(b);
                guard += 1;
            }
        }
    });
}

/// Enable receive interrupts on COM1 and route IRQ 4 to the TTY.
pub fn enable_rx_interrupts() {
    let Some(v) = crate::arch::x86_64::idt::alloc_vector(|_f| {
        poll_rx();
        crate::arch::x86_64::apic::eoi();
    }) else {
        return;
    };
    unsafe {
        // Absent UART: the scratch register does not hold a value.
        let mut scratch = Port::<u8>::new(COM1_PORT + 7);
        scratch.write(0x5A);
        if scratch.read() != 0x5A {
            return;
        }
        Port::<u8>::new(COM1_PORT + INTERRUPT_ENABLE_OFFSET).write(0x01);
        RX_ENABLED.store(true, Ordering::Relaxed);
        Port::<u8>::new(COM1_PORT + MODEM_CONTROL_OFFSET).write(0x0B);
    }
    crate::arch::x86_64::apic::route_isa_irq(4, v);
}

/// Write raw bytes (translating `\n` to `\r\n`).
pub fn write_bytes(bytes: &[u8]) {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| {
        let _guard = SERIAL_LOCK.lock();
        init();
        for &b in bytes {
            if b == b'\n' {
                write_byte(b'\r');
            }
            write_byte(b);
        }
    });
}

struct SerialWriter;

impl fmt::Write for SerialWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            match byte {
                b'\n' => {
                    write_byte(b'\r');
                    write_byte(b'\n');
                }
                byte => write_byte(byte),
            }
        }
        Ok(())
    }
}

fn write_byte(byte: u8) {
    unsafe {
        let mut line_status = Port::<u8>::new(COM1_PORT + LINE_STATUS_OFFSET);
        for _ in 0..SERIAL_SPIN_LIMIT {
            if line_status.read() & TRANSMIT_EMPTY_BIT != 0 {
                break;
            }
            core::hint::spin_loop();
        }
        // If absent hardware never reports ready, still attempt the byte once.
        // This keeps serial diagnostic output best-effort rather than blocking
        // boot on real laptops without COM1.
        Port::<u8>::new(COM1_PORT + DATA_OFFSET).write(byte);
    }
}

/// Prints to the host through the serial interface.
#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => {
        $crate::drivers::serial::_print(format_args!($($arg)*));
    };
}

/// Prints to the host through the serial interface, appending a newline.
#[macro_export]
macro_rules! serial_println {
    () => ($crate::serial_print!("\n"));
    ($fmt:expr) => ($crate::serial_print!(concat!($fmt, "\n")));
    ($fmt:expr, $($arg:tt)*) => ($crate::serial_print!(
        concat!($fmt, "\n"), $($arg)*));
}
