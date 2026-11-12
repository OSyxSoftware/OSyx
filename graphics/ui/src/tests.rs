extern crate std;

use crate::event::{ClickCounter, Event, KeyEvent, Response};
use crate::focus::FocusChain;
use crate::icon::{self, Icon};
use crate::layout::*;
use crate::paint::{Align, FontKind, Fonts, Painter};
use crate::theme::MIDNIGHT;
use crate::time::{civil_from_days, days_from_civil, Tz};
use crate::widget::*;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use osyx_gfx::{Rect, Surface};
use osyx_input::{button, key, modifier};

fn key_down(k: u16, text: Option<char>, mods: u32) -> Event {
    Event::Key(KeyEvent { key: k, text, mods, down: true, repeat: false })
}

fn typed(c: char) -> Event {
    key_down(0, Some(c), 0)
}

fn click(x: i32, y: i32, clicks: u8) -> [Event; 2] {
    [Event::PointerDown { x, y, button: button::LEFT, clicks }, Event::PointerUp { x, y, button: button::LEFT }]
}

#[test]
fn layout_splits_and_rows() {
    let r = Rect::new(10, 20, 300, 200);
    let (top, rest) = split_top(r, 40);
    assert_eq!(top, Rect::new(10, 20, 300, 40));
    assert_eq!(rest, Rect::new(10, 60, 300, 160));
    let (rest, bottom) = split_bottom(r, 500);
    assert_eq!((rest.h, bottom), (0, r), "oversized splits take everything");
    let [a, b, c] = row(Rect::new(0, 0, 100, 10), [20, 30, 40], 5);
    assert_eq!((a.x, b.x, c.x, c.w), (0, 25, 60, 40));
    let [x, y] = row_right(Rect::new(0, 0, 100, 10), [20, 30], 5);
    assert_eq!((x.x, y.x), (80, 45));
    let cols: Vec<Rect> = columns(Rect::new(0, 0, 100, 10), 3, 5).collect();
    assert_eq!(cols.iter().map(|c| c.x).collect::<Vec<_>>(), [0, 35, 70]);
    assert_eq!(center(Rect::new(0, 0, 100, 100), 20, 10), Rect::new(40, 45, 20, 10));
    assert_eq!(inset(Rect::new(0, 0, 10, 10), 8), Rect::new(8, 8, 0, 0));
    assert!(contains(&r, 10, 20) && !contains(&r, 310, 20));
}

#[test]
fn focus_chain_cycles_with_tab_and_shift_tab() {
    let mut f = FocusChain::new(&[3, 5, 9]);
    assert_eq!(f.focused(), Some(3));
    let tab = KeyEvent { key: key::TAB, text: None, mods: 0, down: true, repeat: false };
    assert!(f.handle_key(&tab));
    assert!(f.is(5));
    assert!(f.handle_key(&KeyEvent { mods: modifier::LEFT_SHIFT, ..tab }));
    assert!(f.handle_key(&KeyEvent { mods: modifier::LEFT_SHIFT, ..tab }));
    assert_eq!(f.focused(), Some(9), "shift+tab wraps backwards");
    assert!(f.set(5) && !f.set(42));
    assert!(!f.handle_key(&KeyEvent { mods: modifier::LEFT_ALT, ..tab }), "alt+tab belongs to PRISM");
    assert_eq!(FocusChain::new(&[]).focused(), None);
}

#[test]
fn click_counter_detects_double_clicks() {
    let mut c = ClickCounter::default();
    assert_eq!(c.press(1000, 10, 10, 1), 1);
    assert_eq!(c.press(1200, 12, 11, 1), 2);
    assert_eq!(c.press(1300, 12, 11, 1), 3);
    assert_eq!(c.press(2000, 12, 11, 1), 1, "too slow");
    assert_eq!(c.press(2100, 40, 11, 1), 1, "moved too far");
    assert_eq!(c.press(2200, 40, 11, 2), 1, "other button");
}

#[test]
fn button_activates_on_release_inside_and_by_keyboard() {
    let mut b = Button::new(7, "OK", ButtonStyle::Primary);
    b.rect = Rect::new(10, 10, 80, 30);
    assert_eq!(b.event(&Event::PointerMove { x: 20, y: 20 }, false), Response::Redraw);
    assert!(b.is_hovered());
    assert_eq!(b.event(&Event::PointerDown { x: 20, y: 20, button: button::LEFT, clicks: 1 }, false), Response::Redraw);
    assert_eq!(b.event(&Event::PointerUp { x: 21, y: 21, button: button::LEFT }, false), Response::Activated(7));
    // Press inside, release outside: no activation.
    b.event(&Event::PointerDown { x: 20, y: 20, button: button::LEFT, clicks: 1 }, false);
    assert_eq!(b.event(&Event::PointerUp { x: 200, y: 20, button: button::LEFT }, false), Response::Redraw);
    assert_eq!(b.event(&key_down(key::ENTER, None, 0), true), Response::Activated(7));
    assert_eq!(b.event(&key_down(key::ENTER, None, 0), false), Response::Ignored, "keys only when focused");
    b.enabled = false;
    for e in click(20, 20, 1) {
        assert!(!matches!(b.event(&e, false), Response::Activated(_)));
    }
}

#[test]
fn checkbox_toggles() {
    let mut c = Checkbox::new("Show hidden files", false);
    c.rect = Rect::new(0, 0, 200, 24);
    let [d, u] = click(5, 5, 1);
    c.event(&d, false);
    assert_eq!(c.event(&u, false), Response::Changed);
    assert!(c.checked);
    assert_eq!(c.event(&key_down(key::SPACE, Some(' '), 0), true), Response::Changed);
    assert!(!c.checked);
}

#[test]
fn text_field_editing() {
    let fonts = Fonts::load();
    let mut f = TextField::new("");
    f.rect = Rect::new(0, 0, 200, 32);
    for c in "zażółć gęślą".chars() {
        assert_eq!(f.event(&typed(c), true, &fonts), Response::Changed);
    }
    assert_eq!(f.text(), "zażółć gęślą");
    f.event(&key_down(key::BACKSPACE, None, 0), true, &fonts);
    assert_eq!(f.text(), "zażółć gęśl", "backspace removes one character, not one byte");
    f.event(&key_down(key::BACKSPACE, None, modifier::LEFT_CTRL), true, &fonts);
    assert_eq!(f.text(), "zażółć ", "ctrl+backspace removes a word");
    f.event(&key_down(key::HOME, None, 0), true, &fonts);
    f.event(&key_down(key::RIGHT, None, modifier::LEFT_SHIFT), true, &fonts);
    f.event(&key_down(key::RIGHT, None, modifier::LEFT_SHIFT), true, &fonts);
    f.event(&key_down(key::RIGHT, None, modifier::LEFT_SHIFT), true, &fonts);
    assert_eq!(f.selection(), Some((0, 4)), "three characters, four bytes");
    f.event(&typed('X'), true, &fonts);
    assert_eq!(f.text(), "Xółć ");
    f.event(&key_down(key::END, None, 0), true, &fonts);
    f.event(&key_down(key::DELETE, None, 0), true, &fonts);
    assert_eq!(f.text(), "Xółć ", "delete at the end does nothing");
    f.event(&key_down(key::A, Some('a'), modifier::LEFT_CTRL), true, &fonts);
    assert_eq!(f.selection(), Some((0, f.text().len())), "ctrl+a selects all and types nothing");
    assert_eq!(f.event(&key_down(key::ENTER, None, 0), true, &fonts), Response::Submitted);
    assert_eq!(f.event(&key_down(key::ESCAPE, None, 0), true, &fonts), Response::Cancelled);
    assert_eq!(f.event(&typed('q'), false, &fonts), Response::Ignored, "unfocused fields ignore keys");
    // Control characters and Ctrl/Alt combinations insert nothing.
    f.set_text("ab");
    f.event(&key_down(key::C, Some('c'), modifier::LEFT_CTRL), true, &fonts);
    f.event(&typed('\u{7}'), true, &fonts);
    assert_eq!(f.text(), "ab");
    f.max_len = 3;
    f.event(&typed('ż'), true, &fonts);
    assert_eq!(f.text(), "ab", "a two-byte character does not fit in one byte");
    f.event(&typed('c'), true, &fonts);
    assert_eq!(f.text(), "abc");
}

#[test]
fn text_field_clipboard_shortcuts_preserve_unicode_and_masked_text() {
    let fonts = Fonts::load();
    let mut clipboard = crate::Clipboard::default();
    let mut field = TextField::new("Zażółć 🌍");
    field.select_all();
    field.event_with_clipboard(&key_down(key::C, Some('c'), modifier::LEFT_CTRL), true, &fonts, &mut clipboard);
    assert_eq!(clipboard.text(), Some("Zażółć 🌍"));
    field.event_with_clipboard(&key_down(key::X, Some('x'), modifier::LEFT_CTRL), true, &fonts, &mut clipboard);
    assert_eq!(field.text(), "");
    field.event_with_clipboard(&key_down(key::V, Some('v'), modifier::LEFT_CTRL), true, &fonts, &mut clipboard);
    assert_eq!(field.text(), "Zażółć 🌍");
    field.masked = true;
    field.select_all();
    clipboard.receive(Some("safe".into()));
    field.event_with_clipboard(&key_down(key::C, Some('c'), modifier::LEFT_CTRL), true, &fonts, &mut clipboard);
    assert_eq!(clipboard.text(), Some("safe"));
}

#[test]
fn text_field_click_places_the_cursor() {
    let fonts = Fonts::load();
    let mut f = TextField::new("hello world");
    f.rect = Rect::new(0, 0, 300, 32);
    let w = fonts.measure(FontKind::Ui, "hello") as i32;
    f.event(&Event::PointerDown { x: 10 + w, y: 10, button: button::LEFT, clicks: 1 }, true, &fonts);
    f.event(&Event::PointerUp { x: 10 + w, y: 10, button: button::LEFT }, true, &fonts);
    assert_eq!(f.cursor(), 5);
    f.event(&Event::PointerDown { x: 12, y: 10, button: button::LEFT, clicks: 2 }, true, &fonts);
    assert_eq!(f.selection(), Some((0, 11)), "double click selects everything");
}

fn list_with(n: usize) -> ListView {
    let mut l = ListView::new(vec![Column::new("Name", 0, Align::Left), Column::new("Size", 80, Align::Right)]);
    l.rect = Rect::new(0, 0, 300, 28 + 30 * 5);
    l.set_items((0..n).map(|i| ListItem::new(Some(Icon::File), &[&std::format!("file{i}"), "1 KiB"])).collect());
    l
}

#[test]
fn list_selection_activation_and_keyboard() {
    let mut l = list_with(20);
    // Row 1 is at y = 28 (header) + 30..60.
    let [d, u] = click(50, 28 + 45, 1);
    assert_eq!(l.event(&d, true), Response::Changed);
    l.event(&u, true);
    assert_eq!(l.selected(), Some(1));
    assert_eq!(l.event(&Event::PointerDown { x: 50, y: 28 + 45, button: button::LEFT, clicks: 2 }, true), Response::Activated(1));
    assert_eq!(l.event(&key_down(key::DOWN, None, 0), true), Response::Changed);
    assert_eq!(l.selected(), Some(2));
    l.event(&key_down(key::END, None, 0), true);
    assert_eq!(l.selected(), Some(19));
    assert!(l.scroll.offset > 0, "the selection is scrolled into view");
    assert_eq!(l.scroll.offset, 20 * 30 - 150);
    assert_eq!(l.event(&key_down(key::ENTER, None, 0), true), Response::Activated(19));
    l.event(&key_down(key::HOME, None, 0), true);
    assert_eq!((l.selected(), l.scroll.offset), (Some(0), 0));
    l.event(&key_down(key::PAGE_DOWN, None, 0), true);
    assert_eq!(l.selected(), Some(5));
    // A click below the last row clears the selection.
    let mut short = list_with(2);
    short.select(Some(1));
    assert_eq!(short.event(&Event::PointerDown { x: 50, y: 28 + 140, button: button::LEFT, clicks: 1 }, true), Response::Changed);
    assert_eq!(short.selected(), None);
}

#[test]
fn list_scrolls_with_the_wheel_and_clamps() {
    let mut l = list_with(20);
    assert_eq!(l.event(&Event::Wheel { x: 50, y: 100, dy: -1 }, false), Response::Redraw);
    assert_eq!(l.scroll.offset, 48);
    for _ in 0..50 {
        l.event(&Event::Wheel { x: 50, y: 100, dy: -1 }, false);
    }
    assert_eq!(l.scroll.offset, l.scroll.max_offset(), "never past the end");
    assert_eq!(l.row_at(50, 28 + 149), Some(19));
    // Replacing the items with fewer rows keeps everything in range.
    l.select(Some(19));
    l.set_items(vec![ListItem::new(None, &["only"])]);
    assert_eq!((l.selected(), l.scroll.offset), (Some(0), 0));
}

#[test]
fn scroll_thumb_drag_maps_to_offset() {
    let mut s = Scroll::default();
    s.set_sizes(1000, 100);
    let track = Rect::new(90, 0, 10, 100);
    let th = s.thumb(track).unwrap();
    assert_eq!(th.h, 24, "minimum thumb size");
    s.event(&Event::PointerDown { x: 95, y: th.y + 5, button: button::LEFT, clicks: 1 }, track, track);
    s.event(&Event::PointerMove { x: 95, y: th.y + 5 + 76 }, track, track);
    assert_eq!(s.offset, 900, "dragging the thumb to the bottom shows the end");
    s.event(&Event::PointerUp { x: 95, y: 0, button: button::LEFT }, track, track);
    assert!(!s.is_dragging());
    assert!(s.ensure_visible(0, 10));
    assert_eq!(s.offset, 0);
    let mut fits = Scroll::default();
    fits.set_sizes(50, 100);
    assert!(fits.thumb(track).is_none());
}

#[test]
fn menu_keyboard_and_pointer() {
    let fonts = Fonts::load();
    let mut items = vec![MenuItem::new("Open Terminal", Some(Icon::Terminal)), MenuItem::new("Disabled", None), MenuItem::new("Files", None)];
    items[1].enabled = false;
    let mut m = Menu::new(items);
    let (w, h) = m.size(&fonts);
    assert!(w >= 180 && h == 3 * 32 + 12);
    m.rect = Rect::new(0, 0, w, h);
    assert_eq!(m.event(&key_down(key::DOWN, None, 0)), Response::Redraw);
    assert_eq!(m.hovered(), Some(0));
    m.event(&key_down(key::DOWN, None, 0));
    assert_eq!(m.hovered(), Some(2), "disabled items are skipped");
    assert_eq!(m.event(&key_down(key::ENTER, None, 0)), Response::Activated(2));
    let [d, u] = click(20, 6 + 16, 1);
    m.event(&d);
    assert_eq!(m.event(&u), Response::Activated(0));
    let [d, u] = click(20, 6 + 32 + 16, 1);
    m.event(&d);
    assert_eq!(m.event(&u), Response::Ignored, "disabled item");
    assert_eq!(m.event(&Event::PointerDown { x: 900, y: 900, button: button::LEFT, clicks: 1 }), Response::Cancelled);
    assert_eq!(m.event(&key_down(key::ESCAPE, None, 0)), Response::Cancelled);
}

#[test]
fn dialog_input_and_buttons() {
    let fonts = Fonts::load();
    let mut d = Dialog::new(1, "New folder", "Name of the new folder:", &[("Cancel", ButtonStyle::Secondary), ("Create", ButtonStyle::Primary)])
        .with_input("Untitled", "Folder name");
    d.layout(Rect::new(0, 0, 640, 480), &fonts);
    assert!(contains(&Rect::new(0, 0, 640, 480), d.rect.x, d.rect.y));
    // The initial text is selected: typing replaces it.
    for c in "Projekty".chars() {
        d.event(&typed(c), &fonts);
    }
    assert_eq!(d.input_text(), "Projekty");
    assert_eq!(d.event(&key_down(key::ENTER, None, 0), &fonts), Response::Activated(1), "Enter = default button");
    assert_eq!(d.event(&key_down(key::ESCAPE, None, 0), &fonts), Response::Cancelled);
    let cancel = d.buttons[0].rect;
    let [down, up] = click(cancel.x + 5, cancel.y + 5, 1);
    d.event(&down, &fonts);
    assert_eq!(d.event(&up, &fonts), Response::Activated(0));
    // Clicks elsewhere in the window are swallowed (modal).
    assert_eq!(d.event(&Event::PointerDown { x: 2, y: 2, button: button::LEFT, clicks: 1 }, &fonts), Response::Consumed);
    // Without an input the default button has the focus.
    let mut q = Dialog::new(2, "Delete?", "x", &[("Cancel", ButtonStyle::Secondary), ("Delete", ButtonStyle::Danger)]);
    q.layout(Rect::new(0, 0, 640, 480), &fonts);
    assert_eq!(q.event(&key_down(key::ENTER, None, 0), &fonts), Response::Activated(1));
    q.event(&key_down(key::TAB, None, 0), &fonts);
    assert_eq!(q.event(&key_down(key::ENTER, None, 0), &fonts), Response::Activated(0), "tab moved to Cancel");
}

#[test]
fn wrap_text_respects_width() {
    let fonts = Fonts::load();
    let lines = wrap_text(&fonts, FontKind::Ui, "The quick brown fox jumps over the lazy dog\nsecond", 120);
    assert!(lines.len() >= 4);
    assert!(lines.iter().all(|l| fonts.measure(FontKind::Ui, l) <= 120));
    assert_eq!(lines.last().map(String::as_str), Some("second"));
    let long = wrap_text(&fonts, FontKind::Ui, "/a/very/long/path/without/any/spaces/at/all", 80);
    assert!(long.len() > 1 && long.iter().all(|l| fonts.measure(FontKind::Ui, l) <= 80));
    assert_eq!(long.concat(), "/a/very/long/path/without/any/spaces/at/all");
    // A long word after other text on the line is cut too.
    let after = wrap_text(&fonts, FontKind::Ui, "Arguments: --a-very-long-option-without-any-spaces-at-all", 120);
    assert!(after.len() > 2 && after.iter().all(|l| fonts.measure(FontKind::Ui, l) <= 120), "{after:?}");
    assert_eq!(after[0], "Arguments:");
    assert_eq!(after[1..].concat(), "--a-very-long-option-without-any-spaces-at-all");
}

#[test]
fn painter_never_draws_outside_its_clip() {
    let fonts = Fonts::load();
    let mut px = vec![0u32; 120 * 80];
    let mut s = Surface::new(&mut px, 120, 80, 120).unwrap();
    let mut p = Painter::new(&mut s, &MIDNIGHT, &fonts);
    let clip = Rect::new(20, 10, 50, 30);
    let old = p.push_clip(clip);
    p.rounded(Rect::new(-10, -10, 200, 200), 12, 0xFFFFFF, 255);
    p.disc(40, 20, 50, 0xFFFFFF, 255);
    p.segment((0, 0), (120 << 8, 80 << 8), 2048, 0xFFFFFF, 255);
    p.text(FontKind::Large, 0, 0, "OSyx PRISM", 0xFFFFFF);
    p.text(FontKind::Mono, 0, 12, "terminal text", 0xFFFFFF);
    p.ring(40, 30, 30, 4, 0xFFFFFF, 255, true);
    icon::draw(&mut p, Icon::Folder, Rect::new(0, 0, 120, 80), 0xFFFFFF);
    p.restore_clip(old);
    for y in 0..80 {
        for x in 0..120 {
            let inside = contains(&clip, x, y);
            let v = s.get(x, y).unwrap();
            assert!(inside || v == 0, "pixel ({x},{y}) drawn outside the clip");
        }
    }
    assert!(s.get(30, 20).unwrap() != 0, "something was drawn inside");
}

#[test]
fn every_icon_stays_in_its_box() {
    let fonts = Fonts::load();
    let all = [
        Icon::Folder,
        Icon::File,
        Icon::TextFile,
        Icon::Program,
        Icon::Terminal,
        Icon::Info,
        Icon::Power,
        Icon::Restart,
        Icon::Apps,
        Icon::Up,
        Icon::NewFolder,
        Icon::Rename,
        Icon::Trash,
        Icon::Refresh,
        Icon::Home,
        Icon::Drive,
        Icon::Close,
        Icon::Bell,
        Icon::Link,
        Icon::Device,
    ];
    for ic in all {
        let mut px = vec![0u32; 64 * 64];
        let mut s = Surface::new(&mut px, 64, 64, 64).unwrap();
        let mut p = Painter::new(&mut s, &MIDNIGHT, &fonts);
        icon::draw(&mut p, ic, Rect::new(16, 16, 32, 32), 0xFFFFFF);
        let mut ink = 0;
        for y in 0..64 {
            for x in 0..64 {
                if s.get(x, y).unwrap() != 0 {
                    ink += 1;
                    assert!((14..50).contains(&x) && (14..50).contains(&y), "{ic:?} drew at ({x},{y})");
                }
            }
        }
        assert!(ink > 20, "{ic:?} is visible");
    }
}

#[test]
fn civil_dates_round_trip() {
    assert_eq!(days_from_civil(1970, 1, 1), 0);
    assert_eq!(civil_from_days(0), (1970, 1, 1));
    assert_eq!(days_from_civil(2000, 3, 1), 11_017);
    assert_eq!(civil_from_days(days_from_civil(2026, 10, 23)), (2026, 10, 23));
    assert_eq!(civil_from_days(days_from_civil(2024, 2, 29)), (2024, 2, 29));
    assert_eq!(crate::time::weekday(days_from_civil(2026, 10, 23)), 5, "a Friday");
    for d in [-1_000_000i64, -1, 59, 60, 365, 20_000, 2_000_000] {
        let (y, m, dd) = civil_from_days(d);
        assert_eq!(days_from_civil(y, m, dd), d);
    }
}

#[test]
fn offered_time_zones_parse_and_have_the_right_offsets() {
    use crate::time::ZONES;
    // 2026-01-15 12:00 UTC and 2026-07-15 12:00 UTC.
    let (winter, summer) = (1_768_478_400, 1_784_116_800);
    let hour = |zone: &str, at: i64| {
        let tz = Tz::parse(zone).unwrap_or_else(|| panic!("{zone} must parse"));
        let l = tz.local(at);
        (l.hour, l.minute)
    };
    for (zone, name) in ZONES {
        assert!(Tz::parse(zone).is_some(), "{zone}");
        assert!(!name.is_empty());
    }
    let unique: std::collections::BTreeSet<&str> = ZONES.iter().map(|(z, _)| *z).collect();
    assert_eq!(unique.len(), ZONES.len(), "no zone is listed twice");
    assert_eq!(hour("CET-1CEST,M3.5.0,M10.5.0/3", winter), (13, 0));
    assert_eq!(hour("CET-1CEST,M3.5.0,M10.5.0/3", summer), (14, 0));
    assert_eq!(hour("EST5EDT,M3.2.0,M11.1.0", winter), (7, 0));
    assert_eq!(hour("EST5EDT,M3.2.0,M11.1.0", summer), (8, 0));
    assert_eq!(hour("IST-5:30", winter), (17, 30));
    assert_eq!(hour("<-03>3", summer), (9, 0));
    assert_eq!(hour("AEST-10AEDT,M10.1.0,M4.1.0/3", winter), (23, 0), "southern summer");
    assert_eq!(hour("AEST-10AEDT,M10.1.0,M4.1.0/3", summer), (22, 0));
    assert_eq!(hour("HST10", summer), (2, 0));
    assert_eq!(hour("NZST-12NZDT,M9.5.0,M4.1.0/3", winter), (1, 0));
}

#[test]
fn clock_formats() {
    let utc = Tz::parse("UTC0").unwrap();
    // 00:05:09, 12:00:00, 14:05:09 and 23:59:59 on the first day.
    let at = |h: i64, m: i64, s: i64| utc.local(h * 3600 + m * 60 + s);
    assert_eq!(at(14, 5, 9).clock(false, false), "14:05");
    assert_eq!(at(14, 5, 9).clock(false, true), "14:05:09");
    assert_eq!(at(14, 5, 9).clock(true, false), "2:05 PM");
    assert_eq!(at(14, 5, 9).clock(true, true), "2:05:09 PM");
    assert_eq!(at(0, 5, 9).clock(true, false), "12:05 AM");
    assert_eq!(at(0, 5, 9).clock(false, false), "00:05");
    assert_eq!(at(12, 0, 0).clock(true, false), "12:00 PM");
    assert_eq!(at(23, 59, 59).clock(true, true), "11:59:59 PM");
    assert_eq!(at(9, 30, 0).clock(false, false), at(9, 30, 0).hm());
}

#[test]
fn posix_time_zones() {
    let warsaw = Tz::parse("CET-1CEST,M3.5.0,M10.5.0/3").unwrap();
    // 2026-10-23 09:00 UTC is 11:00 CEST.
    let t = days_from_civil(2026, 10, 23) * 86_400 + 9 * 3600;
    let l = warsaw.local(t);
    assert_eq!((l.hour, l.minute, l.zone.as_str(), l.offset), (11, 0, "CEST", 7200));
    assert_eq!(l.short_date(), "Fri 23 Oct");
    assert_eq!(l.long_date(), "Friday, 23 October 2026");
    // DST ends 2026-10-25 01:00 UTC (03:00 CEST -> 02:00 CET).
    let end = days_from_civil(2026, 10, 25) * 86_400 + 3600;
    assert_eq!(warsaw.offset_at(end - 1).0, 7200);
    assert_eq!(warsaw.offset_at(end).0, 3600);
    // DST starts 2026-03-29 01:00 UTC.
    let start = days_from_civil(2026, 3, 29) * 86_400 + 3600;
    assert_eq!(warsaw.offset_at(start - 1).1, "CET");
    assert_eq!(warsaw.offset_at(start).1, "CEST");
    assert_eq!(warsaw.local(start).iso(), "2026-03-29 03:00:00");
    // Southern hemisphere: DST across the new year.
    let sydney = Tz::parse("AEST-10AEDT,M10.1.0,M4.1.0/3").unwrap();
    assert_eq!(sydney.offset_at(days_from_civil(2026, 1, 15) * 86_400).1, "AEDT");
    assert_eq!(sydney.offset_at(days_from_civil(2026, 7, 15) * 86_400).1, "AEST");
    let india = Tz::parse("<+0530>-5:30").unwrap();
    assert_eq!(india.local(0).hm(), "05:30");
    assert_eq!(Tz::parse("UTC0").unwrap().local(0).iso(), "1970-01-01 00:00:00");
    for bad in ["", "U", "CET", "CET-1CEST,M13.5.0,M10.5.0", "CET-1CEST,J60,M10.5.0", "CET-200", "CET-1CEST,M3.5.0"] {
        assert!(Tz::parse(bad).is_none(), "{bad:?} must be rejected");
    }
}

#[test]
fn list_marks_for_multi_selection() {
    use crate::widget::{Column, ListItem, ListView};
    let mut l = ListView::new(vec![Column::new("Name", 0, Align::Left)]);
    l.set_items((0..5).map(|i| ListItem::new(None, &[&alloc::format!("row {i}")])).collect());
    assert!(l.marked().is_empty());
    l.select(Some(1));
    assert_eq!(l.chosen(), [1], "without marks the selected row is chosen");
    l.toggle_mark(3);
    l.toggle_mark(4);
    assert_eq!(l.marked(), [3, 4]);
    assert_eq!(l.chosen(), [3, 4]);
    l.toggle_mark(3);
    assert_eq!(l.marked(), [4]);
    l.set_marked(&[0, 2, 9]);
    assert_eq!(l.marked(), [0, 2], "rows past the end are ignored");
    l.mark_all();
    assert_eq!(l.marked().len(), 5);
    l.clear_marks();
    assert!(l.marked().is_empty());
    // New items reset the marks.
    l.mark_all();
    l.set_items(vec![ListItem::new(None, &["only"])]);
    assert!(l.marked().is_empty());
}

/// A 2x2 icon DIB: BITMAPINFOHEADER, `pixels` (bottom row first) and a mask.
fn dib(bpp: u16, pixels: &[u8], mask: [u8; 2]) -> Vec<u8> {
    let mut d = Vec::new();
    d.extend_from_slice(&40u32.to_le_bytes());
    d.extend_from_slice(&2u32.to_le_bytes()); // width
    d.extend_from_slice(&4u32.to_le_bytes()); // height of colour plus mask
    d.extend_from_slice(&1u16.to_le_bytes());
    d.extend_from_slice(&bpp.to_le_bytes());
    d.extend_from_slice(&[0u8; 24]); // BI_RGB, sizes, resolution, colours
    d.extend_from_slice(pixels);
    d.extend_from_slice(&[mask[0], 0, 0, 0, mask[1], 0, 0, 0]); // two 4-byte mask rows
    d
}

#[test]
fn dib_icon_32_bit_keeps_its_alpha() {
    // Bottom row: opaque red, half-transparent green; top row: blue, clear.
    let pixels = [0, 0, 255, 255, 0, 255, 0, 128, 255, 0, 0, 255, 0, 0, 0, 0];
    let b = icon::decode_dib(&dib(32, &pixels, [0, 0])).expect("a 32-bit icon");
    assert_eq!((b.width, b.height), (2, 2));
    assert_eq!(b.pixels, [0xFF00_00FF, 0x0000_0000, 0xFFFF_0000, 0x8000_FF00]);
}

#[test]
fn dib_icon_without_alpha_uses_the_mask() {
    // 24 bits: bottom row white, white; top row black, black; the mask (1 =
    // transparent) clears the top right pixel.
    let pixels = [255, 255, 255, 255, 255, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let b = icon::decode_dib(&dib(24, &pixels, [0, 0b0100_0000])).expect("a 24-bit icon");
    // Rows of 2 pixels pad to 8 bytes; the first 6 bytes of each row count.
    assert_eq!(b.pixels, [0xFF00_0000, 0, 0xFFFF_FFFF, 0xFFFF_FFFF]);
}

#[test]
fn dib_icon_rejects_what_it_cannot_read() {
    assert!(icon::decode_dib(&[]).is_none());
    assert!(icon::decode_dib(b"\x89PNG\r\n\x1a\n").is_none());
    let mut compressed = dib(32, &[0; 16], [0, 0]);
    compressed[16] = 1; // BI_RLE8
    assert!(icon::decode_dib(&compressed).is_none());
    assert!(icon::decode_dib(&dib(32, &[0; 4], [0, 0])).is_none()); // truncated colour data
}

#[test]
fn bitmap_icons_register_and_draw_scaled() {
    assert!(icon::register_bitmap(icon::Bitmap { width: 2, height: 2, pixels: vec![0; 3] }).is_none());
    let red = icon::register_bitmap(icon::Bitmap { width: 2, height: 2, pixels: vec![0xFFFF_0000; 4] }).expect("registered");
    let mut px = vec![0x0000_00FFu32; 8 * 8];
    let mut s = Surface::new(&mut px, 8, 8, 8).unwrap();
    let fonts = Fonts::load();
    let theme = MIDNIGHT;
    let mut p = Painter::new(&mut s, &theme, &fonts);
    icon::draw(&mut p, red, Rect::new(2, 2, 4, 4), 0xFFFFFF);
    assert_eq!(px[3 * 8 + 3], 0x00FF_0000, "inside the box the bitmap's colour");
    assert_eq!(px[0], 0x0000_00FF, "outside it nothing is drawn");
}

/// A PNG with the given colour type and filtered rows (checksums are not read).
fn png(width: u32, height: u32, colour_type: u8, extra: &[(&[u8; 4], &[u8])], filtered: &[u8]) -> Vec<u8> {
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut chunk = |kind: &[u8; 4], body: &[u8]| {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out.extend_from_slice(&[0; 4]);
    };
    let mut header = Vec::new();
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, colour_type, 0, 0, 0]);
    chunk(b"IHDR", &header);
    for (kind, body) in extra {
        chunk(kind, body);
    }
    chunk(b"IDAT", &miniz_oxide::deflate::compress_to_vec_zlib(filtered, 6));
    chunk(b"IEND", &[]);
    out
}

#[test]
fn png_icon_rgba_with_filters() {
    // Row 0 (Sub): red, then red + (0,255,0,0) = yellow; row 1 (Up): the row
    // above plus (0,0,255,0) each = magenta-ish.
    let data = png(2, 2, 6, &[], &[1, 255, 0, 0, 255, 0, 255, 0, 0, 2, 0, 0, 255, 0, 0, 0, 255, 0]);
    let b = icon::decode_image(&data).expect("a PNG");
    assert_eq!((b.width, b.height), (2, 2));
    assert_eq!(b.pixels, [0xFFFF_0000, 0xFFFF_FF00, 0xFFFF_00FF, 0xFFFF_FFFF]);
}

#[test]
fn png_icon_indexed_with_transparency() {
    let palette = [10, 20, 30, 200, 100, 50];
    let data = png(2, 1, 3, &[(b"PLTE", &palette), (b"tRNS", &[255, 0])], &[0, 0, 1]);
    let b = icon::decode_png(&data).expect("an indexed PNG");
    assert_eq!(b.pixels, [0xFF0A_141E, 0x00C8_6432]);
}

#[test]
fn png_icon_refuses_what_it_does_not_support() {
    assert!(icon::decode_png(&[]).is_none());
    assert!(icon::decode_png(&png(0, 1, 6, &[], &[0])).is_none());
    assert!(icon::decode_png(&png(1, 1, 6, &[], &[0, 1, 2])).is_none()); // too little data
    assert!(icon::decode_png(&png(300, 1, 2, &[], &[0])).is_none()); // larger than an icon
    assert!(icon::decode_png(&png(1, 1, 6, &[], &[9, 0, 0, 0, 0])).is_none()); // unknown filter
}

// ----- TextArea -------------------------------------------------------------

fn area(text: &str) -> (TextArea, Fonts, crate::Clipboard) {
    let fonts = Fonts::load();
    let mut a = TextArea::new(text);
    a.rect = Rect::new(0, 0, 600, 300);
    a.layout(&fonts);
    (a, fonts, crate::Clipboard::default())
}

#[test]
fn text_area_finds_text_around_letters_whose_case_changes_their_length() {
    // Lower-casing the first letter makes it one byte longer and the third
    // one byte shorter: byte offsets of the folded text are not the line's.
    let (mut a, fonts, _) = area("\u{23a}\u{e9}\u{23a}K and \u{130}x\u{e9}");
    assert!(a.find("\u{e9}", true));
    assert_eq!(a.selected_text().as_deref(), Some("\u{e9}"));
    assert_eq!(a.cursor(), Pos::new(0, 2));
    assert!(a.find("\u{e9}", true));
    assert_eq!(a.cursor(), Pos::new(0, 12));
    assert!(a.find("k and", true));
    assert_eq!(a.selected_text().as_deref(), Some("K and"));
    // Drawing marks the matches without slicing inside a character either.
    let mut buf = vec![0u32; 400 * 100];
    let mut s = Surface::new(&mut buf, 400, 100, 400).unwrap();
    a.draw(&mut Painter::new(&mut s, &MIDNIGHT, &fonts), true);
}

#[test]
fn text_area_draws_a_very_long_line_by_its_visible_part() {
    let long: String = "ab\tcd ".repeat(200_000);
    let (mut a, fonts, _) = area(&long);
    a.move_to(Pos::new(0, 700_000), false);
    a.reveal_cursor(&fonts);
    let mut buf = vec![0u32; 400 * 100];
    let mut s = Surface::new(&mut buf, 400, 100, 400).unwrap();
    let started = std::time::Instant::now();
    for _ in 0..20 {
        a.draw(&mut Painter::new(&mut s, &MIDNIGHT, &fonts), true);
    }
    // Twenty frames of a 1.2 MB line.
    assert!(started.elapsed().as_millis() < 3000, "{} ms", started.elapsed().as_millis());
}

#[test]
fn text_area_keeps_its_screen_cell_and_tab_stops_with_tabs() {
    // Column 2 of the first line is screen cell 5: Down twice stays there.
    let (mut a, fonts, mut clip) = area("\tx\nabcdef\nabcdef\nab\nabcdef");
    a.move_to(Pos::new(0, 2), false);
    a.event(&key_down(key::DOWN, None, 0), &fonts, &mut clip);
    assert_eq!(a.cursor(), Pos::new(1, 5));
    a.event(&key_down(key::DOWN, None, 0), &fonts, &mut clip);
    assert_eq!(a.cursor(), Pos::new(2, 5));
    // A short line on the way does not change the cell aimed for.
    a.event(&key_down(key::DOWN, None, 0), &fonts, &mut clip);
    assert_eq!(a.cursor(), Pos::new(3, 2));
    a.event(&key_down(key::DOWN, None, 0), &fonts, &mut clip);
    assert_eq!(a.cursor(), Pos::new(4, 5));
    // Tab after a tab and a letter (cell 5) goes to cell 8: three spaces.
    a.move_to(Pos::new(0, 2), false);
    a.event(&key_down(key::TAB, None, 0), &fonts, &mut clip);
    assert_eq!(a.line(0), "\tx   ");
}

#[test]
fn text_area_read_only_keeps_history_and_long_words_move_fast() {
    let (mut a, fonts, mut clip) = area("");
    a.event(&typed('x'), &fonts, &mut clip);
    a.read_only = true;
    assert!(!a.undo());
    assert_eq!(a.text(), "x");
    a.read_only = false;
    assert!(a.undo());
    a.read_only = true;
    assert!(!a.redo());
    assert_eq!(a.text(), "");
    // One word of 300 000 letters, then another line: each move reads
    // the line once.
    let long = "w".repeat(300_000);
    let (mut a, fonts, mut clip) = area(&alloc::format!("  {long}  next\nlast"));
    let started = std::time::Instant::now();
    a.event(&key_down(key::RIGHT, None, modifier::LEFT_CTRL), &fonts, &mut clip);
    assert_eq!(a.cursor(), Pos::new(0, 2));
    a.event(&key_down(key::RIGHT, None, modifier::LEFT_CTRL), &fonts, &mut clip);
    assert_eq!(a.cursor(), Pos::new(0, 300_004));
    a.event(&key_down(key::RIGHT, None, modifier::LEFT_CTRL), &fonts, &mut clip);
    assert_eq!(a.cursor(), Pos::new(1, 0));
    a.event(&key_down(key::LEFT, None, modifier::LEFT_CTRL), &fonts, &mut clip);
    assert_eq!(a.cursor(), Pos::new(0, 300_004));
    a.event(&key_down(key::LEFT, None, modifier::LEFT_CTRL), &fonts, &mut clip);
    assert_eq!(a.cursor(), Pos::new(0, 2));
    a.event(&key_down(key::LEFT, None, modifier::LEFT_CTRL), &fonts, &mut clip);
    assert_eq!(a.cursor(), Pos::new(0, 0));
    assert!(started.elapsed().as_millis() < 3000, "{} ms", started.elapsed().as_millis());
    // A line of one letter 100 000 times, every one a match: drawn by its
    // visible part.
    let (mut a, fonts, _) = area(&"a".repeat(100_000));
    assert!(a.find("a", true));
    let mut buf = vec![0u32; 400 * 100];
    let mut s = Surface::new(&mut buf, 400, 100, 400).unwrap();
    let started = std::time::Instant::now();
    a.draw(&mut Painter::new(&mut s, &MIDNIGHT, &fonts), true);
    assert!(started.elapsed().as_millis() < 3000, "{} ms", started.elapsed().as_millis());
}

#[test]
fn text_area_typing_lines_and_unicode() {
    let (mut a, fonts, mut clip) = area("");
    for c in "zażółć".chars() {
        assert_eq!(a.event(&typed(c), &fonts, &mut clip), Response::Changed);
    }
    a.event(&key_down(key::ENTER, None, 0), &fonts, &mut clip);
    for c in "gęślą".chars() {
        a.event(&typed(c), &fonts, &mut clip);
    }
    assert_eq!(a.text(), "zażółć\ngęślą");
    assert_eq!((a.line_count(), a.cursor()), (2, Pos::new(1, 5)));
    // Backspace at the start of a line joins it to the one above.
    a.event(&key_down(key::HOME, None, 0), &fonts, &mut clip);
    a.event(&key_down(key::BACKSPACE, None, 0), &fonts, &mut clip);
    assert_eq!((a.text().as_str(), a.cursor()), ("zażółćgęślą", Pos::new(0, 6)));
    // Delete takes one character, not one byte.
    a.event(&key_down(key::DELETE, None, 0), &fonts, &mut clip);
    assert_eq!(a.text(), "zażółćęślą");
    assert!(a.is_modified());
    a.mark_saved();
    assert!(!a.is_modified());
}

#[test]
fn text_area_enter_keeps_the_indentation_and_tab_goes_to_the_next_stop() {
    let (mut a, fonts, mut clip) = area("    if x {");
    a.event(&key_down(key::END, None, 0), &fonts, &mut clip);
    a.event(&key_down(key::ENTER, None, 0), &fonts, &mut clip);
    assert_eq!(a.text(), "    if x {\n    ");
    a.event(&typed('a'), &fonts, &mut clip);
    a.event(&key_down(key::TAB, None, 0), &fonts, &mut clip);
    assert_eq!(a.line(1), "    a   ", "from column 5 to column 8");
    // Backspace in the indentation removes a whole stop.
    let (mut b, fonts, mut clip) = area("        x");
    b.move_to(Pos::new(0, 8), false);
    b.event(&key_down(key::BACKSPACE, None, 0), &fonts, &mut clip);
    assert_eq!(b.text(), "    x");
    // Home goes to the text first, then to the margin.
    b.event(&key_down(key::END, None, 0), &fonts, &mut clip);
    b.event(&key_down(key::HOME, None, 0), &fonts, &mut clip);
    assert_eq!(b.cursor(), Pos::new(0, 4));
    b.event(&key_down(key::HOME, None, 0), &fonts, &mut clip);
    assert_eq!(b.cursor(), Pos::new(0, 0));
}

#[test]
fn text_area_selection_across_lines_and_the_clipboard() {
    let (mut a, fonts, mut clip) = area("one\ntwo\nthree");
    a.move_to(Pos::new(0, 1), false);
    a.event(&key_down(key::DOWN, None, modifier::LEFT_SHIFT), &fonts, &mut clip);
    a.event(&key_down(key::DOWN, None, modifier::LEFT_SHIFT), &fonts, &mut clip);
    a.event(&key_down(key::RIGHT, None, modifier::LEFT_SHIFT), &fonts, &mut clip);
    assert_eq!(a.selection(), Some((Pos::new(0, 1), Pos::new(2, 2))));
    assert_eq!(a.selected_text().as_deref(), Some("ne\ntwo\nth"));
    assert_eq!(a.event(&key_down(key::C, Some('c'), modifier::LEFT_CTRL), &fonts, &mut clip), Response::Consumed);
    assert_eq!(clip.text(), Some("ne\ntwo\nth"));
    a.event(&key_down(key::X, Some('x'), modifier::LEFT_CTRL), &fonts, &mut clip);
    assert_eq!((a.text().as_str(), a.cursor()), ("oree", Pos::new(0, 1)));
    // Pasting text with Windows line ends gives plain lines.
    clip.receive(Some("A\r\nB".into()));
    a.event(&key_down(key::V, Some('v'), modifier::LEFT_CTRL), &fonts, &mut clip);
    assert_eq!((a.text().as_str(), a.cursor()), ("oA\nBree", Pos::new(1, 1)));
    a.event(&key_down(key::A, Some('a'), modifier::LEFT_CTRL), &fonts, &mut clip);
    a.event(&typed('!'), &fonts, &mut clip);
    assert_eq!(a.text(), "!");
}

#[test]
fn text_area_undo_and_redo() {
    let (mut a, fonts, mut clip) = area("start");
    a.event(&key_down(key::END, None, 0), &fonts, &mut clip);
    for c in " one".chars() {
        a.event(&typed(c), &fonts, &mut clip);
    }
    a.event(&key_down(key::ENTER, None, 0), &fonts, &mut clip);
    for c in "two".chars() {
        a.event(&typed(c), &fonts, &mut clip);
    }
    assert_eq!(a.text(), "start one\ntwo");
    // Characters typed in a row are one step; the line break is its own.
    let undo = key_down(key::Z, Some('z'), modifier::LEFT_CTRL);
    a.event(&undo, &fonts, &mut clip);
    assert_eq!(a.text(), "start one\n");
    a.event(&undo, &fonts, &mut clip);
    assert_eq!(a.text(), "start one");
    a.event(&undo, &fonts, &mut clip);
    assert_eq!((a.text().as_str(), a.cursor()), ("start", Pos::new(0, 5)));
    assert!(!a.is_modified(), "back at the loaded text");
    assert_eq!(a.event(&undo, &fonts, &mut clip), Response::Consumed, "nothing left to undo");
    let redo = key_down(key::Y, Some('y'), modifier::LEFT_CTRL);
    a.event(&redo, &fonts, &mut clip);
    a.event(&key_down(key::Z, Some('Z'), modifier::LEFT_CTRL | modifier::LEFT_SHIFT), &fonts, &mut clip);
    assert_eq!(a.text(), "start one\n");
    // A new edit drops what could be redone.
    a.event(&typed('x'), &fonts, &mut clip);
    assert!(!a.can_redo());
    assert_eq!(a.text(), "start one\nx");
    // Undoing a deletion of several lines restores them and the cursor.
    let (mut b, fonts, mut clip) = area("a\nb\nc\nd");
    b.move_to(Pos::new(0, 1), false);
    b.move_to(Pos::new(3, 0), true);
    b.event(&key_down(key::DELETE, None, 0), &fonts, &mut clip);
    assert_eq!(b.text(), "ad");
    b.event(&undo, &fonts, &mut clip);
    assert_eq!((b.text().as_str(), b.cursor()), ("a\nb\nc\nd", Pos::new(3, 0)));
    // Saved, changed, undone back to the save: not modified.
    b.event(&typed('q'), &fonts, &mut clip);
    b.mark_saved();
    b.event(&typed('r'), &fonts, &mut clip);
    assert!(b.is_modified());
    b.event(&undo, &fonts, &mut clip);
    assert!(!b.is_modified());
    assert_eq!(b.text(), "a\nb\nc\nqd");
    b.event(&undo, &fonts, &mut clip);
    assert!(b.is_modified(), "before the save is a change too");
}

#[test]
fn text_area_word_movement_and_vertical_columns() {
    let (mut a, fonts, mut clip) = area("alpha beta_2, gamma\nxy\nlonger line here");
    a.event(&key_down(key::RIGHT, None, modifier::LEFT_CTRL), &fonts, &mut clip);
    assert_eq!(a.cursor(), Pos::new(0, 6));
    a.event(&key_down(key::RIGHT, None, modifier::LEFT_CTRL), &fonts, &mut clip);
    assert_eq!(a.cursor(), Pos::new(0, 14), "over the word and the punctuation after it");
    a.event(&key_down(key::LEFT, None, modifier::LEFT_CTRL), &fonts, &mut clip);
    assert_eq!(a.cursor(), Pos::new(0, 6));
    a.event(&key_down(key::BACKSPACE, None, modifier::LEFT_CTRL), &fonts, &mut clip);
    assert_eq!(a.line(0), "beta_2, gamma");
    // Down through a short line and on: the column aimed for is kept.
    a.event(&key_down(key::END, None, 0), &fonts, &mut clip);
    a.event(&key_down(key::DOWN, None, 0), &fonts, &mut clip);
    assert_eq!(a.cursor(), Pos::new(1, 2));
    a.event(&key_down(key::DOWN, None, 0), &fonts, &mut clip);
    assert_eq!(a.cursor(), Pos::new(2, 13));
    a.event(&key_down(key::HOME, None, modifier::LEFT_CTRL), &fonts, &mut clip);
    assert_eq!(a.cursor(), Pos::new(0, 0));
    a.event(&key_down(key::END, None, modifier::LEFT_CTRL | modifier::LEFT_SHIFT), &fonts, &mut clip);
    assert_eq!(a.selection(), Some((Pos::new(0, 0), Pos::new(2, 16))));
    // Down through a tab that puts the cursor to the right of the cell
    // aimed for: the cell is kept, not the place the tab gave.
    let (mut a, fonts, mut clip) = area("ab\n\t\nabcdef");
    a.event(&key_down(key::END, None, 0), &fonts, &mut clip);
    a.event(&key_down(key::DOWN, None, 0), &fonts, &mut clip);
    assert_eq!(a.cursor().line, 1);
    a.event(&key_down(key::DOWN, None, 0), &fonts, &mut clip);
    assert_eq!(a.cursor(), Pos::new(2, 2));
    // A move sideways that ends where it began still gives up the cell
    // aimed for: the cursor's own cell counts from then on.
    let (mut a, fonts, mut clip) = area("ab\n\t\nabcdef");
    a.event(&key_down(key::END, None, 0), &fonts, &mut clip);
    a.event(&key_down(key::DOWN, None, 0), &fonts, &mut clip);
    assert_eq!(a.cursor(), Pos::new(1, 1));
    a.event(&key_down(key::LEFT, None, 0), &fonts, &mut clip);
    a.event(&key_down(key::RIGHT, None, 0), &fonts, &mut clip);
    a.event(&key_down(key::DOWN, None, 0), &fonts, &mut clip);
    assert_eq!(a.cursor(), Pos::new(2, 4));
}

#[test]
fn text_area_pointer_places_selects_and_scrolls() {
    let (mut a, fonts, mut clip) = area("first line\nsecond word here\nthird");
    a.line_numbers = false;
    let (cw, lh) = (fonts.mono.cell_width() as i32, fonts.line_height(FontKind::Mono) as i32);
    let at = |col: i32, line: i32| (6 + col * cw + 1, line * lh + lh / 2);
    let (x, y) = at(7, 1);
    for e in click(x, y, 1) {
        a.event(&e, &fonts, &mut clip);
    }
    assert_eq!((a.cursor(), a.selection()), (Pos::new(1, 7), None));
    // Double click: the word; triple: the line.
    for e in click(x, y, 2) {
        a.event(&e, &fonts, &mut clip);
    }
    assert_eq!(a.selected_text().as_deref(), Some("word"));
    for e in click(x, y, 3) {
        a.event(&e, &fonts, &mut clip);
    }
    assert_eq!(a.selected_text().as_deref(), Some("second word here"));
    // Press, drag to another line, release.
    let (x0, y0) = at(2, 0);
    let (x1, y1) = at(3, 2);
    a.event(&Event::PointerDown { x: x0, y: y0, button: button::LEFT, clicks: 1 }, &fonts, &mut clip);
    a.event(&Event::PointerMove { x: x1, y: y1 }, &fonts, &mut clip);
    a.event(&Event::PointerUp { x: x1, y: y1, button: button::LEFT }, &fonts, &mut clip);
    assert_eq!(a.selection(), Some((Pos::new(0, 2), Pos::new(2, 3))));
    // A click past the end of a line lands at its end.
    for e in click(500, lh * 2 + 2, 1) {
        a.event(&e, &fonts, &mut clip);
    }
    assert_eq!(a.cursor(), Pos::new(2, 5));
    // A long text: the cursor is kept in view.
    let many: String = (0..200).map(|i| alloc::format!("line {i}\n")).collect();
    let (mut b, fonts, mut clip) = area(&many);
    b.event(&key_down(key::END, None, modifier::LEFT_CTRL), &fonts, &mut clip);
    assert!(b.scroll.offset > 0 && b.scroll.offset as i32 + 300 >= 200 * lh, "scrolled to the last line");
    b.event(&key_down(key::PAGE_UP, None, 0), &fonts, &mut clip);
    assert_eq!(b.cursor().line, 200 - (300 / lh as usize - 1));
    assert_eq!(b.event(&Event::Wheel { x: 10, y: 10, dy: 3 }, &fonts, &mut clip), Response::Redraw);
}

#[test]
fn text_area_find_wraps_and_ignores_case_for_lower_case_needles() {
    let (mut a, _fonts, _clip) = area("Echo one\necho two\nprint(\"echo\")");
    assert!(a.find("echo", true));
    assert_eq!(a.selection(), Some((Pos::new(0, 0), Pos::new(0, 4))), "lower case finds Echo");
    assert!(a.find("echo", true));
    assert_eq!(a.selection().map(|s| s.0), Some(Pos::new(1, 0)));
    assert!(a.find("echo", true));
    assert_eq!(a.selection().map(|s| s.0), Some(Pos::new(2, 7)));
    assert!(a.find("echo", true));
    assert_eq!(a.selection().map(|s| s.0), Some(Pos::new(0, 0)), "wraps to the top");
    assert!(a.find("echo", false));
    assert_eq!(a.selection().map(|s| s.0), Some(Pos::new(2, 7)), "backwards wraps to the bottom");
    assert!(a.find("Echo", true));
    assert_eq!(a.selection().map(|s| s.0), Some(Pos::new(0, 0)), "a capital letter makes it exact");
    assert!(a.find("Echo", true));
    assert_eq!(a.selection().map(|s| s.0), Some(Pos::new(0, 0)), "the only exact match again");
    assert!(!a.find("absent", true));
    assert!(!a.find("", true));
}

#[test]
fn text_area_read_only_and_drawing() {
    let (mut a, fonts, mut clip) = area("fixed\ttab\nsecond");
    a.read_only = true;
    a.event(&typed('x'), &fonts, &mut clip);
    a.event(&key_down(key::BACKSPACE, None, 0), &fonts, &mut clip);
    assert_eq!(a.text(), "fixed\ttab\nsecond");
    a.event(&key_down(key::A, Some('a'), modifier::LEFT_CTRL), &fonts, &mut clip);
    a.event(&key_down(key::C, Some('c'), modifier::LEFT_CTRL), &fonts, &mut clip);
    assert_eq!(clip.text(), Some("fixed\ttab\nsecond"), "copying still works");
    // Drawing: text pixels appear, a highlighter's colour is used, and the
    // character after a tab starts at the tab stop.
    fn all_keyword(line: &str) -> Vec<(usize, usize, Style)> {
        vec![(0, line.chars().count().min(5), Style::Keyword)]
    }
    a.highlighter = Some(all_keyword);
    a.line_numbers = false;
    a.move_to(Pos::new(1, 0), false);
    let mut px = vec![0u32; 600 * 300];
    let mut s = Surface::new(&mut px, 600, 300, 600).unwrap();
    let mut p = Painter::new(&mut s, &MIDNIGHT, &fonts);
    a.draw(&mut p, true);
    let (cw, lh) = (fonts.mono.cell_width() as usize, fonts.line_height(FontKind::Mono) as usize);
    let cell = |px: &[u32], col: usize, line: usize| -> Vec<u32> {
        (0..lh).flat_map(|y| (0..cw).map(move |x| (x, y))).map(|(x, y)| px[(line * lh + y) * 600 + 6 + col * cw + x]).collect()
    };
    let keyword = Style::Keyword.color(&MIDNIGHT);
    let plain = Style::Plain.color(&MIDNIGHT);
    assert!(cell(&px, 0, 0).contains(&keyword), "the first word in the keyword colour");
    assert!(!cell(&px, 5, 0).contains(&plain) && !cell(&px, 6, 0).contains(&plain), "the tab is a gap");
    assert!(cell(&px, 8, 0).contains(&plain), "'t' after the tab at column 8");
    assert!(cell(&px, 0, 1).contains(&MIDNIGHT.accent), "the cursor on the second line");
}

// ----- themes and the sun ---------------------------------------------------

#[test]
fn sunrise_and_sunset_match_an_almanac_within_minutes() {
    use crate::time::{sun_is_up, sun_times, Sun};
    // (place, day of year, latitude, longitude, sunrise UTC, sunset UTC),
    // times as hours and minutes approximated from an almanac.
    let cases = [
        ("Warsaw, 21 June", 172, 52.23, 21.01, (2, 14), (19, 1)),
        ("Warsaw, 21 December", 355, 52.23, 21.01, (6, 43), (14, 25)),
        ("Warsaw, 20 March", 79, 52.23, 21.01, (4, 39), (16, 49)),
        ("Quito, 21 June", 172, -0.18, -78.47, (11, 12), (23, 19)),
        ("Sydney, 21 December", 355, -33.87, 151.21, (18, 41), (9, 5)),
    ];
    for (place, day, lat, lon, rise, set) in cases {
        let Sun::RisesAndSets(r, s) = sun_times(day, lat, lon) else { panic!("{place}: no sunrise") };
        let minutes = |(h, m): (i32, i32)| (h * 60 + m) as f64;
        // Sydney's day starts on the previous UTC day.
        let near = |got: f64, want: f64| [-1440.0, 0.0, 1440.0].iter().any(|k| (got + k - want).abs() <= 6.0);
        assert!(near(r, minutes(rise)), "{place}: sunrise at {:.0} min, table {:.0}", r, minutes(rise));
        assert!(near(s, minutes(set)), "{place}: sunset at {:.0} min, table {:.0}", s, minutes(set));
    }
    assert_eq!(sun_times(172, 78.0, 15.0), Sun::AlwaysUp, "midnight sun on Svalbard");
    assert_eq!(sun_times(355, 78.0, 15.0), Sun::AlwaysDown, "polar night");
    // 2026-06-21 12:00 UTC and 23:00 UTC in Warsaw; 02:00 UTC is day in Sydney's summer.
    let noon = days_from_civil(2026, 6, 21) * 86_400 + 12 * 3600;
    assert!(sun_is_up(noon, 52.23, 21.01));
    assert!(!sun_is_up(noon + 11 * 3600, 52.23, 21.01));
    let december = days_from_civil(2026, 12, 21) * 86_400 + 2 * 3600;
    assert!(sun_is_up(december, -33.87, 151.21), "the Sydney day that began on the previous UTC date");
    assert!(!sun_is_up(december, 52.23, 21.01));
}

#[test]
fn theme_choices_parse_and_follow_the_sun() {
    use crate::theme::{Scheme, SchemeChoice, DAYLIGHT};
    assert_eq!(SchemeChoice::parse("light\n"), SchemeChoice::Light);
    assert_eq!(SchemeChoice::parse("auto"), SchemeChoice::Automatic);
    assert_eq!(SchemeChoice::parse(""), SchemeChoice::Dark);
    assert_eq!(SchemeChoice::parse("nonsense"), SchemeChoice::Dark);
    for c in [SchemeChoice::Dark, SchemeChoice::Light, SchemeChoice::Automatic] {
        assert_eq!(SchemeChoice::parse(c.name()), c);
    }
    let noon = days_from_civil(2026, 6, 21) * 86_400 + 12 * 3600;
    assert_eq!(SchemeChoice::Automatic.at(noon, 52.23, 21.01), Scheme::Light);
    assert_eq!(SchemeChoice::Automatic.at(noon + 11 * 3600, 52.23, 21.01), Scheme::Dark);
    assert_eq!(SchemeChoice::Dark.at(noon, 52.23, 21.01), Scheme::Dark);
    assert!(DAYLIGHT.is_light() && !MIDNIGHT.is_light());
    assert_eq!(crate::Theme::of(Scheme::Light), DAYLIGHT);
    // Text stays readable on every surface of both themes.
    for t in [MIDNIGHT, DAYLIGHT] {
        let lum = |c: crate::Color| (((c >> 16) & 255) * 3 + ((c >> 8) & 255) * 6 + (c & 255)) as i32;
        for bg in [t.window, t.surface, t.raised, t.field, t.selection] {
            assert!((lum(t.text) - lum(bg)).abs() > 1200, "text on {bg:06x}");
            assert!((lum(t.text_dim) - lum(bg)).abs() > 600, "dim text on {bg:06x}");
        }
        assert!((lum(t.on_accent) - lum(t.accent)).abs() > 700);
    }
}
