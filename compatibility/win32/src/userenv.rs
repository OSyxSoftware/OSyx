//! userenv: AppContainer SIDs and profiles, environment blocks and the
//! group-policy critical sections. The SID of an AppContainer name is
//! derived exactly as on Windows: S-1-15-2 followed by the first seven
//! DWORDs of the SHA-256 of the lower-cased UTF-16LE name. Running
//! AppContainers is not supported on OSyx (tokens.rs); profiles are made
//! (their folder) and the SIDs are the ones such a container would have.
//! OSyx has no group-policy engine: policy never changes, so a
//! registered notification never fires.
#![allow(non_snake_case)]
use crate::{error::*, kernel32::{self, fail}, objects, path, security::sid};
use alloc::{string::String, vec::Vec};
use os_std::sync::Mutex;

const E_INVALIDARG: u32 = 0x8007_0057;
const E_OUTOFMEMORY: u32 = 0x8007_000e;
/// AppContainer names are 1 to 64 characters.
const MAX_NAME: usize = 64;

/// The AppContainer SID of `name`.
pub fn appcontainer_sid(name: &str) -> Vec<u8> {
    let lower: Vec<u8> = name.chars().flat_map(|c| c.to_lowercase()).collect::<alloc::string::String>().encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let hash = osyx_crypto::Sha256::digest(&lower);
    let mut sub = alloc::vec![2u32];
    sub.extend((0..7).map(|i| u32::from_le_bytes([hash[i * 4], hash[i * 4 + 1], hash[i * 4 + 2], hash[i * 4 + 3]])));
    sid(15, &sub)
}

extern "win64" fn DeriveAppContainerSidFromAppContainerName(name: *const u16, out: *mut u64) -> u32 {
    if out.is_null() {
        return E_INVALIDARG;
    }
    // SAFETY: NULL or a NUL-terminated name.
    let Some(name) = (unsafe { path::wide_to_string(name) }).filter(|n| !n.is_empty() && n.encode_utf16().count() <= MAX_NAME) else {
        return E_INVALIDARG;
    };
    let s = appcontainer_sid(&name);
    let Some(p) = kernel32::heap_alloc(s.len(), false) else { return E_OUTOFMEMORY };
    // SAFETY: a fresh block of s.len() bytes (freed with FreeSid); the caller's out pointer.
    unsafe {
        core::ptr::copy_nonoverlapping(s.as_ptr(), p as *mut u8, s.len());
        *out = p;
    }
    0
}

const HRESULT_ALREADY_EXISTS: u32 = 0x8007_00B7;
const ERROR_NOT_OWNER: u32 = 288;

/// CreateAppContainerProfile: the profile's folder,
/// %LOCALAPPDATA%\Packages\<name>, and the container's SID. An existing
/// profile is HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS), as on Windows.
extern "win64" fn CreateAppContainerProfile(name: *const u16, display: *const u16, _description: *const u16, _caps: u64, _count: u32, out: *mut u64) -> u32 {
    if out.is_null() || display.is_null() {
        return E_INVALIDARG;
    }
    // SAFETY: NULL or a NUL-terminated name.
    let Some(name) = (unsafe { path::wide_to_string(name) }).filter(|n| !n.is_empty() && n.encode_utf16().count() <= MAX_NAME && !n.contains(['\\', '/'])) else {
        return E_INVALIDARG;
    };
    let base = kernel32::env_get("LOCALAPPDATA").unwrap_or_else(|| String::from("C:\\Users\\user\\AppData\\Local"));
    // The Packages folder may not exist yet: resolve the profile below the
    // local application data folder itself.
    let Ok((local, _)) = kernel32::locate(&base) else { return E_INVALIDARG };
    let dir = alloc::format!("{}/Packages/{name}", local.trim_end_matches('/'));
    if os_sys::stat(&dir).is_ok() {
        return HRESULT_ALREADY_EXISTS;
    }
    if os_std::fs::create_dir_all(&dir).is_err() {
        return 0x8007_0005; // E_ACCESSDENIED
    }
    DeriveAppContainerSidFromAppContainerName(name.encode_utf16().chain([0]).collect::<Vec<u16>>().as_ptr(), out)
}

/// The system's variables (the same for every user), taken from this
/// process, which OSyx starts with them.
const SYSTEM_VARIABLES: [&str; 15] = [
    "ALLUSERSPROFILE", "CommonProgramFiles", "COMPUTERNAME", "ComSpec", "NUMBER_OF_PROCESSORS", "OS", "Path", "PATHEXT",
    "PROCESSOR_ARCHITECTURE", "ProgramData", "ProgramFiles", "PUBLIC", "SystemDrive", "SystemRoot", "windir",
];

/// The variables of the user a token names: this process's for its own
/// user, and the profile folders under C:\Users for another.
fn user_variables(uid: u32) -> Vec<(String, String)> {
    let name = crate::accounts::user_name(uid);
    let own = |k: &str| kernel32::env_get(k);
    let profile = if uid == os_sys::getuid().0 { own("USERPROFILE") } else { None }.unwrap_or_else(|| alloc::format!("C:\\Users\\{name}"));
    let mine = uid == os_sys::getuid().0;
    let pick = |k: &str, default: String| if mine { own(k).unwrap_or(default) } else { default };
    let local = pick("LOCALAPPDATA", alloc::format!("{profile}\\AppData\\Local"));
    let temp = pick("TEMP", alloc::format!("{local}\\Temp"));
    alloc::vec![
        (String::from("USERNAME"), name),
        (String::from("USERDOMAIN"), crate::accounts::computer_name()),
        (String::from("USERPROFILE"), profile.clone()),
        (String::from("HOMEDRIVE"), String::from("C:")),
        (String::from("HOMEPATH"), String::from(profile.get(2..).unwrap_or("\\"))),
        (String::from("APPDATA"), pick("APPDATA", alloc::format!("{profile}\\AppData\\Roaming"))),
        (String::from("LOCALAPPDATA"), local),
        (String::from("TEMP"), temp.clone()),
        (String::from("TMP"), temp),
    ]
}

/// CreateEnvironmentBlock: a sorted UTF-16 NAME=value block: the system's
/// variables, with a token the variables of its user, and when inheriting
/// this process's environment over them. Freed with
/// DestroyEnvironmentBlock.
extern "win64" fn CreateEnvironmentBlock(out: *mut u64, token: u64, inherit: i32) -> i32 {
    if out.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let all = kernel32::env_vars();
    let mut vars: Vec<(String, String)> = SYSTEM_VARIABLES.iter().filter_map(|k| all.iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).cloned()).collect();
    let set = |vars: &mut Vec<(String, String)>, (k, v): (String, String)| {
        vars.retain(|(n, _)| !n.eq_ignore_ascii_case(&k));
        vars.push((k, v));
    };
    if token == 0 {
        // Without a user, the system's temporary folder.
        let root = all.iter().find(|(n, _)| n.eq_ignore_ascii_case("SystemRoot")).map_or(String::from("C:\\Windows"), |(_, v)| v.clone());
        set(&mut vars, (String::from("TEMP"), alloc::format!("{root}\\TEMP")));
        set(&mut vars, (String::from("TMP"), alloc::format!("{root}\\TEMP")));
    } else {
        let t = match crate::tokens::token(token) {
            Ok(t) => t,
            Err(e) => return fail(e, 0),
        };
        for v in user_variables(t.uid) {
            set(&mut vars, v);
        }
    }
    if inherit != 0 {
        for v in all {
            set(&mut vars, v);
        }
    }
    vars.sort_by_key(|(k, _)| k.to_uppercase());
    let mut units: Vec<u16> = Vec::new();
    for (k, v) in &vars {
        units.extend(k.encode_utf16());
        units.push(b'=' as u16);
        units.extend(v.encode_utf16());
        units.push(0);
    }
    units.push(0);
    if vars.is_empty() {
        units.push(0);
    }
    let Some(p) = kernel32::heap_alloc(units.len() * 2, false) else { return fail(ERROR_NOT_ENOUGH_MEMORY, 0) };
    // SAFETY: `p` holds `units.len()` u16s; the caller's out pointer.
    unsafe {
        core::ptr::copy_nonoverlapping(units.as_ptr(), p as *mut u16, units.len());
        *out = p;
    }
    1
}

extern "win64" fn DestroyEnvironmentBlock(block: u64) -> i32 {
    if block == 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    kernel32::heap_free(block);
    1
}

/// The machine and user policy critical sections, mutexes as on Windows:
/// (owning thread, acquisitions). The owner may enter again; each enter
/// has its leave.
static POLICY_LOCKS: [Mutex<(u64, u32)>; 2] = [Mutex::new((0, 0)), Mutex::new((0, 0))];
const POLICY_HANDLES: [u64; 2] = [0x9011_0000, 0x9011_0004];

extern "win64" fn EnterCriticalPolicySection(machine: i32) -> u64 {
    let i = (machine != 0) as usize;
    let me = os_sys::gettid();
    loop {
        {
            let mut l = POLICY_LOCKS[i].lock().unwrap();
            if l.1 == 0 || l.0 == me {
                *l = (me, l.1 + 1);
                return POLICY_HANDLES[i];
            }
        }
        os_std::thread::sleep(core::time::Duration::from_millis(1));
    }
}

extern "win64" fn LeaveCriticalPolicySection(h: u64) -> i32 {
    let Some(i) = POLICY_HANDLES.iter().position(|&p| p == h) else { return fail(ERROR_INVALID_HANDLE, 0) };
    let mut l = POLICY_LOCKS[i].lock().unwrap();
    if l.1 == 0 || l.0 != os_sys::gettid() {
        return fail(ERROR_NOT_OWNER, 0);
    }
    l.1 -= 1;
    1
}

/// Events registered for policy changes, with machine or user policy.
static GP_EVENTS: Mutex<Vec<(u64, bool)>> = Mutex::new(Vec::new());

extern "win64" fn RegisterGPNotification(event: u64, machine: i32) -> i32 {
    if !matches!(objects::get(event), Some(objects::Object::Event(_))) {
        return fail(ERROR_INVALID_HANDLE, 0);
    }
    GP_EVENTS.lock().unwrap().push((event, machine != 0));
    1
}

extern "win64" fn UnregisterGPNotification(event: u64) -> i32 {
    let mut events = GP_EVENTS.lock().unwrap();
    match events.iter().position(|(e, _)| *e == event) {
        Some(i) => {
            events.remove(i);
            1
        }
        None => fail(ERROR_INVALID_PARAMETER, 0),
    }
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        DeriveAppContainerSidFromAppContainerName,
        CreateAppContainerProfile,
        CreateEnvironmentBlock,
        DestroyEnvironmentBlock,
        EnterCriticalPolicySection,
        LeaveCriticalPolicySection,
        RegisterGPNotification,
        UnregisterGPNotification,
    ]
}
