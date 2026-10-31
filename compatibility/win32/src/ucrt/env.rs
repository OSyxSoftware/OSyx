//! The C runtime's environment (environment contract): narrow and wide
//! `environ` tables made from the process environment on first use,
//! getenv/_putenv keeping both tables and the Win32 environment in step,
//! and the current directory.

use super::*;
use alloc::string::String;

/// One variable: its text and the heap string programs see (stable until
/// this variable changes, as in the UCRT).
struct Entry {
    text: Vec<u16>,
    ptr: usize,
}

struct Env {
    narrow: Option<Vec<Entry>>,
    wide: Option<Vec<Entry>>,
}

static ENV: Mutex<Env> = Mutex::new(Env { narrow: None, wide: None });
/// `_environ` / `_wenviron`: the NULL-terminated pointer arrays (0 until
/// created); reallocated on every change.
static ENVIRON: AtomicU64 = AtomicU64::new(0);
static WENVIRON: AtomicU64 = AtomicU64::new(0);
/// The same tables with four-byte slots, for a 32-bit CRT (`__getmainargs`).
static ENVIRON32: AtomicU64 = AtomicU64::new(0);
static WENVIRON32: AtomicU64 = AtomicU64::new(0);

fn os_snapshot() -> Vec<Vec<u16>> {
    crate::kernel32::with_process(|p| {
        p.env.iter().map(|(k, v)| alloc::format!("{k}={v}").encode_utf16().collect()).collect()
    })
}

/// Name part of an entry (up to the first '=' after position 0).
fn name_of(e: &[u16]) -> &[u16] {
    let i = e.iter().skip(1).position(|&c| c == b'=' as u16).map_or(e.len(), |i| i + 1);
    &e[..i]
}

fn eq_ci(a: &[u16], b: &[u16]) -> bool {
    let fold = |c: u16| if (b'a' as u16..=b'z' as u16).contains(&c) { c - 32 } else { c };
    a.len() == b.len() && a.iter().zip(b).all(|(&x, &y)| fold(x) == fold(y))
}

/// Narrow text of wide text (OSyx's ANSI code page is UTF-8).
fn narrow(e: &[u16]) -> Vec<u8> {
    String::from_utf16_lossy(e).into_bytes()
}

fn entry(text: Vec<u16>, wide: bool) -> Entry {
    let ptr = if wide { heap_wstr(&text) as usize } else { heap_cstr(&narrow(&text)) as usize };
    Entry { text, ptr }
}

/// Reallocates the pointer array of one table.
fn publish(t: Option<&Vec<Entry>>, var: &AtomicU64) {
    let old = var.load(Ordering::SeqCst);
    let new = match t {
        None => 0,
        Some(t) => {
            let arr = crt_malloc((t.len() + 1) * 8) as *mut usize;
            if !arr.is_null() {
                for (i, e) in t.iter().enumerate() {
                    // SAFETY: `arr` holds len + 1 pointers.
                    unsafe { *arr.add(i) = e.ptr };
                }
                // SAFETY: as above.
                unsafe { *arr.add(t.len()) = 0 };
            }
            arr as u64
        }
    };
    var.store(new, Ordering::SeqCst);
    crt_free(old as *mut u8);
}

/// Reallocates the pointer array of one 32-bit table from the same entries;
/// the heap strings are allocated below 4 GiB, so their low half is a
/// guest pointer.
fn publish32(t: Option<&Vec<Entry>>, var: &AtomicU64) {
    let old = var.load(Ordering::SeqCst);
    let new = match t {
        None => 0,
        Some(t) => {
            let arr = crt_malloc((t.len() + 1) * 4) as *mut u32;
            if !arr.is_null() {
                for (i, e) in t.iter().enumerate() {
                    // SAFETY: `arr` holds len + 1 slots.
                    unsafe { *arr.add(i) = e.ptr as u32 };
                }
                // SAFETY: as above.
                unsafe { *arr.add(t.len()) = 0 };
            }
            arr as u64
        }
    };
    var.store(new, Ordering::SeqCst);
    crt_free(old as *mut u8);
}

/// Re-creates the 32-bit array if a 32-bit caller ever asked for it, so
/// `_putenv` changes stay visible to it.
fn republish32(t: Option<&Vec<Entry>>, wide: bool) {
    let var = if wide { &WENVIRON32 } else { &ENVIRON32 };
    if var.load(Ordering::SeqCst) != 0 {
        publish32(t, var);
    }
}

/// Creates the narrow (or wide) table if missing: from the other table
/// when that exists (it carries earlier _putenv changes), else from the
/// process environment.
fn ensure(env: &mut Env, wide: bool) {
    if (wide && env.wide.is_some()) || (!wide && env.narrow.is_some()) {
        return;
    }
    let src: Vec<Vec<u16>> = match if wide { &env.narrow } else { &env.wide } {
        Some(t) => t.iter().map(|e| e.text.clone()).collect(),
        None => os_snapshot(),
    };
    let t: Vec<Entry> = src.into_iter().map(|s| entry(s, wide)).collect();
    if wide {
        env.wide = Some(t);
        publish(env.wide.as_ref(), &WENVIRON);
        republish32(env.wide.as_ref(), true);
    } else {
        env.narrow = Some(t);
        publish(env.narrow.as_ref(), &ENVIRON);
        republish32(env.narrow.as_ref(), false);
    }
}

/// `_initialize_narrow_environment` / `_initialize_wide_environment`.
pub fn init(wide: bool) {
    let mut env = ENV.lock().unwrap();
    ensure(&mut env, wide);
}

pub fn environ_ptr(wide: bool) -> u64 {
    if wide {
        WENVIRON.load(Ordering::SeqCst)
    } else {
        ENVIRON.load(Ordering::SeqCst)
    }
}

/// The environment as a four-byte-slot pointer array, for a 32-bit caller
/// of the legacy CRT (`__getmainargs`).
pub fn environ_ptr32(wide: bool) -> u32 {
    let mut env = ENV.lock().unwrap();
    ensure(&mut env, wide);
    let t = if wide { env.wide.as_ref() } else { env.narrow.as_ref() };
    let var = if wide { &WENVIRON32 } else { &ENVIRON32 };
    publish32(t, var);
    var.load(Ordering::SeqCst) as u32
}

/// Looks `name` up; returns the address of its value.
fn lookup(name: &[u16], wide: bool) -> u64 {
    let mut env = ENV.lock().unwrap();
    ensure(&mut env, wide);
    let t = if wide { env.wide.as_ref() } else { env.narrow.as_ref() };
    let Some(e) = t.and_then(|t| t.iter().find(|e| eq_ci(name_of(&e.text), name))) else { return 0 };
    if wide {
        e.ptr as u64 + (name.len() as u64 + 1) * 2
    } else {
        e.ptr as u64 + narrow(name).len() as u64 + 1
    }
}

/// Sets or (empty value) removes a variable in both tables and in the
/// Win32 environment.
fn set(name: &[u16], value: &[u16]) -> i32 {
    {
        let mut env = ENV.lock().unwrap();
        if env.narrow.is_none() && env.wide.is_none() {
            ensure(&mut env, false);
        }
        let mut text: Vec<u16> = name.to_vec();
        text.push(b'=' as u16);
        text.extend_from_slice(value);
        let e = &mut *env;
        for (wide, t) in [(false, e.narrow.as_mut()), (true, e.wide.as_mut())] {
            let Some(t) = t else { continue };
            let pos = t.iter().position(|e| eq_ci(name_of(&e.text), name));
            if let Some(i) = pos {
                crt_free(t.remove(i).ptr as *mut u8);
            }
            if !value.is_empty() {
                let e = entry(text.clone(), wide);
                match pos {
                    Some(i) => t.insert(i, e),
                    None => t.push(e),
                }
            }
        }
        publish(env.narrow.as_ref(), &ENVIRON);
        publish(env.wide.as_ref(), &WENVIRON);
        republish32(env.narrow.as_ref(), false);
        republish32(env.wide.as_ref(), true);
    }
    let mut n = name.to_vec();
    n.push(0);
    let mut v = value.to_vec();
    v.push(0);
    let vp = if value.is_empty() { core::ptr::null() } else { v.as_ptr() };
    if set_environment_variable(n.as_ptr(), vp) == 0 && !value.is_empty() {
        dosmaperr(get_last_error());
        return -1;
    }
    0
}

k32fn!(fn set_environment_variable = "SetEnvironmentVariableW"(n: *const u16, v: *const u16) -> i32);
k32fn!(fn get_current_directory = "GetCurrentDirectoryW"(size: u32, buf: *mut u16) -> u32);

const MAX_ENV: usize = 32767;

fn widen(s: &[u8]) -> Vec<u16> {
    String::from_utf8_lossy(s).encode_utf16().collect()
}

extern "win64" fn getenv(name: *const u8) -> u64 {
    validate!(!name.is_null(), EINVAL, 0);
    // SAFETY: a NUL-terminated name.
    let n = unsafe { cstr(name) };
    validate!(n.len() < MAX_ENV, EINVAL, 0);
    lookup(&widen(n), false)
}

extern "win64" fn _wgetenv(name: *const u16) -> u64 {
    validate!(!name.is_null(), EINVAL, 0);
    // SAFETY: as above.
    let n = unsafe { wstr(name) };
    validate!(n.len() < MAX_ENV, EINVAL, 0);
    lookup(n, true)
}

extern "win64" fn getenv_s(need: *mut usize, buf: *mut u8, cap: usize, name: *const u8) -> i32 {
    validate!(!need.is_null(), EINVAL, EINVAL);
    // SAFETY: the required-length output was validated.
    unsafe { *need = 0 };
    validate!(!buf.is_null() || cap == 0, EINVAL, EINVAL);
    if !buf.is_null() && cap > 0 {
        // SAFETY: the caller provided a nonempty output buffer.
        unsafe { *buf = 0 };
    }
    validate!(!name.is_null(), EINVAL, EINVAL);
    // SAFETY: a NUL-terminated environment name.
    let name = unsafe { cstr(name) };
    validate!(name.len() < MAX_ENV, EINVAL, EINVAL);
    let name = widen(name);
    // Keep the environment locked until the value has been copied out.
    let mut env = ENV.lock().unwrap();
    ensure(&mut env, false);
    let value = env.narrow.as_ref().and_then(|t| t.iter().find(|e| eq_ci(name_of(&e.text), &name)));
    let Some(value) = value else { return 0 };
    let text = narrow(&value.text[name.len() + 1..]);
    // SAFETY: validated length output.
    unsafe { *need = text.len() + 1 };
    if buf.is_null() {
        return 0;
    }
    if text.len() >= cap {
        return ERANGE;
    }
    // SAFETY: text.len() + 1 <= cap, with ENV held throughout the copy.
    unsafe {
        core::ptr::copy_nonoverlapping(text.as_ptr(), buf, text.len());
        *buf.add(text.len()) = 0;
    }
    0
}

/// "NAME=value" (value empty: remove).
fn putenv(s: Vec<u16>) -> i32 {
    let Some(eq) = s.iter().position(|&c| c == b'=' as u16).filter(|&i| i > 0) else {
        set_errno(EINVAL);
        return -1;
    };
    if s.len() >= MAX_ENV {
        set_errno(EINVAL);
        return -1;
    }
    set(&s[..eq], &s[eq + 1..])
}

extern "win64" fn _putenv(s: *const u8) -> i32 {
    validate!(!s.is_null(), EINVAL, -1);
    // SAFETY: a NUL-terminated string.
    putenv(widen(unsafe { cstr(s) }))
}

extern "win64" fn _wputenv(s: *const u16) -> i32 {
    validate!(!s.is_null(), EINVAL, -1);
    // SAFETY: as above.
    putenv(unsafe { wstr(s) }.to_vec())
}

fn putenv_s(name: Vec<u16>, value: Vec<u16>) -> i32 {
    validate!(!name.is_empty() && !name.contains(&(b'=' as u16)), EINVAL, EINVAL);
    if set(&name, &value) == 0 {
        0
    } else {
        errno()
    }
}

extern "win64" fn _putenv_s(name: *const u8, value: *const u8) -> i32 {
    validate!(!name.is_null() && !value.is_null(), EINVAL, EINVAL);
    // SAFETY: NUL-terminated strings.
    putenv_s(widen(unsafe { cstr(name) }), widen(unsafe { cstr(value) }))
}

extern "win64" fn _wputenv_s(name: *const u16, value: *const u16) -> i32 {
    validate!(!name.is_null() && !value.is_null(), EINVAL, EINVAL);
    // SAFETY: NUL-terminated strings.
    putenv_s(unsafe { wstr(name) }.to_vec(), unsafe { wstr(value) }.to_vec())
}

/// `_dupenv_s` / `_wdupenv_s`: a heap copy of the value (NULL, 0 when
/// the variable does not exist).
fn dupenv<T: Copy + Default + PartialEq>(out: *mut *mut T, len: *mut usize, value: u64) -> i32 {
    if value == 0 {
        return 0;
    }
    let mut n = 0;
    // SAFETY: a NUL-terminated value inside the environment table.
    while unsafe { *(value as *const T).add(n) } != T::default() {
        n += 1;
    }
    let size = (n + 1) * core::mem::size_of::<T>();
    let p = crt_malloc(size) as *mut T;
    if p.is_null() {
        set_errno(ENOMEM);
        return ENOMEM;
    }
    // SAFETY: the value and a fresh block of n + 1 units; out pointers
    // were checked by the caller.
    unsafe {
        core::ptr::copy_nonoverlapping(value as *const T, p, n + 1);
        *out = p;
        if !len.is_null() {
            *len = n + 1;
        }
    }
    0
}

extern "win64" fn _dupenv_s(out: *mut *mut u8, len: *mut usize, name: *const u8) -> i32 {
    validate!(!out.is_null(), EINVAL, EINVAL);
    // SAFETY: validated out pointers.
    unsafe {
        *out = core::ptr::null_mut();
        if !len.is_null() {
            *len = 0;
        }
    }
    validate!(!name.is_null(), EINVAL, EINVAL);
    dupenv(out, len, getenv(name))
}

extern "win64" fn _wdupenv_s(out: *mut *mut u16, len: *mut usize, name: *const u16) -> i32 {
    validate!(!out.is_null(), EINVAL, EINVAL);
    // SAFETY: validated out pointers.
    unsafe {
        *out = core::ptr::null_mut();
        if !len.is_null() {
            *len = 0;
        }
    }
    validate!(!name.is_null(), EINVAL, EINVAL);
    dupenv(out, len, _wgetenv(name))
}

/// `__p__environ`: the address of `_environ`.
pub extern "win64" fn __p__environ() -> u64 {
    ENVIRON.as_ptr() as u64
}

pub extern "win64" fn __p__wenviron() -> u64 {
    WENVIRON.as_ptr() as u64
}

pub fn current_dir() -> Vec<u16> {
    let n = get_current_directory(0, core::ptr::null_mut());
    let mut buf = alloc::vec![0u16; n as usize + 1];
    let k = get_current_directory(buf.len() as u32, buf.as_mut_ptr());
    buf.truncate(k as usize);
    buf
}

fn getcwd<T: Copy + From<u8>>(buf: *mut T, max: i32, text: &[T]) -> *mut T {
    if buf.is_null() {
        let size = (text.len() + 1).max(max.max(0) as usize);
        let p = crt_malloc(size * core::mem::size_of::<T>()) as *mut T;
        if p.is_null() {
            set_errno(ENOMEM);
            return p;
        }
        // SAFETY: a fresh block of at least len + 1 units.
        unsafe {
            core::ptr::copy_nonoverlapping(text.as_ptr(), p, text.len());
            *p.add(text.len()) = T::from(0);
        }
        return p;
    }
    validate!(max > 0, EINVAL, core::ptr::null_mut());
    if text.len() >= max as usize {
        set_errno(ERANGE);
        return core::ptr::null_mut();
    }
    // SAFETY: the caller's buffer of `max` units.
    unsafe {
        core::ptr::copy_nonoverlapping(text.as_ptr(), buf, text.len());
        *buf.add(text.len()) = T::from(0);
    }
    buf
}

extern "win64" fn _wgetcwd(buf: *mut u16, max: i32) -> *mut u16 {
    getcwd(buf, max, &current_dir())
}

extern "win64" fn _getcwd(buf: *mut u8, max: i32) -> *mut u8 {
    getcwd(buf, max, &narrow(&current_dir()))
}

extern "win64" fn _wgetdcwd(drive: i32, buf: *mut u16, max: i32) -> *mut u16 {
    if !(0..=26).contains(&drive) {
        ptd().doserrno = 15;
        set_errno(EACCES);
        invalid_parameter();
        return core::ptr::null_mut();
    }
    // OSyx exposes only C:. Drive 0 means the current drive.
    if drive != 0 && drive != 3 {
        ptd().doserrno = 15; // ERROR_INVALID_DRIVE
        set_errno(EACCES);
        return core::ptr::null_mut();
    }
    _wgetcwd(buf, max)
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        getenv,
        _wgetenv,
        _putenv,
        _wputenv,
        _putenv_s,
        _wputenv_s,
        _dupenv_s,
        _wdupenv_s,
        __p__environ,
        __p__wenviron,
        _wgetcwd,
        _getcwd,
        _wgetdcwd,
        getenv_s,
    ]
}
