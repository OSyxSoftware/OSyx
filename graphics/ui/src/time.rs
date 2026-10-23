//! Wall-clock time for the desktop clock: civil dates from Unix time and
//! POSIX `TZ` rules (`CET-1CEST,M3.5.0,M10.5.0/3`), the format the system
//! time zone is configured in (`/etc/TZ`).

use alloc::string::String;

/// Days since 1970-01-01 of a proleptic Gregorian date.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// (year, month 1..=12, day 1..=31) of a day number.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// 0 = Sunday.
pub fn weekday(days: i64) -> u32 {
    (days + 4).rem_euclid(7) as u32
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        _ => 28,
    }
}

/// A DST transition rule `Mm.w.d/time`: day `d` (0 = Sunday) of week `w`
/// (5 = last) of month `m`, at local `time` seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rule {
    pub month: u32,
    pub week: u32,
    pub day: u32,
    pub time: i64,
}

impl Rule {
    /// Day number of the rule's date in `year`.
    fn day_in(&self, year: i64) -> i64 {
        let first = days_from_civil(year, self.month, 1);
        let first_wd = weekday(first);
        let mut d = (self.day + 7 - first_wd) % 7 + 1 + 7 * (self.week - 1);
        let dim = days_in_month(year, self.month);
        while d > dim {
            d -= 7;
        }
        first + d as i64 - 1
    }
}

/// A parsed POSIX time zone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tz {
    pub std_name: String,
    /// Seconds east of UTC (the POSIX string says west, sign inverted).
    pub std_offset: i64,
    pub dst: Option<(String, i64, Rule, Rule)>,
}

impl Tz {
    pub fn utc() -> Tz {
        Tz { std_name: String::from("UTC"), std_offset: 0, dst: None }
    }

    /// Parses a POSIX `TZ` string; `None` if malformed.
    pub fn parse(s: &str) -> Option<Tz> {
        let mut p = Parser { s: s.trim().as_bytes(), i: 0 };
        let std_name = p.name()?;
        let std_offset = -p.offset()?;
        if p.done() {
            return Some(Tz { std_name, std_offset, dst: None });
        }
        let dst_name = p.name()?;
        let dst_offset = if p.peek() == Some(b',') || p.done() { std_offset + 3600 } else { -p.offset()? };
        // Without explicit rules POSIX leaves them implementation-defined;
        // use the US rules like glibc.
        let (start, end) = if p.eat(b',') {
            let a = p.rule()?;
            if !p.eat(b',') {
                return None;
            }
            (a, p.rule()?)
        } else {
            (Rule { month: 3, week: 2, day: 0, time: 7200 }, Rule { month: 11, week: 1, day: 0, time: 7200 })
        };
        if !p.done() {
            return None;
        }
        Some(Tz { std_name, std_offset, dst: Some((dst_name, dst_offset, start, end)) })
    }

    /// (offset east of UTC, zone abbreviation) in effect at `unix`.
    pub fn offset_at(&self, unix: i64) -> (i64, &str) {
        let Some((name, dst_off, start, end)) = &self.dst else { return (self.std_offset, &self.std_name) };
        let (year, _, _) = civil_from_days((unix + self.std_offset).div_euclid(86_400));
        // Transition instants in UTC: the start is given in standard time,
        // the end in daylight time.
        let s = start.day_in(year) * 86_400 + start.time - self.std_offset;
        let e = end.day_in(year) * 86_400 + end.time - dst_off;
        let in_dst = if s < e { unix >= s && unix < e } else { !(unix >= e && unix < s) };
        if in_dst {
            (*dst_off, name)
        } else {
            (self.std_offset, &self.std_name)
        }
    }

    pub fn local(&self, unix: i64) -> LocalTime {
        let (off, zone) = self.offset_at(unix);
        let t = unix + off;
        let days = t.div_euclid(86_400);
        let secs = t.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days);
        LocalTime {
            year,
            month,
            day,
            hour: (secs / 3600) as u32,
            minute: (secs / 60 % 60) as u32,
            second: (secs % 60) as u32,
            weekday: weekday(days),
            zone: String::from(zone),
            offset: off,
        }
    }
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn done(&self) -> bool {
        self.i >= self.s.len()
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn name(&mut self) -> Option<String> {
        let start = self.i;
        if self.eat(b'<') {
            while self.peek().is_some_and(|c| c != b'>') {
                self.i += 1;
            }
            let n = core::str::from_utf8(&self.s[start + 1..self.i]).ok()?;
            if !self.eat(b'>') || n.len() < 3 {
                return None;
            }
            return Some(String::from(n));
        }
        while self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
            self.i += 1;
        }
        (self.i - start >= 3).then(|| String::from(core::str::from_utf8(&self.s[start..self.i]).unwrap_or("")))
    }

    fn number(&mut self) -> Option<i64> {
        let start = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
        if self.i == start || self.i - start > 3 {
            return None;
        }
        core::str::from_utf8(&self.s[start..self.i]).ok()?.parse().ok()
    }

    /// `[+-]hh[:mm[:ss]]` in seconds (positive = west, as written).
    fn offset(&mut self) -> Option<i64> {
        let sign = if self.eat(b'-') {
            -1
        } else {
            self.eat(b'+');
            1
        };
        let h = self.number()?;
        let m = if self.eat(b':') { self.number()? } else { 0 };
        let s = if self.eat(b':') { self.number()? } else { 0 };
        if h > 167 || m > 59 || s > 59 {
            return None;
        }
        Some(sign * (h * 3600 + m * 60 + s))
    }

    fn rule(&mut self) -> Option<Rule> {
        if !self.eat(b'M') {
            return None; // Julian-day rules are not used by any configured zone
        }
        let month = self.number()? as u32;
        if !self.eat(b'.') {
            return None;
        }
        let week = self.number()? as u32;
        if !self.eat(b'.') {
            return None;
        }
        let day = self.number()? as u32;
        if !(1..=12).contains(&month) || !(1..=5).contains(&week) || day > 6 {
            return None;
        }
        let time = if self.eat(b'/') { self.offset()? } else { 7200 };
        Some(Rule { month, week, day, time })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalTime {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    /// 0 = Sunday.
    pub weekday: u32,
    pub zone: String,
    /// Seconds east of UTC.
    pub offset: i64,
}

const WEEKDAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const MONTHS: [&str; 12] =
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

impl LocalTime {
    /// `14:05`
    pub fn hm(&self) -> String {
        alloc::format!("{:02}:{:02}", self.hour, self.minute)
    }

    /// `Fri 23 Oct`
    pub fn short_date(&self) -> String {
        alloc::format!("{} {} {}", &WEEKDAYS[self.weekday as usize][..3], self.day, &MONTHS[(self.month - 1) as usize][..3])
    }

    /// `Friday, 23 October 2026`
    pub fn long_date(&self) -> String {
        alloc::format!("{}, {} {} {}", WEEKDAYS[self.weekday as usize], self.day, MONTHS[(self.month - 1) as usize], self.year)
    }

    /// `2026-10-23 14:05:09`
    pub fn iso(&self) -> String {
        alloc::format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            self.year,
            self.month,
            self.day,
            self.hour,
            self.minute,
            self.second
        )
    }
}
