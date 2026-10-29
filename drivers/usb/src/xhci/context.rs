//! Slot, endpoint and input contexts (xHCI 1.2 section 6.2).
//!
//! Contexts are 32 or 64 bytes (HCCPARAMS1.CSZ); only the first 32 bytes
//! carry fields. An Input Context is an Input Control Context followed by a
//! Slot Context and 31 Endpoint Contexts; a Device (output) Context is a
//! Slot Context followed by 31 Endpoint Contexts. The functions here encode
//! and decode the dwords; the caller places them at [`ContextLayout`]
//! offsets in DMA memory.

use crate::Speed;

/// Endpoint types in the Endpoint Context `EP Type` field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum EndpointType {
    IsochOut = 1,
    BulkOut = 2,
    InterruptOut = 3,
    Control = 4,
    IsochIn = 5,
    BulkIn = 6,
    InterruptIn = 7,
}

impl EndpointType {
    /// From an endpoint descriptor's `bmAttributes` and address.
    pub fn from_descriptor(address: u8, attributes: u8) -> EndpointType {
        let dir_in = address & 0x80 != 0;
        match (attributes & 3, dir_in) {
            (0, _) => EndpointType::Control,
            (1, false) => EndpointType::IsochOut,
            (1, true) => EndpointType::IsochIn,
            (2, false) => EndpointType::BulkOut,
            (2, true) => EndpointType::BulkIn,
            (_, false) => EndpointType::InterruptOut,
            (_, true) => EndpointType::InterruptIn,
        }
    }
}

/// Endpoint states (Endpoint Context `EP State`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndpointState {
    Disabled,
    Running,
    Halted,
    Stopped,
    Error,
    Reserved(u8),
}

impl EndpointState {
    pub fn from_raw(v: u8) -> EndpointState {
        match v {
            0 => EndpointState::Disabled,
            1 => EndpointState::Running,
            2 => EndpointState::Halted,
            3 => EndpointState::Stopped,
            4 => EndpointState::Error,
            v => EndpointState::Reserved(v),
        }
    }
}

/// Where the contexts live inside input and device context buffers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextLayout {
    pub context_size: usize,
}

impl ContextLayout {
    pub fn new(csz64: bool) -> ContextLayout {
        ContextLayout { context_size: if csz64 { 64 } else { 32 } }
    }

    /// Bytes of an Input Context (control + slot + 31 endpoints).
    pub fn input_len(&self) -> usize {
        33 * self.context_size
    }

    /// Bytes of a Device Context (slot + 31 endpoints).
    pub fn device_len(&self) -> usize {
        32 * self.context_size
    }

    pub fn input_control(&self) -> usize {
        0
    }

    pub fn input_slot(&self) -> usize {
        self.context_size
    }

    /// Offset of endpoint `dci` (1..=31) in an Input Context.
    pub fn input_endpoint(&self, dci: u8) -> usize {
        assert!((1..=31).contains(&dci), "DCI out of range");
        (1 + dci as usize) * self.context_size
    }

    pub fn device_slot(&self) -> usize {
        0
    }

    /// Offset of endpoint `dci` (1..=31) in a Device Context.
    pub fn device_endpoint(&self, dci: u8) -> usize {
        assert!((1..=31).contains(&dci), "DCI out of range");
        dci as usize * self.context_size
    }
}

/// Input Control Context: which contexts to add (A0 = slot, A1 = EP0, ...)
/// and drop.
pub fn input_control(add: u32, drop: u32) -> [u32; 8] {
    [drop, add, 0, 0, 0, 0, 0, 0]
}

/// Slot Context fields this driver sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotContext {
    pub route_string: u32,
    pub speed: Speed,
    /// Index of the last valid endpoint context (1 = only EP0).
    pub context_entries: u8,
    pub root_port: u8,
    pub interrupter: u16,
    /// Downstream ports when the device is a hub (0: not a hub).
    pub hub_ports: u8,
    /// Hub with one transaction translator per port.
    pub mtt: bool,
    /// Hub TT think time (0..=3, in units of 8 FS bit times).
    pub ttt: u8,
    /// For a low/full-speed device behind a high-speed hub: that hub's slot
    /// and the hub port it hangs off (0 otherwise).
    pub tt_slot: u8,
    pub tt_port: u8,
}

impl SlotContext {
    pub fn encode(&self) -> [u32; 8] {
        let mut d = [0u32; 8];
        d[0] = (self.route_string & 0xF_FFFF)
            | ((self.speed.psi() as u32) << 20)
            | ((self.mtt as u32) << 25)
            | (((self.hub_ports != 0) as u32) << 26)
            | ((self.context_entries as u32 & 0x1F) << 27);
        d[1] = ((self.root_port as u32) << 16) | ((self.hub_ports as u32) << 24);
        d[2] = (self.tt_slot as u32) | ((self.tt_port as u32) << 8) | (((self.ttt & 3) as u32) << 16) | (((self.interrupter as u32) & 0x3FF) << 22);
        d
    }
}

/// Fields of an output Slot Context the driver reads back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotState {
    pub device_address: u8,
    /// 0 disabled/enabled, 1 default, 2 addressed, 3 configured.
    pub state: u8,
    pub context_entries: u8,
}

impl SlotState {
    pub fn decode(d: &[u32]) -> SlotState {
        SlotState { device_address: d[3] as u8, state: (d[3] >> 27) as u8, context_entries: (d[0] >> 27) as u8 }
    }
}

/// Endpoint Context fields this driver sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EndpointContext {
    pub ep_type: EndpointType,
    pub max_packet: u16,
    pub max_burst: u8,
    /// Encoded interval (period = 2^interval * 125 us).
    pub interval: u8,
    /// Transfer ring dequeue pointer (16-byte aligned) and cycle state.
    pub dequeue: u64,
    pub dcs: bool,
    pub average_trb_length: u16,
    pub max_esit_payload: u32,
    /// Error count before the controller gives up (3 = maximum retries).
    pub error_count: u8,
}

impl EndpointContext {
    pub fn encode(&self) -> [u32; 8] {
        let mut d = [0u32; 8];
        d[0] = ((self.interval as u32) << 16) | ((self.max_esit_payload >> 16) << 24);
        d[1] = ((self.error_count as u32 & 3) << 1)
            | ((self.ep_type as u32) << 3)
            | ((self.max_burst as u32) << 8)
            | ((self.max_packet as u32) << 16);
        let ptr = (self.dequeue & !0xF) | u64::from(self.dcs);
        d[2] = ptr as u32;
        d[3] = (ptr >> 32) as u32;
        d[4] = (self.average_trb_length as u32) | ((self.max_esit_payload & 0xFFFF) << 16);
        d
    }
}

/// State of an endpoint from its output context.
pub fn endpoint_state(d: &[u32]) -> EndpointState {
    EndpointState::from_raw((d[0] & 7) as u8)
}

/// xHCI `Interval` for a periodic endpoint from the descriptor's
/// `bInterval` (xHCI 6.2.3.6): full/low-speed interrupt endpoints give a
/// period in 1 ms frames, everything else an exponent `2^(bInterval-1)`
/// microframes.
pub fn periodic_interval(speed: Speed, ep_type: EndpointType, b_interval: u8) -> u8 {
    let fs_ls = matches!(speed, Speed::Full | Speed::Low);
    let interrupt = matches!(ep_type, EndpointType::InterruptIn | EndpointType::InterruptOut);
    if fs_ls && interrupt {
        // Period in frames -> microframe exponent: floor(log2(frames * 8)).
        let frames = b_interval.max(1) as u32;
        let exp = 31 - (frames * 8).leading_zeros();
        exp.clamp(3, 10) as u8
    } else {
        // Exponent form, 1..=16 -> 0..=15.
        (b_interval.clamp(1, 16) - 1)
            .max(if fs_ls { 3 } else { 0 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_offsets_for_both_context_sizes() {
        let l = ContextLayout::new(false);
        assert_eq!((l.input_len(), l.device_len()), (33 * 32, 32 * 32));
        assert_eq!(l.input_slot(), 32);
        assert_eq!(l.input_endpoint(1), 64);
        assert_eq!(l.input_endpoint(3), 128);
        assert_eq!(l.device_endpoint(1), 32);
        assert_eq!(l.device_endpoint(31), 31 * 32);
        let l = ContextLayout::new(true);
        assert_eq!((l.input_len(), l.device_len()), (33 * 64, 32 * 64));
        assert_eq!(l.input_endpoint(1), 128);
        assert_eq!(l.device_endpoint(2), 128);
        assert!(l.input_len() <= 4096, "an input context fits one page");
    }

    #[test]
    #[should_panic]
    fn dci_zero_is_rejected() {
        ContextLayout::new(false).input_endpoint(0);
    }

    #[test]
    fn slot_context_encoding() {
        let s = SlotContext { route_string: 0, speed: Speed::Full, context_entries: 1, root_port: 3, interrupter: 0, hub_ports: 0, mtt: false, ttt: 0, tt_slot: 0, tt_port: 0 };
        let d = s.encode();
        assert_eq!(d[0], (1 << 20) | (1 << 27));
        assert_eq!(d[1], 3 << 16);
        // A low-speed device on port 2 of a hub that is on port 4 of a
        // high-speed hub in slot 5, both behind root port 1.
        let dev = SlotContext { route_string: 0x24, speed: Speed::Low, context_entries: 1, root_port: 1, interrupter: 0, hub_ports: 0, mtt: false, ttt: 0, tt_slot: 5, tt_port: 4 };
        let d = dev.encode();
        assert_eq!(d[0], 0x24 | (2 << 20) | (1 << 27));
        assert_eq!(d[2], 5 | (4 << 8));
        let hub = SlotContext { route_string: 3, speed: Speed::High, context_entries: 1, root_port: 2, interrupter: 0, hub_ports: 7, mtt: true, ttt: 1, tt_slot: 0, tt_port: 0 };
        let d = hub.encode();
        assert_eq!(d[0], 3 | (3 << 20) | (1 << 25) | (1 << 26) | (1 << 27));
        assert_eq!(d[1], (2 << 16) | (7 << 24));
        assert_eq!(d[2], 1 << 16);
        let out = SlotState::decode(&[3 << 27, 0, 0, (2 << 27) | 5, 0, 0, 0, 0]);
        assert_eq!(out, SlotState { device_address: 5, state: 2, context_entries: 3 });
    }

    #[test]
    fn endpoint_context_encoding() {
        let e = EndpointContext {
            ep_type: EndpointType::InterruptIn,
            max_packet: 8,
            max_burst: 0,
            interval: 6,
            dequeue: 0x1234_5670,
            dcs: true,
            average_trb_length: 8,
            max_esit_payload: 8,
            error_count: 3,
        };
        let d = e.encode();
        assert_eq!(d[0], 6 << 16);
        assert_eq!(d[1], (3 << 1) | (7 << 3) | (8 << 16));
        assert_eq!((d[2], d[3]), (0x1234_5671, 0));
        assert_eq!(d[4], 8 | (8 << 16));
        assert_eq!(endpoint_state(&[2, 0]), EndpointState::Halted);
        assert_eq!(endpoint_state(&[1, 0]), EndpointState::Running);
        let ep0 = EndpointContext { ep_type: EndpointType::Control, max_packet: 512, dequeue: 0x1_0000_0000, ..e };
        let d = ep0.encode();
        assert_eq!((d[1] >> 3) & 7, 4);
        assert_eq!(d[1] >> 16, 512);
        assert_eq!((d[2], d[3]), (1, 1), "64-bit dequeue pointer with DCS");
    }

    #[test]
    fn endpoint_types_from_descriptors() {
        assert_eq!(EndpointType::from_descriptor(0x81, 0x03), EndpointType::InterruptIn);
        assert_eq!(EndpointType::from_descriptor(0x02, 0x03), EndpointType::InterruptOut);
        assert_eq!(EndpointType::from_descriptor(0x83, 0x02), EndpointType::BulkIn);
        assert_eq!(EndpointType::from_descriptor(0x04, 0x01), EndpointType::IsochOut);
        assert_eq!(EndpointType::from_descriptor(0x00, 0x00), EndpointType::Control);
    }

    #[test]
    fn interrupt_intervals() {
        // Full-speed mouse with bInterval 10 ms: 2^6 * 125 us = 8 ms <= 10 ms.
        assert_eq!(periodic_interval(Speed::Full, EndpointType::InterruptIn, 10), 6);
        assert_eq!(periodic_interval(Speed::Low, EndpointType::InterruptIn, 1), 3);
        assert_eq!(periodic_interval(Speed::Full, EndpointType::InterruptIn, 255), 10);
        assert_eq!(periodic_interval(Speed::Full, EndpointType::InterruptIn, 0), 3);
        // High-speed: exponent form.
        assert_eq!(periodic_interval(Speed::High, EndpointType::InterruptIn, 4), 3);
        assert_eq!(periodic_interval(Speed::High, EndpointType::InterruptIn, 1), 0);
        assert_eq!(periodic_interval(Speed::Super, EndpointType::InterruptIn, 16), 15);
        assert_eq!(periodic_interval(Speed::Super, EndpointType::InterruptIn, 40), 15);
    }
}
