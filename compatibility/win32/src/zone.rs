//! Windows time-zone and calendar-time calls over the OSyx system zone
//! (`tz::system`, a POSIX rule from /etc/TZ). TIME_ZONE_INFORMATION is the
//! same rule in Windows form: biases in minutes west of UTC and transition
//! dates as "day d of week w of month m" SYSTEMTIMEs. OSyx has no Windows
//! time-zone database, so the names are the zone's abbreviations (the
//! dynamic key name is the standard abbreviation too) and every year uses
//! the same rule.
#![allow(non_snake_case)]
use crate::{
    error::*,
    kernel32::fail,
    sysext, teb, tz,
};

const TIME_ZONE_ID_UNKNOWN: u32 = 0;
const TIME_ZONE_ID_STANDARD: u32 = 1;
const TIME_ZONE_ID_DAYLIGHT: u32 = 2;
const TZI_SIZE: usize = 172;

/// A TIME_ZONE_INFORMATION in its fields.
struct Tzi {
    bias: i32,
    std_name: alloc::string::String,
    std_date: [u16; 8],
    std_bias: i32,
    dst_name: alloc::string::String,
    dst_date: [u16; 8],
    dst_bias: i32,
}

/// A POSIX rule as the SYSTEMTIME of a transition: year 0, the month, the
/// weekday and its week (5 = last), the local time of day.
fn rule_date(r: &osyx_ui::time::Rule) -> [u16; 8] {
    let t = r.time.rem_euclid(86_400);
    [0, r.month as u16, r.day as u16, r.week as u16, (t / 3600) as u16, (t / 60 % 60) as u16, (t % 60) as u16, 0]
}

fn current() -> Tzi {
    let z = tz::system();
    let bias = (-z.std_offset / 60) as i32;
    match &z.dst {
        Some((name, off, start, end)) => Tzi {
            bias,
            std_name: z.std_name.clone(),
            std_date: rule_date(end),
            std_bias: 0,
            dst_name: name.clone(),
            dst_date: rule_date(start),
            dst_bias: (-(off - z.std_offset) / 60) as i32,
        },
        None => Tzi {
            bias,
            std_name: z.std_name.clone(),
            std_date: [0; 8],
            std_bias: 0,
            dst_name: alloc::string::String::new(),
            dst_date: [0; 8],
            dst_bias: 0,
        },
    }
}

/// Writes up to 31 UTF-16 units and a NUL at `p` (a WCHAR[32]).
///
/// # Safety
/// `p` must be writable for 64 bytes.
unsafe fn put_name(p: u64, s: &str) {
    for (i, c) in s.encode_utf16().take(31).chain(core::iter::repeat(0)).take(32).enumerate() {
        // SAFETY: within the caller's 32 units.
        unsafe { teb::put(p, i * 2, c) };
    }
}

/// # Safety
/// `out` must be a writable TIME_ZONE_INFORMATION (172 bytes).
unsafe fn write_tzi(out: u64, t: &Tzi) {
    // SAFETY: the caller's structure.
    unsafe {
        core::ptr::write_bytes(out as *mut u8, 0, TZI_SIZE);
        teb::put(out, 0, t.bias);
        put_name(out + 4, &t.std_name);
        teb::put(out, 68, t.std_date);
        teb::put(out, 84, t.std_bias);
        put_name(out + 88, &t.dst_name);
        teb::put(out, 152, t.dst_date);
        teb::put(out, 168, t.dst_bias);
    }
}

/// # Safety
/// `p` must be a readable TIME_ZONE_INFORMATION.
unsafe fn read_tzi(p: u64) -> Tzi {
    // SAFETY: the caller's structure.
    unsafe {
        Tzi {
            bias: teb::get(p, 0),
            std_name: alloc::string::String::new(),
            std_date: teb::get(p, 68),
            std_bias: teb::get(p, 84),
            dst_name: alloc::string::String::new(),
            dst_date: teb::get(p, 152),
            dst_bias: teb::get(p, 168),
        }
    }
}

fn zone_id(t: &Tzi) -> u32 {
    if t.dst_date[1] == 0 {
        return TIME_ZONE_ID_UNKNOWN;
    }
    let now = (sysext::now_filetime() / 10_000_000) as i64 - 11_644_473_600;
    if in_daylight(t, now) {
        TIME_ZONE_ID_DAYLIGHT
    } else {
        TIME_ZONE_ID_STANDARD
    }
}

fn month_days(y: i64, m: u32) -> u32 {
    match m {
        4 | 6 | 9 | 11 => 30,
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        _ => 31,
    }
}

/// Local seconds since 1970 at which transition `d` happens in `year`.
fn transition(d: &[u16; 8], year: i64) -> i64 {
    let r = osyx_ui::time::Rule { month: d[1] as u32, week: d[3] as u32, day: d[2] as u32, time: 0 };
    let first = osyx_ui::time::days_from_civil(year, r.month, 1);
    let first_wd = osyx_ui::time::weekday(first);
    let mut day = (r.day + 7 - first_wd) % 7 + 1 + 7 * (r.week.max(1) - 1);
    let dim = month_days(year, r.month);
    while day > dim {
        day -= 7;
    }
    (first + day as i64 - 1) * 86_400 + d[4] as i64 * 3600 + d[5] as i64 * 60 + d[6] as i64
}

/// Whether UTC instant `utc` falls in daylight time under `t`.
fn in_daylight(t: &Tzi, utc: i64) -> bool {
    if t.dst_date[1] == 0 {
        return false;
    }
    let std_off = -(t.bias as i64 + t.std_bias as i64) * 60;
    let dst_off = -(t.bias as i64 + t.dst_bias as i64) * 60;
    let (year, _, _) = osyx_ui::time::civil_from_days((utc + std_off).div_euclid(86_400));
    // The start is given in standard local time, the end in daylight time.
    let s = transition(&t.dst_date, year) - std_off;
    let e = transition(&t.std_date, year) - dst_off;
    if s < e {
        utc >= s && utc < e
    } else {
        !(utc >= e && utc < s)
    }
}

extern "win64" fn GetTimeZoneInformation(out: u64) -> u32 {
    if out == 0 {
        return fail(ERROR_INVALID_PARAMETER, u32::MAX);
    }
    let t = current();
    // SAFETY: the caller's TIME_ZONE_INFORMATION.
    unsafe { write_tzi(out, &t) };
    zone_id(&t)
}

/// DYNAMIC_TIME_ZONE_INFORMATION: the 172-byte TIME_ZONE_INFORMATION, the
/// key name (WCHAR[128]) and DynamicDaylightTimeDisabled.
extern "win64" fn GetDynamicTimeZoneInformation(out: u64) -> u32 {
    if out == 0 {
        return fail(ERROR_INVALID_PARAMETER, u32::MAX);
    }
    let t = current();
    // SAFETY: the caller's 432-byte structure.
    unsafe {
        core::ptr::write_bytes(out as *mut u8, 0, 432);
        write_tzi(out, &t);
        for (i, c) in t.std_name.encode_utf16().take(127).enumerate() {
            teb::put(out + TZI_SIZE as u64, i * 2, c);
        }
    }
    zone_id(&t)
}

extern "win64" fn GetTimeZoneInformationForYear(year: u16, dynamic: u64, out: u64) -> i32 {
    if out == 0 || year == 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    if dynamic != 0 {
        // Only the zone in effect is known (no Windows zone database).
        // SAFETY: the caller's DYNAMIC_TIME_ZONE_INFORMATION key name.
        let key = unsafe { crate::path::wide_to_string((dynamic + TZI_SIZE as u64) as *const u16) }.unwrap_or_default();
        if !key.is_empty() && key != current().std_name {
            return fail(ERROR_FILE_NOT_FOUND, 0);
        }
    }
    // SAFETY: the caller's TIME_ZONE_INFORMATION.
    unsafe { write_tzi(out, &current()) };
    1
}

/// UTC SYSTEMTIME → local SYSTEMTIME under `tzi` (NULL: the current zone).
extern "win64" fn SystemTimeToTzSpecificLocalTime(tzi: u64, utc: *const [u16; 8], local: *mut [u16; 8]) -> i32 {
    if utc.is_null() || local.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let mut ft = 0u64;
    if sysext::SystemTimeToFileTime(utc, &mut ft) == 0 {
        return 0;
    }
    // SAFETY: NULL or the caller's TIME_ZONE_INFORMATION.
    let t = if tzi == 0 { current() } else { unsafe { read_tzi(tzi) } };
    let secs = (ft / 10_000_000) as i64 - 11_644_473_600;
    let extra = if in_daylight(&t, secs) { t.dst_bias } else { t.std_bias };
    let shift = -((t.bias + extra) as i64) * 60 * 10_000_000;
    let Some(lft) = (ft as i64).checked_add(shift).filter(|v| *v >= 0) else { return fail(ERROR_INVALID_PARAMETER, 0) };
    // SAFETY: the caller's SYSTEMTIME.
    unsafe { *local = sysext::system_time(lft as u64) };
    1
}

/// The local time now, in the system zone.
pub fn local_now(out: &mut [u16; 8]) {
    let ft = sysext::now_filetime();
    let secs = (ft / 10_000_000) as i64 - 11_644_473_600;
    let off = tz::system().offset_at(secs).0;
    *out = sysext::system_time((ft as i64 + off * 10_000_000) as u64);
}

extern "win64" fn GetLocalTime(out: *mut [u16; 8]) {
    if !out.is_null() {
        // SAFETY: the caller's SYSTEMTIME.
        local_now(unsafe { &mut *out });
    }
}

extern "win64" fn FileTimeToSystemTime(ft: *const u64, out: *mut [u16; 8]) -> i32 {
    if ft.is_null() || out.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: the caller's FILETIME (4-byte aligned on Windows).
    let v = unsafe { core::ptr::read_unaligned(ft) };
    if v > i64::MAX as u64 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: the caller's SYSTEMTIME.
    unsafe { *out = sysext::system_time(v) };
    1
}

extern "win64" fn CompareFileTime(a: *const u64, b: *const u64) -> i32 {
    // SAFETY: the caller's FILETIMEs.
    let (a, b) = unsafe { (core::ptr::read_unaligned(a), core::ptr::read_unaligned(b)) };
    a.cmp(&b) as i32
}

pub fn exports() -> alloc::vec::Vec<(&'static str, u64)> {
    export_table![
        GetTimeZoneInformation,
        GetDynamicTimeZoneInformation,
        GetTimeZoneInformationForYear,
        SystemTimeToTzSpecificLocalTime,
        GetLocalTime,
        FileTimeToSystemTime,
        CompareFileTime,
    ]
}
