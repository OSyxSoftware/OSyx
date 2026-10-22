//! Interrupt Descriptor Table, entry stubs and the common dispatcher.
//!
//! Every vector has a 16-byte stub in `osyx_isr_table` that normalises the
//! stack (pushes a zero error code where the CPU does not), pushes the vector
//! number and jumps to `osyx_isr_common`, which saves all general purpose
//! registers into a [`TrapFrame`] and calls [`osyx_interrupt_dispatch`].

use super::gdt::{self, KERNEL_CS};
use core::arch::{asm, global_asm};
use core::mem::size_of;

global_asm!(
    r#"
    .section .text
    .balign 16
    .global osyx_isr_table
osyx_isr_table:
    .set vec, 0
    .rept 256
    .balign 16
    .set has_err, (vec == 8) | (vec == 10) | (vec == 11) | (vec == 12) | (vec == 13) | (vec == 14) | (vec == 17) | (vec == 21) | (vec == 29) | (vec == 30)
    .if has_err == 0
    pushq $0
    .endif
    pushq $vec
    jmp osyx_isr_common
    .set vec, vec + 1
    .endr

osyx_isr_common:
    pushq %rax
    pushq %rbx
    pushq %rcx
    pushq %rdx
    pushq %rsi
    pushq %rdi
    pushq %rbp
    pushq %r8
    pushq %r9
    pushq %r10
    pushq %r11
    pushq %r12
    pushq %r13
    pushq %r14
    pushq %r15
    movq %rsp, %rdi
    cld
    call osyx_interrupt_dispatch
    popq %r15
    popq %r14
    popq %r13
    popq %r12
    popq %r11
    popq %r10
    popq %r9
    popq %r8
    popq %rbp
    popq %rdi
    popq %rsi
    popq %rdx
    popq %rcx
    popq %rbx
    popq %rax
    addq $16, %rsp
    iretq
    "#,
    options(att_syntax)
);

extern "C" {
    static osyx_isr_table: [u8; 256 * 16];
}

/// Register state saved on interrupt entry (lowest address first).
#[repr(C)]
#[derive(Debug)]
pub struct TrapFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    pub vector: u64,
    pub error_code: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Gate {
    offset_low: u16,
    selector: u16,
    ist: u8,
    attributes: u8,
    offset_mid: u16,
    offset_high: u32,
    _zero: u32,
}

static mut IDT: [Gate; 256] =
    [Gate { offset_low: 0, selector: 0, ist: 0, attributes: 0, offset_mid: 0, offset_high: 0, _zero: 0 }; 256];

#[repr(C, packed)]
struct Pointer {
    limit: u16,
    base: u64,
}

/// Installs interrupt gates for all 256 vectors and loads the IDT.
///
/// # Safety
/// Must run once after `gdt::init` (the gates reference KERNEL_CS and the IST).
pub unsafe fn init() {
    let table = &raw const osyx_isr_table as u64;
    let idt = &raw mut IDT;
    for v in 0..256usize {
        let handler = table + v as u64 * 16;
        let ist = match v {
            8 => gdt::IST_DOUBLE_FAULT,
            2 => gdt::IST_NMI,
            18 => gdt::IST_MACHINE_CHECK,
            _ => 0,
        };
        (*idt)[v] = Gate {
            offset_low: handler as u16,
            selector: KERNEL_CS,
            ist,
            attributes: 0x8E, // present, DPL0, 64-bit interrupt gate (clears IF)
            offset_mid: (handler >> 16) as u16,
            offset_high: (handler >> 32) as u32,
            _zero: 0,
        };
    }
    let ptr = Pointer { limit: (size_of::<[Gate; 256]>() - 1) as u16, base: idt as u64 };
    asm!("lidt [{}]", in(reg) &ptr, options(readonly, nostack, preserves_flags));
}

pub const EXCEPTION_NAMES: [&str; 32] = [
    "Divide error (#DE)",
    "Debug (#DB)",
    "Non-maskable interrupt",
    "Breakpoint (#BP)",
    "Overflow (#OF)",
    "Bound range exceeded (#BR)",
    "Invalid opcode (#UD)",
    "Device not available (#NM)",
    "Double fault (#DF)",
    "Coprocessor segment overrun",
    "Invalid TSS (#TS)",
    "Segment not present (#NP)",
    "Stack-segment fault (#SS)",
    "General protection fault (#GP)",
    "Page fault (#PF)",
    "Reserved",
    "x87 floating-point exception (#MF)",
    "Alignment check (#AC)",
    "Machine check (#MC)",
    "SIMD floating-point exception (#XM)",
    "Virtualization exception (#VE)",
    "Control protection exception (#CP)",
    "Reserved",
    "Reserved",
    "Reserved",
    "Reserved",
    "Reserved",
    "Reserved",
    "Hypervisor injection exception (#HV)",
    "VMM communication exception (#VC)",
    "Security exception (#SX)",
    "Reserved",
];
