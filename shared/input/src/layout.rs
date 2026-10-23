//! Keyboard layouts: key code + modifier state -> text.
//!
//! Layouts are applied by whoever turns key events into text (the UI
//! toolkit), never by the kernel. Available: `us` (US QWERTY) and `pl`
//! (Polish "programmer's" layout: US QWERTY plus AltGr letters
//! ąćęłńóśźż). Ctrl (or Super) combinations produce no text: they are
//! shortcuts.

use crate::key::{self, *};
use crate::modifier;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    Us,
    Polish,
}

impl Layout {
    pub const ALL: [Layout; 2] = [Layout::Us, Layout::Polish];

    /// `us` or `pl` (case-insensitive).
    pub fn from_name(name: &str) -> Option<Layout> {
        if name.eq_ignore_ascii_case("us") {
            Some(Layout::Us)
        } else if name.eq_ignore_ascii_case("pl") {
            Some(Layout::Polish)
        } else {
            None
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Layout::Us => "us",
            Layout::Polish => "pl",
        }
    }

    /// The character `usage` produces with `mods`, if any.
    pub fn translate(self, usage: u16, mods: u32) -> Option<char> {
        if mods & (modifier::CTRL | modifier::GUI) != 0 {
            return None;
        }
        let shift = mods & modifier::SHIFT != 0;
        let caps = mods & modifier::CAPS_LOCK != 0;
        let altgr = mods & modifier::RIGHT_ALT != 0;
        if mods & modifier::LEFT_ALT != 0 {
            return None;
        }
        if altgr {
            return match self {
                Layout::Polish => polish_altgr(usage).map(|(lower, upper)| if shift ^ caps { upper } else { lower }),
                Layout::Us => None,
            };
        }
        if (A..=Z).contains(&usage) {
            let c = (b'a' + (usage - A) as u8) as char;
            return Some(if shift ^ caps { c.to_ascii_uppercase() } else { c });
        }
        if let Some(c) = keypad(usage, mods & modifier::NUM_LOCK != 0) {
            return Some(c);
        }
        let (plain, shifted) = us_symbol(usage)?;
        Some(if shift { shifted } else { plain })
    }
}

/// Non-letter keys of the US layout: (unshifted, shifted).
fn us_symbol(usage: u16) -> Option<(char, char)> {
    const DIGITS: [(char, char); 10] =
        [('1', '!'), ('2', '@'), ('3', '#'), ('4', '$'), ('5', '%'), ('6', '^'), ('7', '&'), ('8', '*'), ('9', '('), ('0', ')')];
    Some(match usage {
        DIGIT1..=DIGIT0 => DIGITS[(usage - DIGIT1) as usize],
        SPACE => (' ', ' '),
        MINUS => ('-', '_'),
        EQUAL => ('=', '+'),
        LEFT_BRACKET => ('[', '{'),
        RIGHT_BRACKET => (']', '}'),
        BACKSLASH | NON_US_HASH => ('\\', '|'),
        SEMICOLON => (';', ':'),
        APOSTROPHE => ('\'', '"'),
        GRAVE => ('`', '~'),
        COMMA => (',', '<'),
        PERIOD => ('.', '>'),
        SLASH => ('/', '?'),
        NON_US_BACKSLASH => ('\\', '|'),
        _ => return None,
    })
}

/// Keypad keys: operators always, digits and the decimal point only with
/// Num Lock on (otherwise they are navigation keys).
fn keypad(usage: u16, num_lock: bool) -> Option<char> {
    match usage {
        KP_DIVIDE => Some('/'),
        KP_MULTIPLY => Some('*'),
        KP_MINUS => Some('-'),
        KP_PLUS => Some('+'),
        KP_1..=0x61 if num_lock => Some((b'1' + (usage - KP_1) as u8) as char),
        KP_0 if num_lock => Some('0'),
        KP_PERIOD if num_lock => Some('.'),
        _ => None,
    }
}

/// AltGr letters of the Polish programmer's layout.
fn polish_altgr(usage: u16) -> Option<(char, char)> {
    Some(match usage {
        key::A => ('ą', 'Ą'),
        key::C => ('ć', 'Ć'),
        key::E => ('ę', 'Ę'),
        key::L => ('ł', 'Ł'),
        key::N => ('ń', 'Ń'),
        key::O => ('ó', 'Ó'),
        key::S => ('ś', 'Ś'),
        key::X => ('ź', 'Ź'),
        key::Z => ('ż', 'Ż'),
        _ => return None,
    })
}

/// Keypad navigation meaning with Num Lock off (for editors).
pub fn keypad_navigation(usage: u16) -> Option<u16> {
    Some(match usage {
        0x59 => END,
        0x5A => DOWN,
        0x5B => PAGE_DOWN,
        0x5C => LEFT,
        0x5E => RIGHT,
        0x5F => HOME,
        0x60 => UP,
        0x61 => PAGE_UP,
        KP_0 => INSERT,
        KP_PERIOD => DELETE,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn us_letters_digits_symbols() {
        let us = Layout::Us;
        assert_eq!(us.translate(A, 0), Some('a'));
        assert_eq!(us.translate(Z, modifier::LEFT_SHIFT), Some('Z'));
        assert_eq!(us.translate(DIGIT1, 0), Some('1'));
        assert_eq!(us.translate(DIGIT1, modifier::RIGHT_SHIFT), Some('!'));
        assert_eq!(us.translate(DIGIT0, modifier::LEFT_SHIFT), Some(')'));
        assert_eq!(us.translate(SLASH, modifier::LEFT_SHIFT), Some('?'));
        assert_eq!(us.translate(SPACE, 0), Some(' '));
        assert_eq!(us.translate(ENTER, 0), None, "control keys are not text");
        assert_eq!(us.translate(A, modifier::LEFT_CTRL), None, "shortcuts are not text");
        assert_eq!(us.translate(A, modifier::RIGHT_ALT), None);
    }

    #[test]
    fn caps_lock_affects_letters_only() {
        let us = Layout::Us;
        let caps = modifier::CAPS_LOCK;
        assert_eq!(us.translate(Q, caps), Some('Q'));
        assert_eq!(us.translate(Q, caps | modifier::LEFT_SHIFT), Some('q'));
        assert_eq!(us.translate(DIGIT1, caps), Some('1'));
    }

    #[test]
    fn polish_altgr_letters() {
        let pl = Layout::Polish;
        let altgr = modifier::RIGHT_ALT;
        assert_eq!(pl.translate(A, altgr), Some('ą'));
        assert_eq!(pl.translate(X, altgr), Some('ź'));
        assert_eq!(pl.translate(Z, altgr), Some('ż'));
        assert_eq!(pl.translate(L, altgr | modifier::LEFT_SHIFT), Some('Ł'));
        assert_eq!(pl.translate(S, altgr | modifier::CAPS_LOCK), Some('Ś'));
        assert_eq!(pl.translate(S, altgr | modifier::CAPS_LOCK | modifier::LEFT_SHIFT), Some('ś'));
        assert_eq!(pl.translate(B, altgr), None);
        assert_eq!(pl.translate(B, 0), Some('b'), "without AltGr it is the US layout");
        assert_eq!(pl.translate(A, modifier::LEFT_ALT), None, "left Alt is not AltGr");
    }

    #[test]
    fn keypad_respects_num_lock() {
        let us = Layout::Us;
        assert_eq!(us.translate(KP_1, modifier::NUM_LOCK), Some('1'));
        assert_eq!(us.translate(0x61, modifier::NUM_LOCK), Some('9'));
        assert_eq!(us.translate(KP_0, modifier::NUM_LOCK), Some('0'));
        assert_eq!(us.translate(KP_1, 0), None);
        assert_eq!(keypad_navigation(KP_1), Some(END));
        assert_eq!(us.translate(KP_PLUS, 0), Some('+'));
    }

    #[test]
    fn layout_names() {
        assert_eq!(Layout::from_name("PL"), Some(Layout::Polish));
        assert_eq!(Layout::from_name("us"), Some(Layout::Us));
        assert_eq!(Layout::from_name("de"), None);
        for l in Layout::ALL {
            assert_eq!(Layout::from_name(l.name()), Some(l));
        }
    }
}
