//! Desktop options kept in small `key=value` files under `/etc/osyx`,
//! written by Settings and read by the programs they concern. Parsing is
//! forgiving: a missing file, a missing key or a malformed value leaves
//! the default, and unknown keys are ignored, so an older program reads a
//! newer file.

use alloc::format;
use alloc::string::String;

/// The value of `key` in `key=value` lines.
fn value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|line| {
        let (k, v) = line.split_once('=')?;
        (k.trim() == key).then_some(v.trim())
    })
}

/// `/etc/osyx/power`: how the battery is shown and when it warns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowerOptions {
    /// Charge (percent) at which "battery low" is announced; 0 = never.
    pub low: u8,
    /// The percentage beside the battery in the task bar.
    pub show_percent: bool,
}

impl PowerOptions {
    pub const PATH: &'static str = "/etc/osyx/power";
    /// The thresholds Settings offers.
    pub const LOW_CHOICES: [u8; 5] = [0, 10, 15, 20, 30];

    pub fn parse(text: &str) -> PowerOptions {
        let d = PowerOptions::default();
        PowerOptions {
            low: value(text, "low").and_then(|v| v.parse::<u8>().ok()).filter(|&v| v <= 50).unwrap_or(d.low),
            show_percent: value(text, "percent").map_or(d.show_percent, |v| v != "0"),
        }
    }

    pub fn to_text(&self) -> String {
        format!("low={}\npercent={}\n", self.low, self.show_percent as u8)
    }

    /// The charge at which the warning turns into "critically low".
    pub fn critical(&self) -> u8 {
        if self.low == 0 {
            0
        } else {
            (self.low / 2).min(5).max(1)
        }
    }
}

impl Default for PowerOptions {
    fn default() -> Self {
        PowerOptions { low: 15, show_percent: true }
    }
}

/// How urgent a battery warning is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum BatteryWarning {
    Low,
    Critical,
}

/// Decides when to warn about a battery that runs down: once when the
/// charge reaches the low threshold, once more when it becomes critical,
/// and again only after the battery was charged or recovered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BatteryWatch {
    warned: Option<BatteryWarning>,
}

impl BatteryWatch {
    /// `percent` and whether the machine runs on the battery right now;
    /// returns the warning to show, if this reading calls for a new one.
    pub fn update(&mut self, percent: u8, discharging: bool, options: &PowerOptions) -> Option<BatteryWarning> {
        if !discharging || options.low == 0 || percent > options.low.saturating_add(2) {
            // Plugged in, switched off, or clearly above the threshold
            // again (two points of hysteresis against a flickering gauge).
            self.warned = None;
            return None;
        }
        let level = if percent <= options.critical() {
            BatteryWarning::Critical
        } else if percent <= options.low {
            BatteryWarning::Low
        } else {
            return None;
        };
        if self.warned.is_some_and(|w| w >= level) {
            return None;
        }
        self.warned = Some(level);
        Some(level)
    }
}

/// `/etc/osyx/clock`: how the task bar shows the time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockOptions {
    /// `2:05 PM` instead of `14:05`.
    pub twelve_hour: bool,
    pub seconds: bool,
    /// The date under the time.
    pub date: bool,
}

impl ClockOptions {
    pub const PATH: &'static str = "/etc/osyx/clock";

    pub fn parse(text: &str) -> ClockOptions {
        let d = ClockOptions::default();
        ClockOptions {
            twelve_hour: value(text, "hours").map_or(d.twelve_hour, |v| v == "12"),
            seconds: value(text, "seconds").map_or(d.seconds, |v| v == "1"),
            date: value(text, "date").map_or(d.date, |v| v != "0"),
        }
    }

    pub fn to_text(&self) -> String {
        format!(
            "hours={}\nseconds={}\ndate={}\n",
            if self.twelve_hour { 12 } else { 24 },
            self.seconds as u8,
            self.date as u8
        )
    }
}

impl Default for ClockOptions {
    fn default() -> Self {
        ClockOptions { twelve_hour: false, seconds: false, date: true }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_options_parse_and_round_trip() {
        assert_eq!(PowerOptions::parse(""), PowerOptions { low: 15, show_percent: true });
        assert_eq!(PowerOptions::parse("low=20\npercent=0\n"), PowerOptions { low: 20, show_percent: false });
        // Malformed and out-of-range values keep the default; unknown keys are skipped.
        assert_eq!(PowerOptions::parse("low=abc\nfuture=1\npercent=1"), PowerOptions::default());
        assert_eq!(PowerOptions::parse("low=99").low, 15);
        assert_eq!(PowerOptions::parse(" low = 10 ").low, 10);
        for low in PowerOptions::LOW_CHOICES {
            let o = PowerOptions { low, show_percent: low % 20 == 0 };
            assert_eq!(PowerOptions::parse(&o.to_text()), o);
        }
        assert_eq!(PowerOptions { low: 15, show_percent: true }.critical(), 5);
        assert_eq!(PowerOptions { low: 8, show_percent: true }.critical(), 4);
        assert_eq!(PowerOptions { low: 0, show_percent: true }.critical(), 0);
    }

    #[test]
    fn battery_warnings_come_once_per_level() {
        let o = PowerOptions::default();
        let mut w = BatteryWatch::default();
        assert_eq!(w.update(40, true, &o), None);
        assert_eq!(w.update(16, true, &o), None);
        assert_eq!(w.update(15, true, &o), Some(BatteryWarning::Low));
        assert_eq!(w.update(15, true, &o), None, "not again at the same level");
        assert_eq!(w.update(14, true, &o), None);
        // The gauge flickers up by a point: no second warning on the way down.
        assert_eq!(w.update(16, true, &o), None);
        assert_eq!(w.update(15, true, &o), None);
        assert_eq!(w.update(5, true, &o), Some(BatteryWarning::Critical));
        assert_eq!(w.update(3, true, &o), None);
        // Charging resets: the next discharge warns again.
        assert_eq!(w.update(4, false, &o), None);
        assert_eq!(w.update(4, true, &o), Some(BatteryWarning::Critical));
        // Recovered well above the threshold without a charger seen.
        assert_eq!(w.update(60, true, &o), None);
        assert_eq!(w.update(12, true, &o), Some(BatteryWarning::Low));
    }

    #[test]
    fn battery_warnings_can_be_turned_off_and_start_low() {
        let off = PowerOptions { low: 0, show_percent: true };
        let mut w = BatteryWatch::default();
        assert_eq!(w.update(1, true, &off), None);
        // A session that starts on a nearly empty battery says so at once,
        // and only the stronger warning.
        let mut w = BatteryWatch::default();
        assert_eq!(w.update(2, true, &PowerOptions::default()), Some(BatteryWarning::Critical));
        // Not on battery: nothing, however low.
        let mut w = BatteryWatch::default();
        assert_eq!(w.update(2, false, &PowerOptions::default()), None);
    }

    #[test]
    fn clock_options_parse_and_round_trip() {
        assert_eq!(ClockOptions::parse(""), ClockOptions { twelve_hour: false, seconds: false, date: true });
        assert_eq!(ClockOptions::parse("hours=12\nseconds=1\ndate=0\n"), ClockOptions { twelve_hour: true, seconds: true, date: false });
        assert_eq!(ClockOptions::parse("hours=25\nseconds=yes"), ClockOptions::default());
        for (twelve_hour, seconds, date) in [(true, false, true), (false, true, false), (true, true, true)] {
            let o = ClockOptions { twelve_hour, seconds, date };
            assert_eq!(ClockOptions::parse(&o.to_text()), o);
        }
    }
}
