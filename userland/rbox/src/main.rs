//! rbox: multi-call binary with the standard command-line tools.
//!
//! The applet is chosen by `argv[0]` (installed as symlinks in /bin) or by
//! the first argument (`rbox ls -l`).

#![no_std]
#![no_main]

extern crate alloc;

mod files;
mod fsutil;
mod hash;
mod hw;
mod input;
mod login;
mod regex;
mod sys;
mod text;

use rustos_rt::fs;
use rustos_rt::prelude::*;

rustos_rt::entry!(main);

pub type Applet = fn(&[String]) -> i32;

const APPLETS: &[(&str, Applet)] = &[
    ("basename", text::basename),
    ("bugreport", hw::bugreport),
    ("cat", files::cat),
    ("chmod", files::chmod),
    ("chvt", sys::chvt),
    ("clear", sys::clear),
    ("cmp", files::cmp),
    ("cp", files::cp),
    ("cut", text::cut),
    ("date", sys::date),
    ("dd", files::dd),
    ("df", sys::df),
    ("diff", text::diff),
    ("dirname", text::dirname),
    ("dmesg", sys::dmesg),
    ("du", files::du),
    ("echo", text::echo),
    ("env", sys::env),
    ("evtest", input::evtest),
    ("fallocate", fsutil::fallocate),
    ("false", |_| 1),
    ("fgconsole", sys::fgconsole),
    ("find", files::find),
    ("free", sys::free),
    ("grep", text::grep),
    ("head", text::head),
    ("hexdump", files::hexdump),
    ("hostname", sys::hostname),
    ("hwcheck", hw::hwcheck),
    ("id", sys::id),
    ("tty", sys::tty),
    ("kill", sys::kill),
    ("ln", files::ln),
    ("login", login::login),
    ("ls", files::ls),
    ("lsblk", sys::lsblk),
    ("lspci", sys::lspci),
    ("lsusb", sys::lsusb),
    ("md5sum", files::md5sum),
    ("meminfo", sys::free),
    ("mkdir", files::mkdir),
    ("mkfifo", files::mkfifo),
    ("mkfs", sys::mkfs),
    ("mkfs.fat", sys::mkfs),
    ("mkfs.vfat", sys::mkfs),
    ("more", text::more),
    ("mount", sys::mount),
    ("mv", files::mv),
    ("nl", text::nl),
    ("passwd", login::passwd),
    ("poweroff", sys::poweroff),
    ("printf", text::printf),
    ("ps", sys::ps),
    ("pwd", files::pwd),
    ("readlink", files::readlink),
    ("realpath", files::realpath),
    ("reboot", sys::reboot),
    ("rev", text::rev),
    ("rm", files::rm),
    ("rmdir", files::rmdir),
    ("sed", text::sed),
    ("seq", text::seq),
    ("sha256sum", files::sha256sum),
    ("shutdown", sys::poweroff),
    ("sleep", sys::sleep),
    ("sort", text::sort),
    ("stat", files::stat),
    ("sync", sys::sync),
    ("tac", text::tac),
    ("tail", text::tail),
    ("tee", text::tee),
    ("test", sys::test),
    ("[", sys::test),
    ("time", sys::time),
    ("top", sys::top),
    ("touch", files::touch),
    ("tr", text::tr),
    ("truncate", fsutil::truncate),
    ("true", |_| 0),
    ("umount", sys::umount),
    ("uname", sys::uname),
    ("uniq", text::uniq),
    ("uptime", sys::uptime),
    ("watch", sys::watch),
    ("wc", text::wc),
    ("which", sys::which),
    ("whoami", sys::whoami),
    ("xargs", text::xargs),
    ("xxd", files::hexdump),
    ("yes", text::yes),
];

fn main(args: Vec<String>) -> i32 {
    let name = fs::basename(args.first().map_or("rbox", |s| s.as_str())).to_string();
    if let Some((_, f)) = APPLETS.iter().find(|(n, _)| *n == name) {
        return f(&args);
    }
    if name == "rbox" {
        if let Some(sub) = args.get(1)
            && let Some((_, f)) = APPLETS.iter().find(|(n, _)| n == sub)
        {
            return f(&args[1..]);
        }
        println!("rbox: multi-call binary. Applets:");
        let names: Vec<&str> = APPLETS.iter().map(|(n, _)| *n).collect();
        println!("  {}", names.join(" "));
        return 0;
    }
    eprintln!("rbox: unknown applet '{}'", name);
    127
}

/// Split `args[1..]` into (flags, operands). Flags are single-dash clusters
/// ("-la" -> 'l','a'); "--" ends flags; long options are returned as-is.
pub fn parse_flags(args: &[String]) -> (Vec<char>, Vec<String>, Vec<String>) {
    let mut flags = Vec::new();
    let mut long = Vec::new();
    let mut operands = Vec::new();
    let mut done = false;
    for a in args.iter().skip(1) {
        if done || a == "-" || !a.starts_with('-') {
            operands.push(a.clone());
        } else if a == "--" {
            done = true;
        } else if let Some(l) = a.strip_prefix("--") {
            long.push(l.to_string());
        } else {
            flags.extend(a[1..].chars());
        }
    }
    (flags, operands, long)
}

pub fn err(applet: &str, what: &str, e: rustos_rt::Error) -> i32 {
    eprintln!("{}: {}: {}", applet, what, e);
    1
}

/// Read a file (or stdin for "-") fully.
pub fn read_input(path: &str) -> rustos_rt::Result<Vec<u8>> {
    if path == "-" {
        let mut v = Vec::new();
        rustos_rt::io::stdin().read_to_end(&mut v)?;
        Ok(v)
    } else {
        fs::read(path)
    }
}

/// Human-readable size (1K, 2.3M, ...).
pub fn human(n: u64) -> String {
    const U: [&str; 5] = ["", "K", "M", "G", "T"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < 4 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{}", n)
    } else if v < 10.0 {
        let tenths = (v * 10.0) as u64;
        format!("{}.{}{}", tenths / 10, tenths % 10, U[i])
    } else {
        format!("{}{}", v as u64, U[i])
    }
}
