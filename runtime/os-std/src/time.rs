//! Monotonic time. On OSyx the clock currently ticks at the kernel timer
//! resolution (10 ms); `Instant` still has nanosecond units.

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

#[cfg(test)]
mod tests {
    use super::*;

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
