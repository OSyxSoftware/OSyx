//! Win64 SID/ACL/security-descriptor data APIs and queryable process tokens.
//! Tokens derive their identity from the kernel's actual uid/gid. This is
//! not impersonation, a Windows sandbox, or an ACL enforcement service.
//! Unsupported token classes/remote process queries fail explicitly.
#![allow(non_snake_case)]
use crate::{
    error::*,
    kernel32::fail,
    objects::{self, Object},
};
use alloc::{sync::Arc, vec::Vec};
use core::ptr::{copy_nonoverlapping, read_unaligned, write_unaligned};

const INVALID_SID: u32 = 1337;
const INVALID_ACL: u32 = 1336;
const BAD_DESCRIPTOR: u32 = 1338;
const REVISION_MISMATCH: u32 = 1306;
const NO_SPACE: u32 = 1344;
const TOKEN_QUERY: u32 = 8;
const TOKEN_ALL: u32 = 0x000f_01ff;

/// Access is per handle, including copies made with DuplicateHandle.
#[derive(Clone)]
pub struct Token {
    pub uid: u32,
    pub gid: u32,
    pub access: u32,
}
/// Default attributes are implementable with native credentials. Custom
/// DACLs and inheritable handles require additional kernel/object support;
/// never silently ignore them while reporting successful object creation.
pub fn validate_attributes(p: u64) -> Result<(), u32> {
    if p == 0 {
        return Ok(());
    }
    let p = p as *const u8;
    if unsafe { rd::<u32>(p, 0) } != 24 {
        return Err(ERROR_INVALID_PARAMETER);
    }
    if unsafe { rd::<u64>(p, 8) } != 0 || unsafe { rd::<i32>(p, 16) } != 0 {
        return Err(ERROR_NOT_SUPPORTED);
    }
    Ok(())
}
pub fn token_access(access: u32) -> Result<u32, u32> {
    let mut mapped = access & 0x0fff_ffff;
    for (generic, rights) in
        [(0x8000_0000, 0x20008), (0x4000_0000, 0x200e0), (0x2000_0000, 0x20000), (0x1000_0000, TOKEN_ALL)]
    {
        if access & generic != 0 {
            mapped |= rights;
        }
    }
    if mapped & 0x0200_0000 != 0 {
        mapped = (mapped & !0x0200_0000) | TOKEN_ALL;
    }
    if mapped & !TOKEN_ALL != 0 {
        return Err(ERROR_ACCESS_DENIED);
    }
    Ok(mapped)
}
unsafe fn rd<T: Copy>(p: *const u8, at: usize) -> T {
    unsafe { read_unaligned(p.add(at).cast()) }
}
unsafe fn wr<T>(p: *mut u8, at: usize, value: T) {
    unsafe { write_unaligned(p.add(at).cast(), value) }
}
unsafe fn sid_len(sid: *const u8) -> Option<usize> {
    if sid.is_null() || unsafe { *sid } != 1 {
        return None;
    }
    let count = unsafe { *sid.add(1) };
    (count <= 15).then_some(8 + count as usize * 4)
}
fn sid(authority: u64, sub: &[u32]) -> Vec<u8> {
    let mut data = alloc::vec![1, sub.len() as u8];
    data.extend_from_slice(&authority.to_be_bytes()[2..]);
    for n in sub {
        data.extend_from_slice(&n.to_le_bytes());
    }
    data
}
unsafe fn same_sid(a: *const u8, b: *const u8) -> bool {
    let Some(n) = (unsafe { sid_len(a) }) else { return false };
    if unsafe { sid_len(b) } != Some(n) {
        return false;
    }
    unsafe { core::slice::from_raw_parts(a, n) == core::slice::from_raw_parts(b, n) }
}
extern "win64" fn IsValidSid(p: *const u8) -> i32 {
    unsafe { sid_len(p).is_some() as i32 }
}
extern "win64" fn GetSidLengthRequired(count: u8) -> u32 {
    8 + 4 * count as u32
}
extern "win64" fn GetLengthSid(p: *const u8) -> u32 {
    unsafe { sid_len(p) }.map_or_else(|| fail(INVALID_SID, 0), |n| n as u32)
}
extern "win64" fn InitializeSid(p: *mut u8, authority: *const u8, count: u8) -> i32 {
    if p.is_null() || authority.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    if count > 15 {
        return fail(INVALID_SID, 0);
    }
    unsafe {
        *p = 1;
        *p.add(1) = count;
        copy_nonoverlapping(authority, p.add(2), 6);
        core::ptr::write_bytes(p.add(8), 0, count as usize * 4);
    }
    1
}
extern "win64" fn GetSidIdentifierAuthority(p: *mut u8) -> *mut u8 {
    if unsafe { sid_len(p) }.is_none() {
        return fail(INVALID_SID, core::ptr::null_mut());
    }
    unsafe { p.add(2) }
}
extern "win64" fn GetSidSubAuthorityCount(p: *mut u8) -> *mut u8 {
    if unsafe { sid_len(p) }.is_none() {
        return fail(INVALID_SID, core::ptr::null_mut());
    }
    unsafe { p.add(1) }
}
extern "win64" fn GetSidSubAuthority(p: *mut u8, index: u32) -> *mut u32 {
    if unsafe { sid_len(p) }.is_none() {
        return fail(INVALID_SID, core::ptr::null_mut());
    }
    if index >= unsafe { *p.add(1) } as u32 {
        return fail(ERROR_INVALID_PARAMETER, core::ptr::null_mut());
    }
    unsafe { p.add(8 + index as usize * 4).cast() }
}
extern "win64" fn CopySid(bytes: u32, out: *mut u8, source: *const u8) -> i32 {
    let Some(n) = (unsafe { sid_len(source) }) else { return fail(INVALID_SID, 0) };
    if bytes < n as u32 {
        return fail(ERROR_INSUFFICIENT_BUFFER, 0);
    }
    if out.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    unsafe {
        core::ptr::copy(source, out, n);
    }
    1
}
extern "win64" fn EqualSid(a: *const u8, b: *const u8) -> i32 {
    unsafe { same_sid(a, b) as i32 }
}
fn known_sid(kind: u32, domain: *const u8) -> Result<Vec<u8>, u32> {
    Ok(match kind {
        0 => sid(0, &[0]),
        1 => sid(1, &[0]),
        2 => sid(2, &[0]),
        3..=6 => sid(3, &[kind - 3]),
        7 => sid(5, &[]),
        8..=20 => sid(5, &[[1, 2, 3, 4, 6, 7, 8, 9, 10, 11, 12, 13, 14][(kind - 8) as usize]]),
        22..=24 => sid(5, &[kind - 4]),
        25 => sid(5, &[32]),
        26..=37 => sid(5, &[32, [544, 545, 546, 547, 548, 549, 550, 551, 552, 554, 555, 556][(kind - 26) as usize]]),
        38..=50 => {
            let Some(n) = (unsafe { sid_len(domain) }) else { return Err(ERROR_INVALID_PARAMETER) };
            let bytes = unsafe { core::slice::from_raw_parts(domain, n) };
            if bytes[1] != 4 || bytes[2..8] != [0, 0, 0, 0, 0, 5] || bytes[8..12] != 21u32.to_le_bytes() {
                return Err(ERROR_INVALID_PARAMETER);
            }
            let mut result = bytes.to_vec();
            result[1] += 1;
            let rid: u32 = [500, 501, 502, 512, 513, 514, 515, 516, 517, 518, 519, 520, 553][(kind - 38) as usize];
            result.extend_from_slice(&rid.to_le_bytes());
            result
        }
        65..=69 => sid(16, &[(kind - 65) * 4096]),
        79 => sid(16, &[8448]),
        84 => sid(15, &[2, 1]),
        _ => return Err(ERROR_NOT_SUPPORTED),
    })
}
extern "win64" fn CreateWellKnownSid(kind: u32, domain: *const u8, out: *mut u8, size: *mut u32) -> i32 {
    if size.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let data = match known_sid(kind, domain) {
        Ok(v) => v,
        Err(e) => return fail(e, 0),
    };
    let available = unsafe { *size };
    unsafe {
        *size = data.len() as u32;
    }
    if out.is_null() || available < data.len() as u32 {
        return fail(ERROR_INSUFFICIENT_BUFFER, 0);
    }
    unsafe {
        copy_nonoverlapping(data.as_ptr(), out, data.len());
    }
    1
}
extern "win64" fn IsWellKnownSid(p: *const u8, kind: u32) -> i32 {
    known_sid(kind, core::ptr::null()).is_ok_and(|s| unsafe { same_sid(p, s.as_ptr()) }) as i32
}

// ACL walks are bounded by the caller's WORD AclSize, never by AceCount alone.
unsafe fn acl_used(p: *const u8) -> Result<usize, u32> {
    if p.is_null() {
        return Err(INVALID_ACL);
    }
    let revision = unsafe { *p };
    let size: u16 = unsafe { rd(p, 2) };
    let count: u16 = unsafe { rd(p, 4) };
    if ![2, 4].contains(&revision) || size < 8 {
        return Err(INVALID_ACL);
    }
    let data = unsafe { core::slice::from_raw_parts(p, size as usize) };
    let mut at = 8usize;
    for _ in 0..count {
        if at + 4 > data.len() {
            return Err(INVALID_ACL);
        }
        let n = u16::from_le_bytes([data[at + 2], data[at + 3]]) as usize;
        if n < 4 || !n.is_multiple_of(4) || at + n > data.len() {
            return Err(INVALID_ACL);
        }
        if [0, 1, 2, 0x11].contains(&data[at])
            && (n < 16 || data[at + 8] != 1 || data[at + 9] > 15 || 16 + data[at + 9] as usize * 4 > n)
        {
            return Err(INVALID_ACL);
        }
        at += n;
    }
    Ok(at)
}
extern "win64" fn InitializeAcl(p: *mut u8, size: u32, revision: u32) -> i32 {
    if p.is_null() || !(8..=65535).contains(&size) || !size.is_multiple_of(4) {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    if revision != 2 && revision != 4 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    unsafe {
        core::ptr::write_bytes(p, 0, 8);
        *p = revision as u8;
        wr(p, 2, size as u16);
    }
    1
}
extern "win64" fn IsValidAcl(p: *const u8) -> i32 {
    unsafe { acl_used(p).is_ok() as i32 }
}
fn add_ace(p: *mut u8, revision: u32, flags: u32, access: u32, who: *const u8, kind: u8) -> i32 {
    if revision != 2 && revision != 4 {
        return fail(REVISION_MISMATCH, 0);
    }
    if flags & !0x1f != 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let Some(n) = (unsafe { sid_len(who) }) else { return fail(INVALID_SID, 0) };
    let at = match unsafe { acl_used(p) } {
        Ok(n) => n,
        Err(e) => return fail(e, 0),
    };
    let capacity: u16 = unsafe { rd(p, 2) };
    if at + 8 + n > capacity as usize {
        return fail(NO_SPACE, 0);
    }
    unsafe {
        *p = (*p).max(revision as u8);
        *p.add(at) = kind;
        *p.add(at + 1) = flags as u8;
        wr(p, at + 2, (8 + n) as u16);
        wr(p, at + 4, access);
        copy_nonoverlapping(who, p.add(at + 8), n);
        let count: u16 = rd(p, 4);
        wr(p, 4, count + 1);
    }
    1
}
extern "win64" fn AddAccessAllowedAce(p: *mut u8, revision: u32, access: u32, who: *const u8) -> i32 {
    add_ace(p, revision, 0, access, who, 0)
}
extern "win64" fn AddAccessDeniedAce(p: *mut u8, revision: u32, access: u32, who: *const u8) -> i32 {
    add_ace(p, revision, 0, access, who, 1)
}
extern "win64" fn AddAccessAllowedAceEx(p: *mut u8, revision: u32, flags: u32, access: u32, who: *const u8) -> i32 {
    add_ace(p, revision, flags, access, who, 0)
}
extern "win64" fn AddAccessDeniedAceEx(p: *mut u8, revision: u32, flags: u32, access: u32, who: *const u8) -> i32 {
    add_ace(p, revision, flags, access, who, 1)
}
extern "win64" fn GetAce(p: *const u8, index: u32, out: *mut u64) -> i32 {
    if out.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    if let Err(e) = unsafe { acl_used(p) } {
        return fail(e, 0);
    }
    if index >= unsafe { rd::<u16>(p, 4) } as u32 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let mut at = 8;
    for _ in 0..index {
        at += unsafe { rd::<u16>(p, at + 2) } as usize;
    }
    unsafe {
        *out = p.add(at) as u64;
    }
    1
}

#[repr(C)]
struct Descriptor {
    revision: u8,
    reserved: u8,
    control: u16,
    owner: u64,
    group: u64,
    sacl: u64,
    dacl: u64,
}
const _: () = assert!(core::mem::size_of::<Descriptor>() == 40);
unsafe fn descriptor_control(p: *const u8) -> Result<u16, u32> {
    if p.is_null() || unsafe { *p } != 1 {
        return Err(BAD_DESCRIPTOR);
    }
    Ok(unsafe { rd(p, 2) })
}
unsafe fn component(p: *const u8, field: usize) -> Result<*const u8, u32> {
    let control = unsafe { descriptor_control(p) }?;
    if control & 0x8000 != 0 {
        let offset = unsafe { rd::<u32>(p, 4 + field * 4) } as usize;
        if offset == 0 {
            return Ok(core::ptr::null());
        }
        if offset < 20 {
            return Err(BAD_DESCRIPTOR);
        }
        (p as usize).checked_add(offset).map(|p| p as *const u8).ok_or(BAD_DESCRIPTOR)
    } else {
        Ok(unsafe { rd::<u64>(p, 8 + field * 8) } as *const u8)
    }
}
extern "win64" fn InitializeSecurityDescriptor(p: *mut u8, revision: u32) -> i32 {
    if p.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    if revision != 1 {
        return fail(REVISION_MISMATCH, 0);
    }
    unsafe {
        core::ptr::write_bytes(p, 0, 40);
        *p = 1;
    }
    1
}
fn set_component(p: *mut u8, field: usize, value: *const u8, bit: u16, defaulted: i32, present: bool) -> i32 {
    let mut control = match unsafe { descriptor_control(p) } {
        Ok(c) => c,
        Err(e) => return fail(e, 0),
    };
    if control & 0x8000 != 0 {
        return fail(BAD_DESCRIPTOR, 0);
    } // setters require absolute form
    if !value.is_null() && present {
        if field < 2 && unsafe { sid_len(value) }.is_none() {
            return fail(INVALID_SID, 0);
        }
        if field >= 2 && unsafe { acl_used(value) }.is_err() {
            return fail(INVALID_ACL, 0);
        }
    }
    if defaulted != 0 {
        control |= bit;
    } else {
        control &= !bit;
    }
    if field >= 2 {
        let flag = if field == 2 { 0x10 } else { 4 };
        if present {
            control |= flag;
        } else {
            control &= !(flag | bit);
        }
    }
    unsafe {
        wr(p, 2, control);
        wr(p, 8 + field * 8, if present { value as u64 } else { 0 });
    }
    1
}
extern "win64" fn SetSecurityDescriptorOwner(p: *mut u8, owner: *const u8, defaulted: i32) -> i32 {
    set_component(p, 0, owner, 1, defaulted, true)
}
extern "win64" fn SetSecurityDescriptorGroup(p: *mut u8, group: *const u8, defaulted: i32) -> i32 {
    set_component(p, 1, group, 2, defaulted, true)
}
extern "win64" fn SetSecurityDescriptorDacl(p: *mut u8, present: i32, acl: *const u8, defaulted: i32) -> i32 {
    set_component(p, 3, acl, 8, defaulted, present != 0)
}
extern "win64" fn SetSecurityDescriptorSacl(p: *mut u8, present: i32, acl: *const u8, defaulted: i32) -> i32 {
    set_component(p, 2, acl, 0x20, defaulted, present != 0)
}
fn get_component(p: *const u8, field: usize, out: *mut u64, defaulted: *mut i32, present: Option<*mut i32>) -> i32 {
    if out.is_null() || defaulted.is_null() || present.is_some_and(|p| p.is_null()) {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let control = match unsafe { descriptor_control(p) } {
        Ok(c) => c,
        Err(e) => return fail(e, 0),
    };
    let value = match unsafe { component(p, field) } {
        Ok(c) => c,
        Err(e) => return fail(e, 0),
    };
    let (d, flag) = [(1, 0), (2, 0), (0x20, 0x10), (8, 4)][field];
    unsafe {
        *out = value as u64;
        *defaulted = i32::from(control & d != 0);
        if let Some(present) = present {
            *present = i32::from(control & flag != 0);
        }
    }
    1
}
extern "win64" fn GetSecurityDescriptorOwner(p: *const u8, out: *mut u64, d: *mut i32) -> i32 {
    get_component(p, 0, out, d, None)
}
extern "win64" fn GetSecurityDescriptorGroup(p: *const u8, out: *mut u64, d: *mut i32) -> i32 {
    get_component(p, 1, out, d, None)
}
extern "win64" fn GetSecurityDescriptorDacl(p: *const u8, present: *mut i32, out: *mut u64, d: *mut i32) -> i32 {
    get_component(p, 3, out, d, Some(present))
}
extern "win64" fn GetSecurityDescriptorSacl(p: *const u8, present: *mut i32, out: *mut u64, d: *mut i32) -> i32 {
    get_component(p, 2, out, d, Some(present))
}
extern "win64" fn GetSecurityDescriptorControl(p: *const u8, control: *mut u16, revision: *mut u32) -> i32 {
    if control.is_null() || revision.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    match unsafe { descriptor_control(p) } {
        Ok(c) => {
            unsafe {
                *control = c;
                *revision = 1;
            }
            1
        }
        Err(e) => fail(e, 0),
    }
}
extern "win64" fn IsValidSecurityDescriptor(p: *const u8) -> i32 {
    let control = match unsafe { descriptor_control(p) } {
        Ok(c) => c,
        Err(_) => return 0,
    };
    for field in 0..4 {
        if field == 2 && control & 0x10 == 0 || field == 3 && control & 4 == 0 {
            continue;
        }
        let value = match unsafe { component(p, field) } {
            Ok(v) => v,
            Err(_) => return 0,
        };
        if value.is_null() {
            continue;
        }
        if field < 2 && unsafe { sid_len(value) }.is_none() || field >= 2 && unsafe { acl_used(value) }.is_err() {
            return 0;
        }
    }
    1
}

pub(crate) extern "win64" fn OpenProcessToken(process: u64, access: u32, out: *mut u64) -> i32 {
    if out.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    if process != objects::CURRENT_PROCESS {
        return fail(ERROR_INVALID_HANDLE, 0);
    }
    let access = match token_access(access) {
        Ok(v) => v,
        Err(e) => return fail(e, 0),
    };
    let (uid, gid) = os_sys::getuid();
    unsafe {
        *out = objects::insert(Object::Token(Arc::new(Token { uid, gid, access })));
    }
    1
}
fn group_sids(token: &Token) -> Vec<Vec<u8>> {
    let mut groups = alloc::vec![sid(22, &[2, token.gid]), sid(1, &[0]), sid(5, &[11]), sid(5, &[32, 545])];
    if token.uid == 0 {
        groups.push(sid(5, &[32, 544]));
    }
    groups
}
fn put_sid(data: &mut Vec<u8>, at: usize, s: &[u8], base: u64) {
    let offset = data.len();
    data[at..at + 8].copy_from_slice(&(base + offset as u64).to_le_bytes());
    data.extend_from_slice(s);
}
fn token_info(t: &Token, class: u32, base: u64) -> Result<Vec<u8>, u32> {
    let mut data;
    match class {
        1 | 4 | 5 => {
            data = alloc::vec![0; if class==1 {16} else {8}];
            let value = if class == 5 { sid(22, &[2, t.gid]) } else { sid(22, &[1, t.uid]) };
            put_sid(&mut data, 0, &value, base);
        }
        2 => {
            let groups = group_sids(t);
            data = alloc::vec![0; 8+16*groups.len()];
            data[..4].copy_from_slice(&(groups.len() as u32).to_le_bytes());
            for (i, s) in groups.iter().enumerate() {
                let at = 8 + 16 * i;
                data[at + 8..at + 12].copy_from_slice(&7u32.to_le_bytes());
                put_sid(&mut data, at, s, base);
            }
        }
        8 => data = 1u32.to_le_bytes().to_vec(), // TokenPrimary; no thread impersonation
        _ => return Err(ERROR_NOT_SUPPORTED),
    }
    Ok(data)
}
extern "win64" fn GetTokenInformation(h: u64, class: u32, out: *mut u8, size: u32, needed: *mut u32) -> i32 {
    let Some(Object::Token(t)) = objects::get(h) else { return fail(ERROR_INVALID_HANDLE, 0) };
    if t.access & TOKEN_QUERY == 0 {
        return fail(ERROR_ACCESS_DENIED, 0);
    }
    if needed.is_null() || out.is_null() && size != 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let data = match token_info(&t, class, out as u64) {
        Ok(v) => v,
        Err(e) => return fail(e, 0),
    };
    unsafe {
        *needed = data.len() as u32;
    }
    if size < data.len() as u32 || out.is_null() {
        return fail(ERROR_INSUFFICIENT_BUFFER, 0);
    }
    unsafe {
        copy_nonoverlapping(data.as_ptr(), out, data.len());
    }
    1
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        IsValidSid,
        GetSidLengthRequired,
        GetLengthSid,
        InitializeSid,
        GetSidIdentifierAuthority,
        GetSidSubAuthorityCount,
        GetSidSubAuthority,
        CopySid,
        EqualSid,
        CreateWellKnownSid,
        IsWellKnownSid,
        InitializeAcl,
        IsValidAcl,
        AddAccessAllowedAce,
        AddAccessDeniedAce,
        AddAccessAllowedAceEx,
        AddAccessDeniedAceEx,
        GetAce,
        InitializeSecurityDescriptor,
        IsValidSecurityDescriptor,
        SetSecurityDescriptorOwner,
        SetSecurityDescriptorGroup,
        SetSecurityDescriptorDacl,
        SetSecurityDescriptorSacl,
        GetSecurityDescriptorOwner,
        GetSecurityDescriptorGroup,
        GetSecurityDescriptorDacl,
        GetSecurityDescriptorSacl,
        GetSecurityDescriptorControl,
        OpenProcessToken,
        GetTokenInformation
    ]
}
