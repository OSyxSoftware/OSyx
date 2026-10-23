//! Wall-clock time: the CMOS real-time clock is read once at boot and the
//! timer's uptime is added to it. QEMU's RTC runs in UTC by default.

use crate::arch::cpu;
use core::sync::atomic::{AtomicI64, Ordering};

static BOOT_EPOCH_NS: AtomicI64 = AtomicI64::new(0);

fn cmos(reg: u8) -> u8 {
    // SAFETY: ports 0x70/0x71 are the CMOS index/data pair; bit 7 of the
    // index keeps NMIs disabled while we read.
    unsafe {
        cpu::outb(0x70, 0x80 | reg);
        cpu::inb(0x71)
    }
}

/// Days from 1970-01-01 to the given civil date (proleptic Gregorian).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn read_rtc() -> [u8; 7] {
    // Wait until no update is in progress, then read twice until stable.
    let read = || {
        while cmos(0x0A) & 0x80 != 0 {
            core::hint::spin_loop();
        }
        [cmos(0x00), cmos(0x02), cmos(0x04), cmos(0x07), cmos(0x08), cmos(0x09), cmos(0x32)]
    };
    let mut a = read();
    loop {
        let b = read();
        if a == b {
            return a;
        }
        a = b;
    }
}

/// Reads the RTC; returns the time as seconds since the Unix epoch, or
/// `None` if the values are implausible.
pub fn init() -> Option<i64> {
    let [mut sec, mut min, mut hour, mut day, mut mon, mut year, mut century] = read_rtc();
    let status_b = cmos(0x0B);
    if status_b & 0x04 == 0 {
        let bcd = |v: u8| (v & 0x0F) + (v >> 4) * 10;
        sec = bcd(sec);
        min = bcd(min);
        hour = bcd(hour & 0x7F) | (hour & 0x80);
        day = bcd(day);
        mon = bcd(mon);
        year = bcd(year);
        century = bcd(century);
    }
    if status_b & 0x02 == 0 && hour & 0x80 != 0 {
        hour = ((hour & 0x7F) + 12) % 24;
    }
    let century = if (19..=30).contains(&century) { century as i64 } else { 20 };
    let year = century * 100 + year as i64;
    if !(1..=12).contains(&mon) || !(1..=31).contains(&day) || hour > 23 || min > 59 || sec > 60 || year < 2000 {
        return None;
    }
    let secs = days_from_civil(year, mon as i64, day as i64) * 86_400 + hour as i64 * 3600 + min as i64 * 60 + sec as i64;
    let uptime = crate::interrupts::uptime_ms() as i64;
    BOOT_EPOCH_NS.store(secs * 1_000_000_000 - uptime * 1_000_000, Ordering::Relaxed);
    Some(secs)
}

/// Nanoseconds since 1970-01-01 UTC (uptime only if the RTC was unusable).
pub fn now_ns() -> i64 {
    BOOT_EPOCH_NS.load(Ordering::Relaxed) + crate::interrupts::uptime_ms() as i64 * 1_000_000
}

/// `YYYY-MM-DD hh:mm:ss` of a Unix time, for log lines.
pub fn format_utc(secs: i64) -> alloc::string::String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Inverse of days_from_civil.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    alloc::format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}
