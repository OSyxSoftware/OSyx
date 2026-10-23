//! The time zone Windows programs see: the system zone of OSyx, a POSIX
//! rule string in /etc/TZ (as the desktop clock uses), or UTC without one.
//! The C runtime lets the TZ variable override it, as the UCRT does; TZ's
//! Windows form ("PST8PDT", "XXX[+|-]h[:mm[:ss]][YYY]") is a POSIX string
//! whose daylight rules default to the US ones, which is also what the
//! UCRT applies.
use alloc::string::String;
use os_std::sync::Mutex;
pub use osyx_ui::time::Tz;

static SYSTEM: Mutex<Option<Tz>> = Mutex::new(None);

/// The OSyx system zone (/etc/TZ), read once.
pub fn system() -> Tz {
    let mut s = SYSTEM.lock().unwrap();
    s.get_or_insert_with(|| {
        os_std::fs::read("/etc/TZ")
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .and_then(|t| Tz::parse(t.trim()))
            .unwrap_or_else(Tz::utc)
    })
    .clone()
}

/// The zone of the C runtime: TZ if it is set and valid, else the system zone.
pub fn crt() -> Tz {
    match crate::kernel32::env_get("TZ").filter(|t| !t.is_empty()) {
        Some(t) => Tz::parse(&t).unwrap_or_else(system),
        None => system(),
    }
}

/// The standard and daylight offsets east of UTC, in seconds.
pub fn offsets(tz: &Tz) -> (i64, Option<i64>) {
    (tz.std_offset, tz.dst.as_ref().map(|d| d.1))
}

/// The UTC instant of local time `local` (seconds since 1970 in local
/// wall time): `dst` Some(true/false) forces daylight or standard time,
/// None determines it (the earlier instant when ambiguous).
pub fn to_utc(tz: &Tz, local: i64, dst: Option<bool>) -> i64 {
    let (std, dst_off) = offsets(tz);
    let Some(d) = dst_off else { return local - std };
    match dst {
        Some(true) => local - d,
        Some(false) => local - std,
        None => {
            let as_dst = local - d;
            if tz.offset_at(as_dst).0 == d {
                as_dst
            } else {
                local - std
            }
        }
    }
}
