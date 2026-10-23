//! Key auto-repeat, done once in the input core for every keyboard.
//!
//! The most recently pressed repeatable key repeats: first after `delay`,
//! then every `interval`. Releasing it (or pressing another key) stops it.
//! Times are milliseconds of a monotonic clock.

use crate::key;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RepeatConfig {
    pub delay_ms: u64,
    pub interval_ms: u64,
}

impl Default for RepeatConfig {
    /// 500 ms delay, 30 repeats per second.
    fn default() -> Self {
        RepeatConfig { delay_ms: 500, interval_ms: 33 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Held {
    device: u32,
    usage: u16,
    next_ms: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Repeater {
    pub config: RepeatConfig,
    held: Option<Held>,
}

impl Repeater {
    pub fn new(config: RepeatConfig) -> Repeater {
        Repeater { config, held: None }
    }

    /// A key went down on `device`.
    pub fn press(&mut self, device: u32, usage: u16, now_ms: u64) {
        if key::repeats(usage) {
            self.held = Some(Held { device, usage, next_ms: now_ms + self.config.delay_ms });
        }
    }

    /// A key went up.
    pub fn release(&mut self, device: u32, usage: u16) {
        if self.held.is_some_and(|h| h.device == device && h.usage == usage) {
            self.held = None;
        }
    }

    /// The device went away: its key must not keep repeating.
    pub fn forget_device(&mut self, device: u32) {
        if self.held.is_some_and(|h| h.device == device) {
            self.held = None;
        }
    }

    /// When the next repeat is due, if a key is held.
    pub fn deadline(&self) -> Option<u64> {
        self.held.map(|h| h.next_ms)
    }

    /// Returns (device, key) if a repeat is due at `now_ms`. At most one
    /// repeat per call; a caller that slept long gets repeats spaced from
    /// now, not a burst.
    pub fn poll(&mut self, now_ms: u64) -> Option<(u32, u16)> {
        let h = self.held.as_mut()?;
        if now_ms < h.next_ms {
            return None;
        }
        h.next_ms = now_ms.max(h.next_ms) + self.config.interval_ms;
        Some((h.device, h.usage))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key::*;

    #[test]
    fn repeats_after_delay_then_at_interval() {
        let mut r = Repeater::new(RepeatConfig { delay_ms: 500, interval_ms: 50 });
        r.press(1, A, 1000);
        assert_eq!(r.deadline(), Some(1500));
        assert_eq!(r.poll(1499), None);
        assert_eq!(r.poll(1500), Some((1, A)));
        assert_eq!(r.poll(1520), None);
        assert_eq!(r.poll(1550), Some((1, A)));
        r.release(1, A);
        assert_eq!(r.poll(5000), None);
        assert_eq!(r.deadline(), None);
    }

    #[test]
    fn latest_key_wins_and_modifiers_do_not_repeat() {
        let mut r = Repeater::new(RepeatConfig::default());
        r.press(1, A, 0);
        r.press(1, B, 100);
        r.release(1, A);
        assert_eq!(r.poll(600), Some((1, B)), "releasing an older key keeps the newer one repeating");
        r.press(1, LEFT_SHIFT, 700);
        assert_eq!(r.deadline(), Some(633), "shift does not take over");
        r.press(2, C, 800);
        r.forget_device(2);
        assert_eq!(r.deadline(), None);
    }

    #[test]
    fn a_late_poll_does_not_burst() {
        let mut r = Repeater::new(RepeatConfig { delay_ms: 100, interval_ms: 10 });
        r.press(1, A, 0);
        assert_eq!(r.poll(1000), Some((1, A)));
        assert_eq!(r.poll(1001), None);
        assert_eq!(r.deadline(), Some(1010));
    }
}
