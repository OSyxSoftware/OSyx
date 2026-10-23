//! Native xHCI (USB 3.x host controller) driver.
//!
//! Every PCI function with class 0x0C, subclass 0x03, programming
//! interface 0x30 gets a kernel thread that owns the controller completely:
//! it takes the controller from the firmware (USB legacy support
//! handoff), resets it, builds the data structures the controller reads
//! (device context base address array, scratchpads, command ring, event
//! ring and its segment table), routes the event interrupt through MSI-X
//! or MSI to the local APIC, and then serves root hub ports: connect,
//! reset, slot enable, addressing, descriptor reads, configuration,
//! interrupt endpoints for HID devices, and teardown on disconnect.
//!
//! Hardware facts are read from the controller (capability registers,
//! extended capabilities, port speeds, context size, scratchpad count,
//! 64-bit addressing); nothing about QEMU is assumed. The interrupt handler
//! only counts and wakes the thread; all ring processing happens in the
//! thread, which also polls once per timer tick when no MSI is available.
//! Every wait has a timeout, and an unresponsive controller or device is
//! reported and abandoned instead of hanging the kernel.

pub mod dma;
mod hid;

use crate::sync::IrqSpinLock;
use crate::waitq::{block_on, WaitQueue};
use crate::{input, interrupts, irq, kprintln, pci, sched};
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{fence, AtomicU64, AtomicUsize, Ordering};
use dma::{DmaError, DmaPage, DmaRing};
use hid::{HidFunction, HidKind};
use osyx_usb::descriptor::{self, Configuration, DescriptorError, DeviceDescriptor, Interface, SetupPacket};
use osyx_usb::hid::{parse_report_descriptor, BootKeyboard, MouseLayout};
use osyx_usb::xhci::context::{self, ContextLayout, EndpointContext, EndpointType, SlotContext};
use osyx_usb::xhci::regs::{self, ext, op, portsc, rt, usbcmd, usbsts, Capabilities, SupportedProtocol};
use osyx_usb::xhci::ring::{erst_entry, EventRing, ProducerRing, RingError, TrbMemory};
use osyx_usb::xhci::trb::{CompletionCode, Trb, TrbType};
use osyx_usb::Speed;

const MAX_CONTROLLERS: usize = 4;
const COMMAND_TIMEOUT_MS: u64 = 1000;
const TRANSFER_TIMEOUT_MS: u64 = 1000;
const RESET_TIMEOUT_MS: u64 = 1000;
const PORT_RESET_TIMEOUT_MS: u64 = 500;
/// Interrupt moderation interval, in 250 ns units (40 us).
const IMOD_INTERVAL: u32 = 160;
/// Consecutive transfer errors after which an endpoint is given up.
const MAX_ENDPOINT_ERRORS: u32 = 20;

static WAKE: [WaitQueue; MAX_CONTROLLERS] = [const { WaitQueue::new() }; MAX_CONTROLLERS];
static IRQS: [AtomicU64; MAX_CONTROLLERS] = [const { AtomicU64::new(0) }; MAX_CONTROLLERS];
static CONTROLLERS: AtomicUsize = AtomicUsize::new(0);
/// One status line per controller, for diagnostics and tests.
static STATUS: IrqSpinLock<Vec<String>> = IrqSpinLock::new(Vec::new());

#[derive(Debug)]
pub enum XhciError {
    Bar(&'static str),
    Caps(regs::CapError),
    Dma(DmaError),
    Timeout(&'static str),
    Command(CompletionCode),
    Transfer(CompletionCode),
    Ring(RingError),
    Descriptor(DescriptorError),
    BadSlot,
    PortGone,
    Unsupported(&'static str),
}

impl fmt::Display for XhciError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            XhciError::Bar(s) => write!(f, "BAR: {s}"),
            XhciError::Caps(e) => write!(f, "capability registers: {e:?}"),
            XhciError::Dma(e) => write!(f, "DMA memory: {e:?}"),
            XhciError::Timeout(s) => write!(f, "timeout: {s}"),
            XhciError::Command(c) => write!(f, "command failed: {c:?}"),
            XhciError::Transfer(c) => write!(f, "transfer failed: {c:?}"),
            XhciError::Ring(e) => write!(f, "ring: {e:?}"),
            XhciError::Descriptor(e) => write!(f, "malformed descriptor: {e:?}"),
            XhciError::BadSlot => write!(f, "controller returned an invalid slot id"),
            XhciError::PortGone => write!(f, "device disconnected"),
            XhciError::Unsupported(s) => write!(f, "unsupported: {s}"),
        }
    }
}

impl From<DmaError> for XhciError {
    fn from(e: DmaError) -> Self {
        XhciError::Dma(e)
    }
}

impl From<RingError> for XhciError {
    fn from(e: RingError) -> Self {
        XhciError::Ring(e)
    }
}

impl From<DescriptorError> for XhciError {
    fn from(e: DescriptorError) -> Self {
        XhciError::Descriptor(e)
    }
}

type Result<T> = core::result::Result<T, XhciError>;

/// Volatile access to the controller's register BAR.
#[derive(Clone, Copy)]
struct Mmio(u64);

impl Mmio {
    fn r32(self, off: u64) -> u32 {
        // SAFETY: `self.0` maps the whole register BAR uncached; offsets come
        // from validated capability registers.
        unsafe { read_volatile((self.0 + off) as *const u32) }
    }
    fn w32(self, off: u64, v: u32) {
        // SAFETY: as in `r32`.
        unsafe { write_volatile((self.0 + off) as *mut u32, v) }
    }
    fn r64(self, off: u64) -> u64 {
        self.r32(off) as u64 | (self.r32(off + 4) as u64) << 32
    }
    /// 64-bit registers are written low dword first (xHCI 5.1).
    fn w64(self, off: u64, v: u64) {
        self.w32(off, v as u32);
        self.w32(off + 4, (v >> 32) as u32);
    }
}

/// Probes every xHCI controller; each gets its own thread. Returns how
/// many were found.
pub fn probe(devs: &[pci::Device]) -> usize {
    let mut found = 0;
    for d in devs.iter().filter(|d| d.class == 0x0C && d.subclass == 0x03 && d.prog_if == 0x30) {
        let index = CONTROLLERS.fetch_add(1, Ordering::Relaxed);
        found += 1;
        if index >= MAX_CONTROLLERS {
            kprintln!("xhci: controller at {} ignored (only {} supported)", d.addr, MAX_CONTROLLERS);
            continue;
        }
        kprintln!("xhci{}: USB 3 host controller {:04x}:{:04x} at {}", index, d.vendor, d.device, d.addr);
        let dev = Box::into_raw(Box::new(*d)) as u64;
        sched::spawn_kernel("xhci", controller_thread, index as u64, dev);
    }
    found
}

/// One line per controller ("xhci0: running, MSI-X, 2 devices").
pub fn status() -> Vec<String> {
    STATUS.lock().clone()
}

fn set_status(index: usize, s: String) {
    let mut st = STATUS.lock();
    if st.len() <= index {
        st.resize(index + 1, String::new());
    }
    st[index] = s;
}

fn controller_thread(index: u64, dev: u64) {
    // SAFETY: `dev` is the Box leaked by `probe` for exactly this thread.
    let dev = *unsafe { Box::from_raw(dev as *mut pci::Device) };
    let index = index as usize;
    match Xhci::start(index, dev) {
        Ok(mut hc) => hc.run(),
        Err(e) => {
            kprintln!("xhci{}: initialisation failed: {}", index, e);
            set_status(index, alloc::format!("xhci{index}: failed: {e}"));
        }
    }
}

fn irq_handler(index: u64) {
    IRQS[index as usize].fetch_add(1, Ordering::Relaxed);
    WAKE[index as usize].wake_all();
}

fn ms_to_ticks(ms: u64) -> u64 {
    (ms * crate::arch::pic::TIMER_HZ as u64).div_ceil(1000)
}

/// A HID interrupt IN endpoint with its transfer ring and report buffer.
struct InterruptEndpoint {
    dci: u8,
    ring: ProducerRing,
    mem: DmaRing,
    buffer: DmaPage,
    len: u16,
    /// TRB of the transfer in flight.
    pending: Option<u64>,
    errors: u32,
    function: HidFunction,
}

/// An addressed USB device in a slot.
struct Device {
    port: u8,
    speed: Speed,
    output: DmaPage,
    input: DmaPage,
    ep0: ProducerRing,
    ep0_mem: DmaRing,
    ep0_max_packet: u16,
    /// Data stage buffer of control transfers.
    buffer: DmaPage,
    /// Transfer events of the default control endpoint not consumed yet.
    ep0_events: Vec<Trb>,
    interrupt: Vec<InterruptEndpoint>,
    name: String,
}

impl Device {
    fn new(port: u8, speed: Speed, ac64: bool) -> Result<Device> {
        let ep0_mem = DmaRing::new(ac64)?;
        Ok(Device {
            port,
            speed,
            output: DmaPage::new(ac64)?,
            input: DmaPage::new(ac64)?,
            ep0: ProducerRing::new(&ep0_mem),
            ep0_mem,
            ep0_max_packet: speed.default_max_packet0(),
            buffer: DmaPage::new(ac64)?,
            ep0_events: Vec::new(),
            interrupt: Vec::new(),
            name: String::new(),
        })
    }
}

pub struct Xhci {
    index: usize,
    pci: pci::Device,
    mmio: Mmio,
    caps: Capabilities,
    layout: ContextLayout,
    max_slots: u8,
    protocols: Vec<SupportedProtocol>,
    dcbaa: DmaPage,
    _scratchpad_array: Option<DmaPage>,
    _scratchpads: Vec<DmaPage>,
    cmd: ProducerRing,
    cmd_mem: DmaRing,
    events: EventRing,
    event_mem: DmaRing,
    _erst: DmaPage,
    /// Command completion events by command TRB address.
    completions: BTreeMap<u64, Trb>,
    devices: BTreeMap<u8, Device>,
    /// Root hub port -> slot.
    ports: BTreeMap<u8, u8>,
    pending_ports: VecDeque<u8>,
    irq_vector: Option<u8>,
    irq_mode: &'static str,
    seen_irqs: u64,
}

impl Xhci {
    fn op(&self, reg: u64) -> u64 {
        self.caps.op_reg(reg)
    }

    fn ir(&self, reg: u64) -> u64 {
        self.caps.interrupter_reg(0, reg)
    }

    fn portsc(&self, port: u8) -> u32 {
        self.mmio.r32(self.op(op::portsc(port)))
    }

    fn write_portsc(&self, port: u8, v: u32) {
        self.mmio.w32(self.op(op::portsc(port)), v);
    }

    /// Waits until `done()` with sleeps of one tick; false on timeout.
    fn poll_until(&self, timeout_ms: u64, mut done: impl FnMut(&Xhci) -> bool) -> bool {
        let deadline = interrupts::ticks() + ms_to_ticks(timeout_ms).max(1);
        loop {
            if done(self) {
                return true;
            }
            if interrupts::ticks() > deadline {
                return false;
            }
            sched::sleep_ticks(1);
        }
    }

    // ---- bring-up ------------------------------------------------------------------

    fn start(index: usize, pci: pci::Device) -> Result<Xhci> {
        set_status(index, alloc::format!("xhci{index}: initialising"));
        let Some(pci::Bar::Memory { base, size, .. }) = pci.bar(0) else { return Err(XhciError::Bar("BAR0 is not a memory BAR")) };
        if base == 0 || size < 0x1000 {
            return Err(XhciError::Bar("BAR0 unassigned or too small"));
        }
        let virt = crate::mem::map_mmio(base, size).map_err(|_| XhciError::Bar("cannot map BAR0"))?;
        let mmio = Mmio(virt);
        // Memory decoding and bus mastering (DMA) on, legacy INTx off.
        pci.addr.write16(0x04, pci.command() | pci::CMD_MEMORY | pci::CMD_BUS_MASTER | pci::CMD_INTX_DISABLE);
        let caps = Capabilities::read(|o| mmio.r32(o), size).map_err(XhciError::Caps)?;
        let (maj, min) = caps.version_str();
        kprintln!(
            "xhci{}: version {}.{:02x}, {} slots, {} ports, {} interrupters, {}-byte contexts, {}-bit DMA, {} scratchpads, BAR0 {:#x} ({} KiB)",
            index,
            maj,
            min,
            caps.max_slots,
            caps.max_ports,
            caps.max_interrupters,
            caps.context_size(),
            if caps.ac64 { 64 } else { 32 },
            caps.max_scratchpad,
            base,
            size >> 10
        );
        let ext_caps = regs::ext_caps(|o| mmio.r32(o), caps.xecp, size);
        let mut protocols = Vec::new();
        let mut legacy = None;
        for c in &ext_caps {
            match c.id {
                ext::LEGACY_SUPPORT => legacy = Some(c.offset),
                ext::SUPPORTED_PROTOCOL => {
                    if let Some(p) = SupportedProtocol::read(|o| mmio.r32(o), c.offset) {
                        kprintln!(
                            "xhci{}: ports {}-{}: USB {}.{}",
                            index,
                            p.first_port,
                            p.first_port as u16 + p.port_count as u16 - 1,
                            p.major,
                            p.minor >> 4
                        );
                        protocols.push(p);
                    }
                }
                _ => {}
            }
        }
        if let Some(off) = legacy {
            take_ownership(index, mmio, off);
        }
        let ac64 = caps.ac64;
        let cmd_mem = DmaRing::new(ac64)?;
        let event_mem = DmaRing::new(ac64)?;
        let mut hc = Xhci {
            index,
            pci,
            mmio,
            caps,
            layout: ContextLayout::new(caps.csz64),
            max_slots: caps.max_slots,
            protocols,
            dcbaa: DmaPage::new(ac64)?,
            _scratchpad_array: None,
            _scratchpads: Vec::new(),
            cmd: ProducerRing::new(&cmd_mem),
            cmd_mem,
            events: EventRing::new(&event_mem),
            event_mem,
            _erst: DmaPage::new(ac64)?,
            completions: BTreeMap::new(),
            devices: BTreeMap::new(),
            ports: BTreeMap::new(),
            pending_ports: VecDeque::new(),
            irq_vector: None,
            irq_mode: "polling",
            seen_irqs: 0,
        };
        hc.reset()?;
        hc.setup_structures()?;
        hc.setup_interrupts();
        hc.run_controller()?;
        // Prove the command ring works before touching devices.
        hc.command(Trb::noop_command())?;
        kprintln!("xhci{}: running ({} interrupts), command ring verified with a no-op", index, hc.irq_mode);
        set_status(index, alloc::format!("xhci{index}: running, {} interrupts", hc.irq_mode));
        Ok(hc)
    }

    fn reset(&mut self) -> Result<()> {
        let cmd = self.op(op::USBCMD);
        let sts = self.op(op::USBSTS);
        let m = self.mmio;
        if m.r32(sts) & usbsts::HCH == 0 {
            m.w32(cmd, m.r32(cmd) & !usbcmd::RUN);
            if !self.poll_until(RESET_TIMEOUT_MS, |h| h.mmio.r32(sts) & usbsts::HCH != 0) {
                return Err(XhciError::Timeout("controller did not halt"));
            }
        }
        m.w32(cmd, usbcmd::HCRST);
        if !self.poll_until(RESET_TIMEOUT_MS, |h| h.mmio.r32(cmd) & usbcmd::HCRST == 0 && h.mmio.r32(sts) & usbsts::CNR == 0) {
            return Err(XhciError::Timeout("controller reset did not complete"));
        }
        if m.r32(self.op(op::PAGESIZE)) & 1 == 0 {
            return Err(XhciError::Unsupported("controller without 4 KiB page support"));
        }
        Ok(())
    }

    fn setup_structures(&mut self) -> Result<()> {
        let m = self.mmio;
        let ac64 = self.caps.ac64;
        m.w32(self.op(op::CONFIG), self.max_slots as u32);
        // Scratchpad buffers: an array of page pointers in DCBAA entry 0.
        let n = self.caps.max_scratchpad as usize;
        if n > 0 {
            if n > 512 {
                return Err(XhciError::Unsupported("more than 512 scratchpad buffers"));
            }
            let array = DmaPage::new(ac64)?;
            for i in 0..n {
                let page = DmaPage::new(ac64)?;
                array.write64(i * 8, page.phys());
                self._scratchpads.push(page);
            }
            self.dcbaa.write64(0, array.phys());
            self._scratchpad_array = Some(array);
        }
        m.w64(self.op(op::DCBAAP), self.dcbaa.phys());
        let (cmd_start, cycle) = self.cmd.start();
        m.w64(self.op(op::CRCR), cmd_start | u64::from(cycle));
        // Event ring: one segment, interrupter 0.
        let e = erst_entry(self.event_mem.phys(), self.events.segment_len() as u16);
        self._erst.write_dwords(0, &e);
        m.w32(self.ir(rt::ERSTSZ), 1);
        m.w64(self.ir(rt::ERDP), self.events.dequeue_phys());
        m.w64(self.ir(rt::ERSTBA), self._erst.phys());
        m.w32(self.ir(rt::IMOD), IMOD_INTERVAL);
        m.w32(self.ir(rt::IMAN), rt::IMAN_IE | rt::IMAN_IP);
        Ok(())
    }

    fn setup_interrupts(&mut self) {
        let Some(vector) = irq::allocate(irq_handler, self.index as u64) else {
            kprintln!("xhci{}: no MSI vector available, polling every timer tick", self.index);
            return;
        };
        let (address, data) = crate::arch::apic::msi_message(vector);
        let mode = match self.pci.enable_msix(address, data) {
            Ok(()) => Some("MSI-X"),
            Err(why) => {
                kprintln!("xhci{}: MSI-X unavailable ({}), trying MSI", self.index, why);
                match self.pci.enable_msi(address, data) {
                    Ok(()) => Some("MSI"),
                    Err(why) => {
                        kprintln!("xhci{}: MSI unavailable ({}), polling every timer tick", self.index, why);
                        None
                    }
                }
            }
        };
        match mode {
            Some(m) => {
                self.irq_vector = Some(vector);
                self.irq_mode = m;
                kprintln!("xhci{}: event interrupts via {} on vector {:#x}", self.index, m, vector);
            }
            None => irq::free(vector),
        }
    }

    fn run_controller(&mut self) -> Result<()> {
        let m = self.mmio;
        let mut cmd = usbcmd::RUN | usbcmd::HSEE;
        if self.irq_vector.is_some() {
            cmd |= usbcmd::INTE;
        }
        m.w32(self.op(op::USBCMD), cmd);
        let sts = self.op(op::USBSTS);
        if !self.poll_until(RESET_TIMEOUT_MS, |h| h.mmio.r32(sts) & usbsts::HCH == 0) {
            return Err(XhciError::Timeout("controller did not start"));
        }
        // Power every port if the controller lets software switch power.
        if self.caps.ppc {
            for p in 1..=self.caps.max_ports {
                let v = self.portsc(p);
                if v & portsc::PP == 0 {
                    self.write_portsc(p, portsc::neutral(v) | portsc::PP);
                }
            }
            sched::sleep_ticks(ms_to_ticks(20));
        }
        Ok(())
    }

    fn port_is_usb3(&self, port: u8) -> bool {
        self.protocols.iter().any(|p| p.major >= 3 && p.contains(port))
    }

    // ---- events ----------------------------------------------------------------------

    /// Consumes every pending event. Command completions are stored for
    /// their waiters, control endpoint events go to their device, interrupt
    /// endpoint events are handled at once (HID reports), port changes are
    /// queued for the main loop.
    fn pump(&mut self) -> usize {
        let m = self.mmio;
        // Acknowledge the interrupt before reading, so an event that arrives
        // meanwhile raises a new one.
        m.w32(self.ir(rt::IMAN), rt::IMAN_IE | rt::IMAN_IP);
        m.w32(self.op(op::USBSTS), usbsts::EINT);
        let mut n = 0;
        while let Some(ev) = self.events.pop(&self.event_mem) {
            n += 1;
            match ev.trb_type() {
                Some(TrbType::CommandCompletion) => {
                    let ptr = ev.param() & !0xF;
                    let _ = self.cmd.retire(ptr);
                    self.completions.insert(ptr, ev);
                }
                Some(TrbType::TransferEvent) => self.transfer_event(ev),
                Some(TrbType::PortStatusChange) => {
                    let p = ev.port_id();
                    if !self.pending_ports.contains(&p) {
                        self.pending_ports.push_back(p);
                    }
                }
                Some(TrbType::HostController) => {
                    kprintln!("xhci{}: host controller event: {:?}", self.index, ev.completion_code());
                }
                _ => {}
            }
        }
        if n > 0 {
            // New dequeue pointer; writing EHB clears the busy flag.
            m.w64(self.ir(rt::ERDP), self.events.dequeue_phys() | rt::ERDP_EHB);
        }
        let status = m.r32(self.op(op::USBSTS));
        if status & (usbsts::HSE | usbsts::HCE) != 0 {
            kprintln!("xhci{}: controller error, USBSTS {:#x}", self.index, status);
        }
        n
    }

    fn transfer_event(&mut self, ev: Trb) {
        let (slot, dci) = (ev.slot_id(), ev.endpoint_id());
        let Some(dev) = self.devices.get_mut(&slot) else { return }; // stale: device gone
        if dci == 1 {
            let _ = dev.ep0.retire(ev.param() & !0xF);
            dev.ep0_events.push(ev);
            return;
        }
        let Some(i) = dev.interrupt.iter().position(|e| e.dci == dci) else { return };
        let ep = &mut dev.interrupt[i];
        let ptr = ev.param() & !0xF;
        let _ = ep.ring.retire(ptr);
        if ep.pending != Some(ptr) {
            return;
        }
        ep.pending = None;
        let code = ev.completion_code();
        if code.is_ok() {
            ep.errors = 0;
            let len = (ep.len as u32).saturating_sub(ev.transfer_residual()) as usize;
            let mut data = [0u8; 64];
            ep.buffer.copy_out(&mut data[..len.min(64)]);
            if let Err(e) = ep.function.report(&data[..len.min(64)]) {
                if ep.function.bad_reports <= 3 {
                    kprintln!("usb {}: ignored a malformed HID report ({:?}, {} bytes)", dev.name, e, len);
                }
            }
        } else {
            ep.errors += 1;
            if ep.errors <= 3 || ep.errors == MAX_ENDPOINT_ERRORS {
                kprintln!("usb {}: interrupt endpoint {} transfer error {:?} ({} in a row)", dev.name, dci, code, ep.errors);
            }
            if code.halts_endpoint() {
                let s = slot;
                self.recover_endpoint(s, dci);
            }
        }
        let s = slot;
        self.queue_interrupt(s, dci);
    }

    /// Blocks until an interrupt (or one tick when polling) or `deadline`.
    fn wait_activity(&mut self, deadline_tick: u64) {
        let index = self.index;
        let seen = self.seen_irqs;
        let polling = self.irq_vector.is_none();
        let limit = if polling { (interrupts::ticks() + 1).min(deadline_tick) } else { deadline_tick };
        let events = &self.events;
        let mem = &self.event_mem;
        let _ = block_on(&[&WAKE[index]], Some(limit), || {
            (IRQS[index].load(Ordering::Relaxed) != seen || events.ready(mem)).then_some(())
        });
        self.seen_irqs = IRQS[index].load(Ordering::Relaxed);
    }

    // ---- commands and transfers ----------------------------------------------------------

    fn command(&mut self, trb: Trb) -> Result<Trb> {
        let ptr = self.cmd.push_td(&mut self.cmd_mem, &[trb])?;
        fence(Ordering::SeqCst);
        self.mmio.w32(self.caps.doorbell(0), 0);
        let deadline = interrupts::ticks() + ms_to_ticks(COMMAND_TIMEOUT_MS);
        loop {
            self.pump();
            if let Some(ev) = self.completions.remove(&ptr) {
                return match ev.completion_code() {
                    CompletionCode::Success => Ok(ev),
                    c => Err(XhciError::Command(c)),
                };
            }
            if interrupts::ticks() > deadline {
                self.abort_command();
                return Err(XhciError::Timeout("command"));
            }
            self.wait_activity(deadline);
        }
    }

    /// Stops a command the controller did not complete (xHCI 4.6.1.2).
    fn abort_command(&mut self) {
        let crcr = self.op(op::CRCR);
        self.mmio.w64(crcr, regs::crcr::CA);
        let stopped = self.poll_until(COMMAND_TIMEOUT_MS, |h| h.mmio.r64(crcr) & regs::crcr::CRR == 0);
        kprintln!("xhci{}: command timed out; ring {}", self.index, if stopped { "stopped" } else { "did not stop" });
        self.pump();
    }

    fn ring_doorbell(&self, slot: u8, target: u8) {
        fence(Ordering::SeqCst);
        self.mmio.w32(self.caps.doorbell(slot), target as u32);
    }

    /// A control transfer on the default endpoint; `data` is the OUT data
    /// or the IN buffer. Returns the bytes transferred in the data stage.
    fn control(&mut self, slot: u8, setup: SetupPacket, data: &mut [u8]) -> Result<usize> {
        let len = setup.length as usize;
        if len > data.len() || len > 4096 {
            return Err(XhciError::Unsupported("control transfer larger than 4 KiB"));
        }
        let dir_in = setup.is_in();
        let (status_ptr, data_present) = {
            let dev = self.devices.get_mut(&slot).ok_or(XhciError::BadSlot)?;
            if !dir_in && len > 0 {
                dev.buffer.copy_in(&data[..len]);
            }
            let mut trbs = vec![Trb::setup(&setup)];
            if len > 0 {
                trbs.push(Trb::data(dev.buffer.phys(), len as u32, dir_in));
            }
            trbs.push(Trb::status_stage(dir_in, len > 0));
            dev.ep0_events.clear();
            (dev.ep0.push_td(&mut dev.ep0_mem, &trbs)?, len > 0)
        };
        self.ring_doorbell(slot, 1);
        let deadline = interrupts::ticks() + ms_to_ticks(TRANSFER_TIMEOUT_MS);
        loop {
            self.pump();
            let dev = self.devices.get_mut(&slot).ok_or(XhciError::PortGone)?;
            if let Some(bad) = dev.ep0_events.iter().find(|e| !e.completion_code().is_ok()) {
                let code = bad.completion_code();
                dev.ep0_events.clear();
                if code.halts_endpoint() {
                    self.recover_endpoint(slot, 1);
                }
                return Err(XhciError::Transfer(code));
            }
            if dev.ep0_events.iter().any(|e| e.param() & !0xF == status_ptr) {
                let short = dev.ep0_events.iter().find(|e| e.completion_code() == CompletionCode::ShortPacket);
                let done = match short {
                    Some(e) if data_present => len.saturating_sub(e.transfer_residual() as usize),
                    _ => len,
                };
                dev.ep0_events.clear();
                if dir_in && done > 0 {
                    dev.buffer.copy_out(&mut data[..done]);
                }
                return Ok(done);
            }
            if interrupts::ticks() > deadline {
                return Err(XhciError::Timeout("control transfer"));
            }
            self.wait_activity(deadline);
        }
    }

    fn control_in(&mut self, slot: u8, setup: SetupPacket, buf: &mut [u8]) -> Result<usize> {
        self.control(slot, setup, buf)
    }

    fn control_out(&mut self, slot: u8, setup: SetupPacket, data: &[u8]) -> Result<()> {
        let mut copy = data.to_vec();
        self.control(slot, setup, &mut copy).map(|_| ())
    }

    /// Resets a halted endpoint and moves its dequeue pointer past whatever
    /// was in flight (xHCI 4.6.8, 4.6.10).
    fn recover_endpoint(&mut self, slot: u8, dci: u8) {
        if let Err(e) = self.command(Trb::reset_endpoint(slot, dci)) {
            kprintln!("xhci{}: reset of slot {} endpoint {} failed: {}", self.index, slot, dci, e);
            return;
        }
        let Some(dev) = self.devices.get_mut(&slot) else { return };
        let (ptr, cycle) = if dci == 1 {
            dev.ep0.reset_dequeue();
            dev.ep0.start()
        } else if let Some(ep) = dev.interrupt.iter_mut().find(|e| e.dci == dci) {
            ep.ring.reset_dequeue();
            ep.pending = None;
            ep.ring.start()
        } else {
            return;
        };
        if let Err(e) = self.command(Trb::set_tr_dequeue(ptr, cycle, slot, dci)) {
            kprintln!("xhci{}: moving the dequeue pointer of slot {} endpoint {} failed: {}", self.index, slot, dci, e);
        }
    }

    /// Queues the next interrupt IN transfer of an endpoint.
    fn queue_interrupt(&mut self, slot: u8, dci: u8) {
        let Some(dev) = self.devices.get_mut(&slot) else { return };
        let Some(ep) = dev.interrupt.iter_mut().find(|e| e.dci == dci) else { return };
        if ep.pending.is_some() || ep.errors >= MAX_ENDPOINT_ERRORS {
            return;
        }
        let trb = Trb::normal(ep.buffer.phys(), ep.len as u32, true);
        match ep.ring.push_td(&mut ep.mem, &[trb]) {
            Ok(ptr) => {
                ep.pending = Some(ptr);
                self.ring_doorbell(slot, dci);
            }
            Err(e) => kprintln!("usb {}: cannot queue an interrupt transfer: {:?}", dev.name, e),
        }
    }

    // ---- ports and devices --------------------------------------------------------------

    fn port_changed(&mut self, port: u8) {
        if port == 0 || port > self.caps.max_ports {
            return;
        }
        let v = self.portsc(port);
        // Acknowledge the change bits.
        self.write_portsc(port, portsc::neutral(v) | (v & portsc::CHANGE_BITS));
        let connected = v & portsc::CCS != 0;
        let attached = self.ports.contains_key(&port);
        let replaced = connected && attached && v & portsc::CSC != 0;
        if attached && (!connected || replaced) {
            self.detach(port);
        }
        if connected && (!attached || replaced) {
            match self.attach(port) {
                Ok(()) => {}
                Err(e) => {
                    kprintln!("usb: port {}: device not usable: {}", port, e);
                    if let Some(slot) = self.ports.get(&port).copied() {
                        self.release_slot(port, slot);
                    }
                }
            }
        }
        let n = self.devices.len();
        set_status(self.index, alloc::format!("xhci{}: running, {} interrupts, {} devices", self.index, self.irq_mode, n));
    }

    fn reset_port(&mut self, port: u8) -> Result<()> {
        let v = self.portsc(port);
        self.write_portsc(port, portsc::neutral(v) | portsc::PR);
        let ok = self.poll_until(PORT_RESET_TIMEOUT_MS, |h| {
            let v = h.portsc(port);
            v & portsc::PRC != 0 || v & portsc::CCS == 0
        });
        let v = self.portsc(port);
        self.write_portsc(port, portsc::neutral(v) | (v & portsc::CHANGE_BITS));
        if v & portsc::CCS == 0 {
            return Err(XhciError::PortGone);
        }
        if !ok {
            return Err(XhciError::Timeout("port reset"));
        }
        Ok(())
    }

    fn attach(&mut self, port: u8) -> Result<()> {
        let usb3 = self.port_is_usb3(port);
        if !usb3 || self.portsc(port) & portsc::PED == 0 {
            self.reset_port(port)?;
        }
        let v = self.portsc(port);
        if v & portsc::CCS == 0 {
            return Err(XhciError::PortGone);
        }
        if v & portsc::PED == 0 {
            return Err(XhciError::Unsupported("port not enabled after reset"));
        }
        let speed = Speed::from_psi(portsc::speed(v)).ok_or(XhciError::Unsupported("unknown port speed"))?;
        kprintln!("usb: port {}: {} device connected (USB {} port)", port, speed.name(), if usb3 { 3 } else { 2 });
        // Reset recovery time (USB 2.0 7.1.7.5: 10 ms).
        sched::sleep_ticks(ms_to_ticks(10));
        let ev = self.command(Trb::enable_slot(0))?;
        let slot = ev.slot_id();
        if slot == 0 || slot > self.max_slots {
            return Err(XhciError::BadSlot);
        }
        let dev = Device::new(port, speed, self.caps.ac64)?;
        self.dcbaa.write64(slot as usize * 8, dev.output.phys());
        self.devices.insert(slot, dev);
        self.ports.insert(port, slot);
        self.address_device(slot)?;
        self.enumerate(slot)
    }

    fn address_device(&mut self, slot: u8) -> Result<()> {
        let l = self.layout;
        let dev = self.devices.get_mut(&slot).ok_or(XhciError::BadSlot)?;
        dev.input.zero();
        dev.input.write_dwords(l.input_control(), &context::input_control(0b11, 0));
        let sc = SlotContext { route_string: 0, speed: dev.speed, context_entries: 1, root_port: dev.port, interrupter: 0 };
        dev.input.write_dwords(l.input_slot(), &sc.encode());
        let (deq, dcs) = dev.ep0.start();
        let ep0 = EndpointContext {
            ep_type: EndpointType::Control,
            max_packet: dev.ep0_max_packet,
            max_burst: 0,
            interval: 0,
            dequeue: deq,
            dcs,
            average_trb_length: 8,
            max_esit_payload: 0,
            error_count: 3,
        };
        dev.input.write_dwords(l.input_endpoint(1), &ep0.encode());
        let input = dev.input.phys();
        self.command(Trb::address_device(input, slot, false))?;
        let dev = &self.devices[&slot];
        let mut out = [0u32; 8];
        dev.output.read_dwords(l.device_slot(), &mut out);
        let st = context::SlotState::decode(&out);
        kprintln!("usb: port {}: slot {} addressed, USB address {}", dev.port, slot, st.device_address);
        Ok(())
    }

    /// Updates the default endpoint's max packet size (Evaluate Context).
    fn set_ep0_max_packet(&mut self, slot: u8, mps: u16) -> Result<()> {
        let l = self.layout;
        let dev = self.devices.get_mut(&slot).ok_or(XhciError::BadSlot)?;
        dev.ep0_max_packet = mps;
        dev.input.zero();
        dev.input.write_dwords(l.input_control(), &context::input_control(0b10, 0));
        let (deq, dcs) = dev.ep0.start();
        let ep0 = EndpointContext {
            ep_type: EndpointType::Control,
            max_packet: mps,
            max_burst: 0,
            interval: 0,
            dequeue: deq,
            dcs,
            average_trb_length: 8,
            max_esit_payload: 0,
            error_count: 3,
        };
        dev.input.write_dwords(l.input_endpoint(1), &ep0.encode());
        let input = dev.input.phys();
        self.command(Trb::evaluate_context(input, slot)).map(|_| ())
    }

    fn string(&mut self, slot: u8, index: u8, lang: u16) -> Option<String> {
        if index == 0 {
            return None;
        }
        let mut buf = [0u8; 255];
        let n = self.control_in(slot, SetupPacket::get_descriptor(descriptor::DESC_STRING, index, lang, 255), &mut buf).ok()?;
        descriptor::parse_string(&buf[..n]).ok().map(|s| String::from(s.trim()))
    }

    fn enumerate(&mut self, slot: u8) -> Result<()> {
        let usb3 = self.devices[&slot].speed.is_superspeed();
        // The first 8 bytes tell the real max packet size of endpoint 0.
        let mut head = [0u8; 8];
        let n = self.control_in(slot, SetupPacket::get_descriptor(descriptor::DESC_DEVICE, 0, 0, 8), &mut head)?;
        let raw = DeviceDescriptor::max_packet0_from_prefix(&head[..n])?;
        let mps = if usb3 { 1u16 << raw.min(15) } else { raw as u16 };
        if mps != 0 && mps != self.devices[&slot].ep0_max_packet {
            self.set_ep0_max_packet(slot, mps)?;
        }
        let mut dd = [0u8; DeviceDescriptor::LEN];
        let n = self.control_in(slot, SetupPacket::get_descriptor(descriptor::DESC_DEVICE, 0, 0, DeviceDescriptor::LEN as u16), &mut dd)?;
        let desc = DeviceDescriptor::parse(&dd[..n])?;
        let mut c9 = [0u8; 9];
        let n = self.control_in(slot, SetupPacket::get_descriptor(descriptor::DESC_CONFIGURATION, 0, 0, 9), &mut c9)?;
        let total = descriptor::configuration_total_length(&c9[..n])? as usize;
        if total > 4096 {
            return Err(XhciError::Unsupported("configuration descriptor larger than 4 KiB"));
        }
        let mut raw_cfg = vec![0u8; total];
        let n = self.control_in(slot, SetupPacket::get_descriptor(descriptor::DESC_CONFIGURATION, 0, 0, total as u16), &mut raw_cfg)?;
        let cfg = Configuration::parse(&raw_cfg[..n])?;
        let mut langs = [0u8; 4];
        let lang = self
            .control_in(slot, SetupPacket::get_descriptor(descriptor::DESC_STRING, 0, 0, 4), &mut langs)
            .ok()
            .and_then(|n| descriptor::first_language(&langs[..n]))
            .unwrap_or(0x0409);
        let manufacturer = self.string(slot, desc.manufacturer_index, lang).unwrap_or_default();
        let product = self.string(slot, desc.product_index, lang).unwrap_or_default();
        let port = self.devices[&slot].port;
        let name = if product.is_empty() { alloc::format!("{:04x}:{:04x}", desc.vendor, desc.product) } else { product.clone() };
        kprintln!(
            "usb: port {}: {:04x}:{:04x} \"{}\" \"{}\", USB {:x}.{:02x}, class {:02x}, {} configuration(s), {} interface(s), EP0 max packet {}",
            port,
            desc.vendor,
            desc.product,
            manufacturer,
            product,
            desc.usb_version >> 8,
            desc.usb_version & 0xFF,
            desc.class,
            desc.num_configurations,
            cfg.interfaces.len(),
            mps
        );
        self.devices.get_mut(&slot).expect("device exists").name = alloc::format!("{}-{} ({})", self.index + 1, port, name);
        // HID functions this driver supports.
        let hid: Vec<Interface> = cfg
            .default_interfaces()
            .filter(|i| i.class == descriptor::CLASS_HID && i.interrupt_in().is_some())
            .cloned()
            .collect();
        if hid.is_empty() {
            kprintln!("usb {}: no supported function (not HID); left unconfigured", self.devices[&slot].name);
            return Ok(());
        }
        self.control_out(slot, SetupPacket::set_configuration(cfg.value), &[])?;
        let mut configured = Vec::new();
        for iface in &hid {
            match self.setup_hid(slot, iface, &name) {
                Ok(Some(f)) => configured.push((iface.clone(), f)),
                Ok(None) => {}
                Err(e) => kprintln!("usb {}: interface {}: {}", self.devices[&slot].name, iface.number, e),
            }
        }
        if configured.is_empty() {
            return Ok(());
        }
        self.configure_endpoints(slot, configured)
    }

    /// Prepares a HID interface: protocol, idle rate, report layout.
    fn setup_hid(&mut self, slot: u8, iface: &Interface, name: &str) -> Result<Option<HidFunction>> {
        let devname = self.devices[&slot].name.clone();
        let boot = iface.subclass == descriptor::HID_SUBCLASS_BOOT;
        if iface.is_hid_boot(descriptor::HID_PROTOCOL_KEYBOARD) {
            self.control_out(slot, SetupPacket::hid_set_protocol(iface.number, false), &[])?;
            // Report only on change; some keyboards stall this request.
            let _ = self.control_out(slot, SetupPacket::hid_set_idle(iface.number, 0), &[]);
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
        let len = iface.hid.map_or(0, |h| h.report_length).min(1024);
        if len > 0 {
            let mut rd = vec![0u8; len as usize];
            match self.control_in(slot, SetupPacket::get_interface_descriptor(descriptor::DESC_HID_REPORT, iface.number, len), &mut rd) {
                Ok(n) => match parse_report_descriptor(&rd[..n]) {
                    Ok(fields) => layout = MouseLayout::from_fields(&fields),
                    Err(e) => kprintln!("usb {}: interface {}: report descriptor rejected: {:?}", devname, iface.number, e),
                },
                Err(e) => kprintln!("usb {}: interface {}: no report descriptor: {}", devname, iface.number, e),
            }
        }
        let is_boot_mouse = iface.is_hid_boot(descriptor::HID_PROTOCOL_MOUSE);
        let how = match (&layout, is_boot_mouse) {
            (Some(l), _) => {
                if boot {
                    self.control_out(slot, SetupPacket::hid_set_protocol(iface.number, true), &[])?;
                }
                alloc::format!(
                    "report protocol: {} buttons, {}-bit X/Y{}{}",
                    l.buttons.len(),
                    l.x.size,
                    if l.wheel.is_some() { ", wheel" } else { "" },
                    if l.pan.is_some() { ", horizontal wheel" } else { "" }
                )
            }
            (None, true) => {
                self.control_out(slot, SetupPacket::hid_set_protocol(iface.number, false), &[])?;
                String::from("boot protocol: 3 buttons, 8-bit X/Y")
            }
            (None, false) => {
                kprintln!(
                    "usb {}: HID interface {} (subclass {}, protocol {}) is not a boot keyboard or relative pointer: not supported",
                    devname,
                    iface.number,
                    iface.subclass,
                    iface.protocol
                );
                return Ok(None);
            }
        };
        let _ = self.control_out(slot, SetupPacket::hid_set_idle(iface.number, 0), &[]);
        let id = input::register(name, input::class::MOUSE);
        kprintln!("usb {}: HID mouse on interface {} ({}) -> input device {}", devname, iface.number, how, id);
        Ok(Some(HidFunction { interface: iface.number, input_id: id, kind: HidKind::Mouse { layout, buttons: 0 }, reports: 0, bad_reports: 0 }))
    }

    /// Configure Endpoint for every HID interrupt IN endpoint, then starts
    /// their transfers.
    fn configure_endpoints(&mut self, slot: u8, functions: Vec<(Interface, HidFunction)>) -> Result<()> {
        let l = self.layout;
        let ac64 = self.caps.ac64;
        let dev = self.devices.get_mut(&slot).ok_or(XhciError::BadSlot)?;
        dev.input.zero();
        let mut add = 1u32; // slot context
        let mut max_dci = 1;
        let mut endpoints = Vec::new();
        for (iface, function) in functions {
            let ep = *iface.interrupt_in().expect("filtered for an interrupt IN endpoint");
            let dci = osyx_usb::xhci::dci(ep.address);
            let mem = DmaRing::new(ac64)?;
            let ring = ProducerRing::new(&mem);
            let (deq, dcs) = ring.start();
            let ep_type = EndpointType::InterruptIn;
            let max_packet = ep.max_packet.max(1);
            let payload = max_packet as u32 * (ep.mult as u32 + 1) * (ep.max_burst as u32 + 1);
            let ctx = EndpointContext {
                ep_type,
                max_packet,
                max_burst: ep.max_burst,
                interval: context::periodic_interval(dev.speed, ep_type, ep.interval),
                dequeue: deq,
                dcs,
                average_trb_length: max_packet,
                max_esit_payload: payload,
                error_count: 3,
            };
            dev.input.write_dwords(l.input_endpoint(dci), &ctx.encode());
            add |= 1 << dci;
            max_dci = max_dci.max(dci);
            kprintln!(
                "usb {}: endpoint {:#04x} (DCI {}): interrupt IN, {} bytes, bInterval {}, xHCI interval {}",
                dev.name,
                ep.address,
                dci,
                max_packet,
                ep.interval,
                ctx.interval
            );
            endpoints.push(InterruptEndpoint {
                dci,
                ring,
                mem,
                buffer: DmaPage::new(ac64)?,
                len: max_packet.min(64),
                pending: None,
                errors: 0,
                function,
            });
        }
        dev.input.write_dwords(l.input_control(), &context::input_control(add, 0));
        let sc = SlotContext { route_string: 0, speed: dev.speed, context_entries: max_dci, root_port: dev.port, interrupter: 0 };
        dev.input.write_dwords(l.input_slot(), &sc.encode());
        let input = dev.input.phys();
        dev.interrupt = endpoints;
        if let Err(e) = self.command(Trb::configure_endpoint(input, slot, false)) {
            // The functions were registered with the input core: undo that.
            if let Some(dev) = self.devices.get_mut(&slot) {
                for ep in dev.interrupt.drain(..) {
                    input::unregister(ep.function.input_id);
                }
            }
            return Err(e);
        }
        let dcis: Vec<u8> = self.devices[&slot].interrupt.iter().map(|e| e.dci).collect();
        for dci in dcis {
            self.queue_interrupt(slot, dci);
        }
        Ok(())
    }

    fn detach(&mut self, port: u8) {
        let Some(slot) = self.ports.get(&port).copied() else { return };
        let name = self.devices.get(&slot).map_or(String::new(), |d| d.name.clone());
        kprintln!("usb {}: disconnected from port {}", name, port);
        self.release_slot(port, slot);
    }

    /// Tells the input core the functions are gone, disables the slot (the
    /// controller stops every endpoint) and only then frees the memory the
    /// controller used.
    fn release_slot(&mut self, port: u8, slot: u8) {
        if let Some(dev) = self.devices.get_mut(&slot) {
            for ep in &dev.interrupt {
                input::unregister(ep.function.input_id);
            }
            let reports: u64 = dev.interrupt.iter().map(|e| e.function.reports).sum();
            if !dev.interrupt.is_empty() {
                kprintln!("usb {}: {} HID reports processed", dev.name, reports);
            }
        }
        match self.command(Trb::disable_slot(slot)) {
            Ok(_) => {}
            Err(e) => kprintln!("xhci{}: disable slot {} failed: {}", self.index, slot, e),
        }
        self.dcbaa.write64(slot as usize * 8, 0);
        self.devices.remove(&slot);
        self.ports.remove(&port);
        kprintln!("usb: port {}: slot {} released", port, slot);
    }

    /// Applies the keyboard LED state of the input core (Caps/Num Lock).
    fn update_leds(&mut self) {
        let want = input::leds();
        let mut todo = Vec::new();
        for (slot, dev) in &self.devices {
            for ep in &dev.interrupt {
                if let HidKind::Keyboard { applied_leds, .. } = ep.function.kind {
                    if applied_leds != want {
                        todo.push((*slot, ep.dci, ep.function.interface));
                    }
                }
            }
        }
        for (slot, dci, iface) in todo {
            let ok = self.control_out(slot, SetupPacket::hid_set_output_report(iface, 0, 1), &[want]).is_ok();
            if let Some(ep) = self.devices.get_mut(&slot).and_then(|d| d.interrupt.iter_mut().find(|e| e.dci == dci)) {
                if let HidKind::Keyboard { applied_leds, .. } = &mut ep.function.kind {
                    // A keyboard without LEDs may stall: do not retry forever.
                    *applied_leds = want;
                }
            }
            if !ok {
                kprintln!("usb: keyboard in slot {} did not accept the LED report", slot);
            }
        }
    }

    fn run(&mut self) -> ! {
        for p in 1..=self.caps.max_ports {
            if self.portsc(p) & portsc::CCS != 0 && !self.pending_ports.contains(&p) {
                self.pending_ports.push_back(p);
            }
        }
        loop {
            self.pump();
            while let Some(p) = self.pending_ports.pop_front() {
                self.port_changed(p);
                self.pump();
            }
            self.update_leds();
            let deadline = interrupts::ticks() + ms_to_ticks(1000);
            self.wait_activity(deadline);
        }
    }
}

/// USB legacy support: asks the firmware to release the controller and
/// disables its SMIs (xHCI 4.22.1).
fn take_ownership(index: usize, m: Mmio, off: u64) {
    let v = m.r32(off);
    if v & ext::BIOS_OWNED != 0 {
        m.w32(off, v | ext::OS_OWNED);
        let deadline = interrupts::ticks() + ms_to_ticks(1000);
        while m.r32(off) & ext::BIOS_OWNED != 0 && interrupts::ticks() < deadline {
            sched::sleep_ticks(1);
        }
        if m.r32(off) & ext::BIOS_OWNED != 0 {
            kprintln!("xhci{}: firmware did not release the controller; taking it anyway", index);
            m.w32(off, (m.r32(off) & !ext::BIOS_OWNED) | ext::OS_OWNED);
        } else {
            kprintln!("xhci{}: ownership handed over by the firmware", index);
        }
    } else {
        m.w32(off, v | ext::OS_OWNED);
    }
    let ctl = m.r32(off + 4);
    m.w32(off + 4, (ctl & ext::LEGCTL_KEEP) | ext::LEGCTL_SMI_EVENTS);
}

/// Number of xHCI controllers found.
pub fn controllers() -> usize {
    CONTROLLERS.load(Ordering::Relaxed).min(MAX_CONTROLLERS)
}

/// Interrupts received by controller `index`.
pub fn interrupts_of(index: usize) -> u64 {
    IRQS.get(index).map_or(0, |c| c.load(Ordering::Relaxed))
}
