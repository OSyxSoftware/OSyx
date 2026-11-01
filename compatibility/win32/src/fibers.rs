//! Fibers: ConvertThreadToFiber(Ex), CreateFiber(Ex), SwitchToFiber,
//! DeleteFiber, ConvertFiberToThread and IsThreadAFiber, for 64-bit
//! programs and for 32-bit ones (WoW64).
//!
//! A fiber is a block whose first field is its parameter (the GetFiberData
//! macro reads it). The current fiber is the TEB's NT_TIB.FiberData
//! (gs:[0x20] in 64-bit code, fs:[0x10] in 32-bit code), which the
//! GetCurrentFiber macro reads. A switch saves the callee-saved registers,
//! the stack pointer, the x87 control word, MXCSR and the TEB's stack bounds
//! (and, for 32-bit code, the head of the SEH chain) into the current fiber
//! and loads them from the target.
//!
//! 64-bit programs switch in `osyx_fiber_switch64`. A 32-bit switch must
//! change the 32-bit stack and registers, which the WoW64 gate keeps, so
//! 32-bit programs switch in 32-bit code: the bridge binds their
//! SwitchToFiber import straight to [`SWITCH32_CODE`] (see
//! `wow64::fiber_code`).
//!
//! Fiber local storage stays per thread (fls.rs): programs that give each
//! fiber its own FLS values see the values of the thread.
#![allow(non_snake_case)]
use crate::{error::*, kernel32::fail, mem, mem::Prot, teb, wow64};
use alloc::vec::Vec;
use core::arch::global_asm;

/// NT_TIB.FiberData of the 64-bit TEB.
const TEB_FIBER_DATA: usize = 0x20;
/// NT_TIB.FiberData of the 32-bit TEB.
const TEB32_FIBER_DATA: u32 = 0x10;
const TEB32_EXCEPTION_LIST: u32 = 0x00;
const TEB32_STACK_BASE: u32 = 0x04;
const TEB32_STACK_LIMIT: u32 = 0x08;
/// NT_TIB.Version shares FiberData: threads that are not fibers hold it.
const NOT_A_FIBER: u64 = 0x1E00;
const ERROR_ALREADY_FIBER: u32 = 1280;
const ERROR_ALREADY_THREAD: u32 = 1281;
/// Stack of a fiber created with size 0 (the default reserve of a Windows image).
const DEFAULT_STACK: usize = 1 << 20;
const MIN_STACK: usize = 64 * 1024;
/// x87 control word and MXCSR a new Windows thread starts with.
const INITIAL_FPUCW: u32 = 0x27F;
const INITIAL_MXCSR: u32 = 0x1F80;

/// A 64-bit fiber. The offsets are used by `osyx_fiber_switch64` and
/// `osyx_fiber_start64`.
#[repr(C)]
struct Fiber64 {
    /// 0: the fiber's parameter (GetFiberData).
    param: u64,
    /// 8: stack pointer while switched out (pointing at the return address).
    rsp: u64,
    /// 16: RBX, RBP, RDI, RSI, R12, R13, R14, R15.
    gpr: [u64; 8],
    /// 80: XMM6 to XMM15.
    xmm: [u64; 20],
    /// 240
    mxcsr: u32,
    /// 244
    fpucw: u32,
    /// 248: the start routine (CreateFiber).
    start: u64,
    /// 256: TEB StackBase while switched out.
    stack_base: u64,
    /// 264: TEB StackLimit while switched out.
    stack_limit: u64,
    /// The stack mapping (0: the thread's own stack).
    stack: u64,
    stack_size: u64,
}

const _: () = assert!(
    core::mem::offset_of!(Fiber64, xmm) == 80
        && core::mem::offset_of!(Fiber64, mxcsr) == 240
        && core::mem::offset_of!(Fiber64, start) == 248
        && core::mem::offset_of!(Fiber64, stack_limit) == 264
);

/// A 32-bit fiber. The offsets are used by the 32-bit code in [`SWITCH32_CODE`].
#[repr(C)]
struct Fiber32 {
    /// 0: the fiber's parameter (GetFiberData).
    param: u32,
    /// 4: ESP while switched out (return address, then the argument).
    esp: u32,
    /// 8
    ebx: u32,
    /// 12
    esi: u32,
    /// 16
    edi: u32,
    /// 20
    ebp: u32,
    /// 24: the start routine (CreateFiber).
    start: u32,
    /// 28: the ExitThread import stub the start code calls when the routine returns.
    exit: u32,
    /// 32: TEB StackBase while switched out.
    stack_base: u32,
    /// 36: TEB StackLimit while switched out.
    stack_limit: u32,
    /// 40: TEB ExceptionList while switched out.
    exception_list: u32,
    /// 44
    fpucw: u32,
    /// 48
    mxcsr: u32,
    _pad: u32,
    /// The stack mapping (0: the thread's own stack).
    stack: u64,
    stack_size: u64,
}

const _: () = assert!(
    core::mem::offset_of!(Fiber32, start) == 24
        && core::mem::offset_of!(Fiber32, exception_list) == 40
        && core::mem::offset_of!(Fiber32, mxcsr) == 48
);

/// 32-bit code, placed by the WoW64 bridge in an executable page of its
/// arena. Offset 0: `VOID WINAPI SwitchToFiber(LPVOID)`. Offset
/// [`START32_OFFSET`]: where a new fiber's first switch returns to; it calls
/// `start(param)`, then ExitThread(0). Source (NASM):
///
/// ```text
/// switch: mov edx, [esp+4]          ; the target fiber
///         mov eax, [fs:0x10]        ; the current fiber
///         test eax, eax
///         jz .load
///         mov [eax+4], esp
///         mov [eax+8], ebx
///         mov [eax+12], esi
///         mov [eax+16], edi
///         mov [eax+20], ebp
///         mov ecx, [fs:0]
///         mov [eax+40], ecx
///         mov ecx, [fs:4]
///         mov [eax+32], ecx
///         mov ecx, [fs:8]
///         mov [eax+36], ecx
///         fnstcw [eax+44]
///         stmxcsr [eax+48]
/// .load:  mov [fs:0x10], edx
///         mov ecx, [edx+40]
///         mov [fs:0], ecx
///         mov ecx, [edx+32]
///         mov [fs:4], ecx
///         mov ecx, [edx+36]
///         mov [fs:8], ecx
///         fldcw [edx+44]
///         ldmxcsr [edx+48]
///         mov ebx, [edx+8]
///         mov esi, [edx+12]
///         mov edi, [edx+16]
///         mov ebp, [edx+20]
///         mov esp, [edx+4]
///         ret 4
///         align 16, db 0xCC
/// start:  mov eax, [fs:0x10]
///         push dword [eax]
///         call [eax+24]
///         mov eax, [fs:0x10]
///         push 0
///         call [eax+28]
///         ud2
/// ```
pub(crate) const SWITCH32_CODE: [u8; 152] = [
    0x8B, 0x54, 0x24, 0x04, 0x64, 0xA1, 0x10, 0x00, 0x00, 0x00, 0x85, 0xC0, 0x74, 0x34, 0x89, 0x60,
    0x04, 0x89, 0x58, 0x08, 0x89, 0x70, 0x0C, 0x89, 0x78, 0x10, 0x89, 0x68, 0x14, 0x64, 0x8B, 0x0D,
    0x00, 0x00, 0x00, 0x00, 0x89, 0x48, 0x28, 0x64, 0x8B, 0x0D, 0x04, 0x00, 0x00, 0x00, 0x89, 0x48,
    0x20, 0x64, 0x8B, 0x0D, 0x08, 0x00, 0x00, 0x00, 0x89, 0x48, 0x24, 0xD9, 0x78, 0x2C, 0x0F, 0xAE,
    0x58, 0x30, 0x64, 0x89, 0x15, 0x10, 0x00, 0x00, 0x00, 0x8B, 0x4A, 0x28, 0x64, 0x89, 0x0D, 0x00,
    0x00, 0x00, 0x00, 0x8B, 0x4A, 0x20, 0x64, 0x89, 0x0D, 0x04, 0x00, 0x00, 0x00, 0x8B, 0x4A, 0x24,
    0x64, 0x89, 0x0D, 0x08, 0x00, 0x00, 0x00, 0xD9, 0x6A, 0x2C, 0x0F, 0xAE, 0x52, 0x30, 0x8B, 0x5A,
    0x08, 0x8B, 0x72, 0x0C, 0x8B, 0x7A, 0x10, 0x8B, 0x6A, 0x14, 0x8B, 0x62, 0x04, 0xC2, 0x04, 0x00,
    0x64, 0xA1, 0x10, 0x00, 0x00, 0x00, 0xFF, 0x30, 0xFF, 0x50, 0x18, 0x64, 0xA1, 0x10, 0x00, 0x00,
    0x00, 0x6A, 0x00, 0xFF, 0x50, 0x1C, 0x0F, 0x0B,
];
/// Offset of the 32-bit fiber start code in [`SWITCH32_CODE`].
pub(crate) const START32_OFFSET: u32 = 0x80;

global_asm!(
    r#"
    .section .text
    .balign 16
    .global osyx_fiber_switch64
osyx_fiber_switch64:
    // Win64: RCX = the target fiber.
    mov rax, gs:[0x30]              // TEB
    mov rdx, [rax + 0x20]           // the current fiber
    test rdx, rdx
    jz 2f
    cmp rdx, 0x1E00
    je 2f
    mov [rdx + 8], rsp
    mov [rdx + 16], rbx
    mov [rdx + 24], rbp
    mov [rdx + 32], rdi
    mov [rdx + 40], rsi
    mov [rdx + 48], r12
    mov [rdx + 56], r13
    mov [rdx + 64], r14
    mov [rdx + 72], r15
    movups [rdx + 80], xmm6
    movups [rdx + 96], xmm7
    movups [rdx + 112], xmm8
    movups [rdx + 128], xmm9
    movups [rdx + 144], xmm10
    movups [rdx + 160], xmm11
    movups [rdx + 176], xmm12
    movups [rdx + 192], xmm13
    movups [rdx + 208], xmm14
    movups [rdx + 224], xmm15
    stmxcsr [rdx + 240]
    fnstcw [rdx + 244]
    mov r8, [rax + 8]
    mov [rdx + 256], r8
    mov r8, [rax + 16]
    mov [rdx + 264], r8
2:
    mov [rax + 0x20], rcx
    mov r8, [rcx + 256]
    mov [rax + 8], r8
    mov r8, [rcx + 264]
    mov [rax + 16], r8
    ldmxcsr [rcx + 240]
    fldcw [rcx + 244]
    movups xmm6, [rcx + 80]
    movups xmm7, [rcx + 96]
    movups xmm8, [rcx + 112]
    movups xmm9, [rcx + 128]
    movups xmm10, [rcx + 144]
    movups xmm11, [rcx + 160]
    movups xmm12, [rcx + 176]
    movups xmm13, [rcx + 192]
    movups xmm14, [rcx + 208]
    movups xmm15, [rcx + 224]
    mov rbx, [rcx + 16]
    mov rbp, [rcx + 24]
    mov rdi, [rcx + 32]
    mov rsi, [rcx + 40]
    mov r12, [rcx + 48]
    mov r13, [rcx + 56]
    mov r14, [rcx + 64]
    mov r15, [rcx + 72]
    mov rsp, [rcx + 8]
    ret

    // A new fiber's first switch returns here, with RSP 8 below a 16-byte
    // boundary as at a function entry.
    .balign 16
    .global osyx_fiber_start64
osyx_fiber_start64:
    mov rax, gs:[0x30]
    mov rax, [rax + 0x20]
    mov rcx, [rax]                  // the parameter
    sub rsp, 40                     // shadow space, 16-byte aligned call
    call [rax + 248]
    xor ecx, ecx
    call {exit_thread}
    ud2
    "#,
    exit_thread = sym crate::kernel32::ExitThread,
);

extern "C" {
    fn osyx_fiber_start64();
}

extern "win64" {
    fn osyx_fiber_switch64(fiber: u64);
}

/// The 32-bit TEB of the calling thread when it runs a 32-bit program.
fn teb32() -> Option<u32> {
    if wow64::is_active() { wow64::current_teb32() } else { None }
}

fn fiber_data() -> u64 {
    match teb32() {
        // SAFETY: the calling thread's TEB32.
        Some(t) => u64::from(unsafe { core::ptr::read_volatile((t + TEB32_FIBER_DATA) as *const u32) }),
        // SAFETY: the calling thread's TEB64.
        None => unsafe { teb::get::<u64>(teb::current(), TEB_FIBER_DATA) },
    }
}

fn set_fiber_data(value: u64) {
    match teb32() {
        // SAFETY: the calling thread's TEB32.
        Some(t) => unsafe { core::ptr::write_volatile((t + TEB32_FIBER_DATA) as *mut u32, value as u32) },
        // SAFETY: the calling thread's TEB64.
        None => unsafe { teb::put(teb::current(), TEB_FIBER_DATA, value) },
    }
}

fn is_fiber() -> bool {
    !matches!(fiber_data(), 0 | NOT_A_FIBER)
}

/// One zeroed page for a fiber block. 32-bit code addresses it, so for a
/// 32-bit program it must lie below 4 GiB (the process's low mappings, like
/// its 32-bit stacks).
fn alloc_block() -> Option<u64> {
    let block = mem::alloc_pages(mem::PAGE, Prot::ReadWrite)?;
    if teb32().is_some() && block >= 1 << 32 {
        mem::free_pages(block, mem::PAGE);
        return None;
    }
    Some(block)
}

/// `ConvertThreadToFiber`: makes the calling thread's current execution a
/// fiber and returns it.
extern "win64" fn ConvertThreadToFiber(param: u64) -> u64 {
    ConvertThreadToFiberEx(param, 0)
}

extern "win64" fn ConvertThreadToFiberEx(param: u64, _flags: u32) -> u64 {
    if is_fiber() {
        return fail(ERROR_ALREADY_FIBER, 0);
    }
    let Some(block) = alloc_block() else { return fail(ERROR_NOT_ENOUGH_MEMORY, 0) };
    // SAFETY: the fresh block; the TEBs of the calling thread.
    unsafe {
        match teb32() {
            Some(t) => {
                let f = block as *mut Fiber32;
                (*f).param = param as u32;
                (*f).fpucw = INITIAL_FPUCW;
                (*f).mxcsr = INITIAL_MXCSR;
                (*f).stack_base = core::ptr::read_volatile((t + TEB32_STACK_BASE) as *const u32);
                (*f).stack_limit = core::ptr::read_volatile((t + TEB32_STACK_LIMIT) as *const u32);
                (*f).exception_list = core::ptr::read_volatile((t + TEB32_EXCEPTION_LIST) as *const u32);
            }
            None => {
                let f = block as *mut Fiber64;
                let t = teb::current();
                (*f).param = param;
                (*f).fpucw = INITIAL_FPUCW;
                (*f).mxcsr = INITIAL_MXCSR;
                (*f).stack_base = teb::get(t, teb::TEB_STACK_BASE);
                (*f).stack_limit = teb::get(t, teb::TEB_STACK_LIMIT);
            }
        }
    }
    set_fiber_data(block);
    block
}

/// `ConvertFiberToThread`: the calling thread stops being a fiber.
extern "win64" fn ConvertFiberToThread() -> i32 {
    if !is_fiber() {
        return fail(ERROR_ALREADY_THREAD, 0);
    }
    let block = fiber_data();
    set_fiber_data(0);
    if !owns_stack(block) {
        mem::free_pages(block, mem::PAGE);
    }
    1
}

/// Whether the fiber has a stack of its own (made by CreateFiber).
fn owns_stack(block: u64) -> bool {
    // SAFETY: a fiber block made here, in the layout of the calling code.
    unsafe {
        if teb32().is_some() { (*(block as *const Fiber32)).stack != 0 } else { (*(block as *const Fiber64)).stack != 0 }
    }
}

extern "win64" fn CreateFiber(stack_size: usize, start: u64, param: u64) -> u64 {
    CreateFiberEx(stack_size, 0, 0, start, param)
}

/// `CreateFiberEx`: a fiber that runs `start(param)` on a stack of its own
/// when first switched to. When the routine returns, the thread exits.
extern "win64" fn CreateFiberEx(commit: usize, reserve: usize, _flags: u32, start: u64, param: u64) -> u64 {
    let size = match commit.max(reserve) {
        0 => DEFAULT_STACK,
        n => n.max(MIN_STACK).next_multiple_of(MIN_STACK),
    };
    let Some(block) = alloc_block() else { return fail(ERROR_NOT_ENOUGH_MEMORY, 0) };
    let Some(stack) = mem::alloc_stack(size) else {
        mem::free_pages(block, mem::PAGE);
        return fail(ERROR_NOT_ENOUGH_MEMORY, 0);
    };
    if teb32().is_some() && stack + size as u64 > 1 << 32 {
        // 32-bit code cannot reach it.
        mem::free_stack(stack, size);
        mem::free_pages(block, mem::PAGE);
        return fail(ERROR_NOT_ENOUGH_MEMORY, 0);
    }
    let top = stack + size as u64;
    // SAFETY: the fresh block and the top of the fresh stack.
    unsafe {
        if teb32().is_some() {
            let Some(code) = wow64::fiber_code() else {
                mem::free_stack(stack, size);
                mem::free_pages(block, mem::PAGE);
                return fail(ERROR_INVALID_PARAMETER, 0);
            };
            let exit = wow64::import_active("ExitThread", crate::kernel32::ExitThread as *const () as u64).unwrap_or(0);
            let f = block as *mut Fiber32;
            (*f).param = param as u32;
            (*f).start = start as u32;
            (*f).exit = exit;
            (*f).stack_base = top as u32;
            (*f).stack_limit = stack as u32;
            (*f).exception_list = u32::MAX; // end of the SEH chain
            (*f).fpucw = INITIAL_FPUCW;
            (*f).mxcsr = INITIAL_MXCSR;
            (*f).stack = stack;
            (*f).stack_size = size as u64;
            // The first switch's `ret 4` pops the start code and the
            // argument slot, leaving ESP at top - 8.
            let esp = top as u32 - 16;
            core::ptr::write((esp as u64) as *mut u32, code + START32_OFFSET);
            (*f).esp = esp;
        } else {
            let f = block as *mut Fiber64;
            (*f).param = param;
            (*f).start = start;
            (*f).stack_base = top;
            (*f).stack_limit = stack;
            (*f).fpucw = INITIAL_FPUCW;
            (*f).mxcsr = INITIAL_MXCSR;
            (*f).stack = stack;
            (*f).stack_size = size as u64;
            // The first switch's `ret` pops the start code, leaving RSP at
            // top - 8, as after a call.
            let rsp = top - 16;
            core::ptr::write(rsp as *mut u64, osyx_fiber_start64 as *const () as u64);
            (*f).rsp = rsp;
        }
    }
    block
}

/// `DeleteFiber`: frees a fiber and its stack. Deleting the running fiber
/// ends the thread, as on Windows.
extern "win64" fn DeleteFiber(fiber: u64) {
    if fiber == 0 {
        return;
    }
    if fiber == fiber_data() {
        crate::kernel32::ExitThread(0);
    }
    // SAFETY: a fiber block made by CreateFiberEx or ConvertThreadToFiberEx.
    let (stack, size) = unsafe {
        if teb32().is_some() {
            let f = fiber as *const Fiber32;
            ((*f).stack, (*f).stack_size)
        } else {
            let f = fiber as *const Fiber64;
            ((*f).stack, (*f).stack_size)
        }
    };
    if stack != 0 {
        mem::free_stack(stack, size as usize);
    }
    mem::free_pages(fiber, mem::PAGE);
}

/// `SwitchToFiber` for 64-bit callers (32-bit callers are bound to
/// [`SWITCH32_CODE`] by the bridge and never reach the host).
extern "win64" fn SwitchToFiber(fiber: u64) {
    if fiber == 0 {
        return;
    }
    // SAFETY: `fiber` is a fiber block made here; the switch saves the
    // caller's state into the current fiber and resumes the target.
    unsafe { osyx_fiber_switch64(fiber) }
}

extern "win64" fn IsThreadAFiber() -> i32 {
    is_fiber() as i32
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        ConvertThreadToFiber,
        ConvertThreadToFiberEx,
        ConvertFiberToThread,
        CreateFiber,
        CreateFiberEx,
        DeleteFiber,
        SwitchToFiber,
        IsThreadAFiber,
    ]
}
