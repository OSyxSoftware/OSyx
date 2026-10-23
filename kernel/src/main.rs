//! OSyx kernel entry point and early initialisation.
//!
//! Init order matters:
//!  1. serial (diagnostics), BootInfo validation and copy into kernel memory
//!  2. own GDT/TSS/IDT (firmware tables live in memory we are about to reuse)
//!  3. frame allocator, NX state, removal of the boot identity map
//!  4. kernel heap, then reclaim of bootloader memory
//!  5. PIC/PIT timer, interrupts on, framebuffer status screen, self-tests
//!  6. scheduler (this flow becomes thread "kmain")
//!  7. PCI enumeration, disk drivers, volume mounts (drivers sleep, so they
//!     need the scheduler), then self-tests and the init program in ring 3

#![no_std]
#![no_main]

extern crate alloc;

mod arch;
mod block;
mod devices;
mod display;
mod drivers;
mod fixup;
mod futex;
mod interrupts;
mod mem;
mod modules;
mod pci;
mod process;
mod qemu;
mod random;
mod sched;
mod selftest;
mod selftest_fs;
mod serial;
mod storage;
mod sync;
mod syscall;
mod time;
mod vfs;

use alloc::vec::Vec;
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
        syscall::init();
    }
    let cpuinfo = cpu::CpuInfo::detect();
    kprintln!("cpu: {}", cpuinfo);
    // SAFETY: CR0.WP makes read-only pages read-only for ring 0 too; SSE is
    // enabled for user code (the kernel itself is soft-float).
    unsafe {
        core::arch::asm!("mov rax, cr0", "or rax, 0x10000", "mov cr0, rax", out("rax") _);
        cpu::enable_sse();
    }

    log_memory_map(boot.memory_map());
    // SAFETY: own GDT/IDT are loaded; called once.
    unsafe { mem::init(boot.memory_map(), raw.hhdm_offset, raw.hhdm_size) };
    mem::paging::init();
    let removed = mem::paging::remove_identity_map();
    kprintln!("paging: NX {}, removed {} identity-map PML4 entries", mem::paging::nx_enabled(), removed);
    let heap = mem::heap::init();
    kprintln!("heap: {} KiB mapped at {:#x}", heap >> 10, mem::heap::HEAP_START);
    // Boot modules live in bootloader memory: copy them before reclaiming.
    // SAFETY: BootInfo was validated; nothing has been reclaimed yet.
    let module_count = unsafe { modules::init(&raw) };
    let reclaimed = mem::reclaim_bootloader_memory(boot.memory_map());
    let (free, total) = mem::frame_stats();
    kprintln!("reclaimed {} KiB of boot loader memory; {} / {} MiB free", reclaimed * 4, free >> 8, total >> 8);

    // SAFETY: IDT is loaded, interrupts still disabled.
    unsafe { arch::pic::init() };
    cpu::enable_interrupts();
    kprintln!("timer: PIT at {} Hz, interrupts enabled", arch::pic::TIMER_HZ);
    sched::init();
    kprintln!("scheduler: round robin, {} ms time slice", sched::TIME_SLICE_TICKS * 10);
    let pci_devices = pci::enumerate();
    pci::log_devices(&pci_devices);
    match time::init() {
        Some(secs) => kprintln!("time: RTC says {} UTC", time::format_utc(secs)),
        None => kprintln!("time: RTC unreadable; timestamps count from boot"),
    }
    let disks = block::probe(&pci_devices);
    let partitions = block::scan_partitions();
    random::init(&pci_devices);
    devices::init();
    let mounted = storage::mount_all(boot);
    kprintln!(
        "storage: {} PCI functions, {} disks, {} partitions, {} mounts, root: {}",
        pci_devices.len(),
        disks,
        partitions,
        mounted.mounts,
        mounted.root
    );

    let mut footer = Line::new();
    let fb = &raw.framebuffer;
    let _ = write!(footer, "{}x{}  {} MiB RAM", fb.width, fb.height, total >> 8);
    let has_display = raw.has_framebuffer() && display::init(fb, footer.as_str());
    kprintln!("display: {}", if has_display { "framebuffer console active" } else { "none" });
    display::status("Running self-tests", osyx_gfx::splash::TEXT, &[]);

    let summary = selftest::run(boot);
    let mut line1 = Line::new();
    let (free, _) = mem::frame_stats();
    let _ =
        write!(line1, "Self-tests: {} passed, {} failed  |  {} MiB free", summary.passed, summary.failed, free >> 8);
    let mut ok = summary.failed == 0;
    display::status("Starting init", osyx_gfx::splash::TEXT, &[line1.as_str()]);

    let mut line2 = Line::new();
    let init_name = boot.option_value("init").unwrap_or("init");
    if modules::get(init_name).is_some() {
        ok &= run_init(boot, init_name, &mut line2);
    } else {
        kprintln!("no init program {:?} among {} boot modules", init_name, module_count);
        let _ = write!(line2, "No init program (\\EFI\\OSYX\\BIN\\{})", init_name);
    }
    let color = if ok { osyx_gfx::splash::OK } else { osyx_gfx::splash::ERROR };
    let title = if ok { "OSyx kernel is running" } else { "OSyx: a check failed" };
    display::status(title, color, &[line1.as_str(), line2.as_str()]);

    if let Some(kind) = boot.option_value("test.fault") {
        trigger_fault(kind);
    }
    if boot.has_option("test.exit") {
        vfs::shutdown();
        kprintln!("test.exit: leaving QEMU ({})", if ok { "success" } else { "failure" });
        qemu::exit(if ok { qemu::ExitCode::Success } else { qemu::ExitCode::TestFailure });
    }
    kprintln!("entering idle loop");
    idle(line1, line2, ok)
}

/// Runs the init program with `init.args=a,b,c` and checks its exit status
/// against `init.expect=N` (default 0). Returns whether it matched.
fn run_init(boot: &BootData, name: &str, summary: &mut Line) -> bool {
    let mut args: Vec<&str> = alloc::vec![name];
    if let Some(extra) = boot.option_value("init.args") {
        args.extend(extra.split(',').filter(|a| !a.is_empty()));
    }
    let expect = boot.option_value("init.expect").and_then(parse_int).unwrap_or(0);
    let result = process::spawn(process::KERNEL_PID, name, &args)
        .map_err(|e| alloc::format!("{e}"))
        .and_then(|pid| process::wait(process::KERNEL_PID, pid).map_err(|e| alloc::format!("{e:?}")));
    match result {
        Ok((_, status)) => {
            let st = sched::stats();
            kprintln!(
                "scheduler: {} context switches, {} threads alive ({} ready), {} processes left",
                st.switches,
                st.threads,
                st.ready,
                process::count()
            );
            sched::dump();
            let ok = status == expect;
            kprintln!(
                "init: exit status {:#x}, expected {:#x}: {}",
                status,
                expect,
                if ok { "ok" } else { "MISMATCH" }
            );
            let tail = process::output_tail();
            let last = tail.last().map(|s| s.as_str()).unwrap_or("");
            let _ = write!(summary, "init exited with {}: \"{}\"", status, last);
            ok
        }
        Err(e) => {
            kprintln!("init: failed to start: {}", e);
            let _ = write!(summary, "init failed to start: {}", e);
            false
        }
    }
}

fn parse_int(s: &str) -> Option<i32> {
    match s.strip_prefix("0x") {
        Some(hex) => u32::from_str_radix(hex, 16).ok().map(|v| v as i32),
        None => s.parse().ok(),
    }
}

/// kmain's final loop: animates the status screen while the idle thread
/// halts the CPU in between.
fn idle(line1: Line, line2: Line, ok: bool) -> ! {
    let mut last = 0;
    loop {
        sched::sleep_ticks(4);
        let t = interrupts::ticks();
        display::spin((t * 4) as u32);
        if t / 100 != last / 100 {
            let mut up = Line::new();
            let _ = write!(up, "Uptime {} s", t / 100);
            let (title, color) = if ok {
                ("OSyx kernel is running", osyx_gfx::splash::OK)
            } else {
                ("OSyx: a check failed", osyx_gfx::splash::ERROR)
            };
            display::status(title, color, &[line1.as_str(), line2.as_str(), up.as_str()]);
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
