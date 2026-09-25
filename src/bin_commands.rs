use crate::vfs::{NodeType, VFS};

const VIRTUAL_BIN_COMMANDS: &[&str] = &[
    "help", "echo", "clear", "uname", "color", "pwd", "ls", "cd", "mkdir", "rm", "cat", "write",
    "cp", "mv", "meminfo", "mount", "exec", "usbscan", "reboot", "shutdown", "rsh", "lspci",
    "lsusb", "lsblk", "grep", "ps",
];

pub fn virtual_bin_commands() -> &'static [&'static str] {
    VIRTUAL_BIN_COMMANDS
}

pub fn is_virtual_bin_path(path: &str) -> Option<&str> {
    let cmd = path.strip_prefix("/bin/")?;
    if cmd.is_empty() || cmd.contains('/') {
        return None;
    }
    if VIRTUAL_BIN_COMMANDS.contains(&cmd) {
        Some(cmd)
    } else {
        None
    }
}

pub fn run_virtual_bin_command(path: &str) -> Option<i64> {
    let cmd = is_virtual_bin_path(path)?;
    let code = match cmd {
        "help" => cmd_help(),
        "echo" => cmd_echo(),
        "clear" => cmd_clear(),
        "uname" => cmd_uname(),
        "pwd" => cmd_pwd(),
        "ls" => cmd_ls(),
        "meminfo" => cmd_meminfo(),
        "mount" => cmd_mount(),
        "usbscan" => cmd_usbscan(),
        "reboot" => cmd_reboot(),
        "shutdown" => cmd_shutdown(),
        "rsh" => {
            crate::println!("rsh is already running on the console");
            0
        }
        "color" => {
            crate::println!("Usage: /bin/color <fg> <bg>");
            2
        }
        "cd" => {
            crate::println!("Usage: /bin/cd <path>");
            2
        }
        "mkdir" => {
            crate::println!("Usage: /bin/mkdir <path>");
            2
        }
        "rm" => {
            crate::println!("Usage: /bin/rm <path>");
            2
        }
        "cat" => {
            crate::println!("Usage: /bin/cat <path>");
            2
        }
        "write" => {
            crate::println!("Usage: /bin/write <path> <text>");
            2
        }
        "cp" => {
            crate::println!("Usage: /bin/cp <src> <dst>");
            2
        }
        "mv" => {
            crate::println!("Usage: /bin/mv <src> <dst>");
            2
        }
        "exec" => {
            crate::println!("Usage: /bin/exec <path>");
            2
        }
        "lspci" => {
            crate::shell::commands::cmd_lspci(&[]);
            0
        }
        "lsusb" => {
            crate::shell::commands::cmd_lsusb(&[]);
            0
        }
        "lsblk" => {
            crate::shell::commands::cmd_lsblk(&[]);
            0
        }
        "ps" => {
            crate::shell::commands::cmd_ps(&[]);
            0
        }
        "grep" => {
            crate::println!("Usage: /bin/grep <pattern> <file>");
            2
        }
        _ => 127,
    };
    Some(code)
}

fn cmd_help() -> i64 {
    crate::shell::commands::print_help();
    0
}

fn cmd_echo() -> i64 {
    crate::println!();
    0
}

fn cmd_clear() -> i64 {
    crate::drivers::console::clear_screen();
    0
}

fn cmd_uname() -> i64 {
    crate::println!("RustOS v{}", env!("CARGO_PKG_VERSION"));
    crate::println!("Architecture: x86_64  Mode: Long mode (64-bit)");
    0
}

fn cmd_pwd() -> i64 {
    crate::println!("/");
    0
}

fn cmd_ls() -> i64 {
    let result = VFS.lock().as_mut().and_then(|vfs| vfs.list_dir("/").ok());
    match result {
        Some(mut entries) => {
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            for e in &entries {
                match e.node_type {
                    NodeType::Directory => crate::println!("{}/", e.name),
                    NodeType::File => crate::println!("{}", e.name),
                }
            }
            0
        }
        None => {
            crate::println!("ls: /: not found or error");
            1
        }
    }
}

fn cmd_meminfo() -> i64 {
    crate::println!("Heap start: 0x{:016x}", crate::allocator::HEAP_START);
    crate::println!(
        "Heap size:  {} KiB ({} bytes)",
        crate::allocator::heap_size() / 1024,
        crate::allocator::heap_size(),
    );
    0
}

fn cmd_mount() -> i64 {
    let vfs = VFS.lock();
    match vfs.as_ref() {
        Some(vfs) => {
            crate::println!("/  ({})", vfs.root_name());
            let m = vfs.list_mounts();
            for mp in &m {
                crate::println!("{}  (fat32)", mp);
            }
        }
        None => crate::println!("mount: VFS not initialised"),
    }
    0
}

fn cmd_usbscan() -> i64 {
    crate::println!("Scanning USB ports for new devices...");
    let new_devs = crate::usb::scan_and_mount();
    if new_devs == 0 {
        crate::println!("usbscan: no new USB storage devices found");
    } else {
        crate::println!("usbscan: {} new device(s) mounted", new_devs);
    }
    0
}

/// Triggers an immediate system reboot via keyboard controller reset and never returns.
fn cmd_reboot() -> ! {
    crate::acpi::reboot();
}

/// Attempts an ACPI S5 power-off and never returns.
fn cmd_shutdown() -> ! {
    crate::acpi::shutdown();
}
