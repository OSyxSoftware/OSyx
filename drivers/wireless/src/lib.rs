//! Hardware-independent Wi-Fi and Bluetooth building blocks.
//!
//! * [`ids`]: which wireless chip a PCI or USB id is (shown to the user even
//!   before a driver exists for it).
//! * [`wpa`]: the WPA2-Personal key hierarchy and the EAPOL 4-way handshake
//!   of the station (IEEE 802.11-2016 12.7), independent of any chip.
//!
//! Everything here is `no_std` and tested on the host.

#![no_std]

extern crate alloc;

pub mod ids;
#[cfg(feature = "wpa")]
pub mod wpa;

#[cfg(all(test, feature = "wpa"))]
mod tests;
