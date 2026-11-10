//! virtio-input (virtio specification section 5.8): the keyboards, mice
//! and tablets a hypervisor gives a virtual machine without emulating USB
//! or PS/2 hardware (QEMU's `virtio-keyboard-pci`, `virtio-mouse-pci`,
//! `virtio-tablet-pci`).
//!
//! A device sends Linux evdev events, eight bytes each, into buffers the
//! driver puts on its event queue. `osyx_input::evdev` turns them into
//! what the input core takes; this file is the transport: the device's
//! name and absolute ranges from its configuration space, the event queue,
//! and one kernel thread that collects the events of every device four
//! times per timer tick at most (the queues are polled; an idle device
//! costs one memory read per round).

use super::virtio::{AsyncQueue, Buffer, Transport, VirtioError};
use crate::input;
use crate::mem::{alloc_frame, phys_to_virt};
use crate::pci;
use crate::sync::IrqSpinLock;
use alloc::string::String;
use alloc::vec::Vec;
use osyx_input::evdev::{self, Action, Decoder};

/// The modern virtio-input PCI device id (0x1040 + device type 18); there
/// is no transitional one.
pub const DEVICE_ID: u16 = 0x1052;

// Configuration space (struct virtio_input_config): what to look at, and
// the answer's size and bytes.
const CFG_SELECT: u64 = 0;
const CFG_SUBSEL: u64 = 1;
const CFG_SIZE: u64 = 2;
const CFG_DATA: u64 = 8;
const SELECT_NAME: u8 = 0x01;
const SELECT_EV_BITS: u8 = 0x11;
const SELECT_ABS_INFO: u8 = 0x12;

const EVENT_QUEUE: u16 = 0;
const EVENT_BYTES: u32 = 8;
/// Event buffers kept with the device (one page holds 512).
const BUFFERS: u16 = 64;

struct Device {
    name: String,
    input_id: u32,
    decoder: Decoder,
    queue: AsyncQueue,
    /// The buffer behind each descriptor head.
    buffer_of: Vec<u64>,
    events: u64,
    _transport: Transport,
}

// SAFETY: a device is only touched under `DEVICES`.
unsafe impl Send for Device {}

static DEVICES: IrqSpinLock<Vec<Device>> = IrqSpinLock::new(Vec::new());

/// Selects a configuration item; returns the size of the answer.
fn select(t: &Transport, select: u8, subsel: u8) -> usize {
    t.write_device_config_u8(CFG_SELECT, select);
    t.write_device_config_u8(CFG_SUBSEL, subsel);
    t.device_config_u8(CFG_SIZE) as usize
}

/// Whether the device sends events of `kind` with `code`.
fn has_code(t: &Transport, kind: u16, code: u16) -> bool {
    let size = select(t, SELECT_EV_BITS, kind as u8);
    let byte = code as usize / 8;
    byte < size && t.device_config_u8(CFG_DATA + byte as u64) & (1 << (code % 8)) != 0
}

/// The largest value of absolute axis `axis` (0 if the device has none).
fn abs_max(t: &Transport, axis: u16) -> u32 {
    // struct virtio_input_absinfo: min, max, fuzz, flat, res (le32 each).
    if select(t, SELECT_ABS_INFO, axis as u8) < 8 {
        return 0;
    }
    t.device_config_u32(CFG_DATA + 4)
}

fn probe(dev: &pci::Device) -> Result<Device, VirtioError> {
    let t = Transport::new(dev)?;
    t.negotiate(0)?;
    let size = select(&t, SELECT_NAME, 0).min(64);
    let name: String = (0..size as u64).map(|i| t.device_config_u8(CFG_DATA + i)).take_while(|&b| b != 0).map(|b| b as char).collect();
    let keyboard = has_code(&t, evdev::EV_KEY, evdev::KEY_A);
    let relative = has_code(&t, evdev::EV_REL, evdev::REL_X);
    let range = (abs_max(&t, evdev::ABS_X), abs_max(&t, evdev::ABS_Y));
    let absolute = has_code(&t, evdev::EV_ABS, evdev::ABS_X) && range.0 > 0 && range.1 > 0;
    let mut queue = t.setup_async_queue(EVENT_QUEUE, None)?;
    // Event buffers: eight bytes each, all in one page.
    let page = alloc_frame().ok_or(VirtioError::OutOfMemory)?;
    let mut buffer_of = alloc::vec![0u64; queue.size() as usize];
    for i in 0..BUFFERS.min(queue.size()) as u64 {
        let phys = page + i * EVENT_BYTES as u64;
        if let Some(head) = queue.add(&[Buffer { phys, len: EVENT_BYTES, device_writes: true }]) {
            buffer_of[head as usize] = phys;
        }
    }
    t.driver_ok();
    queue.kick();
    let class = if keyboard && !relative && !absolute { input::class::KEYBOARD } else { input::class::MOUSE };
    let input_id = input::register(if name.is_empty() { "virtio-input" } else { &name }, class);
    crate::kprintln!(
        "virtio-input at {}: \"{}\" ({}) -> input device {}",
        dev.addr,
        name,
        match (keyboard, relative, absolute) {
            (_, _, true) => alloc::format!("absolute pointer, range {}x{}", range.0, range.1),
            (_, true, false) => String::from("relative pointer"),
            (true, false, false) => String::from("keyboard"),
            _ => String::from("no keys or axes this system uses"),
        },
        input_id
    );
    Ok(Device {
        name,
        input_id,
        decoder: Decoder::new(if absolute { range } else { (0, 0) }),
        queue,
        buffer_of,
        events: 0,
        _transport: t,
    })
}

/// Attaches every virtio-input device and starts the thread that reads
/// them. Returns how many were attached.
pub fn init(devices: &[pci::Device]) -> usize {
    let mut found = Vec::new();
    for d in devices.iter().filter(|d| d.vendor == super::virtio::VENDOR && d.device == DEVICE_ID) {
        match probe(d) {
            Ok(dev) => found.push(dev),
            Err(e) => crate::kprintln!("virtio-input at {}: probe failed: {:?}", d.addr, e),
        }
    }
    let n = found.len();
    if n > 0 {
        *DEVICES.lock() = found;
        crate::sched::spawn_kernel("virtio-input", service_thread, 0, 0);
    }
    n
}

/// One round over a device: decoded events since the last one.
fn collect(dev: &mut Device, out: &mut Vec<(u32, Action)>) {
    let id = dev.input_id;
    let mut returned = false;
    while let Some((head, len)) = dev.queue.pop_used() {
        let phys = dev.buffer_of.get(head as usize).copied().unwrap_or(0);
        if phys == 0 {
            continue;
        }
        if len >= EVENT_BYTES {
            // SAFETY: the buffer is this driver's and the device returned it.
            let (kind, code, value) = unsafe {
                let p = phys_to_virt(phys) as *const u8;
                (
                    core::ptr::read_unaligned(p as *const u16),
                    core::ptr::read_unaligned(p.add(2) as *const u16),
                    core::ptr::read_unaligned(p.add(4) as *const u32),
                )
            };
            dev.events += 1;
            dev.decoder.event(u16::from_le(kind), u16::from_le(code), u32::from_le(value), &mut |a| out.push((id, a)));
        }
        // Back to the device for the next event.
        if let Some(new_head) = dev.queue.add(&[Buffer { phys, len: EVENT_BYTES, device_writes: true }]) {
            dev.buffer_of[new_head as usize] = phys;
            returned = true;
        }
    }
    if returned {
        dev.queue.kick();
    }
}

fn service_thread(_: u64, _: u64) {
    // About 250 rounds a second: finer than a frame, coarser than a tick
    // on a fast timer.
    let ticks = (crate::arch::timer::TIMER_HZ as u64 / 250).max(1);
    let mut actions: Vec<(u32, Action)> = Vec::new();
    loop {
        {
            let mut devices = DEVICES.lock();
            for dev in devices.iter_mut() {
                if dev.queue.has_used() {
                    collect(dev, &mut actions);
                }
            }
        }
        // The input core is called without this driver's lock held.
        for (id, action) in actions.drain(..) {
            match action {
                Action::Key { usage, down } => input::key(id, usage, down),
                Action::Button { number, down } => input::button(id, number, down),
                Action::Motion { dx, dy } => input::motion(id, dx, dy),
                Action::Absolute { x, y } => input::absolute(id, x, y),
                Action::Wheel { vertical, horizontal } => input::wheel(id, vertical, horizontal),
            }
        }
        crate::sched::sleep_ticks(ticks);
    }
}

/// One line per device for the kernel's status report.
pub fn status() -> Vec<String> {
    DEVICES.lock().iter().map(|d| alloc::format!("virtio-input \"{}\": input device {}, {} events", d.name, d.input_id, d.events)).collect()
}
