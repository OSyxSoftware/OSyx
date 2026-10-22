//! Exception fixups: instructions that are allowed to fault.
//!
//! Each probe routine has an entry `(faulting_instruction, recovery_address)`
//! in the `.osyx_fixup` section. When a #PF or #GP hits such an instruction,
//! the dispatcher resumes at the recovery address instead of panicking. This
//! is the mechanism future `copy_from_user` / `copy_to_user` will use to
//! validate user pointers without trusting them.

use core::arch::global_asm;

global_asm!(
    r#"
    .section .text
    .global osyx_probe_read_u8
osyx_probe_read_u8:
    xor eax, eax
1:  movzx ecx, byte ptr [rdi]
    mov byte ptr [rsi], cl
    ret
2:  mov eax, 1
    ret

    .global osyx_probe_write_u8
osyx_probe_write_u8:
    xor eax, eax
3:  mov byte ptr [rdi], sil
    ret
4:  mov eax, 1
    ret

    .pushsection .osyx_fixup, "a"
    .balign 8
    .quad 1b, 2b
    .quad 3b, 4b
    .popsection
    "#
);

#[repr(C)]
struct Fixup {
    insn: u64,
    recover: u64,
}

extern "C" {
    fn osyx_probe_read_u8(addr: u64, out: *mut u8) -> u32;
    fn osyx_probe_write_u8(addr: u64, value: u8) -> u32;
    static __fixup_start: Fixup;
    static __fixup_end: Fixup;
}

/// Recovery address for a faulting instruction pointer, if it has one.
pub fn lookup(rip: u64) -> Option<u64> {
    let start = &raw const __fixup_start;
    let end = &raw const __fixup_end;
    let count = (end as usize - start as usize) / core::mem::size_of::<Fixup>();
    // SAFETY: the linker script places a contiguous array of `Fixup` between
    // the two symbols.
    let table = unsafe { core::slice::from_raw_parts(start, count) };
    table.iter().find(|f| f.insn == rip).map(|f| f.recover)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fault;

/// Reads a byte from an arbitrary (possibly unmapped or non-canonical)
/// address without crashing the kernel.
pub fn probe_read_u8(addr: u64) -> Result<u8, Fault> {
    let mut v = 0u8;
    // SAFETY: the routine either completes or is redirected by the fixup.
    let r = unsafe { osyx_probe_read_u8(addr, &mut v) };
    if r == 0 {
        Ok(v)
    } else {
        Err(Fault)
    }
}

/// Writes a byte to an arbitrary address, reporting faults instead of crashing.
///
/// # Safety
/// A successful write modifies memory at `addr`; the caller must ensure that
/// is acceptable (e.g. the page belongs to the caller).
pub unsafe fn probe_write_u8(addr: u64, value: u8) -> Result<(), Fault> {
    if osyx_probe_write_u8(addr, value) == 0 {
        Ok(())
    } else {
        Err(Fault)
    }
}
