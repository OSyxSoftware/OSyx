//! netapi32.dll: what a stand-alone workstation answers about its domain
//! membership and local users. OSyx machines join no domain and no Azure AD
//! tenant: they are in the workgroup "WORKGROUP". Users are the local
//! accounts of /etc/passwd (accounts.rs), uid 0 being the administrator.
//! Only the local machine is answered; a named server is not reachable.
#![allow(non_snake_case)]
use crate::{error::*, path};
use alloc::vec::Vec;

const NERR_SUCCESS: u32 = 0;
const NERR_USER_NOT_FOUND: u32 = 2221;
const ERROR_INVALID_LEVEL: u32 = 124;
const ERROR_BAD_NETPATH: u32 = 53;

const WORKGROUP: &str = "WORKGROUP";
/// NETSETUP_JOIN_STATUS: NetSetupWorkgroupName.
const NET_SETUP_WORKGROUP_NAME: u32 = 2;

/// Whether `server` names this machine: NULL, empty, or its own name
/// (with or without the leading backslashes).
///
/// # Safety
/// `server` is NULL or a NUL-terminated string.
unsafe fn local(server: *const u16) -> bool {
    // SAFETY: the caller's contract.
    match unsafe { path::wide_to_string(server) } {
        None => true,
        Some(s) => {
            let s = s.trim_start_matches('\\');
            s.is_empty() || s.eq_ignore_ascii_case(&crate::accounts::computer_name())
        }
    }
}

/// A buffer NetApiBufferFree releases: `fixed` bytes of structure followed
/// by the strings, whose addresses are returned in order (0 for None).
fn buffer(fixed: usize, strings: &[Option<&str>]) -> Option<(u64, Vec<u64>)> {
    let wide: Vec<Option<Vec<u16>>> = strings.iter().map(|s| s.map(|s| s.encode_utf16().chain([0]).collect())).collect();
    let total = fixed + wide.iter().flatten().map(|w| w.len() * 2).sum::<usize>();
    let base = crate::kernel32::heap_alloc(total.max(8), true)?;
    let mut at = base + fixed as u64;
    let mut addrs = Vec::new();
    for w in &wide {
        match w {
            Some(w) => {
                // SAFETY: inside the fresh block of `total` bytes.
                unsafe { core::ptr::copy_nonoverlapping(w.as_ptr(), at as *mut u16, w.len()) };
                addrs.push(at);
                at += w.len() as u64 * 2;
            }
            None => addrs.push(0),
        }
    }
    Some((base, addrs))
}

fn put<T>(p: u64, v: T) {
    // SAFETY: inside a block made by `buffer` for this structure.
    unsafe { core::ptr::write_unaligned(p as *mut T, v) };
}

extern "win64" fn NetApiBufferFree(p: u64) -> u32 {
    if p != 0 {
        crate::kernel32::heap_free(p);
    }
    NERR_SUCCESS
}

/// The workgroup the machine is in.
extern "win64" fn NetGetJoinInformation(server: *const u16, name: *mut u64, status: *mut u32) -> u32 {
    if name.is_null() || status.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    // SAFETY: NULL or a NUL-terminated name.
    if !unsafe { local(server) } {
        return ERROR_BAD_NETPATH;
    }
    let Some((base, _)) = buffer(0, &[Some(WORKGROUP)]) else { return ERROR_NOT_ENOUGH_MEMORY };
    // SAFETY: the caller's out values.
    unsafe {
        *name = base;
        *status = NET_SETUP_WORKGROUP_NAME;
    }
    NERR_SUCCESS
}

/// Not joined to Azure AD: S_OK with no information.
extern "win64" fn NetGetAadJoinInformation(_tenant: *const u16, info: *mut u64) -> u32 {
    if info.is_null() {
        return 0x8007_0057; // E_INVALIDARG
    }
    // SAFETY: the caller's out pointer.
    unsafe { *info = 0 };
    0
}

extern "win64" fn NetFreeAadJoinInformation(info: u64) {
    if info != 0 {
        crate::kernel32::heap_free(info);
    }
}

const DS_ROLE_PRIMARY_DOMAIN_INFO_BASIC: u32 = 1;
const DS_ROLE_UPGRADE_STATUS: u32 = 2;
const DS_ROLE_OPERATION_STATE: u32 = 3;

/// A stand-alone workstation (DsRole_RoleStandaloneWorkstation) in the
/// workgroup, never upgrading, with no role operation running.
extern "win64" fn DsRoleGetPrimaryDomainInformation(server: *const u16, level: u32, out: *mut u64) -> u32 {
    if out.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    // SAFETY: NULL or a NUL-terminated name.
    if !unsafe { local(server) } {
        return ERROR_BAD_NETPATH;
    }
    let block = match level {
        // MachineRole, Flags, DomainNameFlat, DomainNameDns,
        // DomainForestName, DomainGuid.
        DS_ROLE_PRIMARY_DOMAIN_INFO_BASIC => buffer(48, &[Some(WORKGROUP)]).map(|(base, s)| {
            put(base + 8, s[0]);
            base
        }),
        // OperationState, PreviousServerState: both 0.
        DS_ROLE_UPGRADE_STATUS => buffer(8, &[]).map(|(base, _)| base),
        // DsRoleOperationIdle.
        DS_ROLE_OPERATION_STATE => buffer(4, &[]).map(|(base, _)| base),
        _ => return ERROR_INVALID_PARAMETER,
    };
    let Some(base) = block else { return ERROR_NOT_ENOUGH_MEMORY };
    // SAFETY: the caller's out pointer.
    unsafe { *out = base };
    NERR_SUCCESS
}

extern "win64" fn DsRoleFreeMemory(p: u64) {
    if p != 0 {
        crate::kernel32::heap_free(p);
    }
}

const USER_PRIV_USER: u32 = 1;
const USER_PRIV_ADMIN: u32 = 2;
/// UF_SCRIPT | UF_NORMAL_ACCOUNT | UF_DONT_EXPIRE_PASSWD.
const USER_FLAGS: u32 = 0x0001 | 0x0200 | 0x1_0000;

/// NetUserGetInfo levels 0 (name), 1 (name, privilege, flags), 10 (name,
/// comments, full name) and 20 (name, full name, comment, flags, RID) for
/// a local account.
extern "win64" fn NetUserGetInfo(server: *const u16, user: *const u16, level: u32, out: *mut u64) -> u32 {
    if out.is_null() {
        return ERROR_INVALID_PARAMETER;
    }
    // SAFETY: NULL or NUL-terminated strings.
    if !unsafe { local(server) } {
        return ERROR_BAD_NETPATH;
    }
    // SAFETY: as above.
    let Some(name) = (unsafe { path::wide_to_string(user) }) else { return ERROR_INVALID_PARAMETER };
    let me = os_sys::getuid().0;
    let uid = if name.eq_ignore_ascii_case(&crate::accounts::user_name(me)) {
        me
    } else {
        match crate::accounts::local_accounts().into_iter().find(|a| a.name.eq_ignore_ascii_case(&name)) {
            Some(a) => a.uid,
            None => return NERR_USER_NOT_FOUND,
        }
    };
    let privilege = if uid == 0 { USER_PRIV_ADMIN } else { USER_PRIV_USER };
    let block = match level {
        0 => buffer(8, &[Some(&name)]).map(|(b, s)| {
            put(b, s[0]);
            b
        }),
        // name, password, password_age, priv, home_dir, comment, flags,
        // script_path.
        1 => buffer(56, &[Some(&name), Some(""), Some(""), Some("")]).map(|(b, s)| {
            put(b, s[0]);
            put(b + 20, privilege);
            put(b + 24, s[1]);
            put(b + 32, s[2]);
            put(b + 40, USER_FLAGS);
            put(b + 48, s[3]);
            b
        }),
        // name, comment, usr_comment, full_name.
        10 => buffer(32, &[Some(&name), Some(""), Some(""), Some("")]).map(|(b, s)| {
            for (i, a) in s.iter().enumerate() {
                put(b + 8 * i as u64, *a);
            }
            b
        }),
        // name, full_name, comment, flags, user_id.
        20 => buffer(32, &[Some(&name), Some(""), Some("")]).map(|(b, s)| {
            put(b, s[0]);
            put(b + 8, s[1]);
            put(b + 16, s[2]);
            put(b + 24, USER_FLAGS);
            // The last subauthority of the user's SID (S-1-22-1-uid).
            put(b + 28, uid);
            b
        }),
        _ => return ERROR_INVALID_LEVEL,
    };
    let Some(base) = block else { return ERROR_NOT_ENOUGH_MEMORY };
    // SAFETY: the caller's out pointer.
    unsafe { *out = base };
    NERR_SUCCESS
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        NetApiBufferFree,
        NetGetJoinInformation,
        NetGetAadJoinInformation,
        NetFreeAadJoinInformation,
        DsRoleGetPrimaryDomainInformation,
        DsRoleFreeMemory,
        NetUserGetInfo
    ]
}
