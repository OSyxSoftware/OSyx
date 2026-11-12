//! USB HID class drivers on top of the xHCI and EHCI drivers: boot protocol
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

use crate::{input, kprintln};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use osyx_usb::descriptor::{self, Interface, SetupPacket};
use osyx_usb::hid::parse_report_descriptor;
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
        if let Err(e) = r {
            self.bad_reports += 1;
            // The first few say what the device sends; a report with
            // another function's id, or a keyboard's rollover, is expected and
            // not one of them.
            if !matches!(e, ReportError::UnknownReportId | ReportError::Rollover) && self.bad_reports <= 4 {
                kprintln!("usb input device {}: report not understood ({:?}): {:02x?}", self.input_id, e, &data[..data.len().min(16)]);
            }
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

/// The default control endpoint of a device, whatever host controller
/// drives it.
pub trait ControlPipe {
    type Error: core::fmt::Display;
    fn control_in(&mut self, setup: SetupPacket, buf: &mut [u8]) -> Result<usize, Self::Error>;
    fn control_out(&mut self, setup: SetupPacket, data: &[u8]) -> Result<(), Self::Error>;
}

/// Prepares a HID interface of a configured device: protocol, idle rate,
/// report layout; registers it with the input core. `None` for an
/// interface that is neither a boot keyboard nor a pointer.
pub fn setup<P: ControlPipe>(pipe: &mut P, iface: &Interface, name: &str, devname: &str) -> Result<Option<HidFunction>, P::Error> {
    let boot = iface.subclass == descriptor::HID_SUBCLASS_BOOT;
    if iface.is_hid_boot(descriptor::HID_PROTOCOL_KEYBOARD) {
        pipe.control_out(SetupPacket::hid_set_protocol(iface.number, false), &[])?;
        // Report only on change; some keyboards stall this request.
        let _ = pipe.control_out(SetupPacket::hid_set_idle(iface.number, 0), &[]);
        let id = input::register(name, input::class::KEYBOARD);
        kprintln!("usb {}: HID boot keyboard on interface {} -> input device {}", devname, iface.number, id);
        return Ok(Some(HidFunction {
            interface: iface.number,
            input_id: id,
            kind: HidKind::Keyboard { state: BootKeyboard::new(), applied_leds: 0 },
            reports: 0,
            bad_reports: 0,
        }));
    }
    // A pointer: prefer the report descriptor (wheel, more buttons).
    let mut layout = None;
    let mut tablet = None;
    // (Gaming mice describe their extra functions at length; both host
    // controller drivers carry 2 KiB in a control transfer.)
    let len = iface.hid.map_or(0, |h| h.report_length).min(2048);
    if len > 0 {
        let mut rd = vec![0u8; len as usize];
        match pipe.control_in(SetupPacket::get_interface_descriptor(descriptor::DESC_HID_REPORT, iface.number, len), &mut rd) {
            Ok(n) => match parse_report_descriptor(&rd[..n]) {
                Ok(fields) => {
                    layout = MouseLayout::from_fields(&fields);
                    if layout.is_none() {
                        tablet = TabletLayout::from_fields(&fields);
                    }
                }
                Err(e) => kprintln!("usb {}: interface {}: report descriptor rejected: {:?}", devname, iface.number, e),
            },
            Err(e) => kprintln!("usb {}: interface {}: no report descriptor: {}", devname, iface.number, e),
        }
    }
    // Absolute X/Y: a tablet, a touch screen or a virtual machine's
    // pointer. Its reports only make sense in the report protocol.
    if let Some(l) = tablet {
        if boot {
            pipe.control_out(SetupPacket::hid_set_protocol(iface.number, true), &[])?;
        }
        let _ = pipe.control_out(SetupPacket::hid_set_idle(iface.number, 0), &[]);
        let id = input::register(name, input::class::MOUSE);
        kprintln!(
            "usb {}: HID absolute pointer on interface {} ({} buttons, {}-bit X/Y{}) -> input device {}",
            devname,
            iface.number,
            l.buttons.len(),
            l.x.size,
            if l.wheel.is_some() { ", wheel" } else { "" },
            id
        );
        return Ok(Some(HidFunction {
            interface: iface.number,
            input_id: id,
            kind: HidKind::Tablet { layout: l, buttons: 0 },
            reports: 0,
            bad_reports: 0,
        }));
    }
    let is_boot_mouse = iface.is_hid_boot(descriptor::HID_PROTOCOL_MOUSE);
    // The report protocol is what a device starts in, so one that refuses
    // the request is still driven. Which protocol it is in is then asked,
    // not guessed from its reports: only a device that says "boot" is
    // read as a boot mouse.
    if layout.is_some() && boot {
        if let Err(e) = pipe.control_out(SetupPacket::hid_set_protocol(iface.number, true), &[]) {
            let mut protocol = [0xFFu8; 1];
            let asked = pipe.control_in(SetupPacket::hid_get_protocol(iface.number), &mut protocol);
            let in_boot = is_boot_mouse && matches!(asked, Ok(1)) && protocol[0] == 0;
            kprintln!(
                "usb {}: interface {}: SET_PROTOCOL(report) refused ({}); the device says it is in the {} protocol",
                devname,
                iface.number,
                e,
                match (in_boot, &asked) {
                    (true, _) => "boot",
                    (false, Ok(1)) => "report",
                    _ => "report (it did not answer; assumed)",
                }
            );
            if in_boot {
                layout = None;
            }
        }
    }
    let how = match (&layout, is_boot_mouse) {
        (Some(l), _) => {
            alloc::format!(
                "report protocol: {} buttons, {}-bit X/Y{}{}",
                l.buttons.len(),
                l.x.size,
                if l.wheel.is_some() { ", wheel" } else { "" },
                if l.pan.is_some() { ", horizontal wheel" } else { "" }
            )
        }
        (None, true) => {
            if let Err(e) = pipe.control_out(SetupPacket::hid_set_protocol(iface.number, false), &[]) {
                kprintln!("usb {}: interface {}: SET_PROTOCOL(boot) refused ({}); carrying on", devname, iface.number, e);
            }
            String::from("boot protocol: 3 buttons, 8-bit X/Y")
        }
        (None, false) => {
            kprintln!(
                "usb {}: HID interface {} (subclass {}, protocol {}) is not a boot keyboard or a pointer: not supported",
                devname,
                iface.number,
                iface.subclass,
                iface.protocol
            );
            return Ok(None);
        }
    };
    let _ = pipe.control_out(SetupPacket::hid_set_idle(iface.number, 0), &[]);
    let id = input::register(name, input::class::MOUSE);
    kprintln!("usb {}: HID mouse on interface {} ({}) -> input device {}", devname, iface.number, how, id);
    Ok(Some(HidFunction { interface: iface.number, input_id: id, kind: HidKind::Mouse { layout, buttons: 0 }, reports: 0, bad_reports: 0 }))
}
