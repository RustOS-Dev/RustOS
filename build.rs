//! Builds the userland (userland/) and packs it into the initramfs that the
//! kernel embeds.

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "build/linuxkpi.rs"]
mod linuxkpi;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let userland = manifest_dir.join("userland");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=userland");
    println!("cargo:rerun-if-changed=tools/build-musl.sh");
    // Library crates the userland programs are built from.
    for c in [
        "rustos-rt",
        "weburl",
        "http",
        "nettls",
        "html",
        "css",
        "layout",
        "jsproto",
    ] {
        println!("cargo:rerun-if-changed=crates/{c}/src");
    }
    println!("cargo:rerun-if-env-changed=RUSTOS_SKIP_USERLAND");
    println!("cargo:rerun-if-changed=build");
    linuxkpi::build(&manifest_dir, &out_dir);

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
    // Ports staged with `tools/install-port.sh --initramfs` (under /usr/local).
    let ports = manifest_dir.join("target/ports-root");
    println!("cargo:rerun-if-changed={}", ports.display());
    let before = files.len();
    add_tree(&ports, "", &mut files);
    for (name, e) in files[before..].iter_mut() {
        if name.contains("/bin/")
            && let Entry::File(_, mode) = e
        {
            *mode = 0o755;
        }
    }
    // Ports listed in ports/default.list (busybox, curl, QuickJS), built
    // against musl on first use and installed under /usr/bin.
    println!("cargo:rerun-if-env-changed=RUSTOS_PORTS");
    println!("cargo:rerun-if-changed=ports");
    if !skip && std::env::var("RUSTOS_PORTS").as_deref() != Ok("0") {
        add_default_ports(&manifest_dir, &mut files);
        add_jsd(&manifest_dir, &mut files);
        add_fonts(&mut files);
    }
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

/// Build (if needed) and add the ports named in ports/default.list. A port
/// that cannot be built (no network for its sources, no C compiler) is
/// left out with a warning.
fn add_default_ports(root: &Path, files: &mut Vec<(String, Entry)>) {
    let Ok(list) = std::fs::read_to_string(root.join("ports/default.list")) else {
        return;
    };
    for name in list
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let script = root.join("ports").join(name).join("build.sh");
        let staged = root.join("target/ports").join(name);
        let stamp = staged.join(".built-from");
        let recipe = std::fs::read(&script).unwrap_or_default();
        if std::fs::read(&stamp).ok().as_deref() != Some(&recipe[..]) {
            let ok = Command::new("sh")
                .arg(root.join("tools/install-port.sh"))
                .arg(name)
                .status()
                .is_ok_and(|s| s.success());
            if !ok {
                println!("cargo:warning=port {name} could not be built; not installed");
                continue;
            }
            let _ = std::fs::write(&stamp, &recipe);
        }
        for dir in ["usr", "usr/bin"] {
            if !files.iter().any(|(n, _)| n == dir) {
                files.push((dir.into(), Entry::Dir));
            }
        }
        let before = files.len();
        add_tree(&staged.join("bin"), "usr/bin/", files);
        for (_, e) in files[before..].iter_mut() {
            if let Entry::File(_, mode) = e {
                *mode = 0o755;
            }
        }
    }
}

/// DejaVu fonts for the graphical browser (from the host's
/// fonts-dejavu-core, or RUSTOS_FONTS_DIR), with their license.
fn add_fonts(files: &mut Vec<(String, Entry)>) {
    println!("cargo:rerun-if-env-changed=RUSTOS_FONTS_DIR");
    let dir = std::env::var("RUSTOS_FONTS_DIR")
        .unwrap_or_else(|_| "/usr/share/fonts/truetype/dejavu".into());
    let names = [
        "DejaVuSans.ttf",
        "DejaVuSans-Bold.ttf",
        "DejaVuSerif.ttf",
        "DejaVuSerif-Bold.ttf",
        "DejaVuSansMono.ttf",
        "DejaVuSansMono-Bold.ttf",
    ];
    let mut found = Vec::new();
    for n in names {
        if let Ok(d) = std::fs::read(Path::new(&dir).join(n)) {
            found.push((n, d));
        }
    }
    if found.is_empty() {
        println!("cargo:warning=DejaVu fonts not found in {dir}: browse -g has no fonts");
        return;
    }
    for d in ["usr/share", "usr/share/fonts", "usr/share/fonts/dejavu"] {
        if !files.iter().any(|(n, _)| n == d) {
            files.push((d.into(), Entry::Dir));
        }
    }
    for (n, d) in found {
        files.push((format!("usr/share/fonts/dejavu/{n}"), Entry::File(d, 0o644)));
    }
    if let Ok(l) = std::fs::read("/usr/share/doc/fonts-dejavu-core/copyright") {
        files.push((
            "usr/share/fonts/dejavu/LICENSE".into(),
            Entry::File(l, 0o644),
        ));
    }
}

/// The browser's JavaScript helper (userland/jsd): C plus the web platform
/// library in lib/*.js, linked against the QuickJS port. Left out (the
/// browser then runs without JavaScript) when QuickJS is not built.
fn add_jsd(root: &Path, files: &mut Vec<(String, Entry)>) {
    let qjs = root.join("target/ports/quickjs");
    let lib = qjs.join("lib/libquickjs.a");
    if !lib.exists() {
        println!("cargo:warning=QuickJS port not built: jsd not installed");
        return;
    }
    let src = root.join("userland/jsd");
    let out = root.join("target/jsd");
    let _ = std::fs::create_dir_all(&out);
    // The library, embedded as a C array (files in name order).
    let mut names: Vec<PathBuf> = std::fs::read_dir(src.join("lib"))
        .map(|d| d.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default();
    names.retain(|p| p.extension().is_some_and(|e| e == "js"));
    names.sort();
    let mut js = Vec::new();
    for n in &names {
        js.extend(std::fs::read(n).unwrap_or_default());
        js.push(b'\n');
    }
    let mut h = String::from("static const char jsd_lib[] = {\n");
    for chunk in js.chunks(24) {
        for b in chunk {
            h.push_str(&b.to_string());
            h.push(',');
        }
        h.push('\n');
    }
    h.push_str("0};\n");
    let _ = std::fs::write(out.join("jsd_lib.h"), h);
    let status = Command::new("sh")
        .arg(root.join("tools/rustos-cc"))
        .args(["-O2", "-static", "-D_GNU_SOURCE"])
        .arg(format!("-I{}", qjs.join("include/quickjs").display()))
        .arg(format!("-I{}", out.display()))
        .arg("-o")
        .arg(out.join("jsd"))
        .arg(src.join("jsd.c"))
        .arg(&lib)
        .arg("-lm")
        .status();
    if !matches!(status, Ok(s) if s.success()) {
        println!("cargo:warning=jsd failed to build; not installed");
        return;
    }
    if let Ok(data) = std::fs::read(out.join("jsd")) {
        files.push(("usr/libexec".into(), Entry::Dir));
        files.push(("usr/libexec/jsd".into(), Entry::File(data, 0o755)));
    }
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
            // No SSE/AVX in userland: portable backends for every crypto crate.
            "-C relocation-model=static -C link-arg=-T{} -C link-arg=--no-pie --cfg aes_force_soft --cfg polyval_force_soft --cfg chacha20_force_soft --cfg poly1305_force_soft --cfg curve25519_dalek_backend=\"serial\"",
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
    out.push(("lib".into(), Entry::Dir));
    out.push((
        "lib/ld-rustos.so.1".into(),
        Entry::File(build_ldso(userland, root)?, 0o755),
    ));
    out.extend(build_dyntest(userland, root));
    out.extend(build_musl_tests(userland, root));
    Ok(out)
}

/// musl (third_party/musl) and C test programs built against it; skipped
/// with a warning when musl or a host C compiler is unavailable.
fn build_musl_tests(userland: &Path, root: &Path) -> Vec<(String, Entry)> {
    let script = root.join("tools/build-musl.sh");
    match Command::new("sh").arg(&script).status() {
        Ok(st) if st.success() => {}
        _ => {
            println!("cargo:warning=musl not built: musl test programs not installed");
            return Vec::new();
        }
    }
    let cc = root.join("tools/rustos-cc");
    let src = userland.join("musltest");
    let out = root.join("target").join("musltest");
    let _ = std::fs::create_dir_all(&out);
    let steps: [(&[&str], &str, &str); 7] = [
        (&["-O2"], "musl-hello", "hello.c"),
        (&["-O2", "-static"], "musl-hello-static", "hello.c"),
        (&["-O2", "-pthread"], "musl-threads", "threads.c"),
        (&["-O2", "-fPIC", "-shared"], "libplugin.so", "plugin.c"),
        (&["-O2"], "musl-dlopen", "dlopen.c"),
        (&["-O2"], "musl-libctest", "libctest.c"),
        (&["-O2"], "musl-kpitest", "kpitest.c"),
    ];
    for (flags, name, file) in steps {
        let status = Command::new("sh")
            .arg(&cc)
            .args(flags)
            .arg("-o")
            .arg(out.join(name))
            .arg(src.join(file))
            .args(if file == "libctest.c" {
                &["-lm"][..]
            } else {
                &[][..]
            })
            .status();
        if !matches!(status, Ok(st) if st.success()) {
            println!("cargo:warning=musl test {name} failed to build");
            return Vec::new();
        }
    }
    let mut v = vec![("usr/lib".to_string(), Entry::Dir)];
    if let Ok(d) = std::fs::read(root.join("target/sysroot/usr/lib/libc.so")) {
        v.push(("lib/ld-musl-x86_64.so.1".into(), Entry::File(d, 0o755)));
        v.push((
            "usr/lib/libc.so".into(),
            Entry::Symlink("/lib/ld-musl-x86_64.so.1".into()),
        ));
    }
    for (name, dst, mode) in [
        ("musl-hello", "bin/musl-hello", 0o755),
        ("musl-hello-static", "bin/musl-hello-static", 0o755),
        ("musl-threads", "bin/musl-threads", 0o755),
        ("musl-dlopen", "bin/musl-dlopen", 0o755),
        ("musl-libctest", "bin/musl-libctest", 0o755),
        ("musl-kpitest", "bin/musl-kpitest", 0o755),
        ("libplugin.so", "usr/lib/libplugin.so", 0o644),
    ] {
        if let Ok(d) = std::fs::read(out.join(name)) {
            v.push((dst.to_string(), Entry::File(d, mode)));
        }
    }
    v
}

/// The dynamic linker: a static PIE built in its own workspace.
fn build_ldso(userland: &Path, root: &Path) -> Result<Vec<u8>, String> {
    let target_dir = root.join("target").join("ldso");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.current_dir(userland.join("ldso"))
        .args(["build", "--release", "--target", "x86_64-unknown-none"])
        .arg("--target-dir")
        .arg(&target_dir);
    for (k, _) in std::env::vars() {
        if k.starts_with("CARGO_") && k != "CARGO_HOME"
            || k == "RUSTFLAGS"
            || k == "RUSTC_WRAPPER"
            || k == "RUSTC_WORKSPACE_WRAPPER"
        {
            cmd.env_remove(&k);
        }
    }
    let status = cmd.status().map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("ld.so build: cargo exited with {status}"));
    }
    std::fs::read(target_dir.join("x86_64-unknown-none/release/ldso")).map_err(|e| e.to_string())
}

/// libc-free C programs exercising the dynamic linker (skipped without a
/// host C compiler).
fn build_dyntest(userland: &Path, root: &Path) -> Vec<(String, Entry)> {
    let src = userland.join("dyntest");
    let out = root.join("target").join("dyntest");
    let _ = std::fs::create_dir_all(&out);
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
    let common = [
        "-O2",
        "-nostdlib",
        "-fno-stack-protector",
        "-fno-builtin",
        "-ffreestanding",
        "-mno-sse",
        "-mno-red-zone",
    ];
    let pie: &[&str] = &["-fPIE", "-pie", "-Wl,--dynamic-linker=/lib/ld-rustos.so.1"];
    let steps: [(&[&str], &str, &str, &[&str]); 7] = [
        (
            &["-fPIC", "-shared", "-Wl,-soname,libgreet.so"],
            "libgreet.so",
            "greet.c",
            &[],
        ),
        (
            &["-fPIC", "-shared", "-Wl,-soname,libtlsdemo.so"],
            "libtlsdemo.so",
            "tlslib.c",
            &[],
        ),
        (
            &["-fPIC", "-shared", "-Wl,-soname,libdl.so"],
            "libdl.so",
            "dlstub.c",
            &[],
        ),
        (pie, "dyntest", "main.c", &["-lgreet"]),
        (
            &[
                "-fno-pie",
                "-no-pie",
                "-Wl,--dynamic-linker=/lib/ld-rustos.so.1",
            ],
            "dyntest-nopie",
            "main.c",
            &["-lgreet"],
        ),
        (
            pie,
            "tlstest",
            "tlsmain.c",
            &["-ltlsdemo", "-Wl,--allow-shlib-undefined"],
        ),
        (pie, "dltest", "dltest.c", &["-ldl"]),
    ];
    for (flags, name, file, libs) in steps {
        let mut cmd = Command::new(&cc);
        cmd.args(common)
            .args(flags)
            .arg("-o")
            .arg(out.join(name))
            .arg(src.join(file));
        if !libs.is_empty() {
            cmd.arg("-L").arg(&out).args(libs);
        }
        match cmd.status() {
            Ok(st) if st.success() => {}
            _ => {
                println!("cargo:warning=no host C compiler: dynamic linking tests not installed");
                return Vec::new();
            }
        }
    }
    let mut v = Vec::new();
    for (name, dst, mode) in [
        ("libgreet.so", "lib/libgreet.so", 0o644),
        ("libtlsdemo.so", "lib/libtlsdemo.so", 0o644),
        ("libdl.so", "lib/libdl.so", 0o644),
        ("dyntest", "bin/dyntest", 0o755),
        ("dyntest-nopie", "bin/dyntest-nopie", 0o755),
        ("tlstest", "bin/tlstest", 0o755),
        ("dltest", "bin/dltest", 0o755),
    ] {
        if let Ok(d) = std::fs::read(out.join(name)) {
            v.push((dst.to_string(), Entry::File(d, mode)));
        }
    }
    v
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
