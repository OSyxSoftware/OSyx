//! Wireless hardware found at boot: Wi-Fi cards on PCI and Bluetooth
//! controllers on USB, identified by `osyx_wireless::ids`. They appear in
//! the boot log and the device list (Device Manager, quick settings) with
//! their real names even while OSyx has no driver for them.

use crate::sync::IrqSpinLock;
use crate::{kprintln, pci};
use alloc::string::String;
use alloc::vec::Vec;
use osyx_wireless::ids;

#[derive(Clone)]
pub struct Found {
    pub bluetooth: bool,
    pub name: String,
    pub hardware_id: String,
    pub location: String,
}

static FOUND: IrqSpinLock<Vec<Found>> = IrqSpinLock::new(Vec::new());

/// The name of a PCI function if it is a Wi-Fi card.
pub fn pci_name(d: &pci::Device) -> Option<String> {
    match ids::wifi_chip(d.vendor, d.device) {
        Some(c) => Some(String::from(c.name)),
        None if ids::is_wifi_class(d.class, d.subclass) => Some(alloc::format!("Wireless network adapter {:04x}:{:04x}", d.vendor, d.device)),
        None => None,
    }
}

/// Logs the Wi-Fi cards among the PCI functions.
pub fn scan_pci(devs: &[pci::Device]) {
    for d in devs {
        let Some(name) = pci_name(d) else { continue };
        kprintln!("wifi: {} ({:04x}:{:04x}) at {}: no OSyx driver yet", name, d.vendor, d.device, d.addr);
        FOUND.lock().push(Found {
            bluetooth: false,
            name,
            hardware_id: alloc::format!("PCI\\VEN_{:04X}&DEV_{:04X}", d.vendor, d.device),
            location: alloc::format!("PCI {}", d.addr),
        });
    }
}

/// Records a USB device if it is a Bluetooth controller; `class` is the
/// device's class triple, or its first interface's when the device defers
/// to its interfaces. True if it was one.
pub fn usb_device(vid: u16, pid: u16, class: (u8, u8, u8), location: &str) -> bool {
    let Some(chip) = ids::bt_chip(vid, pid, class) else { return false };
    kprintln!("bluetooth: {} ({:04x}:{:04x}) on USB {}: no OSyx driver yet", chip.name, vid, pid, location);
    let mut f = FOUND.lock();
    f.retain(|x| !(x.bluetooth && x.location == location));
    f.push(Found {
        bluetooth: true,
        name: String::from(chip.name),
        hardware_id: alloc::format!("USB\\VID_{vid:04X}&PID_{pid:04X}"),
        location: String::from(location),
    });
    true
}

/// Forgets a Bluetooth controller that was unplugged.
pub fn usb_gone(location: &str) {
    FOUND.lock().retain(|x| !(x.bluetooth && x.location == location));
}

/// Bluetooth controllers currently attached (Wi-Fi cards are listed with
/// the PCI functions).
pub fn bluetooth() -> Vec<Found> {
    FOUND.lock().iter().filter(|f| f.bluetooth).cloned().collect()
}
