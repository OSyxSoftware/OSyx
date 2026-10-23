//! The OSyx input core: one stream of raw input events from every device.
//!
//! Device drivers (the USB HID keyboard and mouse drivers) [`register`] a
//! device and report key changes, motion, buttons and wheel steps. The core
//! adds what is per seat rather than per device: modifier and lock state
//! (carried in every event), key auto-repeat (a kernel thread), keyboard LED
//! state for the drivers to apply, and clean-up when a device disappears
//! (held keys and buttons are released, a `DEVICE_REMOVED` event follows).
//!
//! Events go to two kinds of bounded queues, both readable as character
//! devices (whole 32-byte `osyx_input::InputEvent` records; a read blocks
//! until at least one event is there):
//! * `/dev/input/event<N>`: one device's events (root only), removed with
//!   the device;
//! * `/dev/input/seat0`: every device's events plus device arrival and
//!   removal, for the graphical session. It can be open only once at a time
//!   (exclusive routing to the compositor) and only by root.
//!
//! A full queue drops its oldest event and the reader receives a `DROPPED`
//! event with the count before the next one. Raw events never contain
//! text: keyboard layouts are applied by the reader.

use crate::devices::{self, CharDevice, DeviceKind};
use crate::sync::IrqSpinLock;
use crate::waitq::{block_on, WaitQueue};
use crate::{interrupts, kprintln, sched};
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use osyx_abi::ipc::{WAIT_HANGUP, WAIT_READABLE};
use osyx_abi::Errno;
use osyx_input::repeat::{RepeatConfig, Repeater};
use osyx_input::{kind, InputEvent, ModifierState};

const DEVICE_QUEUE: usize = 256;
const SEAT_QUEUE: usize = 1024;

pub use osyx_input::class;

/// A bounded event queue behind a character device.
pub struct EventQueue {
    state: IrqSpinLock<QueueState>,
    waitq: WaitQueue,
    capacity: usize,
}

struct QueueState {
    events: VecDeque<InputEvent>,
    dropped: u32,
    seq: u32,
    /// The device went away: readers get end of file once drained.
    closed: bool,
}

impl EventQueue {
    const fn new(capacity: usize) -> EventQueue {
        EventQueue {
            state: IrqSpinLock::new(QueueState { events: VecDeque::new(), dropped: 0, seq: 0, closed: false }),
            waitq: WaitQueue::new(),
            capacity,
        }
    }

    fn push(&self, mut e: InputEvent) {
        {
            let mut s = self.state.lock();
            if s.events.len() >= self.capacity {
                s.events.pop_front();
                s.dropped = s.dropped.saturating_add(1);
            }
            s.seq = s.seq.wrapping_add(1);
            e.seq = s.seq;
            s.events.push_back(e);
        }
        self.waitq.wake_all();
    }

    fn close(&self) {
        self.state.lock().closed = true;
        self.waitq.wake_all();
    }

    /// Clears the queue for a new reader (the seat is reopened by a
    /// restarted compositor: it must not see stale events).
    fn reset(&self) {
        let mut s = self.state.lock();
        s.events.clear();
        s.dropped = 0;
    }

    fn read(&self, buf: &mut [u8]) -> Result<usize, Errno> {
        const SIZE: usize = InputEvent::SIZE;
        if buf.len() < SIZE {
            return Err(Errno::InvalidArgument);
        }
        block_on(&[&self.waitq], None, || {
            let mut s = self.state.lock();
            if s.events.is_empty() && s.dropped == 0 {
                return if s.closed { Some(0) } else { None };
            }
            let mut n = 0;
            if s.dropped > 0 && buf.len() >= SIZE {
                let e = InputEvent {
                    time_ns: interrupts::uptime_ms() * 1_000_000,
                    kind: kind::DROPPED,
                    value: s.dropped as i32,
                    ..Default::default()
                };
                buf[..SIZE].copy_from_slice(&e.encode());
                s.dropped = 0;
                n += SIZE;
            }
            while n + SIZE <= buf.len() {
                let Some(e) = s.events.pop_front() else { break };
                buf[n..n + SIZE].copy_from_slice(&e.encode());
                n += SIZE;
            }
            Some(n)
        })
    }

    fn poll(&self) -> u32 {
        let s = self.state.lock();
        if !s.events.is_empty() || s.dropped > 0 {
            WAIT_READABLE
        } else if s.closed {
            WAIT_READABLE | WAIT_HANGUP
        } else {
            0
        }
    }
}

struct DeviceNode {
    queue: Arc<EventQueue>,
}

impl CharDevice for DeviceNode {
    fn read(&self, buf: &mut [u8]) -> Result<usize, Errno> {
        self.queue.read(buf)
    }
    fn write(&self, _data: &[u8]) -> Result<usize, Errno> {
        Err(Errno::NotSupported)
    }
    fn poll(&self) -> u32 {
        self.queue.poll()
    }
    fn wait_queue(&self) -> Option<&WaitQueue> {
        Some(&self.queue.waitq)
    }
}

static SEAT: EventQueue = EventQueue::new(SEAT_QUEUE);
static SEAT_OPEN: AtomicBool = AtomicBool::new(false);

struct Seat;

impl CharDevice for Seat {
    fn read(&self, buf: &mut [u8]) -> Result<usize, Errno> {
        SEAT.read(buf)
    }
    fn write(&self, _data: &[u8]) -> Result<usize, Errno> {
        Err(Errno::NotSupported)
    }
    fn poll(&self) -> u32 {
        SEAT.poll()
    }
    fn wait_queue(&self) -> Option<&WaitQueue> {
        Some(&SEAT.waitq)
    }
    fn open(&self) -> Result<(), Errno> {
        if SEAT_OPEN.swap(true, Ordering::AcqRel) {
            return Err(Errno::Busy);
        }
        SEAT.reset();
        kprintln!("input: seat0 opened by process {}", sched::current_pid());
        Ok(())
    }
    fn close(&self) {
        SEAT_OPEN.store(false, Ordering::Release);
        kprintln!("input: seat0 closed");
    }
}

struct Device {
    name: String,
    class: i32,
    queue: Arc<EventQueue>,
    path: String,
    /// Keys and buttons currently down (released if the device goes away).
    keys: Vec<u16>,
    buttons: u32,
    events: u64,
}

struct Core {
    devices: BTreeMap<u32, Device>,
    modifiers: ModifierState,
    repeater: Repeater,
}

static CORE: IrqSpinLock<Option<Core>> = IrqSpinLock::new(None);
static NEXT_ID: AtomicU32 = AtomicU32::new(0);
static LOG_EVENTS: AtomicBool = AtomicBool::new(false);
/// Bumped on every key press/release so the repeat thread re-evaluates.
static REPEAT_GEN: AtomicU64 = AtomicU64::new(0);
static REPEAT_WAKE: WaitQueue = WaitQueue::new();
/// HID LED byte for the lock state, and a counter of changes.
static LEDS: AtomicU32 = AtomicU32::new(0);
static TOTAL_EVENTS: AtomicU64 = AtomicU64::new(0);

/// Sets up the core, `/dev/input/seat0` and the key repeat thread.
/// `log` writes every event to the kernel log (for tests).
pub fn init(log: bool) {
    *CORE.lock() = Some(Core { devices: BTreeMap::new(), modifiers: ModifierState::default(), repeater: Repeater::new(RepeatConfig::default()) });
    LOG_EVENTS.store(log, Ordering::Relaxed);
    if let Err(e) = devices::register("input/seat0", DeviceKind::Char(Arc::new(Seat)), 0o600, 13 << 8 | 63) {
        kprintln!("input: cannot create /dev/input/seat0: {e:?}");
    }
    sched::spawn_kernel("input-repeat", repeat_thread, 0, 0);
    kprintln!("input: core ready (seat0, key repeat {} ms / {} ms)", RepeatConfig::default().delay_ms, RepeatConfig::default().interval_ms);
}

fn now_ns() -> u64 {
    interrupts::uptime_ms() * 1_000_000
}

/// Adds a device; returns its id. Creates `/dev/input/event<id>`.
pub fn register(name: &str, class: i32) -> u32 {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let queue = Arc::new(EventQueue::new(DEVICE_QUEUE));
    let path = alloc::format!("input/event{id}");
    if let Err(e) = devices::register(&path, DeviceKind::Char(Arc::new(DeviceNode { queue: queue.clone() })), 0o600, 13 << 8 | (64 + id)) {
        kprintln!("input: cannot create /dev/{path}: {e:?}");
    }
    let mods = {
        let mut g = CORE.lock();
        let c = g.as_mut().expect("input core initialised");
        c.devices.insert(id, Device { name: String::from(name), class, queue, path: path.clone(), keys: Vec::new(), buttons: 0, events: 0 });
        c.modifiers.bits()
    };
    kprintln!("input: device {} \"{}\" ({}) attached as /dev/{}", id, name, class_name(class), path);
    SEAT.push(InputEvent { time_ns: now_ns(), device: id, kind: kind::DEVICE, code: osyx_input::DEVICE_ADDED, value: class, modifiers: mods, ..Default::default() });
    id
}

fn class_name(class: i32) -> &'static str {
    match class {
        class::KEYBOARD => "keyboard",
        class::MOUSE => "mouse",
        _ => "input device",
    }
}

/// Removes a device: everything it held is released first.
pub fn unregister(id: u32) {
    let released: Vec<u16>;
    let buttons: u32;
    {
        let g = CORE.lock();
        let Some(d) = g.as_ref().and_then(|c| c.devices.get(&id)) else { return };
        released = d.keys.clone();
        buttons = d.buttons;
    }
    for k in released {
        key(id, k, false);
    }
    for b in 0..32u16 {
        if buttons & (1 << b) != 0 {
            button(id, b + 1, false);
        }
    }
    let (dev, mods) = {
        let mut g = CORE.lock();
        let c = g.as_mut().expect("input core initialised");
        c.repeater.forget_device(id);
        let dev = c.devices.remove(&id);
        if !c.devices.values().any(|d| d.class == class::KEYBOARD) {
            c.modifiers.release_held();
        }
        (dev, c.modifiers.bits())
    };
    let Some(dev) = dev else { return };
    let _ = devices::unregister(&dev.path);
    dev.queue.close();
    kprintln!("input: device {} \"{}\" detached after {} events", id, dev.name, dev.events);
    SEAT.push(InputEvent {
        time_ns: now_ns(),
        device: id,
        kind: kind::DEVICE,
        code: osyx_input::DEVICE_REMOVED,
        value: dev.class,
        modifiers: mods,
        ..Default::default()
    });
}

/// Delivers an event of device `id` to its queue and the seat.
fn emit(id: u32, mut e: InputEvent, c: &mut Core) -> Option<Arc<EventQueue>> {
    let d = c.devices.get_mut(&id)?;
    d.events += 1;
    e.device = id;
    e.time_ns = now_ns();
    e.modifiers = c.modifiers.bits();
    TOTAL_EVENTS.fetch_add(1, Ordering::Relaxed);
    if LOG_EVENTS.load(Ordering::Relaxed) {
        kprintln!(
            "input: dev {} {} code {:#x} value {} {} mods {:#x}",
            id,
            match e.kind {
                kind::KEY => "key",
                kind::MOTION => "motion",
                kind::BUTTON => "button",
                kind::WHEEL => "wheel",
                _ => "event",
            },
            e.code,
            e.value,
            e.value2,
            e.modifiers
        );
    }
    let q = d.queue.clone();
    // The device queue and the seat get the same event.
    q.push(e);
    SEAT.push(e);
    Some(q)
}

/// A key went down or up.
pub fn key(id: u32, usage: u16, down: bool) {
    let mut g = CORE.lock();
    let Some(c) = g.as_mut() else { return };
    let Some(d) = c.devices.get_mut(&id) else { return };
    if down {
        if d.keys.contains(&usage) {
            return;
        }
        d.keys.push(usage);
    } else {
        let Some(i) = d.keys.iter().position(|&k| k == usage) else { return };
        d.keys.remove(i);
    }
    let lock_changed = c.modifiers.apply(usage, down);
    if lock_changed {
        LEDS.store(c.modifiers.leds() as u32, Ordering::Release);
    }
    let now_ms = interrupts::uptime_ms();
    if down {
        c.repeater.press(id, usage, now_ms);
    } else {
        c.repeater.release(id, usage);
    }
    let value = if down { osyx_input::KEY_DOWN } else { osyx_input::KEY_UP };
    emit(id, InputEvent { kind: kind::KEY, code: usage, value, ..Default::default() }, c);
    drop(g);
    REPEAT_GEN.fetch_add(1, Ordering::Relaxed);
    REPEAT_WAKE.wake_all();
}

pub fn motion(id: u32, dx: i32, dy: i32) {
    if dx == 0 && dy == 0 {
        return;
    }
    let mut g = CORE.lock();
    if let Some(c) = g.as_mut() {
        emit(id, InputEvent { kind: kind::MOTION, value: dx, value2: dy, ..Default::default() }, c);
    }
}

/// Button `b` (1 = left, 2 = right, 3 = middle, ...).
pub fn button(id: u32, b: u16, pressed: bool) {
    if b == 0 || b > 32 {
        return;
    }
    let mut g = CORE.lock();
    let Some(c) = g.as_mut() else { return };
    let Some(d) = c.devices.get_mut(&id) else { return };
    let bit = 1u32 << (b - 1);
    if (d.buttons & bit != 0) == pressed {
        return;
    }
    d.buttons ^= bit;
    emit(id, InputEvent { kind: kind::BUTTON, code: b, value: i32::from(pressed), ..Default::default() }, c);
}

pub fn wheel(id: u32, vertical: i32, horizontal: i32) {
    if vertical == 0 && horizontal == 0 {
        return;
    }
    let mut g = CORE.lock();
    if let Some(c) = g.as_mut() {
        emit(id, InputEvent { kind: kind::WHEEL, value: vertical, value2: horizontal, ..Default::default() }, c);
    }
}

/// The keyboard LED byte the drivers should apply.
pub fn leds() -> u8 {
    LEDS.load(Ordering::Acquire) as u8
}

/// Events delivered since boot.
pub fn total_events() -> u64 {
    TOTAL_EVENTS.load(Ordering::Relaxed)
}

/// (id, name, class, events) of every attached device.
pub fn devices() -> Vec<(u32, String, i32, u64)> {
    let g = CORE.lock();
    g.as_ref().map_or(Vec::new(), |c| c.devices.iter().map(|(id, d)| (*id, d.name.clone(), d.class, d.events)).collect())
}

/// The first timer tick at or after uptime ms (rounding down would wake
/// the thread a tick early and make it spin until the deadline).
fn ms_to_tick(ms: u64) -> u64 {
    (ms * crate::arch::pic::TIMER_HZ as u64).div_ceil(1000)
}

/// Generates `KEY_REPEAT` events while a key is held.
fn repeat_thread(_: u64, _: u64) {
    loop {
        let deadline = {
            let mut g = CORE.lock();
            let c = g.as_mut().expect("input core initialised");
            if let Some((dev, usage)) = c.repeater.poll(interrupts::uptime_ms()) {
                emit(dev, InputEvent { kind: kind::KEY, code: usage, value: osyx_input::KEY_REPEAT, ..Default::default() }, c);
            }
            c.repeater.deadline()
        };
        let generation = REPEAT_GEN.load(Ordering::Relaxed);
        let _ = block_on(&[&REPEAT_WAKE], deadline.map(ms_to_tick), || (REPEAT_GEN.load(Ordering::Relaxed) != generation).then_some(()));
    }
}
