//! x64 table-based unwinding: function tables of the loaded PE images
//! (their `.pdata`) and of `RtlAddFunctionTable`, `RtlVirtualUnwind` over
//! UNWIND_INFO codes and epilogues, context capture and stack back traces.
//! Built-in DLLs are winrun's own code and have no tables: a walk stops
//! at the first frame outside a PE image. Frame-based exception dispatch
//! (`RtlUnwindEx`) is not implemented here yet.
#![allow(non_snake_case)]
use crate::{kernel32::fail, loader, objects};
use crate::error::*;
use alloc::vec::Vec;
use core::arch::global_asm;
use os_std::sync::Mutex;

// CONTEXT offsets (winnt.h, AMD64).
const CTX_FLAGS: usize = 0x30;
const CTX_RSP: usize = 0x98;
const CTX_REGS: usize = 0x78;
const CTX_RIP: usize = 0xf8;
const CTX_XMM: usize = 0x1a0;
const RSP: u8 = 4;

const UNW_FLAG_CHAININFO: u8 = 4;
const UWOP_PUSH_NONVOL: u8 = 0;
const UWOP_ALLOC_LARGE: u8 = 1;
const UWOP_ALLOC_SMALL: u8 = 2;
const UWOP_SET_FPREG: u8 = 3;
const UWOP_SAVE_NONVOL: u8 = 4;
const UWOP_SAVE_NONVOL_FAR: u8 = 5;
const UWOP_EPILOG: u8 = 6;
const UWOP_SAVE_XMM128: u8 = 8;
const UWOP_SAVE_XMM128_FAR: u8 = 9;
const UWOP_PUSH_MACHFRAME: u8 = 10;

/// RUNTIME_FUNCTION: RVAs of the code range and its UNWIND_INFO.
#[repr(C)]
#[derive(Clone, Copy)]
struct Function {
    begin: u32,
    end: u32,
    unwind: u32,
}

struct Dynamic {
    table: u64,
    count: u32,
    base: u64,
    low: u64,
    high: u64,
}

static DYNAMIC: Mutex<Vec<Dynamic>> = Mutex::new(Vec::new());

fn rd<T: Copy>(addr: u64) -> T {
    // SAFETY: addresses inside mapped images or the stack being unwound.
    unsafe { core::ptr::read_unaligned(addr as *const T) }
}

fn reg(ctx: u64, r: u8) -> u64 {
    rd(ctx + (CTX_REGS + r as usize * 8) as u64)
}

fn set_reg(ctx: u64, ptrs: u64, r: u8, value_at: u64) {
    // SAFETY: the caller's CONTEXT and optional KNONVOLATILE_CONTEXT_POINTERS.
    unsafe {
        *((ctx + (CTX_REGS + r as usize * 8) as u64) as *mut u64) = rd(value_at);
        if ptrs != 0 {
            *((ptrs + 128 + r as u64 * 8) as *mut u64) = value_at;
        }
    }
}

fn set_xmm(ctx: u64, ptrs: u64, r: u8, value_at: u64) {
    // SAFETY: as above.
    unsafe {
        *((ctx + (CTX_XMM + r as usize * 16) as u64) as *mut u128) = rd(value_at);
        if ptrs != 0 {
            *((ptrs + r as u64 * 8) as *mut u64) = value_at;
        }
    }
}

/// The exception directory of the PE image mapped at `base`.
fn image_table(base: u64) -> Option<(u64, u32)> {
    if rd::<u16>(base) != 0x5a4d {
        return None;
    }
    let nt = base + rd::<u32>(base + 0x3c) as u64;
    if rd::<u32>(nt) != 0x4550 || rd::<u16>(nt + 24) != 0x20b || rd::<u32>(nt + 24 + 108) <= 3 {
        return None;
    }
    let dir = nt + 24 + 112 + 3 * 8;
    let (rva, size) = (rd::<u32>(dir), rd::<u32>(dir + 4));
    (rva != 0 && size >= 12).then_some((base + rva as u64, size / 12))
}

fn search(table: u64, count: u32, base: u64, pc: u64) -> Option<u64> {
    let rva = pc.checked_sub(base)?;
    let (mut lo, mut hi) = (0u32, count);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let e = table + mid as u64 * 12;
        let f: Function = rd(e);
        if rva < f.begin as u64 {
            hi = mid;
        } else if rva >= f.end as u64 {
            lo = mid + 1;
        } else {
            return Some(e);
        }
    }
    None
}

/// RUNTIME_FUNCTION covering `pc` and its image base.
pub fn lookup(pc: u64) -> Option<(u64, u64)> {
    if let Some((base, _)) = loader::image_containing(pc) {
        let (table, count) = image_table(base)?;
        let mut e = search(table, count, base, pc)?;
        // An entry whose UnwindData has bit 0 set refers to another entry.
        let f: Function = rd(e);
        if f.unwind & 1 != 0 {
            e = base + (f.unwind & !1) as u64;
        }
        return Some((e, base));
    }
    let d = DYNAMIC.lock().unwrap();
    d.iter().filter(|t| pc >= t.low && pc < t.high).find_map(|t| search(t.table, t.count, t.base, pc).map(|e| (e, t.base)))
}

extern "win64" fn RtlLookupFunctionEntry(pc: u64, base: *mut u64, _history: u64) -> u64 {
    match lookup(pc) {
        Some((e, b)) => {
            if !base.is_null() {
                // SAFETY: the caller's out pointer.
                unsafe { *base = b };
            }
            e
        }
        None => 0,
    }
}

extern "win64" fn RtlAddFunctionTable(table: u64, count: u32, base: u64) -> u8 {
    if table == 0 || count == 0 {
        return 0;
    }
    let (mut low, mut high) = (u64::MAX, 0);
    for i in 0..count as u64 {
        let f: Function = rd(table + i * 12);
        low = low.min(base + f.begin as u64);
        high = high.max(base + f.end as u64);
    }
    DYNAMIC.lock().unwrap().push(Dynamic { table, count, base, low, high });
    1
}

/// Registers a function table of winrun's own code (see seh.rs).
pub fn add_table(table: u64, count: u32, base: u64) {
    RtlAddFunctionTable(table, count, base);
}

extern "win64" fn RtlDeleteFunctionTable(table: u64) -> u8 {
    let mut d = DYNAMIC.lock().unwrap();
    match d.iter().position(|t| t.table == table) {
        Some(i) => {
            d.remove(i);
            1
        }
        None => 0,
    }
}

/// Base of the PE image containing `pc` (built-ins are not images).
extern "win64" fn RtlPcToFileHeader(pc: u64, base: *mut u64) -> u64 {
    let b = loader::image_containing(pc).map_or(0, |(b, _)| b);
    if !base.is_null() {
        // SAFETY: the caller's out pointer.
        unsafe { *base = b };
    }
    b
}

/// Whether the instructions at `pc` are an epilogue: an optional
/// `add rsp, imm` / `lea rsp, [reg+disp]`, `pop` of nonvolatiles, then
/// `ret` or a jump out of the function.
fn epilog_length(pc: u64, begin: u64, end: u64) -> Option<()> {
    let mut p = pc;
    let b0 = rd::<u8>(p);
    // add rsp, imm8 / imm32
    if b0 == 0x48 && rd::<u8>(p + 1) == 0x83 && rd::<u8>(p + 2) == 0xc4 {
        p += 4;
    } else if b0 == 0x48 && rd::<u8>(p + 1) == 0x81 && rd::<u8>(p + 2) == 0xc4 {
        p += 7;
    } else if (b0 & 0xf8) == 0x48 && rd::<u8>(p + 1) == 0x8d {
        // lea rsp, [base + disp8/disp32]
        let modrm = rd::<u8>(p + 2);
        if (modrm >> 3) & 7 != 4 || b0 & 4 != 0 {
            return None;
        }
        let mut len = 3;
        if modrm & 7 == 4 {
            len += 1; // SIB
        }
        match modrm >> 6 {
            1 => len += 1,
            2 => len += 4,
            _ => return None,
        }
        p += len;
    }
    loop {
        let b = rd::<u8>(p);
        if (0x58..=0x5f).contains(&b) {
            p += 1;
        } else if b == 0x41 && (0x58..=0x5f).contains(&rd::<u8>(p + 1)) {
            p += 2;
        } else {
            break;
        }
    }
    match rd::<u8>(p) {
        0xc3 | 0xc2 => Some(()),
        0xf3 if rd::<u8>(p + 1) == 0xc3 => Some(()),
        // Tail call: jmp rel8/rel32 out of the function, or jmp [rip+x].
        0xeb => {
            let t = (p + 2).wrapping_add(rd::<i8>(p + 1) as i64 as u64);
            (t < begin || t >= end).then_some(())
        }
        0xe9 => {
            let t = (p + 5).wrapping_add(rd::<i32>(p + 1) as i64 as u64);
            (t < begin || t >= end).then_some(())
        }
        0xff if rd::<u8>(p + 1) == 0x25 => Some(()),
        0x48 if rd::<u8>(p + 1) == 0xff && rd::<u8>(p + 2) == 0x25 => Some(()),
        _ => None,
    }
}

/// Executes the epilogue at the context's RIP.
fn run_epilog(ctx: u64, ptrs: u64) {
    let mut p: u64 = rd(ctx + CTX_RIP as u64);
    let mut rsp = reg(ctx, RSP);
    let b0 = rd::<u8>(p);
    if b0 == 0x48 && rd::<u8>(p + 1) == 0x83 {
        rsp = rsp.wrapping_add(rd::<i8>(p + 3) as i64 as u64);
        p += 4;
    } else if b0 == 0x48 && rd::<u8>(p + 1) == 0x81 {
        rsp = rsp.wrapping_add(rd::<i32>(p + 3) as i64 as u64);
        p += 7;
    } else if (b0 & 0xf8) == 0x48 && rd::<u8>(p + 1) == 0x8d {
        let modrm = rd::<u8>(p + 2);
        let base_reg = (modrm & 7) | if b0 & 1 != 0 { 8 } else { 0 };
        let mut q = p + 3;
        let base_reg = if modrm & 7 == 4 {
            let sib = rd::<u8>(q);
            q += 1;
            (sib & 7) | if b0 & 1 != 0 { 8 } else { 0 }
        } else {
            base_reg
        };
        let disp = if modrm >> 6 == 1 {
            let d = rd::<i8>(q) as i64;
            q += 1;
            d
        } else {
            let d = rd::<i32>(q) as i64;
            q += 4;
            d
        };
        rsp = reg(ctx, base_reg).wrapping_add(disp as u64);
        p = q;
    }
    loop {
        let b = rd::<u8>(p);
        let r = if (0x58..=0x5f).contains(&b) {
            p += 1;
            b - 0x58
        } else if b == 0x41 && (0x58..=0x5f).contains(&rd::<u8>(p + 1)) {
            p += 2;
            rd::<u8>(p - 1) - 0x58 + 8
        } else {
            break;
        };
        set_reg(ctx, ptrs, r, rsp);
        rsp += 8;
    }
    // SAFETY: the caller's CONTEXT.
    unsafe {
        *((ctx + CTX_RIP as u64) as *mut u64) = rd(rsp);
        *((ctx + CTX_RSP as u64) as *mut u64) = rsp + 8;
    }
}

/// Size in slots of the unwind code at `i`.
fn code_slots(op: u8, info: u8) -> usize {
    match op {
        UWOP_ALLOC_LARGE => if info == 0 { 2 } else { 3 },
        UWOP_SAVE_NONVOL | UWOP_SAVE_XMM128 | UWOP_EPILOG => 2,
        UWOP_SAVE_NONVOL_FAR | UWOP_SAVE_XMM128_FAR => 3,
        _ => 1,
    }
}

/// Unwinds one frame of `ctx`. Returns the language handler (0 if none
/// applies) and sets the handler data and establisher frame.
#[allow(clippy::too_many_arguments)]
pub fn virtual_unwind(kind: u32, base: u64, pc: u64, function: u64, ctx: u64, data: *mut u64, frame_out: *mut u64, ptrs: u64) -> u64 {
    let mut f: Function = rd(function);
    let mut info = base + f.unwind as u64;
    let mut offset = pc.wrapping_sub(base + f.begin as u64);
    let header: u8 = rd(info);
    let prolog: u8 = rd(info + 1);
    let fp: u8 = rd(info + 3);
    let frame_reg = fp & 15;
    let frame = if frame_reg != 0 { reg(ctx, frame_reg).wrapping_sub((fp >> 4) as u64 * 16) } else { reg(ctx, RSP) };
    // SAFETY: the caller's out pointers.
    let set_frame = |v: u64| unsafe {
        if !frame_out.is_null() {
            *frame_out = v;
        }
    };
    if !matches!(header & 7, 1 | 2) {
        set_frame(frame);
        return 0;
    }
    if offset > prolog as u64 && epilog_length(pc, base + f.begin as u64, base + f.end as u64).is_some() {
        run_epilog(ctx, ptrs);
        set_frame(frame);
        return 0;
    }
    let in_prolog = offset < prolog as u64;
    let mut machine_frame = false;
    loop {
        let count: u8 = rd(info + 2);
        let fp: u8 = rd(info + 3);
        let codes = info + 4;
        let mut i = 0usize;
        while i < count as usize {
            let code_offset: u8 = rd(codes + i as u64 * 2);
            let op_info: u8 = rd(codes + i as u64 * 2 + 1);
            let (op, oi) = (op_info & 15, op_info >> 4);
            let slots = code_slots(op, oi);
            if (code_offset as u64) > offset {
                i += slots;
                continue;
            }
            let rsp = reg(ctx, RSP);
            // SAFETY: the caller's CONTEXT.
            let put_rsp = |v: u64| unsafe { *((ctx + CTX_RSP as u64) as *mut u64) = v };
            let slot16 = |n: usize| rd::<u16>(codes + (i + n) as u64 * 2) as u64;
            let slot32 = |n: usize| rd::<u32>(codes + (i + n) as u64 * 2) as u64;
            match op {
                UWOP_PUSH_NONVOL => {
                    set_reg(ctx, ptrs, oi, rsp);
                    put_rsp(rsp + 8);
                }
                UWOP_ALLOC_LARGE => put_rsp(rsp + if oi == 0 { slot16(1) * 8 } else { slot32(1) }),
                UWOP_ALLOC_SMALL => put_rsp(rsp + (oi as u64 + 1) * 8),
                UWOP_SET_FPREG => put_rsp(reg(ctx, fp & 15).wrapping_sub((fp >> 4) as u64 * 16)),
                UWOP_SAVE_NONVOL => set_reg(ctx, ptrs, oi, frame + slot16(1) * 8),
                UWOP_SAVE_NONVOL_FAR => set_reg(ctx, ptrs, oi, frame + slot32(1)),
                UWOP_SAVE_XMM128 => set_xmm(ctx, ptrs, oi, frame + slot16(1) * 16),
                UWOP_SAVE_XMM128_FAR => set_xmm(ctx, ptrs, oi, frame + slot32(1)),
                UWOP_EPILOG => {}
                UWOP_PUSH_MACHFRAME => {
                    let at = if oi != 0 { rsp + 8 } else { rsp };
                    // SAFETY: the caller's CONTEXT.
                    unsafe { *((ctx + CTX_RIP as u64) as *mut u64) = rd(at) };
                    put_rsp(rd(at + 24));
                    machine_frame = true;
                }
                _ => {
                    set_frame(frame);
                    return 0;
                }
            }
            i += slots;
        }
        let header: u8 = rd(info);
        if header >> 3 & UNW_FLAG_CHAININFO == 0 {
            break;
        }
        // The chained entry follows the codes (padded to an even count).
        let chained = codes + ((count as u64 + 1) & !1) * 2;
        f = rd(chained);
        info = base + f.unwind as u64;
        offset = u64::MAX;
    }
    if !machine_frame {
        let rsp = reg(ctx, RSP);
        // SAFETY: the caller's CONTEXT.
        unsafe {
            *((ctx + CTX_RIP as u64) as *mut u64) = rd(rsp);
            *((ctx + CTX_RSP as u64) as *mut u64) = rsp + 8;
        }
    }
    set_frame(frame);
    let header: u8 = rd(info);
    if in_prolog || (header >> 3) as u32 & kind == 0 {
        return 0;
    }
    let count: u8 = rd(info + 2);
    let handler = info + 4 + ((count as u64 + 1) & !1) * 2;
    if !data.is_null() {
        // SAFETY: the caller's out pointer.
        unsafe { *data = handler + 4 };
    }
    base + rd::<u32>(handler) as u64
}

#[allow(clippy::too_many_arguments)]
extern "win64" fn RtlVirtualUnwind(
    kind: u32,
    base: u64,
    pc: u64,
    function: u64,
    ctx: u64,
    data: *mut u64,
    frame: *mut u64,
    ptrs: u64,
) -> u64 {
    virtual_unwind(kind, base, pc, function, ctx, data, frame, ptrs)
}

global_asm!(
    r#"
.section .text
.global osyx_RtlCaptureContext
osyx_RtlCaptureContext:
    mov [rcx+0x78], rax
    pushfq
    pop rax
    mov [rcx+0x44], eax
    mov word ptr [rcx+0x38], cs
    mov word ptr [rcx+0x3a], ds
    mov word ptr [rcx+0x3c], es
    mov word ptr [rcx+0x3e], fs
    mov word ptr [rcx+0x40], gs
    mov word ptr [rcx+0x42], ss
    mov [rcx+0x80], rcx
    mov [rcx+0x88], rdx
    mov [rcx+0x90], rbx
    mov [rcx+0xa0], rbp
    mov [rcx+0xa8], rsi
    mov [rcx+0xb0], rdi
    mov [rcx+0xb8], r8
    mov [rcx+0xc0], r9
    mov [rcx+0xc8], r10
    mov [rcx+0xd0], r11
    mov [rcx+0xd8], r12
    mov [rcx+0xe0], r13
    mov [rcx+0xe8], r14
    mov [rcx+0xf0], r15
    lea rax, [rsp+8]
    mov [rcx+0x98], rax
    mov rax, [rsp]
    mov [rcx+0xf8], rax
    fxsave64 [rcx+0x100]
    stmxcsr [rcx+0x34]
    mov dword ptr [rcx+0x30], 0x10000f
    mov rax, [rcx+0x78]
    ret

.global osyx_RtlCaptureStackBackTrace
osyx_RtlCaptureStackBackTrace:
    sub rsp, 1288
    mov [rsp+48+0x90], rbx
    mov [rsp+48+0xa0], rbp
    mov [rsp+48+0xa8], rsi
    mov [rsp+48+0xb0], rdi
    mov [rsp+48+0xd8], r12
    mov [rsp+48+0xe0], r13
    mov [rsp+48+0xe8], r14
    mov [rsp+48+0xf0], r15
    lea rax, [rsp+1296]
    mov [rsp+48+0x98], rax
    mov rax, [rsp+1288]
    mov [rsp+48+0xf8], rax
    lea rax, [rsp+48]
    mov [rsp+32], rax
    call osyx_backtrace
    add rsp, 1288
    ret
"#
);

extern "win64" {
    #[link_name = "osyx_RtlCaptureContext"]
    fn RtlCaptureContext(ctx: u64);
    #[link_name = "osyx_RtlCaptureStackBackTrace"]
    fn RtlCaptureStackBackTrace(skip: u32, count: u32, frames: *mut u64, hash: *mut u32) -> u16;
}

/// Walks the caller's stack (`ctx` holds its nonvolatile registers, RSP
/// and return address) through PE images.
#[no_mangle]
extern "win64" fn osyx_backtrace(skip: u32, count: u32, frames: *mut u64, hash: *mut u32, ctx: u64) -> u16 {
    let mut n = 0u32;
    let mut index = 0u32;
    let mut sum = 0u32;
    while n < count && index < 1024 {
        let pc: u64 = rd(ctx + CTX_RIP as u64);
        if pc == 0 || loader::image_containing(pc).is_none() {
            break;
        }
        if index >= skip {
            // SAFETY: the caller's array of `count` entries.
            unsafe { *frames.add(n as usize) = pc };
            sum = sum.wrapping_add(pc as u32);
            n += 1;
        }
        index += 1;
        let rsp_before = reg(ctx, RSP);
        match lookup(pc) {
            Some((f, base)) => {
                let mut data = 0;
                let mut frame = 0;
                virtual_unwind(0, base, pc, f, ctx, &mut data, &mut frame, 0);
            }
            None => {
                // A leaf function: the return address is at RSP.
                // SAFETY: the CONTEXT built by the entry thunk.
                unsafe {
                    *((ctx + CTX_RIP as u64) as *mut u64) = rd(rsp_before);
                    *((ctx + CTX_RSP as u64) as *mut u64) = rsp_before + 8;
                }
            }
        }
        if reg(ctx, RSP) <= rsp_before {
            break; // the stack must move up
        }
    }
    if !hash.is_null() {
        // SAFETY: the caller's out pointer.
        unsafe { *hash = sum };
    }
    n as u16
}

/// Only the calling thread's own context can be read: OSyx cannot stop a
/// running thread and read its registers.
extern "win64" fn GetThreadContext(thread: u64, ctx: u64) -> i32 {
    let own = thread == objects::CURRENT_THREAD
        || matches!(objects::get(thread), Some(objects::Object::Thread(t)) if t.tid.load(core::sync::atomic::Ordering::SeqCst) as u64 == os_sys::gettid());
    if ctx == 0 || !ctx.is_multiple_of(16) {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    if !own {
        return fail(ERROR_NOT_SUPPORTED, 0);
    }
    let wanted: u32 = rd(ctx + CTX_FLAGS as u64);
    // SAFETY: the caller's 16-byte aligned CONTEXT.
    unsafe { RtlCaptureContext(ctx) };
    // SAFETY: as above.
    unsafe { *((ctx + CTX_FLAGS as u64) as *mut u32) = wanted & 0x10000f | 0x100000 };
    1
}

pub fn ntdll_exports() -> Vec<(&'static str, u64)> {
    alloc::vec![
        ("RtlLookupFunctionEntry", RtlLookupFunctionEntry as *const () as u64),
        ("RtlVirtualUnwind", RtlVirtualUnwind as *const () as u64),
        ("RtlAddFunctionTable", RtlAddFunctionTable as *const () as u64),
        ("RtlDeleteFunctionTable", RtlDeleteFunctionTable as *const () as u64),
        ("RtlPcToFileHeader", RtlPcToFileHeader as *const () as u64),
        ("RtlCaptureContext", RtlCaptureContext as *const () as u64),
        ("RtlCaptureStackBackTrace", RtlCaptureStackBackTrace as *const () as u64),
    ]
}

pub fn kernel32_exports() -> Vec<(&'static str, u64)> {
    let mut e = ntdll_exports();
    e.push(("GetThreadContext", GetThreadContext as *const () as u64));
    e
}
