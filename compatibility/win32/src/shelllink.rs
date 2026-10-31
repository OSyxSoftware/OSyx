//! Shell links: `IShellLink` and `IPersistFile` of `CLSID_ShellLink`.
//!
//! Installers make their Start Menu and desktop icons with this object
//! (NSIS `CreateShortCut`): create it, set the target path, arguments,
//! working directory, description and icon, then `IPersistFile::Save` a
//! `.lnk` file. The file is a real Shell Link (MS-SHLLINK): the shell of
//! OSyx reads the target back to list the program and to show its icon on
//! the desktop.
//!
//! One heap block holds both interfaces: slot 0 is the `IShellLinkW` vtable
//! pointer, slot 8 the `IPersistFile` one (its interface pointer is the block
//! plus 8). The state lives in a table keyed by the block, so a 32-bit
//! caller only ever sees two pointers. For a PE32 program the vtables hold
//! bridge gates (`wow64::import_active`), four bytes per entry.

use crate::{kernel32, path, wow64};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use os_std::sync::Mutex;

const S_OK: u32 = 0;
const S_FALSE: u32 = 1;
const E_NOTIMPL: u32 = 0x8000_4001;
const E_NOINTERFACE: u32 = 0x8000_4002;
const E_POINTER: u32 = 0x8000_4003;
const E_FAIL: u32 = 0x8000_4005;
const E_OUTOFMEMORY: u32 = 0x8007_000E;
const E_INVALIDARG: u32 = 0x8007_0057;
const HRESULT_FILE_NOT_FOUND: u32 = 0x8007_0002;

/// {00021401-0000-0000-C000-000000000046}
pub(crate) const CLSID_SHELLLINK: [u8; 16] = [0x01, 0x14, 0x02, 0, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46];
const IID_IUNKNOWN: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46];
/// {000214F9-0000-0000-C000-000000000046}
const IID_ISHELLLINKW: [u8; 16] = [0xF9, 0x14, 0x02, 0, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46];
/// {0000010B-0000-0000-C000-000000000046}
const IID_IPERSISTFILE: [u8; 16] = [0x0B, 0x01, 0, 0, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46];
/// {0000010C-0000-0000-C000-000000000046}
const IID_IPERSIST: [u8; 16] = [0x0C, 0x01, 0, 0, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46];

/// What a shell link says (the fields `IShellLinkW` sets and gets).
#[derive(Default)]
struct Link {
    /// Windows path of the target.
    path: String,
    description: String,
    arguments: String,
    working_dir: String,
    icon: String,
    icon_index: i32,
    show: i32,
    hotkey: u16,
    /// The file last saved or loaded (Windows path).
    file: String,
    refs: u32,
}

static LINKS: Mutex<BTreeMap<u64, Link>> = Mutex::new(BTreeMap::new());
/// The two vtables, built on first use: (IShellLinkW, IPersistFile).
static TABLES: Mutex<Option<(u64, u64)>> = Mutex::new(None);

fn with_link<R>(object: u64, f: impl FnOnce(&mut Link) -> R) -> Option<R> {
    LINKS.lock().unwrap().get_mut(&object).map(f)
}

/// The object an `IPersistFile` pointer belongs to.
fn of_file(this: u64) -> u64 {
    this.wrapping_sub(8)
}

/// A vtable of `entries`: host functions for a 64-bit program, bridge gates
/// for a 32-bit one.
fn build_table(entries: &[(&str, u64)]) -> Option<u64> {
    if wow64::is_active() {
        let mut gates: Vec<u32> = Vec::with_capacity(entries.len());
        for (name, host) in entries {
            gates.push(wow64::import_active(name, *host)?);
        }
        Some(Box::leak(gates.into_boxed_slice()).as_ptr() as u64)
    } else {
        let table: Vec<u64> = entries.iter().map(|(_, host)| *host).collect();
        Some(Box::leak(table.into_boxed_slice()).as_ptr() as u64)
    }
}

macro_rules! entry {
    ($f:ident) => {
        (stringify!($f), $f as *const () as u64)
    };
}

fn tables() -> Option<(u64, u64)> {
    let mut slot = TABLES.lock().unwrap();
    if slot.is_none() {
        let link = build_table(&[
            entry!(ShellLinkW_QueryInterface),
            entry!(ShellLinkW_AddRef),
            entry!(ShellLinkW_Release),
            entry!(ShellLinkW_GetPath),
            entry!(ShellLinkW_GetIDList),
            entry!(ShellLinkW_SetIDList),
            entry!(ShellLinkW_GetDescription),
            entry!(ShellLinkW_SetDescription),
            entry!(ShellLinkW_GetWorkingDirectory),
            entry!(ShellLinkW_SetWorkingDirectory),
            entry!(ShellLinkW_GetArguments),
            entry!(ShellLinkW_SetArguments),
            entry!(ShellLinkW_GetHotkey),
            entry!(ShellLinkW_SetHotkey),
            entry!(ShellLinkW_GetShowCmd),
            entry!(ShellLinkW_SetShowCmd),
            entry!(ShellLinkW_GetIconLocation),
            entry!(ShellLinkW_SetIconLocation),
            entry!(ShellLinkW_SetRelativePath),
            entry!(ShellLinkW_Resolve),
            entry!(ShellLinkW_SetPath),
        ])?;
        let file = build_table(&[
            entry!(PersistFile_QueryInterface),
            entry!(PersistFile_AddRef),
            entry!(PersistFile_Release),
            entry!(PersistFile_GetClassID),
            entry!(PersistFile_IsDirty),
            entry!(PersistFile_Load),
            entry!(PersistFile_Save),
            entry!(PersistFile_SaveCompleted),
            entry!(PersistFile_GetCurFile),
        ])?;
        *slot = Some((link, file));
    }
    *slot
}

/// `CoCreateInstance(CLSID_ShellLink, ..., iid, out)`.
///
/// # Safety
/// `iid` points at a GUID and `out` at a pointer-sized slot.
pub(crate) unsafe fn create(iid: *const u8, out: *mut u64) -> u32 {
    let Some((link_table, file_table)) = tables() else { return E_FAIL };
    let Some(object) = kernel32::heap_alloc(32, true) else { return E_OUTOFMEMORY };
    // SAFETY: a fresh 32-byte block; slot 0 and slot 8 are the vtable pointers.
    unsafe {
        *(object as *mut u64) = link_table;
        *((object + 8) as *mut u64) = file_table;
    }
    LINKS.lock().unwrap().insert(object, Link { show: 1, refs: 1, ..Link::default() });
    let hr = query(object, iid, out);
    release(object); // the caller's reference is the one query() added
    hr
}

fn query(object: u64, riid: *const u8, out: *mut u64) -> u32 {
    if out.is_null() || riid.is_null() {
        return E_POINTER;
    }
    // SAFETY: the caller's IID.
    let iid: [u8; 16] = unsafe { *(riid as *const [u8; 16]) };
    let interface = if iid == IID_IUNKNOWN || iid == IID_ISHELLLINKW {
        object
    } else if iid == IID_IPERSISTFILE || iid == IID_IPERSIST {
        object + 8
    } else {
        // SAFETY: the caller's out pointer.
        unsafe { *out = 0 };
        return E_NOINTERFACE;
    };
    with_link(object, |l| l.refs += 1);
    // SAFETY: the caller's out pointer.
    unsafe { *out = interface };
    S_OK
}

fn add_ref(object: u64) -> u32 {
    with_link(object, |l| {
        l.refs += 1;
        l.refs
    })
    .unwrap_or(0)
}

fn release(object: u64) -> u32 {
    let left = with_link(object, |l| {
        l.refs = l.refs.saturating_sub(1);
        l.refs
    })
    .unwrap_or(0);
    if left == 0 && LINKS.lock().unwrap().remove(&object).is_some() {
        kernel32::heap_free(object);
    }
    left
}

/// Copies `text` into the caller's buffer of `cch` UTF-16 units, cut and
/// terminated if it does not fit.
fn put_wide(dst: *mut u16, cch: i32, text: &str) -> u32 {
    if dst.is_null() || cch <= 0 {
        return E_INVALIDARG;
    }
    let units: Vec<u16> = text.encode_utf16().collect();
    let n = units.len().min(cch as usize - 1);
    // SAFETY: the caller's buffer of `cch` units.
    unsafe {
        core::ptr::copy_nonoverlapping(units.as_ptr(), dst, n);
        *dst.add(n) = 0;
    }
    S_OK
}

fn read_wide(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: a NUL-terminated string.
    unsafe { path::wide_to_string(p) }.unwrap_or_default()
}

fn text_getter(object: u64, buf: *mut u16, cch: i32, field: impl FnOnce(&Link) -> String) -> u32 {
    match with_link(object, |l| field(l)) {
        Some(text) => put_wide(buf, cch, &text),
        None => E_FAIL,
    }
}

fn text_setter(object: u64, text: *const u16, set: impl FnOnce(&mut Link, String)) -> u32 {
    let text = read_wide(text);
    match with_link(object, |l| set(l, text)) {
        Some(()) => S_OK,
        None => E_FAIL,
    }
}

// ---- IShellLinkW ------------------------------------------------------------------------

#[allow(non_snake_case)]
extern "win64" fn ShellLinkW_QueryInterface(this: u64, riid: *const u8, out: *mut u64) -> u32 {
    query(this, riid, out)
}
#[allow(non_snake_case)]
extern "win64" fn ShellLinkW_AddRef(this: u64) -> u32 {
    add_ref(this)
}
#[allow(non_snake_case)]
extern "win64" fn ShellLinkW_Release(this: u64) -> u32 {
    release(this)
}
#[allow(non_snake_case)]
extern "win64" fn ShellLinkW_GetPath(this: u64, buf: *mut u16, cch: i32, find: *mut u8, _flags: u32) -> u32 {
    if !find.is_null() {
        // SAFETY: a WIN32_FIND_DATAW (592 bytes, the same for 32 and 64 bits).
        unsafe { core::ptr::write_bytes(find, 0, 592) };
    }
    text_getter(this, buf, cch, |l| l.path.clone())
}
#[allow(non_snake_case)]
extern "win64" fn ShellLinkW_GetIDList(_this: u64, pidl: *mut u64) -> u32 {
    if !pidl.is_null() {
        // SAFETY: the caller's out pointer.
        unsafe { *pidl = 0 };
    }
    S_FALSE // no item ID list: the link names its target by path
}
#[allow(non_snake_case)]
extern "win64" fn ShellLinkW_SetIDList(_this: u64, _pidl: *const u8) -> u32 {
    E_NOTIMPL
}
#[allow(non_snake_case)]
extern "win64" fn ShellLinkW_GetDescription(this: u64, buf: *mut u16, cch: i32) -> u32 {
    text_getter(this, buf, cch, |l| l.description.clone())
}
#[allow(non_snake_case)]
extern "win64" fn ShellLinkW_SetDescription(this: u64, text: *const u16) -> u32 {
    text_setter(this, text, |l, t| l.description = t)
}
#[allow(non_snake_case)]
extern "win64" fn ShellLinkW_GetWorkingDirectory(this: u64, buf: *mut u16, cch: i32) -> u32 {
    text_getter(this, buf, cch, |l| l.working_dir.clone())
}
#[allow(non_snake_case)]
extern "win64" fn ShellLinkW_SetWorkingDirectory(this: u64, text: *const u16) -> u32 {
    text_setter(this, text, |l, t| l.working_dir = t)
}
#[allow(non_snake_case)]
extern "win64" fn ShellLinkW_GetArguments(this: u64, buf: *mut u16, cch: i32) -> u32 {
    text_getter(this, buf, cch, |l| l.arguments.clone())
}
#[allow(non_snake_case)]
extern "win64" fn ShellLinkW_SetArguments(this: u64, text: *const u16) -> u32 {
    text_setter(this, text, |l, t| l.arguments = t)
}
#[allow(non_snake_case)]
extern "win64" fn ShellLinkW_GetHotkey(this: u64, out: *mut u16) -> u32 {
    if out.is_null() {
        return E_POINTER;
    }
    match with_link(this, |l| l.hotkey) {
        Some(key) => {
            // SAFETY: the caller's WORD.
            unsafe { *out = key };
            S_OK
        }
        None => E_FAIL,
    }
}
#[allow(non_snake_case)]
extern "win64" fn ShellLinkW_SetHotkey(this: u64, key: u32) -> u32 {
    with_link(this, |l| l.hotkey = key as u16).map_or(E_FAIL, |_| S_OK)
}
#[allow(non_snake_case)]
extern "win64" fn ShellLinkW_GetShowCmd(this: u64, out: *mut i32) -> u32 {
    if out.is_null() {
        return E_POINTER;
    }
    match with_link(this, |l| l.show) {
        Some(show) => {
            // SAFETY: the caller's int.
            unsafe { *out = show };
            S_OK
        }
        None => E_FAIL,
    }
}
#[allow(non_snake_case)]
extern "win64" fn ShellLinkW_SetShowCmd(this: u64, show: i32) -> u32 {
    with_link(this, |l| l.show = show).map_or(E_FAIL, |_| S_OK)
}
#[allow(non_snake_case)]
extern "win64" fn ShellLinkW_GetIconLocation(this: u64, buf: *mut u16, cch: i32, index: *mut i32) -> u32 {
    if index.is_null() {
        return E_POINTER;
    }
    let Some(i) = with_link(this, |l| l.icon_index) else { return E_FAIL };
    // SAFETY: the caller's int.
    unsafe { *index = i };
    text_getter(this, buf, cch, |l| l.icon.clone())
}
#[allow(non_snake_case)]
extern "win64" fn ShellLinkW_SetIconLocation(this: u64, text: *const u16, index: i32) -> u32 {
    let text = read_wide(text);
    with_link(this, |l| {
        l.icon = text;
        l.icon_index = index;
    })
    .map_or(E_FAIL, |_| S_OK)
}
#[allow(non_snake_case)]
extern "win64" fn ShellLinkW_SetRelativePath(_this: u64, _text: *const u16, _reserved: u32) -> u32 {
    S_OK // the target is stored as a full path
}
#[allow(non_snake_case)]
extern "win64" fn ShellLinkW_Resolve(_this: u64, _hwnd: u64, _flags: u32) -> u32 {
    S_OK // a path target is already resolved
}
#[allow(non_snake_case)]
extern "win64" fn ShellLinkW_SetPath(this: u64, text: *const u16) -> u32 {
    if text.is_null() {
        return E_INVALIDARG;
    }
    text_setter(this, text, |l, t| l.path = t)
}

// ---- IPersistFile -----------------------------------------------------------------------

#[allow(non_snake_case)]
extern "win64" fn PersistFile_QueryInterface(this: u64, riid: *const u8, out: *mut u64) -> u32 {
    query(of_file(this), riid, out)
}
#[allow(non_snake_case)]
extern "win64" fn PersistFile_AddRef(this: u64) -> u32 {
    add_ref(of_file(this))
}
#[allow(non_snake_case)]
extern "win64" fn PersistFile_Release(this: u64) -> u32 {
    release(of_file(this))
}
#[allow(non_snake_case)]
extern "win64" fn PersistFile_GetClassID(_this: u64, out: *mut u8) -> u32 {
    if out.is_null() {
        return E_POINTER;
    }
    // SAFETY: the caller's CLSID.
    unsafe { core::ptr::copy_nonoverlapping(CLSID_SHELLLINK.as_ptr(), out, 16) };
    S_OK
}
#[allow(non_snake_case)]
extern "win64" fn PersistFile_IsDirty(_this: u64) -> u32 {
    S_FALSE
}
#[allow(non_snake_case)]
extern "win64" fn PersistFile_Load(this: u64, file: *const u16, _mode: u32) -> u32 {
    let win = read_wide(file);
    let Ok(osyx) = path::to_osyx(&win, &kernel32::cwd_win()) else { return HRESULT_FILE_NOT_FOUND };
    let Ok(data) = os_std::fs::read(&osyx) else { return HRESULT_FILE_NOT_FOUND };
    let Some(mut link) = parse(&data) else { return E_FAIL };
    link.file = win;
    with_link(of_file(this), |l| {
        link.refs = l.refs;
        *l = link;
    })
    .map_or(E_FAIL, |_| S_OK)
}
#[allow(non_snake_case)]
extern "win64" fn PersistFile_Save(this: u64, file: *const u16, _remember: i32) -> u32 {
    let object = of_file(this);
    let Some((bytes, current)) = with_link(object, |l| (build(l), l.file.clone())) else { return E_FAIL };
    let win = if file.is_null() { current } else { read_wide(file) };
    if win.is_empty() {
        return E_INVALIDARG;
    }
    let Ok(osyx) = path::to_osyx(&win, &kernel32::cwd_win()) else { return E_FAIL };
    if let Some((dir, _)) = osyx.rsplit_once('/') {
        if !dir.is_empty() && os_std::fs::create_dir_all(dir).is_err() {
            return E_FAIL;
        }
    }
    if os_std::fs::write(&osyx, bytes).is_err() {
        return E_FAIL;
    }
    with_link(object, |l| l.file = win);
    S_OK
}
#[allow(non_snake_case)]
extern "win64" fn PersistFile_SaveCompleted(_this: u64, _file: *const u16) -> u32 {
    S_OK
}
#[allow(non_snake_case)]
extern "win64" fn PersistFile_GetCurFile(this: u64, out: *mut u64) -> u32 {
    if out.is_null() {
        return E_POINTER;
    }
    let Some(file) = with_link(of_file(this), |l| l.file.clone()) else { return E_FAIL };
    let units: Vec<u16> = file.encode_utf16().chain(core::iter::once(0)).collect();
    let Some(block) = kernel32::heap_alloc(units.len() * 2, false) else { return E_OUTOFMEMORY };
    // SAFETY: a fresh block of the right size (CoTaskMemFree frees it).
    unsafe {
        core::ptr::copy_nonoverlapping(units.as_ptr(), block as *mut u16, units.len());
        *out = block;
    }
    if file.is_empty() { S_FALSE } else { S_OK }
}

// ---- the .lnk file ----------------------------------------------------------------------

const HAS_LINK_INFO: u32 = 0x02;
const HAS_NAME: u32 = 0x04;
const HAS_WORKING_DIR: u32 = 0x10;
const HAS_ARGUMENTS: u32 = 0x20;
const HAS_ICON_LOCATION: u32 = 0x40;
const IS_UNICODE: u32 = 0x80;
const FORCE_NO_LINK_TRACK: u32 = 0x10_0000;

/// A Shell Link file: header, a LinkInfo block with the target's local base
/// path, and the Unicode string data (name, working directory, arguments,
/// icon location).
fn build(l: &Link) -> Vec<u8> {
    let strings: [(u32, &str); 4] = [
        (HAS_NAME, &l.description),
        (HAS_WORKING_DIR, &l.working_dir),
        (HAS_ARGUMENTS, &l.arguments),
        (HAS_ICON_LOCATION, &l.icon),
    ];
    let mut flags = IS_UNICODE | FORCE_NO_LINK_TRACK;
    if !l.path.is_empty() {
        flags |= HAS_LINK_INFO;
    }
    for (bit, text) in strings {
        if !text.is_empty() {
            flags |= bit;
        }
    }
    let mut out = Vec::new();
    out.extend_from_slice(&0x4Cu32.to_le_bytes());
    out.extend_from_slice(&[0x01, 0x14, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46]);
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&0x20u32.to_le_bytes()); // FILE_ATTRIBUTE_ARCHIVE
    out.extend_from_slice(&[0u8; 24]); // creation, access and write times
    out.extend_from_slice(&0u32.to_le_bytes()); // target size
    out.extend_from_slice(&l.icon_index.to_le_bytes());
    out.extend_from_slice(&(l.show.max(1) as u32).to_le_bytes());
    out.extend_from_slice(&l.hotkey.to_le_bytes());
    out.extend_from_slice(&[0u8; 10]); // reserved
    if flags & HAS_LINK_INFO != 0 {
        let mut target = l.path.as_bytes().to_vec();
        target.push(0);
        let mut volume = [0u8; 17];
        volume[0..4].copy_from_slice(&17u32.to_le_bytes()); // VolumeIDSize
        volume[4..8].copy_from_slice(&3u32.to_le_bytes()); // DRIVE_FIXED
        volume[12..16].copy_from_slice(&16u32.to_le_bytes()); // an empty label
        let size = 0x1C + volume.len() + target.len() + 1;
        out.extend_from_slice(&(size as u32).to_le_bytes());
        out.extend_from_slice(&0x1Cu32.to_le_bytes()); // LinkInfoHeaderSize
        out.extend_from_slice(&1u32.to_le_bytes()); // VolumeIDAndLocalBasePath
        out.extend_from_slice(&0x1Cu32.to_le_bytes()); // VolumeIDOffset
        out.extend_from_slice(&((0x1C + volume.len()) as u32).to_le_bytes()); // LocalBasePathOffset
        out.extend_from_slice(&0u32.to_le_bytes()); // CommonNetworkRelativeLinkOffset
        out.extend_from_slice(&((0x1C + volume.len() + target.len()) as u32).to_le_bytes()); // CommonPathSuffixOffset
        out.extend_from_slice(&volume);
        out.extend_from_slice(&target);
        out.push(0); // empty CommonPathSuffix
    }
    for (_, text) in strings {
        if text.is_empty() {
            continue;
        }
        let units: Vec<u16> = text.encode_utf16().collect();
        out.extend_from_slice(&(units.len().min(0xFFFF) as u16).to_le_bytes());
        for unit in units.iter().take(0xFFFF) {
            out.extend_from_slice(&unit.to_le_bytes());
        }
    }
    out
}

/// The fields of a Shell Link file this module writes.
fn parse(data: &[u8]) -> Option<Link> {
    let u16_at = |at: usize| data.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize);
    let u32_at = |at: usize| data.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
    if u32_at(0)? != 0x4C {
        return None;
    }
    let flags = u32_at(0x14)? as u32;
    let mut link = Link { show: u32_at(0x3C)? as i32, icon_index: u32_at(0x38)? as i32, hotkey: u16_at(0x40)? as u16, ..Link::default() };
    let mut at = 0x4C;
    if flags & 0x01 != 0 {
        at += 2 + u16_at(at)?;
    }
    if flags & HAS_LINK_INFO != 0 {
        let base = at;
        let size = u32_at(base)?;
        if u32_at(base + 8)? & 1 != 0 {
            let text = |from: usize| -> Option<String> {
                let bytes = data.get(from..)?;
                let end = bytes.iter().position(|&b| b == 0)?;
                Some(String::from_utf8_lossy(&bytes[..end]).into_owned())
            };
            let mut target = text(base + u32_at(base + 0x10)?)?;
            let tail = text(base + u32_at(base + 0x18)?).unwrap_or_default();
            if !tail.is_empty() {
                if !target.ends_with('\\') {
                    target.push('\\');
                }
                target.push_str(&tail);
            }
            link.path = target;
        }
        at = base + size;
    }
    let unicode = flags & IS_UNICODE != 0;
    for bit in [HAS_NAME, 0x08, HAS_WORKING_DIR, HAS_ARGUMENTS, HAS_ICON_LOCATION] {
        if flags & bit == 0 {
            continue;
        }
        let count = u16_at(at)?;
        at += 2;
        let bytes = if unicode { count * 2 } else { count };
        let raw = data.get(at..at + bytes)?;
        at += bytes;
        let text = if unicode {
            char::decode_utf16((0..raw.len() / 2).map(|i| u16::from_le_bytes([raw[2 * i], raw[2 * i + 1]])))
                .map(|c| c.unwrap_or('\u{FFFD}'))
                .collect::<String>()
        } else {
            String::from_utf8_lossy(raw).into_owned()
        };
        match bit {
            HAS_NAME => link.description = text,
            HAS_WORKING_DIR => link.working_dir = text,
            HAS_ARGUMENTS => link.arguments = text,
            HAS_ICON_LOCATION => link.icon = text,
            _ => {}
        }
    }
    Some(link)
}
