//! Exception reporting for the window between ExitBootServices and the
//! kernel's own IDT.
//!
//! After ExitBootServices the firmware IDT still points at firmware handlers;
//! on real machines those print to an absent serial port and spin, so a fault
//! in the hand-off or in the kernel's first instructions just looks like a
//! frozen splash screen. This IDT catches the 32 CPU exceptions instead and
//! shows the vector, RIP and CR2 on the screen (and COM1), then halts.
//!
//! Everything the handlers touch lives in loader memory (code, the IDT, a
//! private stack, the screen's back buffers), which the boot page tables
//! identity map and the kernel does not reuse before it loads its own IDT.

use crate::screen::Screen;
use core::arch::{asm, global_asm};
use core::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

const VECTORS: usize = 32;
/// Bytes per entry stub (see the `.balign` in the assembly below).
const STUB_SIZE: u64 = 16;
const STACK_SIZE: usize = 16 * 1024;

static SCREEN: AtomicPtr<Screen> = AtomicPtr::new(core::ptr::null_mut());
static KERNEL_BASE: AtomicU64 = AtomicU64::new(0);
static KERNEL_END: AtomicU64 = AtomicU64::new(0);

#[repr(C, align(16))]
struct Idt([u64; VECTORS * 2]);
static mut IDT: Idt = Idt([0; VECTORS * 2]);

#[repr(C, align(16))]
struct Stack([u8; STACK_SIZE]);
#[no_mangle]
static mut LOADER_FAULT_STACK: Stack = Stack([0; STACK_SIZE]);

// One 16-byte stub per vector: push a dummy error code where the CPU pushes
// none, push the vector, then switch to the private stack (the faulting one
// may be the cause) and call the reporter with the old RSP.
macro_rules! stubs {
    ($($v:literal $err:literal),*) => {
        global_asm!(
            ".globl loader_fault_stubs",
            ".balign 16",
            "loader_fault_stubs:",
            $(
                ".balign 16",
                concat!(".if ", $err, " == 0"),
                "push 0",
                ".endif",
                concat!("push ", $v),
                "jmp 2f",
            )*
            "2:",
            "mov rdi, rsp",
            "lea rsp, [rip + LOADER_FAULT_STACK + {stack}]",
            "call {report}",
            "ud2",
            stack = const STACK_SIZE,
            report = sym report,
        );
    };
}
stubs!(
    0 0, 1 0, 2 0, 3 0, 4 0, 5 0, 6 0, 7 0, 8 1, 9 0, 10 1, 11 1, 12 1, 13 1, 14 1, 15 0,
    16 0, 17 1, 18 0, 19 0, 20 0, 21 1, 22 0, 23 0, 24 0, 25 0, 26 0, 27 0, 28 0, 29 1, 30 1, 31 0
);

extern "C" {
    static loader_fault_stubs: u8;
}

const NAMES: [&str; VECTORS] = [
    "divide error", "debug", "NMI", "breakpoint", "overflow", "bound range", "invalid opcode",
    "device not available", "double fault", "coprocessor overrun", "invalid TSS", "segment not present",
    "stack fault", "general protection", "page fault", "reserved", "x87 error", "alignment check",
    "machine check", "SIMD error", "virtualization", "control protection", "reserved", "reserved",
    "reserved", "reserved", "reserved", "reserved", "hypervisor injection", "VMM communication",
    "security", "reserved",
];

/// Loads the reporting IDT. Interrupts must be disabled; `screen` (if any)
/// must stay valid until the kernel replaces the IDT.
///
/// # Safety
/// Only after ExitBootServices, with IF clear, from the loader's own code.
pub unsafe fn install(screen: Option<&mut Screen>, kernel_base: u64, kernel_size: u64) {
    SCREEN.store(screen.map_or(core::ptr::null_mut(), |s| s as *mut Screen), Ordering::Relaxed);
    KERNEL_BASE.store(kernel_base, Ordering::Relaxed);
    KERNEL_END.store(kernel_base.wrapping_add(kernel_size), Ordering::Relaxed);
    let cs: u16;
    asm!("mov {0:x}, cs", out(reg) cs, options(nomem, nostack, preserves_flags));
    let base = &raw const loader_fault_stubs as u64;
    let idt = &raw mut IDT;
    for v in 0..VECTORS {
        let h = base + v as u64 * STUB_SIZE;
        // 64-bit interrupt gate, present, DPL 0, IST 0.
        let low = (h & 0xFFFF) | (cs as u64) << 16 | 0x8E00u64 << 32 | ((h >> 16) & 0xFFFF) << 48;
        (*idt).0[v * 2] = low;
        (*idt).0[v * 2 + 1] = h >> 32;
    }
    let mut desc = [0u8; 10];
    desc[..2].copy_from_slice(&((VECTORS * 16 - 1) as u16).to_le_bytes());
    desc[2..].copy_from_slice(&(idt as u64).to_le_bytes());
    asm!("lidt [{}]", in(reg) desc.as_ptr(), options(readonly, nostack, preserves_flags));
}

/// `frame`: vector, error code, then the CPU's RIP, CS, RFLAGS, RSP, SS.
extern "sysv64" fn report(frame: *const u64) -> ! {
    // SAFETY: the stub passes the RSP at which it pushed these seven words.
    let f = unsafe { core::slice::from_raw_parts(frame, 7) };
    let (vector, error, rip, rsp) = (f[0] as usize, f[1], f[2], f[5]);
    let (cr2, cr3): (u64, u64);
    // SAFETY: reading control registers in ring 0 has no side effects.
    unsafe {
        asm!("mov {}, cr2", out(reg) cr2, options(nomem, nostack, preserves_flags));
        asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags));
    }
    let name = NAMES.get(vector).copied().unwrap_or("?");
    let (kb, ke) = (KERNEL_BASE.load(Ordering::Relaxed), KERNEL_END.load(Ordering::Relaxed));
    let place = if (kb..ke).contains(&rip) { "kernel" } else { "loader/firmware" };
    let line1 = crate::fmt_buf!(128, "#{} {} (error {:#x}) at RIP {:#x} ({}, +{:#x})", vector, name, error, rip, place, rip.wrapping_sub(kb));
    let line2 = crate::fmt_buf!(128, "CR2 {:#x}  RSP {:#x}  CR3 {:#x}", cr2, rsp, cr3);
    crate::log!("CPU EXCEPTION before the kernel IDT: {}", line1.as_str());
    crate::log!("{}", line2.as_str());
    let s = SCREEN.load(Ordering::Relaxed);
    if !s.is_null() {
        // SAFETY: `install` stored a screen that outlives the hand-off; nothing
        // else runs once the CPU is here.
        unsafe { (*s).error("CPU exception while starting the kernel", &[line1.as_str(), line2.as_str()]) };
    }
    loop {
        // SAFETY: halting is always sound.
        unsafe { asm!("cli; hlt", options(nomem, nostack)) };
    }
}
