//! Tick source selection, calibration and tick-independent deadlines.
//!
//! The scheduler tick (1 kHz) comes from the local APIC timer when there
//! is a local APIC, and from the PIT through the 8259 PIC and LINT0
//! ("virtual wire") otherwise. Modern boards often break the PIT path
//! (the PIT clock gated off, ExtINT not routed), so the APIC timer is the
//! preferred source and the PIT path is the fallback.
//!
//! The APIC timer runs at an unknown bus clock and has to be calibrated:
//! it and the TSC are measured over [`CALIBRATION_MS`] against the first
//! reference that is seen to count: PIT channel 2, the ACPI PM timer, the
//! HPET (`timer.calib=pit|pm|hpet` forces one). After interrupts are on,
//! [`verify`] checks that ticks really arrive and falls back to the PIT if
//! they do not.
//!
//! [`Deadline`] expires on either the tick count or the calibrated TSC, so
//! driver timeouts end even when no tick ever arrives.

use super::{apic, pic};
use crate::{interrupts, kprintln, mem};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};

/// Vector of the local APIC timer (above the MSI range of `crate::irq`).
pub const APIC_TIMER_VECTOR: u8 = 0xF0;
pub const TIMER_HZ: u32 = pic::TIMER_HZ;
const CALIBRATION_MS: u64 = 20;
/// Counter reads after which a reference that has not changed counts as
/// stopped (each read is an I/O or uncached access: well over 1 us).
const ALIVE_READS: u32 = 4000;
/// Upper bound on polls of a reference during one calibration interval,
/// in case it starts counting and then stalls.
const MAX_POLLS: u32 = 2_000_000;
/// TSC rate assumed when nothing could measure it (an upper bound, so
/// TSC-based timeouts err on the long side).
const FALLBACK_TSC_KHZ: u64 = 6_000_000;
const PM_TIMER_HZ: u64 = 3_579_545;

const SOURCE_NONE: u8 = 0;
const SOURCE_PIT: u8 = 1;
const SOURCE_APIC: u8 = 2;

static SOURCE: AtomicU8 = AtomicU8::new(SOURCE_NONE);
static TSC_KHZ: AtomicU64 = AtomicU64::new(0);
static HEALTHY: AtomicBool = AtomicBool::new(false);
static SECONDARY_COUNT: AtomicU32 = AtomicU32::new(0);

pub fn secondary_available()->bool { SECONDARY_COUNT.load(Ordering::Relaxed)!=0 }
pub fn start_secondary() { apic::timer_start_periodic(APIC_TIMER_VECTOR,SECONDARY_COUNT.load(Ordering::Relaxed)); }
pub fn spin_ms(ms:u64) { let end=tsc().saturating_add(tsc_khz().saturating_mul(ms));while tsc()<end {core::hint::spin_loop();} }

#[derive(Clone, Copy)]
enum Reference {
    Pit,
    PmTimer { port: u16, mask: u32 },
    Hpet { regs: u64, period_fs: u64, mask: u64 },
}

const REFERENCE_NAMES: [&str; 4] = ["none", "PIT channel 2", "ACPI PM timer", "HPET"];

impl Reference {
    fn id(&self) -> u8 {
        match self {
            Reference::Pit => 1,
            Reference::PmTimer { .. } => 2,
            Reference::Hpet { .. } => 3,
        }
    }

    /// Waits `ms` milliseconds on this reference; `start` is called exactly
    /// when the measured interval begins. None if the reference does not
    /// count (then `start` may not have been called).
    fn wait_ms(&self, ms: u64, start: &mut dyn FnMut()) -> Option<()> {
        match *self {
            Reference::Pit => {
                let count = (pic::PIT_FREQ as u64 * ms / 1000).min(0xFFFF) as u16;
                pic::channel2_start(count);
                let first = pic::channel2_count();
                if !(0..ALIVE_READS).any(|_| pic::channel2_count() != first) {
                    return None;
                }
                // Restart so the whole interval is measured.
                pic::channel2_start(count);
                start();
                (0..MAX_POLLS).any(|_| pic::channel2_done()).then_some(())
            }
            Reference::PmTimer { port, mask } => {
                // SAFETY: the PM timer port comes from the FADT; reading it
                // has no side effects.
                let read = || unsafe { super::cpu::inl(port) } & mask;
                let first = read();
                if !(0..ALIVE_READS).any(|_| read() != first) {
                    return None;
                }
                let target = (PM_TIMER_HZ * ms / 1000) as u32;
                let t0 = read();
                start();
                (0..MAX_POLLS).any(|_| read().wrapping_sub(t0) & mask >= target).then_some(())
            }
            Reference::Hpet { regs, period_fs, mask } => {
                // SAFETY: `regs` maps the HPET register block (uncached).
                let read = || unsafe { core::ptr::read_volatile((regs + 0xF0) as *const u64) } & mask;
                let first = read();
                if !(0..ALIVE_READS).any(|_| read() != first) {
                    return None;
                }
                let target = ms * 1_000_000_000_000 / period_fs;
                let t0 = read();
                start();
                (0..MAX_POLLS).any(|_| read().wrapping_sub(t0) & mask >= target).then_some(())
            }
        }
    }
}

/// Command-line name, index into `REFERENCE_NAMES`, constructor.
type Candidate = (&'static str, usize, fn() -> Option<Reference>);

fn pm_timer_reference() -> Option<Reference> {
    let (port, ext32) = crate::acpi::pm_timer()?;
    Some(Reference::PmTimer { port, mask: if ext32 { u32::MAX } else { 0x00FF_FFFF } })
}

fn hpet_reference() -> Option<Reference> {
    let base = crate::acpi::hpet()?;
    let regs = mem::map_mmio(base, 0x400).ok()?;
    // SAFETY: `regs` maps the HPET register block (uncached).
    let cap = unsafe { core::ptr::read_volatile(regs as *const u64) };
    let period_fs = cap >> 32;
    if period_fs == 0 || period_fs > 100_000_000 {
        return None;
    }
    // SAFETY: as above; sets ENABLE_CNF so the main counter runs.
    unsafe {
        let cfg = (regs + 0x10) as *mut u64;
        core::ptr::write_volatile(cfg, core::ptr::read_volatile(cfg) | 1);
    }
    let mask = if cap & (1 << 13) != 0 { u64::MAX } else { u32::MAX as u64 };
    Some(Reference::Hpet { regs, period_fs, mask })
}

pub fn tsc() -> u64 {
    // SAFETY: RDTSC is available on every x86-64 CPU.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// TSC rate in kHz: measured, else from CPUID, else an upper-bound guess.
pub fn tsc_khz() -> u64 {
    match TSC_KHZ.load(Ordering::Relaxed) {
        0 => FALLBACK_TSC_KHZ,
        k => k,
    }
}

/// The measured TSC rate, or 0 when only the fallback guess is available.
pub fn measured_tsc_khz() -> u64 {
    TSC_KHZ.load(Ordering::Relaxed)
}

/// TSC frequency reported by CPUID leaf 0x15 (crystal ratio) or 0x16
/// (base frequency), in kHz.
fn cpuid_tsc_khz() -> Option<u64> {
    let (max, ..) = super::cpu::cpuid(0, 0);
    if max >= 0x15 {
        let (den, num, crystal, _) = super::cpu::cpuid(0x15, 0);
        if den != 0 && num != 0 && crystal != 0 {
            return Some(crystal as u64 * num as u64 / den as u64 / 1000);
        }
    }
    if max >= 0x16 {
        let (base_mhz, ..) = super::cpu::cpuid(0x16, 0);
        if base_mhz != 0 {
            return Some(base_mhz as u64 * 1000);
        }
    }
    None
}

/// Calibrates and starts the tick source. Interrupts must still be
/// disabled; `apic::init` and `acpi::init` must have run. `force` is
/// `timer=` from the command line (`pit` uses the PIT path) and `calib`
/// is `timer.calib=`.
pub fn init(force: Option<&str>, calib: Option<&str>) {
    let use_apic = apic::available();
    let mut measured = None;
    let candidates: [Candidate; 3] =
        [("pit", 1, || Some(Reference::Pit)), ("pm", 2, pm_timer_reference), ("hpet", 3, hpet_reference)];
    for (name, id, make) in candidates {
        if calib.is_some_and(|c| c != name) {
            continue;
        }
        let Some(r) = make() else {
            kprintln!("timer: no usable {} described by ACPI", REFERENCE_NAMES[id]);
            continue;
        };
        let mut t0 = 0;
        let ok = r.wait_ms(CALIBRATION_MS, &mut || {
            if use_apic {
                apic::timer_start_calibration();
            }
            t0 = tsc();
        });
        if ok.is_none() {
            kprintln!("timer: {} does not count, skipped", REFERENCE_NAMES[id]);
            continue;
        }
        let tsc_delta = tsc() - t0;
        let apic_delta = if use_apic { apic::timer_elapsed() as u64 } else { 0 };
        measured = Some((r, tsc_delta, apic_delta));
        break;
    }
    if use_apic {
        apic::timer_stop();
    }
    let apic_per_tick = match measured {
        Some((r, tsc_delta, apic_delta)) => {
            TSC_KHZ.store((tsc_delta / CALIBRATION_MS).max(1), Ordering::Relaxed);
            kprintln!(
                "timer: calibrated against the {}: TSC {} MHz, local APIC timer {} kHz",
                REFERENCE_NAMES[r.id() as usize],
                tsc_delta / CALIBRATION_MS / 1000,
                apic_delta / CALIBRATION_MS
            );
            apic_delta * 1000 / (CALIBRATION_MS * TIMER_HZ as u64)
        }
        None => {
            let khz = cpuid_tsc_khz();
            if let Some(k) = khz {
                TSC_KHZ.store(k, Ordering::Relaxed);
            }
            kprintln!(
                "timer: WARNING: no reference timer counts (PIT, ACPI PM timer, HPET); TSC {}",
                match khz {
                    Some(k) => alloc::format!("{} MHz from CPUID", k / 1000),
                    None => alloc::format!("rate unknown (assuming {} MHz for timeouts)", FALLBACK_TSC_KHZ / 1000),
                }
            );
            0
        }
    };
    if use_apic && force != Some("pit") && (1..=u32::MAX as u64).contains(&apic_per_tick) {
        pic::disable_timer_irq();
        apic::timer_start_periodic(APIC_TIMER_VECTOR, apic_per_tick as u32);
        SECONDARY_COUNT.store(apic_per_tick as u32,Ordering::Relaxed);
        SOURCE.store(SOURCE_APIC, Ordering::Relaxed);
    } else {
        start_pit();
    }
}

fn start_pit() {
    SECONDARY_COUNT.store(0,Ordering::Relaxed);
    apic::timer_stop();
    pic::enable_timer_irq();
    SOURCE.store(SOURCE_PIT, Ordering::Relaxed);
}

/// Waits up to 200 ms (by the TSC, or a bounded spin) for three ticks.
fn ticks_arrive() -> bool {
    let start = interrupts::ticks();
    let end = tsc() + 200 * tsc_khz();
    for _ in 0..200_000_000u64 {
        if interrupts::ticks() >= start + 3 {
            return true;
        }
        if tsc() > end {
            return false;
        }
        core::hint::spin_loop();
    }
    false
}

/// With interrupts enabled: checks that the tick source interrupts and
/// falls back to the PIT if the APIC timer does not. Logs the outcome.
pub fn verify() -> bool {
    let mut ok = ticks_arrive();
    if !ok && SOURCE.load(Ordering::Relaxed) == SOURCE_APIC {
        kprintln!("timer: WARNING: the local APIC timer does not interrupt; falling back to the PIT");
        start_pit();
        ok = ticks_arrive();
    }
    HEALTHY.store(ok, Ordering::Relaxed);
    if ok {
        kprintln!("timer: tick source: {} at {} Hz, {} mode, interrupts enabled", source_name(), TIMER_HZ, apic::mode_name());
    } else {
        kprintln!(
            "timer: WARNING: no tick source interrupts ({} tried, {} mode); sleeping and preemption will not work",
            source_name(),
            apic::mode_name()
        );
    }
    ok
}

pub fn source_name() -> &'static str {
    match SOURCE.load(Ordering::Relaxed) {
        SOURCE_APIC => "local APIC timer",
        SOURCE_PIT => "PIT via 8259 PIC (virtual wire)",
        _ => "none",
    }
}

/// Ticks for at least `ms` milliseconds.
pub fn ms_to_ticks(ms: u64) -> u64 {
    (ms * TIMER_HZ as u64).div_ceil(1000)
}

/// Whether timer ticks were seen to arrive (sleeping works).
pub fn healthy() -> bool {
    HEALTHY.load(Ordering::Relaxed)
}

/// Waits at least `ms` milliseconds: sleeps when ticks are known to
/// arrive, otherwise spins on the TSC (so polling loops still progress).
pub fn sleep_ms(ms: u64) {
    if healthy() {
        crate::sched::sleep_ticks(ms_to_ticks(ms).max(1));
    } else {
        let end = tsc() + tsc_khz() * ms;
        while tsc() < end {
            core::hint::spin_loop();
        }
    }
}

/// Waits about one tick (see [`sleep_ms`]).
pub fn pause_tick() {
    sleep_ms(1000 / TIMER_HZ as u64);
}

/// A timeout that ends when the tick count passes it or, as a backstop
/// against a timer that stopped, when the TSC passes it (two ticks later).
#[derive(Clone, Copy, Debug)]
pub struct Deadline {
    tick: u64,
    tsc: u64,
}

impl Deadline {
    pub fn after_ms(ms: u64) -> Deadline {
        let tick = interrupts::ticks() + ms_to_ticks(ms).max(1);
        let backstop = ms + 2000 / TIMER_HZ as u64;
        Deadline { tick, tsc: tsc().saturating_add(backstop.saturating_mul(tsc_khz())) }
    }

    pub fn after_ticks(ticks: u64) -> Deadline {
        Deadline::after_ms(ticks * 1000 / TIMER_HZ as u64)
    }

    pub fn expired(&self) -> bool {
        interrupts::ticks() > self.tick || tsc() > self.tsc
    }

    /// The tick at which the deadline passes (for blocking waits).
    pub fn tick(&self) -> u64 {
        self.tick
    }
}
