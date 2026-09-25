//! CMOS real-time clock.

use x86_64::instructions::port::Port;

fn cmos_read(reg: u8) -> u8 {
    unsafe {
        Port::<u8>::new(0x70).write(reg | 0x80); // keep NMI disabled while selecting
        Port::<u8>::new(0x71).read()
    }
}

fn update_in_progress() -> bool {
    cmos_read(0x0A) & 0x80 != 0
}

fn raw() -> [u8; 7] {
    let century_reg = super::acpi::platform().century_reg;
    [
        cmos_read(0x00),
        cmos_read(0x02),
        cmos_read(0x04),
        cmos_read(0x07),
        cmos_read(0x08),
        cmos_read(0x09),
        if century_reg != 0 {
            cmos_read(century_reg)
        } else {
            0
        },
    ]
}

/// Read the RTC and convert it to seconds since the Unix epoch (the RTC is
/// assumed to hold UTC).
pub fn read_unix_time() -> u64 {
    // Read twice until two consecutive reads agree and no update is running.
    let mut last;
    let mut cur = [0u8; 7];
    for _ in 0..1_000_000 {
        if !update_in_progress() {
            break;
        }
    }
    cur.copy_from_slice(&raw());
    loop {
        last = cur;
        for _ in 0..1_000_000 {
            if !update_in_progress() {
                break;
            }
        }
        cur = raw();
        if cur == last {
            break;
        }
    }
    let status_b = cmos_read(0x0B);
    let bcd = status_b & 0x04 == 0;
    let conv = |v: u8| if bcd { (v & 0x0F) + (v >> 4) * 10 } else { v };
    let sec = conv(cur[0]) as u32;
    let min = conv(cur[1]) as u32;
    let mut hour_raw = cur[2];
    let pm = hour_raw & 0x80 != 0;
    hour_raw &= 0x7F;
    let mut hour = conv(hour_raw) as u32;
    if status_b & 0x02 == 0 {
        // 12-hour mode
        hour %= 12;
        if pm {
            hour += 12;
        }
    }
    let day = conv(cur[3]) as u32;
    let month = conv(cur[4]) as u32;
    let mut year = conv(cur[5]) as u32;
    year += if cur[6] != 0 {
        conv(cur[6]) as u32 * 100
    } else if year < 70 {
        2000
    } else {
        1900
    };
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return 0;
    }
    crate::time::unix_from_civil(year, month, day, hour, min, sec)
}
