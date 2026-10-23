//! Credential Manager (CredRead/CredWrite/CredDelete/CredFree, ANSI forms)
//! over a per-user file, $HOME/.osyx-credentials, created with mode 0600
//! so only its owner can read it. Unlike Windows the blobs are not
//! encrypted with a user key (OSyx has no DPAPI); the file permission is
//! their only protection. Session-persistent credentials live in memory.
//! Credential attributes and target aliases are not kept.
#![allow(non_snake_case)]
use crate::{error::*, kernel32::fail, path, teb};
use alloc::{format, string::String, vec::Vec};
use os_std::sync::Mutex;

const CRED_PERSIST_SESSION: u32 = 1;
const CRED_MAX_CREDENTIAL_BLOB_SIZE: u32 = 5 * 512;

#[derive(Clone)]
struct Cred {
    kind: u32,
    target: String,
    user: String,
    comment: String,
    persist: u32,
    blob: Vec<u8>,
    written: u64,
}

/// Session credentials; the persistent ones are read from the file.
static SESSION: Mutex<Vec<Cred>> = Mutex::new(Vec::new());

fn file() -> String {
    let home = os_std::env::var("HOME").unwrap_or_else(|_| String::from("/home/user"));
    format!("{}/.osyx-credentials", home.trim_end_matches('/'))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

fn load() -> Vec<Cred> {
    let Ok(text) = os_std::fs::read_to_string(file()) else { return Vec::new() };
    let s = |h: &str| unhex(h).and_then(|b| String::from_utf8(b).ok());
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            let [kind, target, user, comment, persist, blob, written] = f.as_slice() else { return None };
            Some(Cred {
                kind: kind.parse().ok()?,
                target: s(target)?,
                user: s(user)?,
                comment: s(comment)?,
                persist: persist.parse().ok()?,
                blob: unhex(blob)?,
                written: written.parse().ok()?,
            })
        })
        .collect()
}

fn store(all: &[Cred]) -> Result<(), u32> {
    let mut text = String::new();
    for c in all {
        text.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            c.kind,
            hex(c.target.as_bytes()),
            hex(c.user.as_bytes()),
            hex(c.comment.as_bytes()),
            c.persist,
            hex(&c.blob),
            c.written
        ));
    }
    let name = file();
    let tmp = format!("{name}.{}.tmp", crate::kernel32::pid());
    let fd = os_sys::open(&tmp, osyx_abi::OPEN_WRITE | osyx_abi::OPEN_CREATE | osyx_abi::OPEN_TRUNCATE, 0o600).map_err(errno_to_win32)?;
    let r = os_sys::write(fd, text.as_bytes()).map(|_| ()).and_then(|()| os_sys::fsync(fd));
    let _ = os_sys::close(fd);
    if let Err(e) = r {
        let _ = os_sys::unlink(&tmp);
        return Err(errno_to_win32(e));
    }
    os_sys::rename(&tmp, &name).map_err(errno_to_win32)
}

fn find(target: &str, kind: u32) -> Option<Cred> {
    let same = |c: &&Cred| c.kind == kind && c.target.eq_ignore_ascii_case(target);
    SESSION.lock().unwrap().iter().find(same).cloned().or_else(|| load().iter().find(same).cloned())
}

/// CREDENTIALA (80 bytes): Flags, Type, TargetName, Comment, LastWritten,
/// CredentialBlobSize, CredentialBlob, Persist, AttributeCount,
/// Attributes, TargetAlias, UserName.
unsafe fn read_cred(p: u64) -> Option<Cred> {
    // SAFETY: the caller's structure and its strings.
    unsafe {
        let blob_len: u32 = teb::get(p, 32);
        let blob_p: u64 = teb::get(p, 40);
        if blob_len > CRED_MAX_CREDENTIAL_BLOB_SIZE || (blob_len > 0 && blob_p == 0) {
            return None;
        }
        Some(Cred {
            kind: teb::get(p, 4),
            target: path::ansi_to_string(teb::get::<u64>(p, 8) as *const u8)?,
            comment: path::ansi_to_string(teb::get::<u64>(p, 16) as *const u8).unwrap_or_default(),
            written: crate::sysext::now_filetime(),
            blob: if blob_len == 0 { Vec::new() } else { core::slice::from_raw_parts(blob_p as *const u8, blob_len as usize).to_vec() },
            persist: teb::get(p, 48),
            user: path::ansi_to_string(teb::get::<u64>(p, 72) as *const u8).unwrap_or_default(),
        })
    }
}

extern "win64" fn CredWriteA(cred: *const u8, flags: u32) -> i32 {
    if cred.is_null() || flags & !1 != 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: the caller's CREDENTIALA.
    let Some(c) = (unsafe { read_cred(cred as u64) }) else { return fail(ERROR_INVALID_PARAMETER, 0) };
    if !(1..=6).contains(&c.kind) || !(1..=3).contains(&c.persist) || c.target.is_empty() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let same = |x: &Cred| x.kind == c.kind && x.target.eq_ignore_ascii_case(&c.target);
    if c.persist == CRED_PERSIST_SESSION {
        let mut s = SESSION.lock().unwrap();
        s.retain(|x| !same(x));
        s.push(c);
        return 1;
    }
    let mut all = load();
    all.retain(|x| !same(x));
    all.push(c);
    match store(&all) {
        Ok(()) => 1,
        Err(e) => fail(e, 0),
    }
}

/// One heap block: the CREDENTIALA, then the target, comment, user name
/// and blob it points to (freed by CredFree).
extern "win64" fn CredReadA(target: *const u8, kind: u32, flags: u32, out: *mut u64) -> i32 {
    if out.is_null() || flags != 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: a NUL-terminated target name.
    let Some(t) = (unsafe { path::ansi_to_string(target) }) else { return fail(ERROR_INVALID_PARAMETER, 0) };
    let Some(c) = find(&t, kind) else { return fail(ERROR_NOT_FOUND, 0) };
    let strings = [c.target.as_bytes(), c.comment.as_bytes(), c.user.as_bytes()];
    let size = 80 + strings.iter().map(|s| s.len() + 1).sum::<usize>() + c.blob.len();
    let Some(p) = crate::kernel32::heap_alloc(size, true) else { return fail(ERROR_NOT_ENOUGH_MEMORY, 0) };
    // SAFETY: a fresh zeroed block of `size` bytes.
    unsafe {
        let mut at = p + 80;
        let mut put_str = |s: &[u8]| {
            core::ptr::copy_nonoverlapping(s.as_ptr(), at as *mut u8, s.len());
            let start = at;
            at += s.len() as u64 + 1;
            start
        };
        let (tp, cp, up) = (put_str(strings[0]), put_str(strings[1]), put_str(strings[2]));
        core::ptr::copy_nonoverlapping(c.blob.as_ptr(), at as *mut u8, c.blob.len());
        teb::put(p, 4, c.kind);
        teb::put(p, 8, tp);
        teb::put(p, 16, if c.comment.is_empty() { 0 } else { cp });
        teb::put(p, 24, c.written);
        teb::put(p, 32, c.blob.len() as u32);
        teb::put(p, 40, if c.blob.is_empty() { 0 } else { at });
        teb::put(p, 48, c.persist);
        teb::put(p, 72, if c.user.is_empty() { 0 } else { up });
        *out = p;
    }
    1
}

extern "win64" fn CredDeleteA(target: *const u8, kind: u32, flags: u32) -> i32 {
    if flags != 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: a NUL-terminated target name.
    let Some(t) = (unsafe { path::ansi_to_string(target) }) else { return fail(ERROR_INVALID_PARAMETER, 0) };
    let same = |x: &Cred| x.kind == kind && x.target.eq_ignore_ascii_case(&t);
    let mut s = SESSION.lock().unwrap();
    let before = s.len();
    s.retain(|x| !same(x));
    if s.len() != before {
        return 1;
    }
    drop(s);
    let mut all = load();
    let before = all.len();
    all.retain(|x| !same(x));
    if all.len() == before {
        return fail(ERROR_NOT_FOUND, 0);
    }
    match store(&all) {
        Ok(()) => 1,
        Err(e) => fail(e, 0),
    }
}

extern "win64" fn CredFree(p: u64) {
    if p != 0 {
        crate::kernel32::heap_free(p);
    }
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![CredWriteA, CredReadA, CredDeleteA, CredFree]
}
