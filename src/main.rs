#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(rustos::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader_api::{BootInfo, entry_point};
use core::panic::PanicInfo;
use rustos::println;

entry_point!(kernel_main, config = &rustos::BOOTLOADER_CONFIG);

fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    rustos::kernel_init(boot_info);

    // Initialise VFS
    rustos::vfs::init();

    // Enable the heap-backed terminal scrollback buffer now that the allocator
    // is ready.  Arrow Up/Down will scroll through terminal history.
    rustos::drivers::framebuffer::enable_scrollback();

    // Probe PCI for XHCI and mount USB FAT32.
    init_usb_storage();

    #[cfg(test)]
    test_main();

    // This enters the interactive rsh shell loop and never returns.
    launch_rsh();
}

/// Scan PCI for an XHCI controller, initialise it, store it in the global
/// `USB_XHCI`, and mount any FAT32 volumes found on connected drives.
fn init_usb_storage() {
    use rustos::pci;
    use rustos::usb::USB_XHCI;
    use rustos::usb::xhci::Xhci;

    let devices = pci::enumerate();
    let dev = match pci::find_xhci(&devices) {
        Some(d) => d,
        None => {
            rustos::serial_println!("[usb] no XHCI controller found");
            return;
        }
    };

    let ctrl = match Xhci::init(dev) {
        Some(c) => c,
        None => {
            rustos::serial_println!("[usb] XHCI init failed");
            return;
        }
    };

    let found = ctrl.devices.len();
    rustos::serial_println!("[usb] XHCI ready, {} device(s) enumerated", found);

    // Store the controller globally so ongoing I/O and hot-plug rescans work.
    *USB_XHCI.lock() = Some(ctrl);

    if found == 0 {
        rustos::serial_println!("[usb] no USB storage devices found at boot");
        return;
    }

    if !rustos::usb::mount_boot_storage_root() {
        println!("[init] no FAT32 root partition on USB device 0; keeping the RAM root");
    }

    // Mount all found devices under /usb* (device 0 → /usb, device 1 → /usb1, …).
    rustos::usb::mount_storage_devices(0);
}

/// Runs the default rsh-compatible shell using the framebuffer output.
fn launch_rsh() -> ! {
    rustos::serial_println!("[init] Starting /bin/rsh...");
    println!("\n=== RustOS rsh ===");
    println!("Type 'help' for available commands\n");

    let mut shell = rustos::shell::Shell::rsh();
    shell.print_prompt();

    loop {
        // Wait for keyboard input
        match rustos::task::keyboard::read_input_event() {
            Some(rustos::task::keyboard::InputEvent::Char(byte)) => {
                shell.handle_char(byte as char);
            }
            Some(rustos::task::keyboard::InputEvent::ArrowUp) => {
                rustos::drivers::framebuffer::scroll_view_up();
            }
            Some(rustos::task::keyboard::InputEvent::ArrowDown) => {
                rustos::drivers::framebuffer::scroll_view_down();
            }
            None => {
                if rustos::task::keyboard::interrupt_input_observed() {
                    x86_64::instructions::hlt();
                } else {
                    // Before seeing any IRQ1 traffic, do not halt: some real laptops
                    // expose PS/2-compatible keyboard data only through polling after
                    // UEFI handoff. Once an IRQ arrives, the branch above can halt.
                    core::hint::spin_loop();
                }
            }
        }
    }
}

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("{}", info);
    rustos::hlt_loop();
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    rustos::test_panic_handler(info)
}

#[test_case]
fn trivial_assertion() {
    assert_eq!(1, 1);
}
