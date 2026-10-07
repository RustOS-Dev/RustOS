//! /dev/fb0: raw framebuffer access (read/write/mmap and the Linux fbdev
//! screen-info ioctls).

use crate::errno::*;
use crate::process::uaccess;
use crate::vfs::FileLike;
use alloc::sync::Arc;
use core::any::Any;

/// The console's framebuffer, whichever buffer it currently draws on: the
/// firmware framebuffer, or a display driver's buffer once a Linux DRM
/// driver has taken the display (then mapped page by page).
pub struct FbDev;

/// Geometry of the current buffer.
struct Geometry {
    width: u32,
    height: u32,
    stride: u32,
    bpp: u32,
    bgr: bool,
}

impl FbDev {
    pub fn new() -> Option<Arc<FbDev>> {
        super::framebuffer::geometry()?;
        Some(Arc::new(FbDev))
    }

    fn geometry() -> KResult<Geometry> {
        let (width, height, stride, bpp, bgr) = super::framebuffer::geometry().ok_or(ENODEV)?;
        Ok(Geometry {
            width: width as u32,
            height: height as u32,
            stride: stride as u32,
            bpp: bpp as u32,
            bgr,
        })
    }

    /// (address, length) of the buffer.
    fn buffer() -> KResult<(usize, usize)> {
        super::framebuffer::buffer()
            .map(|(p, l, _)| (p, l))
            .ok_or(ENODEV)
    }
}

/// Pages of a display driver's buffer (virtually contiguous only).
struct BufferPages {
    base: usize,
    len: usize,
}

impl crate::vfs::MapPages for BufferPages {
    fn fault(&self, pgoff: u64, _write: bool) -> KResult<(u64, crate::mm::Cache)> {
        let off = (pgoff * crate::mm::FRAME_SIZE) as usize;
        if off >= self.len {
            return Err(EFAULT);
        }
        let phys = crate::mm::virt_to_phys((self.base + off) as u64).ok_or(EFAULT)?;
        Ok((phys, crate::mm::Cache::WriteBack))
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
        let (base, size) = FbDev::buffer()?;
        if off + len > (size as u64).next_multiple_of(crate::mm::FRAME_SIZE) {
            return Err(EINVAL);
        }
        if let Some((phys, _)) = super::framebuffer::framebuffer_phys() {
            return Ok(Some(crate::vfs::DeviceMap::Phys {
                base: phys + off,
                cache: crate::mm::Cache::WriteCombining,
            }));
        }
        // Page-aligned (a vmap), so page `pgoff` of the file is at base + pgoff pages.
        Ok(Some(crate::vfs::DeviceMap::Pages(Arc::new(BufferPages {
            base,
            len: size,
        }))))
    }
    fn read(&self, _b: &mut [u8], _nb: bool) -> KResult<usize> {
        Ok(0)
    }
    fn write(&self, b: &[u8], _nb: bool) -> KResult<usize> {
        Ok(b.len())
    }
    fn read_at(&self, off: u64, buf: &mut [u8]) -> Option<KResult<usize>> {
        let (base, len) = match FbDev::buffer() {
            Ok(b) => b,
            Err(e) => return Some(Err(e)),
        };
        let off = off as usize;
        if off >= len {
            return Some(Ok(0));
        }
        let n = buf.len().min(len - off);
        unsafe { core::ptr::copy_nonoverlapping((base + off) as *const u8, buf.as_mut_ptr(), n) };
        Some(Ok(n))
    }
    fn write_at(&self, off: u64, buf: &[u8]) -> Option<KResult<usize>> {
        let (base, len) = match FbDev::buffer() {
            Ok(b) => b,
            Err(e) => return Some(Err(e)),
        };
        let off = off as usize;
        if off >= len {
            return Some(Err(ENOSPC));
        }
        let n = buf.len().min(len - off);
        unsafe { core::ptr::copy_nonoverlapping(buf.as_ptr(), (base + off) as *mut u8, n) };
        Some(Ok(n))
    }
    fn size(&self) -> Option<u64> {
        FbDev::buffer().ok().map(|(_, l)| l as u64)
    }
    fn ioctl(&self, cmd: u64, arg: u64) -> KResult<i64> {
        const FBIOGET_VSCREENINFO: u64 = 0x4600;
        const FBIOPUT_VSCREENINFO: u64 = 0x4601;
        const FBIOGET_FSCREENINFO: u64 = 0x4602;
        let g = FbDev::geometry()?;
        match cmd {
            FBIOGET_VSCREENINFO => {
                let (r, b) = if g.bgr { (16, 0) } else { (0, 16) };
                let v = VarScreenInfo {
                    xres: g.width,
                    yres: g.height,
                    xres_virtual: g.width,
                    yres_virtual: g.height,
                    bits_per_pixel: g.bpp * 8,
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
                    smem_start: super::framebuffer::framebuffer_phys().map_or(0, |(p, _)| p),
                    smem_len: FbDev::buffer().map_or(0, |(_, l)| l as u32),
                    kind: 0,
                    type_aux: 0,
                    visual: 2, // TRUECOLOR
                    xpanstep: 0,
                    ypanstep: 0,
                    ywrapstep: 0,
                    line_length: g.stride * g.bpp,
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
