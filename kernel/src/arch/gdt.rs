//! Global Descriptor Table and Task State Segment.
//!
//! Selector layout is chosen for SYSCALL/SYSRET (STAR): kernel CS 0x08, SS
//! 0x10; user SS 0x18|3, CS 0x20|3. The TSS provides dedicated IST stacks for
//! exceptions that may arrive on a corrupted stack (#DF, NMI, #MC).

use core::arch::asm;
use core::mem::size_of;
use core::ptr::addr_of;

pub const KERNEL_CS: u16 = 0x08;
pub const KERNEL_DS: u16 = 0x10;
pub const USER_DS: u16 = 0x18 | 3;
pub const USER_CS: u16 = 0x20 | 3;
pub const TSS_SEL: u16 = 0x28;

// SYSRET loads SS = STAR[63:48] + 8 and CS = STAR[63:48] + 16 (RPL forced to
// 3), so the user data descriptor must directly precede user code.
const _: () = assert!(USER_DS + 8 == USER_CS && KERNEL_DS == KERNEL_CS + 8);

pub const IST_DOUBLE_FAULT: u8 = 1;
pub const IST_NMI: u8 = 2;
pub const IST_MACHINE_CHECK: u8 = 3;

const IST_STACK_SIZE: usize = 16 * 1024;

#[repr(C, packed(4))]
struct Tss {
    _reserved0: u32,
    rsp: [u64; 3],
    _reserved1: u64,
    ist: [u64; 7],
    _reserved2: u64,
    _reserved3: u16,
    iomap_base: u16,
}

#[repr(C, align(16))]
struct Stack([u8; IST_STACK_SIZE]);

static mut IST_STACKS: [Stack; 3] = [const { Stack([0; IST_STACK_SIZE]) }; 3];

static mut TSS: Tss = Tss {
    _reserved0: 0,
    rsp: [0; 3],
    _reserved1: 0,
    ist: [0; 7],
    _reserved2: 0,
    _reserved3: 0,
    iomap_base: size_of::<Tss>() as u16, // no I/O permission bitmap
};

static mut GDT: [u64; 7] = [
    0,
    0x00AF_9A00_0000_FFFF, // kernel code: present, DPL0, exec/read, long mode
    0x00CF_9200_0000_FFFF, // kernel data: present, DPL0, read/write
    0x00CF_F200_0000_FFFF, // user data:   present, DPL3, read/write
    0x00AF_FA00_0000_FFFF, // user code:   present, DPL3, exec/read, long mode
    0,                     // TSS low  (filled in by init)
    0,                     // TSS high
];

/// Sets the stack the CPU switches to when an interrupt or exception arrives
/// while running in ring 3 (TSS.RSP0).
///
/// # Safety
/// `top` must be the 16-byte aligned top of a mapped kernel stack that stays
/// valid while user code runs.
pub unsafe fn set_kernel_stack(top: u64) {
    let tss = &raw mut TSS;
    // The TSS is packed: write the field without creating a reference.
    core::ptr::addr_of_mut!((*tss).rsp).write_unaligned([top, 0, 0]);
}

#[repr(C, packed)]
struct Pointer {
    limit: u16,
    base: u64,
}

/// Loads the GDT, reloads all segment registers and the task register.
///
/// # Safety
/// Must be called once, on the boot CPU, before interrupts are enabled.
pub unsafe fn init() {
    let tss = &raw mut TSS;
    let stacks = &raw const IST_STACKS;
    for (i, ist) in [IST_DOUBLE_FAULT, IST_NMI, IST_MACHINE_CHECK].iter().enumerate() {
        let top = addr_of!((*stacks)[i]) as u64 + IST_STACK_SIZE as u64;
        (*tss).ist[*ist as usize - 1] = top;
    }
    let base = tss as u64;
    let limit = (size_of::<Tss>() - 1) as u64;
    let gdt = &raw mut GDT;
    (*gdt)[5] = (limit & 0xFFFF)
        | ((base & 0xFF_FFFF) << 16)
        | (0x89 << 40) // present, 64-bit available TSS
        | (((limit >> 16) & 0xF) << 48)
        | (((base >> 24) & 0xFF) << 56);
    (*gdt)[6] = base >> 32;

    let ptr = Pointer { limit: (size_of::<[u64; 7]>() - 1) as u16, base: gdt as u64 };
    asm!(
        "lgdt [{ptr}]",
        // Reload CS with a far return.
        "push {cs}",
        "lea {tmp}, [rip + 2f]",
        "push {tmp}",
        "retfq",
        "2:",
        "mov ds, {ds:x}",
        "mov es, {ds:x}",
        "mov ss, {ds:x}",
        "xor {tmp:e}, {tmp:e}",
        "mov fs, {tmp:x}",
        "mov gs, {tmp:x}",
        "ltr {tss:x}",
        ptr = in(reg) &ptr,
        cs = in(reg) KERNEL_CS as u64,
        ds = in(reg) KERNEL_DS as u64,
        tss = in(reg) TSS_SEL as u64,
        tmp = out(reg) _,
    );
}
