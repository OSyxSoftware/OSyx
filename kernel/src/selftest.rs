//! In-kernel self-tests, run on every boot. They exercise the real hardware
//! paths (page tables, CPU exceptions, timer interrupts) that host unit tests
//! cannot reach. Results go to the serial log as `[test] <name> ... ok|FAILED`.

use crate::arch::cpu;
use crate::fixup::{probe_read_u8, probe_write_u8};
use crate::mem::paging::{self, Flags, MapError};
use crate::mem::{self, heap, phys_to_virt, PAGE_SIZE};
use crate::{interrupts, kprintln, BootData};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use osyx_bootinfo::MemoryKind;

/// Scratch virtual address for mapping tests (PML4 slot 416, unused otherwise).
const TEST_VA: u64 = 0xFFFF_D000_0000_0000;

static RODATA_PROBE: [u8; 8] = *b"OSyx-ro\0";
static mut DATA_PROBE: [u8; 8] = *b"OSyx-rw\0";

pub struct Summary {
    pub passed: u32,
    pub failed: u32,
}

type TestResult = Result<(), String>;

macro_rules! ensure {
    ($cond:expr, $($msg:tt)*) => {
        if !$cond {
            return Err(format!($($msg)*));
        }
    };
}

pub fn run(boot: &BootData) -> Summary {
    let tests: [(&str, &dyn Fn() -> TestResult); 9] = [
        ("boot memory map", &|| memory_map(boot)),
        ("frame allocator", &frame_allocator),
        ("page map/unmap", &map_unmap),
        ("exception fixups", &fixups),
        ("identity map removed", &|| identity_removed(boot)),
        ("kernel W^X", &kernel_wx),
        ("kernel heap", &heap_test),
        ("breakpoint exception", &breakpoint),
        ("timer interrupts", &timer),
    ];
    let mut s = Summary { passed: 0, failed: 0 };
    for (name, t) in tests.iter() {
        match t() {
            Ok(()) => {
                kprintln!("[test] {name} ... ok");
                s.passed += 1;
            }
            Err(e) => {
                kprintln!("[test] {name} ... FAILED: {e}");
                s.failed += 1;
            }
        }
    }
    kprintln!("self-tests: {} passed, {} failed", s.passed, s.failed);
    s
}

fn memory_map(boot: &BootData) -> TestResult {
    let map = boot.memory_map();
    ensure!(!map.is_empty(), "empty memory map");
    for w in map.windows(2) {
        ensure!(w[0].end() <= w[1].phys_start, "regions overlap or unsorted at {:#x}", w[1].phys_start);
    }
    let k = &boot.info.kernel;
    let covered = map
        .iter()
        .any(|d| d.kind == MemoryKind::KernelImage && d.phys_start <= k.phys_base && k.phys_base + k.size <= d.end());
    ensure!(covered, "kernel image {:#x}+{:#x} not reported as KernelImage", k.phys_base, k.size);
    Ok(())
}

fn frame_allocator() -> TestResult {
    let (free_before, _) = mem::frame_stats();
    let mut frames = Vec::new();
    for i in 0..256u64 {
        let f = mem::alloc_frame().ok_or("out of frames")?;
        ensure!(f.is_multiple_of(PAGE_SIZE), "unaligned frame {f:#x}");
        // SAFETY: freshly allocated frame, reachable through the HHDM.
        unsafe { (phys_to_virt(f) as *mut u64).write(i ^ 0xA5A5_A5A5) };
        frames.push(f);
    }
    for (i, &f) in frames.iter().enumerate() {
        // SAFETY: as above.
        let v = unsafe { (phys_to_virt(f) as *const u64).read() };
        ensure!(v == i as u64 ^ 0xA5A5_A5A5, "frame {f:#x} content corrupted");
    }
    let mut sorted = frames.clone();
    sorted.sort_unstable();
    sorted.dedup();
    ensure!(sorted.len() == frames.len(), "a frame was handed out twice");
    for f in &frames {
        mem::free_frame(*f).map_err(|e| format!("free {f:#x}: {e:?}"))?;
    }
    ensure!(mem::free_frame(frames[0]).is_err(), "double free not detected");
    let (free_after, _) = mem::frame_stats();
    ensure!(free_after == free_before, "leak: {free_before} free before, {free_after} after");
    Ok(())
}

fn map_unmap() -> TestResult {
    let frame = mem::alloc_frame().ok_or("out of frames")?;
    paging::map(TEST_VA, frame, Flags::KERNEL_RW).map_err(|e| format!("map: {e:?}"))?;
    // SAFETY: TEST_VA was just mapped writable to `frame`.
    unsafe { (TEST_VA as *mut u64).write_volatile(0x0123_4567_89AB_CDEF) };
    // SAFETY: same frame through the HHDM.
    let via_hhdm = unsafe { (phys_to_virt(frame) as *const u64).read_volatile() };
    ensure!(via_hhdm == 0x0123_4567_89AB_CDEF, "aliasing through HHDM failed");
    let t = paging::translate(TEST_VA).ok_or("translate failed")?;
    ensure!(t.phys == frame && t.writable && t.page_size == PAGE_SIZE, "bad translation {t:?}");
    ensure!(!t.executable || !paging::nx_enabled(), "data page is executable");

    let expect = |r: Result<(), MapError>, e: MapError, what: &str| -> TestResult {
        ensure!(r == Err(e), "{what}: expected {e:?}, got {r:?}");
        Ok(())
    };
    expect(paging::map(TEST_VA, frame, Flags::KERNEL_RW), MapError::AlreadyMapped, "double map")?;
    expect(paging::map(TEST_VA + 8, frame, Flags::KERNEL_RW), MapError::Unaligned, "unaligned va")?;
    expect(paging::map(TEST_VA + PAGE_SIZE, frame + 1, Flags::KERNEL_RW), MapError::Unaligned, "unaligned pa")?;
    expect(paging::map(0x0000_8000_0000_0000, frame, Flags::KERNEL_RW), MapError::NonCanonical, "non-canonical")?;
    expect(paging::map(mem::hhdm_offset(), frame, Flags::KERNEL_RW), MapError::ReservedRange, "HHDM")?;
    expect(
        paging::map(run as *const () as u64 & !0xFFF, frame, Flags::KERNEL_RW),
        MapError::ReservedRange,
        "kernel image",
    )?;

    let pa = paging::unmap(TEST_VA).map_err(|e| format!("unmap: {e:?}"))?;
    ensure!(pa == frame, "unmap returned {pa:#x}, expected {frame:#x}");
    ensure!(paging::translate(TEST_VA).is_none(), "still mapped after unmap");
    ensure!(paging::unmap(TEST_VA) == Err(MapError::NotMapped), "second unmap must fail");
    ensure!(probe_read_u8(TEST_VA).is_err(), "unmapped page still readable");
    mem::free_frame(frame).map_err(|e| format!("free: {e:?}"))?;
    Ok(())
}

fn fixups() -> TestResult {
    ensure!(probe_read_u8(0xDEAD_BEEF_0000_0000).is_err(), "non-canonical read (#GP) not caught");
    ensure!(probe_read_u8(0xFFFF_D100_0000_0000).is_err(), "unmapped read (#PF) not caught");
    let ok = probe_read_u8(RODATA_PROBE.as_ptr() as u64);
    ensure!(ok == Ok(b'O'), "mapped read returned {ok:?}");
    ensure!(cpu::read_cr0() & (1 << 16) != 0, "CR0.WP is clear");
    // SAFETY: the write targets read-only data and must fault; if it did not,
    // it rewrites the byte with its existing value.
    let w = unsafe { probe_write_u8(RODATA_PROBE.as_ptr() as u64, b'O') };
    ensure!(w.is_err(), "write to read-only kernel data succeeded");
    Ok(())
}

fn identity_removed(boot: &BootData) -> TestResult {
    ensure!(probe_read_u8(0x1000).is_err(), "low memory still identity mapped");
    let k = boot.info.kernel.phys_base;
    ensure!(probe_read_u8(k).is_err(), "kernel physical address {k:#x} still identity mapped");
    ensure!(paging::translate(k).is_none(), "translate({k:#x}) succeeded");
    Ok(())
}

fn kernel_wx() -> TestResult {
    let code = paging::translate(run as *const () as u64).ok_or("code not mapped")?;
    ensure!(code.executable && !code.writable, "code page flags {code:?}");
    let ro = paging::translate(RODATA_PROBE.as_ptr() as u64).ok_or("rodata not mapped")?;
    ensure!(!ro.writable, "rodata writable");
    let rw = paging::translate(&raw const DATA_PROBE as u64).ok_or("data not mapped")?;
    ensure!(rw.writable, "data not writable");
    if paging::nx_enabled() {
        ensure!(!ro.executable && !rw.executable, "data executable (ro {ro:?}, rw {rw:?})");
    }
    Ok(())
}

fn heap_test() -> TestResult {
    let before = heap::stats();
    {
        let b = Box::new(0xDEAD_BEEFu64);
        ensure!(*b == 0xDEAD_BEEF, "box content");
        let v: Vec<u64> = (0..100_000).collect();
        ensure!(v.iter().sum::<u64>() == 4_999_950_000, "vec sum");
        let s = format!("{}-{:x}-{:?}", "OSyx", 0xC0FFEEu32, [1, 2, 3]);
        ensure!(s == "OSyx-c0ffee-[1, 2, 3]", "format produced {s:?}");
        let mut m = BTreeMap::new();
        for i in 0..2000u32 {
            m.insert(i.wrapping_mul(2_654_435_761), i);
        }
        for i in (0..2000u32).step_by(2) {
            m.remove(&i.wrapping_mul(2_654_435_761));
        }
        ensure!(m.len() == 1000, "btreemap len {}", m.len());
        // Larger than the initial heap: forces on-demand growth.
        let big = alloc::vec![0x5Au8; 8 << 20];
        ensure!(big.iter().all(|&b| b == 0x5A), "large allocation content");
        ensure!(heap::stats().capacity > heap::HEAP_INITIAL as usize, "heap did not grow");
    }
    let after = heap::stats();
    ensure!(after.used == before.used, "leak: {} bytes used before, {} after", before.used, after.used);
    Ok(())
}

fn breakpoint() -> TestResult {
    let before = interrupts::breakpoints();
    // SAFETY: int3 is handled by the breakpoint handler, which returns.
    unsafe { core::arch::asm!("int3") };
    ensure!(interrupts::breakpoints() == before + 1, "breakpoint handler did not run");
    Ok(())
}

fn timer() -> TestResult {
    ensure!(cpu::interrupts_enabled(), "interrupts disabled");
    let start = interrupts::ticks();
    for _ in 0..1000 {
        if interrupts::ticks() >= start + 5 {
            return Ok(());
        }
        cpu::enable_and_halt();
    }
    Err(format!("timer stuck at {} ticks", interrupts::ticks()))
}
