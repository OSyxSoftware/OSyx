//! Kernel random number generator behind `/dev/random`.
//!
//! Entropy sources (only hardware sources count; timer jitter in an
//! emulator is not treated as entropy):
//! * `RDSEED` / `RDRAND` when CPUID reports them (each read checks CF);
//! * a virtio-rng device (QEMU forwards the host's random source).
//!
//! 64 bytes from every available source are condensed with SHA-256 into
//! the seed of a ChaCha20 DRBG with fast key erasure (`osyx-crypto`).
//! Every request mixes in fresh `RDRAND` output when available, and the
//! virtio device reseeds the generator every [`RESEED_INTERVAL`] requests.
//! Without any source the generator stays unseeded and reads fail with
//! `WouldBlock`; predictable bytes are never returned.

use crate::arch::cpu;
use crate::drivers::virtio_rng::{self, VirtioRng};
use crate::sync::{IrqSpinLock, KMutex};
use crate::{kprintln, pci};
use alloc::string::String;
use alloc::vec::Vec;
use osyx_abi::Errno;
use osyx_crypto::{Drbg, Sha256};

const RESEED_INTERVAL: u64 = 64;

static DRBG: KMutex<Drbg> = KMutex::new(Drbg::new());
static VIRTIO: IrqSpinLock<Option<alloc::sync::Arc<VirtioRng>>> = IrqSpinLock::new(None);
static SOURCES: IrqSpinLock<Vec<&'static str>> = IrqSpinLock::new(Vec::new());

fn has_rdrand() -> bool {
    cpu::cpuid(1, 0).2 & (1 << 30) != 0
}

fn has_rdseed() -> bool {
    cpu::cpuid(0, 0).0 >= 7 && cpu::cpuid(7, 0).1 & (1 << 18) != 0
}

/// One hardware random word; `None` if the instruction keeps failing.
fn hw_word(seed: bool) -> Option<u64> {
    for _ in 0..32 {
        let v: u64;
        let ok: u8;
        // SAFETY: only executed after CPUID reported the instruction.
        unsafe {
            if seed {
                core::arch::asm!("rdseed {v}", "setc {ok}", v = out(reg) v, ok = out(reg_byte) ok, options(nomem, nostack));
            } else {
                core::arch::asm!("rdrand {v}", "setc {ok}", v = out(reg) v, ok = out(reg_byte) ok, options(nomem, nostack));
            }
        }
        if ok == 1 {
            return Some(v);
        }
        core::hint::spin_loop();
    }
    None
}

fn hw_bytes(seed: bool, out: &mut [u8]) -> bool {
    for chunk in out.chunks_mut(8) {
        match hw_word(seed) {
            Some(w) => chunk.copy_from_slice(&w.to_le_bytes()[..chunk.len()]),
            None => return false,
        }
    }
    true
}

/// Probes the sources and seeds the generator. Returns the source names.
pub fn init(pci_devices: &[pci::Device]) -> Vec<&'static str> {
    for d in pci_devices {
        if d.vendor == crate::drivers::virtio::VENDOR && virtio_rng::DEVICE_IDS.contains(&d.device) {
            match VirtioRng::probe(d) {
                Ok(r) => {
                    kprintln!("virtio-rng at {}", r.location);
                    *VIRTIO.lock() = Some(alloc::sync::Arc::new(r));
                }
                Err(e) => kprintln!("virtio-rng at {}: not usable: {:?}", d.addr, e),
            }
        }
    }
    let mut h = Sha256::new();
    let mut sources = Vec::new();
    let mut buf = [0u8; 64];
    if has_rdseed() && hw_bytes(true, &mut buf) {
        h.update(&buf);
        sources.push("rdseed");
    }
    if has_rdrand() && hw_bytes(false, &mut buf) {
        h.update(&buf);
        sources.push("rdrand");
    }
    let virtio = VIRTIO.lock().clone();
    if let Some(v) = virtio {
        if v.read(&mut buf).is_ok_and(|n| n == buf.len()) {
            h.update(&buf);
            sources.push("virtio-rng");
        }
    }
    buf.fill(0);
    if sources.is_empty() {
        kprintln!("random: no hardware entropy source; /dev/random is unavailable");
    } else {
        DRBG.lock().reseed(&h.finish());
        kprintln!("random: ChaCha20 DRBG seeded from {}", sources.join(" + "));
    }
    *SOURCES.lock() = sources.clone();
    sources
}

pub fn sources() -> String {
    SOURCES.lock().join(" + ")
}

pub fn is_seeded() -> bool {
    DRBG.lock().is_seeded()
}

/// Fills `out` with cryptographically secure random bytes.
pub fn fill(out: &mut [u8]) -> Result<(), Errno> {
    let mut g = DRBG.lock();
    if !g.is_seeded() {
        return Err(Errno::WouldBlock);
    }
    let mut fresh = [0u8; 32];
    let mut mixed = false;
    if has_rdrand() && hw_bytes(false, &mut fresh) {
        mixed = true;
    }
    if g.requests >= RESEED_INTERVAL {
        let virtio = VIRTIO.lock().clone();
        if let Some(v) = virtio {
            let mut extra = [0u8; 32];
            if v.read(&mut extra).is_ok_and(|n| n == 32) {
                let mut h = Sha256::new();
                h.update(&fresh);
                h.update(&extra);
                fresh = h.finish();
                mixed = true;
            }
        }
    }
    if mixed {
        g.reseed(&fresh);
    }
    for chunk in out.chunks_mut(osyx_crypto::MAX_REQUEST) {
        g.fill(chunk).ok_or(Errno::WouldBlock)?;
    }
    Ok(())
}
