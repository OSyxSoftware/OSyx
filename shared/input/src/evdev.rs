//! Linux input event codes, as virtio-input devices send them (virtio
//! specification 5.8: "the event types, codes and values are those of the
//! Linux evdev interface"), turned into what the input core takes: keys by
//! HID usage, buttons by number, relative motion, absolute positions and
//! wheel detents.
//!
//! A device sends the parts of one change as separate events and ends them
//! with a synchronisation event; [`Decoder`] collects motion and position
//! until then, so a diagonal movement is one motion, not two.

use crate::key;

pub const EV_SYN: u16 = 0;
pub const EV_KEY: u16 = 1;
pub const EV_REL: u16 = 2;
pub const EV_ABS: u16 = 3;

pub const REL_X: u16 = 0;
pub const REL_Y: u16 = 1;
pub const REL_HWHEEL: u16 = 6;
pub const REL_WHEEL: u16 = 8;
pub const ABS_X: u16 = 0;
pub const ABS_Y: u16 = 1;

pub const KEY_A: u16 = 30;
pub const BTN_LEFT: u16 = 0x110;
pub const BTN_TOUCH: u16 = 0x14A;

/// The HID usage of a Linux key code; `None` for codes that are not keys of
/// a keyboard this system knows (buttons, multimedia keys beyond volume).
pub fn key_usage(code: u16) -> Option<u16> {
    /// Codes 1 to 88 in order: the main block, function keys and keypad.
    const MAIN: [u16; 88] = [
        key::ESCAPE, 0x1E, 0x1F, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, key::MINUS, key::EQUAL, key::BACKSPACE, key::TAB,
        key::Q, key::W, key::E, key::R, key::T, key::Y, key::U, key::I, key::O, key::P, key::LEFT_BRACKET, key::RIGHT_BRACKET,
        key::ENTER, key::LEFT_CTRL, key::A, key::S, key::D, key::F, key::G, key::H, key::J, key::K, key::L, key::SEMICOLON,
        key::APOSTROPHE, key::GRAVE, key::LEFT_SHIFT, key::BACKSLASH, key::Z, key::X, key::C, key::V, key::B, key::N, key::M,
        key::COMMA, key::PERIOD, key::SLASH, 0xE5, key::KP_MULTIPLY, key::LEFT_ALT, key::SPACE, key::CAPS_LOCK,
        0x3A, 0x3B, 0x3C, 0x3D, 0x3E, 0x3F, 0x40, 0x41, 0x42, 0x43, key::NUM_LOCK, key::SCROLL_LOCK,
        0x5F, 0x60, 0x61, key::KP_MINUS, 0x5C, 0x5D, 0x5E, key::KP_PLUS, 0x59, 0x5A, 0x5B, key::KP_0, key::KP_PERIOD,
        0, 0, key::NON_US_BACKSLASH, 0x44, key::F12,
    ];
    let usage = match code {
        1..=88 => MAIN[code as usize - 1],
        96 => key::KP_ENTER,
        97 => 0xE4, // right Ctrl
        98 => key::KP_DIVIDE,
        99 => key::PRINT_SCREEN,
        100 => 0xE6, // right Alt
        102 => key::HOME,
        103 => key::UP,
        104 => key::PAGE_UP,
        105 => key::LEFT,
        106 => key::RIGHT,
        107 => key::END,
        108 => key::DOWN,
        109 => key::PAGE_DOWN,
        110 => key::INSERT,
        111 => key::DELETE,
        113 => key::MUTE,
        114 => key::VOLUME_DOWN,
        115 => key::VOLUME_UP,
        119 => key::PAUSE,
        125 => key::LEFT_GUI,
        126 => 0xE7, // right GUI
        127 => key::MENU,
        _ => 0,
    };
    (usage != 0).then_some(usage)
}

/// The button number (1 = left, 2 = right, 3 = middle, 4 = back, 5 =
/// forward) of a Linux button code; a touch or a pen tip is the left one.
pub fn button_number(code: u16) -> Option<u16> {
    match code {
        BTN_LEFT..=0x114 => Some(code - BTN_LEFT + 1),
        BTN_TOUCH => Some(1),
        _ => None,
    }
}

/// What a decoded event asks of the input core.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Key { usage: u16, down: bool },
    Button { number: u16, down: bool },
    Motion { dx: i32, dy: i32 },
    /// Each coordinate from 0 to [`crate::ABSOLUTE_MAX`].
    Absolute { x: i32, y: i32 },
    Wheel { vertical: i32, horizontal: i32 },
}

/// Collects the events of one device between synchronisation events.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Decoder {
    /// The largest X and Y the device reports (0: not an absolute device).
    abs_max: (u32, u32),
    motion: (i32, i32),
    wheel: (i32, i32),
    position: (u32, u32),
    moved: bool,
}

impl Decoder {
    /// `abs_max`: the device's largest absolute X and Y, (0, 0) for a
    /// keyboard or a relative pointer.
    pub fn new(abs_max: (u32, u32)) -> Decoder {
        Decoder { abs_max, ..Decoder::default() }
    }

    /// One event of the device; `emit` receives what it amounts to. Keys
    /// and buttons pass at once (after the position gathered so far, so a
    /// press lands where the pointer is); motion, position and wheel at
    /// the synchronisation event.
    pub fn event(&mut self, kind: u16, code: u16, value: u32, emit: &mut dyn FnMut(Action)) {
        match (kind, code) {
            (EV_SYN, _) => self.flush(emit),
            (EV_KEY, _) => {
                // 2 is the device's own auto-repeat: the input core repeats.
                if value > 1 {
                    return;
                }
                if let Some(number) = button_number(code) {
                    self.flush(emit);
                    emit(Action::Button { number, down: value == 1 });
                } else if let Some(usage) = key_usage(code) {
                    emit(Action::Key { usage, down: value == 1 });
                }
            }
            (EV_REL, REL_X) => self.motion.0 = self.motion.0.saturating_add(value as i32),
            (EV_REL, REL_Y) => self.motion.1 = self.motion.1.saturating_add(value as i32),
            (EV_REL, REL_WHEEL) => self.wheel.0 = self.wheel.0.saturating_add(value as i32),
            (EV_REL, REL_HWHEEL) => self.wheel.1 = self.wheel.1.saturating_add(value as i32),
            (EV_ABS, ABS_X) => {
                self.position.0 = value;
                self.moved = true;
            }
            (EV_ABS, ABS_Y) => {
                self.position.1 = value;
                self.moved = true;
            }
            _ => {}
        }
    }

    fn flush(&mut self, emit: &mut dyn FnMut(Action)) {
        if self.moved && self.abs_max.0 > 0 && self.abs_max.1 > 0 {
            let scale = |v: u32, max: u32| (v.min(max) as u64 * crate::ABSOLUTE_MAX as u64 / max as u64) as i32;
            emit(Action::Absolute { x: scale(self.position.0, self.abs_max.0), y: scale(self.position.1, self.abs_max.1) });
        }
        self.moved = false;
        if self.motion != (0, 0) {
            emit(Action::Motion { dx: self.motion.0, dy: self.motion.1 });
            self.motion = (0, 0);
        }
        if self.wheel != (0, 0) {
            emit(Action::Wheel { vertical: self.wheel.0, horizontal: self.wheel.1 });
            self.wheel = (0, 0);
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    fn run(d: &mut Decoder, events: &[(u16, u16, i32)]) -> Vec<Action> {
        let mut out = Vec::new();
        for &(kind, code, value) in events {
            d.event(kind, code, value as u32, &mut |a| out.push(a));
        }
        out
    }

    #[test]
    fn key_codes_map_to_hid_usages() {
        assert_eq!(key_usage(1), Some(key::ESCAPE));
        assert_eq!(key_usage(2), Some(key::DIGIT1));
        assert_eq!(key_usage(11), Some(key::DIGIT0));
        assert_eq!(key_usage(16), Some(key::Q));
        assert_eq!(key_usage(KEY_A), Some(key::A));
        assert_eq!(key_usage(44), Some(key::Z));
        assert_eq!(key_usage(28), Some(key::ENTER));
        assert_eq!(key_usage(57), Some(key::SPACE));
        assert_eq!(key_usage(59), Some(key::F1));
        assert_eq!(key_usage(63), Some(key::F5));
        assert_eq!(key_usage(88), Some(key::F12));
        assert_eq!(key_usage(79), Some(key::KP_1));
        assert_eq!(key_usage(86), Some(key::NON_US_BACKSLASH));
        assert_eq!((key_usage(29), key_usage(97)), (Some(key::LEFT_CTRL), Some(0xE4)));
        assert_eq!((key_usage(42), key_usage(54)), (Some(key::LEFT_SHIFT), Some(0xE5)));
        assert_eq!((key_usage(56), key_usage(100)), (Some(key::LEFT_ALT), Some(0xE6)));
        assert_eq!((key_usage(103), key_usage(108), key_usage(105), key_usage(106)), (Some(key::UP), Some(key::DOWN), Some(key::LEFT), Some(key::RIGHT)));
        assert_eq!((key_usage(111), key_usage(110), key_usage(125)), (Some(key::DELETE), Some(key::INSERT), Some(key::LEFT_GUI)));
        assert_eq!((key_usage(0), key_usage(84), key_usage(85), key_usage(BTN_LEFT), key_usage(600)), (None, None, None, None, None));
        // Every letter and digit has exactly one code.
        let mut letters = 0u32;
        for code in 1..=127 {
            if let Some(u) = key_usage(code) {
                if (key::A..=key::Z).contains(&u) {
                    assert_eq!(letters & 1 << (u - key::A), 0, "usage {u:#x} twice");
                    letters |= 1 << (u - key::A);
                }
            }
        }
        assert_eq!(letters, (1 << 26) - 1);
    }

    #[test]
    fn keyboard_events() {
        let mut d = Decoder::new((0, 0));
        let a = run(&mut d, &[(EV_KEY, KEY_A, 1), (EV_SYN, 0, 0), (EV_KEY, KEY_A, 2), (EV_SYN, 0, 0), (EV_KEY, KEY_A, 0), (EV_SYN, 0, 0), (EV_KEY, 240, 1)]);
        assert_eq!(a, [Action::Key { usage: key::A, down: true }, Action::Key { usage: key::A, down: false }]);
    }

    #[test]
    fn relative_motion_is_gathered_until_the_synchronisation() {
        let mut d = Decoder::new((0, 0));
        let a = run(&mut d, &[(EV_REL, REL_X, 3), (EV_REL, REL_Y, -2), (EV_REL, REL_X, 1), (EV_SYN, 0, 0), (EV_SYN, 0, 0)]);
        assert_eq!(a, [Action::Motion { dx: 4, dy: -2 }]);
        let a = run(&mut d, &[(EV_REL, REL_WHEEL, -1), (EV_REL, REL_HWHEEL, 2), (EV_SYN, 0, 0)]);
        assert_eq!(a, [Action::Wheel { vertical: -1, horizontal: 2 }]);
        // A press between motion and synchronisation lands after the motion.
        let a = run(&mut d, &[(EV_REL, REL_X, 5), (EV_KEY, BTN_LEFT, 1), (EV_SYN, 0, 0), (EV_KEY, BTN_LEFT + 1, 1), (EV_KEY, BTN_LEFT + 2, 0)]);
        assert_eq!(
            a,
            [Action::Motion { dx: 5, dy: 0 }, Action::Button { number: 1, down: true }, Action::Button { number: 2, down: true }, Action::Button { number: 3, down: false }]
        );
    }

    #[test]
    fn absolute_positions_are_scaled_and_precede_the_press() {
        // QEMU's virtio-tablet: 0..0x7FFF on both axes.
        let mut d = Decoder::new((0x7FFF, 0x7FFF));
        let a = run(&mut d, &[(EV_ABS, ABS_X, 0x7FFF), (EV_ABS, ABS_Y, 0), (EV_KEY, BTN_LEFT, 1), (EV_SYN, 0, 0)]);
        assert_eq!(a, [Action::Absolute { x: crate::ABSOLUTE_MAX, y: 0 }, Action::Button { number: 1, down: true }]);
        // Only one axis changes: the other keeps its last value.
        let a = run(&mut d, &[(EV_ABS, ABS_Y, 0x4000), (EV_SYN, 0, 0), (EV_SYN, 0, 0)]);
        assert_eq!(a, [Action::Absolute { x: crate::ABSOLUTE_MAX, y: 0x4000 * crate::ABSOLUTE_MAX / 0x7FFF }]);
        // Out of range is clamped; a touch is the left button.
        let a = run(&mut d, &[(EV_ABS, ABS_X, 0x9000), (EV_KEY, BTN_TOUCH, 1)]);
        assert_eq!(a[0], Action::Absolute { x: crate::ABSOLUTE_MAX, y: 0x4000 * crate::ABSOLUTE_MAX / 0x7FFF });
        assert_eq!(a[1], Action::Button { number: 1, down: true });
        // A device that never said its range reports no positions.
        let mut d = Decoder::new((0, 0));
        assert!(run(&mut d, &[(EV_ABS, ABS_X, 5), (EV_SYN, 0, 0)]).is_empty());
    }
}
