//! Small, bounded policy for protecting regions that repeatedly refault.

/// Counts quick refaults in a window and temporarily protects a hot region.
#[derive(Default)]
pub struct RefaultGuard {
    window_start: u64,
    quick: u32,
    protected_until: u64,
}

impl RefaultGuard {
    /// Times use caller-supplied monotonic ticks. Parameters use the same unit.
    pub fn restored(&mut self, evicted: u64, now: u64, window: u64, threshold: u32, cooldown: u64) -> bool {
        if now.saturating_sub(evicted) > window || threshold == 0 { return false; }
        if now.saturating_sub(self.window_start) > window {
            self.window_start = now;
            self.quick = 0;
        }
        self.quick = self.quick.saturating_add(1);
        if self.quick >= threshold {
            self.protected_until = now.saturating_add(cooldown);
            self.quick = 0;
            self.window_start = now;
        }
        true
    }

    /// Extreme pressure may override protection to preserve forward progress.
    pub fn protects(&self, now: u64, emergency: bool) -> bool {
        !emergency && now < self.protected_until
    }
}

#[cfg(test)]
mod tests {
    use super::RefaultGuard;

    #[test]
    fn repeated_quick_refaults_temporarily_protect_region() {
        let mut guard = RefaultGuard::default();
        assert!(guard.restored(100, 110, 50, 3, 100));
        assert!(guard.restored(101, 111, 50, 3, 100));
        assert!(!guard.protects(111, false));
        assert!(guard.restored(102, 112, 50, 3, 100));
        assert!(guard.protects(112, false));
        assert!(!guard.protects(112, true));
        assert!(!guard.protects(212, false));
    }

    #[test]
    fn old_or_spread_out_refaults_do_not_trigger_protection() {
        let mut guard = RefaultGuard::default();
        assert!(!guard.restored(0, 100, 20, 2, 50));
        assert!(guard.restored(100, 101, 20, 2, 50));
        assert!(guard.restored(150, 151, 20, 2, 50));
        assert!(!guard.protects(151, false));
    }
}
