#![no_std]
#![no_main]

use bootloader_api::{BootInfo, entry_point};
use core::panic::PanicInfo;
use rustos::{QemuExitCode, exit_qemu, serial_print, serial_println};

entry_point!(main, config = &rustos::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    rustos::kernel_init(boot_info);
    serial_print!("stack_overflow::stack_overflow...\t");

    // A double fault must land on its own IST stack and reach this handler.
    rustos::idt::register(rustos::idt::VEC_DOUBLE_FAULT, |_frame| {
        serial_println!("[ok]");
        exit_qemu(QemuExitCode::Success);
        rustos::hlt_loop();
    });

    stack_overflow();

    panic!("Execution continued after stack overflow");
}

#[allow(unconditional_recursion)]
fn stack_overflow() {
    stack_overflow(); // for each recursion, the return address is pushed
    let x = 0u64;
    unsafe { core::ptr::read_volatile(&x) }; // prevent tail recursion optimizations
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    rustos::test_panic_handler(info)
}
