//! Imports that only 32-bit code can implement: they read or walk the
//! caller's own x86 registers and frame chain, which the 64-bit gate does
//! not keep. The bridge binds these imports straight to the routines below
//! (winrun's text is below 4 GiB, so 32-bit code calls them directly).
//!
//! RtlCaptureContext follows the x86 Windows implementation: the integer
//! and segment registers of the caller, and the control registers of the
//! caller's frame (EIP = [EBP+4], EBP = [EBP], ESP = EBP+8), so the
//! context describes the function that called RtlCaptureContext's caller
//! as seen through its standard EBP frame.
//!
//! RtlCaptureStackBackTrace walks the EBP chain from the caller, inside
//! the thread's stack bounds (TEB StackLimit/StackBase at fs:[8]/fs:[4]),
//! and reports return addresses; the hash is their 32-bit sum.

use core::arch::global_asm;

// CONTEXT (x86) offsets: ContextFlags 0x00, SegGs 0x8C, SegFs 0x90,
// SegEs 0x94, SegDs 0x98, Edi 0x9C, Esi 0xA0, Ebx 0xA4, Edx 0xA8,
// Ecx 0xAC, Eax 0xB0, Ebp 0xB4, Eip 0xB8, SegCs 0xBC, EFlags 0xC0,
// Esp 0xC4, SegSs 0xC8. ContextFlags: CONTEXT_i386 | CONTROL | INTEGER |
// SEGMENTS (0x10007).
global_asm!(
    r#"
    .section .text
    .code32
    .balign 16
    .global osyx_x86_RtlCaptureContext
osyx_x86_RtlCaptureContext:
    push ebx
    mov ebx, [esp+8]
    mov [ebx+0xB0], eax
    mov [ebx+0xAC], ecx
    mov [ebx+0xA8], edx
    mov eax, [esp]
    mov [ebx+0xA4], eax
    mov [ebx+0xA0], esi
    mov [ebx+0x9C], edi
    xor eax, eax
    mov ax, cs
    mov [ebx+0xBC], eax
    mov ax, ds
    mov [ebx+0x98], eax
    mov ax, es
    mov [ebx+0x94], eax
    mov ax, fs
    mov [ebx+0x90], eax
    mov ax, gs
    mov [ebx+0x8C], eax
    mov ax, ss
    mov [ebx+0xC8], eax
    pushfd
    pop eax
    mov [ebx+0xC0], eax
    mov eax, [ebp+4]
    mov [ebx+0xB8], eax
    mov eax, [ebp]
    mov [ebx+0xB4], eax
    lea eax, [ebp+8]
    mov [ebx+0xC4], eax
    mov dword ptr [ebx], 0x10007
    mov eax, [ebx+0xB0]
    pop ebx
    ret 4

    // USHORT RtlCaptureStackBackTrace(ULONG skip, ULONG count, PVOID *frames, PULONG hash)
    .balign 16
    .global osyx_x86_RtlCaptureStackBackTrace
osyx_x86_RtlCaptureStackBackTrace:
    push ebp
    mov ebp, esp
    push ebx
    push esi
    push edi
    mov esi, [ebp+8]                // frames still to skip
    mov edi, [ebp+16]               // output array
    xor ecx, ecx                    // frames stored
    xor ebx, ebx                    // hash
    mov edx, ebp                    // this frame: [edx+4] returns into the caller
2:
    cmp ecx, [ebp+12]
    jae 4f
    test edx, 3
    jnz 4f
    cmp edx, fs:[8]                 // below StackLimit
    jb 4f
    mov eax, fs:[4]                 // StackBase
    sub eax, 8
    cmp edx, eax
    ja 4f
    mov eax, [edx+4]
    test eax, eax
    jz 4f
    test esi, esi
    jz 3f
    dec esi
    jmp 5f
3:
    test edi, edi
    jz 4f
    mov [edi+ecx*4], eax
    add ebx, eax
    inc ecx
5:
    mov eax, [edx]
    cmp eax, edx                    // the chain must climb the stack
    jbe 4f
    mov edx, eax
    jmp 2b
4:
    mov eax, [ebp+20]
    test eax, eax
    jz 6f
    mov [eax], ebx
6:
    mov eax, ecx
    pop edi
    pop esi
    pop ebx
    pop ebp
    ret 16
    .code64
    "#
);

extern "C" {
    fn osyx_x86_RtlCaptureContext();
    fn osyx_x86_RtlCaptureStackBackTrace();
}

/// The 32-bit routine that implements `symbol`, if it is one of these.
pub(super) fn routine(symbol: &str) -> Option<u32> {
    let f = match symbol {
        "RtlCaptureContext" => osyx_x86_RtlCaptureContext as *const () as usize,
        "RtlCaptureStackBackTrace" => osyx_x86_RtlCaptureStackBackTrace as *const () as usize,
        _ => return None,
    };
    assert!(f < 1usize << 32, "wow64: 32-bit routine is not below 4 GiB");
    Some(f as u32)
}
