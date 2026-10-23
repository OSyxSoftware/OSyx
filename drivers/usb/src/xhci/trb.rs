//! Transfer Request Blocks (xHCI 1.2 section 6.4).
//!
//! A TRB is four little-endian dwords: a 64-bit parameter, a status dword
//! and a control dword whose bit 0 is the cycle bit and bits 15:10 the type.

use crate::descriptor::SetupPacket;

#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Trb(pub [u32; 4]);

/// TRB type codes (xHCI table 6-91).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum TrbType {
    Normal = 1,
    SetupStage = 2,
    DataStage = 3,
    StatusStage = 4,
    Isoch = 5,
    Link = 6,
    EventData = 7,
    NoOp = 8,
    EnableSlot = 9,
    DisableSlot = 10,
    AddressDevice = 11,
    ConfigureEndpoint = 12,
    EvaluateContext = 13,
    ResetEndpoint = 14,
    StopEndpoint = 15,
    SetTrDequeue = 16,
    ResetDevice = 17,
    NoOpCommand = 23,
    TransferEvent = 32,
    CommandCompletion = 33,
    PortStatusChange = 34,
    BandwidthRequest = 35,
    HostController = 37,
    DeviceNotification = 38,
    MfindexWrap = 39,
}

impl TrbType {
    pub fn from_raw(v: u8) -> Option<TrbType> {
        use TrbType::*;
        Some(match v {
            1 => Normal,
            2 => SetupStage,
            3 => DataStage,
            4 => StatusStage,
            5 => Isoch,
            6 => Link,
            7 => EventData,
            8 => NoOp,
            9 => EnableSlot,
            10 => DisableSlot,
            11 => AddressDevice,
            12 => ConfigureEndpoint,
            13 => EvaluateContext,
            14 => ResetEndpoint,
            15 => StopEndpoint,
            16 => SetTrDequeue,
            17 => ResetDevice,
            23 => NoOpCommand,
            32 => TransferEvent,
            33 => CommandCompletion,
            34 => PortStatusChange,
            35 => BandwidthRequest,
            37 => HostController,
            38 => DeviceNotification,
            39 => MfindexWrap,
            _ => return None,
        })
    }
}

/// Completion codes (xHCI table 6-90).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionCode {
    Invalid,
    Success,
    DataBufferError,
    BabbleDetected,
    UsbTransactionError,
    TrbError,
    StallError,
    ResourceError,
    BandwidthError,
    NoSlotsAvailable,
    InvalidStreamType,
    SlotNotEnabled,
    EndpointNotEnabled,
    ShortPacket,
    RingUnderrun,
    RingOverrun,
    VfEventRingFull,
    ParameterError,
    BandwidthOverrun,
    ContextStateError,
    NoPingResponse,
    EventRingFull,
    IncompatibleDevice,
    MissedService,
    CommandRingStopped,
    CommandAborted,
    Stopped,
    StoppedLengthInvalid,
    StoppedShortPacket,
    MaxExitLatencyTooLarge,
    IsochBufferOverrun,
    EventLost,
    UndefinedError,
    InvalidStreamId,
    SecondaryBandwidthError,
    SplitTransactionError,
    /// Reserved or vendor-defined code.
    Other(u8),
}

impl CompletionCode {
    pub fn from_raw(v: u8) -> CompletionCode {
        use CompletionCode::*;
        match v {
            0 => Invalid,
            1 => Success,
            2 => DataBufferError,
            3 => BabbleDetected,
            4 => UsbTransactionError,
            5 => TrbError,
            6 => StallError,
            7 => ResourceError,
            8 => BandwidthError,
            9 => NoSlotsAvailable,
            10 => InvalidStreamType,
            11 => SlotNotEnabled,
            12 => EndpointNotEnabled,
            13 => ShortPacket,
            14 => RingUnderrun,
            15 => RingOverrun,
            16 => VfEventRingFull,
            17 => ParameterError,
            18 => BandwidthOverrun,
            19 => ContextStateError,
            20 => NoPingResponse,
            21 => EventRingFull,
            22 => IncompatibleDevice,
            23 => MissedService,
            24 => CommandRingStopped,
            25 => CommandAborted,
            26 => Stopped,
            27 => StoppedLengthInvalid,
            28 => StoppedShortPacket,
            29 => MaxExitLatencyTooLarge,
            31 => IsochBufferOverrun,
            32 => EventLost,
            33 => UndefinedError,
            34 => InvalidStreamId,
            35 => SecondaryBandwidthError,
            36 => SplitTransactionError,
            v => Other(v),
        }
    }

    /// Success, or a short packet (the device sent less than asked for,
    /// which is normal for descriptors and HID reports).
    pub fn is_ok(self) -> bool {
        matches!(self, CompletionCode::Success | CompletionCode::ShortPacket)
    }

    /// The endpoint is halted after this error and needs a Reset Endpoint
    /// command before it can be used again (xHCI 4.8.3).
    pub fn halts_endpoint(self) -> bool {
        matches!(
            self,
            CompletionCode::StallError
                | CompletionCode::UsbTransactionError
                | CompletionCode::BabbleDetected
                | CompletionCode::SplitTransactionError
        )
    }
}

// Control dword bits.
pub const CYCLE: u32 = 1 << 0;
/// Link TRB: toggle cycle. Transfer TRBs: evaluate next TRB.
pub const TOGGLE_CYCLE: u32 = 1 << 1;
pub const ENT: u32 = 1 << 1;
/// Interrupt on short packet.
pub const ISP: u32 = 1 << 2;
pub const CHAIN: u32 = 1 << 4;
/// Interrupt on completion.
pub const IOC: u32 = 1 << 5;
/// Immediate data.
pub const IDT: u32 = 1 << 6;
/// Address Device: block SET_ADDRESS request.
pub const BSR: u32 = 1 << 9;
/// Data/Status stage direction IN.
pub const DIR_IN: u32 = 1 << 16;

/// Transfer type of a Setup Stage TRB.
pub const TRT_NO_DATA: u32 = 0;
pub const TRT_OUT: u32 = 2 << 16;
pub const TRT_IN: u32 = 3 << 16;

const fn ty(t: TrbType) -> u32 {
    (t as u32) << 10
}

const fn slot(id: u8) -> u32 {
    (id as u32) << 24
}

const fn endpoint(dci: u8) -> u32 {
    ((dci as u32) & 0x1F) << 16
}

impl Trb {
    pub const fn new(param: u64, status: u32, control: u32) -> Trb {
        Trb([param as u32, (param >> 32) as u32, status, control])
    }

    pub fn param(&self) -> u64 {
        self.0[0] as u64 | (self.0[1] as u64) << 32
    }

    pub fn status(&self) -> u32 {
        self.0[2]
    }

    pub fn control(&self) -> u32 {
        self.0[3]
    }

    pub fn cycle(&self) -> bool {
        self.0[3] & CYCLE != 0
    }

    /// Returns a copy with the cycle bit set to `c`.
    pub fn with_cycle(mut self, c: bool) -> Trb {
        self.0[3] = (self.0[3] & !CYCLE) | u32::from(c);
        self
    }

    pub fn with_flags(mut self, flags: u32) -> Trb {
        self.0[3] |= flags;
        self
    }

    pub fn without_flags(mut self, flags: u32) -> Trb {
        self.0[3] &= !flags;
        self
    }

    pub fn raw_type(&self) -> u8 {
        ((self.0[3] >> 10) & 0x3F) as u8
    }

    pub fn trb_type(&self) -> Option<TrbType> {
        TrbType::from_raw(self.raw_type())
    }

    pub fn has_chain(&self) -> bool {
        self.0[3] & CHAIN != 0
    }

    // ---- event fields ------------------------------------------------------

    pub fn completion_code(&self) -> CompletionCode {
        CompletionCode::from_raw((self.0[2] >> 24) as u8)
    }

    /// Transfer events: bytes *not* transferred (residual length).
    pub fn transfer_residual(&self) -> u32 {
        self.0[2] & 0x00FF_FFFF
    }

    pub fn slot_id(&self) -> u8 {
        (self.0[3] >> 24) as u8
    }

    /// Transfer events: the Device Context Index of the endpoint.
    pub fn endpoint_id(&self) -> u8 {
        ((self.0[3] >> 16) & 0x1F) as u8
    }

    /// Port status change events: 1-based root hub port.
    pub fn port_id(&self) -> u8 {
        (self.0[0] >> 24) as u8
    }

    /// Transfer events with ED set carry Event Data instead of a TRB pointer.
    pub fn event_data(&self) -> bool {
        self.0[3] & (1 << 2) != 0
    }

    // ---- transfer TRBs -------------------------------------------------------

    /// A Normal TRB for `len` bytes at physical `buf`.
    pub fn normal(buf: u64, len: u32, ioc: bool) -> Trb {
        let mut c = ty(TrbType::Normal) | ISP;
        if ioc {
            c |= IOC;
        }
        Trb::new(buf, len & 0x1_FFFF, c)
    }

    /// The Setup Stage TRB of a control transfer (8 bytes immediate data).
    pub fn setup(s: &SetupPacket) -> Trb {
        let trt = match (s.length, s.is_in()) {
            (0, _) => TRT_NO_DATA,
            (_, true) => TRT_IN,
            (_, false) => TRT_OUT,
        };
        Trb::new(s.to_u64(), 8, ty(TrbType::SetupStage) | IDT | trt)
    }

    /// The Data Stage TRB of a control transfer.
    pub fn data(buf: u64, len: u32, dir_in: bool) -> Trb {
        let mut c = ty(TrbType::DataStage) | ISP;
        if dir_in {
            c |= DIR_IN;
        }
        Trb::new(buf, len & 0x1_FFFF, c)
    }

    /// The Status Stage TRB: its direction is IN unless the data stage was
    /// IN (USB 2.0 8.5.3).
    pub fn status_stage(data_in: bool, has_data: bool) -> Trb {
        let mut c = ty(TrbType::StatusStage) | IOC;
        if !has_data || !data_in {
            c |= DIR_IN;
        }
        Trb::new(0, 0, c)
    }

    pub fn link(target: u64, toggle: bool) -> Trb {
        let mut c = ty(TrbType::Link);
        if toggle {
            c |= TOGGLE_CYCLE;
        }
        Trb::new(target, 0, c)
    }

    // ---- commands ------------------------------------------------------------

    pub fn noop_command() -> Trb {
        Trb::new(0, 0, ty(TrbType::NoOpCommand))
    }

    pub fn enable_slot(slot_type: u8) -> Trb {
        Trb::new(0, 0, ty(TrbType::EnableSlot) | ((slot_type as u32 & 0x1F) << 16))
    }

    pub fn disable_slot(id: u8) -> Trb {
        Trb::new(0, 0, ty(TrbType::DisableSlot) | slot(id))
    }

    pub fn address_device(input_ctx: u64, id: u8, block_set_address: bool) -> Trb {
        let mut c = ty(TrbType::AddressDevice) | slot(id);
        if block_set_address {
            c |= BSR;
        }
        Trb::new(input_ctx, 0, c)
    }

    pub fn configure_endpoint(input_ctx: u64, id: u8, deconfigure: bool) -> Trb {
        let mut c = ty(TrbType::ConfigureEndpoint) | slot(id);
        if deconfigure {
            c |= 1 << 9;
        }
        Trb::new(input_ctx, 0, c)
    }

    pub fn evaluate_context(input_ctx: u64, id: u8) -> Trb {
        Trb::new(input_ctx, 0, ty(TrbType::EvaluateContext) | slot(id))
    }

    pub fn reset_endpoint(id: u8, dci: u8) -> Trb {
        Trb::new(0, 0, ty(TrbType::ResetEndpoint) | slot(id) | endpoint(dci))
    }

    pub fn stop_endpoint(id: u8, dci: u8) -> Trb {
        Trb::new(0, 0, ty(TrbType::StopEndpoint) | slot(id) | endpoint(dci))
    }

    /// Set TR Dequeue Pointer: `ptr` must be 16-byte aligned; `dcs` is the
    /// cycle state the controller resumes with.
    pub fn set_tr_dequeue(ptr: u64, dcs: bool, id: u8, dci: u8) -> Trb {
        Trb::new((ptr & !0xF) | u64::from(dcs), 0, ty(TrbType::SetTrDequeue) | slot(id) | endpoint(dci))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_sixteen_bytes() {
        assert_eq!(core::mem::size_of::<Trb>(), 16);
        assert_eq!(core::mem::align_of::<Trb>(), 16);
    }

    #[test]
    fn command_encodings() {
        let t = Trb::enable_slot(0);
        assert_eq!(t.trb_type(), Some(TrbType::EnableSlot));
        assert_eq!(t.control(), 9 << 10);
        let t = Trb::address_device(0x1234_5000, 7, false);
        assert_eq!(t.param(), 0x1234_5000);
        assert_eq!(t.control(), (11 << 10) | (7 << 24));
        assert_eq!(Trb::address_device(0, 7, true).control() & BSR, BSR);
        let t = Trb::configure_endpoint(0xABC0, 3, false);
        assert_eq!((t.trb_type(), t.slot_id()), (Some(TrbType::ConfigureEndpoint), 3));
        let t = Trb::reset_endpoint(2, 3);
        assert_eq!(t.control(), (14 << 10) | (3 << 16) | (2 << 24));
        let t = Trb::set_tr_dequeue(0x8000_0010, true, 1, 3);
        assert_eq!(t.param(), 0x8000_0011);
        assert_eq!(Trb::disable_slot(9).slot_id(), 9);
        assert_eq!(Trb::stop_endpoint(4, 5).endpoint_id(), 5);
    }

    #[test]
    fn control_transfer_encodings() {
        let get = SetupPacket::get_descriptor(1, 0, 0, 18);
        let s = Trb::setup(&get);
        assert_eq!(s.param(), 0x0012_0000_0100_0680);
        assert_eq!(s.status(), 8);
        assert_eq!(s.control(), (2 << 10) | IDT | TRT_IN);
        let d = Trb::data(0x5000, 18, true);
        assert_eq!(d.control() & DIR_IN, DIR_IN);
        assert_eq!(d.status(), 18);
        // IN data stage: status stage is OUT.
        assert_eq!(Trb::status_stage(true, true).control() & DIR_IN, 0);
        // No data stage or OUT data: status stage is IN.
        assert_eq!(Trb::status_stage(false, false).control() & DIR_IN, DIR_IN);
        assert_eq!(Trb::status_stage(false, true).control() & DIR_IN, DIR_IN);
        let set_cfg = SetupPacket::set_configuration(1);
        assert_eq!(Trb::setup(&set_cfg).control() & (3 << 16), TRT_NO_DATA);
    }

    #[test]
    fn event_field_decoding() {
        // Transfer event: 3 bytes residual, short packet, slot 2, DCI 3.
        let e = Trb([0x1000, 0, (13 << 24) | 3, (2 << 24) | (3 << 16) | (32 << 10) | 1]);
        assert_eq!(e.trb_type(), Some(TrbType::TransferEvent));
        assert_eq!(e.completion_code(), CompletionCode::ShortPacket);
        assert!(e.completion_code().is_ok());
        assert_eq!(e.transfer_residual(), 3);
        assert_eq!((e.slot_id(), e.endpoint_id()), (2, 3));
        assert!(e.cycle());
        let p = Trb([5 << 24, 0, 1 << 24, 34 << 10]);
        assert_eq!((p.trb_type(), p.port_id()), (Some(TrbType::PortStatusChange), 5));
        assert_eq!(Trb([0, 0, 0, 63 << 10]).trb_type(), None);
    }

    #[test]
    fn completion_codes() {
        for v in 0..=255u8 {
            let c = CompletionCode::from_raw(v);
            if matches!(v, 30 | 37..=255) {
                assert_eq!(c, CompletionCode::Other(v));
            } else {
                assert_ne!(c, CompletionCode::Other(v), "code {v} must be named");
            }
        }
        assert!(CompletionCode::StallError.halts_endpoint());
        assert!(!CompletionCode::ShortPacket.halts_endpoint());
        assert!(!CompletionCode::StallError.is_ok());
    }

    #[test]
    fn cycle_and_flag_helpers() {
        let t = Trb::normal(0x2000, 8, true);
        assert!(!t.cycle());
        assert!(t.with_cycle(true).cycle());
        assert!(!t.with_cycle(true).with_cycle(false).cycle());
        assert!(t.with_flags(CHAIN).has_chain());
        assert!(!t.with_flags(CHAIN).without_flags(CHAIN).has_chain());
        assert_eq!(t.control() & IOC, IOC);
        assert_eq!(Trb::normal(0, 0x2_0005, false).status(), 0x0005, "length is 17 bits");
    }
}
