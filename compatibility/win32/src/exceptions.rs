//! Vectored hardware exception callbacks over real resumable OSyx faults.
//! CONTEXT carries actual integer/control/x87/SSE state. Frame-based SEH,
//! debugger dispatch, nested hardware exceptions and AVX state are not yet
//! implemented; unhandled faults terminate rather than reporting success.
#![allow(non_snake_case)]
use crate::{error::*, kernel32::fail};
use alloc::{collections::BTreeMap, vec::Vec};
use core::arch::global_asm;
use core::sync::atomic::{AtomicU64, Ordering};
use os_std::sync::Mutex;
use osyx_abi::fault::{self, Context as NativeContext};
#[repr(C, align(16))]
#[derive(Clone, Copy)]
pub(crate) struct Context {
    home: [u64; 6],
    pub flags: u32,
    pub mxcsr: u32,
    segments: [u16; 6],
    pub eflags: u32,
    debug: [u64; 6],
    pub regs: [u64; 16],
    pub rip: u64,
    pub fx: [u8; 512],
    vectors: [u128; 26],
    vector_control: u64,
    debug_control: u64,
    last: [u64; 4],
}
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct Record {
    pub code: u32,
    pub flags: u32,
    pub nested: u64,
    pub address: u64,
    pub count: u32,
    pub reserved: u32,
    pub info: [u64; 15],
}
#[repr(C)]
struct Pointers {
    record: *mut Record,
    context: *mut Context,
}
const _: () = assert!(core::mem::size_of::<Context>() == 1232);
const _: () = assert!(core::mem::offset_of!(Context, regs) == 120);
const _: () = assert!(core::mem::offset_of!(Context, rip) == 248);
const _: () = assert!(core::mem::offset_of!(Context, fx) == 256);
const _: () = assert!(core::mem::size_of::<Record>() == 152);
const REGISTER_MAP: [usize; 16] = [
    fault::RAX,
    fault::RCX,
    fault::RDX,
    fault::RBX,
    fault::RSP,
    fault::RBP,
    fault::RSI,
    fault::RDI,
    fault::R8,
    fault::R9,
    fault::R10,
    fault::R11,
    fault::R12,
    fault::R13,
    fault::R14,
    fault::R15,
];
#[derive(Clone, Copy)]
struct Handler {
    id: u64,
    callback: u64,
    continuation: bool,
}
static HANDLERS: Mutex<Vec<Handler>> = Mutex::new(Vec::new());
static NEXT: AtomicU64 = AtomicU64::new(0x1000);
static FILTER: AtomicU64 = AtomicU64::new(0);
struct Software {
    record: Record,
    context: NativeContext,
}
static SOFTWARE: Mutex<BTreeMap<u64, Software>> = Mutex::new(BTreeMap::new());
global_asm!(
    r#"
.section .text
.global osyx_windows_raise_entry
osyx_windows_raise_entry:
    pushfq
    sub rsp,720
    mov [rsp+32],rax
    mov [rsp+40],rbx
    mov [rsp+48],rcx
    mov [rsp+56],rdx
    mov [rsp+64],rsi
    mov [rsp+72],rdi
    mov [rsp+80],rbp
    mov [rsp+96],r8
    mov [rsp+104],r9
    mov [rsp+112],r10
    mov [rsp+120],r11
    mov [rsp+128],r12
    mov [rsp+136],r13
    mov [rsp+144],r14
    mov [rsp+152],r15
    lea rax,[rsp+736]
    mov [rsp+88],rax
    mov rax,[rsp+728]
    mov [rsp+160],rax
    mov rax,[rsp+720]
    mov [rsp+168],rax
    xor eax,eax
    mov [rsp+176],rax
    mov [rsp+184],rax
    mov [rsp+192],rax
    mov [rsp+200],rax
    cld
    lea rdi,[rsp+208]
    mov ecx,64
    rep stosq
    fxsave64 [rsp+208]
    lea rcx,[rsp+32]
    call osyx_windows_raise_captured
    ud2
.global osyx_windows_raise_trap
osyx_windows_raise_trap:
    ud2
    ret
"#
);
extern "C" {
    fn osyx_windows_raise_trap();
}
extern "win64" {
    #[link_name = "osyx_windows_raise_entry"]
    pub(crate) fn RaiseException(code: u32, flags: u32, count: u32, args: *const u64);
}
/// Faults of Windows code always go through entry: frame-based handlers
/// (seh.rs) need no registration. Without kernel support, faults keep
/// terminating the process as before.
pub fn install() {
    // SAFETY: entry is a valid fault callback for the whole process.
    let _ = unsafe { os_sys::fault_handler(entry as *const () as u64) };
}
pub fn set_filter(callback: u64) -> u64 {
    let _handlers = HANDLERS.lock().unwrap();
    if let Err(e) = unsafe { os_sys::fault_handler(entry as *const () as u64) } {
        return fail(errno_to_win32(e), 0);
    }
    FILTER.swap(callback, Ordering::SeqCst)
}
#[no_mangle]
extern "win64" fn osyx_windows_raise_captured(captured: *const NativeContext) -> ! {
    let context = unsafe { *captured };
    let code = context.gpr[fault::RCX] as u32;
    let flags = context.gpr[fault::RDX] as u32;
    let count = context.gpr[fault::R8] as u32;
    let args = context.gpr[fault::R9] as *const u64;
    if count > 15 || count != 0 && args.is_null() {
        os_std::process::exit(0xc000000du32 as i32);
    }
    {
        let _handlers = HANDLERS.lock().unwrap();
        if unsafe { os_sys::fault_handler(entry as *const () as u64) }.is_err() {
            os_std::process::exit(code as i32);
        }
    }
    let mut r = Record {
        code: code & !0x10000000,
        flags: flags & 1,
        nested: 0,
        address: context.rip,
        count,
        reserved: 0,
        info: [0; 15],
    };
    for i in 0..count as usize {
        r.info[i] = unsafe { *args.add(i) };
    }
    let tid = os_sys::gettid();
    SOFTWARE.lock().unwrap().insert(tid, Software { record: r, context });
    // Use the real native trap delivery/return path, with registers/FPU
    // captured at the Windows entry thunk. Continuation skips these helper
    // frames and resumes the PE caller's actual return address and stack.
    unsafe {
        osyx_windows_raise_trap();
    }
    os_std::process::exit(0xc000000du32 as i32)
}
fn add(first: u32, callback: u64, continuation: bool) -> u64 {
    if callback == 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let mut handlers = HANDLERS.lock().unwrap();
    // Registration succeeds only when the kernel can deliver real faults.
    if let Err(e) = unsafe { os_sys::fault_handler(entry as *const () as u64) } {
        return fail(errno_to_win32(e), 0);
    }
    let h = Handler { id: NEXT.fetch_add(16, Ordering::Relaxed), callback, continuation };
    if first != 0 {
        handlers.insert(0, h);
    } else {
        handlers.push(h);
    }
    h.id
}
fn remove(id: u64, continuation: bool) -> u32 {
    let mut handlers = HANDLERS.lock().unwrap();
    let Some(at) = handlers.iter().position(|h| h.id == id && h.continuation == continuation) else { return 0 };
    handlers.remove(at);
    1
}
extern "win64" fn AddVectoredExceptionHandler(first: u32, callback: u64) -> u64 {
    add(first, callback, false)
}
extern "win64" fn RemoveVectoredExceptionHandler(id: u64) -> u32 {
    remove(id, false)
}
extern "win64" fn AddVectoredContinueHandler(first: u32, callback: u64) -> u64 {
    add(first, callback, true)
}
extern "win64" fn RemoveVectoredContinueHandler(id: u64) -> u32 {
    remove(id, true)
}
fn windows_context(n: &NativeContext) -> Context {
    let mut c = Context {
        home: [0; 6],
        flags: 0x0010_000b,
        mxcsr: u32::from_le_bytes(n.fx[24..28].try_into().unwrap()),
        segments: [0x23, 0x1b, 0x1b, 0, 0, 0x1b],
        eflags: n.rflags as u32,
        debug: [0; 6],
        regs: [0; 16],
        rip: if n.vector == 3 { n.rip.saturating_sub(1) } else { n.rip },
        fx: n.fx,
        vectors: [0; 26],
        vector_control: 0,
        debug_control: 0,
        last: [0; 4],
    };
    for (i, &index) in REGISTER_MAP.iter().enumerate() {
        c.regs[i] = n.gpr[index];
    }
    c
}
fn record(n: &NativeContext) -> Record {
    let code = match n.vector {
        0 => 0xc0000094,
        1 => 0x80000004,
        3 => 0x80000003,
        4 => 0xc0000095,
        5 => 0xc000008c,
        6 => 0xc000001d,
        13 | 14 => 0xc0000005,
        16 | 19 => 0xc0000090,
        17 => 0x80000002,
        _ => 0xc0000001,
    };
    let mut r = Record {
        code,
        flags: 0,
        nested: 0,
        address: if n.vector == 3 { n.rip.saturating_sub(1) } else { n.rip },
        count: 0,
        reserved: 0,
        info: [0; 15],
    };
    if n.vector == 13 || n.vector == 14 {
        r.count = 2;
        r.info[0] = if n.error & 16 != 0 {
            8
        } else if n.error & 2 != 0 {
            1
        } else {
            0
        };
        r.info[1] = n.address;
    }
    r
}
fn call(h: Handler, p: &mut Pointers) -> i32 {
    let callback: extern "win64" fn(*mut Pointers) -> i32 = unsafe { core::mem::transmute(h.callback) };
    callback(p)
}
fn apply(c: &Context, n: &mut NativeContext) {
    if c.flags & 1 != 0 {
        n.rip = c.rip;
        n.rflags = c.eflags as u64;
        n.gpr[fault::RSP] = c.regs[4];
    }
    if c.flags & 2 != 0 {
        for (i, &index) in REGISTER_MAP.iter().enumerate() {
            if index != fault::RSP {
                n.gpr[index] = c.regs[i];
            }
        }
    }
    if c.flags & 8 != 0 {
        let old_mxcsr = u32::from_le_bytes(n.fx[24..28].try_into().unwrap());
        n.fx = c.fx;
        if c.mxcsr != old_mxcsr {
            n.fx[24..28].copy_from_slice(&c.mxcsr.to_le_bytes());
        }
    }
}
/// Every register of `c` into the native frame (RtlRestoreContext).
pub(crate) fn apply_all(c: &Context, n: &mut NativeContext) {
    let mut full = *c;
    full.flags |= 0xb;
    apply(&full, n);
}
extern "C" fn entry(native: *mut NativeContext) -> ! {
    let n = unsafe { &mut *native };
    let mut context = windows_context(n);
    let mut record = record(n);
    let software = n.vector == 6 && n.rip == osyx_windows_raise_trap as *const () as u64;
    if software {
        if let Some(s) = SOFTWARE.lock().unwrap().remove(&os_sys::gettid()) {
            *n = s.context;
            record = s.record;
            context = windows_context(n);
        }
    }
    // Unwinding during this dispatch (RtlUnwindEx) starts at this context
    // and ends the kernel delivery through `native`.
    crate::seh::begin(native, &context);
    let mut p = Pointers { record: &mut record, context: &mut context };
    let handlers = HANDLERS.lock().unwrap().clone();
    let mut handled = handlers.iter().filter(|h| !h.continuation).any(|h| call(*h, &mut p) == -1);
    if !handled {
        // SAFETY: `p` points at this frame's record and context.
        handled = unsafe { crate::seh::dispatch(&mut *p.record, &mut *p.context) };
    }
    if !handled {
        let callback = FILTER.load(Ordering::SeqCst);
        if callback != 0 {
            handled = call(Handler { id: 0, callback, continuation: false }, &mut p) == -1;
        }
    }
    if handled {
        for h in handlers.iter().filter(|h| h.continuation) {
            if call(*h, &mut p) == -1 {
                break;
            }
        }
    }
    // fault_return never returns on success: explicitly release this heap
    // snapshot before abandoning the dispatcher's Rust stack frames.
    drop(handlers);
    crate::seh::end();
    if handled {
        if record.flags & 1 != 0 {
            os_std::process::exit(0xc0000025u32 as i32);
        }
        apply(&context, n);
        let error = unsafe { os_sys::fault_return(n) };
        os_std::eprintln!("winrun: exception continuation rejected: {error:?}");
        os_std::process::exit(0xc000000du32 as i32);
    }
    os_std::eprintln!("winrun: unhandled Windows exception {:#x} at {:#x}", record.code, record.address);
    os_std::process::exit(record.code as i32)
}
pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        AddVectoredExceptionHandler,
        RemoveVectoredExceptionHandler,
        AddVectoredContinueHandler,
        RemoveVectoredContinueHandler
    ]
}
pub fn native_exports() -> Vec<(&'static str, u64)> {
    alloc::vec![
        ("RtlAddVectoredExceptionHandler", AddVectoredExceptionHandler as *const () as u64),
        ("RtlRemoveVectoredExceptionHandler", RemoveVectoredExceptionHandler as *const () as u64),
        ("RtlAddVectoredContinueHandler", AddVectoredContinueHandler as *const () as u64),
        ("RtlRemoveVectoredContinueHandler", RemoveVectoredContinueHandler as *const () as u64)
    ]
}
