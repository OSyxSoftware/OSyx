//! Monotonic time ([`Instant`]) and wall-clock time ([`SystemTime`]). On
//! OSyx the monotonic clock is the timer tick (1 ms) interpolated with the
//! TSC (sub-microsecond); the wall clock is the CMOS RTC read at boot plus
//! the uptime.

use crate::sys;
use core::ops::{Add, Sub};
pub use core::time::Duration;

/// A point on the monotonic clock (time since boot on OSyx).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Instant {
    ns: u64,
}

impl Instant {
    /// Reads the monotonic clock. Panics only if the platform has no
    /// monotonic clock, which every supported back end provides.
    pub fn now() -> Instant {
        Instant { ns: sys::monotonic_ns().expect("monotonic clock unavailable") }
    }

    pub fn duration_since(&self, earlier: Instant) -> Duration {
        self.checked_duration_since(earlier).unwrap_or(Duration::ZERO)
    }

    pub fn checked_duration_since(&self, earlier: Instant) -> Option<Duration> {
        self.ns.checked_sub(earlier.ns).map(Duration::from_nanos)
    }

    pub fn elapsed(&self) -> Duration {
        Instant::now().duration_since(*self)
    }

    pub fn checked_add(&self, d: Duration) -> Option<Instant> {
        let ns = u64::try_from(d.as_nanos()).ok()?;
        self.ns.checked_add(ns).map(|ns| Instant { ns })
    }

    /// Nanoseconds since the clock's epoch (boot on OSyx).
    pub fn as_nanos_since_boot(&self) -> u64 {
        self.ns
    }
}

impl Add<Duration> for Instant {
    type Output = Instant;
    fn add(self, d: Duration) -> Instant {
        self.checked_add(d).expect("overflow when adding duration to instant")
    }
}

impl Sub<Instant> for Instant {
    type Output = Duration;
    fn sub(self, other: Instant) -> Duration {
        self.duration_since(other)
    }
}

/// A point in wall-clock time (nanoseconds since 1970-01-01 UTC).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SystemTime {
    ns: i64,
}

pub const UNIX_EPOCH: SystemTime = SystemTime { ns: 0 };

impl SystemTime {
    pub const UNIX_EPOCH: SystemTime = UNIX_EPOCH;

    pub fn now() -> SystemTime {
        SystemTime { ns: sys::realtime_ns().expect("wall clock unavailable") }
    }

    pub(crate) fn from_unix_nanos(ns: i64) -> SystemTime {
        SystemTime { ns }
    }

    /// `Err` holds how far `earlier` is in the future, like std.
    pub fn duration_since(&self, earlier: SystemTime) -> core::result::Result<Duration, Duration> {
        if self.ns >= earlier.ns {
            Ok(Duration::from_nanos((self.ns - earlier.ns) as u64))
        } else {
            Err(Duration::from_nanos((earlier.ns - self.ns) as u64))
        }
    }

    pub fn elapsed(&self) -> core::result::Result<Duration, Duration> {
        SystemTime::now().duration_since(*self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_time_is_after_2020() {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        assert!(now.as_secs() > 1_577_836_800);
        let a = SystemTime::from_unix_nanos(5);
        let b = SystemTime::from_unix_nanos(8);
        assert_eq!(b.duration_since(a), Ok(Duration::from_nanos(3)));
        assert_eq!(a.duration_since(b), Err(Duration::from_nanos(3)));
    }

    #[test]
    fn instant_arithmetic() {
        let a = Instant { ns: 1_000 };
        let b = a + Duration::from_micros(3);
        assert_eq!(b.as_nanos_since_boot(), 4_000);
        assert_eq!(b - a, Duration::from_nanos(3_000));
        assert_eq!(a - b, Duration::ZERO, "saturates like std");
        assert_eq!(a.checked_duration_since(b), None);
        assert_eq!(a.checked_add(Duration::MAX), None);
    }

    #[test]
    fn host_clock_is_monotonic() {
        let a = Instant::now();
        let b = Instant::now();
        assert!(b >= a);
        assert!(a.elapsed() >= Duration::ZERO);
    }
}
