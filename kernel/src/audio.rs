//! Sound output on the OpenBSD audio drivers (`drivers/bsd/compat/audio.c`,
//! azalia(4) for Intel High Definition Audio controllers and codecs).
//!
//! - `/dev/audio` (anyone may open it): written PCM is played; the format
//!   is fixed at 48 kHz, signed 16-bit little-endian, stereo (4 bytes per
//!   frame). Writes block while the 200 ms queue is full; the hardware runs
//!   while at least one handle is open, and closing waits for the queue to
//!   drain.
//! - `/dev/mixer`: writing `volume=N` (0..100) or `mute=0|1` lines changes
//!   the software volume, applied to every sample.
//! - `/run/audio.status`: the device, the volume and the mute state as
//!   `key=value` lines, rewritten on every change.

use crate::devices::{self, CharDevice, DeviceKind};
use crate::sync::{IrqSpinLock, KMutex};
use crate::waitq::{self, WaitQueue};
use crate::{bsd, kprintln};
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};
use osyx_abi::Errno;

/// Bytes per frame (two 16-bit channels).
pub const FRAME: usize = 4;
pub const RATE: u32 = 48_000;
/// 200 ms of queued sound.
const QUEUE_MAX: usize = RATE as usize / 5 * FRAME;
const STATUS: &str = "/run/audio.status";

static QUEUE: IrqSpinLock<VecDeque<u8>> = IrqSpinLock::new(VecDeque::new());
/// Woken when the hardware took samples from the queue.
static SPACE: WaitQueue = WaitQueue::new();
static VOLUME: AtomicU32 = AtomicU32::new(70);
static MUTED: AtomicBool = AtomicBool::new(false);
/// The audio device in use (index into the C side's table), -1 for none.
static UNIT: AtomicI32 = AtomicI32::new(-1);
static NAME: KMutex<String> = KMutex::new(String::new());
/// Blocks the hardware has taken (each `osyx_bsd_audio_fill` call).
static FILLS: AtomicU64 = AtomicU64::new(0);
/// Blocks in the hardware ring: after the queue empties, this many more
/// fills push the last queued sample out.
const RING_BLOCKS: u64 = 8;
/// Open `/dev/audio` handles, and whether the hardware is running.
static OPENS: KMutex<(u32, bool)> = KMutex::new((0, false));

#[cfg(osyx_bsd)]
extern "C" {
    fn osyx_bsd_audio_count() -> i32;
    fn osyx_bsd_audio_info(unit: i32, name: *mut u8, cap: usize) -> u32;
    fn osyx_bsd_audio_start(unit: i32, rate: *mut i32) -> i32;
    fn osyx_bsd_audio_stop(unit: i32);
}

/// Picks the sound card and adds the device nodes. HDMI-only controllers
/// (AMD/ATI graphics, vendor 1002) come last: laptops such as the
/// ThinkPad T495 have one next to the analog controller.
pub fn init() {
    #[cfg(osyx_bsd)]
    {
        let mut best: Option<(i32, String, u32)> = None;
        {
            let _g = bsd::BIG_LOCK.lock();
            // SAFETY: the lock is held; the name buffer has the given capacity.
            for unit in 0..unsafe { osyx_bsd_audio_count() } {
                let mut name = [0u8; 32];
                let id = unsafe { osyx_bsd_audio_info(unit, name.as_mut_ptr(), name.len()) };
                let len = name.iter().position(|&b| b == 0).unwrap_or(name.len());
                let name = String::from_utf8_lossy(&name[..len]).into_owned();
                kprintln!("audio: audio{} is {} ({:04x}:{:04x})", unit, name, id & 0xffff, id >> 16);
                let hdmi = id & 0xffff == 0x1002;
                if best.as_ref().is_none_or(|b| b.2 & 0xffff == 0x1002 && !hdmi) {
                    best = Some((unit, name, id));
                }
            }
        }
        if let Some((unit, name, _)) = best {
            UNIT.store(unit, Ordering::Release);
            *NAME.lock() = name;
        }
    }
    if UNIT.load(Ordering::Acquire) < 0 {
        return;
    }
    // `write` fills the queue with interrupts off: no allocation there.
    QUEUE.lock().reserve(QUEUE_MAX);
    for (path, dev) in [("audio", Arc::new(Audio) as Arc<dyn CharDevice>), ("mixer", Arc::new(Mixer))] {
        if let Err(e) = devices::register(path, DeviceKind::Char(dev), 0o666, 0) {
            kprintln!("audio: cannot add /dev/{}: {:?}", path, e);
        }
    }
    kprintln!("audio: /dev/audio plays on {}", *NAME.lock());
    write_status();
}

fn write_status() {
    let text = alloc::format!(
        "device={}\nvolume={}\nmute={}\nrate={}\n",
        *NAME.lock(),
        VOLUME.load(Ordering::Relaxed),
        MUTED.load(Ordering::Relaxed) as u8,
        RATE
    );
    bsd::write_file(STATUS, text.as_bytes());
}

/// Called by the C side from the bsd thread (with the big lock held): the
/// next `len` bytes the hardware will play.
#[no_mangle]
extern "C" fn osyx_bsd_audio_fill(_unit: i32, dst: *mut u8, len: i32) {
    // SAFETY: C passes `len` writable bytes of the DMA ring.
    let out = unsafe { core::slice::from_raw_parts_mut(dst, len.max(0) as usize) };
    let gain = if MUTED.load(Ordering::Relaxed) {
        0
    } else {
        // A square law follows loudness better than a linear one.
        let v = VOLUME.load(Ordering::Relaxed) as i32;
        v * v
    };
    let mut q = QUEUE.lock();
    let n = q.len().min(out.len()) & !(FRAME - 1);
    let (samples, _) = out[..n].as_chunks_mut::<2>();
    for pair in samples {
        let s = i16::from_le_bytes([q.pop_front().unwrap_or(0), q.pop_front().unwrap_or(0)]) as i32;
        *pair = ((s * gain / 10_000) as i16).to_le_bytes();
    }
    drop(q);
    out[n..].fill(0);
    FILLS.fetch_add(1, Ordering::Release);
    SPACE.wake_all();
}

struct Audio;

impl CharDevice for Audio {
    fn read(&self, _buf: &mut [u8]) -> Result<usize, Errno> {
        Err(Errno::NotSupported)
    }

    fn write(&self, data: &[u8]) -> Result<usize, Errno> {
        if data.is_empty() {
            return Ok(0);
        }
        waitq::block_on(&[&SPACE], None, || {
            let mut q = QUEUE.lock();
            let n = (QUEUE_MAX - q.len()).min(data.len());
            if n == 0 {
                return None;
            }
            q.extend(&data[..n]);
            Some(n)
        })
    }

    fn poll(&self) -> u32 {
        if QUEUE.lock().len() < QUEUE_MAX {
            osyx_abi::ipc::WAIT_WRITABLE
        } else {
            0
        }
    }

    fn wait_queue(&self) -> Option<&WaitQueue> {
        Some(&SPACE)
    }

    fn open(&self) -> Result<(), Errno> {
        let mut o = OPENS.lock();
        if !o.1 {
            start()?;
            o.1 = true;
        }
        o.0 += 1;
        Ok(())
    }

    fn close(&self) {
        let mut o = OPENS.lock();
        o.0 = o.0.saturating_sub(1);
        if o.0 > 0 || !o.1 {
            return;
        }
        // Let the queued sound play out, then stop the hardware. Progress is
        // counted in blocks played, not in time (an emulated card can play
        // slower than real time); the deadline only guards against a stalled
        // card.
        let deadline = crate::interrupts::ticks() + crate::arch::timer::ms_to_ticks(3000);
        let _ = waitq::block_on(&[&SPACE], Some(deadline), || QUEUE.lock().is_empty().then_some(()));
        let last = FILLS.load(Ordering::Acquire) + RING_BLOCKS;
        let _ = waitq::block_on(&[&SPACE], Some(deadline), || (FILLS.load(Ordering::Acquire) >= last).then_some(()));
        stop();
        o.1 = false;
        QUEUE.lock().clear();
    }
}

fn start() -> Result<(), Errno> {
    #[cfg(osyx_bsd)]
    {
        let unit = UNIT.load(Ordering::Acquire);
        let mut rate = 0;
        let e = {
            let _g = bsd::BIG_LOCK.lock();
            // SAFETY: the lock is held.
            unsafe { osyx_bsd_audio_start(unit, &mut rate) }
        };
        if e != 0 {
            kprintln!("audio: starting {} failed (error {})", *NAME.lock(), e);
            return Err(Errno::Io);
        }
        if rate as u32 != RATE {
            kprintln!("audio: {} plays at {} Hz instead of {}", *NAME.lock(), rate, RATE);
        }
        Ok(())
    }
    #[cfg(not(osyx_bsd))]
    Err(Errno::NotSupported)
}

fn stop() {
    #[cfg(osyx_bsd)]
    {
        let _g = bsd::BIG_LOCK.lock();
        // SAFETY: the lock is held.
        unsafe { osyx_bsd_audio_stop(UNIT.load(Ordering::Acquire)) };
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
            let v: u32 = v.trim().parse().map_err(|_| Errno::InvalidArgument)?;
            match k.trim() {
                "volume" => VOLUME.store(v.min(100), Ordering::Relaxed),
                "mute" => MUTED.store(v != 0, Ordering::Relaxed),
                _ => return Err(Errno::InvalidArgument),
            }
        }
        write_status();
        Ok(data.len())
    }
}
