//! Userspace runtime for RustOS.
//!
//! Programs are `#![no_std] #![no_main]` binaries for the
//! `x86_64-unknown-none` target that declare their entry point with
//! [`entry!`]. The runtime provides `_start`, argument/environment parsing,
//! a heap backed by `brk`, a panic handler, and std-like modules.
//!
//! ```ignore
//! #![no_std]
//! #![no_main]
//! use rustos_rt::prelude::*;
//! rustos_rt::entry!(main);
//! fn main(args: Vec<String>) -> i32 {
//!     println!("hello from {}", args[0]);
//!     0
//! }
//! ```

#![no_std]

extern crate alloc;

pub mod env;
pub mod fs;
pub mod io;
pub mod net;
pub mod process;
pub mod signal;
pub mod sys;
pub mod term;
pub mod time;

pub use alloc::{boxed::Box, format, string::String, string::ToString, vec, vec::Vec};

pub mod prelude {
    pub use crate::io::{Read, Write};
    pub use crate::{eprint, eprintln, print, println};
    pub use alloc::{
        borrow::ToOwned, boxed::Box, format, string::String, string::ToString, vec, vec::Vec,
    };
}

/// Error from a failed system call (holds the positive errno).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error(pub i32);

pub type Result<T> = core::result::Result<T, Error>;

impl Error {
    pub fn message(&self) -> &'static str {
        sys::strerror(self.0)
    }
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.message())
    }
}

// ---------------------------------------------------------------------------
// Heap
// ---------------------------------------------------------------------------

mod heap {
    use core::alloc::{GlobalAlloc, Layout};
    use core::cell::UnsafeCell;
    use linked_list_allocator::Heap;

    pub struct BrkHeap {
        heap: UnsafeCell<Heap>,
        top: UnsafeCell<usize>,
    }

    // Programs are single threaded (threads would need a lock here).
    unsafe impl Sync for BrkHeap {}

    impl BrkHeap {
        pub const fn new() -> BrkHeap {
            BrkHeap {
                heap: UnsafeCell::new(Heap::empty()),
                top: UnsafeCell::new(0),
            }
        }

        unsafe fn grow(&self, need: usize) -> bool {
            unsafe {
                let heap = &mut *self.heap.get();
                let top = &mut *self.top.get();
                let chunk = need.max(256 * 1024).next_multiple_of(4096) + 4096;
                if *top == 0 {
                    let start = crate::sys::brk(0);
                    let start = (start + 15) & !15;
                    let end = crate::sys::brk(start + chunk);
                    if end < start + chunk {
                        return false;
                    }
                    heap.init(start as *mut u8, chunk);
                    *top = start + chunk;
                } else {
                    let end = crate::sys::brk(*top + chunk);
                    if end < *top + chunk {
                        return false;
                    }
                    heap.extend(chunk);
                    *top += chunk;
                }
                true
            }
        }
    }

    unsafe impl GlobalAlloc for BrkHeap {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            unsafe {
                let heap = &mut *self.heap.get();
                if let Ok(p) = heap.allocate_first_fit(layout) {
                    return p.as_ptr();
                }
                if !self.grow(layout.size() + layout.align()) {
                    return core::ptr::null_mut();
                }
                (*self.heap.get())
                    .allocate_first_fit(layout)
                    .map_or(core::ptr::null_mut(), |p| p.as_ptr())
            }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe {
                (*self.heap.get()).deallocate(core::ptr::NonNull::new_unchecked(ptr), layout);
            }
        }
    }
}

#[global_allocator]
static ALLOCATOR: heap::BrkHeap = heap::BrkHeap::new();

// ---------------------------------------------------------------------------
// Entry
// ---------------------------------------------------------------------------

core::arch::global_asm!(
    ".globl _start",
    "_start:",
    "    xor rbp, rbp",
    "    mov rdi, rsp",
    "    and rsp, -16",
    "    call {start}",
    "    ud2",
    start = sym rt_start,
);

static mut ARGS: *const u64 = core::ptr::null();

unsafe extern "Rust" {
    fn __rustos_main(args: Vec<String>) -> i32;
}

unsafe extern "C" fn rt_start(sp: *const u64) -> ! {
    unsafe {
        ARGS = sp;
    }
    env::init_from_stack(sp);
    let args = env::args();
    let code = unsafe { __rustos_main(args) };
    io::flush();
    process::exit(code);
}

/// Declare the program's `fn main(args: Vec<String>) -> i32`.
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        #[unsafe(no_mangle)]
        fn __rustos_main(args: $crate::Vec<$crate::String>) -> i32 {
            $main(args)
        }
    };
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    io::flush();
    let _ = io::write_fmt_fd(2, format_args!("panic: {}\n", info));
    process::exit(101);
}
