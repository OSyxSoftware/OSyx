//! Process, thread and machine information xul.dll asks kernel32 for:
//! the one-CPU topology OSyx schedules on, memory counters from the
//! kernel's frame accounting, error modes, session ids, power requests and
//! restart registration, and the computer name.
//!
//! What OSyx does not have is reported as such: there is no per-thread CPU
//! accounting or cycle counter (GetThreadTimes, Query*CycleTime fail with
//! ERROR_NOT_SUPPORTED), no forced termination of another thread, no
//! battery information, and no Windows Error Reporting to act on a restart
//! registration (it is kept and reported back, never used).
#![allow(non_snake_case)]
use crate::{
    error::*,
    kernel32::{self, fail},
    objects::{self, Object},
    teb,
};
use alloc::{collections::BTreeMap, string::String, vec::Vec};
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use os_std::sync::Mutex;

/// The session every OSyx process runs in (the interactive console).
const SESSION: u32 = 1;

fn is_thread(h: u64) -> bool {
    h == objects::CURRENT_THREAD || matches!(objects::get(h), Some(Object::Thread(_)))
}

fn is_process(h: u64) -> bool {
    h == objects::CURRENT_PROCESS || matches!(objects::get(h), Some(Object::Process(_)))
}

// ---- threads ---------------------------------------------------------------------

extern "win64" fn GetProcessIdOfThread(h: u64) -> u32 {
    if !is_thread(h) {
        return fail(ERROR_INVALID_HANDLE, 0);
    }
    kernel32::pid() as u32 // every thread handle here belongs to this process
}

/// A thread of this process by id: a handle to its thread object.
extern "win64" fn OpenThread(_access: u32, inherit: i32, tid: u32) -> u64 {
    if inherit != 0 {
        return fail(ERROR_NOT_SUPPORTED, 0);
    }
    match objects::find(|o| matches!(o, Object::Thread(t) if t.tid.load(Ordering::SeqCst) == tid)) {
        Some(o) => objects::insert(o),
        None => fail(ERROR_INVALID_PARAMETER, 0),
    }
}

extern "win64" fn GetThreadTimes(_h: u64, _c: *mut u64, _e: *mut u64, _k: *mut u64, _u: *mut u64) -> i32 {
    fail(ERROR_NOT_SUPPORTED, 0)
}

extern "win64" fn QueryThreadCycleTime(_h: u64, _cycles: *mut u64) -> i32 {
    fail(ERROR_NOT_SUPPORTED, 0)
}

extern "win64" fn QueryProcessCycleTime(_h: u64, _cycles: *mut u64) -> i32 {
    fail(ERROR_NOT_SUPPORTED, 0)
}

/// Only the calling thread can end itself (ExitThread); OSyx offers no way
/// to stop another thread at an arbitrary point.
extern "win64" fn TerminateThread(h: u64, code: u32) -> i32 {
    if !is_thread(h) {
        return fail(ERROR_INVALID_HANDLE, 0);
    }
    if kernel32::GetThreadId(h) == os_sys::gettid() as u32 {
        kernel32::ExitThread(code);
    }
    fail(ERROR_NOT_SUPPORTED, 0)
}

/// One processor: ideal processor 0 is the only one.
extern "win64" fn SetThreadIdealProcessor(h: u64, n: u32) -> u32 {
    if !is_thread(h) {
        return fail(ERROR_INVALID_HANDLE, u32::MAX);
    }
    if n != 0 && n != 64 {
        return fail(ERROR_INVALID_PARAMETER, u32::MAX); // 64 = MAXIMUM_PROCESSORS: query
    }
    0
}

/// GROUP_AFFINITY { KAFFINITY Mask; WORD Group; WORD Reserved[3] }.
extern "win64" fn GetThreadGroupAffinity(h: u64, out: *mut u8) -> i32 {
    if !is_thread(h) {
        return fail(ERROR_INVALID_HANDLE, 0);
    }
    if out.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: the caller's 16-byte GROUP_AFFINITY.
    unsafe {
        core::ptr::write_bytes(out, 0, 16);
        teb::put(out as u64, 0, 1u64);
    }
    1
}

/// Per thread: the stack guarantee in bytes. OSyx's stack overflow
/// handling does not reserve extra space, so the value is only reported
/// back; requests above the stack's size fail as on Windows.
static GUARANTEES: Mutex<BTreeMap<u64, u32>> = Mutex::new(BTreeMap::new());

extern "win64" fn SetThreadStackGuarantee(size: *mut u32) -> i32 {
    if size.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let me = os_sys::gettid();
    let mut g = GUARANTEES.lock().unwrap();
    let old = g.get(&me).copied().unwrap_or(0);
    // SAFETY: the caller's in/out size.
    let want = unsafe { *size };
    if want > 1 << 20 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    if want > old {
        g.insert(me, want);
    }
    // SAFETY: as above.
    unsafe { *size = old };
    1
}

// ---- processes and sessions ------------------------------------------------------------

extern "win64" fn ProcessIdToSessionId(pid: u32, session: *mut u32) -> i32 {
    if session.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let mut list = alloc::vec![osyx_abi::ipc::ProcInfo::default(); 512];
    let n = os_sys::proc_list(&mut list).unwrap_or(0);
    if !list[..n].iter().any(|p| p.pid == pid as u64) {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: the caller's out pointer.
    unsafe { *session = SESSION };
    1
}

extern "win64" fn WTSGetActiveConsoleSessionId() -> u32 {
    SESSION
}

/// The priority class is a scheduling hint OSyx's scheduler has no levels
/// for; it is kept so it reads back.
pub(crate) static PRIORITY_CLASS: AtomicU32 = AtomicU32::new(0x20); // NORMAL_PRIORITY_CLASS

extern "win64" fn SetPriorityClass(h: u64, class: u32) -> i32 {
    if !is_process(h) {
        return fail(ERROR_INVALID_HANDLE, 0);
    }
    const CLASSES: [u32; 6] = [0x20, 0x40, 0x80, 0x100, 0x4000, 0x8000];
    match class {
        c if CLASSES.contains(&c) => {
            if h == objects::CURRENT_PROCESS {
                PRIORITY_CLASS.store(c, Ordering::Relaxed);
            }
            1
        }
        // PROCESS_MODE_BACKGROUND_BEGIN / _END: the current process only.
        0x0010_0000 | 0x0020_0000 if h == objects::CURRENT_PROCESS => 1,
        _ => fail(ERROR_INVALID_PARAMETER, 0),
    }
}

static SHUTDOWN: AtomicU64 = AtomicU64::new(0x280 << 32);

/// Shutdown order among processes: OSyx's shutdown does not consult it,
/// but the level is validated and kept.
extern "win64" fn SetProcessShutdownParameters(level: u32, flags: u32) -> i32 {
    if !(0x100..=0x4FF).contains(&level) || flags & !1 != 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    SHUTDOWN.store((level as u64) << 32 | flags as u64, Ordering::Relaxed);
    1
}

// ---- error modes ----------------------------------------------------------------------

static ERROR_MODE: AtomicU32 = AtomicU32::new(0);
static THREAD_ERROR_MODES: Mutex<BTreeMap<u64, u32>> = Mutex::new(BTreeMap::new());

extern "win64" fn SetErrorMode(mode: u32) -> u32 {
    ERROR_MODE.swap(mode, Ordering::Relaxed)
}

extern "win64" fn GetErrorMode() -> u32 {
    ERROR_MODE.load(Ordering::Relaxed)
}

/// SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX and SEM_NOOPENFILEERRORBOX
/// are the thread-settable modes.
extern "win64" fn SetThreadErrorMode(mode: u32, old: *mut u32) -> i32 {
    if mode & !(0x1 | 0x2 | 0x8000) != 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let prev = THREAD_ERROR_MODES.lock().unwrap().insert(os_sys::gettid(), mode).unwrap_or(0);
    if !old.is_null() {
        // SAFETY: the caller's out pointer.
        unsafe { *old = prev };
    }
    1
}

// ---- machine --------------------------------------------------------------------------------

const RELATION_PROCESSOR_CORE: u32 = 0;
const RELATION_NUMA_NODE: u32 = 1;
const RELATION_PROCESSOR_PACKAGE: u32 = 3;

/// SYSTEM_LOGICAL_PROCESSOR_INFORMATION (32 bytes each): one core, one
/// package and one NUMA node, all holding processor 0. Caches are not
/// reported (OSyx does not read CPUID cache leaves).
extern "win64" fn GetLogicalProcessorInformation(buf: *mut u8, len: *mut u32) -> i32 {
    if len.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let entries = [RELATION_PROCESSOR_CORE, RELATION_PROCESSOR_PACKAGE, RELATION_NUMA_NODE];
    let need = (entries.len() * 32) as u32;
    // SAFETY: the caller's in/out length and buffer.
    unsafe {
        if buf.is_null() || *len < need {
            *len = need;
            return fail(ERROR_INSUFFICIENT_BUFFER, 0);
        }
        core::ptr::write_bytes(buf, 0, need as usize);
        for (i, r) in entries.iter().enumerate() {
            let e = buf as u64 + i as u64 * 32;
            teb::put(e, 0, 1u64);
            teb::put(e, 8, *r);
        }
        *len = need;
    }
    1
}

/// SYSTEM_CPU_SET_INFORMATION (32 bytes): the one CPU set, id 0x100.
extern "win64" fn GetSystemCpuSetInformation(buf: *mut u8, len: u32, returned: *mut u32, process: u64, flags: u32) -> i32 {
    if returned.is_null() || flags != 0 || (process != 0 && !is_process(process)) {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: the caller's out pointer and buffer of `len` bytes.
    unsafe {
        *returned = 32;
        if buf.is_null() || len < 32 {
            return fail(ERROR_INSUFFICIENT_BUFFER, 0);
        }
        core::ptr::write_bytes(buf, 0, 32);
        teb::put(buf as u64, 0, 32u32); // Size
        teb::put(buf as u64, 8, 0x100u32); // Id
    }
    1
}

/// SYSTEM_POWER_STATUS: the AC line and battery are unknown to OSyx
/// (255 / "no system battery" / unknown lifetimes).
extern "win64" fn GetSystemPowerStatus(out: *mut u8) -> i32 {
    if out.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: the caller's 12-byte structure.
    unsafe {
        *out = 255;
        *out.add(1) = 128;
        *out.add(2) = 255;
        *out.add(3) = 0;
        teb::put(out as u64, 4, u32::MAX);
        teb::put(out as u64, 8, u32::MAX);
    }
    1
}

/// Power requests: OSyx never idles the display or the machine into sleep
/// on its own, so any request is already satisfied; the handle exists for
/// Set/Clear and CloseHandle.
extern "win64" fn PowerCreateRequest(context: *const u8) -> u64 {
    // SAFETY: the caller's REASON_CONTEXT (Version, Flags).
    if context.is_null() || unsafe { teb::get::<u32>(context as u64, 0) } != 0 {
        return fail(ERROR_INVALID_PARAMETER, u64::MAX);
    }
    kernel32::new_event(true, false)
}

fn power_request(h: u64, kind: u32) -> i32 {
    if !matches!(objects::get(h), Some(Object::Event(_))) {
        return fail(ERROR_INVALID_HANDLE, 0);
    }
    if kind > 3 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    1
}

extern "win64" fn PowerSetRequest(h: u64, kind: u32) -> i32 {
    power_request(h, kind)
}

extern "win64" fn PowerClearRequest(h: u64, kind: u32) -> i32 {
    power_request(h, kind)
}

/// MEMORY_RESOURCE_NOTIFICATION: an event signaled when the condition
/// holds at creation (low: under 32 MiB free; high: over a quarter of
/// memory free). It is not updated afterwards.
extern "win64" fn CreateMemoryResourceNotification(kind: u32) -> u64 {
    let Ok(info) = os_sys::sysinfo() else { return fail(ERROR_GEN_FAILURE, 0) };
    let state = match kind {
        0 => info.free_memory < 32 << 20,
        1 => info.free_memory > info.total_memory / 4,
        _ => return fail(ERROR_INVALID_PARAMETER, 0),
    };
    kernel32::new_event(true, state)
}

// ---- restart registration ----------------------------------------------------------------

static RESTART: Mutex<Option<(String, u32)>> = Mutex::new(None);
const S_OK: i32 = 0;
const E_INVALIDARG: i32 = 0x8007_0057u32 as i32;

extern "win64" fn RegisterApplicationRestart(cmdline: *const u16, flags: u32) -> i32 {
    // SAFETY: NULL or a NUL-terminated command line.
    let line = unsafe { crate::path::wide_to_string(cmdline) }.unwrap_or_default();
    if line.encode_utf16().count() >= 1024 || flags & !0xF != 0 {
        return E_INVALIDARG;
    }
    *RESTART.lock().unwrap() = Some((line, flags));
    S_OK
}

extern "win64" fn UnregisterApplicationRestart() -> i32 {
    *RESTART.lock().unwrap() = None;
    S_OK
}

extern "win64" fn GetApplicationRestartSettings(process: u64, buf: *mut u16, size: *mut u32, flags: *mut u32) -> i32 {
    if process != objects::CURRENT_PROCESS || size.is_null() {
        return E_INVALIDARG;
    }
    let Some((line, f)) = RESTART.lock().unwrap().clone() else { return 0x8007_0490u32 as i32 }; // ERROR_NOT_FOUND
    let need = line.encode_utf16().count() as u32 + 1;
    // SAFETY: the caller's in/out size, buffer and flags.
    unsafe {
        if buf.is_null() || *size < need {
            *size = need;
            return 0x8007_007Au32 as i32; // ERROR_INSUFFICIENT_BUFFER
        }
        kernel32::out_w(&line, buf, *size);
        *size = need;
        if !flags.is_null() {
            *flags = f;
        }
    }
    S_OK
}

/// Not a packaged application.
extern "win64" fn GetCurrentApplicationUserModelId(len: *mut u32, _buf: *mut u16) -> i32 {
    if len.is_null() {
        return ERROR_INVALID_PARAMETER as i32;
    }
    15703 // APPMODEL_ERROR_NO_APPLICATION
}

// ---- memory counters ------------------------------------------------------------------------

static PEAK_FRAMES: AtomicU64 = AtomicU64::new(0);

/// The frames this process owns, from the kernel's process list.
fn own_frames() -> Option<u64> {
    let mut list = alloc::vec![osyx_abi::ipc::ProcInfo::default(); 512];
    let n = os_sys::proc_list(&mut list).ok()?;
    let me = kernel32::pid();
    let f = list[..n].iter().find(|p| p.pid == me)?.frames;
    PEAK_FRAMES.fetch_max(f, Ordering::Relaxed);
    Some(f)
}

/// PROCESS_MEMORY_COUNTERS (72 bytes) or _EX (80, adds PrivateUsage): the
/// working set and the private commit are both the process's frames
/// (OSyx neither pages out nor shares anonymous memory); page faults and
/// pool usage are not counted.
extern "win64" fn K32GetProcessMemoryInfo(h: u64, out: *mut u8, cb: u32) -> i32 {
    if h != objects::CURRENT_PROCESS {
        return fail(ERROR_ACCESS_DENIED, 0);
    }
    if out.is_null() || cb < 72 {
        return fail(ERROR_INSUFFICIENT_BUFFER, 0);
    }
    let Some(frames) = own_frames() else { return fail(ERROR_GEN_FAILURE, 0) };
    let (now, peak) = (frames * 4096, PEAK_FRAMES.load(Ordering::Relaxed) * 4096);
    let size = if cb >= 80 { 80 } else { 72 };
    // SAFETY: the caller's structure of `cb` bytes.
    unsafe {
        core::ptr::write_bytes(out, 0, size);
        let p = out as u64;
        teb::put(p, 0, size as u32);
        teb::put(p, 8, peak);
        teb::put(p, 16, now);
        teb::put(p, 56, now);
        teb::put(p, 64, peak);
        if size == 80 {
            teb::put(p, 72, now);
        }
    }
    1
}

/// PSAPI_WORKING_SET_INFORMATION lists every resident page; OSyx has no
/// call that enumerates them.
extern "win64" fn K32QueryWorkingSet(_h: u64, _buf: *mut u8, _len: u32) -> i32 {
    fail(ERROR_NOT_SUPPORTED, 0)
}

/// The image path in NT device form (\Device\HarddiskVolume1\...).
extern "win64" fn K32GetProcessImageFileNameW(h: u64, buf: *mut u16, size: u32) -> u32 {
    if h != objects::CURRENT_PROCESS {
        return fail(ERROR_ACCESS_DENIED, 0);
    }
    let Some(name) = kernel32::module_name(0) else { return fail(ERROR_GEN_FAILURE, 0) };
    let nt = alloc::format!("\\Device\\HarddiskVolume1{}", name.get(2..).unwrap_or(""));
    let r = kernel32::out_w(&nt, buf, size);
    if r >= size {
        return fail(ERROR_INSUFFICIENT_BUFFER, 0);
    }
    r
}

extern "win64" fn QueryFullProcessImageNameA(h: u64, flags: u32, buf: *mut u8, size: *mut u32) -> i32 {
    if size.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let mut w = alloc::vec![0u16; 1024];
    let mut n = w.len() as u32;
    if crate::process::QueryFullProcessImageNameW(h, flags, w.as_mut_ptr(), &mut n) == 0 {
        return 0;
    }
    let s = String::from_utf16_lossy(&w[..n as usize]);
    // SAFETY: the caller's in/out size and buffer.
    unsafe {
        if buf.is_null() || (*size as usize) <= s.len() {
            return fail(ERROR_INSUFFICIENT_BUFFER, 0);
        }
        core::ptr::copy_nonoverlapping(s.as_ptr(), buf, s.len());
        *buf.add(s.len()) = 0;
        *size = s.len() as u32;
    }
    1
}

extern "win64" fn VirtualQueryEx(h: u64, addr: u64, out: *mut u8, len: usize) -> usize {
    if h != objects::CURRENT_PROCESS {
        return fail(ERROR_ACCESS_DENIED, 0);
    }
    kernel32::VirtualQuery(addr, out, len)
}

// ---- computer name ----------------------------------------------------------------------------

fn host_name() -> String {
    os_std::fs::read_to_string("/etc/hostname").map(|s| String::from(s.trim())).unwrap_or_else(|_| String::from("osyx"))
}

/// The NetBIOS form: upper case, at most 15 characters.
fn netbios() -> String {
    host_name().to_uppercase().chars().take(15).collect()
}

/// Computer names: *size in, characters without the NUL out; too small
/// gives `short` (ERROR_BUFFER_OVERFLOW or ERROR_MORE_DATA, depending on
/// the call) and the size needed with the NUL.
fn name_out<T: Copy>(s: &[T], buf: *mut T, size: *mut u32, nul: T, short: u32) -> i32 {
    if size.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: the caller's in/out size and buffer.
    unsafe {
        if buf.is_null() || (*size as usize) <= s.len() {
            *size = s.len() as u32 + 1;
            return fail(short, 0);
        }
        core::ptr::copy_nonoverlapping(s.as_ptr(), buf, s.len());
        *buf.add(s.len()) = nul;
        *size = s.len() as u32;
    }
    1
}

extern "win64" fn GetComputerNameW(buf: *mut u16, size: *mut u32) -> i32 {
    let w: Vec<u16> = netbios().encode_utf16().collect();
    name_out(&w, buf, size, 0, 111) // ERROR_BUFFER_OVERFLOW
}

/// COMPUTER_NAME_FORMAT: NetBIOS forms (0, 4) are upper case; host name
/// forms (1, 3, 5, 7) are /etc/hostname; there is no DNS domain (2, 6).
extern "win64" fn GetComputerNameExA(kind: u32, buf: *mut u8, size: *mut u32) -> i32 {
    let s = match kind {
        0 | 4 => netbios(),
        1 | 3 | 5 | 7 => host_name(),
        2 | 6 => String::new(),
        _ => return fail(ERROR_INVALID_PARAMETER, 0),
    };
    name_out(s.as_bytes(), buf, size, 0, ERROR_MORE_DATA)
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        GetProcessIdOfThread,
        OpenThread,
        GetThreadTimes,
        QueryThreadCycleTime,
        QueryProcessCycleTime,
        TerminateThread,
        SetThreadIdealProcessor,
        GetThreadGroupAffinity,
        SetThreadStackGuarantee,
        ProcessIdToSessionId,
        WTSGetActiveConsoleSessionId,
        SetPriorityClass,
        SetProcessShutdownParameters,
        SetErrorMode,
        GetErrorMode,
        SetThreadErrorMode,
        GetLogicalProcessorInformation,
        GetSystemCpuSetInformation,
        GetSystemPowerStatus,
        PowerCreateRequest,
        PowerSetRequest,
        PowerClearRequest,
        CreateMemoryResourceNotification,
        RegisterApplicationRestart,
        UnregisterApplicationRestart,
        GetApplicationRestartSettings,
        GetCurrentApplicationUserModelId,
        K32GetProcessMemoryInfo,
        K32QueryWorkingSet,
        K32GetProcessImageFileNameW,
        QueryFullProcessImageNameA,
        VirtualQueryEx,
        GetComputerNameW,
        GetComputerNameExA,
    ]
}
