//! Hardware-independent parts of the OSyx USB stack.
//!
//! Everything here is plain data manipulation that can be unit-tested on the
//! development host: xHCI register layouts and bit fields, Transfer Request
//! Blocks, producer/consumer ring bookkeeping (cycle bits, link TRBs,
//! wraparound), device/endpoint contexts, USB descriptor parsing and HID
//! report decoding. The kernel driver (`kernel/src/drivers/xhci`) supplies
//! the hardware side: MMIO, DMA memory, interrupts and timing.
//!
//! References: eXtensible Host Controller Interface for Universal Serial Bus
//! (xHCI) Revision 1.2; Universal Serial Bus Specification 2.0, chapter 9;
//! Device Class Definition for HID 1.11 and HID Usage Tables 1.4.
//!
//! Nothing in this crate trusts device-provided data: descriptor and report
//! parsers check every length and offset and return errors instead of
//! panicking.

#![no_std]

extern crate alloc;

pub mod descriptor;
pub mod hid;
pub mod xhci;

/// Bus speeds as reported in the xHCI PORTSC "Port Speed" field with the
/// default Protocol Speed ID mapping (xHCI 1.2 section 7.2.2.1.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Speed {
    Full,
    Low,
    High,
    Super,
    SuperPlus,
}

impl Speed {
    /// Decodes a default Protocol Speed ID (1..=5).
    pub fn from_psi(psi: u8) -> Option<Speed> {
        match psi {
            1 => Some(Speed::Full),
            2 => Some(Speed::Low),
            3 => Some(Speed::High),
            4 => Some(Speed::Super),
            5 => Some(Speed::SuperPlus),
            _ => None,
        }
    }

    /// The Protocol Speed ID used in slot contexts.
    pub fn psi(self) -> u8 {
        match self {
            Speed::Full => 1,
            Speed::Low => 2,
            Speed::High => 3,
            Speed::Super => 4,
            Speed::SuperPlus => 5,
        }
    }

    /// Initial max packet size of the default control endpoint before the
    /// device descriptor is known (USB 2.0 5.5.3, USB 3.2 9.6.1).
    pub fn default_max_packet0(self) -> u16 {
        match self {
            Speed::Low | Speed::Full => 8,
            Speed::High => 64,
            Speed::Super | Speed::SuperPlus => 512,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Speed::Full => "full-speed (12 Mb/s)",
            Speed::Low => "low-speed (1.5 Mb/s)",
            Speed::High => "high-speed (480 Mb/s)",
            Speed::Super => "SuperSpeed (5 Gb/s)",
            Speed::SuperPlus => "SuperSpeedPlus (10 Gb/s)",
        }
    }

    pub fn is_superspeed(self) -> bool {
        matches!(self, Speed::Super | Speed::SuperPlus)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_ids_round_trip() {
        for psi in 1..=5 {
            assert_eq!(Speed::from_psi(psi).unwrap().psi(), psi);
        }
        assert_eq!(Speed::from_psi(0), None);
        assert_eq!(Speed::from_psi(6), None);
        assert_eq!(Speed::Low.default_max_packet0(), 8);
        assert_eq!(Speed::High.default_max_packet0(), 64);
        assert_eq!(Speed::Super.default_max_packet0(), 512);
    }
}
