//! USB HID class drivers on top of the xHCI driver: boot protocol
//! keyboards, relative pointing devices (mice) and absolute ones (tablets,
//! touch screens, and the pointer QEMU, VirtualBox and remote consoles
//! present so that the guest's pointer follows the host's).
//!
//! Keyboards are driven in the HID boot protocol (8-byte reports; every
//! boot-subclass keyboard must support it). Report-protocol-only keyboards
//! are not supported yet: their report descriptor would have to be parsed
//! for the key array, which `osyx_usb::hid` is prepared for but this driver
//! does not do. Mice use their report descriptor when it describes
//! relative X/Y (this is how wheels and extra buttons are found), and fall
//! back to the boot protocol (buttons, X, Y) otherwise. A pointer whose
//! report descriptor has absolute X/Y is a tablet: its positions are scaled
//! to the input core's range and the compositor maps them to the screen.
//! Reports go to the input core as raw key, button, motion, position and
//! wheel events.

use crate::input;
use alloc::vec::Vec;
use osyx_usb::hid::{self, BootKeyboard, KeyChange, MouseLayout, ReportError, TabletLayout};

pub enum HidKind {
    Keyboard { state: BootKeyboard, applied_leds: u8 },
    /// `layout` `None`: boot protocol reports.
    Mouse { layout: Option<MouseLayout>, buttons: u32 },
    /// An absolute pointing device.
    Tablet { layout: TabletLayout, buttons: u32 },
}

pub struct HidFunction {
    pub interface: u8,
    pub input_id: u32,
    pub kind: HidKind,
    pub reports: u64,
    pub bad_reports: u64,
}

impl HidFunction {
    /// Decodes one interrupt IN report and forwards its changes.
    pub fn report(&mut self, data: &[u8]) -> Result<(), ReportError> {
        self.reports += 1;
        let r = self.decode(data);
        if r.is_err() {
            self.bad_reports += 1;
        }
        r
    }

    fn decode(&mut self, data: &[u8]) -> Result<(), ReportError> {
        let id = self.input_id;
        match &mut self.kind {
            HidKind::Keyboard { state, .. } => {
                let mut changes: Vec<KeyChange> = Vec::new();
                state.update(data, &mut changes)?;
                for c in changes {
                    input::key(id, c.usage as u16, c.pressed);
                }
            }
            HidKind::Mouse { layout, buttons } => {
                let r = match layout {
                    Some(l) => l.decode(data)?,
                    None => hid::boot_mouse(data)?,
                };
                // Buttons first (a click at the position before the motion
                // of the same report would be wrong either way; this order
                // matches what users expect from drags).
                let changed = r.buttons ^ *buttons;
                for b in 0..32u16 {
                    if changed & (1 << b) != 0 {
                        input::button(id, b + 1, r.buttons & (1 << b) != 0);
                    }
                }
                *buttons = r.buttons;
                input::motion(id, r.dx, r.dy);
                input::wheel(id, r.wheel, r.pan);
            }
            HidKind::Tablet { layout, buttons } => {
                let r = layout.decode(data)?;
                // The position first: a touch reports where it lands in the
                // same report as its tip switch, and the press belongs there.
                let scale = |v: i32| (v as i64 * osyx_input::ABSOLUTE_MAX as i64 / TabletLayout::RANGE as i64) as i32;
                input::absolute(id, scale(r.x), scale(r.y));
                let changed = r.buttons ^ *buttons;
                for b in 0..32u16 {
                    if changed & (1 << b) != 0 {
                        input::button(id, b + 1, r.buttons & (1 << b) != 0);
                    }
                }
                *buttons = r.buttons;
                input::wheel(id, r.wheel, 0);
            }
        }
        Ok(())
    }
}
