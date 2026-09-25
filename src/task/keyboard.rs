use conquer_once::spin::OnceCell;
use core::sync::atomic::{AtomicBool, Ordering};
use crossbeam_queue::ArrayQueue;
use lazy_static::lazy_static;
use pc_keyboard::{DecodedKey, HandleControl, KeyCode, Keyboard, ScancodeSet1, layouts};
use spin::Mutex;

static SCANCODE_QUEUE: OnceCell<ArrayQueue<u8>> = OnceCell::uninit();
static KEYBOARD_IRQ_SEEN: AtomicBool = AtomicBool::new(false);
lazy_static! {
    static ref KEYBOARD_DECODER: Mutex<Keyboard<layouts::Us104Key, ScancodeSet1>> =
        Mutex::new(Keyboard::new(
            ScancodeSet1::new(),
            layouts::Us104Key,
            HandleControl::MapLettersToUnicode,
        ));
}

pub fn init() {
    if SCANCODE_QUEUE.try_get().is_err() {
        SCANCODE_QUEUE
            .try_init_once(|| ArrayQueue::new(100))
            .expect("failed to initialize keyboard scancode queue");
    }
}

/// Called by the keyboard interrupt handler
///
/// Must not block or allocate.
pub(crate) fn add_scancode(scancode: u8) {
    KEYBOARD_IRQ_SEEN.store(true, Ordering::Relaxed);

    if let Ok(queue) = SCANCODE_QUEUE.try_get() {
        let _ = queue.push(scancode);
        crate::tty::input_available();
    }
}

pub fn read_input_byte() -> Option<u8> {
    read_input_event().and_then(|e| match e {
        InputEvent::Char(b) => Some(b),
        _ => None,
    })
}

/// An input event decoded from the PS/2 keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    /// A printable ASCII character or a control byte (e.g. `0x08` for Backspace).
    Char(u8),
    /// The Up arrow key was pressed.
    ArrowUp,
    /// The Down arrow key was pressed.
    ArrowDown,
}

/// Returns the next available input event, or `None` if the queue is empty.
pub fn read_input_event() -> Option<InputEvent> {
    let queue = SCANCODE_QUEUE.try_get().ok()?;
    let scancode = queue.pop().or_else(poll_ps2_scancode)?;
    decode_event(scancode)
}

fn decode_event(scancode: u8) -> Option<InputEvent> {
    let mut keyboard = KEYBOARD_DECODER.lock();
    let key_event = keyboard.add_byte(scancode).ok()??;
    let key = keyboard.process_keyevent(key_event)?;
    match key {
        // Ctrl+C (ETX, 0x03) — pass through so the shell can clear the line.
        DecodedKey::Unicode('\x03') => Some(InputEvent::Char(0x03)),
        // Backspace / DEL — must be matched before the control-character filter below.
        DecodedKey::Unicode('\x08') | DecodedKey::Unicode('\x7f') => Some(InputEvent::Char(0x08)),
        DecodedKey::Unicode(c) if c.is_ascii() && !c.is_ascii_control() => {
            Some(InputEvent::Char(c as u8))
        }
        DecodedKey::Unicode('\n') | DecodedKey::Unicode('\r') => Some(InputEvent::Char(b'\n')),
        DecodedKey::RawKey(KeyCode::Backspace) => Some(InputEvent::Char(0x08)),
        DecodedKey::RawKey(KeyCode::ArrowUp) => Some(InputEvent::ArrowUp),
        DecodedKey::RawKey(KeyCode::ArrowDown) => Some(InputEvent::ArrowDown),
        _ => None,
    }
}

pub fn interrupt_input_observed() -> bool {
    KEYBOARD_IRQ_SEEN.load(Ordering::Relaxed)
}

fn poll_ps2_scancode() -> Option<u8> {
    use x86_64::instructions::port::Port;

    unsafe {
        let mut status_port = Port::<u8>::new(0x64);
        let status = status_port.read();
        if status & 0x01 == 0 {
            return None;
        }

        let mut data_port = Port::<u8>::new(0x60);
        let scancode = data_port.read();

        // Bit 5 indicates auxiliary PS/2 mouse data. The shell only decodes
        // keyboard set-1 scancodes, so ignore mouse bytes if firmware enables
        // the auxiliary device.
        if status & 0x20 != 0 {
            return None;
        }

        Some(scancode)
    }
}
