//! vgrab: capture frames from a V4L2 camera (/dev/videoN) through
//! memory-mapped buffers, print what the device is and what it sent, and
//! save the last frame.

use crate::err;
use rustos_rt::fs;
use rustos_rt::io::{PollFd, poll};
use rustos_rt::prelude::*;
use rustos_rt::sys::{check, nr, syscall};

const BUF_TYPE_CAPTURE: u32 = 1;
const MEMORY_MMAP: u32 = 1;
const CAP_VIDEO_CAPTURE: u32 = 1;
const CAP_STREAMING: u32 = 0x0400_0000;
const BUFFERS: u32 = 4;

/// _IOC(dir, 'V', nr, size): dir 1 write, 2 read, 3 both.
fn vidioc(dir: usize, nr: u8, size: usize) -> u64 {
    ((dir << 30) | (size << 16) | ((b'V' as usize) << 8) | nr as usize) as u64
}

const QUERYCAP: (usize, u8, usize) = (2, 0, 104);
const ENUM_FMT: (usize, u8, usize) = (3, 2, 64);
const G_FMT: (usize, u8, usize) = (3, 4, 208);
const REQBUFS: (usize, u8, usize) = (3, 8, 20);
const QUERYBUF: (usize, u8, usize) = (3, 9, 88);
const QBUF: (usize, u8, usize) = (3, 15, 88);
const DQBUF: (usize, u8, usize) = (3, 17, 88);
const STREAMON: (usize, u8, usize) = (1, 18, 4);
const STREAMOFF: (usize, u8, usize) = (1, 19, 4);

fn ioctl(f: &fs::File, req: (usize, u8, usize), buf: &mut [u8]) -> rustos_rt::Result<usize> {
    debug_assert!(buf.len() >= req.2);
    f.ioctl(vidioc(req.0, req.1, req.2), buf.as_mut_ptr() as usize)
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn put_u32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

fn fourcc(v: u32) -> String {
    v.to_le_bytes()
        .iter()
        .map(|&c| if c.is_ascii_graphic() { c as char } else { '?' })
        .collect()
}

/// A struct v4l2_buffer for capture/mmap with `index`.
fn buffer(index: u32) -> [u8; 88] {
    let mut b = [0u8; 88];
    put_u32(&mut b, 0, index);
    put_u32(&mut b, 4, BUF_TYPE_CAPTURE);
    put_u32(&mut b, 60, MEMORY_MMAP);
    b
}

struct Mapping {
    addr: usize,
    len: usize,
}

impl Drop for Mapping {
    fn drop(&mut self) {
        let _ = syscall(nr::MUNMAP, &[self.addr, self.len]);
    }
}

pub fn vgrab(args: &[String]) -> i32 {
    let mut dev = String::from("/dev/video0");
    let mut out = String::from("/tmp/vgrab.raw");
    let mut frames = 10u32;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-d" if i + 1 < args.len() => {
                dev = args[i + 1].clone();
                i += 1;
            }
            "-o" if i + 1 < args.len() => {
                out = args[i + 1].clone();
                i += 1;
            }
            "-n" if i + 1 < args.len() => {
                frames = args[i + 1].parse().unwrap_or(10).max(1);
                i += 1;
            }
            _ => {
                eprintln!("usage: vgrab [-d /dev/videoN] [-n frames] [-o file]");
                return 2;
            }
        }
        i += 1;
    }
    let f = match fs::File::open_with(&dev, fs::O_RDWR, 0) {
        Ok(f) => f,
        Err(e) => return err("vgrab", &dev, e),
    };
    let mut cap = [0u8; 104];
    if let Err(e) = ioctl(&f, QUERYCAP, &mut cap) {
        return err("vgrab", "VIDIOC_QUERYCAP", e);
    }
    let caps = u32_at(&cap, 88);
    println!(
        "{}: {} ({}, {})",
        dev,
        cstr(&cap[16..48]),
        cstr(&cap[0..16]),
        cstr(&cap[48..80])
    );
    if caps & CAP_VIDEO_CAPTURE == 0 || caps & CAP_STREAMING == 0 {
        // UVC cameras also have metadata nodes: nothing to capture there.
        println!("{}: not a streaming capture device", dev);
        return 0;
    }
    for index in 0.. {
        let mut d = [0u8; 64];
        put_u32(&mut d, 0, index);
        put_u32(&mut d, 4, BUF_TYPE_CAPTURE);
        if ioctl(&f, ENUM_FMT, &mut d).is_err() {
            break;
        }
        println!("  format {}: {}", fourcc(u32_at(&d, 44)), cstr(&d[12..44]));
    }
    let mut fmt = [0u8; 208];
    put_u32(&mut fmt, 0, BUF_TYPE_CAPTURE);
    if let Err(e) = ioctl(&f, G_FMT, &mut fmt) {
        return err("vgrab", "VIDIOC_G_FMT", e);
    }
    let (w, h, pix) = (u32_at(&fmt, 8), u32_at(&fmt, 12), u32_at(&fmt, 16));
    println!("  capturing {}x{} {}", w, h, fourcc(pix));

    let mut req = [0u8; 20];
    put_u32(&mut req, 0, BUFFERS);
    put_u32(&mut req, 4, BUF_TYPE_CAPTURE);
    put_u32(&mut req, 8, MEMORY_MMAP);
    if let Err(e) = ioctl(&f, REQBUFS, &mut req) {
        return err("vgrab", "VIDIOC_REQBUFS", e);
    }
    let count = u32_at(&req, 0);
    let mut maps = Vec::new();
    for index in 0..count {
        let mut b = buffer(index);
        if let Err(e) = ioctl(&f, QUERYBUF, &mut b) {
            return err("vgrab", "VIDIOC_QUERYBUF", e);
        }
        let (offset, len) = (u32_at(&b, 64) as usize, u32_at(&b, 72) as usize);
        // PROT_READ|PROT_WRITE, MAP_SHARED.
        let addr = match check(syscall(nr::MMAP, &[0, len, 3, 1, f.fd() as usize, offset])) {
            Ok(a) => a,
            Err(e) => return err("vgrab", "mmap", e),
        };
        maps.push(Mapping { addr, len });
        if let Err(e) = ioctl(&f, QBUF, &mut b) {
            return err("vgrab", "VIDIOC_QBUF", e);
        }
    }
    let mut ty = BUF_TYPE_CAPTURE.to_le_bytes();
    if let Err(e) = ioctl(&f, STREAMON, &mut ty) {
        return err("vgrab", "VIDIOC_STREAMON", e);
    }
    let mut got = 0;
    let mut last = Vec::new();
    let mut status = 0;
    while got < frames {
        let mut pfd = [PollFd {
            fd: f.fd(),
            events: 1,
            revents: 0,
        }];
        if !matches!(poll(&mut pfd, 3000), Ok(n) if n > 0) {
            eprintln!("vgrab: no frame within 3 s ({} received)", got);
            status = 1;
            break;
        }
        let mut b = buffer(0);
        if let Err(e) = ioctl(&f, DQBUF, &mut b) {
            status = err("vgrab", "VIDIOC_DQBUF", e);
            break;
        }
        let (index, used) = (u32_at(&b, 0) as usize, u32_at(&b, 8) as usize);
        if let Some(m) = maps.get(index) {
            let n = used.min(m.len);
            last = unsafe { core::slice::from_raw_parts(m.addr as *const u8, n) }.to_vec();
        }
        got += 1;
        let _ = ioctl(&f, QBUF, &mut b);
    }
    let _ = ioctl(&f, STREAMOFF, &mut ty);
    drop(maps);
    if got > 0 {
        if let Err(e) = fs::write(&out, &last) {
            return err("vgrab", &out, e);
        }
        println!(
            "captured {} frames, {}x{} {}, last frame {} bytes saved to {}",
            got,
            w,
            h,
            fourcc(pix),
            last.len(),
            out
        );
    }
    status
}
