//! shell32 known folders. The user profile is $HOME, with the Windows
//! layout below it (AppData\Roaming, AppData\Local, Documents, ...); the
//! machine folders are the fixed C:\ paths the rest of winrun reports
//! (GetWindowsDirectory). A folder that does not exist is reported as
//! missing unless the caller asks for it to be created (KF_FLAG_CREATE /
//! CSIDL_FLAG_CREATE) or not verified (KF_FLAG_DONT_VERIFY). Folders of
//! other users (a token other than the caller's) are not supported.
#![allow(non_snake_case)]
use crate::{kernel32, path};
use alloc::{format, string::String, vec::Vec};

const S_OK: u32 = 0;
const E_INVALIDARG: u32 = 0x8007_0057;
const E_OUTOFMEMORY: u32 = 0x8007_000e;
const HRESULT_FILE_NOT_FOUND: u32 = 0x8007_0002;
const HRESULT_NOT_SUPPORTED: u32 = 0x8007_0032;

const fn hresult(win32: u32) -> u32 {
    if win32 == 0 { S_OK } else { 0x8007_0000 | (win32 & 0xffff) }
}

#[derive(Clone, Copy)]
enum Base {
    /// Relative to the user profile ("" is the profile itself).
    Profile(&'static str),
    /// A fixed machine path.
    Machine(&'static str),
}

/// (FOLDERID as its GUID fields, CSIDL or 0, location).
type Folder = ((u32, u16, u16, [u8; 8]), u32, Base);

const FOLDERS: &[Folder] = &[
    ((0x5E6C858F, 0x0E22, 0x4760, [0x9A, 0xFE, 0xEA, 0x33, 0x17, 0xB6, 0x71, 0x73]), 0x28, Base::Profile("")),
    ((0x3EB685DB, 0x65F9, 0x4CF6, [0xA0, 0x3A, 0xE3, 0xEF, 0x65, 0x72, 0x9F, 0x3D]), 0x1A, Base::Profile("AppData\\Roaming")),
    ((0xF1B32785, 0x6FBA, 0x4FCF, [0x9D, 0x55, 0x7B, 0x8E, 0x7F, 0x15, 0x70, 0x91]), 0x1C, Base::Profile("AppData\\Local")),
    ((0xA520A1A4, 0x1780, 0x4FF6, [0xBD, 0x18, 0x16, 0x73, 0x43, 0xC5, 0xAF, 0x16]), 0, Base::Profile("AppData\\LocalLow")),
    ((0xB4BFCC3A, 0xDB2C, 0x424C, [0xB0, 0x29, 0x7F, 0xE9, 0x9A, 0x87, 0xC6, 0x41]), 0x10, Base::Profile("Desktop")),
    ((0xFDD39AD0, 0x238F, 0x46AF, [0xAD, 0xB4, 0x6C, 0x85, 0x48, 0x03, 0x69, 0xC7]), 0x05, Base::Profile("Documents")),
    ((0x374DE290, 0x123F, 0x4565, [0x91, 0x64, 0x39, 0xC4, 0x92, 0x5E, 0x46, 0x7B]), 0, Base::Profile("Downloads")),
    ((0x33E28130, 0x4E1E, 0x4676, [0x83, 0x5A, 0x98, 0x39, 0x5C, 0x3B, 0xC3, 0xBB]), 0x27, Base::Profile("Pictures")),
    ((0x4BD8D571, 0x6D19, 0x48D3, [0xBE, 0x97, 0x42, 0x22, 0x20, 0x08, 0x0E, 0x43]), 0x0D, Base::Profile("Music")),
    ((0x18989B1D, 0x99B5, 0x455B, [0x84, 0x1C, 0xAB, 0x7C, 0x74, 0xE4, 0xDD, 0xFC]), 0x0E, Base::Profile("Videos")),
    ((0x5CD7AEE2, 0x2219, 0x4A67, [0xB8, 0x5D, 0x6C, 0x9C, 0xE1, 0x56, 0x60, 0xCB]), 0, Base::Profile("AppData\\Local\\Programs")),
    ((0x62AB5D82, 0xFDC1, 0x4DC3, [0xA9, 0xDD, 0x07, 0x0D, 0x1D, 0x49, 0x5D, 0x97]), 0x23, Base::Machine("C:\\ProgramData")),
    ((0xF38BF404, 0x1D43, 0x42F2, [0x93, 0x05, 0x67, 0xDE, 0x0B, 0x28, 0xFC, 0x23]), 0x24, Base::Machine("C:\\Windows")),
    ((0x1AC14E77, 0x02E7, 0x4E5D, [0xB7, 0x44, 0x2E, 0xB1, 0xAE, 0x51, 0x98, 0xB7]), 0x25, Base::Machine("C:\\Windows\\System32")),
    ((0xFD228CB7, 0xAE11, 0x4AE3, [0x86, 0x4C, 0x16, 0xF3, 0x91, 0x0A, 0xB8, 0xFE]), 0x14, Base::Machine("C:\\Windows\\Fonts")),
    ((0x905E63B6, 0xC1BF, 0x494E, [0xB2, 0x9C, 0x65, 0xB7, 0x32, 0xD3, 0xD2, 0x1A]), 0x26, Base::Machine("C:\\Program Files")),
    ((0x6D809377, 0x6AF0, 0x444B, [0x89, 0x57, 0xA3, 0x77, 0x3F, 0x02, 0x20, 0x0E]), 0, Base::Machine("C:\\Program Files")),
];

/// The profile directory as a Windows path.
fn profile() -> String {
    let home = os_std::env::var("HOME").unwrap_or_else(|_| String::from("/home/user"));
    path::to_windows(home.trim_end_matches('/'))
}

fn location(base: Base) -> String {
    match base {
        Base::Profile("") => profile(),
        Base::Profile(rel) => format!("{}\\{rel}", profile()),
        Base::Machine(p) => String::from(p),
    }
}

/// Creates the directory and its missing parents.
fn create_all(win: &str) -> Result<(), u32> {
    let full = path::to_osyx(win, &kernel32::cwd_win())?;
    let mut at = String::new();
    for part in full.split('/').filter(|p| !p.is_empty()) {
        at.push('/');
        at.push_str(part);
        match os_sys::mkdir(&at, 0o755) {
            Ok(()) | Err(osyx_abi::Errno::AlreadyExists) => {}
            Err(e) => return Err(crate::error::errno_to_win32(e)),
        }
    }
    Ok(())
}

/// Whether the folder exists as a directory.
fn exists(win: &str) -> Result<bool, u32> {
    match kernel32::locate(win) {
        Ok((p, true)) => Ok(os_sys::stat(&p).is_ok_and(|s| s.kind == osyx_abi::KIND_DIR)),
        Ok((_, false)) | Err(3) => Ok(false), // ERROR_PATH_NOT_FOUND: a parent is missing
        Err(e) => Err(e),
    }
}

/// Resolves a folder: Ok(path) or Err(HRESULT).
fn resolve(base: Base, create: bool, verify: bool) -> Result<String, u32> {
    let p = location(base);
    if create {
        create_all(&p).map_err(hresult)?;
    } else if verify && !exists(&p).map_err(hresult)? {
        return Err(HRESULT_FILE_NOT_FOUND);
    }
    Ok(p)
}

const KF_FLAG_DONT_VERIFY: u32 = 0x4000;
const KF_FLAG_CREATE: u32 = 0x8000;
/// Flags with no effect here: no folder redirection, aliases or
/// unexpanded environment strings exist, so the default path is the path.
const KF_FLAGS_NO_EFFECT: u32 = 0x0000_0200 | 0x0000_0400 | 0x0000_0800 | 0x0000_1000 | 0x0000_2000;

/// A token other than the caller's (NULL) would name another user.
fn token_ok(token: u64) -> bool {
    token == 0
}

extern "win64" fn SHGetKnownFolderPath(id: *const u8, flags: u32, token: u64, out: *mut u64) -> u32 {
    if out.is_null() {
        return E_INVALIDARG;
    }
    // SAFETY: the caller's out pointer; NULL on every failure, as Windows does.
    unsafe { *out = 0 };
    if id.is_null() || flags & !(KF_FLAG_DONT_VERIFY | KF_FLAG_CREATE | KF_FLAGS_NO_EFFECT) != 0 {
        return E_INVALIDARG;
    }
    if !token_ok(token) {
        return HRESULT_NOT_SUPPORTED;
    }
    // SAFETY: the caller's KNOWNFOLDERID (a GUID).
    let guid = unsafe {
        (
            core::ptr::read_unaligned(id as *const u32),
            core::ptr::read_unaligned(id.add(4) as *const u16),
            core::ptr::read_unaligned(id.add(6) as *const u16),
            core::ptr::read_unaligned(id.add(8) as *const [u8; 8]),
        )
    };
    let Some(&(_, _, base)) = FOLDERS.iter().find(|f| f.0 == guid) else { return HRESULT_FILE_NOT_FOUND };
    let p = match resolve(base, flags & KF_FLAG_CREATE != 0, flags & KF_FLAG_DONT_VERIFY == 0) {
        Ok(p) => p,
        Err(hr) => return hr,
    };
    let units: Vec<u16> = p.encode_utf16().chain(core::iter::once(0)).collect();
    let block = crate::ole32::CoTaskMemAlloc(units.len() * 2);
    if block == 0 {
        return E_OUTOFMEMORY;
    }
    // SAFETY: a fresh block of units.len() units; the caller's out pointer.
    unsafe {
        core::ptr::copy_nonoverlapping(units.as_ptr(), block as *mut u16, units.len());
        *out = block;
    }
    S_OK
}

const CSIDL_FLAG_CREATE: u32 = 0x8000;
const CSIDL_FLAG_DONT_VERIFY: u32 = 0x4000;
const MAX_PATH: usize = 260;

extern "win64" fn SHGetFolderPathW(_hwnd: u64, csidl: u32, token: u64, kind: u32, out: *mut u16) -> u32 {
    if out.is_null() || kind > 1 {
        return E_INVALIDARG; // SHGFP_TYPE_CURRENT (0) and SHGFP_TYPE_DEFAULT (1) agree here
    }
    // SAFETY: the caller's MAX_PATH buffer; empty on failure.
    unsafe { *out = 0 };
    if !token_ok(token) {
        return HRESULT_NOT_SUPPORTED;
    }
    // CSIDL_DESKTOP (0) resolves to the desktop directory (0x10) as on Windows.
    let id = match csidl & !(CSIDL_FLAG_CREATE | CSIDL_FLAG_DONT_VERIFY) {
        0 => 0x10,
        id => id,
    };
    let Some(&(_, _, base)) = FOLDERS.iter().find(|f| f.1 == id) else { return E_INVALIDARG };
    let p = match resolve(base, csidl & CSIDL_FLAG_CREATE != 0, csidl & CSIDL_FLAG_DONT_VERIFY == 0) {
        Ok(p) => p,
        Err(hr) => return hr,
    };
    let units: Vec<u16> = p.encode_utf16().collect();
    if units.len() >= MAX_PATH {
        return hresult(206); // ERROR_FILENAME_EXCED_RANGE
    }
    // SAFETY: as above; units.len() < MAX_PATH.
    unsafe {
        core::ptr::copy_nonoverlapping(units.as_ptr(), out, units.len());
        *out.add(units.len()) = 0;
    }
    S_OK
}

/// Splits a Windows command line into arguments with the rules
/// `CommandLineToArgvW` documents: whitespace separates, double quotes group
/// (and a doubled quote inside them is one quote), and 2n backslashes before
/// a quote become n backslashes with the quote taking effect, 2n+1 become n
/// backslashes and a literal quote.
fn split_command_line(s: &[u16]) -> Vec<Vec<u16>> {
    let is_space = |c: u16| c == 0x20 || c == 0x09;
    let mut args = Vec::new();
    let mut i = 0;
    while i < s.len() {
        while i < s.len() && is_space(s[i]) {
            i += 1;
        }
        if i >= s.len() {
            break;
        }
        let mut arg: Vec<u16> = Vec::new();
        let mut quoted = false;
        while i < s.len() {
            match s[i] {
                c if c == b'\\' as u16 => {
                    let mut n = 0;
                    while i < s.len() && s[i] == b'\\' as u16 {
                        n += 1;
                        i += 1;
                    }
                    let quoted_quote = i < s.len() && s[i] == b'"' as u16;
                    let slashes = if quoted_quote { n / 2 } else { n };
                    for _ in 0..slashes {
                        arg.push(b'\\' as u16);
                    }
                    if quoted_quote && n % 2 == 1 {
                        arg.push(b'"' as u16);
                        i += 1;
                    }
                }
                c if c == b'"' as u16 => {
                    i += 1;
                    if quoted && i < s.len() && s[i] == b'"' as u16 {
                        arg.push(b'"' as u16);
                        i += 1;
                    } else {
                        quoted = !quoted;
                    }
                }
                c if !quoted && is_space(c) => break,
                c => {
                    arg.push(c);
                    i += 1;
                }
            }
        }
        args.push(arg);
    }
    args
}

/// `CommandLineToArgvW`: the program's arguments in one LocalAlloc block.
/// The pointer array comes first, because that is the pointer handed back
/// (and the block `LocalFree` must free here); the strings follow it.
extern "win64" fn CommandLineToArgvW(cmdline: *const u16, count: *mut i32) -> u64 {
    if count.is_null() {
        return 0;
    }
    // SAFETY: the caller's out parameter.
    unsafe { *count = 0 };
    if cmdline.is_null() {
        return 0;
    }
    let mut len = 0;
    // SAFETY: a NUL-terminated command line from the caller.
    while unsafe { *cmdline.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: `len` units, as read above.
    let text = unsafe { core::slice::from_raw_parts(cmdline, len) };
    let args = if text.is_empty() {
        // Windows reports the program path as the only argument.
        let path: Vec<u16> = kernel32::module_name(0).unwrap_or_default().encode_utf16().collect();
        alloc::vec![path]
    } else {
        split_command_line(text)
    };
    let array = (args.len() + 1) * 8;
    let total: usize = array + args.iter().map(|a| (a.len() + 1) * 2).sum::<usize>();
    let Some(block) = kernel32::heap_alloc(total, true) else { return 0 };
    let mut at = block + array as u64;
    for (i, arg) in args.iter().enumerate() {
        // SAFETY: inside the block just allocated, zeroed and big enough.
        unsafe {
            core::ptr::copy_nonoverlapping(arg.as_ptr(), at as *mut u16, arg.len());
            *((block + (i * 8) as u64) as *mut u64) = at;
        }
        at += ((arg.len() + 1) * 2) as u64;
    }
    // SAFETY: the caller's out parameter; the array's spare entry stays 0.
    unsafe { *count = args.len() as i32 };
    block
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![SHGetKnownFolderPath, SHGetFolderPathW, CommandLineToArgvW]
}
