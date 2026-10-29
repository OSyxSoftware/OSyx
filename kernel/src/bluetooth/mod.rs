//! Bluetooth core: the controllers the USB driver found (`hciN`), the
//! packets queued to and from them, and one kernel thread per controller
//! that brings it up (vendor firmware, then the standard HCI
//! initialisation).
//!
//! Transport: [`crate::drivers::xhci`] sends [`Outgoing`] packets taken
//! with [`take_tx`] and hands what arrives to [`deliver`].

mod intel;

use crate::sync::IrqSpinLock;
use crate::waitq::{self, WaitQueue};
use crate::{interrupts, kprintln};
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

/// A packet for a controller.
pub enum Outgoing {
    /// An HCI command (opcode, length, parameters), sent on the control
    /// endpoint.
    Command(Vec<u8>),
    /// ACL data, or a command for an Intel bootloader, on bulk OUT.
    Bulk(Vec<u8>),
}

/// A packet from a controller.
pub enum Incoming {
    Event(Vec<u8>),
    Acl(Vec<u8>),
    /// A command could not be sent (the transport logged why).
    SendFailed,
}

/// Packets kept per controller before the oldest is dropped.
const RX_MAX: usize = 256;

struct Unit {
    controller: usize,
    slot: u8,
    vendor: u16,
    product: u16,
    name: String,
    rx: VecDeque<Incoming>,
    /// Intel bootloader: events also come on the bulk endpoint.
    bulk_events: bool,
    alive: bool,
}

static UNITS: IrqSpinLock<Vec<Unit>> = IrqSpinLock::new(Vec::new());
static TX: IrqSpinLock<VecDeque<(usize, u8, Outgoing)>> = IrqSpinLock::new(VecDeque::new());
static RX_WAKE: WaitQueue = WaitQueue::new();
static ROOT_READY: AtomicBool = AtomicBool::new(false);

/// Allow controller threads to load firmware after the root is mounted.
pub fn after_mount() {
    ROOT_READY.store(true, Ordering::Release);
    RX_WAKE.wake_all();
}

/// A controller was configured on USB: returns its unit number and starts
/// its thread.
pub fn register(controller: usize, slot: u8, vendor: u16, product: u16, name: &str) -> usize {
    let unit = {
        let mut units = UNITS.lock();
        units.push(Unit {
            controller,
            slot,
            vendor,
            product,
            name: String::from(name),
            rx: VecDeque::new(),
            bulk_events: false,
            alive: true,
        });
        units.len() - 1
    };
    crate::sched::spawn_kernel("bluetooth", unit_thread, unit as u64, 0);
    unit
}

/// The controller was unplugged.
pub fn unregister(unit: usize) {
    let place = UNITS.lock().get_mut(unit).map(|u| {
        u.alive = false;
        u.rx.clear();
        (u.controller, u.slot)
    });
    if let Some(place) = place {
        TX.lock().retain(|t| (t.0, t.1) != place);
    }
    RX_WAKE.wake_all();
}

pub fn deliver(unit: usize, packet: Incoming) {
    if let Some(u) = UNITS.lock().get_mut(unit) {
        if u.rx.len() == RX_MAX {
            u.rx.pop_front();
        }
        u.rx.push_back(packet);
    }
    RX_WAKE.wake_all();
}

pub fn take_tx(controller: usize) -> Option<(u8, Outgoing)> {
    let mut tx = TX.lock();
    let i = tx.iter().position(|t| t.0 == controller)?;
    tx.remove(i).map(|(_, slot, p)| (slot, p))
}

pub fn has_tx(controller: usize) -> bool {
    TX.lock().iter().any(|t| t.0 == controller)
}

pub fn bulk_carries_events(unit: usize) -> bool {
    UNITS.lock().get(unit).is_some_and(|u| u.bulk_events)
}

#[derive(Debug)]
pub enum HciError {
    /// The controller was unplugged.
    Gone,
    Timeout(u16),
    /// The command was not sent.
    NotSent(u16),
    /// Command Complete/Status with this status code.
    Status(u16, u8),
    /// A malformed or unexpected answer.
    Protocol(&'static str),
}

impl core::fmt::Display for HciError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            HciError::Gone => write!(f, "controller gone"),
            HciError::Timeout(op) => write!(f, "no answer to command {op:#06x}"),
            HciError::NotSent(op) => write!(f, "command {op:#06x} not sent"),
            HciError::Status(op, s) => write!(f, "command {op:#06x} failed with status {s:#04x}"),
            HciError::Protocol(m) => write!(f, "{m}"),
        }
    }
}

/// One controller, as its thread sees it.
pub struct Hci {
    unit: usize,
    controller: usize,
    slot: u8,
    /// Events that arrived while waiting for something else.
    stash: VecDeque<Vec<u8>>,
}

pub const EVT_COMMAND_COMPLETE: u8 = 0x0e;
pub const EVT_COMMAND_STATUS: u8 = 0x0f;
pub const EVT_VENDOR: u8 = 0xff;

impl Hci {
    fn alive(&self) -> bool {
        UNITS.lock().get(self.unit).is_some_and(|u| u.alive)
    }

    pub fn set_bulk_events(&self, on: bool) {
        if let Some(u) = UNITS.lock().get_mut(self.unit) {
            u.bulk_events = on;
        }
    }

    fn queue(&self, p: Outgoing) {
        TX.lock().push_back((self.controller, self.slot, p));
        crate::drivers::xhci::wake(self.controller);
    }

    fn encode(opcode: u16, params: &[u8]) -> Vec<u8> {
        let mut c = Vec::with_capacity(3 + params.len());
        c.extend_from_slice(&opcode.to_le_bytes());
        c.push(params.len() as u8);
        c.extend_from_slice(params);
        c
    }

    pub fn send_command(&self, opcode: u16, params: &[u8]) {
        self.queue(Outgoing::Command(Self::encode(opcode, params)));
    }

    /// A command sent on the bulk endpoint (Intel bootloader).
    pub fn send_bulk_command(&self, opcode: u16, params: &[u8]) {
        self.queue(Outgoing::Bulk(Self::encode(opcode, params)));
    }

    pub fn send_acl(&self, packet: Vec<u8>) {
        self.queue(Outgoing::Bulk(packet));
    }

    /// The next incoming packet, or `None` at the deadline (timer tick).
    pub fn recv(&mut self, deadline: u64) -> Result<Option<Incoming>, HciError> {
        if let Some(e) = self.stash.pop_front() {
            return Ok(Some(Incoming::Event(e)));
        }
        let unit = self.unit;
        let r = waitq::block_on(&[&RX_WAKE], Some(deadline), || {
            let mut units = UNITS.lock();
            let u = units.get_mut(unit)?;
            if !u.alive {
                return Some(None);
            }
            u.rx.pop_front().map(Some)
        });
        match r {
            Ok(Some(p)) => Ok(Some(p)),
            Ok(None) => Err(HciError::Gone),
            Err(_) => Ok(None),
        }
    }

    /// Waits for an event accepted by `want`, stashing the others.
    pub fn wait_event(&mut self, timeout_ms: u64, mut want: impl FnMut(&[u8]) -> bool) -> Result<Option<Vec<u8>>, HciError> {
        let deadline = interrupts::ticks() + crate::arch::timer::ms_to_ticks(timeout_ms);
        let mut other = VecDeque::new();
        let mut stash = core::mem::take(&mut self.stash);
        let found = loop {
            if let Some(e) = stash.pop_front() {
                if want(&e) {
                    break Some(e);
                }
                other.push_back(e);
                continue;
            }
            if !self.alive() {
                self.stash = other;
                return Err(HciError::Gone);
            }
            let deadline_left = deadline;
            let unit = self.unit;
            let r = waitq::block_on(&[&RX_WAKE], Some(deadline_left), || {
                let mut units = UNITS.lock();
                let u = units.get_mut(unit)?;
                if !u.alive {
                    return Some(None);
                }
                u.rx.pop_front().map(Some)
            });
            match r {
                Ok(Some(Incoming::Event(e))) => stash.push_back(e),
                Ok(Some(Incoming::SendFailed)) => {
                    self.stash = other;
                    return Err(HciError::NotSent(0));
                }
                // ACL data is not expected while commands are waited for.
                Ok(Some(Incoming::Acl(_))) => {}
                Ok(None) => {
                    self.stash = other;
                    return Err(HciError::Gone);
                }
                Err(_) => break None,
            }
        };
        other.extend(stash);
        self.stash = other;
        Ok(found)
    }

    /// Sends a command and returns the return parameters of its Command
    /// Complete event (status first), or the status of a Command Status.
    pub fn command(&mut self, opcode: u16, params: &[u8], timeout_ms: u64) -> Result<Vec<u8>, HciError> {
        self.send_command(opcode, params);
        self.answer(opcode, timeout_ms)
    }

    /// The Command Complete/Status answer of `opcode`.
    pub fn answer(&mut self, opcode: u16, timeout_ms: u64) -> Result<Vec<u8>, HciError> {
        let ev = self
            .wait_event(timeout_ms, |e| match e.first() {
                Some(&EVT_COMMAND_COMPLETE) => e.len() >= 5 && u16::from_le_bytes([e[3], e[4]]) == opcode,
                Some(&EVT_COMMAND_STATUS) => e.len() >= 6 && u16::from_le_bytes([e[4], e[5]]) == opcode,
                _ => false,
            })?
            .ok_or(HciError::Timeout(opcode))?;
        let ret = if ev[0] == EVT_COMMAND_COMPLETE { ev[5..].to_vec() } else { alloc::vec![ev[2]] };
        match ret.first() {
            Some(0) => Ok(ret),
            Some(&s) => Err(HciError::Status(opcode, s)),
            None => Ok(ret),
        }
    }
}

const OP_RESET: u16 = 0x0c03;
const OP_READ_LOCAL_VERSION: u16 = 0x1001;
const OP_READ_BD_ADDR: u16 = 0x1009;

fn format_addr(a: &[u8]) -> String {
    alloc::format!("{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", a[5], a[4], a[3], a[2], a[1], a[0])
}

fn unit_thread(unit: u64, _: u64) {
    let unit = unit as usize;
    let (controller, slot, vendor, product, name) = {
        let units = UNITS.lock();
        let u = &units[unit];
        (u.controller, u.slot, u.vendor, u.product, u.name.clone())
    };
    let mut hci = Hci { unit, controller, slot, stash: VecDeque::new() };
    if vendor == 0x8087 {
        // USB enumeration starts before storage is mounted. Do not try to
        // read the firmware until the boot thread has installed the root.
        let ready = waitq::block_on(&[&RX_WAKE], None, || {
            if !hci.alive() {
                Some(false)
            } else if ROOT_READY.load(Ordering::Acquire) {
                Some(true)
            } else {
                None
            }
        });
        if !matches!(ready, Ok(true)) {
            return;
        }
        if let Err(e) = intel::setup(&mut hci, unit, product) {
            kprintln!("bluetooth: hci{}: Intel firmware setup failed: {}", unit, e);
            return;
        }
    }
    let r = (|| -> Result<(), HciError> {
        hci.command(OP_RESET, &[], 2000)?;
        let addr = hci.command(OP_READ_BD_ADDR, &[], 1000)?;
        let ver = hci.command(OP_READ_LOCAL_VERSION, &[], 1000)?;
        if addr.len() < 7 || ver.len() < 9 {
            return Err(HciError::Protocol("short version or address"));
        }
        kprintln!(
            "bluetooth: hci{} ({}): address {}, HCI version {}, LMP version {}, manufacturer {}",
            unit,
            name,
            format_addr(&addr[1..7]),
            ver[1],
            ver[4],
            u16::from_le_bytes([ver[5], ver[6]])
        );
        Ok(())
    })();
    match r {
        Ok(()) => kprintln!("bluetooth: hci{}: controller ready", unit),
        Err(e) => kprintln!("bluetooth: hci{}: initialisation failed: {}", unit, e),
    }
}
