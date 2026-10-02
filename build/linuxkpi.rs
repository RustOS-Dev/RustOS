//! Kernel C build for LinuxKPI: compiles the Linux sources listed in
//! `src/linuxkpi/groups/<group>.list` (from `third_party/linux`, or from
//! `src/linuxkpi/c` for entries prefixed `kpi:`) with clang into one static
//! library per group, linked into the kernel as a whole archive so that
//! initcall sections are kept.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

/// Cargo feature → groups it compiles.
const FEATURES: &[(&str, &[&str])] = &[
    ("LINUXKPI", &["proof", "kpi", "base"]),
    ("LINUX_E1000", &["e1000"]),
    ("LINUX_TEST", &["testdev"]),
    ("LINUX_WIFI", &["crypto", "netlink", "cfg80211", "mac80211"]),
    ("LINUX_HWSIM", &["hwsim"]),
    ("LINUX_USB", &["usb"]),
    ("LINUX_USBNET", &["usbnet"]),
    ("LINUX_I2C", &["i2c"]),
    ("LINUX_SERIAL", &["tty", "usbserial"]),
    ("LINUX_MMC", &["mmc"]),
    ("LINUX_HID", &["input", "hid", "i2chid"]),
    ("LINUX_USBHID", &["usbhid"]),
    ("LINUX_PLATFORM", &["gpio", "i2cplat"]),
    ("LINUX_PHY", &["phy"]),
    ("LINUX_ETH", &["eth"]),
    ("LINUX_MT7921", &["mt7921", "mt7921u"]),
];

pub fn build(root: &Path, out: &Path) {
    let mut groups: Vec<&str> = Vec::new();
    for (feature, gs) in FEATURES {
        if std::env::var_os(format!("CARGO_FEATURE_{feature}")).is_some() {
            groups.extend(gs.iter().copied());
        }
    }
    if groups.is_empty() {
        return;
    }
    for p in [
        "src/linuxkpi/cflags.txt",
        "src/linuxkpi/groups",
        "src/linuxkpi/include",
        "src/linuxkpi/c",
        "third_party/linux",
    ] {
        println!("cargo:rerun-if-changed={p}");
    }
    println!("cargo:rerun-if-env-changed=RUSTOS_CLANG");
    // clippy does not link; skip the C build.
    if std::env::var_os("CLIPPY_ARGS").is_some() {
        return;
    }
    let clang = std::env::var("RUSTOS_CLANG").unwrap_or_else(|_| "clang".into());
    check_clang(&clang);
    let ar = llvm_ar();
    let libdir = out.join("linuxkpi");
    std::fs::create_dir_all(&libdir).unwrap();
    let flags = cflags(root);
    // Objects depend on the flags and on which override headers exist: a
    // new header in src/linuxkpi/include shadows a Linux one that the old
    // dependency files still name.
    let stamp = {
        let mut h = DefaultHasher::new();
        flags.hash(&mut h);
        let mut overrides = Vec::new();
        list_files(&root.join("src/linuxkpi/include"), &mut overrides);
        overrides.sort();
        overrides.hash(&mut h);
        format!("{:016x}", h.finish())
    };
    for g in groups {
        let objs = compile_group(root, &libdir, g, &clang, &flags, &stamp);
        let lib = libdir.join(format!("liblinuxkpi_{g}.a"));
        let _ = std::fs::remove_file(&lib);
        let st = Command::new(&ar).arg("rcs").arg(&lib).args(&objs).status();
        if !matches!(st, Ok(s) if s.success()) {
            panic!("{} failed to archive {}", ar.display(), lib.display());
        }
        println!("cargo:rustc-link-lib=static:+whole-archive=linuxkpi_{g}");
    }
    println!("cargo:rustc-link-search=native={}", libdir.display());
    // Keep sections referenced only through __start_/__stop_ (initcalls).
    println!("cargo:rustc-link-arg=-znostart-stop-gc");
}

fn list_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            list_files(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn check_clang(clang: &str) {
    let out = Command::new(clang).arg("--version").output();
    let text = match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => panic!(
            "LinuxKPI needs clang (15 or newer) to compile Linux drivers; install it or \
             set RUSTOS_CLANG, or build without the linux-* features"
        ),
    };
    let major = text
        .split("version ")
        .nth(1)
        .and_then(|v| v.split('.').next())
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0);
    if major < 15 {
        panic!(
            "LinuxKPI needs clang 15 or newer, found: {}",
            text.lines().next().unwrap_or("")
        );
    }
}

/// llvm-ar from the toolchain's llvm-tools component, else from PATH.
fn llvm_ar() -> PathBuf {
    if let Ok(p) = std::env::var("RUSTOS_LLVM_AR") {
        return p.into();
    }
    llvm_tool("llvm-ar")
}

/// Linux initcall section names, renamed to C identifiers so the linker
/// defines __start_/__stop_ symbols for them (src/linuxkpi/c/initcalls.c).
const INITCALL_LEVELS: &[&str] = &[
    "early", "0", "0s", "1", "1s", "2", "2s", "3", "3s", "4", "4s", "5", "5s", "rootfs", "6", "6s",
    "7", "7s",
];

/// An LLVM tool from the toolchain's llvm-tools component, else from PATH.
fn llvm_tool(name: &str) -> PathBuf {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let host = std::env::var("HOST").unwrap_or_else(|_| "x86_64-unknown-linux-gnu".into());
    if let Ok(o) = Command::new(rustc).args(["--print", "sysroot"]).output() {
        let sysroot = String::from_utf8_lossy(&o.stdout).trim().to_string();
        let p = Path::new(&sysroot)
            .join("lib/rustlib")
            .join(host)
            .join("bin")
            .join(name);
        if p.exists() {
            return p;
        }
    }
    name.into()
}

fn cflags(root: &Path) -> Vec<String> {
    let linux = root.join("third_party/linux");
    let kpi = root.join("src/linuxkpi");
    let text = std::fs::read_to_string(kpi.join("cflags.txt")).expect("src/linuxkpi/cflags.txt");
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            l.replace("@LINUX@", linux.to_str().unwrap())
                .replace("@KPI@", kpi.to_str().unwrap())
        })
        .collect()
}

/// One source of a group: its path, the module it belongs to
/// (KBUILD_MODNAME) and extra compiler flags.
struct Source {
    path: PathBuf,
    module: String,
    flags: Vec<String>,
}

/// The sources of `group`. Besides file paths, a list may hold
/// `module: NAME` (KBUILD_MODNAME of the files that follow; default: the
/// group name) and `cflags: FLAGS` (extra flags for the files that follow,
/// like a Makefile's ccflags-y; `@LINUX@` is the imported tree). A
/// `module:` line clears the flags.
fn group_sources(root: &Path, group: &str) -> Vec<Source> {
    let list = root
        .join("src/linuxkpi/groups")
        .join(format!("{group}.list"));
    let text = std::fs::read_to_string(&list).unwrap_or_else(|e| panic!("{}: {e}", list.display()));
    let linux = root.join("third_party/linux");
    let mut module = group.to_string();
    let mut flags: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for l in text.lines().map(|l| l.split('#').next().unwrap().trim()) {
        if l.is_empty() {
            continue;
        }
        if let Some(m) = l.strip_prefix("module:") {
            module = m.trim().to_string();
            flags.clear();
            continue;
        }
        if let Some(f) = l.strip_prefix("cflags:") {
            flags.extend(
                f.split_whitespace()
                    .map(|x| x.replace("@LINUX@", linux.to_str().unwrap())),
            );
            continue;
        }
        let path = match l.strip_prefix("kpi:") {
            Some(rest) => root.join("src/linuxkpi/c").join(rest),
            None => linux.join(l),
        };
        out.push(Source {
            path,
            module: module.clone(),
            flags: flags.clone(),
        });
    }
    out
}

/// True if `obj` is newer than every dependency in its `.d` file and was
/// built with the same flags.
fn up_to_date(obj: &Path, stamp: &str) -> bool {
    let (Ok(meta), Ok(d), Ok(s)) = (
        std::fs::metadata(obj),
        std::fs::read_to_string(obj.with_extension("d")),
        std::fs::read_to_string(obj.with_extension("stamp")),
    ) else {
        return false;
    };
    if s != stamp {
        return false;
    }
    let built = meta.modified().unwrap();
    let deps = d.replace("\\\n", " ");
    let Some((_, list)) = deps.split_once(':') else {
        return false;
    };
    list.split_whitespace().all(|p| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .map(|t| t <= built)
            .unwrap_or(false)
    })
}

fn compile_group(
    root: &Path,
    libdir: &Path,
    group: &str,
    clang: &str,
    flags: &[String],
    stamp: &str,
) -> Vec<PathBuf> {
    let srcs = group_sources(root, group);
    let objdir = libdir.join(group);
    std::fs::create_dir_all(&objdir).unwrap();
    let jobs: Vec<(Source, PathBuf)> = srcs
        .into_iter()
        .map(|src| {
            let rel = src.path.strip_prefix(root).unwrap_or(&src.path);
            let name = rel.to_string_lossy().replace(['/', '\\'], "__");
            let obj = objdir.join(format!("{}.o", name.trim_end_matches(".c")));
            (src, obj)
        })
        .collect();
    let objs: Vec<PathBuf> = jobs.iter().map(|(_, o)| o.clone()).collect();
    let objcopy = llvm_tool("llvm-objcopy");
    let queue = Mutex::new(jobs);
    let errors = Mutex::new(Vec::<String>::new());
    let n = std::env::var("NUM_JOBS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4usize)
        .max(1);
    std::thread::scope(|s| {
        for _ in 0..n {
            s.spawn(|| {
                loop {
                    let Some((src, obj)) = queue.lock().unwrap().pop() else {
                        break;
                    };
                    // The object depends on its own module name and flags too.
                    let stamp = format!("{stamp} {} {}", src.module, src.flags.join(" "));
                    if up_to_date(&obj, &stamp) {
                        continue;
                    }
                    let module = &src.module;
                    let src_flags = &src.flags;
                    let src = &src.path;
                    let base = src.file_stem().unwrap().to_string_lossy().replace('-', "_");
                    let out = Command::new(clang)
                        .args(flags)
                        .args(src_flags)
                        .arg(format!("-DKBUILD_MODNAME=\"{module}\""))
                        .arg(format!("-DKBUILD_BASENAME=\"{base}\""))
                        .arg(format!("-DKBUILD_MODFILE=\"{module}\""))
                        .arg("-MD")
                        .arg("-MF")
                        .arg(obj.with_extension("d"))
                        .arg("-c")
                        .arg(src)
                        .arg("-o")
                        .arg(&obj)
                        .output();
                    match out {
                        Ok(o) if o.status.success() => {
                            let mut oc = Command::new(&objcopy);
                            for l in INITCALL_LEVELS {
                                oc.arg(format!(
                                    "--rename-section=.initcall{l}.init=kpi_initcall_{l}"
                                ));
                            }
                            if !matches!(oc.arg(&obj).status(), Ok(s) if s.success()) {
                                errors
                                    .lock()
                                    .unwrap()
                                    .push(format!("llvm-objcopy failed on {}", obj.display()));
                                continue;
                            }
                            std::fs::write(obj.with_extension("stamp"), &stamp).unwrap();
                        }
                        Ok(o) => {
                            let err = String::from_utf8_lossy(&o.stderr);
                            let tail: String = err.chars().rev().take(6000).collect();
                            errors.lock().unwrap().push(format!(
                                "{}:\n{}",
                                src.display(),
                                tail.chars().rev().collect::<String>()
                            ));
                        }
                        Err(e) => errors.lock().unwrap().push(format!("{clang}: {e}")),
                    }
                }
            });
        }
    });
    let errors = errors.into_inner().unwrap();
    if !errors.is_empty() {
        panic!(
            "LinuxKPI group `{group}` failed to compile:\n{}",
            errors.join("\n")
        );
    }
    objs
}
