//! Builds the userland (userland/) and packs it into the initramfs that the
//! kernel embeds.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let userland = manifest_dir.join("userland");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=userland");
    println!("cargo:rerun-if-changed=crates/rustos-rt/src");
    println!("cargo:rerun-if-env-changed=RUSTOS_SKIP_USERLAND");

    let cpio = out_dir.join("initramfs.cpio");
    let skip = std::env::var_os("RUSTOS_SKIP_USERLAND").is_some()
        || std::env::var_os("CLIPPY_ARGS").is_some()
        || !userland.join("Cargo.toml").exists();
    let mut files: Vec<(String, Entry)> = Vec::new();
    if !skip {
        match build_userland(&userland, &manifest_dir) {
            Ok(entries) => files = entries,
            Err(e) => panic!("userland build failed: {e}"),
        }
    }
    add_tree(&userland.join("root"), "", &mut files);
    // Trust store for `wget https://`: the build host's CA bundle, or the
    // file named by RUSTOS_CA_BUNDLE (empty to leave it out).
    println!("cargo:rerun-if-env-changed=RUSTOS_CA_BUNDLE");
    let ca = std::env::var("RUSTOS_CA_BUNDLE")
        .unwrap_or_else(|_| "/etc/ssl/certs/ca-certificates.crt".into());
    if let Ok(data) = std::fs::read(&ca) {
        files.push(("etc/ssl".into(), Entry::Dir));
        files.push(("etc/ssl/certs".into(), Entry::Dir));
        files.push((
            "etc/ssl/certs/ca-certificates.crt".into(),
            Entry::File(data, 0o644),
        ));
    }
    std::fs::write(&cpio, make_cpio(&files)).expect("write initramfs");
}

enum Entry {
    Dir,
    File(Vec<u8>, u32),
    Symlink(String),
}

fn build_userland(userland: &Path, root: &Path) -> Result<Vec<(String, Entry)>, String> {
    let target_dir = root.join("target").join("userland");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.current_dir(userland)
        .args(["build", "--release", "--target", "x86_64-unknown-none"])
        .arg("--target-dir")
        .arg(&target_dir);
    // Do not leak the kernel build's configuration into the nested build.
    for (k, _) in std::env::vars() {
        if k.starts_with("CARGO_") && k != "CARGO_HOME"
            || k == "RUSTFLAGS"
            || k == "RUSTC_WRAPPER"
            || k == "RUSTC_WORKSPACE_WRAPPER"
            || k == "__CARGO_FIX_PLZ"
        {
            cmd.env_remove(&k);
        }
    }
    cmd.env(
        "RUSTFLAGS",
        format!(
            "-C relocation-model=static -C link-arg=-T{} -C link-arg=--no-pie --cfg aes_force_soft --cfg polyval_force_soft",
            userland.join("userland.ld").display()
        ),
    );
    let status = cmd.status().map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("cargo exited with {status}"));
    }
    let bin_dir = target_dir.join("x86_64-unknown-none").join("release");
    let manifest: Vec<(String, Vec<String>)> =
        std::fs::read_to_string(userland.join("install.list"))
            .map_err(|e| e.to_string())?
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
            .map(|l| {
                let mut parts = l.split_whitespace();
                let bin = parts.next().unwrap().to_string();
                (bin, parts.map(String::from).collect())
            })
            .collect();
    let mut out = Vec::new();
    for d in ["bin", "sbin", "etc", "usr", "usr/bin"] {
        out.push((d.to_string(), Entry::Dir));
    }
    for (bin, links) in manifest {
        let data = std::fs::read(bin_dir.join(&bin)).map_err(|e| format!("{bin}: {e}"))?;
        let mut targets = links.into_iter();
        let first = targets.next().unwrap_or_else(|| format!("bin/{bin}"));
        out.push((first.clone(), Entry::File(data, 0o755)));
        for l in targets {
            out.push((l, Entry::Symlink(format!("/{first}"))));
        }
    }
    Ok(out)
}

fn add_tree(dir: &Path, prefix: &str, out: &mut Vec<(String, Entry)>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = format!("{}{}", prefix, e.file_name().to_string_lossy());
        let p = e.path();
        if p.is_dir() {
            out.push((name.clone(), Entry::Dir));
            add_tree(&p, &format!("{name}/"), out);
        } else if let Ok(data) = std::fs::read(&p) {
            out.push((name, Entry::File(data, 0o644)));
        }
    }
}

fn make_cpio(files: &[(String, Entry)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut ino = 1;
    let mut push = |name: &str, mode: u32, data: &[u8]| {
        let hdr = format!(
            "070701{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}",
            ino,
            mode,
            0,
            0,
            1,
            0,
            data.len(),
            0,
            0,
            0,
            0,
            name.len() + 1,
            0
        );
        ino += 1;
        out.extend_from_slice(hdr.as_bytes());
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        while out.len() % 4 != 0 {
            out.push(0);
        }
        out.extend_from_slice(data);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    };
    for (name, e) in files {
        match e {
            Entry::Dir => push(name, 0o040755, &[]),
            Entry::File(d, m) => push(name, 0o100000 | m, d),
            Entry::Symlink(t) => push(name, 0o120777, t.as_bytes()),
        }
    }
    push("TRAILER!!!", 0, &[]);
    out
}
