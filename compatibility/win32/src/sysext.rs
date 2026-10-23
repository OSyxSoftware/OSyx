//! Process/system queries mozglue needs at start-up: calendar time,
//! interrupt time, process times, version verification, pointer probes,
//! path search and message formatting.
#![allow(non_snake_case)]
use crate::{
    error::*,
    kernel32::{self, fail},
    mem, objects, path, teb,
};
use alloc::{
    string::{String, ToString},
    vec::Vec,
};
use core::sync::atomic::{AtomicU64, Ordering};
use os_std::sync::Mutex;

/// Wall-clock time the process started (ns since 1970), set by init_process.
static START_NS: AtomicU64 = AtomicU64::new(0);

pub fn mark_start() {
    START_NS.store(os_sys::clock(osyx_abi::CLOCK_REALTIME).unwrap_or(0), Ordering::Relaxed);
}

// ---- calendar time ----------------------------------------------------------------

/// Days since 1970-01-01 → (year, month, day), proleptic Gregorian.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// SYSTEMTIME: year, month, day of week, day, hour, minute, second, ms.
fn system_time(ft: u64) -> [u16; 8] {
    let ms_total = ft / 10_000;
    let days = (ms_total / 86_400_000) as i64 - 134_774; // 1601 → 1970
    let ms_day = ms_total % 86_400_000;
    let (y, m, d) = civil(days);
    let weekday = (days + 4).rem_euclid(7) as u16; // 1970-01-01 was a Thursday
    [
        y as u16,
        m as u16,
        weekday,
        d as u16,
        (ms_day / 3_600_000) as u16,
        (ms_day / 60_000 % 60) as u16,
        (ms_day / 1000 % 60) as u16,
        (ms_day % 1000) as u16,
    ]
}

fn now_filetime() -> u64 {
    kernel32::filetime(os_sys::clock(osyx_abi::CLOCK_REALTIME).unwrap_or(0) as i64)
}

extern "win64" fn GetSystemTime(out: *mut [u16; 8]) {
    if !out.is_null() {
        // SAFETY: the caller's SYSTEMTIME.
        unsafe { *out = system_time(now_filetime()) };
    }
}

extern "win64" fn SystemTimeToFileTime(st: *const [u16; 8], out: *mut u64) -> i32 {
    if st.is_null() || out.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: the caller's SYSTEMTIME.
    let [y, m, _, d, h, min, s, ms] = unsafe { *st };
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let month_days = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if !(1601..=30827).contains(&y) || !(1..=12).contains(&m) || d == 0 || d > month_days[m as usize - 1] || h > 23 || min > 59 || s > 59 || ms > 999 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let days = days_from_civil(y as i64, m as u32, d as u32) + 134_774;
    let ms_total = days as u64 * 86_400_000 + h as u64 * 3_600_000 + min as u64 * 60_000 + s as u64 * 1000 + ms as u64;
    // SAFETY: the caller's FILETIME.
    unsafe { core::ptr::write_unaligned(out, ms_total * 10_000) };
    1
}

/// Interrupt time in 100 ns units. OSyx's monotonic clock does not advance
/// while the machine is suspended, so it is already unbiased.
extern "win64" fn QueryUnbiasedInterruptTime(out: *mut u64) -> i32 {
    if out.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: the caller's out pointer.
    unsafe { *out = os_sys::clock(osyx_abi::CLOCK_MONOTONIC).unwrap_or(0) / 100 };
    1
}

extern "win64" fn QueryUnbiasedInterruptTimePrecise(out: *mut u64) {
    if !out.is_null() {
        // SAFETY: as above.
        unsafe { *out = os_sys::clock(osyx_abi::CLOCK_MONOTONIC).unwrap_or(0) / 100 };
    }
}

/// Creation time is when winrun started the process. CPU time comes from
/// the kernel's per-process scheduler samples (100 Hz); OSyx does not
/// separate kernel from user time, so it is all reported as user time.
extern "win64" fn GetProcessTimes(process: u64, creation: *mut u64, exit: *mut u64, kernel: *mut u64, user: *mut u64) -> i32 {
    if process != objects::CURRENT_PROCESS {
        return fail(ERROR_NOT_SUPPORTED, 0);
    }
    if creation.is_null() || exit.is_null() || kernel.is_null() || user.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let mut list = alloc::vec![osyx_abi::ipc::ProcInfo::default(); 256];
    let n = match os_sys::proc_list(&mut list) {
        Ok(n) => n,
        Err(e) => return fail(errno_to_win32(e), 0),
    };
    let me = kernel32::pid();
    let Some(info) = list[..n].iter().find(|p| p.pid == me) else { return fail(ERROR_GEN_FAILURE, 0) };
    // SAFETY: the caller's FILETIMEs.
    unsafe {
        core::ptr::write_unaligned(creation, kernel32::filetime(START_NS.load(Ordering::Relaxed) as i64));
        core::ptr::write_unaligned(exit, 0);
        core::ptr::write_unaligned(kernel, 0);
        core::ptr::write_unaligned(user, info.cpu_ticks * 100_000);
    }
    1
}

// ---- version verification ---------------------------------------------------------

const VER_MINORVERSION: u32 = 0x1;
const VER_MAJORVERSION: u32 = 0x2;
const VER_BUILDNUMBER: u32 = 0x4;
const VER_PLATFORMID: u32 = 0x8;
const VER_SERVICEPACKMINOR: u32 = 0x10;
const VER_SERVICEPACKMAJOR: u32 = 0x20;
const VER_SUITENAME: u32 = 0x40;
const VER_PRODUCT_TYPE: u32 = 0x80;
const ERROR_OLD_WIN_VERSION: u32 = 1150;

/// The version GetVersionExW reports (kernel32.rs): 10.0.19041,
/// VER_PLATFORM_WIN32_NT, no service pack, workstation, no suites.
const OS: [u32; 8] = [0, 10, 19041, 2, 0, 0, 0, 1];

/// Bit index of a VER_* type → its slot in the condition mask.
fn slot(type_bit: u32) -> Option<usize> {
    [VER_MINORVERSION, VER_MAJORVERSION, VER_BUILDNUMBER, VER_PLATFORMID, VER_SERVICEPACKMINOR, VER_SERVICEPACKMAJOR, VER_SUITENAME, VER_PRODUCT_TYPE]
        .iter()
        .position(|&b| b == type_bit)
}

extern "win64" fn VerSetConditionMask(mask: u64, type_mask: u32, condition: u8) -> u64 {
    // Like Windows, one type per call: the highest-priority bit given.
    let order = [VER_PRODUCT_TYPE, VER_SUITENAME, VER_SERVICEPACKMAJOR, VER_SERVICEPACKMINOR, VER_PLATFORMID, VER_BUILDNUMBER, VER_MAJORVERSION, VER_MINORVERSION];
    match order.iter().find(|&&b| type_mask & b != 0).and_then(|&b| slot(b)) {
        // Windows also sets bit 63 in every mask it builds.
        Some(i) => mask | 1 << 63 | ((condition as u64 & 7) << (i * 3)),
        None => mask,
    }
}

fn compare(have: u32, want: u32, condition: u64) -> Option<bool> {
    Some(match condition {
        1 => have == want,
        2 => have > want,
        3 => have >= want,
        4 => have < want,
        5 => have <= want,
        _ => return None,
    })
}

extern "win64" fn VerifyVersionInfoW(info: *const u8, type_mask: u32, mask: u64) -> i32 {
    if info.is_null() || type_mask == 0 || mask == 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: the caller's OSVERSIONINFOEXW (284 bytes).
    let want = unsafe {
        let size: u32 = teb::get(info as u64, 0);
        if size < 284 {
            return fail(ERROR_INVALID_PARAMETER, 0);
        }
        let g32 = |o: usize| teb::get::<u32>(info as u64, o);
        let g16 = |o: usize| teb::get::<u16>(info as u64, o) as u32;
        [g32(8), g32(4), g32(12), g32(16), g16(278), g16(276), g16(280), teb::get::<u8>(info as u64, 282) as u32]
    };
    let cond = |i: usize| (mask >> (i * 3)) & 7;
    // Major, minor and service pack compare as one version tuple.
    let tuple = [(1usize, VER_MAJORVERSION), (0, VER_MINORVERSION), (5, VER_SERVICEPACKMAJOR), (4, VER_SERVICEPACKMINOR)];
    let mut ok = true;
    let mut decided = false;
    for &(i, bit) in &tuple {
        if decided || type_mask & bit == 0 {
            continue;
        }
        let Some(r) = compare(OS[i], want[i], cond(i)) else { return fail(ERROR_INVALID_PARAMETER, 0) };
        if OS[i] != want[i] {
            ok = r;
            decided = true;
        } else if !r {
            ok = false;
            decided = true;
        }
    }
    for (i, bit) in [(2usize, VER_BUILDNUMBER), (3, VER_PLATFORMID), (7, VER_PRODUCT_TYPE)] {
        if type_mask & bit != 0 {
            match compare(OS[i], want[i], cond(i)) {
                Some(r) => ok &= r,
                None => return fail(ERROR_INVALID_PARAMETER, 0),
            }
        }
    }
    if type_mask & VER_SUITENAME != 0 {
        ok &= match cond(6) {
            6 => OS[6] & want[6] == want[6], // VER_AND: all requested suites
            7 => OS[6] & want[6] != 0,       // VER_OR: any of them
            _ => return fail(ERROR_INVALID_PARAMETER, 0),
        };
    }
    if ok { 1 } else { fail(ERROR_OLD_WIN_VERSION, 0) }
}

// ---- pointer probes ---------------------------------------------------------------

/// A pipe the kernel copies probe bytes through: a write from an unmapped
/// or unreadable address fails with EFAULT instead of faulting here.
static PROBE: Mutex<Option<(u64, u64)>> = Mutex::new(None);

fn readable(addr: u64) -> bool {
    let mut g = PROBE.lock().unwrap();
    if g.is_none() {
        *g = os_sys::pipe().ok();
    }
    let Some((r, w)) = *g else { return false };
    if os_sys::write_raw(w, addr as usize, 1) != Ok(1) {
        return false;
    }
    let mut b = 0u8;
    let _ = os_sys::read_raw(r, &mut b as *mut u8 as usize, 1);
    true
}

extern "win64" fn IsBadReadPtr(p: u64, len: usize) -> i32 {
    if len == 0 {
        return 0;
    }
    if p == 0 {
        return 1;
    }
    let Some(last) = p.checked_add(len as u64 - 1) else { return 1 };
    // PAGE_NOACCESS memory is mapped readable on OSyx (mem.rs): consult
    // the recorded protection first.
    let mut page = p & !(mem::PAGE as u64 - 1);
    while page <= last {
        if let Some(info) = mem::virtual_query(page) {
            if info.state != mem::MEM_COMMIT || info.protect & (mem::PAGE_NOACCESS | mem::PAGE_GUARD) != 0 {
                return 1;
            }
        }
        if !readable(page.max(p)) {
            return 1;
        }
        page += mem::PAGE as u64;
    }
    i32::from(!readable(last))
}

/// Windows Error Reporting is not part of OSyx: there is no out-of-process
/// crash handler to register with.
extern "win64" fn WerRegisterRuntimeExceptionModule(_dll: *const u16, _context: u64) -> u32 {
    0x8000_4001 // E_NOTIMPL
}

extern "win64" fn WerUnregisterRuntimeExceptionModule(_dll: *const u16, _context: u64) -> u32 {
    0x8000_4001
}

// ---- SearchPath -------------------------------------------------------------------

fn exists(win: &str) -> Option<String> {
    let cwd = os_std::env::current_dir().map(|p| path::to_windows(p.as_str())).unwrap_or_else(|_| String::from("C:\\"));
    let osyx = path::to_osyx(win, &cwd).ok()?;
    match path::resolve(&osyx) {
        Ok((p, true)) => Some(path::to_windows(&p)),
        _ => None,
    }
}

extern "win64" fn SearchPathW(dirs: *const u16, file: *const u16, ext: *const u16, size: u32, buf: *mut u16, part: *mut *mut u16) -> u32 {
    // SAFETY: NULL or NUL-terminated strings from the caller.
    let (dirs, file, ext) = unsafe { (path::wide_to_string(dirs), path::wide_to_string(file), path::wide_to_string(ext)) };
    let Some(mut file) = file.filter(|f| !f.is_empty()) else { return fail(ERROR_INVALID_PARAMETER, 0) };
    let leaf = file.rsplit(['\\', '/']).next().unwrap_or(&file);
    if let Some(e) = ext {
        if !leaf.contains('.') {
            file.push_str(&e);
        }
    }
    let explicit = file.contains(['\\', '/']) || file.as_bytes().get(1) == Some(&b':');
    let found = if explicit {
        exists(&file)
    } else {
        let list: Vec<String> = match dirs {
            Some(d) => d.split(';').filter(|s| !s.is_empty()).map(String::from).collect(),
            None => {
                let mut v = Vec::new();
                if let Some(exe) = kernel32::module_name(0) {
                    if let Some((dir, _)) = exe.rsplit_once('\\') {
                        v.push(String::from(dir));
                    }
                }
                v.push(String::from("."));
                v.push(String::from("C:\\Windows\\System32"));
                v.push(String::from("C:\\Windows"));
                if let Some(p) = kernel32::env_get("PATH") {
                    v.extend(p.split(';').filter(|s| !s.is_empty()).map(String::from));
                }
                v
            }
        };
        list.iter().find_map(|d| exists(&alloc::format!("{}\\{}", d.trim_end_matches('\\'), file)))
    };
    let Some(full) = found else { return fail(ERROR_FILE_NOT_FOUND, 0) };
    let units: Vec<u16> = full.encode_utf16().collect();
    if (size as usize) <= units.len() || buf.is_null() {
        return units.len() as u32 + 1;
    }
    // SAFETY: the caller's buffer of `size` units.
    unsafe {
        core::ptr::copy_nonoverlapping(units.as_ptr(), buf, units.len());
        *buf.add(units.len()) = 0;
        if !part.is_null() {
            let at = units.iter().rposition(|&c| c == b'\\' as u16).map_or(0, |i| i + 1);
            *part = buf.add(at);
        }
    }
    teb::set_last_error(ERROR_SUCCESS);
    units.len() as u32
}

// ---- FormatMessage ----------------------------------------------------------------

const ALLOCATE_BUFFER: u32 = 0x100;
const IGNORE_INSERTS: u32 = 0x200;
const FROM_STRING: u32 = 0x400;
const FROM_HMODULE: u32 = 0x800;
const FROM_SYSTEM: u32 = 0x1000;
const ARGUMENT_ARRAY: u32 = 0x2000;
const ERROR_MR_MID_NOT_FOUND: u32 = 317;

/// The system message table entries for the errors OSyx's runtime reports.
fn system_message(id: u32) -> Option<&'static str> {
    Some(match id {
        0 => "The operation completed successfully.\r\n",
        1 => "Incorrect function.\r\n",
        2 => "The system cannot find the file specified.\r\n",
        3 => "The system cannot find the path specified.\r\n",
        4 => "The system cannot open the file.\r\n",
        5 => "Access is denied.\r\n",
        6 => "The handle is invalid.\r\n",
        8 => "Not enough memory resources are available to process this command.\r\n",
        14 => "Not enough memory resources are available to complete this operation.\r\n",
        17 => "The system cannot move the file to a different disk drive.\r\n",
        19 => "The media is write protected.\r\n",
        31 => "A device attached to the system is not functioning.\r\n",
        32 => "The process cannot access the file because it is being used by another process.\r\n",
        38 => "Reached the end of the file.\r\n",
        50 => "The request is not supported.\r\n",
        80 => "The file exists.\r\n",
        87 => "The parameter is incorrect.\r\n",
        112 => "There is not enough space on the disk.\r\n",
        120 => "This function is not supported on this system.\r\n",
        122 => "The data area passed to a system call is too small.\r\n",
        123 => "The filename, directory name, or volume label syntax is incorrect.\r\n",
        126 => "The specified module could not be found.\r\n",
        127 => "The specified procedure could not be found.\r\n",
        145 => "The directory is not empty.\r\n",
        183 => "Cannot create a file when that file already exists.\r\n",
        193 => "%1 is not a valid Win32 application.\r\n",
        203 => "The system could not find the environment option that was entered.\r\n",
        206 => "The filename or extension is too long.\r\n",
        259 => "No more data is available.\r\n",
        267 => "The directory name is invalid.\r\n",
        487 => "Attempt to access invalid address.\r\n",
        1114 => "A dynamic link library (DLL) initialization routine failed.\r\n",
        1150 => "The specified program requires a newer version of Windows.\r\n",
        1460 => "This operation returned because the timeout period expired.\r\n",
        _ => return None,
    })
}

/// Expands `%n`-style inserts. `args` is the va_list / argument array
/// (one 8-byte slot per argument on x64); `wide` selects the default
/// string width of `!s!`.
fn expand(text: &str, flags: u32, args: *const u64, wide: bool) -> Result<String, u32> {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    let width = flags & 0xff;
    while let Some(c) = chars.next() {
        if c != '%' {
            if width == 0xff && (c == '\r' || c == '\n') {
                if c == '\n' || chars.peek() != Some(&'\n') {
                    out.push(' ');
                }
                continue;
            }
            out.push(c);
            continue;
        }
        let Some(&n) = chars.peek() else { break };
        match n {
            '0' => break,
            'n' => {
                chars.next();
                out.push_str("\r\n");
            }
            't' => {
                chars.next();
                out.push('\t');
            }
            'r' => {
                chars.next();
                out.push('\r');
            }
            '%' | '.' | '!' | ' ' => {
                chars.next();
                out.push(n);
            }
            '1'..='9' => {
                let mut index = 0usize;
                while let Some(d) = chars.peek().and_then(|c| c.to_digit(10)) {
                    index = index * 10 + d as usize;
                    chars.next();
                }
                let mut spec = String::new();
                if chars.peek() == Some(&'!') {
                    chars.next();
                    for s in chars.by_ref() {
                        if s == '!' {
                            break;
                        }
                        spec.push(s);
                    }
                }
                if flags & IGNORE_INSERTS != 0 {
                    out.push('%');
                    out.push_str(&index.to_string());
                    if !spec.is_empty() {
                        out.push('!');
                        out.push_str(&spec);
                        out.push('!');
                    }
                    continue;
                }
                if args.is_null() || index > 99 {
                    return Err(ERROR_INVALID_PARAMETER);
                }
                // SAFETY: the caller supplies at least `index` arguments.
                let v = unsafe { *args.add(index - 1) };
                out.push_str(&format_arg(v, if spec.is_empty() { "s" } else { &spec }, wide)?);
            }
            _ => {
                chars.next();
                out.push(n);
            }
        }
    }
    Ok(out)
}

/// One printf conversion of an insert: s/S strings, d/i/u/x/X/c, with an
/// optional l/h/I64 size prefix and a width.
fn format_arg(v: u64, spec: &str, wide: bool) -> Result<String, u32> {
    let conv = spec.chars().last().ok_or(ERROR_INVALID_PARAMETER)?;
    let body = &spec[..spec.len() - 1];
    let pad: usize = body.trim_start_matches(['-', '0']).trim_end_matches(['l', 'h', 'w']).trim_end_matches("I64").parse().unwrap_or(0);
    let left = body.starts_with('-');
    let zero = body.starts_with('0');
    let long64 = body.contains("I64") || body.contains("ll");
    let text = match conv {
        's' | 'S' => {
            let wide_str = (conv == 's') == wide || body.contains(['l', 'w']);
            // SAFETY: the argument is a NUL-terminated string of that width.
            unsafe {
                if wide_str { path::wide_to_string(v as *const u16) } else { path::ansi_to_string(v as *const u8) }
            }
            .unwrap_or_else(|| String::from("(null)"))
        }
        'd' | 'i' => if long64 { (v as i64).to_string() } else { (v as i32).to_string() },
        'u' => if long64 { v.to_string() } else { (v as u32).to_string() },
        'x' => if long64 { alloc::format!("{v:x}") } else { alloc::format!("{:x}", v as u32) },
        'X' => if long64 { alloc::format!("{v:X}") } else { alloc::format!("{:X}", v as u32) },
        'c' | 'C' => char::from_u32(v as u32).map(String::from).unwrap_or_default(),
        'p' => alloc::format!("{v:016X}"),
        _ => return Err(ERROR_INVALID_PARAMETER),
    };
    let len = text.chars().count();
    if len >= pad {
        return Ok(text);
    }
    let fill: String = core::iter::repeat_n(if zero && !left { '0' } else { ' ' }, pad - len).collect();
    Ok(if left { text + &fill } else { fill + &text })
}

/// The message text, before inserts.
fn source_text(flags: u32, source: u64, id: u32, wide: bool) -> Result<String, u32> {
    if flags & FROM_STRING != 0 {
        // SAFETY: the caller's NUL-terminated format string.
        let s = unsafe { if wide { path::wide_to_string(source as *const u16) } else { path::ansi_to_string(source as *const u8) } };
        return s.ok_or(ERROR_INVALID_PARAMETER);
    }
    if flags & FROM_HMODULE != 0 && source != 0 {
        // Message-table resources of PE modules are not parsed yet.
        return Err(ERROR_NOT_SUPPORTED);
    }
    if flags & (FROM_SYSTEM | FROM_HMODULE) == 0 {
        return Err(ERROR_INVALID_PARAMETER);
    }
    let id = if id & 0xffff_0000 == 0x8007_0000 { id & 0xffff } else { id }; // HRESULT_FROM_WIN32
    system_message(id).map(String::from).ok_or(ERROR_MR_MID_NOT_FOUND)
}

/// Shared body of FormatMessageA/W; writes `units` (bytes or UTF-16).
#[allow(clippy::too_many_arguments)]
fn format_message(flags: u32, source: u64, id: u32, lang: u32, buf: u64, size: u32, args: u64, wide: bool) -> u32 {
    if lang != 0 && lang & 0x3ff != 0x09 {
        return fail(1815, 0); // ERROR_RESOURCE_LANG_NOT_FOUND: only English text
    }
    let args = if args == 0 {
        core::ptr::null()
    } else if flags & ARGUMENT_ARRAY != 0 {
        args as *const u64
    } else {
        // A va_list on x64 is a pointer to the next argument slot.
        // SAFETY: the caller's va_list.
        unsafe { *(args as *const u64) as *const u64 }
    };
    let text = match source_text(flags, source, id, wide).and_then(|t| expand(&t, flags, args, wide)) {
        Ok(t) => t,
        Err(e) => return fail(e, 0),
    };
    let units: Vec<u16> = text.encode_utf16().collect();
    let count = if wide { units.len() } else { text.len() };
    let unit = if wide { 2 } else { 1 };
    let dst = if flags & ALLOCATE_BUFFER != 0 {
        let Some(p) = kernel32::heap_alloc((count.max(size as usize) + 1) * unit, false) else {
            return fail(ERROR_NOT_ENOUGH_MEMORY, 0);
        };
        // SAFETY: `buf` is the caller's pointer-sized out parameter.
        unsafe { *(buf as *mut u64) = p };
        p
    } else {
        if buf == 0 || count + 1 > size as usize {
            return fail(ERROR_INSUFFICIENT_BUFFER, 0);
        }
        buf
    };
    // SAFETY: `dst` holds count + 1 units.
    unsafe {
        if wide {
            core::ptr::copy_nonoverlapping(units.as_ptr(), dst as *mut u16, count);
            *(dst as *mut u16).add(count) = 0;
        } else {
            core::ptr::copy_nonoverlapping(text.as_ptr(), dst as *mut u8, count);
            *(dst as *mut u8).add(count) = 0;
        }
    }
    count as u32
}

extern "win64" fn FormatMessageA(flags: u32, source: u64, id: u32, lang: u32, buf: u64, size: u32, args: u64) -> u32 {
    format_message(flags, source, id, lang, buf, size, args, false)
}

extern "win64" fn FormatMessageW(flags: u32, source: u64, id: u32, lang: u32, buf: u64, size: u32, args: u64) -> u32 {
    format_message(flags, source, id, lang, buf, size, args, true)
}

/// ntdll is VerSetConditionMask's real home; kernel32's forwards to it.
pub fn ntdll_exports() -> Vec<(&'static str, u64)> {
    export_table![VerSetConditionMask]
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        GetSystemTime,
        SystemTimeToFileTime,
        QueryUnbiasedInterruptTime,
        QueryUnbiasedInterruptTimePrecise,
        GetProcessTimes,
        VerSetConditionMask,
        VerifyVersionInfoW,
        IsBadReadPtr,
        WerRegisterRuntimeExceptionModule,
        WerUnregisterRuntimeExceptionModule,
        SearchPathW,
        FormatMessageA,
        FormatMessageW,
    ]
}
