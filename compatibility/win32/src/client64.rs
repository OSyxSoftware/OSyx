//! Functions the 64-bit Steam client (steam.exe, SteamUI.dll and SDL3)
//! imports that winrun lacked: Tool Help snapshots of the processes and
//! of this process's modules, volume GUID names, directory change
//! notification, the TCP table, CoSetProxyBlanket, the shell allocator,
//! icons extracted from files, and the HID class driver interface (hid.dll).
//! Structures with pointer-sized members follow the caller's layout (32-bit
//! for a WoW64 program).
#![allow(non_snake_case)]
use crate::error::*;
use crate::kernel32::{self, fail, Bool, FALSE, TRUE};
use crate::objects::{self, Object};
use crate::{path, teb};
use alloc::{collections::BTreeMap, string::String, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicUsize, Ordering};
use os_std::sync::Mutex;

const ERROR_NO_MORE_FILES: u32 = 18;
const ERROR_BAD_LENGTH: u32 = 24;
const ERROR_IO_PENDING: u32 = 997;

fn narrow() -> bool {
    crate::wow64::is_active()
}

/// Writes `s` as NUL-terminated UTF-16 into a field of `cap` characters
/// (truncated to fit).
///
/// # Safety
/// `at` must be writable for `cap` characters.
unsafe fn put_wide(at: u64, cap: usize, s: &str) {
    let units: Vec<u16> = s.encode_utf16().take(cap - 1).collect();
    // SAFETY: as documented.
    unsafe {
        core::ptr::copy_nonoverlapping(units.as_ptr(), at as *mut u16, units.len());
        *((at as *mut u16).add(units.len())) = 0;
    }
}

/// As [`put_wide`] for an ANSI (UTF-8) field of `cap` bytes.
///
/// # Safety
/// As [`put_wide`].
unsafe fn put_ansi(at: u64, cap: usize, s: &str) {
    let mut n = s.len().min(cap - 1);
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    // SAFETY: as documented.
    unsafe {
        core::ptr::copy_nonoverlapping(s.as_ptr(), at as *mut u8, n);
        *((at as *mut u8).add(n)) = 0;
    }
}

// ---- Tool Help ------------------------------------------------------------------

/// One process of a snapshot: (pid, parent, threads, name).
type ProcessRow = (u32, u32, u32, String);
/// One module of a snapshot: (base, size, name, path).
type ModuleRow = (u64, u32, String, String);

/// A Tool Help snapshot (CreateToolhelp32Snapshot): the processes and the
/// modules of the process it was taken for, and the read positions.
pub struct Snapshot {
    pid: u32,
    processes: Vec<ProcessRow>,
    modules: Vec<ModuleRow>,
    next_process: AtomicUsize,
    next_module: AtomicUsize,
}

const TH32CS_SNAPHEAPLIST: u32 = 0x1;
const TH32CS_SNAPPROCESS: u32 = 0x2;
const TH32CS_SNAPTHREAD: u32 = 0x4;
const TH32CS_SNAPMODULE: u32 = 0x8;
const TH32CS_SNAPMODULE32: u32 = 0x10;
const TH32CS_INHERIT: u32 = 0x8000_0000;

/// The executable name a process is known by: for this process its
/// Windows program; for the others their OSyx program (Windows programs of
/// other processes run in their own `winrun`).
fn process_rows() -> Vec<ProcessRow> {
    let mut info = alloc::vec![osyx_abi::ipc::ProcInfo::default(); 512];
    let n = os_sys::proc_list(&mut info).unwrap_or(0);
    let me = kernel32::pid();
    let own = kernel32::module_name(0).map(|p| String::from(p.rsplit(['\\', '/']).next().unwrap_or(&p)));
    let mut rows: Vec<ProcessRow> = Vec::with_capacity(n + 1);
    // The System Idle Process, as Windows lists first.
    rows.push((0, 0, 1, String::from("[System Process]")));
    for p in &info[..n] {
        let name = if p.pid == me {
            own.clone().unwrap_or_else(|| String::from(osyx_abi::ipc::field_str(&p.name)))
        } else {
            String::from(osyx_abi::ipc::field_str(&p.name))
        };
        rows.push((p.pid as u32, p.parent as u32, p.threads, name));
    }
    rows
}

fn module_rows() -> Vec<ModuleRow> {
    crate::loader::summary()
        .into_iter()
        .filter_map(|(name, base, builtin, _, _)| {
            let size = if builtin { crate::mem::PAGE as u32 } else { crate::loader::image_containing(base).map_or(0, |(_, s)| s as u32) };
            let path = kernel32::module_name(base).unwrap_or_else(|| alloc::format!("C:\\Windows\\System32\\{name}"));
            Some((base, size, name, path))
        })
        .collect()
}

/// CreateToolhelp32Snapshot: processes (TH32CS_SNAPPROCESS) and the
/// modules of this process (TH32CS_SNAPMODULE, pid 0 or its own); another
/// process's modules cannot be read on OSyx (ERROR_ACCESS_DENIED). Thread
/// and heap lists are empty: OSyx does not list other processes' threads
/// or this runtime's heaps.
extern "win64" fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> u64 {
    if flags & !(TH32CS_SNAPHEAPLIST | TH32CS_SNAPPROCESS | TH32CS_SNAPTHREAD | TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32 | TH32CS_INHERIT) != 0 {
        return fail(ERROR_INVALID_PARAMETER, u64::MAX);
    }
    let me = kernel32::pid() as u32;
    let target = if pid == 0 { me } else { pid };
    let want_modules = flags & (TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32) != 0;
    if want_modules && target != me {
        if os_sys::proc_info(target as u64).is_err() {
            return fail(ERROR_INVALID_PARAMETER, u64::MAX);
        }
        return fail(ERROR_ACCESS_DENIED, u64::MAX);
    }
    let snapshot = Snapshot {
        pid: target,
        processes: if flags & TH32CS_SNAPPROCESS != 0 { process_rows() } else { Vec::new() },
        modules: if want_modules { module_rows() } else { Vec::new() },
        next_process: AtomicUsize::new(0),
        next_module: AtomicUsize::new(0),
    };
    objects::insert(Object::Snapshot(Arc::new(snapshot)))
}

fn snapshot(h: u64) -> Result<Arc<Snapshot>, u32> {
    match objects::get(h) {
        Some(Object::Snapshot(s)) => Ok(s),
        _ => Err(ERROR_INVALID_HANDLE),
    }
}

/// PROCESSENTRY32(W): dwSize, cntUsage, th32ProcessID, th32DefaultHeapID
/// (ULONG_PTR), th32ModuleID, cntThreads, th32ParentProcessID,
/// pcPriClassBase, dwFlags, szExeFile[260].
fn put_process(entry: *mut u8, row: &ProcessRow, wide: bool) -> Result<(), u32> {
    let n = narrow();
    let name_at: u64 = if n { 36 } else { 44 };
    let size = name_at as u32 + if wide { 520 } else { 260 };
    // SAFETY: the caller's entry starts with dwSize.
    if entry.is_null() || unsafe { teb::get::<u32>(entry as u64, 0) } < size {
        return Err(ERROR_BAD_LENGTH);
    }
    let p = entry as u64;
    let (heap, module, threads, parent, pri, flags) = if n { (12, 16, 20, 24, 28, 32) } else { (16, 24, 28, 32, 36, 40) };
    // SAFETY: an entry of at least `size` bytes.
    unsafe {
        teb::put(p, 4, 0u32);
        teb::put(p, 8, row.0);
        if n {
            teb::put(p, heap, 0u32);
        } else {
            teb::put(p, heap, 0u64);
        }
        teb::put(p, module, 0u32);
        teb::put(p, threads, row.2);
        teb::put(p, parent, row.1);
        teb::put(p, pri, 8i32); // NORMAL_PRIORITY_CLASS base priority
        teb::put(p, flags, 0u32);
        if wide { put_wide(p + name_at, 260, &row.3) } else { put_ansi(p + name_at, 260, &row.3) }
    }
    Ok(())
}

/// MODULEENTRY32(W): dwSize, th32ModuleID, th32ProcessID, GlblcntUsage,
/// ProccntUsage, modBaseAddr, modBaseSize, hModule, szModule[256],
/// szExePath[260].
fn put_module(entry: *mut u8, row: &ModuleRow, pid: u32, wide: bool) -> Result<(), u32> {
    let n = narrow();
    let unit = if wide { 2 } else { 1 };
    let (base_at, size_at, handle_at, name_at): (u64, u64, u64, u64) = if n { (20, 24, 28, 32) } else { (24, 32, 40, 48) };
    let path_at = name_at + 256 * unit;
    let size = (path_at + 260 * unit) as u32;
    // SAFETY: the caller's entry starts with dwSize.
    if entry.is_null() || unsafe { teb::get::<u32>(entry as u64, 0) } < size {
        return Err(ERROR_BAD_LENGTH);
    }
    let p = entry as u64;
    // SAFETY: an entry of at least `size` bytes.
    unsafe {
        teb::put(p, 4, 1u32); // th32ModuleID: always 1 on Windows
        teb::put(p, 8, pid);
        teb::put(p, 12, 0xFFFFu32);
        teb::put(p, 16, 0xFFFFu32);
        if n {
            teb::put(p, base_at as usize, row.0 as u32);
            teb::put(p, handle_at as usize, row.0 as u32);
        } else {
            teb::put(p, base_at as usize, row.0);
            teb::put(p, handle_at as usize, row.0);
        }
        teb::put(p, size_at as usize, row.1);
        if wide {
            put_wide(p + name_at, 256, &row.2);
            put_wide(p + path_at, 260, &row.3);
        } else {
            put_ansi(p + name_at, 256, &row.2);
            put_ansi(p + path_at, 260, &row.3);
        }
    }
    Ok(())
}

fn next_process(h: u64, entry: *mut u8, wide: bool, first: bool) -> Bool {
    let s = match snapshot(h) {
        Ok(s) => s,
        Err(e) => return fail(e, FALSE),
    };
    if first {
        s.next_process.store(0, Ordering::SeqCst);
    }
    let i = s.next_process.load(Ordering::SeqCst);
    let Some(row) = s.processes.get(i) else { return fail(ERROR_NO_MORE_FILES, FALSE) };
    if let Err(e) = put_process(entry, row, wide) {
        return fail(e, FALSE);
    }
    s.next_process.store(i + 1, Ordering::SeqCst);
    TRUE
}

fn next_module(h: u64, entry: *mut u8, wide: bool, first: bool) -> Bool {
    let s = match snapshot(h) {
        Ok(s) => s,
        Err(e) => return fail(e, FALSE),
    };
    if first {
        s.next_module.store(0, Ordering::SeqCst);
    }
    let i = s.next_module.load(Ordering::SeqCst);
    let Some(row) = s.modules.get(i) else { return fail(ERROR_NO_MORE_FILES, FALSE) };
    if let Err(e) = put_module(entry, row, s.pid, wide) {
        return fail(e, FALSE);
    }
    s.next_module.store(i + 1, Ordering::SeqCst);
    TRUE
}

extern "win64" fn Process32FirstW(h: u64, e: *mut u8) -> Bool {
    next_process(h, e, true, true)
}
extern "win64" fn Process32NextW(h: u64, e: *mut u8) -> Bool {
    next_process(h, e, true, false)
}
extern "win64" fn Process32First(h: u64, e: *mut u8) -> Bool {
    next_process(h, e, false, true)
}
extern "win64" fn Process32Next(h: u64, e: *mut u8) -> Bool {
    next_process(h, e, false, false)
}
extern "win64" fn Module32FirstW(h: u64, e: *mut u8) -> Bool {
    next_module(h, e, true, true)
}
extern "win64" fn Module32NextW(h: u64, e: *mut u8) -> Bool {
    next_module(h, e, true, false)
}
extern "win64" fn Module32First(h: u64, e: *mut u8) -> Bool {
    next_module(h, e, false, true)
}
extern "win64" fn Module32Next(h: u64, e: *mut u8) -> Bool {
    next_module(h, e, false, false)
}

// ---- volume names ---------------------------------------------------------------

/// The volume GUID path of an OSyx volume, from its device id:
/// "\\?\Volume{xxxxxxxx-0000-0000-0000-xxxxxxxxxxxx}\". The id is stable
/// while the volume stays mounted, as a GUID path is.
fn volume_guid_path(dev: u64) -> String {
    alloc::format!("\\\\?\\Volume{{{:08x}-0000-0000-0000-{:012x}}}\\", dev as u32, (dev >> 32) & 0xFFFF_FFFF_FFFF)
}

/// GetVolumeNameForVolumeMountPointW: the GUID path of the volume mounted
/// at a mount point. "C:\" is the OSyx root; a mount point is a root
/// directory of a mounted volume ("C:\mnt\usb\"). The name needs its
/// trailing backslash, as on Windows.
extern "win64" fn GetVolumeNameForVolumeMountPointW(mount: *const u16, out: *mut u16, len: u32) -> Bool {
    const ERROR_NOT_A_REPARSE_POINT: u32 = 4390;
    // SAFETY: a NUL-terminated path.
    let Some(win) = (unsafe { path::wide_to_string(mount) }) else { return fail(ERROR_INVALID_PARAMETER, FALSE) };
    if !win.ends_with('\\') {
        return fail(ERROR_INVALID_NAME, FALSE);
    }
    let p = match kernel32::locate(&win) {
        Ok((p, true)) => p,
        Ok((_, false)) => return fail(ERROR_FILE_NOT_FOUND, FALSE),
        Err(e) => return fail(e, FALSE),
    };
    let Ok(st) = os_sys::stat(&p) else { return fail(ERROR_FILE_NOT_FOUND, FALSE) };
    // A mount point: the root, or a directory whose volume differs from
    // its parent's.
    let trimmed = p.trim_end_matches('/');
    let is_mount = trimmed.is_empty() || {
        let parent = trimmed.rsplit_once('/').map_or("/", |(d, _)| if d.is_empty() { "/" } else { d });
        os_sys::stat(parent).is_ok_and(|ps| ps.dev != st.dev)
    };
    if !is_mount {
        return fail(ERROR_NOT_A_REPARSE_POINT, FALSE);
    }
    let name = volume_guid_path(st.dev);
    let r = kernel32::out_w(&name, out, len);
    if r >= len {
        return fail(ERROR_FILENAME_EXCED_RANGE, FALSE);
    }
    TRUE
}

/// GetVolumePathNamesForVolumeNameW: the drive letter and folder paths a
/// volume is mounted at, as a double-NUL-terminated list.
extern "win64" fn GetVolumePathNamesForVolumeNameW(volume: *const u16, out: *mut u16, len: u32, needed: *mut u32) -> Bool {
    // SAFETY: a NUL-terminated name.
    let Some(name) = (unsafe { path::wide_to_string(volume) }) else { return fail(ERROR_INVALID_PARAMETER, FALSE) };
    let mut paths: Vec<String> = Vec::new();
    for m in os_std::system::mounts().unwrap_or_default() {
        if volume_guid_path(m.dev).eq_ignore_ascii_case(&name) {
            let at = os_std::system::field_str(&m.path);
            let win = path::to_windows(at);
            paths.push(if win.ends_with('\\') { win } else { win + "\\" });
        }
    }
    if paths.is_empty() {
        // The root is mounted before the mount table lists it.
        if os_sys::stat("/").is_ok_and(|st| volume_guid_path(st.dev).eq_ignore_ascii_case(&name)) {
            paths.push(String::from("C:\\"));
        } else {
            return fail(ERROR_INVALID_NAME, FALSE);
        }
    }
    let mut units: Vec<u16> = Vec::new();
    for p in &paths {
        units.extend(p.encode_utf16());
        units.push(0);
    }
    units.push(0);
    if !needed.is_null() {
        // SAFETY: the caller's out value.
        unsafe { *needed = units.len() as u32 };
    }
    if out.is_null() || (len as usize) < units.len() {
        return fail(ERROR_MORE_DATA, FALSE);
    }
    // SAFETY: the caller's buffer of `len` characters.
    unsafe { core::ptr::copy_nonoverlapping(units.as_ptr(), out, units.len()) };
    TRUE
}

// ---- directory changes ----------------------------------------------------------

/// What a directory looked like: relative name -> (is directory, size,
/// write time, attributes).
type Listing = BTreeMap<String, (bool, u64, i64, u32)>;

/// The last listing of each watched directory handle.
static WATCHES: Mutex<BTreeMap<u64, Listing>> = Mutex::new(BTreeMap::new());

fn list_dir(root: &str, rel: &str, subtree: bool, depth: u32, out: &mut Listing) {
    let dir = if rel.is_empty() { String::from(root) } else { alloc::format!("{root}/{}", rel.replace('\\', "/")) };
    let Ok(entries) = os_std::fs::read_dir(&dir) else { return };
    for e in entries {
        let name = e.file_name();
        let full = alloc::format!("{dir}/{name}");
        let Ok(st) = os_sys::stat(&full) else { continue };
        let is_dir = st.kind == osyx_abi::KIND_DIR;
        let key = if rel.is_empty() { String::from(name) } else { alloc::format!("{rel}\\{name}") };
        out.insert(key.clone(), (is_dir, st.size, st.mtime_ns, st.mode));
        if is_dir && subtree && depth < 32 {
            list_dir(root, &key, subtree, depth + 1, out);
        }
    }
}

const FILE_NOTIFY_CHANGE_FILE_NAME: u32 = 0x1;
const FILE_NOTIFY_CHANGE_DIR_NAME: u32 = 0x2;
const FILE_NOTIFY_CHANGE_ATTRIBUTES: u32 = 0x4;
const FILE_NOTIFY_CHANGE_SIZE: u32 = 0x8;
const FILE_NOTIFY_CHANGE_LAST_WRITE: u32 = 0x10;
const FILE_ACTION_ADDED: u32 = 1;
const FILE_ACTION_REMOVED: u32 = 2;
const FILE_ACTION_MODIFIED: u32 = 3;
const FILE_ACTION_RENAMED_OLD_NAME: u32 = 4;
const FILE_ACTION_RENAMED_NEW_NAME: u32 = 5;

/// The changes from `old` to `new` the filter asks for. A name that went
/// away and one that appeared with the same size and time is a rename.
fn changes(old: &Listing, new: &Listing, filter: u32) -> Vec<(u32, String)> {
    let wanted = |is_dir: bool| filter & if is_dir { FILE_NOTIFY_CHANGE_DIR_NAME } else { FILE_NOTIFY_CHANGE_FILE_NAME } != 0;
    let removed: Vec<(&String, &(bool, u64, i64, u32))> = old.iter().filter(|(k, _)| !new.contains_key(*k)).collect();
    let mut added: Vec<(&String, &(bool, u64, i64, u32))> = new.iter().filter(|(k, _)| !old.contains_key(*k)).collect();
    let mut out = Vec::new();
    for (name, v) in removed {
        if let Some(i) = added.iter().position(|(_, a)| a.0 == v.0 && a.1 == v.1 && a.2 == v.2) {
            let (new_name, _) = added.remove(i);
            if wanted(v.0) {
                out.push((FILE_ACTION_RENAMED_OLD_NAME, name.clone()));
                out.push((FILE_ACTION_RENAMED_NEW_NAME, new_name.clone()));
            }
        } else if wanted(v.0) {
            out.push((FILE_ACTION_REMOVED, name.clone()));
        }
    }
    for (name, v) in added {
        if wanted(v.0) {
            out.push((FILE_ACTION_ADDED, name.clone()));
        }
    }
    for (name, n) in new {
        let Some(o) = old.get(name) else { continue };
        let modified = (filter & FILE_NOTIFY_CHANGE_SIZE != 0 && o.1 != n.1 && !n.0)
            || (filter & FILE_NOTIFY_CHANGE_LAST_WRITE != 0 && o.2 != n.2)
            || (filter & FILE_NOTIFY_CHANGE_ATTRIBUTES != 0 && o.3 != n.3);
        if modified {
            out.push((FILE_ACTION_MODIFIED, name.clone()));
        }
    }
    out
}

/// FILE_NOTIFY_INFORMATION records (NextEntryOffset, Action,
/// FileNameLength in bytes, the name; each DWORD-aligned) into `buf`;
/// None when they do not fit (the changes are then lost, and the caller
/// gets 0 bytes, as on Windows).
fn records(list: &[(u32, String)], cap: usize) -> Option<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new();
    let mut last = 0usize;
    for (i, (action, name)) in list.iter().enumerate() {
        let units: Vec<u16> = name.encode_utf16().collect();
        let start = out.len();
        last = start;
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&action.to_le_bytes());
        out.extend_from_slice(&((units.len() * 2) as u32).to_le_bytes());
        for u in units {
            out.extend_from_slice(&u.to_le_bytes());
        }
        while out.len() % 4 != 0 {
            out.push(0);
        }
        if i + 1 < list.len() {
            let next = (out.len() - start) as u32;
            out[start..start + 4].copy_from_slice(&next.to_le_bytes());
        }
    }
    let _ = last;
    (out.len() <= cap).then_some(out)
}

/// Polls `dir` until its listing changes in a way `filter` reports; returns
/// the new listing and the changes. Polling every 100 ms: OSyx has no
/// file system change notification.
fn wait_changes(handle: u64, dir: &str, subtree: bool, filter: u32) -> Vec<(u32, String)> {
    loop {
        let old = WATCHES.lock().unwrap().get(&handle).cloned().unwrap_or_default();
        let mut new = Listing::new();
        list_dir(dir, "", subtree, 0, &mut new);
        let found = changes(&old, &new, filter);
        WATCHES.lock().unwrap().insert(handle, new);
        if !found.is_empty() {
            return found;
        }
        // A closed handle ends the watch.
        if objects::get(handle).is_none() {
            return Vec::new();
        }
        os_std::thread::sleep(core::time::Duration::from_millis(100));
    }
}

/// An overlapped directory read's work, run by a helper thread.
struct DirRead {
    handle: u64,
    dir: String,
    subtree: bool,
    filter: u32,
    buf: u64,
    len: u32,
    ov: u64,
}

extern "win64" fn dir_read_thread(arg: u64) -> u32 {
    // SAFETY: the Box made by ReadDirectoryChangesW for this thread.
    let r = *unsafe { alloc::boxed::Box::from_raw(arg as *mut DirRead) };
    let list = wait_changes(r.handle, &r.dir, r.subtree, r.filter);
    let bytes = records(&list, r.len as usize).unwrap_or_default();
    // SAFETY: the caller's buffer and OVERLAPPED, valid until the
    // operation completes (the Windows contract).
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), r.buf as *mut u8, bytes.len());
        kernel32::overlapped::set_bytes(r.ov, bytes.len() as u64);
        kernel32::overlapped::set_status(r.ov, 0);
        let event = kernel32::overlapped::event(r.ov) & !1;
        if event != 0 {
            kernel32::set_event(event);
        }
    }
    0
}

/// ReadDirectoryChangesW: the changes in a directory (FILE_LIST_DIRECTORY
/// handle) since the previous call; the first call starts the record.
/// Without an OVERLAPPED it waits for a change; with one it returns
/// ERROR_IO_PENDING and completes it (Internal, InternalHigh, the event).
/// Completion routines would need an alertable wait: ERROR_NOT_SUPPORTED.
#[allow(clippy::too_many_arguments)]
extern "win64" fn ReadDirectoryChangesW(handle: u64, buf: *mut u8, len: u32, subtree: Bool, filter: u32, returned: *mut u32, ov: *mut u8, routine: u64) -> Bool {
    let Some(Object::File(f)) = objects::get(handle) else { return fail(ERROR_INVALID_HANDLE, FALSE) };
    if !f.is_dir {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    if buf.is_null() || buf as usize % 4 != 0 || filter == 0 || filter & !0x17F != 0 {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    if routine != 0 {
        return fail(ERROR_NOT_SUPPORTED, FALSE);
    }
    let dir = f.path.clone();
    let subtree = subtree != 0;
    {
        let mut w = WATCHES.lock().unwrap();
        if !w.contains_key(&handle) {
            let mut first = Listing::new();
            list_dir(&dir, "", subtree, 0, &mut first);
            w.insert(handle, first);
        }
    }
    if !ov.is_null() {
        // SAFETY: the caller's OVERLAPPED.
        unsafe {
            kernel32::overlapped::set_status(ov as u64, 0x103); // STATUS_PENDING
            kernel32::overlapped::set_bytes(ov as u64, 0);
            let event = kernel32::overlapped::event(ov as u64) & !1;
            if event != 0 {
                kernel32::reset_event(event);
            }
        }
        let arg = alloc::boxed::Box::into_raw(alloc::boxed::Box::new(DirRead { handle, dir, subtree, filter, buf: buf as u64, len, ov: ov as u64 }));
        let t = kernel32::CreateThread(0, 0, dir_read_thread as *const () as u64, arg as u64, 0, core::ptr::null_mut());
        if t == 0 {
            // SAFETY: not handed to a thread.
            drop(unsafe { alloc::boxed::Box::from_raw(arg) });
            return fail(ERROR_NOT_ENOUGH_MEMORY, FALSE);
        }
        kernel32::CloseHandle(t);
        return fail(ERROR_IO_PENDING, FALSE);
    }
    let list = wait_changes(handle, &dir, subtree, filter);
    let bytes = records(&list, len as usize).unwrap_or_default();
    // SAFETY: the caller's buffer of `len` bytes and out count.
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), buf, bytes.len());
        if !returned.is_null() {
            *returned = bytes.len() as u32;
        }
    }
    TRUE
}

// ---- TCP table ------------------------------------------------------------------

fn tcp_state(name: &str) -> u32 {
    match name {
        "CLOSED" => 1,
        "LISTEN" => 2,
        "SYN-SENT" => 3,
        "SYN-RECEIVED" => 4,
        "ESTABLISHED" => 5,
        "FIN-WAIT-1" => 6,
        "FIN-WAIT-2" => 7,
        "CLOSE-WAIT" => 8,
        "CLOSING" => 9,
        "LAST-ACK" => 10,
        "TIME-WAIT" => 11,
        _ => 12, // MIB_TCP_STATE_DELETE_TCB
    }
}

/// GetExtendedTcpTable for AF_INET (OSyx has no IPv6 sockets: an AF_INET6
/// table is empty): the basic, owner-PID and owner-module classes, with
/// listeners, connections or both. The kernel's socket list records no
/// owner, so dwOwningPid is 0 and the module information is empty.
extern "win64" fn GetExtendedTcpTable(table: *mut u8, size: *mut u32, sorted: Bool, family: u32, class: u32, reserved: u32) -> u32 {
    const AF_INET: u32 = 2;
    const AF_INET6: u32 = 23;
    if size.is_null() || reserved != 0 || class > 8 || !matches!(family, AF_INET | AF_INET6) {
        return ERROR_INVALID_PARAMETER;
    }
    let sockets = if family == AF_INET { os_std::net::sockets().unwrap_or_default() } else { Vec::new() };
    let mut rows: Vec<(u32, [u8; 4], u16, [u8; 4], u16)> = sockets
        .iter()
        .filter(|s| osyx_abi::ipc::field_str(&s.kind) == "tcp" && s.internal == 0)
        .map(|s| (tcp_state(osyx_abi::ipc::field_str(&s.state)), s.local.addr, s.local.port, s.remote.addr, s.remote.port))
        .filter(|r| match class % 3 {
            0 => r.0 == 2,
            1 => r.0 != 2,
            _ => true,
        })
        .collect();
    if sorted != 0 {
        rows.sort_by_key(|r| (u32::from_be_bytes(r.1), r.2, u32::from_be_bytes(r.3), r.4));
    }
    let (row, header) = match class / 3 {
        0 => (20usize, 4usize),
        1 => (24, 4),
        _ => (160, 8),
    };
    let need = (header + row * rows.len().max(1)) as u32;
    // SAFETY: the caller's in/out size.
    let cap = unsafe { *size };
    if table.is_null() || cap < need {
        // SAFETY: as above.
        unsafe { *size = need };
        return ERROR_INSUFFICIENT_BUFFER;
    }
    // SAFETY: the caller's buffer of `cap` >= `need` bytes.
    unsafe {
        core::ptr::write_bytes(table, 0, need as usize);
        let t = table as u64;
        teb::put(t, 0, rows.len() as u32);
        for (i, r) in rows.iter().enumerate() {
            let p = t + (header + i * row) as u64;
            teb::put(p, 0, r.0);
            teb::put(p, 4, u32::from_ne_bytes(r.1));
            teb::put(p, 8, r.2.to_be() as u32);
            teb::put(p, 12, u32::from_ne_bytes(r.3));
            teb::put(p, 16, if r.0 == 2 { 0 } else { r.4.to_be() as u32 });
        }
    }
    0
}

// ---- COM and shell -----------------------------------------------------------

/// IID_IClientSecurity {0000013D-0000-0000-C000-000000000046}.
const IID_ICLIENT_SECURITY: [u8; 16] = [0x3D, 0x01, 0, 0, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46];

/// CoSetProxyBlanket: sets the authentication of a proxy through its
/// IClientSecurity (QueryInterface, SetBlanket, Release). Objects that run
/// in this process are not proxies and have no IClientSecurity:
/// E_NOINTERFACE, as on Windows.
#[allow(clippy::too_many_arguments)]
extern "win64" fn CoSetProxyBlanket(proxy: u64, authn: u32, authz: u32, principal: u64, level: u32, imp: u32, info: u64, caps: u32) -> u32 {
    const E_INVALIDARG: u32 = 0x8007_0057;
    if proxy == 0 {
        return E_INVALIDARG;
    }
    let mut security = 0u64;
    let width = if narrow() { 4 } else { 8 };
    // SAFETY: a COM interface: its first word is the vtable; QueryInterface
    // is slot 0, Release slot 2, IClientSecurity::SetBlanket slot 4.
    let slot = |obj: u64, n: u64| unsafe {
        let vtbl = if width == 4 { *(obj as *const u32) as u64 } else { *(obj as *const u64) };
        if width == 4 { *((vtbl + 4 * n) as *const u32) as u64 } else { *((vtbl + 8 * n) as *const u64) }
    };
    let hr = crate::wow64::callback(slot(proxy, 0), &[proxy, IID_ICLIENT_SECURITY.as_ptr() as u64, &mut security as *mut u64 as u64]) as u32;
    if hr != 0 || security == 0 {
        return hr;
    }
    let security = if width == 4 { security & 0xFFFF_FFFF } else { security };
    let hr = crate::wow64::callback(slot(security, 4), &[security, proxy, authn as u64, authz as u64, principal, level as u64, imp as u64, info, caps as u64]) as u32;
    crate::wow64::callback(slot(security, 2), &[security]);
    hr
}


extern "win64" fn malloc_query(this: u64, iid: *const u8, out: *mut u64) -> u32 {
    // IID_IUnknown {00000000-...-46} and IID_IMalloc {00000002-...-46}.
    const E_NOINTERFACE: u32 = 0x8000_4002;
    if out.is_null() {
        return 0x8000_4003; // E_POINTER
    }
    // SAFETY: the caller's IID.
    let id = unsafe { core::slice::from_raw_parts(iid, 16) };
    let tail = [0u8, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46];
    if id[4..] == tail && (id[0] == 0 || id[0] == 2) && id[1..4] == [0, 0, 0] {
        // SAFETY: the caller's out pointer.
        unsafe { *out = this };
        return 0;
    }
    // SAFETY: as above.
    unsafe { *out = 0 };
    E_NOINTERFACE
}
extern "win64" fn malloc_count(_this: u64) -> u32 {
    1 // a static object: reference counts are not kept
}
extern "win64" fn malloc_alloc(_this: u64, size: usize) -> u64 {
    crate::ole32::CoTaskMemAlloc(size)
}
extern "win64" fn malloc_realloc(_this: u64, p: u64, size: usize) -> u64 {
    crate::ole32::CoTaskMemRealloc(p, size)
}
extern "win64" fn malloc_free(_this: u64, p: u64) {
    crate::ole32::CoTaskMemFree(p)
}
extern "win64" fn malloc_size(_this: u64, p: u64) -> usize {
    if p == 0 { usize::MAX } else { kernel32::heap_size(p) }
}
extern "win64" fn malloc_did_alloc(_this: u64, p: u64) -> i32 {
    if p == 0 { -1 } else { (kernel32::heap_size(p) != 0) as i32 }
}
extern "win64" fn malloc_minimize(_this: u64) {}

/// The shell's task allocator (SHGetMalloc): an IMalloc over
/// CoTaskMemAlloc, made once: its first word points at its vtable.
static MALLOC_READY: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// SHGetMalloc: the shell allocator (64-bit callers; a 32-bit program's
/// IMalloc would need 32-bit methods: E_NOTIMPL). E_INVALIDARG for NULL.
extern "win64" fn SHGetMalloc(out: *mut u64) -> u32 {
    if out.is_null() {
        return 0x8007_0057;
    }
    if narrow() {
        // SAFETY: the caller's out pointer.
        unsafe { core::ptr::write_unaligned(out as *mut u32, 0) };
        return 0x8000_4001; // E_NOTIMPL
    }
    let mut obj = MALLOC_READY.load(Ordering::Acquire);
    if obj == 0 {
        let vtbl: &'static mut [u64; 9] = alloc::boxed::Box::leak(alloc::boxed::Box::new([
            malloc_query as *const () as u64,
            malloc_count as *const () as u64,
            malloc_count as *const () as u64,
            malloc_alloc as *const () as u64,
            malloc_realloc as *const () as u64,
            malloc_free as *const () as u64,
            malloc_size as *const () as u64,
            malloc_did_alloc as *const () as u64,
            malloc_minimize as *const () as u64,
        ]));
        let m: &'static mut u64 = alloc::boxed::Box::leak(alloc::boxed::Box::new(vtbl.as_ptr() as u64));
        obj = m as *mut u64 as u64;
        if let Err(other) = MALLOC_READY.compare_exchange(0, obj, Ordering::AcqRel, Ordering::Acquire) {
            obj = other;
        }
    }
    // SAFETY: the caller's out pointer.
    unsafe { *out = obj };
    0
}

/// The icon groups (RT_GROUP_ICON) of a PE file and their icons
/// (RT_ICON), read from the file.
struct IconFile {
    bytes: Vec<u8>,
}

impl IconFile {
    fn open(win: &str) -> Option<IconFile> {
        let (p, true) = kernel32::locate(win).ok()? else { return None };
        Some(IconFile { bytes: os_std::fs::read(&p).ok()? })
    }

    /// The ids of the icon groups, in directory order.
    fn groups(&self) -> Vec<u16> {
        let Ok(pe) = osyx_pe::PeFile::parse_any(&self.bytes) else { return Vec::new() };
        (1..=u16::MAX).take(4096).filter(|&id| pe.resource(14, Some(id), None).ok().flatten().is_some()).collect()
    }

    /// The icon of group `id` closest to `size` pixels.
    fn icon(&self, id: u16, size: u32) -> Option<Vec<u8>> {
        let pe = osyx_pe::PeFile::parse_any(&self.bytes).ok()?;
        let group = pe.resource(14, Some(id), None).ok()??;
        // GRPICONDIR: reserved, type, count; 14-byte entries: width,
        // height, colours, reserved, planes, bit count, bytes, id.
        let count = u16::from_le_bytes([*group.get(4)?, *group.get(5)?]) as usize;
        let mut best: Option<(u32, u16)> = None;
        for i in 0..count {
            let e = group.get(6 + 14 * i..6 + 14 * (i + 1))?;
            let w = if e[0] == 0 { 256 } else { e[0] as u32 };
            let icon_id = u16::from_le_bytes([e[12], e[13]]);
            let score = w.abs_diff(size) * 64 + 32 - (u16::from_le_bytes([e[6], e[7]]) as u32).min(32);
            if best.is_none_or(|b| score < b.0) {
                best = Some((score, icon_id));
            }
        }
        pe.resource(3, Some(best?.1), None).ok()?.map(|d| d.to_vec())
    }
}

/// ExtractIconExA/W: icons of a PE file's icon groups, the large (32x32)
/// and small (16x16) image of each from `index` on; index -1 with no
/// output asks for the number of groups. A negative index names a
/// resource id. Returns how many icons were extracted.
fn extract_icons(file: Option<String>, index: i32, large: *mut u64, small: *mut u64, n: u32) -> u32 {
    let Some(f) = file.and_then(|f| IconFile::open(&f)) else { return 0 };
    let groups = f.groups();
    if index == -1 && large.is_null() && small.is_null() {
        return groups.len() as u32;
    }
    let selected: Vec<u16> = if index < 0 {
        groups.iter().copied().filter(|&g| g as i32 == -index).collect()
    } else {
        groups.iter().copied().skip(index as usize).take(n as usize).collect()
    };
    let width = if narrow() { 4 } else { 8 };
    let mut done = 0u32;
    for (i, g) in selected.iter().enumerate() {
        for (out, size) in [(large, 32u32), (small, 16)] {
            if out.is_null() {
                continue;
            }
            let h = f.icon(*g, size).map_or(0, |data| crate::user32::icon_from_resource(&data, size));
            // SAFETY: the caller's arrays of `n` HICONs.
            unsafe {
                if width == 4 {
                    core::ptr::write_unaligned((out as *mut u32).add(i), h as u32);
                } else {
                    *out.add(i) = h;
                }
            }
        }
        done += 1;
    }
    done
}

extern "win64" fn ExtractIconExW(file: *const u16, index: i32, large: *mut u64, small: *mut u64, n: u32) -> u32 {
    // SAFETY: a NUL-terminated name.
    extract_icons(unsafe { path::wide_to_string(file) }, index, large, small, n)
}

extern "win64" fn ExtractIconExA(file: *const u8, index: i32, large: *mut u64, small: *mut u64, n: u32) -> u32 {
    // SAFETY: a NUL-terminated name.
    extract_icons(unsafe { path::ansi_to_string(file) }, index, large, small, n)
}

// ---- hid.dll --------------------------------------------------------------------

/// The HID device interface class {4D1E55B2-F16F-11CF-88CB-001111000030}.
const HID_GUID: [u8; 16] = [0xB2, 0x55, 0x1E, 0x4D, 0x6F, 0xF1, 0xCF, 0x11, 0x88, 0xCB, 0x00, 0x11, 0x11, 0x00, 0x00, 0x30];

extern "win64" fn HidD_GetHidGuid(out: *mut u8) {
    if !out.is_null() {
        // SAFETY: the caller's GUID.
        unsafe { core::ptr::copy_nonoverlapping(HID_GUID.as_ptr(), out, 16) };
    }
}

/// The HID class functions work on handles of HID device interfaces.
/// OSyx exposes no HID device to Windows programs (SetupAPI lists no HID
/// interface), so no handle a program can open is one: the requests fail
/// as Windows fails them for a handle that is no HID device
/// (ERROR_INVALID_FUNCTION).
extern "win64" fn HidD_GetAttributes(h: u64, _attributes: *mut u8) -> u8 {
    if objects::get(h).is_none() {
        return fail(ERROR_INVALID_HANDLE, 0);
    }
    fail(ERROR_INVALID_FUNCTION, 0)
}

extern "win64" fn HidD_GetPreparsedData(h: u64, out: *mut u64) -> u8 {
    if !out.is_null() {
        // SAFETY: the caller's out pointer.
        unsafe { core::ptr::write_unaligned(out as *mut u32, 0) };
    }
    if objects::get(h).is_none() {
        return fail(ERROR_INVALID_HANDLE, 0);
    }
    fail(ERROR_INVALID_FUNCTION, 0)
}

/// No preparsed data is ever handed out (see HidD_GetAttributes).
extern "win64" fn HidD_FreePreparsedData(_data: u64) -> u8 {
    0
}

/// HIDP_STATUS_INVALID_PREPARSED_DATA: no preparsed data exists.
extern "win64" fn HidP_GetCaps(_data: u64, _caps: *mut u8) -> u32 {
    0xC011_0001
}

pub fn kernel32_exports() -> Vec<(&'static str, u64)> {
    export_table![
        CreateToolhelp32Snapshot,
        Process32First,
        Process32Next,
        Process32FirstW,
        Process32NextW,
        Module32First,
        Module32Next,
        Module32FirstW,
        Module32NextW,
        GetVolumeNameForVolumeMountPointW,
        GetVolumePathNamesForVolumeNameW,
        ReadDirectoryChangesW,
    ]
}

pub fn iphlpapi_exports() -> Vec<(&'static str, u64)> {
    export_table![GetExtendedTcpTable]
}

pub fn ole32_exports() -> Vec<(&'static str, u64)> {
    export_table![CoSetProxyBlanket]
}

pub fn shell32_exports() -> Vec<(&'static str, u64)> {
    export_table![SHGetMalloc, ExtractIconExW, ExtractIconExA]
}

pub fn hid_exports() -> Vec<(&'static str, u64)> {
    export_table![HidD_GetHidGuid, HidD_GetAttributes, HidD_GetPreparsedData, HidD_FreePreparsedData, HidP_GetCaps]
}
