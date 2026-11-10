//! Keyboard layouts: key code + modifier state -> text.
//!
//! Layouts are applied by whoever turns key events into text (the UI
//! toolkit), never by the kernel. Available: `us` (US QWERTY), `pl`
//! (Polish "programmer's" layout: US QWERTY plus AltGr letters
//! ąćęłńóśźż), `de` (German QWERTZ), `gb` (United Kingdom) and `fr`
//! (French AZERTY). Ctrl (or Super) combinations produce no text: they are
//! shortcuts. Dead keys (the circumflex and the accents of `de` and `fr`)
//! are not composed: the key types its own character.

use crate::key::{self, *};
use crate::modifier;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    Us,
    Polish,
    German,
    UnitedKingdom,
    French,
}

impl Layout {
    pub const ALL: [Layout; 5] = [Layout::Us, Layout::Polish, Layout::German, Layout::UnitedKingdom, Layout::French];

    /// The layout's short name (case-insensitive); `uk` is `gb` too.
    pub fn from_name(name: &str) -> Option<Layout> {
        Layout::ALL
            .into_iter()
            .find(|l| name.eq_ignore_ascii_case(l.name()))
            .or_else(|| name.eq_ignore_ascii_case("uk").then_some(Layout::UnitedKingdom))
    }

    pub fn name(self) -> &'static str {
        match self {
            Layout::Us => "us",
            Layout::Polish => "pl",
            Layout::German => "de",
            Layout::UnitedKingdom => "gb",
            Layout::French => "fr",
        }
    }

    /// What Settings calls the layout.
    pub fn label(self) -> &'static str {
        match self {
            Layout::Us => "English (US)",
            Layout::Polish => "Polish (Programmer)",
            Layout::German => "German (QWERTZ)",
            Layout::UnitedKingdom => "English (United Kingdom)",
            Layout::French => "French (AZERTY)",
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
        if let Some(c) = keypad(usage, mods & modifier::NUM_LOCK != 0) {
            return if altgr { None } else { Some(c) };
        }
        let table: fn(u16) -> Option<Key> = match self {
            Layout::Us | Layout::Polish => |usage| us_key(usage),
            Layout::German => german_key,
            Layout::UnitedKingdom => united_kingdom_key,
            Layout::French => french_key,
        };
        if altgr {
            let c = match self {
                Layout::Polish => polish_altgr(usage).map(|(lower, _)| lower),
                _ => table(usage).and_then(|k| k.2),
            }?;
            // An AltGr letter has an upper case; an AltGr symbol does not.
            return Some(if c.is_alphabetic() && (shift ^ caps) { upper(c) } else { c });
        }
        let Key(plain, shifted, _) = table(usage)?;
        // Caps Lock is for letters: keys whose shifted character is the
        // capital of the plain one. Elsewhere only Shift counts.
        let capital = if plain.is_alphabetic() && upper(plain) == shifted { shift ^ caps } else { shift };
        Some(if capital { shifted } else { plain })
    }
}

/// One key of a layout: plain, with Shift, with AltGr.
#[derive(Clone, Copy)]
struct Key(char, char, Option<char>);

/// The capital of `c` when it is a single character (`ß` has none).
fn upper(c: char) -> char {
    let mut it = c.to_uppercase();
    match (it.next(), it.next()) {
        (Some(u), None) => u,
        _ => c,
    }
}

/// A letter key: its lower and upper case.
fn letter(c: char) -> Key {
    Key(c, upper(c), None)
}

/// The letter the US layout has on `usage`.
fn us_letter(usage: u16) -> Option<char> {
    (A..=Z).contains(&usage).then(|| (b'a' + (usage - A) as u8) as char)
}

fn us_key(usage: u16) -> Option<Key> {
    if let Some(c) = us_letter(usage) {
        return Some(letter(c));
    }
    us_symbol(usage).map(|(plain, shifted)| Key(plain, shifted, None))
}

/// German (T1): Z and Y change places, umlauts and ß beside the letters,
/// brackets and braces on AltGr.
fn german_key(usage: u16) -> Option<Key> {
    const DIGITS: [(char, char, Option<char>); 10] = [
        ('1', '!', None),
        ('2', '"', Some('²')),
        ('3', '§', Some('³')),
        ('4', '$', None),
        ('5', '%', None),
        ('6', '&', None),
        ('7', '/', Some('{')),
        ('8', '(', Some('[')),
        ('9', ')', Some(']')),
        ('0', '=', Some('}')),
    ];
    Some(match usage {
        key::Y => letter('z'),
        key::Z => letter('y'),
        key::Q => Key('q', 'Q', Some('@')),
        key::E => Key('e', 'E', Some('€')),
        key::M => Key('m', 'M', Some('µ')),
        A..=Z => letter(us_letter(usage)?),
        DIGIT1..=DIGIT0 => {
            let (plain, shifted, altgr) = DIGITS[(usage - DIGIT1) as usize];
            Key(plain, shifted, altgr)
        }
        SPACE => Key(' ', ' ', None),
        GRAVE => Key('^', '°', None),
        MINUS => Key('ß', '?', Some('\\')),
        EQUAL => Key('´', '`', None),
        LEFT_BRACKET => letter('ü'),
        RIGHT_BRACKET => Key('+', '*', Some('~')),
        SEMICOLON => letter('ö'),
        APOSTROPHE => letter('ä'),
        BACKSLASH | NON_US_HASH => Key('#', '\'', None),
        NON_US_BACKSLASH => Key('<', '>', Some('|')),
        COMMA => Key(',', ';', None),
        PERIOD => Key('.', ':', None),
        SLASH => Key('-', '_', None),
        _ => return None,
    })
}

/// United Kingdom: `"` and `@` change places, `£` on 3, `#` beside Enter,
/// the backslash beside the left Shift; AltGr gives `€` and acute vowels.
fn united_kingdom_key(usage: u16) -> Option<Key> {
    Some(match usage {
        key::A => Key('a', 'A', Some('á')),
        key::E => Key('e', 'E', Some('é')),
        key::I => Key('i', 'I', Some('í')),
        key::O => Key('o', 'O', Some('ó')),
        key::U => Key('u', 'U', Some('ú')),
        A..=Z => letter(us_letter(usage)?),
        0x1F => Key('2', '"', None),
        0x20 => Key('3', '£', None),
        0x21 => Key('4', '$', Some('€')),
        GRAVE => Key('`', '¬', Some('¦')),
        APOSTROPHE => Key('\'', '@', None),
        BACKSLASH | NON_US_HASH => Key('#', '~', None),
        NON_US_BACKSLASH => Key('\\', '|', None),
        _ => {
            let (plain, shifted) = us_symbol(usage)?;
            Key(plain, shifted, None)
        }
    })
}

/// French: A/Q and Z/W change places, M sits beside L, the digits need
/// Shift, and AltGr gives the programmer's symbols.
fn french_key(usage: u16) -> Option<Key> {
    const DIGITS: [(char, char, Option<char>); 10] = [
        ('&', '1', None),
        ('é', '2', Some('~')),
        ('"', '3', Some('#')),
        ('\'', '4', Some('{')),
        ('(', '5', Some('[')),
        ('-', '6', Some('|')),
        ('è', '7', Some('`')),
        ('_', '8', Some('\\')),
        ('ç', '9', Some('^')),
        ('à', '0', Some('@')),
    ];
    Some(match usage {
        key::Q => letter('a'),
        key::A => letter('q'),
        key::W => letter('z'),
        key::Z => letter('w'),
        key::E => Key('e', 'E', Some('€')),
        key::M => Key(',', '?', None),
        A..=Z => letter(us_letter(usage)?),
        DIGIT1..=DIGIT0 => {
            let (plain, shifted, altgr) = DIGITS[(usage - DIGIT1) as usize];
            Key(plain, shifted, altgr)
        }
        SPACE => Key(' ', ' ', None),
        GRAVE => Key('²', '²', None),
        MINUS => Key(')', '°', Some(']')),
        EQUAL => Key('=', '+', Some('}')),
        LEFT_BRACKET => Key('^', '¨', None),
        RIGHT_BRACKET => Key('$', '£', Some('¤')),
        SEMICOLON => letter('m'),
        APOSTROPHE => Key('ù', '%', None),
        BACKSLASH | NON_US_HASH => Key('*', 'µ', None),
        NON_US_BACKSLASH => Key('<', '>', None),
        COMMA => Key(';', '.', None),
        PERIOD => Key(':', '/', None),
        SLASH => Key('!', '§', None),
        _ => return None,
    })
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
    fn german_layout() {
        let de = Layout::German;
        let (shift, altgr, caps) = (modifier::LEFT_SHIFT, modifier::RIGHT_ALT, modifier::CAPS_LOCK);
        assert_eq!((de.translate(Y, 0), de.translate(Z, 0)), (Some('z'), Some('y')));
        assert_eq!(de.translate(Y, shift), Some('Z'));
        assert_eq!((de.translate(SEMICOLON, 0), de.translate(APOSTROPHE, shift), de.translate(LEFT_BRACKET, caps)), (Some('ö'), Some('Ä'), Some('Ü')));
        assert_eq!((de.translate(MINUS, 0), de.translate(MINUS, shift), de.translate(MINUS, altgr)), (Some('ß'), Some('?'), Some('\\')));
        assert_eq!(de.translate(MINUS, caps), Some('ß'), "ß has no capital on this key");
        assert_eq!((de.translate(Q, altgr), de.translate(E, altgr)), (Some('@'), Some('€')));
        assert_eq!(de.translate(Q, altgr | shift), Some('@'), "symbols have no upper case");
        assert_eq!((de.translate(0x24, altgr), de.translate(0x25, altgr), de.translate(0x26, altgr), de.translate(DIGIT0, altgr)), (Some('{'), Some('['), Some(']'), Some('}')));
        assert_eq!((de.translate(0x1F, shift), de.translate(0x20, shift), de.translate(0x24, shift)), (Some('"'), Some('§'), Some('/')));
        assert_eq!((de.translate(SLASH, 0), de.translate(SLASH, shift)), (Some('-'), Some('_')));
        assert_eq!((de.translate(NON_US_BACKSLASH, 0), de.translate(NON_US_BACKSLASH, shift), de.translate(NON_US_BACKSLASH, altgr)), (Some('<'), Some('>'), Some('|')));
        assert_eq!(de.translate(DIGIT1, caps), Some('1'), "Caps Lock is for letters");
        assert_eq!(de.translate(B, altgr), None);
        assert_eq!(de.translate(A, modifier::LEFT_CTRL), None);
    }

    #[test]
    fn united_kingdom_layout() {
        let gb = Layout::UnitedKingdom;
        let (shift, altgr) = (modifier::RIGHT_SHIFT, modifier::RIGHT_ALT);
        assert_eq!((gb.translate(0x1F, shift), gb.translate(APOSTROPHE, shift)), (Some('"'), Some('@')));
        assert_eq!((gb.translate(0x20, shift), gb.translate(0x21, altgr)), (Some('£'), Some('€')));
        assert_eq!((gb.translate(NON_US_HASH, 0), gb.translate(NON_US_HASH, shift)), (Some('#'), Some('~')));
        assert_eq!((gb.translate(NON_US_BACKSLASH, 0), gb.translate(NON_US_BACKSLASH, shift)), (Some('\\'), Some('|')));
        assert_eq!((gb.translate(GRAVE, shift), gb.translate(GRAVE, altgr)), (Some('¬'), Some('¦')));
        assert_eq!((gb.translate(E, altgr), gb.translate(E, altgr | shift)), (Some('é'), Some('É')));
        assert_eq!((gb.translate(Q, 0), gb.translate(DIGIT1, shift), gb.translate(SLASH, shift)), (Some('q'), Some('!'), Some('?')));
    }

    #[test]
    fn french_layout() {
        let fr = Layout::French;
        let (shift, altgr, caps) = (modifier::LEFT_SHIFT, modifier::RIGHT_ALT, modifier::CAPS_LOCK);
        assert_eq!((fr.translate(Q, 0), fr.translate(A, 0), fr.translate(W, 0), fr.translate(Z, 0)), (Some('a'), Some('q'), Some('z'), Some('w')));
        assert_eq!((fr.translate(SEMICOLON, 0), fr.translate(SEMICOLON, shift), fr.translate(M, 0), fr.translate(M, shift)), (Some('m'), Some('M'), Some(','), Some('?')));
        assert_eq!((fr.translate(DIGIT1, 0), fr.translate(DIGIT1, shift), fr.translate(0x1F, 0), fr.translate(DIGIT0, 0)), (Some('&'), Some('1'), Some('é'), Some('à')));
        assert_eq!(fr.translate(0x1F, caps), Some('é'), "Caps Lock does not turn é into 2");
        assert_eq!((fr.translate(0x20, altgr), fr.translate(0x21, altgr), fr.translate(0x22, altgr), fr.translate(DIGIT0, altgr)), (Some('#'), Some('{'), Some('['), Some('@')));
        assert_eq!((fr.translate(MINUS, altgr), fr.translate(EQUAL, altgr), fr.translate(E, altgr)), (Some(']'), Some('}'), Some('€')));
        assert_eq!((fr.translate(COMMA, 0), fr.translate(COMMA, shift), fr.translate(PERIOD, 0), fr.translate(PERIOD, shift), fr.translate(SLASH, 0)), (Some(';'), Some('.'), Some(':'), Some('/'), Some('!')));
        assert_eq!((fr.translate(APOSTROPHE, 0), fr.translate(APOSTROPHE, shift)), (Some('ù'), Some('%')));
    }

    #[test]
    fn every_layout_types_the_basics() {
        // Letters, digits, the space and the keypad exist everywhere, and
        // no key of any layout produces a control character.
        for l in Layout::ALL {
            let mut seen = 0u32;
            for c in (A..=Z).chain([SEMICOLON]).filter_map(|u| l.translate(u, 0)).filter(|c| c.is_ascii_lowercase()) {
                seen |= 1 << (c as u8 - b'a');
            }
            assert_eq!(seen, (1 << 26) - 1, "{}: every letter a-z", l.name());
            for d in '0'..='9' {
                let typed = (DIGIT1..=DIGIT0).any(|u| l.translate(u, 0) == Some(d) || l.translate(u, modifier::LEFT_SHIFT) == Some(d));
                assert!(typed, "{}: digit {d}", l.name());
            }
            assert_eq!(l.translate(SPACE, 0), Some(' '));
            assert_eq!(l.translate(KP_1, modifier::NUM_LOCK), Some('1'));
            assert_eq!(l.translate(ENTER, 0), None);
            for usage in 0..0x70u16 {
                for mods in [0, modifier::LEFT_SHIFT, modifier::RIGHT_ALT, modifier::CAPS_LOCK] {
                    assert!(l.translate(usage, mods).is_none_or(|c| !c.is_control()), "{} key {usage:#x}", l.name());
                }
            }
            assert!(!l.label().is_empty());
        }
    }

    #[test]
    fn layout_names() {
        assert_eq!(Layout::from_name("PL"), Some(Layout::Polish));
        assert_eq!(Layout::from_name("us"), Some(Layout::Us));
        assert_eq!(Layout::from_name("de"), Some(Layout::German));
        assert_eq!(Layout::from_name("UK"), Some(Layout::UnitedKingdom));
        assert_eq!(Layout::from_name("xx"), None);
        for l in Layout::ALL {
            assert_eq!(Layout::from_name(l.name()), Some(l));
        }
    }
}
