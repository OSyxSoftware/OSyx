//! Frame-based structured exception handling over the x64 unwinder:
//! the search for a language handler (`__C_specific_handler`,
//! `__CxxFrameHandler4`, ...) after the vectored handlers, `RtlUnwindEx`
//! running termination handlers up to a target frame, and
//! `RtlRestoreContext`, including STATUS_UNWIND_CONSOLIDATE (how
//! vcruntime enters a C++ catch block).
//!
//! An unwind started while an exception is being dispatched begins at the
//! context of the exception (the frames below it are the dispatcher's and
//! have no handlers) and ends the kernel's fault delivery with
//! `fault_return`. Collided unwinds (a termination handler starting
//! another unwind) and exit unwinds (no target frame) are not supported
//! and terminate the process with a message.
#![allow(non_snake_case)]
use crate::exceptions::{apply_all, Context, Record};
use crate::{loader, teb, unwind};
use alloc::{boxed::Box, collections::BTreeMap};
use core::arch::global_asm;
use os_std::sync::Mutex;
use osyx_abi::fault::Context as NativeContext;

const EH_NONCONTINUABLE: u32 = 0x1;
const EH_UNWINDING: u32 = 0x2;
const EH_EXIT_UNWIND: u32 = 0x4;
const EH_TARGET_UNWIND: u32 = 0x20;
const STATUS_UNWIND: u32 = 0xc000_0027;
const STATUS_UNWIND_CONSOLIDATE: u32 = 0x8000_0029;
const STATUS_INVALID_DISPOSITION: u32 = 0xc000_0026;
const STATUS_NONCONTINUABLE_EXCEPTION: u32 = 0xc000_0025;
const STATUS_INVALID_UNWIND_TARGET: u32 = 0xc000_0029;
const UNW_FLAG_EHANDLER: u32 = 1;
const UNW_FLAG_UHANDLER: u32 = 2;
const RSP: usize = 4;

/// DISPATCHER_CONTEXT (x64).
#[repr(C)]
struct Dispatcher {
    control_pc: u64,
    image_base: u64,
    function: u64,
    frame: u64,
    target_ip: u64,
    context: *mut Context,
    handler: u64,
    data: u64,
    history: u64,
    scope: u32,
    fill: u32,
}

type LanguageHandler = extern "win64" fn(*mut Record, u64, *mut Context, *mut Dispatcher) -> u32;

/// The exception being dispatched on each thread: the kernel's saved
/// frame and the Windows context of the exception.
struct Outstanding {
    native: usize,
    original: Box<Context>,
}

static OUTSTANDING: Mutex<BTreeMap<u64, Outstanding>> = Mutex::new(BTreeMap::new());

pub fn begin(native: *mut NativeContext, context: &Context) {
    OUTSTANDING.lock().unwrap().insert(os_sys::gettid(), Outstanding { native: native as usize, original: Box::new(*context) });
}

pub fn end() {
    OUTSTANDING.lock().unwrap().remove(&os_sys::gettid());
}

fn die(why: &str, status: u32) -> ! {
    os_std::eprintln!("winrun: {why}");
    os_std::process::exit(status as i32)
}

/// [StackLimit, StackBase) of the calling thread, from its TEB.
fn stack_bounds() -> (u64, u64) {
    let t = teb::current();
    // SAFETY: the thread's TEB.
    unsafe { (teb::get::<u64>(t, teb::TEB_STACK_LIMIT), teb::get::<u64>(t, teb::TEB_STACK_BASE)) }
}

fn read(addr: u64) -> u64 {
    // SAFETY: a stack slot inside the thread's stack bounds.
    unsafe { core::ptr::read_unaligned(addr as *const u64) }
}

/// One frame of `ctx` → (handler, establisher frame, handler data,
/// function entry, image base, caller's context).
fn step(kind: u32, ctx: &Context) -> Option<(u64, u64, u64, u64, u64, Context)> {
    let pc = ctx.rip;
    let mut next = *ctx;
    match unwind::lookup(pc) {
        Some((function, base)) => {
            let (mut data, mut frame) = (0u64, 0u64);
            let handler =
                unwind::virtual_unwind(kind, base, pc, function, &mut next as *mut Context as u64, &mut data, &mut frame, 0);
            Some((handler, frame, data, function, base, next))
        }
        // A leaf function of a PE image: its return address is at RSP.
        None if loader::image_containing(pc).is_some() => {
            let frame = ctx.regs[RSP];
            next.rip = read(frame);
            next.regs[RSP] = frame + 8;
            Some((0, frame, 0, 0, 0, next))
        }
        None => None,
    }
}

/// Searches the frames of `original` for a handler that accepts the
/// exception. True: resume `original` (which the handler may have edited).
pub fn dispatch(record: &mut Record, original: &mut Context) -> bool {
    let (low, high) = stack_bounds();
    let mut ctx = *original;
    while let Some((handler, frame, data, function, base, next)) = step(UNW_FLAG_EHANDLER, &ctx) {
        if frame < low || frame >= high || frame & 7 != 0 {
            return false;
        }
        if handler != 0 {
            let mut d = Dispatcher {
                control_pc: ctx.rip,
                image_base: base,
                function,
                frame,
                target_ip: 0,
                context: &mut ctx,
                handler,
                data,
                history: 0,
                scope: 0,
                fill: 0,
            };
            // SAFETY: the language handler named by the image's unwind data.
            let h: LanguageHandler = unsafe { core::mem::transmute(handler as usize) };
            match h(record, frame, original, &mut d) {
                0 => {
                    if record.flags & EH_NONCONTINUABLE != 0 {
                        die("handler continued a noncontinuable exception", STATUS_NONCONTINUABLE_EXCEPTION);
                    }
                    return true;
                }
                1 | 2 => {} // continue search; a nested exception cannot reach here
                _ => die("exception handler returned an invalid disposition", STATUS_INVALID_DISPOSITION),
            }
        }
        if next.regs[RSP] <= ctx.regs[RSP] {
            return false;
        }
        ctx = next;
    }
    false
}

/// Unwinds from `start` to `target_frame`, running termination handlers,
/// then continues at `target_ip` with RAX = `retval`.
fn unwind_to(target_frame: u64, target_ip: u64, record: *mut Record, retval: u64, start: Context) -> ! {
    let mut local = Record { code: STATUS_UNWIND, flags: 0, nested: 0, address: start.rip, count: 0, reserved: 0, info: [0; 15] };
    // SAFETY: the caller's EXCEPTION_RECORD, or our own.
    let rec = if record.is_null() { &mut local } else { unsafe { &mut *record } };
    if target_frame == 0 {
        die("exit unwind (RtlUnwindEx without a target frame) is not supported", STATUS_INVALID_UNWIND_TARGET);
    }
    rec.flags |= EH_UNWINDING;
    rec.flags &= !EH_EXIT_UNWIND;
    let (low, high) = stack_bounds();
    let mut ctx = start;
    loop {
        let Some((handler, frame, data, function, base, next)) = step(UNW_FLAG_UHANDLER, &ctx) else {
            die("unwind left the Windows code before its target frame", STATUS_INVALID_UNWIND_TARGET);
        };
        if frame < low || frame >= high || frame > target_frame {
            die("unwind target frame is not on the stack", STATUS_INVALID_UNWIND_TARGET);
        }
        if frame == target_frame {
            rec.flags |= EH_TARGET_UNWIND;
        }
        if handler != 0 {
            let mut d = Dispatcher {
                control_pc: ctx.rip,
                image_base: base,
                function,
                frame,
                target_ip,
                context: &mut ctx,
                handler,
                data,
                history: 0,
                scope: 0,
                fill: 0,
            };
            // SAFETY: as in `dispatch`.
            let h: LanguageHandler = unsafe { core::mem::transmute(handler as usize) };
            match h(rec, frame, &mut ctx, &mut d) {
                1 => {}
                3 => die("collided unwind is not supported", STATUS_INVALID_DISPOSITION),
                _ => die("termination handler returned an invalid disposition", STATUS_INVALID_DISPOSITION),
            }
        }
        if frame == target_frame {
            break;
        }
        ctx = next;
    }
    ctx.regs[0] = retval;
    ctx.rip = target_ip;
    restore(ctx, rec)
}

/// Continues at `ctx`: through the kernel when this thread's exception
/// delivery is still outstanding, otherwise directly.
fn restore_raw(ctx: Context) -> ! {
    let pending = OUTSTANDING.lock().unwrap().remove(&os_sys::gettid());
    if let Some(o) = pending {
        // SAFETY: the kernel's saved frame of this thread's delivery.
        let n = unsafe { &mut *(o.native as *mut NativeContext) };
        drop(o.original);
        apply_all(&ctx, n);
        // SAFETY: a validated continuation of the faulting thread.
        let e = unsafe { os_sys::fault_return(n) };
        die(&alloc::format!("continuation rejected: {e:?}"), STATUS_INVALID_UNWIND_TARGET);
    }
    // SAFETY: a complete user context of this thread.
    unsafe { osyx_restore_context(&ctx) }
}

fn restore(ctx: Context, rec: &Record) -> ! {
    if rec.code == STATUS_UNWIND_CONSOLIDATE {
        if rec.count < 1 || rec.info[0] == 0 {
            die("consolidate unwind without a callback", STATUS_INVALID_DISPOSITION);
        }
        register_thunk();
        // Continue in osyx_consolidate_setup on the target frame's stack;
        // the dispatcher's stack (and this frame) is abandoned.
        let data = Box::into_raw(Box::new((ctx, *rec)));
        let mut t = ctx;
        t.regs[RSP] = ctx.regs[RSP] & !15;
        t.rip = osyx_consolidate_setup as *const () as u64;
        t.regs[1] = data as u64;
        restore_raw(t)
    }
    restore_raw(ctx)
}

// ---- consolidation thunk ------------------------------------------------------
//
// Frame at S (16-aligned, below the target RSP):
//   S+0x20  rbx rbp rsi rdi r12 r13 r14 r15   (target values)
//   S+0x60  xmm6..xmm15
//   S+0x100 machine frame: RIP, CS, EFLAGS, RSP, SS of the target
//   S+0x128 the context copy (for the continuation)
// The unwind info below restores exactly these, so a throw from inside
// the catch block unwinds back into the target frame.

global_asm!(
    r#"
.section .text
.global osyx_consolidate_setup
osyx_consolidate_setup:
    mov rdx, rsp
    sub rsp, 0x820
    call osyx_consolidate_fill
    mov rsp, rax
    mov rcx, [rsp+0x128]
    mov rbx, [rcx+0x90]
    mov rbp, [rcx+0xa0]
    mov rsi, [rcx+0xa8]
    mov rdi, [rcx+0xb0]
    mov r12, [rcx+0xd8]
    mov r13, [rcx+0xe0]
    mov r14, [rcx+0xe8]
    mov r15, [rcx+0xf0]
    mov rcx, [rsp+0x130]
    mov r8, [rcx+0x20]
    jmp osyx_consolidate_begin

.balign 16
.global osyx_consolidate_begin
osyx_consolidate_begin:
    call r8
    mov rcx, [rsp+0x128]
    mov [rcx+0xf8], rax
    call osyx_consolidate_done
    ud2
.global osyx_consolidate_end
osyx_consolidate_end:

.balign 4
.global osyx_consolidate_unwind
osyx_consolidate_unwind:
    .byte 1, 0, 39, 0
    .byte 0, 0x34
    .short 4
    .byte 0, 0x54
    .short 5
    .byte 0, 0x64
    .short 6
    .byte 0, 0x74
    .short 7
    .byte 0, 0xc4
    .short 8
    .byte 0, 0xd4
    .short 9
    .byte 0, 0xe4
    .short 10
    .byte 0, 0xf4
    .short 11
    .byte 0, 0x68
    .short 6
    .byte 0, 0x78
    .short 7
    .byte 0, 0x88
    .short 8
    .byte 0, 0x98
    .short 9
    .byte 0, 0xa8
    .short 10
    .byte 0, 0xb8
    .short 11
    .byte 0, 0xc8
    .short 12
    .byte 0, 0xd8
    .short 13
    .byte 0, 0xe8
    .short 14
    .byte 0, 0xf8
    .short 15
    .byte 0, 0x01
    .short 0x20
    .byte 0, 0x0a
    .short 0

.global osyx_restore_context
osyx_restore_context:
    fxrstor64 [rcx+0x100]
    ldmxcsr [rcx+0x34]
    mov rax, [rcx+0x98]
    mov rdx, [rcx+0xf8]
    mov [rax-8], rdx
    mov edx, [rcx+0x44]
    mov [rax-16], rdx
    mov rbx, [rcx+0x90]
    mov rbp, [rcx+0xa0]
    mov rsi, [rcx+0xa8]
    mov rdi, [rcx+0xb0]
    mov r8, [rcx+0xb8]
    mov r9, [rcx+0xc0]
    mov r10, [rcx+0xc8]
    mov r11, [rcx+0xd0]
    mov r12, [rcx+0xd8]
    mov r13, [rcx+0xe0]
    mov r14, [rcx+0xe8]
    mov r15, [rcx+0xf0]
    mov rdx, [rcx+0x88]
    lea rsp, [rax-16]
    popfq
    mov rax, [rcx+0x78]
    mov rcx, [rcx+0x80]
    ret

.global osyx_RtlUnwindEx
osyx_RtlUnwindEx:
    sub rsp, 1288
    mov [rsp+48+0x90], rbx
    mov [rsp+48+0xa0], rbp
    mov [rsp+48+0xa8], rsi
    mov [rsp+48+0xb0], rdi
    mov [rsp+48+0xd8], r12
    mov [rsp+48+0xe0], r13
    mov [rsp+48+0xe8], r14
    mov [rsp+48+0xf0], r15
    fxsave64 [rsp+48+0x100]
    stmxcsr [rsp+48+0x34]
    pushfq
    pop rax
    mov [rsp+48+0x44], eax
    mov dword ptr [rsp+48+0x30], 0x10000b
    lea rax, [rsp+1296]
    mov [rsp+48+0x98], rax
    mov rax, [rsp+1288]
    mov [rsp+48+0xf8], rax
    lea rax, [rsp+48]
    mov [rsp+32], rax
    call osyx_unwind_ex
    ud2
"#
);

extern "C" {
    fn osyx_consolidate_setup();
    fn osyx_consolidate_begin();
    fn osyx_consolidate_end();
    static osyx_consolidate_unwind: u8;
}
extern "win64" {
    fn osyx_restore_context(ctx: *const Context) -> !;
    #[link_name = "osyx_RtlUnwindEx"]
    fn RtlUnwindEx(frame: u64, ip: u64, record: *mut Record, retval: u64, original: u64, history: u64);
}

static REGISTERED: Mutex<bool> = Mutex::new(false);

fn register_thunk() {
    let mut r = REGISTERED.lock().unwrap();
    if *r {
        return;
    }
    let base = osyx_consolidate_begin as *const () as u64;
    let end = osyx_consolidate_end as *const () as u64;
    let info = core::ptr::addr_of!(osyx_consolidate_unwind) as u64;
    let table: &'static mut [u32; 3] = Box::leak(Box::new([0, (end - base) as u32, (info - base) as u32]));
    unwind::add_table(table.as_ptr() as u64, 1, base);
    *r = true;
}

/// Lays out the consolidation frame below `top` (the target RSP) from
/// the heap copy and returns S.
#[no_mangle]
extern "win64" fn osyx_consolidate_fill(data: *mut (Context, Record), top: u64) -> u64 {
    // SAFETY: allocated by `restore`; consumed here.
    let (ctx, rec) = *unsafe { Box::from_raw(data) };
    let copy = (top - 0x500) & !15;
    let rec_at = (copy - 0x100) & !15;
    let s = (rec_at - 0x140) & !15;
    // SAFETY: [s, top) is unused stack below the target frame.
    unsafe {
        core::ptr::write(copy as *mut Context, ctx);
        core::ptr::write(rec_at as *mut Record, rec);
        let w = |off: u64, v: u64| core::ptr::write((s + off) as *mut u64, v);
        for (i, reg) in [3usize, 5, 6, 7, 12, 13, 14, 15].iter().enumerate() {
            w(0x20 + i as u64 * 8, ctx.regs[*reg]);
        }
        for i in 0..10 {
            core::ptr::copy_nonoverlapping(ctx.fx.as_ptr().add(0xa0 + (6 + i) * 16), (s + 0x60 + i as u64 * 16) as *mut u8, 16);
        }
        w(0x100, ctx.rip);
        w(0x108, 0x33);
        w(0x110, ctx.eflags as u64);
        w(0x118, ctx.regs[RSP]);
        w(0x120, 0x2b);
        w(0x128, copy);
        w(0x130, rec_at);
    }
    s
}

/// The catch block's callback returned the continuation (already stored
/// in the context copy): continue the target frame there.
#[no_mangle]
extern "win64" fn osyx_consolidate_done(ctx: *const Context) -> ! {
    // SAFETY: the context copy laid out by osyx_consolidate_fill.
    restore_raw(unsafe { *ctx })
}

#[no_mangle]
extern "win64" fn osyx_unwind_ex(target_frame: u64, target_ip: u64, record: *mut Record, retval: u64, caller: *const Context) -> ! {
    let pending = OUTSTANDING.lock().unwrap().get(&os_sys::gettid()).map(|o| *o.original);
    // SAFETY: the context captured by the entry thunk.
    let start = pending.unwrap_or_else(|| unsafe { *caller });
    unwind_to(target_frame, target_ip, record, retval, start)
}

extern "win64" fn RtlRestoreContext(ctx: *const Context, record: *const Record) -> ! {
    // SAFETY: the caller's CONTEXT and optional EXCEPTION_RECORD.
    let c = unsafe { *ctx };
    match unsafe { record.as_ref() } {
        Some(r) => restore(c, r),
        None => restore_raw(c),
    }
}

/// The language handler of C `__try` blocks. HandlerData is a SCOPE_TABLE:
/// a count, then (begin, end, filter or handler, target) RVAs per scope;
/// target 0 marks a `__finally` block.
extern "win64" fn __C_specific_handler(rec: *mut Record, frame: u64, ctx: *mut Context, d: *mut Dispatcher) -> u32 {
    type Filter = extern "win64" fn(*mut [u64; 2], u64) -> i32;
    type Termination = extern "win64" fn(u8, u64);
    // SAFETY: the dispatcher's structures and the image's scope table.
    unsafe {
        let d = &mut *d;
        let table = d.data;
        let count = *(table as *const u32);
        let scope = |i: u32| *((table + 4 + i as u64 * 16) as *const [u32; 4]);
        let inside = |pc: u64, s: [u32; 4]| pc >= d.image_base + s[0] as u64 && pc < d.image_base + s[1] as u64;
        if (*rec).flags & (EH_UNWINDING | EH_EXIT_UNWIND) != 0 {
            let mut i = d.scope;
            while i < count {
                let s = scope(i);
                if inside(d.control_pc, s) && s[3] == 0 {
                    if (*rec).flags & EH_TARGET_UNWIND != 0 && inside(d.target_ip, s) {
                        break;
                    }
                    d.scope = i + 1;
                    let h: Termination = core::mem::transmute((d.image_base + s[2] as u64) as usize);
                    h(1, frame);
                }
                i += 1;
            }
            return 1;
        }
        for i in d.scope..count {
            let s = scope(i);
            if !inside(d.control_pc, s) || s[3] == 0 {
                continue;
            }
            if s[2] != 1 {
                // EXCEPTION_POINTERS { record, context }
                let mut pointers = [rec as u64, ctx as u64];
                let f: Filter = core::mem::transmute((d.image_base + s[2] as u64) as usize);
                match f(&mut pointers, frame) {
                    r if r < 0 => return 0,
                    0 => continue,
                    _ => {}
                }
            }
            RtlUnwindEx(frame, d.image_base + s[3] as u64, rec, (*rec).code as u64, ctx as u64, d.history);
        }
    }
    1
}

pub fn ntdll_exports() -> alloc::vec::Vec<(&'static str, u64)> {
    let mut e = exports();
    e.push(("__C_specific_handler", __C_specific_handler as *const () as u64));
    e
}

pub fn exports() -> alloc::vec::Vec<(&'static str, u64)> {
    alloc::vec![
        ("RtlUnwindEx", RtlUnwindEx as *const () as u64),
        ("RtlUnwind", RtlUnwindEx as *const () as u64),
        ("RtlRestoreContext", RtlRestoreContext as *const () as u64),
    ]
}
