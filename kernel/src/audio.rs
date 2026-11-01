//! Sound output and input on the OpenBSD audio drivers
//! (`drivers/bsd/compat/audio.c`, azalia(4) for Intel High Definition Audio
//! controllers and codecs). The format is fixed at 48 kHz, signed 16-bit
//! little-endian, stereo (4 bytes per frame), in both directions.
//!
//! Every sound card is a unit with its own nodes (anyone may open them):
//! - `/dev/audioN` (cards that can play): written PCM is played. Writes
//!   block while the 200 ms queue is full; the hardware runs while at least
//!   one handle is open, and closing waits for the queue to drain.
//! - `/dev/audioinN` (cards that can record): reads return recorded PCM,
//!   blocking until some is there (`TimedOut` when the card delivers
//!   nothing for two seconds). The hardware records while at least one
//!   handle is open; when readers fall more than a second behind, the
//!   oldest samples are dropped.
//! - `/dev/audio` and `/dev/audioin`: the default output and input card.
//!
//! - `/dev/mixer`: writing `volume=N` (0..100) or `mute=0|1` lines changes
//!   the software volume, applied to every played sample; `output=N` and
//!   `input=N` (a unit number or card name) choose the default cards, which
//!   cannot change while their alias is open.
//! - `/run/audio.status`: `key=value` lines, rewritten on every change:
//!   `device` (the default output card), `input` (the default input card),
//!   `volume`, `mute`, `rate`, and one `audioN=card` / `audioinN=card` line
//!   per node.

use crate::devices::{self, CharDevice, DeviceKind};
use crate::sync::{IrqSpinLock, KMutex};
use crate::waitq::{self, WaitQueue};
use crate::{bsd, kprintln};
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};
use osyx_abi::Errno;

/// Bytes per frame (two 16-bit channels).
pub const FRAME: usize = 4;
pub const RATE: u32 = 48_000;
/// 200 ms of queued sound to play.
const QUEUE_MAX: usize = RATE as usize / 5 * FRAME;
/// One second of recorded sound waiting for readers.
const REC_MAX: usize = RATE as usize * FRAME;
const STATUS: &str = "/run/audio.status";
/// Cards the C side can hold (MAX_AUDIO in audio.c).
const MAX_UNITS: usize = 4;
/// Blocks in the hardware ring: after the queue empties, this many more
/// fills push the last queued sample out.
const RING_BLOCKS: u64 = 8;
/// A recording card that delivers nothing for this long has stalled.
const STALL_MS: u64 = 2000;
const CAN_PLAY: u32 = 1;
const CAN_RECORD: u32 = 2;

static VOLUME: AtomicU32 = AtomicU32::new(70);
static MUTED: AtomicBool = AtomicBool::new(false);
/// The default output and input units, -1 for none.
static DEFAULT_OUT: AtomicI32 = AtomicI32::new(-1);
static DEFAULT_IN: AtomicI32 = AtomicI32::new(-1);

/// One card's playback and recording state.
struct Unit {
    /// PCM waiting to be played.
    play: IrqSpinLock<VecDeque<u8>>,
    /// Woken when the hardware took samples from `play`.
    space: WaitQueue,
    /// Blocks the hardware has taken (each `osyx_bsd_audio_fill` call).
    fills: AtomicU64,
    /// Open playback handles, and whether the hardware plays.
    play_opens: KMutex<(u32, bool)>,
    /// Recorded PCM waiting for readers.
    rec: IrqSpinLock<VecDeque<u8>>,
    /// Woken when the hardware delivered recorded samples.
    data: WaitQueue,
    /// Open recording handles, and whether the hardware records.
    rec_opens: KMutex<(u32, bool)>,
    /// Recorded bytes dropped because nobody read them in time.
    dropped: AtomicU64,
}

impl Unit {
    const fn new() -> Unit {
        Unit {
            play: IrqSpinLock::new(VecDeque::new()),
            space: WaitQueue::new(),
            fills: AtomicU64::new(0),
            play_opens: KMutex::new((0, false)),
            rec: IrqSpinLock::new(VecDeque::new()),
            data: WaitQueue::new(),
            rec_opens: KMutex::new((0, false)),
            dropped: AtomicU64::new(0),
        }
    }
}

static UNITS: [Unit; MAX_UNITS] = [const { Unit::new() }; MAX_UNITS];

/// A card found at start: its name ("azalia0"), PCI id and what it can do.
#[derive(Clone)]
struct Card {
    name: String,
    id: u32,
    caps: u32,
}

static CARDS: KMutex<Vec<Card>> = KMutex::new(Vec::new());
/// Open `/dev/audio` and `/dev/audioin` handles: the defaults stay put
/// while they are open.
static ALIAS_OPENS: KMutex<[u32; 2]> = KMutex::new([0, 0]);

#[cfg(osyx_bsd)]
extern "C" {
    fn osyx_bsd_audio_count() -> i32;
    fn osyx_bsd_audio_info(unit: i32, name: *mut u8, cap: usize) -> u32;
    fn osyx_bsd_audio_caps(unit: i32) -> i32;
    fn osyx_bsd_audio_start(unit: i32, rate: *mut i32) -> i32;
    fn osyx_bsd_audio_stop(unit: i32);
    fn osyx_bsd_audio_start_rec(unit: i32, rate: *mut i32) -> i32;
    fn osyx_bsd_audio_stop_rec(unit: i32);
}

/// HDMI-only controllers (AMD/ATI graphics, vendor 1002): laptops such as
/// the ThinkPad T495 have one next to the analog controller, which should
/// be the default.
fn is_hdmi(c: &Card) -> bool {
    c.id & 0xffff == 0x1002
}

/// The first card with `cap`, analog ones before HDMI ones.
fn pick_default(cards: &[Card], cap: u32) -> i32 {
    let with = |hdmi: bool| cards.iter().position(|c| c.caps & cap != 0 && is_hdmi(c) == hdmi);
    with(false).or_else(|| with(true)).map_or(-1, |i| i as i32)
}

/// Finds the sound cards and adds the device nodes.
pub fn init() {
    #[cfg(osyx_bsd)]
    {
        let mut cards = Vec::new();
        {
            let _g = bsd::BIG_LOCK.lock();
            // SAFETY: the lock is held; the name buffer has the given capacity.
            for unit in 0..unsafe { osyx_bsd_audio_count() }.min(MAX_UNITS as i32) {
                let mut name = [0u8; 32];
                let id = unsafe { osyx_bsd_audio_info(unit, name.as_mut_ptr(), name.len()) };
                let caps = unsafe { osyx_bsd_audio_caps(unit) } as u32;
                let len = name.iter().position(|&b| b == 0).unwrap_or(name.len());
                let name = String::from_utf8_lossy(&name[..len]).into_owned();
                kprintln!(
                    "audio: audio{} is {} ({:04x}:{:04x}){}{}",
                    unit,
                    name,
                    id & 0xffff,
                    id >> 16,
                    if caps & CAN_PLAY != 0 { ", plays" } else { "" },
                    if caps & CAN_RECORD != 0 { ", records" } else { "" }
                );
                cards.push(Card { name, id, caps });
            }
        }
        DEFAULT_OUT.store(pick_default(&cards, CAN_PLAY), Ordering::Release);
        DEFAULT_IN.store(pick_default(&cards, CAN_RECORD), Ordering::Release);
        *CARDS.lock() = cards;
    }
    let cards = CARDS.lock().clone();
    if cards.is_empty() {
        return;
    }
    let add = |path: String, dev: Arc<dyn CharDevice>| {
        if let Err(e) = devices::register(&path, DeviceKind::Char(dev), 0o666, 0) {
            kprintln!("audio: cannot add /dev/{}: {:?}", path, e);
        }
    };
    for (unit, card) in cards.iter().enumerate() {
        // The queues are filled and drained with interrupts off: no
        // allocation there.
        if card.caps & CAN_PLAY != 0 {
            UNITS[unit].play.lock().reserve(QUEUE_MAX);
            add(alloc::format!("audio{unit}"), Arc::new(Output { unit: Some(unit) }));
        }
        if card.caps & CAN_RECORD != 0 {
            UNITS[unit].rec.lock().reserve(REC_MAX);
            add(alloc::format!("audioin{unit}"), Arc::new(Input { unit: Some(unit) }));
        }
    }
    if DEFAULT_OUT.load(Ordering::Acquire) >= 0 {
        add(String::from("audio"), Arc::new(Output { unit: None }));
    }
    if DEFAULT_IN.load(Ordering::Acquire) >= 0 {
        add(String::from("audioin"), Arc::new(Input { unit: None }));
    }
    add(String::from("mixer"), Arc::new(Mixer));
    if let Some(c) = name_of(DEFAULT_OUT.load(Ordering::Acquire)) {
        kprintln!("audio: /dev/audio plays on {}", c);
    }
    if let Some(c) = name_of(DEFAULT_IN.load(Ordering::Acquire)) {
        kprintln!("audio: /dev/audioin records from {}", c);
    }
    write_status();
}

fn name_of(unit: i32) -> Option<String> {
    let cards = CARDS.lock();
    usize::try_from(unit).ok().and_then(|u| cards.get(u)).map(|c| c.name.clone())
}

fn write_status() {
    let mut text = alloc::format!(
        "device={}\ninput={}\nvolume={}\nmute={}\nrate={}\n",
        name_of(DEFAULT_OUT.load(Ordering::Relaxed)).unwrap_or_default(),
        name_of(DEFAULT_IN.load(Ordering::Relaxed)).unwrap_or_default(),
        VOLUME.load(Ordering::Relaxed),
        MUTED.load(Ordering::Relaxed) as u8,
        RATE
    );
    for (unit, card) in CARDS.lock().iter().enumerate() {
        if card.caps & CAN_PLAY != 0 {
            text.push_str(&alloc::format!("audio{unit}={}\n", card.name));
        }
        if card.caps & CAN_RECORD != 0 {
            text.push_str(&alloc::format!("audioin{unit}={}\n", card.name));
        }
    }
    bsd::write_file(STATUS, text.as_bytes());
}

/// Called by the C side from the bsd thread (with the big lock held): the
/// next `len` bytes the hardware will play.
#[no_mangle]
extern "C" fn osyx_bsd_audio_fill(unit: i32, dst: *mut u8, len: i32) {
    // SAFETY: C passes `len` writable bytes of the DMA ring.
    let out = unsafe { core::slice::from_raw_parts_mut(dst, len.max(0) as usize) };
    let Some(u) = usize::try_from(unit).ok().and_then(|i| UNITS.get(i)) else {
        out.fill(0);
        return;
    };
    let gain = if MUTED.load(Ordering::Relaxed) {
        0
    } else {
        // A square law follows loudness better than a linear one.
        let v = VOLUME.load(Ordering::Relaxed) as i32;
        v * v
    };
    let mut q = u.play.lock();
    let n = q.len().min(out.len()) & !(FRAME - 1);
    let (samples, _) = out[..n].as_chunks_mut::<2>();
    for pair in samples {
        let s = i16::from_le_bytes([q.pop_front().unwrap_or(0), q.pop_front().unwrap_or(0)]) as i32;
        *pair = ((s * gain / 10_000) as i16).to_le_bytes();
    }
    drop(q);
    out[n..].fill(0);
    u.fills.fetch_add(1, Ordering::Release);
    u.space.wake_all();
}

/// Called by the C side from the bsd thread (with the big lock held):
/// `len` bytes the hardware has recorded.
#[no_mangle]
extern "C" fn osyx_bsd_audio_take(unit: i32, src: *const u8, len: i32) {
    let Some(u) = usize::try_from(unit).ok().and_then(|i| UNITS.get(i)) else { return };
    // SAFETY: C passes `len` readable bytes of the DMA ring.
    let block = unsafe { core::slice::from_raw_parts(src, len.max(0) as usize) };
    let block = &block[block.len().saturating_sub(REC_MAX)..];
    let mut q = u.rec.lock();
    // Readers that fell behind lose the oldest second, whole frames only.
    let over = (q.len() + block.len()).saturating_sub(REC_MAX).next_multiple_of(FRAME).min(q.len());
    if over > 0 {
        q.drain(..over);
        u.dropped.fetch_add(over as u64, Ordering::Relaxed);
    }
    q.extend(block);
    drop(q);
    u.data.wake_all();
}

/// The unit of a node: its own, or the current default for an alias.
fn resolve(unit: Option<usize>, default: &AtomicI32) -> Result<usize, Errno> {
    match unit {
        Some(u) => Ok(u),
        None => usize::try_from(default.load(Ordering::Acquire)).map_err(|_| Errno::NotFound),
    }
}

/// A playback node: one card, or the default output (`unit` None).
struct Output {
    unit: Option<usize>,
}

impl CharDevice for Output {
    fn read(&self, _buf: &mut [u8]) -> Result<usize, Errno> {
        Err(Errno::NotSupported)
    }

    fn write(&self, data: &[u8]) -> Result<usize, Errno> {
        if data.is_empty() {
            return Ok(0);
        }
        let u = &UNITS[resolve(self.unit, &DEFAULT_OUT)?];
        waitq::block_on(&[&u.space], None, || {
            let mut q = u.play.lock();
            let n = (QUEUE_MAX - q.len()).min(data.len());
            if n == 0 {
                return None;
            }
            q.extend(&data[..n]);
            Some(n)
        })
    }

    fn poll(&self) -> u32 {
        match resolve(self.unit, &DEFAULT_OUT) {
            Ok(u) if UNITS[u].play.lock().len() < QUEUE_MAX => osyx_abi::ipc::WAIT_WRITABLE,
            _ => 0,
        }
    }

    fn wait_queue(&self) -> Option<&WaitQueue> {
        resolve(self.unit, &DEFAULT_OUT).ok().map(|u| &UNITS[u].space)
    }

    fn open(&self) -> Result<(), Errno> {
        // The alias count is held first, so the default cannot change
        // between choosing the unit and counting the handle.
        let mut alias = ALIAS_OPENS.lock();
        let unit = resolve(self.unit, &DEFAULT_OUT)?;
        let u = &UNITS[unit];
        let mut o = u.play_opens.lock();
        if !o.1 {
            start(unit)?;
            o.1 = true;
        }
        o.0 += 1;
        if self.unit.is_none() {
            alias[0] += 1;
        }
        Ok(())
    }

    fn close(&self) {
        let unit = {
            let mut alias = ALIAS_OPENS.lock();
            if self.unit.is_none() {
                alias[0] = alias[0].saturating_sub(1);
            }
            match resolve(self.unit, &DEFAULT_OUT) {
                Ok(u) => u,
                Err(_) => return,
            }
        };
        let u = &UNITS[unit];
        let mut o = u.play_opens.lock();
        o.0 = o.0.saturating_sub(1);
        if o.0 > 0 || !o.1 {
            return;
        }
        // Let the queued sound play out, then stop the hardware. Progress is
        // counted in blocks played, not in time (an emulated card can play
        // slower than real time); the deadline only guards against a stalled
        // card.
        let deadline = crate::interrupts::ticks() + crate::arch::timer::ms_to_ticks(3000);
        let _ = waitq::block_on(&[&u.space], Some(deadline), || u.play.lock().is_empty().then_some(()));
        let last = u.fills.load(Ordering::Acquire) + RING_BLOCKS;
        let _ = waitq::block_on(&[&u.space], Some(deadline), || (u.fills.load(Ordering::Acquire) >= last).then_some(()));
        stop(unit);
        o.1 = false;
        u.play.lock().clear();
    }
}

/// A recording node: one card, or the default input (`unit` None).
struct Input {
    unit: Option<usize>,
}

impl CharDevice for Input {
    fn read(&self, buf: &mut [u8]) -> Result<usize, Errno> {
        if buf.len() < FRAME {
            return Err(Errno::InvalidArgument);
        }
        let u = &UNITS[resolve(self.unit, &DEFAULT_IN)?];
        // A recording card delivers a block every few milliseconds; one
        // that has delivered nothing for two seconds has stalled (or its
        // host side cannot record, as QEMU's wav backend), and the reader
        // gets TimedOut instead of waiting forever.
        let deadline = crate::interrupts::ticks() + crate::arch::timer::ms_to_ticks(STALL_MS);
        waitq::block_on(&[&u.data], Some(deadline), || {
            let mut q = u.rec.lock();
            let n = q.len().min(buf.len()) & !(FRAME - 1);
            if n == 0 {
                return None;
            }
            for (dst, src) in buf[..n].iter_mut().zip(q.drain(..n)) {
                *dst = src;
            }
            Some(n)
        })
    }

    fn write(&self, _data: &[u8]) -> Result<usize, Errno> {
        Err(Errno::NotSupported)
    }

    fn poll(&self) -> u32 {
        match resolve(self.unit, &DEFAULT_IN) {
            Ok(u) if UNITS[u].rec.lock().len() >= FRAME => osyx_abi::ipc::WAIT_READABLE,
            _ => 0,
        }
    }

    fn wait_queue(&self) -> Option<&WaitQueue> {
        resolve(self.unit, &DEFAULT_IN).ok().map(|u| &UNITS[u].data)
    }

    fn open(&self) -> Result<(), Errno> {
        let mut alias = ALIAS_OPENS.lock();
        let unit = resolve(self.unit, &DEFAULT_IN)?;
        let u = &UNITS[unit];
        let mut o = u.rec_opens.lock();
        if !o.1 {
            // A new recording starts with nothing stale in the queue.
            u.rec.lock().clear();
            start_rec(unit)?;
            o.1 = true;
        }
        o.0 += 1;
        if self.unit.is_none() {
            alias[1] += 1;
        }
        Ok(())
    }

    fn close(&self) {
        let unit = {
            let mut alias = ALIAS_OPENS.lock();
            if self.unit.is_none() {
                alias[1] = alias[1].saturating_sub(1);
            }
            match resolve(self.unit, &DEFAULT_IN) {
                Ok(u) => u,
                Err(_) => return,
            }
        };
        let u = &UNITS[unit];
        let mut o = u.rec_opens.lock();
        o.0 = o.0.saturating_sub(1);
        if o.0 > 0 || !o.1 {
            return;
        }
        stop_rec(unit);
        o.1 = false;
        u.rec.lock().clear();
        let dropped = u.dropped.swap(0, Ordering::Relaxed);
        if dropped > 0 {
            kprintln!("audio: audioin{}: {} recorded bytes were not read in time", unit, dropped);
        }
    }
}

fn start(unit: usize) -> Result<(), Errno> {
    #[cfg(osyx_bsd)]
    {
        let mut rate = 0;
        let e = {
            let _g = bsd::BIG_LOCK.lock();
            // SAFETY: the lock is held.
            unsafe { osyx_bsd_audio_start(unit as i32, &mut rate) }
        };
        let name = name_of(unit as i32).unwrap_or_default();
        if e != 0 {
            kprintln!("audio: starting playback on {} failed (error {})", name, e);
            return Err(Errno::Io);
        }
        if rate as u32 != RATE {
            kprintln!("audio: {} plays at {} Hz instead of {}", name, rate, RATE);
        }
        Ok(())
    }
    #[cfg(not(osyx_bsd))]
    {
        let _ = unit;
        Err(Errno::NotSupported)
    }
}

fn stop(unit: usize) {
    #[cfg(osyx_bsd)]
    {
        let _g = bsd::BIG_LOCK.lock();
        // SAFETY: the lock is held.
        unsafe { osyx_bsd_audio_stop(unit as i32) };
    }
    #[cfg(not(osyx_bsd))]
    let _ = unit;
}

fn start_rec(unit: usize) -> Result<(), Errno> {
    #[cfg(osyx_bsd)]
    {
        let mut rate = 0;
        let e = {
            let _g = bsd::BIG_LOCK.lock();
            // SAFETY: the lock is held.
            unsafe { osyx_bsd_audio_start_rec(unit as i32, &mut rate) }
        };
        let name = name_of(unit as i32).unwrap_or_default();
        if e != 0 {
            kprintln!("audio: starting recording on {} failed (error {})", name, e);
            return Err(Errno::Io);
        }
        if rate as u32 != RATE {
            kprintln!("audio: {} records at {} Hz instead of {}", name, rate, RATE);
        }
        Ok(())
    }
    #[cfg(not(osyx_bsd))]
    {
        let _ = unit;
        Err(Errno::NotSupported)
    }
}

fn stop_rec(unit: usize) {
    #[cfg(osyx_bsd)]
    {
        let _g = bsd::BIG_LOCK.lock();
        // SAFETY: the lock is held.
        unsafe { osyx_bsd_audio_stop_rec(unit as i32) };
    }
    #[cfg(not(osyx_bsd))]
    let _ = unit;
}

/// A card named by its unit number or name, with the capability `cap`.
fn find_card(spec: &str, cap: u32) -> Result<i32, Errno> {
    let cards = CARDS.lock();
    let unit = match spec.parse::<usize>() {
        Ok(n) => n,
        Err(_) => cards.iter().position(|c| c.name == spec).ok_or(Errno::NotFound)?,
    };
    match cards.get(unit) {
        Some(c) if c.caps & cap != 0 => Ok(unit as i32),
        _ => Err(Errno::NotFound),
    }
}

struct Mixer;

impl CharDevice for Mixer {
    fn read(&self, _buf: &mut [u8]) -> Result<usize, Errno> {
        // The state is in /run/audio.status.
        Ok(0)
    }

    fn write(&self, data: &[u8]) -> Result<usize, Errno> {
        let text = core::str::from_utf8(data).map_err(|_| Errno::InvalidArgument)?;
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let (k, v) = line.split_once('=').ok_or(Errno::InvalidArgument)?;
            let (k, v) = (k.trim(), v.trim());
            let number = || v.parse::<u32>().map_err(|_| Errno::InvalidArgument);
            match k {
                "volume" => VOLUME.store(number()?.min(100), Ordering::Relaxed),
                "mute" => MUTED.store(number()? != 0, Ordering::Relaxed),
                "output" | "input" => {
                    let (cap, default, slot) =
                        if k == "output" { (CAN_PLAY, &DEFAULT_OUT, 0) } else { (CAN_RECORD, &DEFAULT_IN, 1) };
                    let unit = find_card(v, cap)?;
                    let alias = ALIAS_OPENS.lock();
                    if alias[slot] > 0 && default.load(Ordering::Acquire) != unit {
                        return Err(Errno::Busy);
                    }
                    default.store(unit, Ordering::Release);
                }
                _ => return Err(Errno::InvalidArgument),
            }
        }
        write_status();
        Ok(data.len())
    }
}
