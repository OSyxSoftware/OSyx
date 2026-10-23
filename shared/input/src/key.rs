//! Key codes: HID Keyboard/Keypad page (0x07) usage ids (HID Usage Tables
//! 1.4, section 10). The names describe the key's position on a US
//! keyboard; layouts give them their meaning.

use crate::modifier;

pub const A: u16 = 0x04;
pub const B: u16 = 0x05;
pub const C: u16 = 0x06;
pub const D: u16 = 0x07;
pub const E: u16 = 0x08;
pub const F: u16 = 0x09;
pub const G: u16 = 0x0A;
pub const H: u16 = 0x0B;
pub const I: u16 = 0x0C;
pub const J: u16 = 0x0D;
pub const K: u16 = 0x0E;
pub const L: u16 = 0x0F;
pub const M: u16 = 0x10;
pub const N: u16 = 0x11;
pub const O: u16 = 0x12;
pub const P: u16 = 0x13;
pub const Q: u16 = 0x14;
pub const R: u16 = 0x15;
pub const S: u16 = 0x16;
pub const T: u16 = 0x17;
pub const U: u16 = 0x18;
pub const V: u16 = 0x19;
pub const W: u16 = 0x1A;
pub const X: u16 = 0x1B;
pub const Y: u16 = 0x1C;
pub const Z: u16 = 0x1D;
pub const DIGIT1: u16 = 0x1E;
pub const DIGIT0: u16 = 0x27;
pub const ENTER: u16 = 0x28;
pub const ESCAPE: u16 = 0x29;
pub const BACKSPACE: u16 = 0x2A;
pub const TAB: u16 = 0x2B;
pub const SPACE: u16 = 0x2C;
pub const MINUS: u16 = 0x2D;
pub const EQUAL: u16 = 0x2E;
pub const LEFT_BRACKET: u16 = 0x2F;
pub const RIGHT_BRACKET: u16 = 0x30;
pub const BACKSLASH: u16 = 0x31;
pub const NON_US_HASH: u16 = 0x32;
pub const SEMICOLON: u16 = 0x33;
pub const APOSTROPHE: u16 = 0x34;
pub const GRAVE: u16 = 0x35;
pub const COMMA: u16 = 0x36;
pub const PERIOD: u16 = 0x37;
pub const SLASH: u16 = 0x38;
pub const CAPS_LOCK: u16 = 0x39;
pub const F1: u16 = 0x3A;
pub const F12: u16 = 0x45;
pub const PRINT_SCREEN: u16 = 0x46;
pub const SCROLL_LOCK: u16 = 0x47;
pub const PAUSE: u16 = 0x48;
pub const INSERT: u16 = 0x49;
pub const HOME: u16 = 0x4A;
pub const PAGE_UP: u16 = 0x4B;
pub const DELETE: u16 = 0x4C;
pub const END: u16 = 0x4D;
pub const PAGE_DOWN: u16 = 0x4E;
pub const RIGHT: u16 = 0x4F;
pub const LEFT: u16 = 0x50;
pub const DOWN: u16 = 0x51;
pub const UP: u16 = 0x52;
pub const NUM_LOCK: u16 = 0x53;
pub const KP_DIVIDE: u16 = 0x54;
pub const KP_MULTIPLY: u16 = 0x55;
pub const KP_MINUS: u16 = 0x56;
pub const KP_PLUS: u16 = 0x57;
pub const KP_ENTER: u16 = 0x58;
pub const KP_1: u16 = 0x59;
pub const KP_0: u16 = 0x62;
pub const KP_PERIOD: u16 = 0x63;
pub const NON_US_BACKSLASH: u16 = 0x64;
pub const MENU: u16 = 0x65;
pub const LEFT_CTRL: u16 = 0xE0;
pub const LEFT_SHIFT: u16 = 0xE1;
pub const LEFT_ALT: u16 = 0xE2;
pub const LEFT_GUI: u16 = 0xE3;
pub const RIGHT_CTRL: u16 = 0xE4;
pub const RIGHT_SHIFT: u16 = 0xE5;
pub const RIGHT_ALT: u16 = 0xE6;
pub const RIGHT_GUI: u16 = 0xE7;

/// The modifier bit of a modifier key.
pub fn modifier_bit(usage: u16) -> Option<u32> {
    match usage {
        LEFT_CTRL..=RIGHT_GUI => Some(1 << (usage - LEFT_CTRL)),
        _ => None,
    }
}

/// Keys that never auto-repeat: modifiers and locks.
pub fn repeats(usage: u16) -> bool {
    modifier_bit(usage).is_none() && !matches!(usage, CAPS_LOCK | NUM_LOCK | SCROLL_LOCK | PRINT_SCREEN | PAUSE)
}

/// Function key number (1..=12) of F1..F12.
pub fn function_number(usage: u16) -> Option<u8> {
    (F1..=F12).contains(&usage).then(|| (usage - F1 + 1) as u8)
}

/// A short, layout-independent name (for diagnostics and shortcuts).
pub fn name(usage: u16) -> &'static str {
    const LETTERS: [&str; 26] =
        ["A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q", "R", "S", "T", "U", "V", "W", "X", "Y", "Z"];
    const DIGITS: [&str; 10] = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "0"];
    const FKEYS: [&str; 12] = ["F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12"];
    match usage {
        A..=Z => LETTERS[(usage - A) as usize],
        DIGIT1..=DIGIT0 => DIGITS[(usage - DIGIT1) as usize],
        F1..=F12 => FKEYS[(usage - F1) as usize],
        ENTER => "Enter",
        ESCAPE => "Escape",
        BACKSPACE => "Backspace",
        TAB => "Tab",
        SPACE => "Space",
        CAPS_LOCK => "CapsLock",
        NUM_LOCK => "NumLock",
        INSERT => "Insert",
        DELETE => "Delete",
        HOME => "Home",
        END => "End",
        PAGE_UP => "PageUp",
        PAGE_DOWN => "PageDown",
        LEFT => "Left",
        RIGHT => "Right",
        UP => "Up",
        DOWN => "Down",
        LEFT_CTRL | RIGHT_CTRL => "Ctrl",
        LEFT_SHIFT | RIGHT_SHIFT => "Shift",
        LEFT_ALT => "Alt",
        RIGHT_ALT => "AltGr",
        LEFT_GUI | RIGHT_GUI => "Super",
        _ => "?",
    }
}

/// True if Ctrl (either side) is held.
pub fn ctrl(m: u32) -> bool {
    m & modifier::CTRL != 0
}

pub fn shift(m: u32) -> bool {
    m & modifier::SHIFT != 0
}

/// Left Alt only (right Alt is AltGr).
pub fn alt(m: u32) -> bool {
    m & modifier::LEFT_ALT != 0
}

pub fn altgr(m: u32) -> bool {
    m & modifier::RIGHT_ALT != 0
}

pub fn gui(m: u32) -> bool {
    m & modifier::GUI != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_bits_match_the_boot_report() {
        assert_eq!(modifier_bit(LEFT_CTRL), Some(modifier::LEFT_CTRL));
        assert_eq!(modifier_bit(RIGHT_ALT), Some(modifier::RIGHT_ALT));
        assert_eq!(modifier_bit(RIGHT_GUI), Some(modifier::RIGHT_GUI));
        assert_eq!(modifier_bit(A), None);
        assert!(!repeats(LEFT_SHIFT) && !repeats(CAPS_LOCK) && repeats(A) && repeats(BACKSPACE));
    }

    #[test]
    fn names_and_function_keys() {
        assert_eq!(name(A), "A");
        assert_eq!(name(Z), "Z");
        assert_eq!(name(DIGIT0), "0");
        assert_eq!(name(0x1F), "2");
        assert_eq!(name(F12), "F12");
        assert_eq!(name(RIGHT_ALT), "AltGr");
        assert_eq!(name(0x99), "?");
        assert_eq!(function_number(F1), Some(1));
        assert_eq!(function_number(0x44), Some(11));
        assert_eq!(function_number(ENTER), None);
        assert!(altgr(modifier::RIGHT_ALT) && !alt(modifier::RIGHT_ALT));
    }
}
