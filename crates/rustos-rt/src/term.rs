//! Terminal control (termios, window size, foreground process group).

use crate::sys::{self, nr};

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Termios {
    pub iflag: u32,
    pub oflag: u32,
    pub cflag: u32,
    pub lflag: u32,
    pub line: u8,
    pub cc: [u8; 19],
}

pub const ICANON: u32 = 0o2;
pub const ECHO: u32 = 0o10;
pub const ISIG: u32 = 0o1;
pub const ICRNL: u32 = 0o400;
pub const OPOST: u32 = 0o1;

pub fn get(fd: i32) -> Option<Termios> {
    let mut t = Termios::default();
    (sys::syscall(nr::IOCTL, &[fd as usize, 0x5401, &mut t as *mut _ as usize]) == 0).then_some(t)
}

pub fn set(fd: i32, t: &Termios) {
    sys::syscall(nr::IOCTL, &[fd as usize, 0x5402, t as *const _ as usize]);
}

/// Put the terminal into raw mode, returning the previous settings.
pub fn raw_mode(fd: i32) -> Option<Termios> {
    let orig = get(fd)?;
    let mut t = orig;
    t.lflag &= !(ICANON | ECHO);
    t.iflag &= !ICRNL;
    t.cc[6] = 1; // VMIN
    t.cc[5] = 0; // VTIME
    set(fd, &t);
    Some(orig)
}

/// (rows, cols)
pub fn size(fd: i32) -> (u16, u16) {
    let mut ws = [0u16; 4];
    if sys::syscall(nr::IOCTL, &[fd as usize, 0x5413, ws.as_mut_ptr() as usize]) == 0 && ws[0] > 0 {
        (ws[0], ws[1])
    } else {
        (25, 80)
    }
}

pub fn tcgetpgrp(fd: i32) -> i32 {
    let mut pg = 0i32;
    sys::syscall(
        nr::IOCTL,
        &[fd as usize, 0x540F, &mut pg as *mut i32 as usize],
    );
    pg
}

pub fn tcsetpgrp(fd: i32, pgrp: i32) {
    sys::syscall(
        nr::IOCTL,
        &[fd as usize, 0x5410, &pgrp as *const i32 as usize],
    );
}
