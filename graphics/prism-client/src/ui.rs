//! Turns PRISM input events into OSyx UI toolkit events.

use os_std::time::Instant;
use osyx_ui::{ClickCounter, Event as UiEvent, KeyEvent};
use prism_proto::{key_state, pointer, Event};

/// Per-connection state for the translation (double-click detection).
pub struct Adapter {
    clicks: ClickCounter,
    start: Instant,
}

impl Default for Adapter {
    fn default() -> Self {
        Adapter { clicks: ClickCounter::default(), start: Instant::now() }
    }
}

impl Adapter {
    /// The window (local id) and toolkit event for a PRISM key or pointer
    /// event; `None` for everything else.
    pub fn translate(&mut self, ev: &Event) -> Option<(u32, UiEvent)> {
        match *ev {
            Event::Pointer { window, kind, x, y, button, pressed, wheel_v, .. } => {
                let e = match kind {
                    pointer::ENTER | pointer::MOTION => UiEvent::PointerMove { x, y },
                    pointer::LEAVE => UiEvent::PointerLeave,
                    pointer::BUTTON if pressed => {
                        let now = self.start.elapsed().as_millis() as u64;
                        UiEvent::PointerDown { x, y, button, clicks: self.clicks.press(now, x, y, button) }
                    }
                    pointer::BUTTON => UiEvent::PointerUp { x, y, button },
                    pointer::WHEEL => UiEvent::Wheel { x, y, dy: wheel_v },
                    _ => return None,
                };
                Some((window, e))
            }
            Event::Key { window, usage, state, modifiers, text } => Some((
                window,
                UiEvent::Key(KeyEvent {
                    key: usage,
                    text,
                    mods: modifiers,
                    down: state != key_state::UP,
                    repeat: state == key_state::REPEAT,
                }),
            )),
            _ => None,
        }
    }
}
