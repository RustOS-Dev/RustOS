//! aml-check DIR: load DIR/DSDT and DIR/SSDT* (as copied from
//! /sys/firmware/acpi/tables) into RustOS's AML interpreter, initialize the
//! namespace, and evaluate _HID, _ADR, _STA and _CRS of every device.
//! Exits 1 if a table does not load or a method fails.
//!
//!   sudo cp -r /sys/firmware/acpi/tables /tmp/acpi && sudo chmod -R a+r /tmp/acpi
//!   cargo run --release -- /tmp/acpi

use aml_check::{Machine, devices, eval, interpreter, load};

fn main() {
    let Some(dir) = std::env::args().nth(1) else {
        eprintln!("usage: aml-check DIR (a copy of /sys/firmware/acpi/tables)");
        std::process::exit(2);
    };
    let read = |name: &str| std::fs::read(format!("{dir}/{name}")).ok();
    let Some(dsdt) = read("DSDT") else {
        eprintln!("aml-check: no {dir}/DSDT");
        std::process::exit(2);
    };
    let mut ssdts: Vec<u32> = std::fs::read_dir(&dir)
        .map(|d| {
            d.filter_map(|e| {
                e.ok()?
                    .file_name()
                    .to_str()?
                    .strip_prefix("SSDT")?
                    .parse()
                    .ok()
            })
            .collect()
        })
        .unwrap_or_default();
    ssdts.sort();
    let machine = Machine::new();
    let interp = interpreter(&machine, read("FACP").as_deref(), dsdt[8]);
    let mut failed = 0;
    let tables = std::iter::once(("DSDT".to_string(), dsdt)).chain(
        ssdts
            .iter()
            .filter_map(|n| Some((format!("SSDT{n}"), read(&format!("SSDT{n}"))?))),
    );
    for (name, data) in tables {
        let id: String = data[16..24]
            .iter()
            .filter(|b| b.is_ascii_graphic())
            .map(|&b| b as char)
            .collect();
        match load(&interp, &data) {
            Ok(()) => println!("{name:7} {id:8} {:7} bytes  loaded", data.len()),
            Err(e) => {
                failed += 1;
                println!(
                    "{name:7} {id:8} {:7} bytes  NOT FULLY LOADED: {e:?}",
                    data.len()
                );
            }
        }
    }
    interp.initialize_namespace();
    let devs = devices(&interp);
    let mut ok = 0;
    for d in &devs {
        for m in ["_HID", "_ADR", "_STA", "_CRS"] {
            let path = format!("{d}.{m}");
            match eval(&interp, &path) {
                Ok(_) => ok += 1,
                Err(e) => {
                    let msg = format!("{e:?}");
                    if msg.contains("DoesNotExist") {
                        continue;
                    }
                    failed += 1;
                    println!("{path}: {msg}");
                }
            }
        }
    }
    println!(
        "{} devices: {ok} methods evaluated, {failed} failures",
        devs.len()
    );
    std::process::exit(if failed == 0 { 0 } else { 1 });
}
