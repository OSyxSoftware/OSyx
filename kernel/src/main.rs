//! OSyx kernel entry point and early initialisation.
//!
//! Init order matters:
//!  1. serial (diagnostics), BootInfo validation and copy into kernel memory
//!  2. own GDT/TSS/IDT (firmware tables live in memory we are about to reuse)
//!  3. frame allocator, NX state, removal of the boot identity map
//!  4. kernel heap, then reclaim of bootloader memory
//!  5. PIC/PIT timer, interrupts on, framebuffer status screen, self-tests

#![no_std]
#![no_main]

extern crate alloc;

mod arch;
mod display;
mod fixup;
mod interrupts;
mod mem;
mod qemu;
mod selftest;
mod serial;
mod sync;

use arch::cpu;
use core::fmt::Write as _;
use osyx_bootinfo::{BootInfo, MemoryDescriptor, MemoryKind};
use sync::BootCell;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const MAX_MEMORY_REGIONS: usize = 512;
const MAX_CMDLINE: usize = 4096;

/// Everything the kernel keeps from the boot loader, copied out of
/// bootloader-owned memory so that memory can be reclaimed.
pub struct BootData {
    pub info: BootInfo,
    map: [MemoryDescriptor; MAX_MEMORY_REGIONS],
    map_len: usize,
    cmdline: [u8; MAX_CMDLINE],
    cmdline_len: usize,
}

impl BootData {
    pub fn memory_map(&self) -> &[MemoryDescriptor] {
        &self.map[..self.map_len]
    }

    pub fn cmdline(&self) -> &str {
        core::str::from_utf8(&self.cmdline[..self.cmdline_len]).unwrap_or("")
    }

    pub fn has_option(&self, opt: &str) -> bool {
        self.cmdline().split_ascii_whitespace().any(|w| w == opt)
    }

    pub fn option_value(&self, key: &str) -> Option<&str> {
        self.cmdline().split_ascii_whitespace().find_map(|w| w.strip_prefix(key)?.strip_prefix('='))
    }
}

static BOOT: BootCell<BootData> = BootCell::new();

/// Kernel entry, reached from the boot loader with the HHDM address of the
/// BootInfo in RDI, interrupts disabled and a 128 KiB stack.
///
/// # Safety
/// Only the OSyx boot loader may call this, as described in `osyx_bootinfo`.
#[no_mangle]
#[link_section = ".text.entry"]
pub unsafe extern "C" fn _start(info: *const BootInfo) -> ! {
    serial::init();
    kprintln!("OSyx kernel {} (x86_64) starting", VERSION);
    if info.is_null() {
        kprintln!("FATAL: no BootInfo");
        cpu::halt_forever();
    }
    // SAFETY: the boot loader passes a pointer to a BootInfo in mapped memory;
    // the header is validated before any field is trusted.
    let raw = unsafe { info.read() };
    if let Err(e) = raw.validate() {
        kprintln!("FATAL: invalid BootInfo: {:?}", e);
        cpu::halt_forever();
    }
    // SAFETY: the header is valid, so the map and command line pointers come
    // from the OSyx loader and point into its (still intact) BootInfo region.
    let boot = unsafe { BOOT.init(copy_boot_data(&raw)) };
    kprintln!(
        "boot loader {}.{}.{}, command line: {:?}",
        raw.bootloader_version >> 16,
        (raw.bootloader_version >> 8) & 0xFF,
        raw.bootloader_version & 0xFF,
        boot.cmdline()
    );

    // SAFETY: single CPU, interrupts disabled, called exactly once.
    unsafe {
        arch::gdt::init();
        arch::idt::init();
    }
    let cpuinfo = cpu::CpuInfo::detect();
    kprintln!("cpu: {}", cpuinfo);
    // SAFETY: CR0.WP makes read-only pages read-only for ring 0 too.
    unsafe { core::arch::asm!("mov rax, cr0", "or rax, 0x10000", "mov cr0, rax", out("rax") _) };

    log_memory_map(boot.memory_map());
    // SAFETY: own GDT/IDT are loaded; called once.
    unsafe { mem::init(boot.memory_map(), raw.hhdm_offset, raw.hhdm_size) };
    mem::paging::init();
    let removed = mem::paging::remove_identity_map();
    kprintln!("paging: NX {}, removed {} identity-map PML4 entries", mem::paging::nx_enabled(), removed);
    let heap = mem::heap::init();
    kprintln!("heap: {} KiB mapped at {:#x}", heap >> 10, mem::heap::HEAP_START);
    let reclaimed = mem::reclaim_bootloader_memory(boot.memory_map());
    let (free, total) = mem::frame_stats();
    kprintln!("reclaimed {} KiB of boot loader memory; {} / {} MiB free", reclaimed * 4, free >> 8, total >> 8);

    // SAFETY: IDT is loaded, interrupts still disabled.
    unsafe { arch::pic::init() };
    cpu::enable_interrupts();
    kprintln!("timer: PIT at {} Hz, interrupts enabled", arch::pic::TIMER_HZ);

    let mut footer = Line::new();
    let fb = &raw.framebuffer;
    let _ = write!(footer, "{}x{}  {} MiB RAM", fb.width, fb.height, total >> 8);
    let has_display = raw.has_framebuffer() && display::init(fb, footer.as_str());
    kprintln!("display: {}", if has_display { "framebuffer console active" } else { "none" });
    display::status("Running self-tests", osyx_gfx::splash::TEXT, &[]);

    let summary = selftest::run(boot);
    let mut line1 = Line::new();
    let _ = write!(line1, "Self-tests: {} passed, {} failed", summary.passed, summary.failed);
    let (free, _) = mem::frame_stats();
    let hs = mem::heap::stats();
    let mut line2 = Line::new();
    let _ = write!(line2, "{} MiB free  |  heap {} KiB used of {} KiB", free >> 8, hs.used >> 10, hs.capacity >> 10);
    let ok = summary.failed == 0;
    let color = if ok { osyx_gfx::splash::OK } else { osyx_gfx::splash::ERROR };
    let title = if ok { "OSyx kernel is running" } else { "Kernel self-tests failed" };
    display::status(title, color, &[line1.as_str(), line2.as_str()]);

    if let Some(kind) = boot.option_value("test.fault") {
        trigger_fault(kind);
    }
    if boot.has_option("test.exit") {
        kprintln!("test.exit: leaving QEMU ({})", if ok { "success" } else { "failure" });
        qemu::exit(if ok { qemu::ExitCode::Success } else { qemu::ExitCode::TestFailure });
    }
    kprintln!("entering idle loop");
    idle(line1, line2)
}

fn idle(line1: Line, line2: Line) -> ! {
    let mut last = 0;
    loop {
        cpu::enable_and_halt();
        let t = interrupts::ticks();
        if t / 4 != last / 4 {
            display::spin((t * 4) as u32);
        }
        if t / 100 != last / 100 {
            let mut up = Line::new();
            let _ = write!(up, "Uptime {} s", t / 100);
            let title = "OSyx kernel is running";
            display::status(title, osyx_gfx::splash::OK, &[line1.as_str(), line2.as_str(), up.as_str()]);
        }
        last = t;
    }
}

/// Deliberately raises a CPU exception to exercise the diagnostics path.
fn trigger_fault(kind: &str) {
    kprintln!("test.fault: triggering {kind}");
    // SAFETY: every branch intentionally faults; the handler reports and panics.
    unsafe {
        match kind {
            "pf" => {
                let _ = core::ptr::read_volatile(0xFFFF_D100_0000_1000 as *const u64);
            }
            "gp" => {
                let _ = core::ptr::read_volatile(0x8000_0000_0000_0000u64 as *const u64);
            }
            "ud" => core::arch::asm!("ud2"),
            "de" => {
                core::arch::asm!("xor edx, edx", "xor eax, eax", "xor ecx, ecx", "div ecx", out("eax") _, out("edx") _, out("ecx") _)
            }
            _ => kprintln!("test.fault: unknown kind {kind:?} (pf, gp, ud, de)"),
        }
    }
}

/// # Safety
/// `raw` must have passed `validate()` so its pointers are the loader's.
unsafe fn copy_boot_data(raw: &BootInfo) -> BootData {
    let mut b = BootData {
        info: *raw,
        map: [MemoryDescriptor { phys_start: 0, page_count: 0, kind: MemoryKind::Reserved, _reserved: 0 };
            MAX_MEMORY_REGIONS],
        map_len: 0,
        cmdline: [0; MAX_CMDLINE],
        cmdline_len: 0,
    };
    let src = raw.memory_map();
    if src.len() > MAX_MEMORY_REGIONS {
        kprintln!("warning: {} memory regions, only {} kept", src.len(), MAX_MEMORY_REGIONS);
    }
    let n = src.len().min(MAX_MEMORY_REGIONS);
    b.map[..n].copy_from_slice(&src[..n]);
    b.map_len = n;
    let cmd = raw.cmdline().as_bytes();
    let n = cmd.len().min(MAX_CMDLINE);
    b.cmdline[..n].copy_from_slice(&cmd[..n]);
    b.cmdline_len = n;
    b
}

fn log_memory_map(map: &[MemoryDescriptor]) {
    let mut usable = 0;
    for d in map {
        if d.kind == MemoryKind::Usable {
            usable += d.byte_len();
        }
    }
    kprintln!("memory map: {} regions, {} MiB usable", map.len(), usable >> 20);
    for d in map.iter().filter(|d| d.kind != MemoryKind::Usable && d.kind != MemoryKind::Reserved) {
        kprintln!("  {:#012x}-{:#012x} {:?}", d.phys_start, d.end(), d.kind);
    }
}

/// Small fixed-capacity string for status lines (usable in the panic path).
pub struct Line {
    buf: [u8; 96],
    len: usize,
}

impl Line {
    pub const fn new() -> Line {
        Line { buf: [0; 96], len: 0 }
    }

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl Default for Line {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Write for Line {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for ch in s.chars() {
            let mut tmp = [0u8; 4];
            let e = ch.encode_utf8(&mut tmp).as_bytes();
            if self.len + e.len() > self.buf.len() {
                return Err(core::fmt::Error);
            }
            self.buf[self.len..self.len + e.len()].copy_from_slice(e);
            self.len += e.len();
        }
        Ok(())
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    cpu::disable_interrupts();
    // SAFETY: we never return to whoever may hold the serial lock.
    unsafe { serial::SERIAL.force_unlock() };
    kprintln!("KERNEL PANIC: {}", info);
    let mut msg = Line::new();
    let _ = write!(msg, "{}", info.message());
    display::panic_screen(msg.as_str());
    if BOOT.get().is_some_and(|b| b.cmdline().contains("test.")) {
        qemu::exit(qemu::ExitCode::Panic);
    }
    cpu::halt_forever()
}
