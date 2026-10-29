//! PHY and link handling for the r8169-family driver (`rtl8168`), written
//! for OSyx from the IEEE 802.3 and PCI Express specifications rather than
//! taken from another driver: the vendor's per-revision tuning tables are
//! not used.
//!
//! * Chip names from the version id (XID) in TxConfig, for the log.
//! * Clause 22 autonegotiation: advertise 10/100 (and 1000 full duplex on
//!   gigabit parts) with symmetric and asymmetric pause, then restart it.
//! * Energy Efficient Ethernet advertisement off (clause 45 register 7.60
//!   through the clause 22 MMD window, registers 13/14): EEE low-power
//!   idle is a common cause of dropped links with these PHYs.
//! * PCIe ASPM off for the function: L0s/L1 exit latency is another.
//! * A link watchdog: a link that stays down is renegotiated every
//!   [`RENEGOTIATE_MS`], and link changes are logged with speed/duplex.

use crate::pci;

/// MII management through the chip (PHYAR or the OCP window).
pub trait Mdio {
    fn mdio_read(&self, reg: u8) -> Option<u16>;
    fn mdio_write(&self, reg: u8, v: u16) -> bool;
}

const BMCR: u8 = 0;
const ANAR: u8 = 4;
const GBCR: u8 = 9;
const MMD_CTRL: u8 = 13;
const MMD_DATA: u8 = 14;

const BMCR_LOOPBACK: u16 = 1 << 14;
const BMCR_AUTONEG: u16 = 1 << 12;
const BMCR_POWER_DOWN: u16 = 1 << 11;
const BMCR_ISOLATE: u16 = 1 << 10;
const BMCR_RESTART: u16 = 1 << 9;
/// Selector 802.3, 10HD, 10FD, 100HD, 100FD, pause, asymmetric pause.
const ANAR_ALL: u16 = 0x0001 | 0x0020 | 0x0040 | 0x0080 | 0x0100 | 0x0400 | 0x0800;
const GBCR_1000_FULL: u16 = 1 << 9;
const GBCR_1000_HALF: u16 = 1 << 8;
/// MMD 7 (autonegotiation), register 60: EEE advertisement.
const MMD_AN: u16 = 7;
const MMD_EEE_ADV: u16 = 60;
const MMD_FN_DATA: u16 = 0x4000;

/// A link that stays down is renegotiated this often.
pub const RENEGOTIATE_MS: u64 = 15_000;

/// The chip family behind a version id, for the log only.
pub fn chip_name(xid: u32) -> &'static str {
    match xid {
        0x541 => "RTL8168H/8111H",
        0x509 => "RTL8168GU/8111GU",
        0x4c0 | 0x4c1 => "RTL8168G/8111G",
        0x5c8 => "RTL8168FP/8117",
        0x488 => "RTL8411",
        0x480 | 0x481 => "RTL8168F/8111F",
        0x2c8 => "RTL8168E-VL/8111E-VL",
        0x2c1 | 0x2c2 => "RTL8168E/8111E",
        0x281 | 0x283 => "RTL8168D/8111D",
        0x3c0..=0x3cf => "RTL8168C/8111C",
        0x300 | 0x304 | 0x380 => "RTL8168B/8111B",
        0x340..=0x34f => "RTL8101E/8102E",
        0x440..=0x44f => "RTL8105E/8106E",
        0x000 | 0x008 | 0x010 | 0x018 | 0x098 | 0x180 | 0x181 => "RTL8169/8110",
        _ => "r8169 family (unlisted revision)",
    }
}

fn mmd_write(phy: &impl Mdio, devad: u16, reg: u16, v: u16) -> bool {
    phy.mdio_write(MMD_CTRL, devad)
        && phy.mdio_write(MMD_DATA, reg)
        && phy.mdio_write(MMD_CTRL, MMD_FN_DATA | devad)
        && phy.mdio_write(MMD_DATA, v)
}

/// Powers the PHY up, sets what it advertises and restarts
/// autonegotiation. `gigabit`: the part has a 1000BASE-T PHY.
pub fn configure(phy: &impl Mdio, gigabit: bool) -> Result<(), &'static str> {
    let bmcr = phy.mdio_read(BMCR).ok_or("PHY does not answer")?;
    if bmcr == 0xffff {
        return Err("no PHY at the management address");
    }
    if bmcr & BMCR_POWER_DOWN != 0 {
        phy.mdio_write(BMCR, bmcr & !BMCR_POWER_DOWN);
    }
    phy.mdio_write(ANAR, ANAR_ALL);
    if gigabit {
        let g = phy.mdio_read(GBCR).unwrap_or(0);
        // Half-duplex gigabit is not offered by most link partners.
        phy.mdio_write(GBCR, (g | GBCR_1000_FULL) & !GBCR_1000_HALF);
    }
    mmd_write(phy, MMD_AN, MMD_EEE_ADV, 0);
    restart_autoneg(phy);
    Ok(())
}

pub fn restart_autoneg(phy: &impl Mdio) {
    let bmcr = phy.mdio_read(BMCR).unwrap_or(0);
    let keep = bmcr & !(BMCR_LOOPBACK | BMCR_POWER_DOWN | BMCR_ISOLATE);
    phy.mdio_write(BMCR, keep | BMCR_AUTONEG | BMCR_RESTART);
}

/// Disables ASPM L0s and L1 in the function's PCI Express link control.
/// True if it was enabled.
pub fn disable_aspm(dev: &pci::Device) -> bool {
    const CAP_PCIE: u8 = 0x10;
    const LINK_CONTROL: u8 = 0x10;
    let Some(cap) = dev.find_capability(CAP_PCIE) else { return false };
    let ctl = dev.addr.read16(cap + LINK_CONTROL);
    if ctl & 3 == 0 {
        return false;
    }
    dev.addr.write16(cap + LINK_CONTROL, ctl & !3);
    true
}

/// Speed and duplex from the MAC's PHYstatus register.
pub fn describe_status(status: u8) -> &'static str {
    match (status & 0x1c, status & 1 != 0) {
        (s, true) if s & 0x10 != 0 => "1000 Mb/s full duplex",
        (s, true) if s & 0x08 != 0 => "100 Mb/s full duplex",
        (s, false) if s & 0x08 != 0 => "100 Mb/s half duplex",
        (s, true) if s & 0x04 != 0 => "10 Mb/s full duplex",
        (s, false) if s & 0x04 != 0 => "10 Mb/s half duplex",
        _ => "unknown speed",
    }
}

/// Link state tracking for the once-a-second health check.
pub struct Watchdog {
    up: bool,
    /// Uptime (ms) of the last renegotiation, or of the link going down.
    since: u64,
    pub renegotiations: u32,
}

/// What the health check should do.
#[derive(Debug, PartialEq, Eq)]
pub enum LinkEvent {
    None,
    Up,
    Down,
    Renegotiate,
}

impl Watchdog {
    pub fn new(up: bool, now_ms: u64) -> Watchdog {
        Watchdog { up, since: now_ms, renegotiations: 0 }
    }

    pub fn check(&mut self, up: bool, now_ms: u64) -> LinkEvent {
        match (self.up, up) {
            (false, true) => {
                self.up = true;
                LinkEvent::Up
            }
            (true, false) => {
                self.up = false;
                self.since = now_ms;
                LinkEvent::Down
            }
            (false, false) if now_ms.saturating_sub(self.since) >= RENEGOTIATE_MS => {
                self.since = now_ms;
                self.renegotiations += 1;
                LinkEvent::Renegotiate
            }
            _ => LinkEvent::None,
        }
    }
}
