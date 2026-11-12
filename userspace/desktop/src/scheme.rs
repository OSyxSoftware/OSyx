//! Which colour scheme to show now: the user's choice in
//! `/etc/osyx/theme` (`dark`, `light` or `auto`), and for `auto` the sun at
//! the computer's place. Shared by the applications' event loop, the
//! desktop shell and PRISM, so that all of them change together.

use os_std::fs;
use osyx_ui::theme::{Scheme, SchemeChoice};
use osyx_ui::time::{civil_from_days, days_from_civil, Tz};

/// The user's choice of scheme: `dark`, `light` or `auto`.
pub const THEME_FILE: &str = "/etc/osyx/theme";
/// Optional `latitude longitude` in degrees (north and east positive) for
/// the automatic scheme.
pub const LOCATION_FILE: &str = "/etc/osyx/location";

fn unix_now() -> i64 {
    os_std::time::SystemTime::now().duration_since(os_std::time::SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

pub fn choice() -> SchemeChoice {
    SchemeChoice::parse(&fs::read_to_string(THEME_FILE).unwrap_or_default())
}

/// Where the computer is, for sunrise and sunset: the configured place, or
/// else the middle of the time zone (15 degrees of longitude per hour of
/// its standard offset) at 50 degrees north.
pub fn location() -> (f64, f64) {
    let text = fs::read_to_string(LOCATION_FILE).unwrap_or_default();
    let mut parts = text.split_whitespace().filter_map(|p| p.parse::<f64>().ok());
    if let (Some(lat), Some(lon)) = (parts.next(), parts.next()) {
        if (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon) {
            return (lat, lon);
        }
    }
    let tz = fs::read_to_string("/etc/TZ").ok().and_then(|s| Tz::parse(s.trim())).unwrap_or_else(Tz::utc);
    // The offset in the middle of January: standard time in the north, an
    // hour off at worst in the south.
    let year = civil_from_days(unix_now().div_euclid(86_400)).0;
    let offset = tz.offset_at(days_from_civil(year, 1, 15) * 86_400).0;
    (50.0, (offset as f64 / 3600.0 * 15.0).clamp(-180.0, 180.0))
}

/// The scheme to show now.
pub fn current() -> Scheme {
    match choice() {
        SchemeChoice::Automatic => {
            let (lat, lon) = location();
            SchemeChoice::Automatic.at(unix_now(), lat, lon)
        }
        fixed => fixed.at(0, 0.0, 0.0),
    }
}
