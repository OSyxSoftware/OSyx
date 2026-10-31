//! ucrtbase.dll: an original implementation of the Universal C Runtime on
//! OSyx, behind the `api-ms-win-crt-*` API set contracts. It follows the
//! documented UCRT behaviour (errno values, the invalid-parameter handler,
//! the `__stdio_common_*` option flags, the FILE buffer protocol used by
//! msvcp140) and calls the OSyx kernel32 for files, processes and the
//! environment, as the real UCRT calls Win32. Requests it cannot honour
//! fail with the documented errno and return value; nothing reports
//! success without doing the work.
//!
//! Locales: the "C" locale and the C locale with the UTF-8 code page
//! (`C.UTF-8`, selected by "", ".UTF-8" or ".65001": OSyx's ANSI code
//! page is UTF-8). OSyx has no national locale data, so other names fail
//! as unknown locales.

#![allow(non_snake_case, non_upper_case_globals)]

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use os_std::sync::Mutex;

/// `_VALIDATE_RETURN`: on a failed precondition sets errno, calls the
/// invalid-parameter handler and returns `$ret`.
macro_rules! validate {
    ($cond:expr, $err:expr, $ret:expr) => {
        if !$cond {
            $crate::ucrt::set_errno($err);
            $crate::ucrt::invalid_parameter();
            return $ret;
        }
    };
}

/// Declares a typed wrapper for a function of the built-in kernel32.
macro_rules! k32fn {
    ($vis:vis fn $name:ident = $sym:literal ($($a:ident : $t:ty),*) -> $r:ty) => {
        #[allow(clippy::too_many_arguments)]
        $vis fn $name($($a: $t),*) -> $r {
            type F = extern "win64" fn($($t),*) -> $r;
            // SAFETY: the kernel32 export named $sym has exactly this
            // signature (see kernel32.rs).
            let f: F = unsafe { core::mem::transmute::<usize, F>($crate::ucrt::k32($sym) as usize) };
            f($($a),*)
        }
    };
}

/// Windows x64 passes float arguments in xmm0-3 and returns floats in
/// xmm0, but winrun is built soft-float (x86_64-unknown-none): Rust passes
/// an f64/f32 in the integer register of the same position and returns it
/// in rax/eax. Each `name => imp [argument moves] result move;` exports a
/// naked trampoline `name` that moves the float arguments over, calls the
/// soft-float `imp` and moves a float result back ("" for other results).
macro_rules! xmm_abi {
    ($($name:ident => $imp:ident [$($arg:literal),*] $ret:literal;)*) => {$(
        #[unsafe(naked)]
        extern "win64" fn $name() {
            core::arch::naked_asm!($($arg,)* "sub rsp, 40", "call {imp}", "add rsp, 40", $ret, "ret", imp = sym $imp)
        }
    )*};
}

mod convert;
mod diagnostics;
mod env;
mod fenv;
mod fs;
mod heap;
pub(crate) mod libm;
mod locale;
mod lowio;
mod math;
mod metadata;
mod multibyte;
pub(crate) mod printf;
mod runtime;
mod scan;
mod stdio;
mod string;
mod time;
mod utility;
pub(crate) use utility::entropy;
pub(crate) use runtime::{GetMainArgs32, WGetMainArgs32};
pub(crate) use scan::legacy_swscanf32;

pub use heap::{crt_free, crt_malloc};

// ---- errno -----------------------------------------------------------------------

pub const ENOENT: i32 = 2;
pub const E2BIG: i32 = 7;
pub const ENOEXEC: i32 = 8;
pub const EBADF: i32 = 9;
pub const ECHILD: i32 = 10;
pub const EAGAIN: i32 = 11;
pub const ENOMEM: i32 = 12;
pub const EACCES: i32 = 13;
pub const EEXIST: i32 = 17;
pub const EXDEV: i32 = 18;
pub const EINVAL: i32 = 22;
pub const EMFILE: i32 = 24;
pub const ENOSPC: i32 = 28;
pub const EPIPE: i32 = 32;
pub const EDOM: i32 = 33;
pub const ERANGE: i32 = 34;
pub const ENOTEMPTY: i32 = 41;
pub const EILSEQ: i32 = 42;
pub const STRUNCATE: i32 = 80;

/// The UCRT's per-thread data.
pub struct Ptd {
    pub errno: i32,
    pub doserrno: u32,
    /// `_configthreadlocale`: this thread uses `locale` instead of the
    /// global locale.
    pub own_locale: bool,
    pub locale: locale::Locale,
    /// strtok / wcstok continuation.
    pub strtok: usize,
    pub wcstok: usize,
    pub rand_next: u32,
    /// `_set_thread_local_invalid_parameter_handler`.
    pub iph: u64,
    /// These APIs have independent buffers and natural element alignment.
    pub calendar: time::Tm,
    pub error_text: Vec<u8>,
    pub wide_error_text: Vec<u16>,
}

static PTDS: Mutex<BTreeMap<u64, Box<Ptd>>> = Mutex::new(BTreeMap::new());

/// The calling thread's data (created on first use).
pub fn ptd() -> &'static mut Ptd {
    let tid = crate::teb::current_tid();
    let p: *mut Ptd = {
        let mut m = PTDS.lock().unwrap();
        &mut **m.entry(tid).or_insert_with(|| {
            Box::new(Ptd {
                errno: 0,
                doserrno: 0,
                own_locale: false,
                locale: locale::Locale::C,
                strtok: 0,
                wcstok: 0,
                rand_next: 1,
                iph: 0,
                calendar: time::Tm::default(),
                error_text: Vec::new(),
                wide_error_text: Vec::new(),
            })
        })
    };
    // SAFETY: the box is only reached through the owning thread (keyed by
    // its id) and is freed only by that thread (`release_ptd`).
    unsafe { &mut *p }
}

/// Frees the calling thread's data (thread exit).
pub fn release_ptd() {
    let b = PTDS.lock().unwrap().remove(&os_sys::gettid());
    drop(b);
}

pub fn set_errno(e: i32) {
    ptd().errno = e;
}

pub fn errno() -> i32 {
    ptd().errno
}

/// Maps a Win32 error to errno (`_dosmaperr`), recording both.
pub fn dosmaperr(win: u32) {
    let e = match win {
        1 | 12 | 13 | 87 | 131 => EINVAL,
        2 | 3 | 15 | 18 | 53 | 67 | 161 | 206 => ENOENT,
        4 => EMFILE,
        5 | 16 | 19..=36 | 65 | 82 | 83 | 108 | 132 | 158 | 167 => EACCES,
        6 | 114 | 130 => EBADF,
        7..=9 | 1816 => ENOMEM,
        10 => E2BIG,
        11 | 188..=202 => ENOEXEC,
        17 => EXDEV,
        80 | 183 => EEXIST,
        89 | 164 | 215 => EAGAIN,
        109 => EPIPE,
        112 => ENOSPC,
        128 | 129 => ECHILD,
        145 => ENOTEMPTY,
        _ => EINVAL,
    };
    let p = ptd();
    p.doserrno = win;
    p.errno = e;
}

// ---- locks ----------------------------------------------------------------------------

/// A recursive lock (the UCRT's critical sections): the owner may lock
/// again, e.g. an atexit function registering another.
pub struct RLock {
    owner: AtomicU64,
    depth: core::sync::atomic::AtomicU32,
    word: core::sync::atomic::AtomicU32,
}

impl RLock {
    pub const fn new() -> RLock {
        RLock {
            owner: AtomicU64::new(0),
            depth: core::sync::atomic::AtomicU32::new(0),
            word: core::sync::atomic::AtomicU32::new(0),
        }
    }

    pub fn lock(&self) {
        let me = crate::teb::current_tid();
        if self.owner.load(Ordering::Acquire) == me {
            self.depth.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if self.word.compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_err() {
            while self.word.swap(2, Ordering::Acquire) != 0 {
                let _ = os_sys::futex_wait(self.word.as_ptr(), 2, 0);
            }
        }
        self.owner.store(me, Ordering::Release);
        self.depth.store(1, Ordering::Relaxed);
    }

    /// Releases one level; false if the caller does not own the lock.
    pub fn unlock(&self) -> bool {
        if self.owner.load(Ordering::Acquire) != crate::teb::current_tid() {
            return false;
        }
        if self.depth.fetch_sub(1, Ordering::Relaxed) == 1 {
            self.owner.store(0, Ordering::Release);
            if self.word.swap(0, Ordering::Release) == 2 {
                os_sys::futex_wake(self.word.as_ptr(), 1);
            }
        }
        true
    }
}

// ---- invalid parameters ---------------------------------------------------------------

static GLOBAL_IPH: AtomicU64 = AtomicU64::new(0);

/// `_invalid_parameter`: the thread's handler, else the process handler,
/// else fail-fast (STATUS_STACK_BUFFER_OVERRUN, as `_invoke_watson`).
pub fn invalid_parameter() {
    let h = match ptd().iph {
        0 => GLOBAL_IPH.load(Ordering::SeqCst),
        h => h,
    };
    if h == 0 {
        fast_fail(5); // FAST_FAIL_INVALID_ARG
    }
    type Handler = extern "win64" fn(u64, u64, u64, u32, u64);
    // SAFETY: a handler installed by the program.
    let f: Handler = unsafe { core::mem::transmute(h as usize) };
    f(0, 0, 0, 0, 0);
}

/// Sets errno, calls the invalid-parameter handler and returns `ret`.
pub fn invalid<T>(err: i32, ret: T) -> T {
    set_errno(err);
    invalid_parameter();
    ret
}

/// `__fastfail`: ends the process at once with STATUS_STACK_BUFFER_OVERRUN.
pub fn fast_fail(code: u32) -> ! {
    os_std::eprintln!("winrun: the C runtime requested fail-fast (code {code})");
    os_std::process::exit(0xC000_0409u32 as i32)
}

extern "win64" fn _set_invalid_parameter_handler(h: u64) -> u64 {
    GLOBAL_IPH.swap(h, Ordering::SeqCst)
}

extern "win64" fn _get_invalid_parameter_handler() -> u64 {
    GLOBAL_IPH.load(Ordering::SeqCst)
}

extern "win64" fn _set_thread_local_invalid_parameter_handler(h: u64) -> u64 {
    core::mem::replace(&mut ptd().iph, h)
}

extern "win64" fn _get_thread_local_invalid_parameter_handler() -> u64 {
    ptd().iph
}

extern "win64" fn _invalid_parameter(_e: u64, _f: u64, _file: u64, _line: u32, _r: u64) {
    invalid_parameter()
}

extern "win64" fn _invalid_parameter_noinfo() {
    invalid_parameter()
}

extern "win64" fn _invalid_parameter_noinfo_noreturn() -> ! {
    invalid_parameter();
    fast_fail(5)
}

extern "win64" fn _invoke_watson(_e: u64, _f: u64, _file: u64, _line: u32, _r: u64) -> ! {
    fast_fail(5)
}

// ---- kernel32 ----------------------------------------------------------------------

static K32: Mutex<BTreeMap<&'static str, u64>> = Mutex::new(BTreeMap::new());

/// Address of a function of the built-in kernel32 (the UCRT is built on
/// Win32, like the real one).
pub fn k32(name: &'static str) -> u64 {
    let mut m = K32.lock().unwrap();
    if m.is_empty() {
        m.extend(crate::kernel32::exports());
    }
    match m.get(name) {
        Some(&a) => a,
        None => panic!("ucrtbase: kernel32!{name} is not implemented"),
    }
}

k32fn!(pub fn get_last_error = "GetLastError"() -> u32);

// ---- strings -------------------------------------------------------------------------

/// The bytes of a NUL-terminated string (without the terminator).
///
/// # Safety
/// `p` must be a readable NUL-terminated string.
pub unsafe fn cstr<'a>(p: *const u8) -> &'a [u8] {
    let mut n = 0;
    // SAFETY: the caller guarantees the terminator.
    while unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    // SAFETY: `n` bytes were just read.
    unsafe { core::slice::from_raw_parts(p, n) }
}

/// As [`cstr`] for UTF-16.
///
/// # Safety
/// `p` must be a readable NUL-terminated UTF-16 string.
pub unsafe fn wstr<'a>(p: *const u16) -> &'a [u16] {
    let mut n = 0;
    // SAFETY: the caller guarantees the terminator.
    while unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    // SAFETY: `n` units were just read.
    unsafe { core::slice::from_raw_parts(p, n) }
}

/// Copies `units` plus a terminator into fresh CRT heap memory.
pub fn heap_wstr(units: &[u16]) -> *mut u16 {
    let p = crt_malloc((units.len() + 1) * 2) as *mut u16;
    if !p.is_null() {
        // SAFETY: a fresh block of len + 1 units.
        unsafe {
            core::ptr::copy_nonoverlapping(units.as_ptr(), p, units.len());
            *p.add(units.len()) = 0;
        }
    }
    p
}

/// Copies `bytes` plus a terminator into fresh CRT heap memory.
pub fn heap_cstr(bytes: &[u8]) -> *mut u8 {
    let p = crt_malloc(bytes.len() + 1);
    if !p.is_null() {
        // SAFETY: a fresh block of len + 1 bytes.
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
            *p.add(bytes.len()) = 0;
        }
    }
    p
}

pub fn exports() -> Vec<(&'static str, u64)> {
    let mut v = export_table![
        _set_invalid_parameter_handler,
        _get_invalid_parameter_handler,
        _set_thread_local_invalid_parameter_handler,
        _get_thread_local_invalid_parameter_handler,
        _invalid_parameter,
        _invalid_parameter_noinfo,
        _invalid_parameter_noinfo_noreturn,
        _invoke_watson,
    ];
    v.extend(heap::exports());
    v.extend(string::exports());
    v.extend(runtime::exports());
    v.extend(convert::exports());
    v.extend(env::exports());
    v.extend(fs::exports());
    v.extend(time::exports());
    v.extend(math::exports());
    v.extend(libm::exports());
    v.extend(utility::exports());
    v.extend(locale::exports());
    v.extend(lowio::exports());
    v.extend(stdio::exports());
    v.extend(printf::exports());
    v.extend(scan::exports());
    v.extend(diagnostics::exports());
    v.extend(fenv::exports());
    v.extend(metadata::exports());
    v.extend(multibyte::exports());
    v
}
