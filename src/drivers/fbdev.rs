//! /dev/fb0: raw framebuffer access (read/write/mmap and the Linux fbdev
//! screen-info ioctls).

use crate::errno::*;
use crate::process::uaccess;
use crate::vfs::FileLike;
use alloc::sync::Arc;
use core::any::Any;

pub struct FbDev {
    pub phys: u64,
    pub len: usize,
    width: u32,
    height: u32,
    stride: u32,
    bpp: u32,
    bgr: bool,
}

impl FbDev {
    pub fn new() -> Option<Arc<FbDev>> {
        let (width, height, stride, bpp, bgr) = super::framebuffer::geometry()?;
        let (phys, len) = super::framebuffer::framebuffer_phys()?;
        Some(Arc::new(FbDev {
            phys,
            len,
            width: width as u32,
            height: height as u32,
            stride: stride as u32,
            bpp: bpp as u32,
            bgr,
        }))
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Bitfield {
    offset: u32,
    length: u32,
    msb_right: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct VarScreenInfo {
    xres: u32,
    yres: u32,
    xres_virtual: u32,
    yres_virtual: u32,
    xoffset: u32,
    yoffset: u32,
    bits_per_pixel: u32,
    grayscale: u32,
    red: Bitfield,
    green: Bitfield,
    blue: Bitfield,
    transp: Bitfield,
    nonstd: u32,
    activate: u32,
    height: u32,
    width: u32,
    accel_flags: u32,
    timing: [u32; 7],
    sync: u32,
    vmode: u32,
    rotate: u32,
    colorspace: u32,
    reserved: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct FixScreenInfo {
    id: [u8; 16],
    smem_start: u64,
    smem_len: u32,
    kind: u32,
    type_aux: u32,
    visual: u32,
    xpanstep: u16,
    ypanstep: u16,
    ywrapstep: u16,
    line_length: u32,
    mmio_start: u64,
    mmio_len: u32,
    accel: u32,
    capabilities: u16,
    reserved: [u16; 2],
}

impl FileLike for FbDev {
    /// The framebuffer, write-combining (much faster than uncached for
    /// the streaming writes graphics code does).
    fn mmap(&self, off: u64, len: u64, _prot: u32) -> KResult<Option<crate::vfs::DeviceMap>> {
        if off + len > (self.len as u64).next_multiple_of(crate::mm::FRAME_SIZE) {
            return Err(EINVAL);
        }
        Ok(Some(crate::vfs::DeviceMap::Phys {
            base: self.phys + off,
            cache: crate::mm::Cache::WriteCombining,
        }))
    }
    fn read(&self, _b: &mut [u8], _nb: bool) -> KResult<usize> {
        Ok(0)
    }
    fn write(&self, b: &[u8], _nb: bool) -> KResult<usize> {
        Ok(b.len())
    }
    fn read_at(&self, off: u64, buf: &mut [u8]) -> Option<KResult<usize>> {
        let off = off as usize;
        if off >= self.len {
            return Some(Ok(0));
        }
        let n = buf.len().min(self.len - off);
        unsafe {
            core::ptr::copy_nonoverlapping(
                crate::mm::phys_ptr::<u8>(self.phys + off as u64),
                buf.as_mut_ptr(),
                n,
            )
        };
        Some(Ok(n))
    }
    fn write_at(&self, off: u64, buf: &[u8]) -> Option<KResult<usize>> {
        let off = off as usize;
        if off >= self.len {
            return Some(Err(ENOSPC));
        }
        let n = buf.len().min(self.len - off);
        unsafe {
            core::ptr::copy_nonoverlapping(
                buf.as_ptr(),
                crate::mm::phys_ptr::<u8>(self.phys + off as u64),
                n,
            )
        };
        Some(Ok(n))
    }
    fn size(&self) -> Option<u64> {
        Some(self.len as u64)
    }
    fn ioctl(&self, cmd: u64, arg: u64) -> KResult<i64> {
        const FBIOGET_VSCREENINFO: u64 = 0x4600;
        const FBIOPUT_VSCREENINFO: u64 = 0x4601;
        const FBIOGET_FSCREENINFO: u64 = 0x4602;
        match cmd {
            FBIOGET_VSCREENINFO => {
                let (r, b) = if self.bgr { (16, 0) } else { (0, 16) };
                let v = VarScreenInfo {
                    xres: self.width,
                    yres: self.height,
                    xres_virtual: self.width,
                    yres_virtual: self.height,
                    bits_per_pixel: self.bpp * 8,
                    red: Bitfield {
                        offset: r,
                        length: 8,
                        msb_right: 0,
                    },
                    green: Bitfield {
                        offset: 8,
                        length: 8,
                        msb_right: 0,
                    },
                    blue: Bitfield {
                        offset: b,
                        length: 8,
                        msb_right: 0,
                    },
                    height: u32::MAX,
                    width: u32::MAX,
                    ..Default::default()
                };
                uaccess::write_user(arg, &v)?;
                Ok(0)
            }
            FBIOPUT_VSCREENINFO => Ok(0),
            FBIOGET_FSCREENINFO => {
                let mut id = [0u8; 16];
                id[..8].copy_from_slice(b"RustOSfb");
                let f = FixScreenInfo {
                    id,
                    smem_start: self.phys,
                    smem_len: self.len as u32,
                    kind: 0,
                    type_aux: 0,
                    visual: 2, // TRUECOLOR
                    xpanstep: 0,
                    ypanstep: 0,
                    ywrapstep: 0,
                    line_length: self.stride * self.bpp,
                    mmio_start: 0,
                    mmio_len: 0,
                    accel: 0,
                    capabilities: 0,
                    reserved: [0; 2],
                };
                uaccess::write_user(arg, &f)?;
                Ok(0)
            }
            _ => Err(ENOTTY),
        }
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
