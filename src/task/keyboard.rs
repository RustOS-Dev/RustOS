//! PS/2 set-1 keyboard decoding into terminal input.
//!
//! Scancodes queued by the IRQ handler are decoded here (in the TTY input
//! thread) into the byte sequences a VT100-style terminal would send.

use crate::sync::Mutex;
use conquer_once::spin::OnceCell;
use core::sync::atomic::{AtomicBool, Ordering};
use crossbeam_queue::ArrayQueue;
use pc_keyboard::{DecodedKey, HandleControl, KeyCode, KeyState, Keyboard, ScancodeSet1, layouts};

static SCANCODE_QUEUE: OnceCell<ArrayQueue<u8>> = OnceCell::uninit();
static KEYBOARD_IRQ_SEEN: AtomicBool = AtomicBool::new(false);
static SHIFT: AtomicBool = AtomicBool::new(false);
static ALT: AtomicBool = AtomicBool::new(false);
/// Lock states for the keyboard LEDs (bit 0 num, 1 caps, 2 scroll).
static LOCKS: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(1);

static DECODER: Mutex<Option<Keyboard<layouts::Us104Key, ScancodeSet1>>> = Mutex::new(None);

pub fn init() {
    if SCANCODE_QUEUE.try_get().is_err() {
        let _ = SCANCODE_QUEUE.try_init_once(|| ArrayQueue::new(256));
    }
    let mut d = DECODER.lock();
    if d.is_none() {
        *d = Some(Keyboard::new(
            ScancodeSet1::new(),
            layouts::Us104Key,
            HandleControl::MapLettersToUnicode,
        ));
    }
}

/// Called by the keyboard interrupt handler. Must not block or allocate.
pub(crate) fn add_scancode(scancode: u8) {
    KEYBOARD_IRQ_SEEN.store(true, Ordering::Relaxed);
    if let Ok(queue) = SCANCODE_QUEUE.try_get() {
        let _ = queue.push(scancode);
        crate::tty::input_available();
    }
}

pub fn interrupt_input_observed() -> bool {
    KEYBOARD_IRQ_SEEN.load(Ordering::Relaxed)
}

/// A decoded key press.
pub enum Key {
    /// Bytes to feed into the terminal (up to 4).
    Bytes([u8; 4], usize),
    /// Shift+PageUp: scroll the console view back.
    ScrollUp,
    /// Shift+PageDown: scroll the console view forward.
    ScrollDown,
    /// Alt+F1..F4: show virtual console n (0-based).
    SwitchVc(usize),
}

fn bytes(s: &[u8]) -> Option<Key> {
    let mut b = [0u8; 4];
    b[..s.len()].copy_from_slice(s);
    Some(Key::Bytes(b, s.len()))
}

/// Next decoded key, or None when the queue is empty.
pub fn read_key() -> Option<Key> {
    let queue = SCANCODE_QUEUE.try_get().ok()?;
    loop {
        let sc = queue.pop()?;
        if let Some(k) = decode(sc) {
            return Some(k);
        }
    }
}

fn decode(scancode: u8) -> Option<Key> {
    let mut guard = DECODER.lock();
    let kb = guard.as_mut()?;
    let ev = kb.add_byte(scancode).ok()??;
    if matches!(ev.code, KeyCode::LShift | KeyCode::RShift) {
        SHIFT.store(ev.state == KeyState::Down, Ordering::Relaxed);
    }
    if matches!(ev.code, KeyCode::LAlt | KeyCode::RAltGr | KeyCode::RAlt2) {
        ALT.store(ev.state == KeyState::Down, Ordering::Relaxed);
    }
    if ev.state == KeyState::Down {
        let bit = match ev.code {
            KeyCode::NumpadLock => 1,
            KeyCode::CapsLock => 2,
            KeyCode::ScrollLock => 4,
            _ => 0,
        };
        if bit != 0 {
            let l = LOCKS.fetch_xor(bit, Ordering::Relaxed) ^ bit;
            crate::drivers::input::console_leds(l);
        }
    }
    let shift = SHIFT.load(Ordering::Relaxed);
    let alt = ALT.load(Ordering::Relaxed);
    let key = kb.process_keyevent(ev)?;
    match key {
        DecodedKey::Unicode('\n') | DecodedKey::Unicode('\r') => bytes(b"\r"),
        DecodedKey::Unicode('\x08') => bytes(b"\x7f"),
        DecodedKey::Unicode(c) if (c as u32) < 0x80 => bytes(&[c as u8]),
        DecodedKey::Unicode(c) => {
            let mut b = [0u8; 4];
            let s = c.encode_utf8(&mut b);
            let n = s.len();
            Some(Key::Bytes(b, n))
        }
        DecodedKey::RawKey(code) => match code {
            KeyCode::ArrowUp => bytes(b"\x1b[A"),
            KeyCode::ArrowDown => bytes(b"\x1b[B"),
            KeyCode::ArrowRight => bytes(b"\x1b[C"),
            KeyCode::ArrowLeft => bytes(b"\x1b[D"),
            KeyCode::Home => bytes(b"\x1b[H"),
            KeyCode::End => bytes(b"\x1b[F"),
            KeyCode::Insert => bytes(b"\x1b[2~"),
            KeyCode::Delete => bytes(b"\x1b[3~"),
            KeyCode::F1 if alt => Some(Key::SwitchVc(0)),
            KeyCode::F2 if alt => Some(Key::SwitchVc(1)),
            KeyCode::F3 if alt => Some(Key::SwitchVc(2)),
            KeyCode::F4 if alt => Some(Key::SwitchVc(3)),
            KeyCode::F1 => bytes(b"\x1bOP"),
            KeyCode::F2 => bytes(b"\x1bOQ"),
            KeyCode::F3 => bytes(b"\x1bOR"),
            KeyCode::F4 => bytes(b"\x1bOS"),
            KeyCode::PageUp if shift => Some(Key::ScrollUp),
            KeyCode::PageDown if shift => Some(Key::ScrollDown),
            KeyCode::PageUp => bytes(b"\x1b[5~"),
            KeyCode::PageDown => bytes(b"\x1b[6~"),
            KeyCode::Backspace => bytes(b"\x7f"),
            KeyCode::Escape => bytes(b"\x1b"),
            KeyCode::Tab => bytes(b"\t"),
            _ => None,
        },
    }
}
