//! Scheduling control for the current thread. Spawning additional threads in
//! a process is not supported yet (each OSyx process has one thread).

use crate::sys;
use core::time::Duration;

/// Blocks for at least `d` (OSyx timer resolution: 10 ms).
pub fn sleep(d: Duration) {
    let ns = u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
    if ns > 0 {
        sys::sleep_ns(ns);
    }
}

/// Gives the rest of the current time slice to other threads.
pub fn yield_now() {
    sys::yield_now()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::Instant;

    #[test]
    fn sleep_waits_at_least_the_duration() {
        let start = Instant::now();
        sleep(Duration::from_millis(20));
        assert!(start.elapsed() >= Duration::from_millis(20));
        yield_now();
    }
}
