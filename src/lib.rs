#![no_std]
#![cfg_attr(test, no_main)]
#![feature(custom_test_frameworks)]
#![test_runner(crate::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;
use bootloader_api::BootInfo;
use bootloader_api::config::{BootloaderConfig, Mapping};
use core::panic::PanicInfo;

pub mod allocator;
pub mod arch;
pub mod block;
pub mod bluetooth;
pub mod drivers;
pub mod errno;
pub mod firmware;
pub mod fs;
pub mod initramfs;
pub mod klog;
#[cfg(feature = "linuxkpi")]
pub mod linuxkpi;
pub mod mm;
pub mod net;
pub mod params;
pub mod pci;
pub mod process;
pub mod sched;
pub mod sound;
pub mod sync;
pub mod syscall;
pub mod task;
pub mod time;
pub mod tty;
pub mod usb;
pub mod vfs;

pub use arch::x86_64::{acpi, apic, cpu, gdt, idt};

/// Bootloader configuration shared by the kernel and every test binary.
///
/// All bootloader-created mappings (kernel image, stack, boot info,
/// framebuffer, physical-memory map) are placed in the upper half so the
/// lower half is free for user processes.
pub const BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config.mappings.dynamic_range_start = Some(mm::DYNAMIC_RANGE_START);
    config.mappings.dynamic_range_end = Some(mm::DYNAMIC_RANGE_END);
    config.kernel_stack_size = 256 * 1024;
    config
};

/// Bring up the core kernel: console, descriptor tables, memory, ACPI,
/// interrupt controllers, timekeeping and the scheduler. Interrupts are
/// enabled on return.
pub fn kernel_init(boot_info: &'static mut BootInfo) {
    x86_64::instructions::interrupts::disable();

    // The GOP framebuffer is the first reliable diagnostic output on real
    // UEFI laptops (which often lack COM1).
    if let Some(fb) = boot_info.framebuffer.take() {
        unsafe { drivers::framebuffer::init(fb) };
    }
    drivers::serial::init();
    println!("\n=== RustOS Kernel Initializing ===\n");

    gdt::init_boot();
    idt::init();

    let rsdp = boot_info.rsdp_addr.into_option();
    let boot_info: &'static BootInfo = boot_info;
    unsafe { mm::init(boot_info) };
    mm::init_pat();
    let (free, total) = mm::memory_stats();
    println!(
        "[mm] {} MiB usable, {} MiB free, heap {} MiB",
        total >> 20,
        free >> 20,
        allocator::heap_size() >> 20
    );

    let tss = gdt::init_cpu();
    cpu::init(0, 0, tss);

    acpi::init(rsdp);
    apic::init_local();
    cpu::set_lapic_id(apic::id());
    apic::init_io();
    idt::register(idt::VEC_APIC_ERROR, apic::apic_error_handler);
    time::calibrate();
    pci::init_ecam();

    sched::init_cpu("kmain");
    time::start_tick();
    arch::x86_64::syscall_entry::init();
    arch::x86_64::smp::init_ipis();
    arch::x86_64::smp::start_aps();
    drivers::ps2::init();

    x86_64::instructions::interrupts::enable();

    vfs::init();
    sched::start_worker();
    tty::start_input_thread();
    drivers::serial::enable_rx_interrupts();
}

/// Bring up devices and filesystems, then start `/sbin/init` (never returns).
pub fn start_userspace() -> ! {
    let n = initramfs::unpack();
    println!("[init] initramfs: {} entries", n);
    net::init();
    drivers::probe_all();
    if let Some(path) = params::load() {
        println!(
            "[init] kernel parameters from {}: {}",
            path,
            params::cmdline()
        );
    }
    if params::flag("log.persist") {
        klog::start_persist();
    }
    bluetooth::late_init();

    syscall::init();
    if option_env!("RUSTOS_STRACE").is_some() {
        syscall::TRACE.store(true, core::sync::atomic::Ordering::Relaxed);
    }
    let candidates = ["/sbin/init", "/bin/init", "/bin/sh"];
    for path in candidates {
        if vfs::exists(path) {
            match process::spawn_init(
                path,
                &[path],
                &[
                    "PATH=/bin:/sbin:/usr/bin:/usr/local/bin",
                    "HOME=/root",
                    "TERM=vt100",
                ],
            ) {
                Ok(p) => {
                    println!("[init] started {} (pid {})", path, p.pid);
                    drop(p);
                    sched::exit_current();
                }
                Err(e) => println!("[init] {}: {}", path, e),
            }
        }
    }
    panic!("no init program found (tried /sbin/init, /bin/init, /bin/sh)");
}

pub trait Testable {
    fn run(&self);
}

impl<T> Testable for T
where
    T: Fn(),
{
    fn run(&self) {
        serial_print!("{}...\t", core::any::type_name::<T>());
        self();
        serial_println!("[ok]");
    }
}

pub fn test_runner(tests: &[&dyn Testable]) {
    serial_println!("Running {} tests", tests.len());
    for test in tests {
        test.run();
    }
    exit_qemu(QemuExitCode::Success);
}

pub fn test_panic_handler(info: &PanicInfo) -> ! {
    serial_println!("[failed]\n");
    serial_println!("Error: {}\n", info);
    exit_qemu(QemuExitCode::Failed);
    hlt_loop();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode {
    Success = 0x10,
    Failed = 0x11,
}

pub fn exit_qemu(exit_code: QemuExitCode) {
    use x86_64::instructions::port::Port;

    unsafe {
        let mut port = Port::new(0xf4);
        port.write(exit_code as u32);
    }
}

pub fn hlt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}

#[cfg(test)]
bootloader_api::entry_point!(test_kernel_main, config = &BOOTLOADER_CONFIG);

#[cfg(test)]
fn test_kernel_main(boot_info: &'static mut BootInfo) -> ! {
    kernel_init(boot_info);
    test_main();
    hlt_loop();
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    test_panic_handler(info)
}

#[test_case]
fn test_breakpoint_exception() {
    x86_64::instructions::interrupts::int3();
}

#[test_case]
fn test_timer_ticks() {
    let t0 = time::ticks();
    time::sleep_ms(50);
    assert!(time::ticks() > t0, "APIC timer is not ticking");
}

#[test_case]
fn test_kernel_threads_run_and_preempt() {
    use core::sync::atomic::{AtomicU64, Ordering};
    static A: AtomicU64 = AtomicU64::new(0);
    static B: AtomicU64 = AtomicU64::new(0);
    // Two busy threads that never yield: both must make progress, which
    // only happens if the timer preempts them.
    sched::spawn("spin-a", || {
        for _ in 0..2_000_000 {
            A.fetch_add(1, Ordering::Relaxed);
        }
    });
    sched::spawn("spin-b", || {
        for _ in 0..2_000_000 {
            B.fetch_add(1, Ordering::Relaxed);
        }
    });
    let done = sched::WaitQueue::new().wait_timeout(20_000, || {
        A.load(Ordering::Relaxed) == 2_000_000 && B.load(Ordering::Relaxed) == 2_000_000
    });
    assert!(
        done,
        "threads did not finish: a={} b={}",
        A.load(Ordering::Relaxed),
        B.load(Ordering::Relaxed)
    );
}

#[test_case]
fn test_wait_queue_wakeup() {
    use alloc::sync::Arc;
    use core::sync::atomic::{AtomicBool, Ordering};
    let wq = Arc::new(sched::WaitQueue::new());
    let flag = Arc::new(AtomicBool::new(false));
    let (wq2, flag2) = (wq.clone(), flag.clone());
    sched::spawn("waker", move || {
        time::sleep_ms(20);
        flag2.store(true, Ordering::SeqCst);
        wq2.wake_all();
    });
    assert!(wq.wait_timeout(5_000, || flag.load(Ordering::SeqCst)));
}

#[test_case]
fn test_dma_buffer_contiguous() {
    let buf = mm::dma::DmaBuffer::new(5 * 4096).expect("dma alloc");
    for i in 0..5u64 {
        let v = buf.virt() + i * 4096;
        assert_eq!(mm::virt_to_phys(v), Some(buf.phys() + i * 4096));
    }
    buf.write::<u32>(4 * 4096, 0xdead_beef);
    assert_eq!(buf.read::<u32>(4 * 4096), 0xdead_beef);
}

#[test_case]
fn test_frames_are_recycled() {
    let (before, _) = mm::memory_stats();
    {
        let _a = mm::dma::DmaBuffer::new(64 * 4096).unwrap();
        let (during, _) = mm::memory_stats();
        assert!(during <= before - 64 * 4096);
    }
    let (after, _) = mm::memory_stats();
    assert_eq!(before, after);
}

#[test_case]
fn test_pci_enumeration_finds_host_bridge() {
    let devs = pci::enumerate();
    assert!(!devs.is_empty());
    assert!(devs.iter().any(|d| d.class == 0x06 && d.subclass == 0x00));
    // No duplicates.
    for (i, a) in devs.iter().enumerate() {
        for b in &devs[i + 1..] {
            assert!((a.segment, a.bus, a.dev, a.func) != (b.segment, b.bus, b.dev, b.func));
        }
    }
}
