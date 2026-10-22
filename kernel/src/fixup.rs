//! Exception fixups: instructions that are allowed to fault.
//!
//! Each probe routine has an entry `(faulting_instruction, recovery_address)`
//! in the `.osyx_fixup` section. When a #PF or #GP hits such an instruction,
//! the dispatcher resumes at the recovery address instead of panicking.
//! [`copy_from_user`] / [`copy_to_user`] are built on this: user pointers are
//! range checked, and an unmapped or read-only user page turns into
//! `Errno::Fault` instead of a kernel crash.

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

    .global osyx_copy_bytes
osyx_copy_bytes:
    mov rcx, rdx
5:  rep movsb
    xor eax, eax
    ret
6:  mov eax, 1
    ret

    .pushsection .osyx_fixup, "a"
    .balign 8
    .quad 1b, 2b
    .quad 3b, 4b
    .quad 5b, 6b
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
    fn osyx_copy_bytes(dst: *mut u8, src: *const u8, len: usize) -> u32;
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

/// True if `[addr, addr + len)` lies entirely inside mappable user space.
pub fn user_range_ok(addr: u64, len: u64) -> bool {
    if len == 0 {
        return true;
    }
    addr >= crate::mem::paging::USER_START && addr.checked_add(len).is_some_and(|end| end <= osyx_abi::USER_END)
}

/// Copies `dst.len()` bytes from user address `src`.
pub fn copy_from_user(dst: &mut [u8], src: u64) -> Result<(), osyx_abi::Errno> {
    if !user_range_ok(src, dst.len() as u64) {
        return Err(osyx_abi::Errno::Fault);
    }
    // SAFETY: `dst` is a valid kernel buffer; faults on the user side are
    // recovered through the fixup table.
    match unsafe { osyx_copy_bytes(dst.as_mut_ptr(), src as *const u8, dst.len()) } {
        0 => Ok(()),
        _ => Err(osyx_abi::Errno::Fault),
    }
}

/// Copies `src` to user address `dst`. Fails on unmapped or read-only pages
/// (CR0.WP makes read-only user pages read-only for the kernel too).
pub fn copy_to_user(dst: u64, src: &[u8]) -> Result<(), osyx_abi::Errno> {
    if !user_range_ok(dst, src.len() as u64) {
        return Err(osyx_abi::Errno::Fault);
    }
    // SAFETY: `src` is a valid kernel buffer; faults on the user side are
    // recovered through the fixup table.
    match unsafe { osyx_copy_bytes(dst as *mut u8, src.as_ptr(), src.len()) } {
        0 => Ok(()),
        _ => Err(osyx_abi::Errno::Fault),
    }
}
