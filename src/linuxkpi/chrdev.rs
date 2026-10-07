//! Linux character devices and files as RustOS objects (the C side is
//! src/linuxkpi/c/chrdev.c).
//!
//! A device node a Linux driver creates (`device_add` with a `devt`) is
//! registered in devfs as a [`DevNode`]; opening it opens a Linux
//! `struct file` through the driver's `open()`, wrapped as a [`LinuxFile`].
//! Anon-inode files (`anon_inode_getfd`, `fd_install`) are installed in
//! the calling process's descriptor table the same way.

use crate::errno::*;
use crate::sched::WaitQueue;
use crate::vfs::{self, DeviceMap, FileLike, FileType, MapPages, Metadata};
use alloc::string::String;
use alloc::sync::Arc;
use core::any::Any;
use core::ffi::{CStr, c_char, c_int, c_long, c_uint, c_void};

unsafe extern "C" {
    fn kpi_chrdev_open(devt: u32, flags: c_uint, out: *mut *mut c_void) -> c_int;
    fn kpi_file_read(file: *mut c_void, buf: *mut u8, len: usize, nonblock: c_int) -> isize;
    fn kpi_file_write(file: *mut c_void, buf: *const u8, len: usize, nonblock: c_int) -> isize;
    fn kpi_file_llseek(file: *mut c_void, off: i64, whence: c_int, out: *mut i64) -> c_int;
    fn kpi_file_ioctl(file: *mut c_void, cmd: c_uint, arg: u64) -> c_long;
    fn kpi_file_poll(file: *mut c_void, waitq: *const c_void) -> c_uint;
    fn kpi_file_mmap(
        file: *mut c_void,
        off: u64,
        len: u64,
        prot: u32,
        kind: *mut c_int,
        phys: *mut u64,
        cache: *mut c_int,
        handle: *mut *mut c_void,
    ) -> c_int;
    fn kpi_vma_fault(
        handle: *mut c_void,
        pgoff: u64,
        write: c_int,
        phys: *mut u64,
        cache: *mut c_int,
    ) -> c_int;
    fn kpi_vma_close(handle: *mut c_void);
    fn kpi_file_put(file: *mut c_void);
}

fn errno(r: i64) -> Errno {
    Errno((-r) as i32)
}

fn cache(c: c_int) -> crate::mm::Cache {
    match c {
        1 => crate::mm::Cache::WriteCombining,
        2 => crate::mm::Cache::WriteBack,
        _ => crate::mm::Cache::Uncached,
    }
}

/// An open Linux `struct file`. Dropping the last reference releases it.
pub struct LinuxFile {
    file: usize,
    rdev: u64,
    /// Woken by the driver's wait queues (see `kpi_file_poll`).
    wq: WaitQueue,
}

impl LinuxFile {
    fn ptr(&self) -> *mut c_void {
        self.file as *mut c_void
    }

    /// The wrapped `struct file *`.
    pub fn file(&self) -> *mut c_void {
        self.ptr()
    }
}

impl Drop for LinuxFile {
    fn drop(&mut self) {
        unsafe { kpi_file_put(self.ptr()) };
    }
}

impl FileLike for LinuxFile {
    fn read(&self, buf: &mut [u8], nonblock: bool) -> KResult<usize> {
        let r =
            unsafe { kpi_file_read(self.ptr(), buf.as_mut_ptr(), buf.len(), nonblock as c_int) };
        if r < 0 {
            Err(errno(r as i64))
        } else {
            Ok(r as usize)
        }
    }

    fn write(&self, buf: &[u8], nonblock: bool) -> KResult<usize> {
        let r = unsafe { kpi_file_write(self.ptr(), buf.as_ptr(), buf.len(), nonblock as c_int) };
        if r < 0 {
            Err(errno(r as i64))
        } else {
            Ok(r as usize)
        }
    }

    fn poll(&self) -> u16 {
        // EPOLL* values are the POLL* ones in the low bits.
        (unsafe { kpi_file_poll(self.ptr(), &self.wq as *const WaitQueue as *const c_void) }
            & 0xffff) as u16
    }

    fn wait_queue(&self) -> &WaitQueue {
        &self.wq
    }

    fn ioctl(&self, cmd: u64, arg: u64) -> KResult<i64> {
        let r = unsafe { kpi_file_ioctl(self.ptr(), cmd as c_uint, arg) } as i64;
        if r < 0 { Err(errno(r)) } else { Ok(r) }
    }

    fn seek(&self, off: i64, whence: u32) -> Option<KResult<u64>> {
        let mut pos = 0i64;
        let r = unsafe { kpi_file_llseek(self.ptr(), off, whence as c_int, &mut pos) };
        match r {
            0 => Some(Ok(pos as u64)),
            1 => None,
            e => Some(Err(errno(e as i64))),
        }
    }

    fn stat(&self) -> KResult<Metadata> {
        let mut m = Metadata::new(FileType::CharDevice, 0o666);
        m.rdev = self.rdev;
        Ok(m)
    }

    fn mmap(&self, off: u64, len: u64, prot: u32) -> KResult<Option<DeviceMap>> {
        let (mut kind, mut phys, mut c, mut handle) = (0, 0u64, 0, core::ptr::null_mut());
        let r = unsafe {
            kpi_file_mmap(
                self.ptr(),
                off,
                len,
                prot,
                &mut kind,
                &mut phys,
                &mut c,
                &mut handle,
            )
        };
        if r == -ENODEV.0 {
            return Ok(None);
        }
        if r < 0 {
            return Err(errno(r as i64));
        }
        Ok(Some(match kind {
            1 => DeviceMap::Phys {
                base: phys,
                cache: cache(c),
            },
            _ => DeviceMap::Pages(Arc::new(LinuxMapping(handle as usize))),
        }))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A mapping whose pages come from the driver's `vm_ops->fault`.
struct LinuxMapping(usize);

impl MapPages for LinuxMapping {
    fn fault(&self, pgoff: u64, write: bool) -> KResult<(u64, crate::mm::Cache)> {
        let (mut phys, mut c) = (0u64, 0);
        let r = unsafe {
            kpi_vma_fault(
                self.0 as *mut c_void,
                pgoff,
                write as c_int,
                &mut phys,
                &mut c,
            )
        };
        if r < 0 {
            return Err(errno(r as i64));
        }
        Ok((phys, cache(c)))
    }
}

impl Drop for LinuxMapping {
    fn drop(&mut self) {
        unsafe { kpi_vma_close(self.0 as *mut c_void) };
    }
}

/// Linux `dev_t` (major << 20 | minor) to the userspace encoding.
fn encode_dev(devt: u32) -> u64 {
    let (major, minor) = ((devt >> 20) as u64, (devt & 0xfffff) as u64);
    (minor & 0xff) | (major << 8) | ((minor & !0xff) << 12)
}

/// A device node a Linux driver created.
struct DevNode {
    devt: u32,
}

impl FileLike for DevNode {
    fn read(&self, _buf: &mut [u8], _nonblock: bool) -> KResult<usize> {
        Err(ENXIO)
    }

    fn write(&self, _buf: &[u8], _nonblock: bool) -> KResult<usize> {
        Err(ENXIO)
    }

    fn open_instance(&self, flags: u32) -> KResult<Option<Arc<dyn FileLike>>> {
        let mut file = core::ptr::null_mut();
        let r = unsafe { kpi_chrdev_open(self.devt, flags, &mut file) };
        if r < 0 {
            return Err(errno(r as i64));
        }
        Ok(Some(Arc::new(LinuxFile {
            file: file as usize,
            rdev: encode_dev(self.devt),
            wq: WaitQueue::new(),
        })))
    }

    fn stat(&self) -> KResult<Metadata> {
        let mut m = Metadata::new(FileType::CharDevice, 0o666);
        m.rdev = encode_dev(self.devt);
        Ok(m)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn cstr(p: *const c_char) -> String {
    String::from(unsafe { CStr::from_ptr(p) }.to_string_lossy())
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_devnode_add(name: *const c_char, devt: u32, block: c_int) -> c_int {
    let kind = if block != 0 {
        FileType::BlockDevice
    } else {
        FileType::CharDevice
    };
    vfs::devfs::register(
        &cstr(name),
        kind,
        encode_dev(devt),
        Arc::new(DevNode { devt }),
    );
    0
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_devnode_remove(name: *const c_char) {
    vfs::devfs::unregister(&cstr(name));
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_waitq_wake(wq: *const c_void) {
    if !wq.is_null() {
        unsafe { &*(wq as *const WaitQueue) }.wake_all();
    }
}

/// Placeholder for a descriptor reserved by `get_unused_fd_flags`.
struct Reserved;

impl FileLike for Reserved {
    fn read(&self, _buf: &mut [u8], _nonblock: bool) -> KResult<usize> {
        Err(EBADF)
    }
    fn write(&self, _buf: &[u8], _nonblock: bool) -> KResult<usize> {
        Err(EBADF)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Install `file` in the current process: at `fd` if it is >= 0 (a
/// reserved descriptor), else the lowest free one. Takes over the caller's
/// reference in every case (on failure it is dropped). Returns the
/// descriptor or -errno.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_fd_install(file: *mut c_void, cloexec: c_int, fd: c_int) -> c_int {
    let obj = Arc::new(LinuxFile {
        file: file as usize,
        rdev: 0,
        wq: WaitQueue::new(),
    });
    let Some(p) = crate::process::current() else {
        return -EBADF.0;
    };
    let mut files = p.files.lock();
    if fd >= 0 {
        let cloexec = files.entry(fd).map(|e| e.cloexec).unwrap_or(cloexec != 0);
        files.install_at(
            fd as usize,
            vfs::File::from_stream(obj, vfs::O_RDWR, "anon_inode"),
            cloexec,
        );
        return fd;
    }
    match files.install(
        vfs::File::from_stream(obj, vfs::O_RDWR, "anon_inode"),
        cloexec != 0,
    ) {
        Ok(fd) => fd,
        Err(e) => -e.0,
    }
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_fd_reserve(cloexec: c_int) -> c_int {
    let Some(p) = crate::process::current() else {
        return -EBADF.0;
    };
    let file = vfs::File::from_stream(Arc::new(Reserved), 0, "reserved");
    match p.files.lock().install(file, cloexec != 0) {
        Ok(fd) => fd,
        Err(e) => -e.0,
    }
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_fd_unreserve(fd: c_int) {
    if let Some(p) = crate::process::current() {
        let _ = p.files.lock().close(fd);
    }
}

/// The `struct file *` behind descriptor `fd` if it is a Linux file (no
/// reference taken), else NULL.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_fd_file(fd: c_int) -> *mut c_void {
    let Some(p) = crate::process::current() else {
        return core::ptr::null_mut();
    };
    let Ok(f) = p.files.lock().get(fd) else {
        return core::ptr::null_mut();
    };
    match f.stream() {
        Some(s) => s
            .as_any()
            .downcast_ref::<LinuxFile>()
            .map_or(core::ptr::null_mut(), |l| l.ptr()),
        None => core::ptr::null_mut(),
    }
}
