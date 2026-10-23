//! xHCI register offsets and bit fields (xHCI 1.2 chapter 5).
//!
//! The controller tells the driver where everything is: the operational
//! registers follow the capability registers at `CAPLENGTH`, the runtime and
//! doorbell arrays are at `RTSOFF` / `DBOFF`, and extended capabilities form
//! a linked list starting at `HCCPARAMS1.xECP`. [`Capabilities`] decodes the
//! raw capability words; nothing here assumes a particular controller.

/// Capability register offsets (from the MMIO base).
pub mod cap {
    pub const CAPLENGTH: u64 = 0x00;
    pub const HCIVERSION: u64 = 0x02;
    pub const HCSPARAMS1: u64 = 0x04;
    pub const HCSPARAMS2: u64 = 0x08;
    pub const HCSPARAMS3: u64 = 0x0C;
    pub const HCCPARAMS1: u64 = 0x10;
    pub const DBOFF: u64 = 0x14;
    pub const RTSOFF: u64 = 0x18;
    pub const HCCPARAMS2: u64 = 0x1C;
}

/// Operational register offsets (from the operational base).
pub mod op {
    pub const USBCMD: u64 = 0x00;
    pub const USBSTS: u64 = 0x04;
    pub const PAGESIZE: u64 = 0x08;
    pub const DNCTRL: u64 = 0x14;
    pub const CRCR: u64 = 0x18;
    pub const DCBAAP: u64 = 0x30;
    pub const CONFIG: u64 = 0x38;
    /// First port register set; port `n` (1-based) is at `PORTS + 0x10 * (n - 1)`.
    pub const PORTS: u64 = 0x400;
    pub const PORTSC: u64 = 0x0;
    pub const PORTPMSC: u64 = 0x4;

    /// Offset of PORTSC of 1-based port `n`.
    pub const fn portsc(n: u8) -> u64 {
        PORTS + 0x10 * (n as u64 - 1) + PORTSC
    }
}

/// USBCMD bits.
pub mod usbcmd {
    pub const RUN: u32 = 1 << 0;
    pub const HCRST: u32 = 1 << 1;
    pub const INTE: u32 = 1 << 2;
    pub const HSEE: u32 = 1 << 3;
}

/// USBSTS bits.
pub mod usbsts {
    pub const HCH: u32 = 1 << 0;
    pub const HSE: u32 = 1 << 2;
    pub const EINT: u32 = 1 << 3;
    pub const PCD: u32 = 1 << 4;
    pub const CNR: u32 = 1 << 11;
    pub const HCE: u32 = 1 << 12;
    /// Bits cleared by writing 1.
    pub const RW1C: u32 = HSE | EINT | PCD | (1 << 10);
}

/// CRCR bits.
pub mod crcr {
    pub const RCS: u64 = 1 << 0;
    pub const CS: u64 = 1 << 1;
    pub const CA: u64 = 1 << 2;
    pub const CRR: u64 = 1 << 3;
}

/// Runtime registers: interrupter register sets.
pub mod rt {
    pub const MFINDEX: u64 = 0x00;
    /// Interrupter `i` register set.
    pub const fn interrupter(i: u16) -> u64 {
        0x20 + 0x20 * i as u64
    }
    pub const IMAN: u64 = 0x00;
    pub const IMOD: u64 = 0x04;
    pub const ERSTSZ: u64 = 0x08;
    pub const ERSTBA: u64 = 0x10;
    pub const ERDP: u64 = 0x18;

    pub const IMAN_IP: u32 = 1 << 0;
    pub const IMAN_IE: u32 = 1 << 1;
    /// ERDP: Event Handler Busy (write 1 to clear).
    pub const ERDP_EHB: u64 = 1 << 3;
}

/// PORTSC bits (xHCI 5.4.8).
pub mod portsc {
    pub const CCS: u32 = 1 << 0;
    pub const PED: u32 = 1 << 1;
    pub const OCA: u32 = 1 << 3;
    pub const PR: u32 = 1 << 4;
    pub const PLS_SHIFT: u32 = 5;
    pub const PLS_MASK: u32 = 0xF << PLS_SHIFT;
    pub const PP: u32 = 1 << 9;
    pub const SPEED_SHIFT: u32 = 10;
    pub const SPEED_MASK: u32 = 0xF << SPEED_SHIFT;
    pub const PIC_MASK: u32 = 3 << 14;
    pub const LWS: u32 = 1 << 16;
    pub const CSC: u32 = 1 << 17;
    pub const PEC: u32 = 1 << 18;
    pub const WRC: u32 = 1 << 19;
    pub const OCC: u32 = 1 << 20;
    pub const PRC: u32 = 1 << 21;
    pub const PLC: u32 = 1 << 22;
    pub const CEC: u32 = 1 << 23;
    pub const CAS: u32 = 1 << 24;
    pub const WCE: u32 = 1 << 25;
    pub const WDE: u32 = 1 << 26;
    pub const WOE: u32 = 1 << 27;
    pub const DR: u32 = 1 << 30;
    pub const WPR: u32 = 1 << 31;

    /// All change bits (write 1 to clear).
    pub const CHANGE_BITS: u32 = CSC | PEC | WRC | OCC | PRC | PLC | CEC;
    /// Read-only bits and read/write bits that keep their value when
    /// written back (Linux calls this the "neutral" state).
    const PRESERVE: u32 = CCS | OCA | SPEED_MASK | CAS | DR | PLS_MASK | PP | PIC_MASK | WCE | WDE | WOE;

    /// A value that, written back, changes nothing: it does not disable the
    /// port (PED is write-1-to-clear), does not acknowledge change bits and
    /// does not start a reset.
    pub const fn neutral(v: u32) -> u32 {
        v & PRESERVE
    }

    /// Port speed field (a Protocol Speed ID).
    pub const fn speed(v: u32) -> u8 {
        ((v & SPEED_MASK) >> SPEED_SHIFT) as u8
    }

    /// Port link state field.
    pub const fn link_state(v: u32) -> u8 {
        ((v & PLS_MASK) >> PLS_SHIFT) as u8
    }
}

/// Extended capability ids (xHCI 7.1).
pub mod ext {
    pub const LEGACY_SUPPORT: u8 = 1;
    pub const SUPPORTED_PROTOCOL: u8 = 2;
    /// USBLEGSUP: BIOS owns the controller.
    pub const BIOS_OWNED: u32 = 1 << 16;
    /// USBLEGSUP: the OS owns the controller.
    pub const OS_OWNED: u32 = 1 << 24;
    /// USBLEGCTLSTS: reserved bits to keep when disabling SMIs.
    pub const LEGCTL_KEEP: u32 = (0x7 << 1) | (0xFF << 5) | (0x7 << 17);
    /// USBLEGCTLSTS: SMI status bits (write 1 to clear).
    pub const LEGCTL_SMI_EVENTS: u32 = 0x7 << 29;
}

/// Decoded capability registers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub cap_length: u8,
    pub version: u16,
    pub max_slots: u8,
    pub max_interrupters: u16,
    pub max_ports: u8,
    /// Isochronous Scheduling Threshold (raw).
    pub ist: u8,
    /// log2 of the Event Ring Segment Table size limit.
    pub erst_max_log2: u8,
    pub max_scratchpad: u16,
    /// Scratchpad Restore: the controller expects scratchpads to survive.
    pub scratchpad_restore: bool,
    /// 64-bit addressing capable.
    pub ac64: bool,
    /// 64-byte contexts.
    pub csz64: bool,
    /// Port power control.
    pub ppc: bool,
    /// Offset (bytes from the MMIO base) of the first extended capability,
    /// 0 if there is none.
    pub xecp: u64,
    pub doorbell_offset: u64,
    pub runtime_offset: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapError {
    /// CAPLENGTH is smaller than the capability registers themselves.
    BadCapLength,
    /// A register array would overlap the capability registers or lie
    /// outside the BAR.
    OutOfBar,
    NoSlots,
    NoPorts,
    NoInterrupters,
}

impl Capabilities {
    /// Decodes the capability registers. `read32(off)` reads a dword at a
    /// byte offset from the MMIO base; `bar_len` is the size of the BAR so
    /// that every derived offset can be validated.
    pub fn read(read32: impl Fn(u64) -> u32, bar_len: u64) -> Result<Capabilities, CapError> {
        let w0 = read32(cap::CAPLENGTH);
        let cap_length = w0 as u8;
        let version = (w0 >> 16) as u16;
        let s1 = read32(cap::HCSPARAMS1);
        let s2 = read32(cap::HCSPARAMS2);
        let c1 = read32(cap::HCCPARAMS1);
        let dboff = (read32(cap::DBOFF) & !0x3) as u64;
        let rtsoff = (read32(cap::RTSOFF) & !0x1F) as u64;
        let c = Capabilities {
            cap_length,
            version,
            max_slots: s1 as u8,
            max_interrupters: ((s1 >> 8) & 0x7FF) as u16,
            max_ports: (s1 >> 24) as u8,
            ist: (s2 & 0xF) as u8,
            erst_max_log2: ((s2 >> 4) & 0xF) as u8,
            max_scratchpad: (((s2 >> 21) & 0x1F) << 5 | (s2 >> 27)) as u16,
            scratchpad_restore: s2 & (1 << 26) != 0,
            ac64: c1 & 1 != 0,
            csz64: c1 & (1 << 2) != 0,
            ppc: c1 & (1 << 3) != 0,
            xecp: ((c1 >> 16) as u64) * 4,
            doorbell_offset: dboff,
            runtime_offset: rtsoff,
        };
        if c.cap_length < 0x20 {
            return Err(CapError::BadCapLength);
        }
        if c.max_slots == 0 {
            return Err(CapError::NoSlots);
        }
        if c.max_ports == 0 {
            return Err(CapError::NoPorts);
        }
        if c.max_interrupters == 0 {
            return Err(CapError::NoInterrupters);
        }
        // Every register block must lie inside the BAR.
        let op_end = c.cap_length as u64 + op::portsc(c.max_ports) + 0x10;
        let rt_end = c.runtime_offset + rt::interrupter(0) + 0x20;
        let db_end = c.doorbell_offset + 4 * (c.max_slots as u64 + 1);
        if op_end > bar_len || rt_end > bar_len || db_end > bar_len || c.runtime_offset < 0x20 || c.doorbell_offset < 0x20 {
            return Err(CapError::OutOfBar);
        }
        if c.xecp != 0 && c.xecp + 8 > bar_len {
            return Err(CapError::OutOfBar);
        }
        Ok(c)
    }

    pub fn operational_offset(&self) -> u64 {
        self.cap_length as u64
    }

    /// Bytes of one device / endpoint context.
    pub fn context_size(&self) -> usize {
        if self.csz64 {
            64
        } else {
            32
        }
    }

    /// Offset of doorbell register `i` (0 = command ring, slot ids after).
    pub fn doorbell(&self, i: u8) -> u64 {
        self.doorbell_offset + 4 * i as u64
    }

    /// Offset of interrupter `i`'s register `reg`.
    pub fn interrupter_reg(&self, i: u16, reg: u64) -> u64 {
        self.runtime_offset + rt::interrupter(i) + reg
    }

    /// Offset of an operational register.
    pub fn op_reg(&self, reg: u64) -> u64 {
        self.cap_length as u64 + reg
    }

    pub fn version_str(&self) -> (u8, u8) {
        ((self.version >> 8) as u8, self.version as u8)
    }
}

/// One entry of the extended capability list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtCap {
    /// Byte offset from the MMIO base.
    pub offset: u64,
    pub id: u8,
}

/// Walks the extended capability list (bounded against loops and the BAR).
pub fn ext_caps(read32: impl Fn(u64) -> u32, first: u64, bar_len: u64) -> alloc::vec::Vec<ExtCap> {
    let mut out = alloc::vec::Vec::new();
    let mut off = first;
    while off != 0 && off + 4 <= bar_len && out.len() < 64 {
        let w = read32(off);
        out.push(ExtCap { offset: off, id: w as u8 });
        let next = ((w >> 8) & 0xFF) as u64 * 4;
        if next == 0 {
            break;
        }
        off += next;
    }
    out
}

/// A Supported Protocol capability: which root hub ports speak which USB
/// major revision (xHCI 7.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SupportedProtocol {
    pub major: u8,
    pub minor: u8,
    /// First port (1-based) and count.
    pub first_port: u8,
    pub port_count: u8,
}

impl SupportedProtocol {
    /// Decodes the capability at `offset`; checks the "USB " name string.
    pub fn read(read32: impl Fn(u64) -> u32, offset: u64) -> Option<SupportedProtocol> {
        let w0 = read32(offset);
        if w0 as u8 != ext::SUPPORTED_PROTOCOL || read32(offset + 4) != u32::from_le_bytes(*b"USB ") {
            return None;
        }
        let w2 = read32(offset + 8);
        let p = SupportedProtocol {
            major: (w0 >> 24) as u8,
            minor: (w0 >> 16) as u8,
            first_port: w2 as u8,
            port_count: (w2 >> 8) as u8,
        };
        (p.first_port != 0 && p.port_count != 0).then_some(p)
    }

    pub fn contains(&self, port: u8) -> bool {
        port >= self.first_port && (port as u16) < self.first_port as u16 + self.port_count as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeMap;

    /// A fake MMIO space made of dwords.
    struct Space(BTreeMap<u64, u32>);
    impl Space {
        fn read(&self, off: u64) -> u32 {
            *self.0.get(&off).unwrap_or(&0)
        }
    }

    /// Capability registers as QEMU's qemu-xhci reports them.
    fn qemu_like() -> Space {
        let mut m = BTreeMap::new();
        m.insert(cap::CAPLENGTH, 0x0100_0040); // version 1.0, caplength 0x40
        m.insert(cap::HCSPARAMS1, (8 << 24) | (16 << 8) | 64);
        m.insert(cap::HCSPARAMS2, 0x0000_000F);
        m.insert(cap::HCCPARAMS1, (0x8 << 16) | 0x1); // xECP at dword 8, AC64
        m.insert(cap::DBOFF, 0x2000);
        m.insert(cap::RTSOFF, 0x1000);
        // Extended capabilities: protocol USB 2.0 at 0x20, USB 3.0 at 0x30.
        m.insert(0x20, (0x02 << 24) | (4 << 8) | 2);
        m.insert(0x24, u32::from_le_bytes(*b"USB "));
        m.insert(0x28, (4 << 8) | 1);
        m.insert(0x30, (0x03 << 24) | 2);
        m.insert(0x34, u32::from_le_bytes(*b"USB "));
        m.insert(0x38, (4 << 8) | 5);
        Space(m)
    }

    #[test]
    fn decodes_capabilities_and_offsets() {
        let s = qemu_like();
        let c = Capabilities::read(|o| s.read(o), 0x4000).unwrap();
        assert_eq!(c.cap_length, 0x40);
        assert_eq!(c.version_str(), (1, 0));
        assert_eq!((c.max_slots, c.max_interrupters, c.max_ports), (64, 16, 8));
        assert!(c.ac64 && !c.csz64);
        assert_eq!(c.context_size(), 32);
        assert_eq!(c.erst_max_log2, 0);
        assert_eq!(c.xecp, 0x20);
        assert_eq!(c.op_reg(op::USBCMD), 0x40);
        assert_eq!(c.op_reg(op::portsc(1)), 0x440);
        assert_eq!(c.op_reg(op::portsc(8)), 0x440 + 7 * 0x10);
        assert_eq!(c.doorbell(0), 0x2000);
        assert_eq!(c.doorbell(3), 0x200C);
        assert_eq!(c.interrupter_reg(0, rt::IMAN), 0x1020);
        assert_eq!(c.interrupter_reg(0, rt::ERDP), 0x1038);
        assert_eq!(c.interrupter_reg(2, rt::ERSTBA), 0x1070);
    }

    #[test]
    fn scratchpad_count_combines_hi_and_lo() {
        let mut s = qemu_like();
        // hi = 0b00001 (bits 25:21), lo = 0b00011 (bits 31:27) -> 32 + 3.
        s.0.insert(cap::HCSPARAMS2, (1 << 21) | (3 << 27) | (1 << 26));
        let c = Capabilities::read(|o| s.read(o), 0x4000).unwrap();
        assert_eq!(c.max_scratchpad, 35);
        assert!(c.scratchpad_restore);
    }

    #[test]
    fn rejects_inconsistent_capabilities() {
        let s = qemu_like();
        assert_eq!(Capabilities::read(|o| s.read(o), 0x1000), Err(CapError::OutOfBar));
        let mut bad = qemu_like();
        bad.0.insert(cap::CAPLENGTH, 0x0100_0010);
        assert_eq!(Capabilities::read(|o| bad.read(o), 0x4000), Err(CapError::BadCapLength));
        let mut bad = qemu_like();
        bad.0.insert(cap::HCSPARAMS1, 16 << 8);
        assert_eq!(Capabilities::read(|o| bad.read(o), 0x4000), Err(CapError::NoSlots));
        let mut bad = qemu_like();
        bad.0.insert(cap::HCSPARAMS1, (16 << 8) | 4);
        assert_eq!(Capabilities::read(|o| bad.read(o), 0x4000), Err(CapError::NoPorts));
    }

    #[test]
    fn walks_extended_capabilities_and_protocols() {
        let s = qemu_like();
        let caps = ext_caps(|o| s.read(o), 0x20, 0x4000);
        assert_eq!(caps, [ExtCap { offset: 0x20, id: 2 }, ExtCap { offset: 0x30, id: 2 }]);
        let usb2 = SupportedProtocol::read(|o| s.read(o), 0x20).unwrap();
        let usb3 = SupportedProtocol::read(|o| s.read(o), 0x30).unwrap();
        assert_eq!((usb2.major, usb2.first_port, usb2.port_count), (2, 1, 4));
        assert_eq!((usb3.major, usb3.first_port, usb3.port_count), (3, 5, 4));
        assert!(usb2.contains(4) && !usb2.contains(5) && usb3.contains(8) && !usb3.contains(9));
    }

    #[test]
    fn extended_capability_walk_is_bounded() {
        // A capability pointing to itself must not loop forever.
        let mut m = BTreeMap::new();
        m.insert(0x20, 0x0000_0001); // next = 0 ends the list
        let s = Space(m);
        assert_eq!(ext_caps(|o| s.read(o), 0x20, 0x100).len(), 1);
        let looping = |_o: u64| (1u32 << 8) | 2; // always "next = +4"
        assert_eq!(ext_caps(looping, 0x20, 0x1_0000).len(), 64);
        assert_eq!(ext_caps(looping, 0x20, 0x30).len(), 4, "stops at the BAR end");
    }

    #[test]
    fn portsc_neutral_value_never_clears_or_resets() {
        let v = portsc::CCS | portsc::PED | portsc::PP | (3 << portsc::SPEED_SHIFT) | portsc::CSC | portsc::PRC;
        let n = portsc::neutral(v);
        assert_eq!(n & portsc::PED, 0, "PED is write-1-to-clear");
        assert_eq!(n & portsc::CHANGE_BITS, 0, "change bits must not be acknowledged by accident");
        assert_eq!(n & portsc::PR, 0);
        assert_ne!(n & portsc::PP, 0, "port power must be kept");
        assert_eq!(portsc::speed(v), 3);
        assert_eq!(portsc::link_state(7 << portsc::PLS_SHIFT), 7);
    }
}
