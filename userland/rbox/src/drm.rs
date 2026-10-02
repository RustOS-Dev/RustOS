//! drmtest: drive a DRM/KMS device (/dev/dri/cardN) the way a display
//! server does: find a connected connector and its mode, put a dumb
//! buffer on the screen, then page-flip to a second one and wait for the
//! flip's event.

use crate::err;
use rustos_rt::fs;
use rustos_rt::io::{PollFd, poll};
use rustos_rt::prelude::*;
use rustos_rt::sys::{check, nr, syscall};
use rustos_rt::time;

/// _IOWR('d', nr, size)
fn iowr(nr: u8, size: usize) -> u64 {
    ((3usize << 30) | (size << 16) | ((b'd' as usize) << 8) | nr as usize) as u64
}

fn ioctl(f: &fs::File, nr: u8, buf: &mut [u8]) -> rustos_rt::Result<usize> {
    f.ioctl(iowr(nr, buf.len()), buf.as_mut_ptr() as usize)
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes(b[o..o + 2].try_into().unwrap())
}

fn put_u32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

fn put_u64(b: &mut [u8], o: usize, v: u64) {
    b[o..o + 8].copy_from_slice(&v.to_le_bytes());
}

const VERSION: u8 = 0x00;
const GETRESOURCES: u8 = 0xA0;
const SETCRTC: u8 = 0xA2;
const GETENCODER: u8 = 0xA6;
const GETCONNECTOR: u8 = 0xA7;
const ADDFB: u8 = 0xAE;
const RMFB: u8 = 0xAF;
const PAGE_FLIP: u8 = 0xB0;
const CREATE_DUMB: u8 = 0xB2;
const MAP_DUMB: u8 = 0xB3;
const DESTROY_DUMB: u8 = 0xB4;

const MODEINFO: usize = 68;
const CONNECTED: u32 = 1;
const PAGE_FLIP_EVENT: u32 = 1;
const EVENT_FLIP_COMPLETE: u32 = 2;

fn ids(n: u32) -> Vec<u32> {
    vec![0u32; n as usize]
}

/// A dumb buffer mapped into our address space, with a framebuffer.
struct Buffer {
    handle: u32,
    fb: u32,
    pitch: u32,
    addr: usize,
    size: usize,
}

fn create_buffer(f: &fs::File, w: u32, h: u32) -> rustos_rt::Result<Buffer> {
    let mut c = [0u8; 32];
    put_u32(&mut c, 0, h);
    put_u32(&mut c, 4, w);
    put_u32(&mut c, 8, 32);
    ioctl(f, CREATE_DUMB, &mut c)?;
    let (handle, pitch) = (u32_at(&c, 16), u32_at(&c, 20));
    let size = u64::from_le_bytes(c[24..32].try_into().unwrap()) as usize;
    let mut m = [0u8; 16];
    put_u32(&mut m, 0, handle);
    ioctl(f, MAP_DUMB, &mut m)?;
    let offset = u64::from_le_bytes(m[8..16].try_into().unwrap()) as usize;
    // PROT_READ|PROT_WRITE, MAP_SHARED.
    let addr = check(syscall(nr::MMAP, &[0, size, 3, 1, f.fd() as usize, offset]))?;
    let mut fb = [0u8; 28];
    put_u32(&mut fb, 4, w);
    put_u32(&mut fb, 8, h);
    put_u32(&mut fb, 12, pitch);
    put_u32(&mut fb, 16, 32);
    put_u32(&mut fb, 20, 24);
    put_u32(&mut fb, 24, handle);
    ioctl(f, ADDFB, &mut fb)?;
    Ok(Buffer {
        handle,
        fb: u32_at(&fb, 0),
        pitch,
        addr,
        size,
    })
}

impl Buffer {
    /// Fill with XRGB8888 `color`, with a white frame one pixel wide.
    fn fill(&self, w: u32, h: u32, color: u32) {
        for y in 0..h {
            let row = (self.addr + (y * self.pitch) as usize) as *mut u32;
            for x in 0..w {
                let edge = x == 0 || y == 0 || x == w - 1 || y == h - 1;
                unsafe {
                    row.add(x as usize)
                        .write_volatile(if edge { 0xffffff } else { color })
                };
            }
        }
    }

    fn destroy(&self, f: &fs::File) {
        let _ = syscall(nr::MUNMAP, &[self.addr, self.size]);
        let mut id = self.fb.to_le_bytes();
        let _ = ioctl(f, RMFB, &mut id);
        let mut d = self.handle.to_le_bytes();
        let _ = ioctl(f, DESTROY_DUMB, &mut d);
    }
}

pub fn drmtest(args: &[String]) -> i32 {
    let mut dev = String::from("/dev/dri/card0");
    let mut hold = 0u64;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-d" if i + 1 < args.len() => {
                dev = args[i + 1].clone();
                i += 1;
            }
            "-t" if i + 1 < args.len() => {
                hold = args[i + 1].parse().unwrap_or(0);
                i += 1;
            }
            _ => {
                eprintln!("usage: drmtest [-d /dev/dri/cardN] [-t seconds to show]");
                return 2;
            }
        }
        i += 1;
    }
    let f = match fs::File::open_with(&dev, fs::O_RDWR, 0) {
        Ok(f) => f,
        Err(e) => return err("drmtest", &dev, e),
    };
    // Driver name.
    let mut name = [0u8; 64];
    let mut v = [0u8; 64];
    put_u64(&mut v, 16, name.len() as u64);
    put_u64(&mut v, 24, name.as_mut_ptr() as u64);
    if let Err(e) = ioctl(&f, VERSION, &mut v) {
        return err("drmtest", "DRM_IOCTL_VERSION", e);
    }
    let nlen = (u64::from_le_bytes(v[16..24].try_into().unwrap()) as usize).min(name.len());
    let driver = String::from_utf8_lossy(&name[..nlen]).into_owned();

    // Resources: counts first, then the ids.
    let mut res = [0u8; 64];
    if let Err(e) = ioctl(&f, GETRESOURCES, &mut res) {
        return err("drmtest", "DRM_IOCTL_MODE_GETRESOURCES", e);
    }
    let (mut crtcs, mut conns, mut encs) = (
        ids(u32_at(&res, 36)),
        ids(u32_at(&res, 40)),
        ids(u32_at(&res, 44)),
    );
    put_u32(&mut res, 32, 0);
    put_u64(&mut res, 0, 0);
    put_u64(&mut res, 8, crtcs.as_mut_ptr() as u64);
    put_u64(&mut res, 16, conns.as_mut_ptr() as u64);
    put_u64(&mut res, 24, encs.as_mut_ptr() as u64);
    if let Err(e) = ioctl(&f, GETRESOURCES, &mut res) {
        return err("drmtest", "DRM_IOCTL_MODE_GETRESOURCES", e);
    }
    println!(
        "{}: {} ({} CRTCs, {} connectors, {} encoders)",
        dev,
        driver,
        crtcs.len(),
        conns.len(),
        encs.len()
    );

    // The first connected connector with modes; its preferred (first) mode.
    let mut chosen = None;
    for &id in &conns {
        let mut c = [0u8; 80];
        put_u32(&mut c, 48, id);
        if ioctl(&f, GETCONNECTOR, &mut c).is_err() {
            continue;
        }
        let nmodes = u32_at(&c, 32);
        let mut modes = vec![0u8; MODEINFO * nmodes as usize];
        let mut cencs = ids(u32_at(&c, 40));
        let mut c2 = [0u8; 80];
        put_u32(&mut c2, 48, id);
        put_u64(&mut c2, 0, cencs.as_mut_ptr() as u64);
        put_u64(&mut c2, 8, modes.as_mut_ptr() as u64);
        put_u32(&mut c2, 32, nmodes);
        put_u32(&mut c2, 40, cencs.len() as u32);
        if ioctl(&f, GETCONNECTOR, &mut c2).is_err() {
            continue;
        }
        let state = u32_at(&c2, 60);
        println!(
            "  connector {}: type {}, {}, {} modes",
            id,
            u32_at(&c2, 52),
            if state == CONNECTED {
                "connected"
            } else {
                "disconnected"
            },
            nmodes
        );
        if state == CONNECTED && nmodes > 0 && chosen.is_none() {
            chosen = Some((id, modes[..MODEINFO].to_vec(), u32_at(&c2, 44), cencs));
        }
    }
    let Some((conn, mode, enc_id, cencs)) = chosen else {
        eprintln!("drmtest: no connected connector");
        return 1;
    };
    // A CRTC: the encoder's current one, else the first it can drive.
    let enc = if enc_id != 0 {
        enc_id
    } else {
        cencs.first().copied().unwrap_or(0)
    };
    let mut e = [0u8; 20];
    put_u32(&mut e, 0, enc);
    let _ = ioctl(&f, GETENCODER, &mut e);
    let crtc = match u32_at(&e, 8) {
        0 => {
            let possible = u32_at(&e, 12);
            match (0..crtcs.len()).find(|&i| possible & (1 << i) != 0) {
                Some(i) => crtcs[i],
                None => crtcs.first().copied().unwrap_or(0),
            }
        }
        c => c,
    };
    let (w, h) = (u16_at(&mode, 4) as u32, u16_at(&mode, 14) as u32);
    let refresh = u32_at(&mode, 24);
    println!("  mode {}x{}@{} on CRTC {}", w, h, refresh, crtc);

    let bufs = match (create_buffer(&f, w, h), create_buffer(&f, w, h)) {
        (Ok(a), Ok(b)) => [a, b],
        (Err(e), _) | (_, Err(e)) => return err("drmtest", "dumb buffer", e),
    };
    bufs[0].fill(w, h, 0x00c00000);
    bufs[1].fill(w, h, 0x000000c0);

    let mut set = [0u8; 104];
    let mut connector = conn;
    put_u64(&mut set, 0, &mut connector as *mut u32 as u64);
    put_u32(&mut set, 8, 1);
    put_u32(&mut set, 12, crtc);
    put_u32(&mut set, 16, bufs[0].fb);
    put_u32(&mut set, 32, 1);
    set[36..36 + MODEINFO].copy_from_slice(&mode);
    if let Err(e) = ioctl(&f, SETCRTC, &mut set) {
        return err("drmtest", "DRM_IOCTL_MODE_SETCRTC", e);
    }
    println!("  showing red");

    let mut flip = [0u8; 24];
    put_u32(&mut flip, 0, crtc);
    put_u32(&mut flip, 4, bufs[1].fb);
    put_u32(&mut flip, 8, PAGE_FLIP_EVENT);
    put_u64(&mut flip, 16, 0x5eed);
    if let Err(e) = ioctl(&f, PAGE_FLIP, &mut flip) {
        return err("drmtest", "DRM_IOCTL_MODE_PAGE_FLIP", e);
    }
    let mut pfd = [PollFd {
        fd: f.fd(),
        events: 1,
        revents: 0,
    }];
    let mut ev = [0u8; 32];
    let got =
        matches!(poll(&mut pfd, 2000), Ok(n) if n > 0) && f.read(&mut ev).is_ok_and(|n| n >= 32);
    if !got || u32_at(&ev, 0) != EVENT_FLIP_COMPLETE || ev[8..16] != 0x5eedu64.to_le_bytes() {
        eprintln!("drmtest: no page flip event");
        return 1;
    }
    println!("  flipped to blue (event for CRTC {})", u32_at(&ev, 28));
    if hold > 0 {
        let end = time::millis() + hold * 1000;
        while time::millis() < end {
            time::sleep_ms(100);
        }
    }
    for b in &bufs {
        b.destroy(&f);
    }
    println!("drmtest: ok");
    0
}
