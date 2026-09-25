//! Built-in shell command implementations.
//!
//! Every command behaves like its Linux counterpart: same flags, same output
//! format, same error messages where practical.

use super::Shell;
use crate::drivers::console::Color;
use crate::vfs::{NodeType, VFS};
use alloc::{
    string::{String, ToString},
    vec::Vec,
};

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

/// Dispatch a parsed command name and argument list to the appropriate handler.
pub fn dispatch(shell: &mut Shell, cmd: &str, args: &[&str]) {
    // Handle /bin/<cmd> format for direct binary invocation
    let resolved = shell.resolve_path(cmd);
    let actual_cmd = if let Some(cmd_name) = crate::bin_commands::is_virtual_bin_path(&resolved) {
        cmd_name
    } else if cmd.starts_with("/bin/") {
        // Could be a real external ELF binary - pass all args including the path
        let mut exec_args = alloc::vec::Vec::with_capacity(args.len() + 1);
        exec_args.push(cmd);
        exec_args.extend_from_slice(args);
        cmd_exec(shell, &exec_args);
        return;
    } else {
        cmd
    };

    match actual_cmd {
        "help" => print_help(),
        "echo" => cmd_echo(args),
        "clear" => cmd_clear(),
        "uname" => cmd_uname(args),
        "color" => cmd_color(shell, args),
        "pwd" => cmd_pwd(shell, args),
        "ls" => cmd_ls(shell, args),
        "cd" => cmd_cd(shell, args),
        "mkdir" => cmd_mkdir(shell, args),
        "rm" => cmd_rm(shell, args),
        "cat" => cmd_cat(shell, args),
        "write" => cmd_write(shell, args),
        "cp" => cmd_cp(shell, args),
        "mv" => cmd_mv(shell, args),
        "meminfo" => cmd_meminfo(),
        "mount" => cmd_mount(shell, args),
        "umount" => cmd_umount(shell, args),
        "exec" => cmd_exec(shell, args),
        "usbscan" => cmd_usbscan(),
        "reboot" => cmd_reboot(),
        "shutdown" => cmd_shutdown(),
        "lspci" => cmd_lspci(args),
        "lsusb" => cmd_lsusb(args),
        "lsblk" => cmd_lsblk(args),
        "grep" => cmd_grep(shell, args),
        "ps" => cmd_ps(args),
        other => crate::println!("{}: command not found", other),
    }
}

// ---------------------------------------------------------------------------
// help
// ---------------------------------------------------------------------------

pub fn print_help() {
    crate::println!("Built-in commands (Linux-compatible flags supported):");
    crate::println!("  echo [-n] [-e] [text]   Print text (-n: no newline, -e: escapes)");
    crate::println!("  cat [-n] [-A] file...   Concatenate and print files");
    crate::println!("  ls [-alh] [path...]     List directory contents");
    crate::println!("  cd [path]               Change working directory");
    crate::println!("  pwd [-LP]               Print working directory");
    crate::println!("  mkdir [-p] path...      Create directories");
    crate::println!("  rm [-rf] path...        Remove files/directories");
    crate::println!("  cp [-r] src... dst      Copy files/directories");
    crate::println!("  mv [-fn] src... dst     Move/rename files");
    crate::println!("  grep [-invrcl] pat file... Search for patterns in files");
    crate::println!("  mount [-t type] [dev dir]  Show/mount filesystems");
    crate::println!("  umount <path>           Unmount a filesystem");
    crate::println!("  lsblk [-f] [-l] [-o cols] [dev]  List block devices");
    crate::println!("  lspci [-v] [-n] [-nn]   List PCI devices");
    crate::println!("  lsusb [-v] [-t]         List USB devices");
    crate::println!("  ps [aux]                List processes");
    crate::println!("  uname [-asnrvmpio]      Print system information");
    crate::println!("  clear                   Clear screen");
    crate::println!("  usbscan                 Scan for newly plugged USB drives");
    crate::println!("  exec <path>             Execute an ELF binary");
    crate::println!("  meminfo                 Show heap memory info");
    crate::println!("  reboot                  Reboot");
    crate::println!("  shutdown                Power off");
    crate::println!("  color <fg> <bg>         Set terminal colors");
    crate::println!();
    crate::println!("/bin commands:");
    for cmd in crate::bin_commands::virtual_bin_commands() {
        crate::println!("  /bin/{}", cmd);
    }
}

// ---------------------------------------------------------------------------
// echo — matches GNU coreutils echo
// ---------------------------------------------------------------------------

fn cmd_echo(args: &[&str]) {
    let mut no_newline = false;
    let mut interpret_escapes = false;
    let mut text_start = 0;

    for (i, &a) in args.iter().enumerate() {
        match a {
            "-n" => {
                no_newline = true;
                text_start = i + 1;
            }
            "-e" => {
                interpret_escapes = true;
                text_start = i + 1;
            }
            "-ne" | "-en" => {
                no_newline = true;
                interpret_escapes = true;
                text_start = i + 1;
            }
            _ => {
                text_start = i;
                break;
            }
        }
    }

    let text = args[text_start..].join(" ");
    let output = if interpret_escapes {
        interpret_escape_sequences(&text)
    } else {
        text
    };

    if no_newline {
        crate::print!("{}", output);
    } else {
        crate::println!("{}", output);
    }
}

fn interpret_escape_sequences(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some('0') => out.push('\0'),
                Some('a') => out.push('\x07'),
                Some('b') => out.push('\x08'),
                Some(c) => {
                    out.push('\\');
                    out.push(c);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// clear
// ---------------------------------------------------------------------------

fn cmd_clear() {
    crate::drivers::console::clear_screen();
}

// ---------------------------------------------------------------------------
// uname — matches Linux uname
// ---------------------------------------------------------------------------

fn cmd_uname(args: &[&str]) {
    let all = args.contains(&"-a");
    let kernel_name = args.contains(&"-s") || all;
    let nodename = args.contains(&"-n") || all;
    let release = args.contains(&"-r") || all;
    let version = args.contains(&"-v") || all;
    let machine = args.contains(&"-m") || all;
    let processor = args.contains(&"-p") || all;
    let hw_platform = args.contains(&"-i") || all;
    let os = args.contains(&"-o") || all;

    // Default (no flags): just print kernel name
    let show_all = args.is_empty();

    let mut parts: Vec<&str> = Vec::new();
    if show_all || kernel_name {
        parts.push("RustOS");
    }
    if nodename {
        parts.push("rustos");
    }
    if release {
        parts.push(env!("CARGO_PKG_VERSION"));
    }
    if version {
        parts.push("#1 SMP RustOS");
    }
    if machine || processor || hw_platform {
        parts.push("x86_64");
    }
    if os {
        parts.push("RustOS/x86_64");
    }
    crate::println!("{}", parts.join(" "));
}

// ---------------------------------------------------------------------------
// color (RustOS-specific)
// ---------------------------------------------------------------------------

fn parse_color(s: &str) -> Option<Color> {
    match s {
        "black" => Some(Color::Black),
        "blue" => Some(Color::Blue),
        "green" => Some(Color::Green),
        "cyan" => Some(Color::Cyan),
        "red" => Some(Color::Red),
        "magenta" => Some(Color::Magenta),
        "brown" => Some(Color::Brown),
        "lightgray" => Some(Color::LightGray),
        "darkgray" => Some(Color::DarkGray),
        "lightblue" => Some(Color::LightBlue),
        "lightgreen" => Some(Color::LightGreen),
        "lightcyan" => Some(Color::LightCyan),
        "lightred" => Some(Color::LightRed),
        "pink" => Some(Color::Pink),
        "yellow" => Some(Color::Yellow),
        "white" => Some(Color::White),
        _ => None,
    }
}

fn cmd_color(shell: &mut Shell, args: &[&str]) {
    if args.len() < 2 {
        crate::println!("Usage: color <fg> <bg>");
        crate::println!("Colors: black blue green cyan red magenta brown lightgray");
        crate::println!(
            "        darkgray lightblue lightgreen lightcyan lightred pink yellow white"
        );
        return;
    }
    let fg = match parse_color(args[0]) {
        Some(c) => c,
        None => {
            crate::println!("color: unknown color: '{}'", args[0]);
            return;
        }
    };
    let bg = match parse_color(args[1]) {
        Some(c) => c,
        None => {
            crate::println!("color: unknown color: '{}'", args[1]);
            return;
        }
    };
    shell.fg_color = fg;
    shell.bg_color = bg;
    crate::drivers::console::set_color(fg, bg);
}

// ---------------------------------------------------------------------------
// pwd — matches Linux pwd
// ---------------------------------------------------------------------------

fn cmd_pwd(shell: &Shell, args: &[&str]) {
    // Parse flags: -L (logical, default), -P (physical, resolved symlinks)
    // In RustOS, there are no symlinks, so both behave identically
    let _logical = !args.contains(&"-P");
    let _physical = args.contains(&"-P");
    crate::println!("{}", shell.cwd);
}

// ---------------------------------------------------------------------------
// ls — matches Linux ls -la etc.
// ---------------------------------------------------------------------------

fn cmd_ls(shell: &mut Shell, args: &[&str]) {
    // Parse flags
    let mut long = false;
    let mut show_all = false;
    let mut human = false;
    let mut paths: Vec<&str> = Vec::new();

    for &a in args {
        if a.starts_with('-') && a.len() > 1 && !a.starts_with("--") {
            for c in a[1..].chars() {
                match c {
                    'l' => long = true,
                    'a' | 'A' => show_all = true,
                    'h' => human = true,
                    '1' => {} // single column — default
                    _ => {}   // ignore unknown flags silently like Linux does
                }
            }
        } else if a == "--all" {
            show_all = true;
        } else {
            paths.push(a);
        }
    }

    if paths.is_empty() {
        ls_dir(shell, "", long, show_all, human, paths.len() > 1);
    } else {
        let multiple = paths.len() > 1;
        for path in &paths {
            ls_dir(shell, path, long, show_all, human, multiple);
        }
    }
}

fn ls_dir(
    shell: &mut Shell,
    input: &str,
    long: bool,
    show_all: bool,
    human: bool,
    print_header: bool,
) {
    let path = shell.resolve_path(input);

    // Check if it is a file (not a dir)
    let is_dir = VFS
        .lock()
        .as_mut()
        .map(|vfs| vfs.is_dir(&path))
        .unwrap_or(false);

    if !is_dir {
        // Single file
        let exists = VFS
            .lock()
            .as_mut()
            .map(|vfs| vfs.exists(&path))
            .unwrap_or(false);
        if !exists {
            crate::println!("ls: cannot access '{}': No such file or directory", path);
            return;
        }
        let name = path.rsplit('/').next().unwrap_or(&path);
        if long {
            crate::println!("-rw-r--r-- 1 root root    0 Jan  1 00:00 {}", name);
        } else {
            crate::println!("{}", name);
        }
        return;
    }

    let result = VFS.lock().as_mut().and_then(|vfs| vfs.list_dir(&path).ok());
    let entries = match result {
        Some(e) => e,
        None => {
            crate::println!(
                "ls: cannot open directory '{}': No such file or directory",
                path
            );
            return;
        }
    };

    if print_header {
        if path.is_empty() || path == "." {
            crate::println!("{}:", shell.cwd);
        } else {
            crate::println!("{}:", path);
        }
    }

    let mut sorted = entries;
    sorted.sort_by(|a, b| a.name.cmp(&b.name));

    if long {
        crate::println!("total {}", sorted.len());
    }

    for e in &sorted {
        // skip dotfiles unless -a
        if !show_all && e.name.starts_with('.') {
            continue;
        }
        match e.node_type {
            NodeType::Directory => {
                if long {
                    crate::println!("drwxr-xr-x 2 root root    0 Jan  1 00:00 {}", e.name);
                } else {
                    crate::println!("{}/", e.name);
                }
            }
            NodeType::File => {
                // Try to get file size for long listing
                let size_str = if long {
                    let full_path = if path == "/" {
                        alloc::format!("/{}", e.name)
                    } else {
                        alloc::format!("{}/{}", path, e.name)
                    };
                    let sz = VFS
                        .lock()
                        .as_mut()
                        .and_then(|vfs| vfs.read_file(&full_path).ok())
                        .map(|d| d.len() as u64)
                        .unwrap_or(0);
                    if human {
                        crate::block::fmt_size(sz)
                    } else {
                        alloc::format!("{}", sz)
                    }
                } else {
                    String::new()
                };
                if long {
                    crate::println!(
                        "-rw-r--r-- 1 root root {:>6} Jan  1 00:00 {}",
                        size_str,
                        e.name
                    );
                } else {
                    crate::println!("{}", e.name);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// cd
// ---------------------------------------------------------------------------

fn cmd_cd(shell: &mut Shell, args: &[&str]) {
    let input = args.first().copied().unwrap_or("/");
    let path = shell.resolve_path(input);
    let is_dir = VFS
        .lock()
        .as_mut()
        .map(|vfs| vfs.is_dir(&path))
        .unwrap_or(false);
    if is_dir {
        shell.cwd = path;
    } else {
        crate::println!("bash: cd: {}: No such file or directory", input);
    }
}

// ---------------------------------------------------------------------------
// mkdir — matches Linux mkdir
// ---------------------------------------------------------------------------

fn cmd_mkdir(shell: &mut Shell, args: &[&str]) {
    let mut parents = false;
    let mut paths: Vec<&str> = Vec::new();

    let mut skip_next = false;
    for (i, &a) in args.iter().enumerate() {
        if skip_next {
            skip_next = false;
            continue;
        }
        if a == "-p" || a == "--parents" {
            parents = true;
        } else if a == "-m" {
            // consume mode argument
            skip_next = true;
        } else if a.starts_with("-m") {
            // -m755 style
        } else if a.starts_with('-') && a.len() > 1 {
            for c in a[1..].chars() {
                if c == 'p' {
                    parents = true;
                }
            }
        } else {
            paths.push(a);
        }
        let _ = i;
    }

    if paths.is_empty() {
        crate::println!("mkdir: missing operand");
        crate::println!("Try 'mkdir --help' for more information.");
        return;
    }

    for &input in &paths {
        let path = shell.resolve_path(input);
        if parents {
            mkdir_parents(shell, &path);
        } else {
            let result = VFS.lock().as_mut().map(|vfs| vfs.mkdir(&path));
            match result {
                Some(Ok(())) => {}
                Some(Err(crate::vfs::VfsError::AlreadyExists)) => {
                    crate::println!("mkdir: cannot create directory '{}': File exists", input);
                }
                Some(Err(e)) => {
                    crate::println!("mkdir: cannot create directory '{}': {}", input, e)
                }
                None => crate::println!("mkdir: VFS not initialised"),
            }
        }
    }
}

fn mkdir_parents(shell: &mut Shell, path: &str) {
    // Build up each component and create it if missing
    let mut so_far = String::new();
    for part in path.split('/') {
        if part.is_empty() {
            if so_far.is_empty() {
                so_far.push('/');
            }
            continue;
        }
        if so_far == "/" {
            so_far.push_str(part);
        } else {
            so_far.push('/');
            so_far.push_str(part);
        }
        let exists = VFS
            .lock()
            .as_mut()
            .map(|vfs| vfs.exists(&so_far))
            .unwrap_or(false);
        if !exists {
            let _ = VFS.lock().as_mut().map(|vfs| vfs.mkdir(&so_far));
        }
    }
    let _ = shell;
}

// ---------------------------------------------------------------------------
// rm — matches Linux rm
// ---------------------------------------------------------------------------

fn cmd_rm(shell: &mut Shell, args: &[&str]) {
    let mut recursive = false;
    let mut force = false;
    let mut paths: Vec<&str> = Vec::new();

    for &a in args {
        if a.starts_with('-') && a.len() > 1 && !a.starts_with("--") {
            for c in a[1..].chars() {
                match c {
                    'r' | 'R' => recursive = true,
                    'f' => force = true,
                    _ => {}
                }
            }
        } else if a == "--recursive" {
            recursive = true;
        } else if a == "--force" {
            force = true;
        } else {
            paths.push(a);
        }
    }

    if paths.is_empty() {
        if !force {
            crate::println!("rm: missing operand");
        }
        return;
    }

    for &input in &paths {
        let path = shell.resolve_path(input);
        rm_path(&path, input, recursive, force);
    }
}

fn rm_path(path: &str, display: &str, recursive: bool, force: bool) {
    let is_dir = VFS
        .lock()
        .as_mut()
        .map(|vfs| vfs.is_dir(path))
        .unwrap_or(false);

    if is_dir && !recursive {
        if !force {
            crate::println!("rm: cannot remove '{}': Is a directory", display);
        }
        return;
    }

    if is_dir && recursive {
        // List and remove children first
        let children = VFS
            .lock()
            .as_mut()
            .and_then(|vfs| vfs.list_dir(path).ok())
            .unwrap_or_default();
        for child in children {
            let child_path = if path == "/" {
                alloc::format!("/{}", child.name)
            } else {
                alloc::format!("{}/{}", path, child.name)
            };
            rm_path(&child_path, &child_path, recursive, force);
        }
    }

    let result = VFS.lock().as_mut().map(|vfs| vfs.remove(path));
    match result {
        Some(Ok(())) => {}
        Some(Err(e)) => {
            if !force {
                crate::println!("rm: cannot remove '{}': {}", display, e);
            }
        }
        None => {
            if !force {
                crate::println!("rm: VFS not initialised");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// cat — matches Linux cat
// ---------------------------------------------------------------------------

fn cmd_cat(shell: &mut Shell, args: &[&str]) {
    let mut number_lines = false;
    let mut show_ends = false;
    let mut paths: Vec<&str> = Vec::new();

    for &a in args {
        if a.starts_with('-') && a.len() > 1 {
            for c in a[1..].chars() {
                match c {
                    'n' => number_lines = true,
                    'A' => show_ends = true,
                    'e' => show_ends = true,
                    'E' => show_ends = true,
                    _ => {}
                }
            }
        } else {
            paths.push(a);
        }
    }

    if paths.is_empty() {
        crate::println!("cat: (standard input not supported in RustOS)");
        return;
    }

    for &input in &paths {
        let path = shell.resolve_path(input);
        let data = VFS
            .lock()
            .as_mut()
            .and_then(|vfs| vfs.read_file(&path).ok());
        match data {
            None => crate::println!("cat: {}: No such file or directory", input),
            Some(bytes) => {
                let text = core::str::from_utf8(&bytes).ok();
                match text {
                    Some(s) => {
                        if number_lines {
                            for (i, line) in s.lines().enumerate() {
                                if show_ends {
                                    crate::println!("{:6}\t{}$", i + 1, line);
                                } else {
                                    crate::println!("{:6}\t{}", i + 1, line);
                                }
                            }
                        } else if show_ends {
                            for line in s.lines() {
                                crate::println!("{}$", line);
                            }
                        } else {
                            crate::print!("{}", s);
                            if !s.ends_with('\n') {
                                crate::println!();
                            }
                        }
                    }
                    None => {
                        // Binary file: print hex dump like `cat` on binary
                        for &b in &bytes {
                            if b.is_ascii_graphic() || b == b' ' {
                                crate::print!("{}", b as char);
                            } else {
                                crate::print!(".");
                            }
                        }
                        crate::println!();
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// write (RustOS-specific, like `tee`)
// ---------------------------------------------------------------------------

fn cmd_write(shell: &mut Shell, args: &[&str]) {
    if args.len() < 2 {
        crate::println!("Usage: write <path> <text>");
        return;
    }
    let path = shell.resolve_path(args[0]);
    let text = args[1..].join(" ");
    let result = VFS
        .lock()
        .as_mut()
        .map(|vfs| vfs.write_file(&path, text.as_bytes()));
    match result {
        Some(Ok(())) => {}
        Some(Err(e)) => crate::println!("write: {}: {}", args[0], e),
        None => crate::println!("write: VFS not initialised"),
    }
}

// ---------------------------------------------------------------------------
// cp — matches Linux cp
// ---------------------------------------------------------------------------

fn cmd_cp(shell: &mut Shell, args: &[&str]) {
    let mut recursive = false;
    let mut paths: Vec<&str> = Vec::new();

    for &a in args {
        if a.starts_with('-') && a.len() > 1 {
            for c in a[1..].chars() {
                match c {
                    'r' | 'R' => recursive = true,
                    _ => {}
                }
            }
        } else if a == "--recursive" {
            recursive = true;
        } else {
            paths.push(a);
        }
    }

    if paths.len() < 2 {
        crate::println!("cp: missing destination file operand");
        crate::println!("Try 'cp --help' for more information.");
        return;
    }

    let dst_input = paths[paths.len() - 1];
    let dst = shell.resolve_path(dst_input);
    let srcs = &paths[..paths.len() - 1];

    for &src_input in srcs {
        let src = shell.resolve_path(src_input);
        let is_dir = VFS
            .lock()
            .as_mut()
            .map(|vfs| vfs.is_dir(&src))
            .unwrap_or(false);
        if is_dir && !recursive {
            crate::println!("cp: -r not specified; omitting directory '{}'", src_input);
            continue;
        }
        // Determine actual destination
        let actual_dst = if VFS
            .lock()
            .as_mut()
            .map(|vfs| vfs.is_dir(&dst))
            .unwrap_or(false)
        {
            let fname = src.rsplit('/').next().unwrap_or(&src);
            if dst == "/" {
                alloc::format!("/{}", fname)
            } else {
                alloc::format!("{}/{}", dst, fname)
            }
        } else {
            dst.clone()
        };
        if is_dir {
            cp_dir(&src, &actual_dst);
        } else {
            let result = VFS.lock().as_mut().map(|vfs| vfs.copy(&src, &actual_dst));
            match result {
                Some(Ok(())) => {}
                Some(Err(e)) => crate::println!("cp: cannot copy '{}': {}", src_input, e),
                None => crate::println!("cp: VFS not initialised"),
            }
        }
    }
}

fn cp_dir(src: &str, dst: &str) {
    let _ = VFS.lock().as_mut().map(|vfs| vfs.mkdir(dst));
    let children = VFS
        .lock()
        .as_mut()
        .and_then(|vfs| vfs.list_dir(src).ok())
        .unwrap_or_default();
    for child in children {
        let src_child = if src == "/" {
            alloc::format!("/{}", child.name)
        } else {
            alloc::format!("{}/{}", src, child.name)
        };
        let dst_child = if dst == "/" {
            alloc::format!("/{}", child.name)
        } else {
            alloc::format!("{}/{}", dst, child.name)
        };
        match child.node_type {
            NodeType::Directory => cp_dir(&src_child, &dst_child),
            NodeType::File => {
                let _ = VFS
                    .lock()
                    .as_mut()
                    .map(|vfs| vfs.copy(&src_child, &dst_child));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// mv — matches Linux mv
// ---------------------------------------------------------------------------

fn cmd_mv(shell: &mut Shell, args: &[&str]) {
    let mut force = false;
    let mut no_clobber = false;
    let mut paths: Vec<&str> = Vec::new();

    for &a in args {
        if a.starts_with('-') && a.len() > 1 {
            for c in a[1..].chars() {
                match c {
                    'f' => force = true,
                    'n' => no_clobber = true,
                    _ => {}
                }
            }
        } else {
            paths.push(a);
        }
    }

    if paths.len() < 2 {
        crate::println!("mv: missing destination file operand");
        crate::println!("Try 'mv --help' for more information.");
        return;
    }

    let dst_input = paths[paths.len() - 1];
    let dst = shell.resolve_path(dst_input);
    let srcs = &paths[..paths.len() - 1];

    for &src_input in srcs {
        let src = shell.resolve_path(src_input);
        // If dst is a directory, move inside it
        let actual_dst = if VFS
            .lock()
            .as_mut()
            .map(|vfs| vfs.is_dir(&dst))
            .unwrap_or(false)
        {
            let fname = src.rsplit('/').next().unwrap_or(&src);
            if dst == "/" {
                alloc::format!("/{}", fname)
            } else {
                alloc::format!("{}/{}", dst, fname)
            }
        } else {
            dst.clone()
        };

        // Check no-clobber
        if no_clobber
            && VFS
                .lock()
                .as_mut()
                .map(|vfs| vfs.exists(&actual_dst))
                .unwrap_or(false)
        {
            continue;
        }

        let result = VFS.lock().as_mut().map(|vfs| vfs.rename(&src, &actual_dst));
        match result {
            Some(Ok(())) => {}
            Some(Err(e)) => {
                if !force {
                    crate::println!("mv: cannot move '{}' to '{}': {}", src_input, dst_input, e);
                }
            }
            None => crate::println!("mv: VFS not initialised"),
        }
    }
}

// ---------------------------------------------------------------------------
// meminfo (RustOS-specific)
// ---------------------------------------------------------------------------

fn cmd_meminfo() {
    crate::println!("Heap start: 0x{:016x}", crate::allocator::HEAP_START);
    crate::println!(
        "Heap size:  {} KiB ({} bytes)",
        crate::allocator::heap_size() / 1024,
        crate::allocator::heap_size(),
    );
}

// ---------------------------------------------------------------------------
// mount — matches Linux mount
// ---------------------------------------------------------------------------

fn cmd_mount(shell: &mut Shell, args: &[&str]) {
    // No args: show all mounts (like /proc/mounts)
    if args.is_empty() || args.iter().all(|a| a.starts_with('-') && !a.contains('/')) {
        // Check if there's actually a device/path argument
        let has_target = args.iter().any(|a| !a.starts_with('-'));
        if !has_target {
            show_mounts();
            return;
        }
    }

    // Parse: mount [-t fstype] [-o opts] <source> <target>
    // or: mount (no args → list)
    let mut fstype: Option<&str> = None;
    let mut positional: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i] {
            "-t" | "--types" => {
                i += 1;
                if i < args.len() {
                    fstype = Some(args[i]);
                }
            }
            "-o" | "--options" => {
                i += 1; // skip options value
            }
            a if a.starts_with('-') => {} // ignore other flags
            a => positional.push(a),
        }
        i += 1;
    }

    if positional.is_empty() {
        show_mounts();
        return;
    }
    if positional.len() < 2 {
        crate::println!("mount: missing target; use: mount <source> <mountpoint>");
        return;
    }

    let source = positional[0];
    let target_input = positional[1];
    let target = shell.resolve_path(target_input);

    // Ensure mount point directory exists
    {
        let exists = VFS
            .lock()
            .as_mut()
            .map(|vfs| vfs.exists(&target))
            .unwrap_or(false);
        if !exists {
            crate::println!("mount: mount point '{}' does not exist", target_input);
            return;
        }
    }

    // Try to find the block device by name and mount it
    let _ = fstype;
    mount_block_device(source, &target);
}

fn show_mounts() {
    let vfs = VFS.lock();
    match vfs.as_ref() {
        Some(vfs) => {
            // Format: device on mountpoint type fstype (options)
            crate::println!(
                "{} on / type {} (rw,relatime)",
                if vfs.root_name().contains("fat32") {
                    "RUSTOS_ROOT"
                } else {
                    "ramfs"
                },
                if vfs.root_name().contains("fat32") {
                    "vfat"
                } else {
                    "ramfs"
                },
            );
            for mp in vfs.list_mounts() {
                crate::println!("block on {} type vfat (rw,relatime)", mp);
            }
        }
        None => crate::println!("mount: VFS not initialised"),
    }
}

fn mount_block_device(source: &str, target: &str) {
    // Try USB devices first (match by name like "sda", "sda1", "sda2")
    let usb_count = crate::usb::USB_XHCI
        .lock()
        .as_ref()
        .map(|x| x.devices.len())
        .unwrap_or(0);

    for dev_idx in 0..usb_count {
        let (block_count, block_size) = {
            let xhci = crate::usb::USB_XHCI.lock();
            xhci.as_ref()
                .and_then(|x| x.devices.get(dev_idx))
                .map(|d| (d.block_count, d.block_size))
                .unwrap_or((0, 512))
        };
        let dev_name = if dev_idx == 0 {
            String::from("sda")
        } else {
            alloc::format!("sd{}", (b'a' + dev_idx as u8) as char)
        };

        // Match whole device
        if source == dev_name {
            try_mount_usb_whole(dev_idx, target);
            return;
        }

        // Match partition (sda1, sda2, …)
        if let Some(part_num_str) = source.strip_prefix(&dev_name)
            && let Ok(part_num) = part_num_str.parse::<usize>()
            && part_num >= 1
        {
            try_mount_usb_partition(dev_idx, part_num - 1, block_size, target, source);
            return;
        }
        let _ = block_count;
    }

    crate::println!("mount: special device {} does not exist", source);
    crate::println!("       (use 'lsblk' to list available block devices)");
}

fn try_mount_usb_whole(dev_idx: usize, target: &str) {
    use crate::usb::XhciBlockDevice;
    if let Some(fat32) =
        crate::fs::fat32::Fat32Fs::new(alloc::boxed::Box::new(XhciBlockDevice { dev_idx }))
    {
        let mut vfs = VFS.lock();
        if let Some(vfs) = vfs.as_mut() {
            vfs.mount(
                target,
                alloc::boxed::Box::new(crate::vfs::Fat32Mount(fat32)),
            );
            crate::println!("mounted on {}", target);
        }
    } else {
        crate::println!("mount: no supported filesystem on device");
    }
}

fn try_mount_usb_partition(
    dev_idx: usize,
    part_idx: usize,
    _block_size: u32,
    target: &str,
    source: &str,
) {
    use crate::usb::{PartitionBlockDevice, XhciBlockDevice};

    let partitions = crate::usb::gpt_partitions_for_device_pub(dev_idx);

    if let Some(part) = partitions.get(part_idx) {
        let block_dev: alloc::boxed::Box<dyn crate::usb::BlockDevice> =
            alloc::boxed::Box::new(PartitionBlockDevice::new(
                alloc::boxed::Box::new(XhciBlockDevice { dev_idx }),
                part.start_lba,
                part.sector_count,
            ));
        if let Some(fat32) = crate::fs::fat32::Fat32Fs::new(block_dev) {
            let mut vfs = VFS.lock();
            if let Some(vfs) = vfs.as_mut() {
                vfs.mount(
                    target,
                    alloc::boxed::Box::new(crate::vfs::Fat32Mount(fat32)),
                );
                crate::println!("mounted on {}", target);
                return;
            }
        }
        crate::println!(
            "mount: {}: no supported filesystem (only FAT32 is currently supported)",
            source
        );
    } else {
        crate::println!("mount: {}: partition not found", source);
    }
}

// ---------------------------------------------------------------------------
// umount
// ---------------------------------------------------------------------------

fn cmd_umount(shell: &mut Shell, args: &[&str]) {
    let paths: Vec<&str> = args
        .iter()
        .filter(|a| !a.starts_with('-'))
        .copied()
        .collect();
    if paths.is_empty() {
        crate::println!("umount: no target specified");
        return;
    }
    for &input in &paths {
        let target = shell.resolve_path(input);
        let result = VFS.lock().as_mut().map(|vfs| vfs.umount(&target));
        match result {
            Some(true) => {}
            Some(false) => crate::println!("umount: {}: not mounted", input),
            None => crate::println!("umount: VFS not initialised"),
        }
    }
}

// ---------------------------------------------------------------------------
// exec (RustOS-specific)
// ---------------------------------------------------------------------------

fn cmd_exec(shell: &mut Shell, args: &[&str]) {
    let Some(&input) = args.first() else {
        crate::println!("Usage: exec <path>");
        return;
    };
    let path = shell.resolve_path(input);
    if let Some(code) = crate::bin_commands::run_virtual_bin_command(&path) {
        crate::println!("exec: process exited with code {}", code);
        return;
    }
    let data = {
        let result = VFS
            .lock()
            .as_mut()
            .and_then(|vfs| vfs.read_file(&path).ok());
        match result {
            Some(d) => d,
            None => {
                crate::println!("exec: {}: not found", path);
                return;
            }
        }
    };
    let _ = data;
    crate::println!("exec: user processes are started by init; use the userland shell");
}

// ---------------------------------------------------------------------------
// usbscan (RustOS-specific)
// ---------------------------------------------------------------------------

fn cmd_usbscan() {
    crate::println!("Scanning USB ports for new devices...");
    let new_devs = crate::usb::scan_and_mount();
    if new_devs == 0 {
        crate::println!("usbscan: no new USB storage devices found");
    } else {
        crate::println!("usbscan: {} new device(s) mounted", new_devs);
    }
}

fn cmd_reboot() {
    crate::acpi::reboot();
}

fn cmd_shutdown() {
    crate::acpi::shutdown();
}

// ---------------------------------------------------------------------------
// lspci — matches Linux lspci
// ---------------------------------------------------------------------------

fn pci_vendor_name(vendor: u16) -> &'static str {
    match vendor {
        0x1002 => "AMD/ATI",
        0x1022 => "AMD",
        0x1106 => "VIA Technologies",
        0x10B9 => "ALi",
        0x10DE => "NVIDIA Corporation",
        0x10EC => "Realtek Semiconductor Co., Ltd.",
        0x1234 => "QEMU/Bochs",
        0x1AF4 => "Red Hat, Inc.",
        0x1B21 => "ASMedia Technology Inc.",
        0x1B36 => "Red Hat, Inc.",
        0x15AD => "VMware",
        0x15B7 => "Sandisk Corp.",
        0x80EE => "InnoTek Systemberatung GmbH",
        0x8086 => "Intel Corporation",
        _ => "Unknown vendor",
    }
}

fn pci_class_name(class: u8, subclass: u8, prog_if: u8) -> &'static str {
    match (class, subclass, prog_if) {
        (0x00, 0x00, _) => "Non-VGA-compatible unclassified device",
        (0x00, 0x01, _) => "VGA-compatible unclassified device",
        (0x01, 0x00, _) => "SCSI storage controller",
        (0x01, 0x01, _) => "IDE interface",
        (0x01, 0x02, _) => "Floppy disk controller",
        (0x01, 0x05, _) => "ATA controller",
        (0x01, 0x06, 0x01) => "SATA controller",
        (0x01, 0x06, _) => "SATA controller",
        (0x01, 0x07, _) => "Serial Attached SCSI controller",
        (0x01, 0x08, 0x02) => "Non-Volatile memory controller",
        (0x01, 0x08, _) => "Mass storage controller",
        (0x02, 0x00, _) => "Ethernet controller",
        (0x02, 0x80, _) => "Network controller",
        (0x03, 0x00, _) => "VGA compatible controller",
        (0x03, 0x01, _) => "XGA compatible controller",
        (0x03, 0x02, _) => "3D controller",
        (0x04, 0x00, _) => "Multimedia video controller",
        (0x04, 0x01, _) => "Multimedia audio controller",
        (0x04, 0x03, _) => "Audio device",
        (0x05, 0x00, _) => "RAM memory",
        (0x06, 0x00, _) => "Host bridge",
        (0x06, 0x01, _) => "ISA bridge",
        (0x06, 0x02, _) => "EISA bridge",
        (0x06, 0x04, _) => "PCI bridge",
        (0x07, 0x00, _) => "Serial controller",
        (0x07, 0x01, _) => "Parallel controller",
        (0x08, 0x00, _) => "PIC",
        (0x08, 0x01, _) => "DMA controller",
        (0x08, 0x02, _) => "Timer",
        (0x08, 0x03, _) => "RTC",
        (0x09, 0x00, _) => "Keyboard controller",
        (0x09, 0x02, _) => "Mouse controller",
        (0x0B, 0x00, _) => "386",
        (0x0C, 0x00, _) => "FireWire (IEEE 1394)",
        (0x0C, 0x03, 0x00) => "USB controller",
        (0x0C, 0x03, 0x10) => "USB controller",
        (0x0C, 0x03, 0x20) => "USB controller",
        (0x0C, 0x03, 0x30) => "USB controller",
        (0x0C, 0x05, _) => "SMBus",
        (0x0C, 0x06, _) => "InfiniBand",
        (0x0D, 0x11, _) => "Bluetooth",
        (0x0D, 0x12, _) => "802.11 Wireless",
        (0x12, 0x00, _) => "Processing accelerators",
        _ => "Unclassified device",
    }
}

pub fn cmd_lspci(args: &[&str]) {
    let verbose = args.iter().any(|&a| a == "-v" || a == "-vv" || a == "-vvv");
    let numeric = args.contains(&"-n");
    let numeric_name = args.contains(&"-nn");
    let slot_filter: Option<&str> = args.windows(2).find(|w| w[0] == "-s").map(|w| w[1]);

    let devices = crate::pci::enumerate();
    if devices.is_empty() {
        return; // no output on empty — like real lspci
    }

    for d in &devices {
        let slot = alloc::format!("{:02x}:{:02x}.{}", d.bus, d.dev, d.func);
        if let Some(filter) = slot_filter
            && !slot.starts_with(filter)
        {
            continue;
        }

        if numeric {
            crate::println!("{} {:04x}:{:04x}", slot, d.vendor_id, d.device_id,);
        } else if numeric_name {
            crate::println!(
                "{} {} [{}] [{:04x}:{:04x}]",
                slot,
                pci_class_name(d.class, d.subclass, d.prog_if),
                pci_vendor_name(d.vendor_id),
                d.vendor_id,
                d.device_id,
            );
        } else {
            crate::println!(
                "{} {}: {} (rev {:02x})",
                slot,
                pci_class_name(d.class, d.subclass, d.prog_if),
                pci_vendor_name(d.vendor_id),
                d.revision,
            );
        }

        if verbose {
            crate::println!("\tVendor ID: {:04x}", d.vendor_id);
            crate::println!("\tDevice ID: {:04x}", d.device_id);
            crate::println!(
                "\tClass:     {:02x}{:02x} ({})",
                d.class,
                d.subclass,
                pci_class_name(d.class, d.subclass, d.prog_if)
            );
            for (i, bar) in d.bars.iter().enumerate() {
                match *bar {
                    crate::pci::Bar::Mmio { addr, size, .. } => {
                        crate::println!("\tBAR{}:      mem {:#x} [size {:#x}]", i, addr, size)
                    }
                    crate::pci::Bar::Io { port, size } => {
                        crate::println!("\tBAR{}:      io  {:#x} [size {:#x}]", i, port, size)
                    }
                    crate::pci::Bar::None => {}
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// lsusb — matches Linux lsusb
// ---------------------------------------------------------------------------

pub fn cmd_lsusb(args: &[&str]) {
    let verbose = args.contains(&"-v");
    let tree = args.contains(&"-t");

    // Trigger a rescan so a recently-plugged USB drive shows up
    {
        let mut xhci = crate::usb::USB_XHCI.lock();
        if let Some(x) = xhci.as_mut() {
            x.scan_new_ports();
        }
    }

    let xhci = crate::usb::USB_XHCI.lock();
    match xhci.as_ref() {
        None => {
            // No XHCI controller — show nothing (like real lsusb on a system
            // without USB would not show anything for that bus)
        }
        Some(ctrl) => {
            if tree {
                crate::println!(
                    "/:  Bus 001.Port 001: Dev 001, Class=root_hub, Driver=xhci_hcd/1p, 480M"
                );
                for (i, dev) in ctrl.devices.iter().enumerate() {
                    let size_mb = dev.block_count * dev.block_size as u64 / (1024 * 1024);
                    crate::println!(
                        "    |__ Port {:03}: Dev {:03}, Class=Mass Storage, Driver=usb-storage, {}",
                        i + 1,
                        i + 2,
                        if size_mb > 0 {
                            alloc::format!("{} MiB", size_mb)
                        } else {
                            String::from("unknown size")
                        }
                    );
                }
                return;
            }

            if ctrl.devices.is_empty() {
                // Print the root hub only (like real lsusb)
                crate::println!(
                    "Bus 001 Device 001: ID 8086:0000 Intel Corporation xHCI Host Controller"
                );
                return;
            }

            crate::println!(
                "Bus 001 Device 001: ID 8086:0000 Intel Corporation xHCI Host Controller"
            );
            for (i, dev) in ctrl.devices.iter().enumerate() {
                let size_mb = dev.block_count * dev.block_size as u64 / (1024 * 1024);
                crate::println!(
                    "Bus 001 Device {:03}: ID 0000:0000 USB Mass Storage Device",
                    i + 2,
                );
                if verbose {
                    crate::println!("  bDeviceClass        8 (Mass Storage)",);
                    crate::println!("  bDeviceSubClass     6 (SCSI)");
                    crate::println!("  bDeviceProtocol    80 (Bulk-Only)");
                    crate::println!(
                        "  Capacity: {} MiB ({} blocks × {} B)",
                        size_mb,
                        dev.block_count,
                        dev.block_size,
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// lsblk — matches Linux lsblk
// ---------------------------------------------------------------------------

pub fn cmd_lsblk(args: &[&str]) {
    // Flags
    let fs_info = args.contains(&"-f");
    let list_fmt = args.contains(&"-l");
    let no_heading = args.contains(&"-n");
    let full_path = args.contains(&"-p");

    // Custom output columns: -o NAME,SIZE,TYPE,...
    let custom_cols: Option<Vec<&str>> = args
        .windows(2)
        .find(|w| w[0] == "-o")
        .map(|w| w[1].split(',').collect());

    // Optional device filter
    let device_filter: Vec<&str> = args
        .iter()
        .filter(|&&a| !a.starts_with('-'))
        .copied()
        .collect();

    // Probe all block devices
    let devs = crate::block::probe_block_devices();

    // Determine columns to show
    let cols: Vec<&str> = custom_cols
        .as_deref()
        .unwrap_or(if fs_info {
            &["NAME", "FSTYPE", "SIZE", "RO", "TYPE", "MOUNTPOINT"][..]
        } else {
            &["NAME", "MAJ:MIN", "RM", "SIZE", "RO", "TYPE", "MOUNTPOINT"][..]
        })
        .to_vec();

    if !no_heading {
        // Print header
        let header = cols.to_vec();
        crate::println!("{}", header.join("   "));
    }

    let mounts = VFS
        .lock()
        .as_ref()
        .map(|v| v.list_mounts())
        .unwrap_or_default();
    let root_is_fat32 = VFS
        .lock()
        .as_ref()
        .map(|v| v.root_name().contains("fat32"))
        .unwrap_or(false);

    for bd in &devs {
        if !device_filter.is_empty()
            && !device_filter
                .iter()
                .any(|&f| bd.name == f || bd.name.starts_with(f))
        {
            continue;
        }

        let dev_name = if full_path {
            alloc::format!("/dev/{}", bd.name)
        } else {
            bd.name.clone()
        };

        // Determine mount point for the whole disk
        let disk_mountpoint = {
            if bd.bus == crate::block::BusType::Usb && root_is_fat32 {
                "/"
            } else {
                ""
            }
        };

        let type_str = match bd.bus {
            crate::block::BusType::Nvme => "disk",
            crate::block::BusType::Ahci => "disk",
            crate::block::BusType::Usb => "disk",
        };

        print_lsblk_row(
            &dev_name,
            &bd.name,
            bd.size_bytes(),
            type_str,
            "",
            disk_mountpoint,
            false,
            &cols,
            &bd.model,
        );

        // Print partitions
        for part in &bd.partitions {
            let part_name = if full_path {
                alloc::format!("/dev/{}", part.name)
            } else {
                part.name.clone()
            };
            let prefix = if list_fmt { "" } else { "├─" };
            let display_name = if list_fmt {
                part_name.clone()
            } else {
                alloc::format!("{}{}", prefix, part_name)
            };

            // Find if partition is mounted
            let mountpoint = mounts
                .iter()
                .find(|m| {
                    // heuristic: part name suffix matches mount path tail
                    m.contains(&part.name) || m.contains(&part_name)
                })
                .map(|s| s.as_str())
                .unwrap_or("");

            let fs_str = part.fs_type.as_str();
            print_lsblk_row(
                &display_name,
                &part.name,
                part.size_bytes(),
                "part",
                fs_str,
                mountpoint,
                false,
                &cols,
                &part.part_type,
            );
        }
    }

    if devs.is_empty() {
        // Still show loop0 etc. like real lsblk would on a minimal system
        if !no_heading {
            crate::println!("(no block devices detected)");
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn print_lsblk_row(
    display_name: &str,
    raw_name: &str,
    size_bytes: u64,
    type_str: &str,
    fs_type: &str,
    mountpoint: &str,
    read_only: bool,
    cols: &[&str],
    extra: &str,
) {
    let ro_str = if read_only { "1" } else { "0" };
    let size_str = crate::block::fmt_size(size_bytes);
    let maj_min = match type_str {
        "disk" if raw_name.starts_with("nvme") => "259:0",
        "disk" => "8:0",
        "part" => "8:1",
        _ => "0:0",
    };

    let mut parts: Vec<String> = Vec::new();
    for &col in cols {
        let val = match col {
            "NAME" => display_name.to_string(),
            "MAJ:MIN" => maj_min.to_string(),
            "RM" => "0".to_string(),
            "SIZE" => size_str.clone(),
            "RO" => ro_str.to_string(),
            "TYPE" => type_str.to_string(),
            "FSTYPE" => fs_type.to_string(),
            "LABEL" => String::new(),
            "MOUNTPOINT" | "MOUNTPOINTS" => mountpoint.to_string(),
            "MODEL" => extra.to_string(),
            _ => String::new(),
        };
        parts.push(val);
    }
    crate::println!("{}", parts.join("   "));
}

// ---------------------------------------------------------------------------
// grep — matches Linux grep
// ---------------------------------------------------------------------------

fn cmd_grep(shell: &mut Shell, args: &[&str]) {
    let mut ignore_case = false;
    let mut line_numbers = false;
    let mut invert = false;
    let mut count_only = false;
    let mut list_files = false;
    let mut recursive = false;
    let mut pattern: Option<&str> = None;
    let mut files: Vec<&str> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        let a = args[i];
        if a == "-e" || a == "--regexp" {
            i += 1;
            if i < args.len() && pattern.is_none() {
                pattern = Some(args[i]);
            }
        } else if a.starts_with('-') && a.len() > 1 && !a.starts_with("--") {
            for c in a[1..].chars() {
                match c {
                    'i' | 'y' => ignore_case = true,
                    'n' => line_numbers = true,
                    'v' => invert = true,
                    'c' => count_only = true,
                    'l' | 'L' => list_files = true,
                    'r' | 'R' => recursive = true,
                    'E' | 'F' | 'P' => {} // treat patterns as literal substrings
                    'H' => {}             // always print filename — we handle below
                    'h' => {}             // suppress filename — handled implicitly
                    _ => {}
                }
            }
        } else if a == "--ignore-case" {
            ignore_case = true;
        } else if a == "--line-number" {
            line_numbers = true;
        } else if a == "--invert-match" {
            invert = true;
        } else if a == "--count" {
            count_only = true;
        } else if a == "--files-with-matches" {
            list_files = true;
        } else if a == "--recursive" {
            recursive = true;
        } else {
            // positional
            if pattern.is_none() {
                pattern = Some(a);
            } else {
                files.push(a);
            }
        }
        i += 1;
    }

    let pat = match pattern {
        Some(p) => p,
        None => {
            crate::println!("grep: no pattern specified");
            crate::println!("Usage: grep [-invrcl] PATTERN [FILE...]");
            return;
        }
    };

    if files.is_empty() {
        crate::println!("grep: (stdin not supported)");
        return;
    }

    let multi_file = files.len() > 1 || recursive;

    for &file_input in &files {
        let path = shell.resolve_path(file_input);
        if recursive {
            let opts = GrepOptions {
                pat,
                ignore_case,
                line_numbers,
                invert,
                count_only,
                list_files,
            };
            grep_recursive(&path, file_input, &opts);
        } else {
            grep_file(
                &path,
                file_input,
                &GrepOptions {
                    pat,
                    ignore_case,
                    line_numbers,
                    invert,
                    count_only,
                    list_files,
                },
                multi_file,
            );
        }
    }
}

fn grep_file(path: &str, display: &str, opts: &GrepOptions<'_>, print_filename: bool) {
    let data = VFS.lock().as_mut().and_then(|vfs| vfs.read_file(path).ok());
    let data = match data {
        Some(d) => d,
        None => {
            crate::println!("grep: {}: No such file or directory", display);
            return;
        }
    };
    let text = match core::str::from_utf8(&data) {
        Ok(s) => s,
        Err(_) => {
            crate::println!("grep: {}: Binary file matches", display);
            return;
        }
    };

    let pat_cmp = if opts.ignore_case {
        opts.pat.to_lowercase()
    } else {
        opts.pat.to_string()
    };

    let mut count = 0usize;
    let mut matched = false;

    for (line_idx, line) in text.lines().enumerate() {
        let line_cmp = if opts.ignore_case {
            line.to_lowercase()
        } else {
            line.to_string()
        };
        let hits = line_cmp.contains(&pat_cmp);
        let show = if opts.invert { !hits } else { hits };
        if show {
            count += 1;
            matched = true;
            if !opts.count_only && !opts.list_files {
                let prefix = if print_filename {
                    alloc::format!("{}:", display)
                } else {
                    String::new()
                };
                if opts.line_numbers {
                    crate::println!("{}{}:{}", prefix, line_idx + 1, line);
                } else {
                    crate::println!("{}{}", prefix, line);
                }
            }
        }
    }

    if opts.count_only {
        if print_filename {
            crate::println!("{}:{}", display, count);
        } else {
            crate::println!("{}", count);
        }
    } else if opts.list_files && matched {
        crate::println!("{}", display);
    }
}

fn grep_recursive(path: &str, display: &str, opts: &GrepOptions<'_>) {
    let is_dir = VFS
        .lock()
        .as_mut()
        .map(|vfs| vfs.is_dir(path))
        .unwrap_or(false);
    if is_dir {
        let children = VFS
            .lock()
            .as_mut()
            .and_then(|vfs| vfs.list_dir(path).ok())
            .unwrap_or_default();
        for child in children {
            let child_path = if path == "/" {
                alloc::format!("/{}", child.name)
            } else {
                alloc::format!("{}/{}", path, child.name)
            };
            let child_display = alloc::format!("{}/{}", display, child.name);
            grep_recursive(&child_path, &child_display, opts);
        }
    } else {
        grep_file(path, display, opts, true);
    }
}

// ---------------------------------------------------------------------------
// ps — matches Linux ps
// ---------------------------------------------------------------------------

pub fn cmd_ps(args: &[&str]) {
    // Parse flags: `ps`, `ps aux`, `ps -e`, `ps -ef`, etc.
    let aux = args.iter().any(|&a| a == "aux" || a == "-aux" || a == "ax");
    let full = args.iter().any(|&a| a == "-f" || a == "-ef");
    let all = args.iter().any(|&a| a == "-e" || a == "-A") || aux;

    let _ = (full, aux, all);
    crate::println!("  TID STATE    TYPE   NAME");
    for (tid, name, state, user) in crate::sched::thread_list() {
        crate::println!(
            "{:5} {:8} {:6} {}",
            tid,
            alloc::format!("{:?}", state),
            if user { "user" } else { "kernel" },
            name
        );
    }
}

struct GrepOptions<'a> {
    pat: &'a str,
    ignore_case: bool,
    line_numbers: bool,
    invert: bool,
    count_only: bool,
    list_files: bool,
}
