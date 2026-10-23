//! Keyboard focus order: Tab moves forward, Shift+Tab back.

use alloc::vec::Vec;

#[derive(Clone, Debug, Default)]
pub struct FocusChain {
    ids: Vec<u32>,
    current: Option<usize>,
}

impl FocusChain {
    /// Focus order of widget ids; the first one is focused.
    pub fn new(ids: &[u32]) -> FocusChain {
        FocusChain { ids: ids.to_vec(), current: if ids.is_empty() { None } else { Some(0) } }
    }

    pub fn focused(&self) -> Option<u32> {
        self.current.map(|i| self.ids[i])
    }

    pub fn is(&self, id: u32) -> bool {
        self.focused() == Some(id)
    }

    /// Focuses `id` (e.g. after a click on it); false if it is not in the chain.
    pub fn set(&mut self, id: u32) -> bool {
        match self.ids.iter().position(|&i| i == id) {
            Some(p) => {
                self.current = Some(p);
                true
            }
            None => false,
        }
    }

    pub fn focus_next(&mut self) -> Option<u32> {
        if self.ids.is_empty() {
            return None;
        }
        self.current = Some(self.current.map_or(0, |i| (i + 1) % self.ids.len()));
        self.focused()
    }

    pub fn focus_prev(&mut self) -> Option<u32> {
        if self.ids.is_empty() {
            return None;
        }
        let n = self.ids.len();
        self.current = Some(self.current.map_or(n - 1, |i| (i + n - 1) % n));
        self.focused()
    }

    /// Handles Tab / Shift+Tab; returns true if focus moved.
    pub fn handle_key(&mut self, k: &crate::KeyEvent) -> bool {
        if k.down && k.key == osyx_input::key::TAB && !k.ctrl() && !k.alt() {
            if k.shift() {
                self.focus_prev();
            } else {
                self.focus_next();
            }
            return true;
        }
        false
    }
}
