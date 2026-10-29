//! Bluetooth controllers on USB (Bluetooth Core, Vol 4, Part B): HCI
//! commands go out on the default control endpoint, events come in on the
//! interrupt IN endpoint and ACL data on the bulk pair of interface 0.
//! The SCO interface (isochronous) is left at its zero-bandwidth setting.
//!
//! Incoming endpoints are byte streams: a packet may arrive in pieces of
//! the endpoint's max packet size, or several in one transfer, so they are
//! reassembled from the HCI headers. Packets are handed to
//! [`crate::bluetooth`], which also queues what is sent.

use super::dma::{DmaPage, DmaRing};
use super::{Xhci, XhciError};
use crate::bluetooth::{self, Incoming, Outgoing};
use crate::kprintln;
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use osyx_usb::descriptor::{EndpointDescriptor, Interface, SetupPacket};
use osyx_usb::xhci::context::{self, EndpointContext, EndpointType};
use osyx_usb::xhci::ring::ProducerRing;
use osyx_usb::xhci::trb::Trb;

/// Interface class of a Bluetooth controller: wireless, RF, Bluetooth.
pub(super) const CLASS: (u8, u8, u8) = (0xE0, 0x01, 0x01);
/// Largest packet accepted from the controller (ACL header + 64 KiB is
/// allowed by the spec; controllers use far less).
const MAX_PACKET: usize = 4 + 4096;
/// Consecutive failed transfers after which an endpoint is given up.
const MAX_ERRORS: u32 = 20;

struct Pipe {
    dci: u8,
    ring: ProducerRing,
    mem: DmaRing,
    buffer: DmaPage,
    /// Bytes per transfer.
    len: u32,
    /// TRB of the transfer in flight.
    pending: Option<u64>,
    errors: u32,
}

impl Pipe {
    fn new(ep: &EndpointDescriptor, ac64: bool, len: u32) -> Result<Pipe, XhciError> {
        let mem = DmaRing::new(ac64)?;
        let ring = ProducerRing::new(&mem);
        Ok(Pipe { dci: osyx_usb::xhci::dci(ep.address), ring, mem, buffer: DmaPage::new(ac64)?, len, pending: None, errors: 0 })
    }

    /// Queues a transfer of `len` bytes from (OUT) or into (IN) the buffer.
    fn queue(&mut self, len: u32) -> Result<(), XhciError> {
        let ptr = self.ring.push_td(&mut self.mem, &[Trb::normal(self.buffer.phys(), len, true)])?;
        self.pending = Some(ptr);
        Ok(())
    }
}

/// Splits a byte stream into HCI packets: `header` bytes, of which the
/// payload length is read by `len_of`.
struct Reassembler {
    buf: Vec<u8>,
    header: usize,
    len_of: fn(&[u8]) -> usize,
}

impl Reassembler {
    fn feed(&mut self, data: &[u8], mut out: impl FnMut(Vec<u8>)) {
        self.buf.extend_from_slice(data);
        loop {
            if self.buf.len() < self.header {
                return;
            }
            let total = self.header + (self.len_of)(&self.buf);
            if total > MAX_PACKET {
                // Lost framing: drop what we have and resynchronise on the
                // next transfer.
                self.buf.clear();
                return;
            }
            if self.buf.len() < total {
                return;
            }
            let rest = self.buf.split_off(total);
            out(core::mem::replace(&mut self.buf, rest));
        }
    }
}

fn event_len(h: &[u8]) -> usize {
    h[1] as usize
}

fn acl_len(h: &[u8]) -> usize {
    u16::from_le_bytes([h[2], h[3]]) as usize
}

pub(super) struct BtState {
    unit: usize,
    interface: u8,
    events: Pipe,
    acl_in: Pipe,
    acl_out: Pipe,
    event_stream: Reassembler,
    acl_stream: Reassembler,
    /// Bulk OUT packets waiting for the one in flight.
    out_queue: VecDeque<Vec<u8>>,
}

impl BtState {
    pub(super) fn owns(&self, dci: u8) -> bool {
        dci == self.events.dci || dci == self.acl_in.dci || dci == self.acl_out.dci
    }

    pub(super) fn unit(&self) -> usize {
        self.unit
    }
}

impl Xhci {
    /// Configures a Bluetooth controller (interface 0: interrupt IN, bulk
    /// IN and OUT) and registers it with the Bluetooth core.
    pub(super) fn setup_bt(&mut self, slot: u8, config: u8, iface: &Interface, vid: u16, pid: u16) -> Result<(), XhciError> {
        let (Some(ev), Some(bin), Some(bout)) = (iface.interrupt_in(), iface.bulk_in(), iface.bulk_out()) else {
            return Err(XhciError::Unsupported("Bluetooth interface without its three endpoints"));
        };
        let (ev, bin, bout) = (*ev, *bin, *bout);
        self.control_out(slot, SetupPacket::set_configuration(config), &[])?;
        let l = self.layout;
        let ac64 = self.caps.ac64;
        let dev = self.devices.get_mut(&slot).ok_or(XhciError::BadSlot)?;
        let events = Pipe::new(&ev, ac64, ev.max_packet.max(1) as u32)?;
        let acl_in = Pipe::new(&bin, ac64, 4096)?;
        let acl_out = Pipe::new(&bout, ac64, 0)?;
        dev.input.zero();
        let mut add = 1u32;
        let mut max_dci = 1;
        for (ep, pipe, ep_type) in
            [(&ev, &events, EndpointType::InterruptIn), (&bin, &acl_in, EndpointType::BulkIn), (&bout, &acl_out, EndpointType::BulkOut)]
        {
            let (dequeue, dcs) = pipe.ring.start();
            let max_packet = ep.max_packet.max(1);
            let periodic = ep_type == EndpointType::InterruptIn;
            let ctx = EndpointContext {
                ep_type,
                max_packet,
                max_burst: ep.max_burst,
                interval: if periodic { context::periodic_interval(dev.speed, ep_type, ep.interval) } else { 0 },
                dequeue,
                dcs,
                average_trb_length: max_packet,
                max_esit_payload: if periodic { max_packet as u32 * (ep.mult as u32 + 1) * (ep.max_burst as u32 + 1) } else { 0 },
                error_count: 3,
            };
            dev.input.write_dwords(l.input_endpoint(pipe.dci), &ctx.encode());
            add |= 1 << pipe.dci;
            max_dci = max_dci.max(pipe.dci);
        }
        dev.input.write_dwords(l.input_control(), &context::input_control(add, 0));
        let sc = dev.slot_context(max_dci);
        dev.input.write_dwords(l.input_slot(), &sc.encode());
        let input = dev.input.phys();
        let name = dev.name.clone();
        self.command(Trb::configure_endpoint(input, slot, false))?;
        let unit = bluetooth::register(self.index, slot, vid, pid, &name);
        kprintln!(
            "usb {}: Bluetooth controller hci{} (events on DCI {}, ACL on DCI {}/{})",
            name,
            unit,
            events.dci,
            acl_in.dci,
            acl_out.dci
        );
        let state = BtState {
            unit,
            interface: iface.number,
            events,
            acl_in,
            acl_out,
            event_stream: Reassembler { buf: Vec::new(), header: 2, len_of: event_len },
            acl_stream: Reassembler { buf: Vec::new(), header: 4, len_of: acl_len },
            out_queue: VecDeque::new(),
        };
        let dev = self.devices.get_mut(&slot).ok_or(XhciError::BadSlot)?;
        dev.bt = Some(state);
        self.bt_queue_in(slot);
        Ok(())
    }

    /// Keeps a transfer pending on both IN endpoints.
    fn bt_queue_in(&mut self, slot: u8) {
        let Some(bt) = self.devices.get_mut(&slot).and_then(|d| d.bt.as_mut()) else { return };
        let mut ring = Vec::new();
        for pipe in [&mut bt.events, &mut bt.acl_in] {
            if pipe.pending.is_none() && pipe.errors < MAX_ERRORS {
                match pipe.queue(pipe.len) {
                    Ok(()) => ring.push(pipe.dci),
                    Err(e) => kprintln!("bluetooth: hci{}: cannot queue a transfer: {}", bt.unit, e),
                }
            }
        }
        for dci in ring {
            self.ring_doorbell(slot, dci);
        }
    }

    /// Starts the next queued bulk OUT packet when none is in flight.
    fn bt_send_next(&mut self, slot: u8) {
        let Some(bt) = self.devices.get_mut(&slot).and_then(|d| d.bt.as_mut()) else { return };
        if bt.acl_out.pending.is_some() {
            return;
        }
        let Some(pkt) = bt.out_queue.pop_front() else { return };
        let len = pkt.len().min(4096);
        bt.acl_out.buffer.copy_in(&pkt[..len]);
        match bt.acl_out.queue(len as u32) {
            Ok(()) => {
                let dci = bt.acl_out.dci;
                self.ring_doorbell(slot, dci);
            }
            Err(e) => kprintln!("bluetooth: hci{}: cannot queue ACL data: {}", bt.unit, e),
        }
    }

    /// A transfer event of one of a Bluetooth controller's endpoints.
    pub(super) fn bt_transfer_event(&mut self, slot: u8, ev: Trb) {
        let dci = ev.endpoint_id();
        let ptr = ev.param() & !0xF;
        let code = ev.completion_code();
        let Some(bt) = self.devices.get_mut(&slot).and_then(|d| d.bt.as_mut()) else { return };
        let unit = bt.unit;
        let pipe = if dci == bt.events.dci {
            &mut bt.events
        } else if dci == bt.acl_in.dci {
            &mut bt.acl_in
        } else {
            &mut bt.acl_out
        };
        let _ = pipe.ring.retire(ptr);
        if pipe.pending != Some(ptr) {
            return;
        }
        pipe.pending = None;
        if !code.is_ok() {
            pipe.errors += 1;
            if pipe.errors <= 3 || pipe.errors == MAX_ERRORS {
                kprintln!("bluetooth: hci{}: endpoint {} transfer error {:?} ({} in a row)", unit, dci, code, pipe.errors);
            }
            if code.halts_endpoint() {
                self.recover_bt_endpoint(slot, dci);
            }
        } else {
            pipe.errors = 0;
            let n = (pipe.len.saturating_sub(ev.transfer_residual()) as usize).min(4096);
            if dci != bt.acl_out.dci {
                let mut data = alloc::vec![0u8; n];
                let pipe = if dci == bt.events.dci { &bt.events } else { &bt.acl_in };
                pipe.buffer.copy_out(&mut data);
                // Intel controllers in bootloader mode answer on the bulk
                // endpoint with events.
                let events = dci == bt.events.dci || bluetooth::bulk_carries_events(unit);
                let stream = if events { &mut bt.event_stream } else { &mut bt.acl_stream };
                stream.feed(&data, |p| bluetooth::deliver(unit, if events { Incoming::Event(p) } else { Incoming::Acl(p) }));
            }
        }
        self.bt_queue_in(slot);
        self.bt_send_next(slot);
    }

    fn recover_bt_endpoint(&mut self, slot: u8, dci: u8) {
        if let Err(e) = self.command(Trb::reset_endpoint(slot, dci)) {
            kprintln!("xhci{}: reset of slot {} endpoint {} failed: {}", self.index, slot, dci, e);
            return;
        }
        let Some(bt) = self.devices.get_mut(&slot).and_then(|d| d.bt.as_mut()) else { return };
        let pipe = if dci == bt.events.dci {
            &mut bt.events
        } else if dci == bt.acl_in.dci {
            &mut bt.acl_in
        } else {
            &mut bt.acl_out
        };
        pipe.ring.reset_dequeue();
        pipe.pending = None;
        let (ptr, cycle) = pipe.ring.start();
        if let Err(e) = self.command(Trb::set_tr_dequeue(ptr, cycle, slot, dci)) {
            kprintln!("xhci{}: moving the dequeue pointer of slot {} endpoint {} failed: {}", self.index, slot, dci, e);
        }
    }

    /// Sends what the Bluetooth core queued for this controller's devices.
    pub(super) fn service_bt(&mut self) {
        while let Some((slot, pkt)) = bluetooth::take_tx(self.index) {
            let Some(bt) = self.devices.get_mut(&slot).and_then(|d| d.bt.as_mut()) else { continue };
            let (unit, iface) = (bt.unit, bt.interface);
            match pkt {
                Outgoing::Command(cmd) => {
                    // Class request to the interface (Vol 4, Part B, 2.2).
                    let setup = SetupPacket { request_type: 0x20, request: 0, value: 0, index: iface as u16, length: cmd.len() as u16 };
                    if let Err(e) = self.control_out(slot, setup, &cmd) {
                        kprintln!("bluetooth: hci{}: command {:#06x} not sent: {}", unit, u16::from_le_bytes([cmd[0], cmd[1]]), e);
                        bluetooth::deliver(unit, Incoming::SendFailed);
                    }
                }
                Outgoing::Bulk(data) => {
                    bt.out_queue.push_back(data);
                    self.bt_send_next(slot);
                }
            }
        }
    }
}
