use fixed_size_block::FixedSizeBlockAllocator;
use x86_64::structures::paging::{PageTableFlags, Size4KiB, mapper::MapToError};

pub mod fixed_size_block;

pub use crate::mm::HEAP_START;

/// Smallest and largest kernel heap. The actual size is a quarter of usable
/// RAM, clamped to this range.
const HEAP_MIN: u64 = 32 * 1024 * 1024;
const HEAP_MAX: u64 = 512 * 1024 * 1024;

static HEAP_SIZE_BYTES: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

#[global_allocator]
static ALLOCATOR: Locked<FixedSizeBlockAllocator> = Locked::new(FixedSizeBlockAllocator::new());

/// Size of the kernel heap in bytes (0 before initialisation).
pub fn heap_size() -> u64 {
    HEAP_SIZE_BYTES.load(core::sync::atomic::Ordering::Relaxed)
}

/// Free bytes in the kernel heap (a lower bound).
pub fn heap_free() -> u64 {
    x86_64::instructions::interrupts::without_interrupts(|| ALLOCATOR.lock().free_bytes() as u64)
}

/// Map and initialise the kernel heap. Called by [`crate::mm::init`].
pub fn init_heap() -> Result<(), MapToError<Size4KiB>> {
    let (free, _) = crate::mm::memory_stats();
    let size = (free / 4).clamp(HEAP_MIN, HEAP_MAX) & !0xfff;
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;
    crate::mm::map_kernel_pages(HEAP_START, size, flags)?;
    unsafe { ALLOCATOR.lock().init(HEAP_START as usize, size as usize) };
    HEAP_SIZE_BYTES.store(size, core::sync::atomic::Ordering::Relaxed);
    Ok(())
}

/// A wrapper around crate::sync::Mutex to permit trait implementations.
pub struct Locked<A> {
    inner: crate::sync::Mutex<A>,
}

impl<A> Locked<A> {
    pub const fn new(inner: A) -> Self {
        Locked {
            inner: crate::sync::Mutex::new(inner),
        }
    }

    pub fn lock(&self) -> spin::MutexGuard<'_, A> {
        self.inner.lock()
    }
}
