//! crypt32 calls of Chromium's network and OS-crypt code (libcef.dll):
//! system and collection stores, adding encoded certificates, comparing
//! names, time validity, key usage, signature checks and DPAPI
//! (CryptProtectData, CryptProtectMemory).
use super::*;
use crate::teb;

const CRYPT_E_EXISTS: u32 = 0x8009_2005;
const NTE_BAD_DATA: u32 = 0x8009_0005;
const NTE_BAD_SIGNATURE: u32 = 0x8009_0006;
const NTE_BAD_ALGID: u32 = 0x8009_0008;
const E_INVALIDARG: u32 = 0x8007_0057;
const ERROR_INVALID_DATA: u32 = 13;
const ERROR_WRITE_FAULT: u32 = 29;

/// A CRYPT_DATA_BLOB-shaped {cbData, pbData} at `p`, copied.
///
/// # Safety
/// `p` must be a readable blob whose data holds `cbData` bytes.
unsafe fn blob_at(p: u64) -> Vec<u8> {
    // SAFETY: the caller's blob.
    unsafe {
        let (n, data): (u32, u64) = (teb::get(p, 0), teb::get(p, 8));
        if n == 0 || data == 0 {
            return Vec::new();
        }
        core::slice::from_raw_parts(data as *const u8, n as usize).to_vec()
    }
}

// ---- system and collection stores --------------------------------------------------------

/// The system stores every Windows installation has.
const SYSTEM_STORES: [&str; 8] = ["MY", "ROOT", "CA", "TRUST", "DISALLOWED", "ADDRESSBOOK", "TRUSTEDPEOPLE", "TRUSTEDPUBLISHER"];

/// System stores by upper-case name, shared by every handle opened on
/// them. OSyx has no Windows certificate stores, so they start empty and
/// hold what the process adds (trust in roots comes from the Mozilla root
/// store chain building uses, not from ROOT).
static SYSTEM: Mutex<BTreeMap<String, Arc<Store>>> = Mutex::new(BTreeMap::new());

const CERT_STORE_CREATE_NEW_FLAG: u32 = 0x2000;
const CERT_STORE_OPEN_EXISTING_FLAG: u32 = 0x4000;

/// A new handle on system store `name`.
pub(super) fn open_system(name: &str, flags: u32) -> u64 {
    if name.is_empty() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let key = name.to_uppercase();
    let mut sys = SYSTEM.lock().unwrap();
    let exists = sys.contains_key(&key) || SYSTEM_STORES.contains(&key.as_str());
    if !exists && flags & CERT_STORE_OPEN_EXISTING_FLAG != 0 {
        return fail(ERROR_FILE_NOT_FOUND, 0);
    }
    if exists && flags & CERT_STORE_CREATE_NEW_FLAG != 0 {
        return fail(ERROR_FILE_EXISTS, 0);
    }
    let store = sys.entry(key).or_insert_with(|| Arc::new(Store::new(Vec::new()))).clone();
    drop(sys);
    let mut h = HANDLES.lock().unwrap();
    let id = new_handle(&mut h);
    h.stores.insert(id, store);
    id
}

extern "win64" fn CertOpenSystemStoreW(_prov: u64, name: *const u16) -> u64 {
    // SAFETY: NULL or the caller's NUL-terminated name.
    let name = unsafe { path::wide_to_string(name) }.unwrap_or_default();
    open_system(&name, 0)
}

extern "win64" fn CertOpenSystemStoreA(_prov: u64, name: *const u8) -> u64 {
    // SAFETY: NULL or the caller's NUL-terminated name.
    let name = unsafe { path::ansi_to_string(name) }.unwrap_or_default();
    open_system(&name, 0)
}

/// Whether `s` is `target` or holds it, directly or through its members.
fn reaches(s: &Arc<Store>, target: &Arc<Store>) -> bool {
    Arc::ptr_eq(s, target) || s.members.lock().unwrap().iter().any(|(_, m)| reaches(m, target))
}

/// CertAddStoreToCollection: `sibling`'s certificates become part of
/// `collection` (live: what is added to the sibling later shows too),
/// higher priorities first.
extern "win64" fn CertAddStoreToCollection(collection: u64, sibling: u64, _flags: u32, priority: u32) -> i32 {
    let (c, s) = {
        let h = HANDLES.lock().unwrap();
        match (h.stores.get(&collection), h.stores.get(&sibling)) {
            (Some(c), Some(s)) => (c.clone(), s.clone()),
            _ => return fail(ERROR_INVALID_HANDLE, 0),
        }
    };
    if reaches(&s, &c) {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let mut members = c.members.lock().unwrap();
    let at = members.iter().position(|(p, _)| *p < priority).unwrap_or(members.len());
    members.insert(at, (priority, s));
    1
}

extern "win64" fn CertRemoveStoreFromCollection(collection: u64, sibling: u64) {
    let h = HANDLES.lock().unwrap();
    if let (Some(c), Some(s)) = (h.stores.get(&collection), h.stores.get(&sibling)) {
        c.members.lock().unwrap().retain(|(_, m)| !Arc::ptr_eq(m, s));
    }
}

const CERT_STORE_CTRL_RESYNC: u32 = 1;
const CERT_STORE_CTRL_NOTIFY_CHANGE: u32 = 2;
const CERT_STORE_CTRL_COMMIT: u32 = 3;
const CERT_STORE_CTRL_AUTO_RESYNC: u32 = 4;
const CERT_STORE_CTRL_CANCEL_NOTIFY: u32 = 5;

/// CertControlStore: the stores live in this process only, so they are
/// always in sync, have nothing to commit and never change behind the
/// caller's back (a change notification is accepted and never fires).
extern "win64" fn CertControlStore(store: u64, _flags: u32, ctrl: u32, _para: u64) -> i32 {
    if !HANDLES.lock().unwrap().stores.contains_key(&store) {
        return fail(ERROR_INVALID_HANDLE, 0);
    }
    match ctrl {
        CERT_STORE_CTRL_RESYNC | CERT_STORE_CTRL_NOTIFY_CHANGE | CERT_STORE_CTRL_COMMIT | CERT_STORE_CTRL_AUTO_RESYNC | CERT_STORE_CTRL_CANCEL_NOTIFY => 1,
        _ => fail(ERROR_CALL_NOT_IMPLEMENTED, 0),
    }
}

const CERT_STORE_ADD_NEW: u32 = 1;
const CERT_STORE_ADD_USE_EXISTING: u32 = 2;
const CERT_STORE_ADD_REPLACE_EXISTING: u32 = 3;
const CERT_STORE_ADD_REPLACE_EXISTING_INHERIT_PROPERTIES: u32 = 5;
const CERT_STORE_ADD_NEWER: u32 = 6;
const CERT_STORE_ADD_NEWER_INHERIT_PROPERTIES: u32 = 7;

/// CertAddEncodedCertificateToStore. A certificate with the same encoding
/// is the existing one; with no store, only the context is made.
extern "win64" fn CertAddEncodedCertificateToStore(store: u64, _encoding: u32, data: *const u8, len: u32, disposition: u32, out: *mut u64) -> i32 {
    if data.is_null() || len == 0 || !(CERT_STORE_ADD_NEW..=CERT_STORE_ADD_NEWER_INHERIT_PROPERTIES).contains(&disposition) {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: the caller's `len` bytes.
    let der = unsafe { core::slice::from_raw_parts(data, len as usize) }.to_vec();
    if let Err(e) = parse_cert(&der) {
        return fail(e, 0);
    }
    let put = |ctx: u64| {
        if out.is_null() {
            CertFreeCertificateContext(ctx);
        } else {
            // SAFETY: the caller's out pointer.
            unsafe { *out = ctx };
        }
    };
    if store == 0 {
        match make_context(0, 0, &der) {
            Ok(c) => put(c),
            Err(e) => return fail(e, 0),
        }
        return 1;
    }
    let Some(s) = HANDLES.lock().unwrap().stores.get(&store).cloned() else { return fail(ERROR_INVALID_HANDLE, 0) };
    let index = {
        let mut certs = s.certs.lock().unwrap();
        let existing = certs.iter().position(|c| *c == der);
        match (disposition, existing) {
            (CERT_STORE_ADD_NEW | CERT_STORE_ADD_NEWER | CERT_STORE_ADD_NEWER_INHERIT_PROPERTIES, Some(_)) => return fail(CRYPT_E_EXISTS, 0),
            // Replacing a certificate with the same encoding keeps it.
            (CERT_STORE_ADD_USE_EXISTING | CERT_STORE_ADD_REPLACE_EXISTING | CERT_STORE_ADD_REPLACE_EXISTING_INHERIT_PROPERTIES, Some(i)) => i,
            // CERT_STORE_ADD_ALWAYS, and every disposition with no match.
            _ => {
                certs.push(der.clone());
                certs.len() - 1
            }
        }
    };
    match make_context(store, index, &der) {
        Ok(c) => put(c),
        Err(e) => return fail(e, 0),
    }
    1
}

/// Names are compared in their encoded form, as Windows does.
extern "win64" fn CertCompareCertificateName(_encoding: u32, a: u64, b: u64) -> i32 {
    if a == 0 || b == 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: the caller's CERT_NAME_BLOBs.
    unsafe { (blob_at(a) == blob_at(b)) as i32 }
}

/// CERT_INFO's NotBefore (64) and NotAfter (72) against `time` (NULL:
/// now): -1 before, 1 after, 0 within.
extern "win64" fn CertVerifyTimeValidity(time: *const u64, info: u64) -> i32 {
    if info == 0 {
        return 0;
    }
    let now = if time.is_null() {
        crate::sysext::now_filetime()
    } else {
        // SAFETY: the caller's FILETIME.
        unsafe { core::ptr::read_unaligned(time) }
    };
    // SAFETY: the caller's CERT_INFO.
    let (before, after): (u64, u64) = unsafe { (teb::get(info, 64), teb::get(info, 72)) };
    if now < before {
        -1
    } else if now > after {
        1
    } else {
        0
    }
}

const OID_KEY_USAGE: &str = "2.5.29.15";

/// The key usage bits of CERT_INFO's KeyUsage extension, zero-padded to
/// `len` bytes; FALSE with no error when there is none.
extern "win64" fn CertGetIntendedKeyUsage(_encoding: u32, info: u64, out: *mut u8, len: u32) -> i32 {
    if info == 0 || (out.is_null() && len != 0) {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: the caller's CERT_INFO: cExtension at 192, rgExtension at 200;
    // each CERT_EXTENSION is (pszObjId, fCritical, Value) in 32 bytes.
    let value = unsafe {
        let (n, rg): (u32, u64) = (teb::get(info, 192), teb::get(info, 200));
        (0..n as u64).map(|i| rg + 32 * i).find(|&e| c_string(teb::get(e, 0)) == OID_KEY_USAGE).map(|e| blob_at(e + 16))
    };
    let Some(value) = value else {
        teb::set_last_error(0);
        return 0;
    };
    let bits = match tlv(&value) {
        Ok((t, _)) if t.tag == 0x03 => t.body.get(1..).unwrap_or_default().to_vec(),
        _ => return fail(ASN1, 0),
    };
    // SAFETY: the caller's buffer of `len` bytes.
    unsafe {
        core::ptr::write_bytes(out, 0, len as usize);
        core::ptr::copy_nonoverlapping(bits.as_ptr(), out, bits.len().min(len as usize));
    }
    1
}

/// CertGetNameStringW's name in UTF-8 (the ANSI code page).
extern "win64" fn CertGetNameStringA(ctx: u64, kind: u32, flags: u32, para: *const u8, out: *mut u8, len: u32) -> u32 {
    let name = context_der(ctx).and_then(|d| parse_cert(&d).ok()).map(|c| cert_name(&c, kind, flags, para)).unwrap_or_default();
    let bytes = name.as_bytes();
    if out.is_null() || len == 0 {
        return bytes.len() as u32 + 1;
    }
    let mut n = bytes.len().min(len as usize - 1);
    while !name.is_char_boundary(n) {
        n -= 1;
    }
    // SAFETY: the caller's buffer of `len` bytes.
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), out, n);
        *out.add(n) = 0;
    }
    n as u32 + 1
}

// ---- signature checks ----------------------------------------------------------------------

/// A SIGNED{} structure (certificate, CRL): the signed part, the signature
/// AlgorithmIdentifier's contents and the signature.
fn signed_parts(der: &[u8]) -> Result<(Vec<u8>, Vec<u8>, Vec<u8>), u32> {
    let (outer, _) = tlv(der)?;
    let parts = items(expect(outer, SEQ)?.body)?;
    let [tbs, alg, sig] = parts.as_slice() else { return bad() };
    let alg = expect(*alg, SEQ)?;
    let sig = expect(*sig, 0x03)?;
    Ok((tbs.raw.to_vec(), alg.body.to_vec(), sig.body.get(1..).unwrap_or_default().to_vec()))
}

/// A certificate's SubjectPublicKeyInfo: the algorithm's contents and the
/// key.
fn cert_key(der: &[u8]) -> Result<(Vec<u8>, Vec<u8>), u32> {
    let (outer, _) = tlv(der)?;
    let parts = items(expect(outer, SEQ)?.body)?;
    let tbs = items(expect(*parts.first().ok_or(ASN1)?, SEQ)?.body)?;
    let i = if tbs.first().is_some_and(|t| t.tag == 0xa0) { 1 } else { 0 };
    let spki = items(expect(*tbs.get(i + 5).ok_or(ASN1)?, SEQ)?.body)?;
    let alg = expect(*spki.first().ok_or(ASN1)?, SEQ)?;
    let key = expect(*spki.get(1).ok_or(ASN1)?, 0x03)?;
    Ok((alg.body.to_vec(), key.body.get(1..).unwrap_or_default().to_vec()))
}

const CRYPT_VERIFY_CERT_SIGN_SUBJECT_BLOB: u32 = 1;
const CRYPT_VERIFY_CERT_SIGN_SUBJECT_CERT: u32 = 2;
const CRYPT_VERIFY_CERT_SIGN_SUBJECT_CRL: u32 = 3;
const CRYPT_VERIFY_CERT_SIGN_ISSUER_PUBKEY: u32 = 1;
const CRYPT_VERIFY_CERT_SIGN_ISSUER_CERT: u32 = 2;

/// CryptVerifyCertificateSignatureEx: the subject's signature (a signed
/// blob, a certificate or a CRL) against the issuer's public key (a
/// CERT_PUBLIC_KEY_INFO or a certificate), with the RustCrypto verifiers
/// chain building uses.
#[allow(clippy::too_many_arguments)]
extern "win64" fn CryptVerifyCertificateSignatureEx(
    _prov: u64, _encoding: u32, subject_type: u32, subject: u64, issuer_type: u32, issuer: u64, _flags: u32, _extra: u64,
) -> i32 {
    if subject == 0 || issuer == 0 {
        return fail(E_INVALIDARG, 0);
    }
    // SAFETY (both matches): the caller's structures of the types given.
    let subject = match subject_type {
        CRYPT_VERIFY_CERT_SIGN_SUBJECT_BLOB => unsafe { blob_at(subject) },
        CRYPT_VERIFY_CERT_SIGN_SUBJECT_CERT => match context_der(subject) {
            Some(d) => d,
            None => return fail(E_INVALIDARG, 0),
        },
        // CRL_CONTEXT: dwCertEncodingType, pbCrlEncoded, cbCrlEncoded.
        CRYPT_VERIFY_CERT_SIGN_SUBJECT_CRL => unsafe { blob_at_parts(teb::get(subject, 16), teb::get(subject, 8)) },
        _ => return fail(E_INVALIDARG, 0),
    };
    let key = match issuer_type {
        CRYPT_VERIFY_CERT_SIGN_ISSUER_PUBKEY => unsafe {
            // CERT_PUBLIC_KEY_INFO: Algorithm (pszObjId, Parameters), PublicKey.
            let oid = c_string(teb::get(issuer, 0));
            let mut alg = match der_oid(&oid) {
                Some(o) => o,
                None => return fail(NTE_BAD_ALGID, 0),
            };
            alg.extend_from_slice(&blob_at(issuer + 8));
            Ok((alg, blob_at(issuer + 24)))
        },
        CRYPT_VERIFY_CERT_SIGN_ISSUER_CERT => match context_der(issuer) {
            Some(d) => cert_key(&d),
            None => return fail(E_INVALIDARG, 0),
        },
        _ => return fail(E_INVALIDARG, 0),
    };
    let ((key_alg, key), (tbs, sig_alg, sig)) = match (key, signed_parts(&subject)) {
        (Ok(k), Ok(s)) => (k, s),
        (Err(e), _) | (_, Err(e)) => return fail(e, 0),
    };
    let algs = rustls_rustcrypto::provider().signature_verification_algorithms.all;
    let Some(alg) = algs.iter().find(|a| a.public_key_alg_id().as_ref() == key_alg.as_slice() && a.signature_alg_id().as_ref() == sig_alg.as_slice()) else {
        return fail(NTE_BAD_ALGID, 0);
    };
    match alg.verify_signature(&key, &tbs, &sig) {
        Ok(()) => 1,
        Err(_) => fail(NTE_BAD_SIGNATURE, 0),
    }
}

/// `n` bytes at `p`, copied.
///
/// # Safety
/// `p` is NULL or holds `n` readable bytes.
unsafe fn blob_at_parts(n: u32, p: u64) -> Vec<u8> {
    if p == 0 || n == 0 {
        return Vec::new();
    }
    // SAFETY: the caller's guarantee.
    unsafe { core::slice::from_raw_parts(p as *const u8, n as usize) }.to_vec()
}

// ---- DPAPI -------------------------------------------------------------------------------

/// The 32-byte master key of the user (in their home) or of the machine,
/// made on first use. Data protected under the user's key can be read by
/// that user only (the file is readable by its owner only); under the
/// machine key, by any local user, as DPAPI's machine scope promises (the
/// file is readable by everyone and written by whoever may create it).
fn master_key(machine: bool) -> Result<[u8; 32], u32> {
    static KEYS: Mutex<[Option<[u8; 32]>; 2]> = Mutex::new([None, None]);
    let mut keys = KEYS.lock().unwrap();
    if let Some(k) = keys[machine as usize] {
        return Ok(k);
    }
    let file = if machine {
        String::from("/etc/osyx/dpapi.key")
    } else {
        let home = os_std::env::var("HOME").unwrap_or_else(|_| String::from("/home/user"));
        alloc::format!("{}/.osyx-dpapi.key", home.trim_end_matches('/'))
    };
    let read = |f: &str| os_std::fs::read(f).ok().and_then(|d| <[u8; 32]>::try_from(d.as_slice()).ok());
    let key = match read(&file) {
        Some(k) => k,
        None => {
            let mut k = [0u8; 32];
            if !crate::ucrt::entropy(&mut k) {
                return Err(NTE_BAD_DATA);
            }
            // Exclusive creation: of two processes making the key at once,
            // one writes it and the other reads that one.
            let mode = if machine { 0o644 } else { 0o600 };
            match os_sys::open(&file, osyx_abi::OPEN_WRITE | osyx_abi::OPEN_CREATE | osyx_abi::OPEN_EXCLUSIVE, mode) {
                Ok(fd) => {
                    let written = os_sys::write_raw(fd, k.as_ptr() as usize, k.len());
                    let synced = os_sys::fsync(fd);
                    let _ = os_sys::close(fd);
                    if written != Ok(k.len()) || synced.is_err() {
                        let _ = os_sys::unlink(&file);
                        return Err(ERROR_WRITE_FAULT);
                    }
                    k
                }
                Err(osyx_abi::Errno::AlreadyExists) => read(&file).ok_or(ERROR_INVALID_DATA)?,
                Err(e) => return Err(errno_to_win32(e)),
            }
        }
    };
    keys[machine as usize] = Some(key);
    Ok(key)
}

fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = osyx_crypto::Sha256::new();
    for p in parts {
        h.update(p);
    }
    h.finish()
}

/// HMAC-SHA256 (RFC 2104) with a 32-byte key.
fn hmac(key: &[u8; 32], data: &[&[u8]]) -> [u8; 32] {
    let (mut ipad, mut opad) = ([0x36u8; 64], [0x5cu8; 64]);
    for i in 0..32 {
        ipad[i] ^= key[i];
        opad[i] ^= key[i];
    }
    let mut inner = osyx_crypto::Sha256::new();
    inner.update(&ipad);
    for d in data {
        inner.update(d);
    }
    let inner = inner.finish();
    sha256(&[&opad, &inner])
}

/// XORs `data` with the ChaCha20 keystream of `key` and `nonce`.
fn chacha20(key: &[u8; 32], nonce: &[u8; 12], data: &mut [u8]) {
    for (i, chunk) in data.chunks_mut(64).enumerate() {
        let block = osyx_crypto::chacha20_block(key, 1 + i as u32, nonce);
        for (b, k) in chunk.iter_mut().zip(block) {
            *b ^= k;
        }
    }
}

const MAGIC: &[u8; 4] = b"OSXD";
const CRYPTPROTECT_LOCAL_MACHINE: u32 = 0x4;

/// The keys of one protected blob: encryption and MAC, from the master key
/// and the caller's optional entropy.
fn blob_keys(master: &[u8; 32], entropy: &[u8]) -> ([u8; 32], [u8; 32]) {
    (sha256(&[b"osyx-dpapi-enc", master, entropy]), sha256(&[b"osyx-dpapi-mac", master, entropy]))
}

/// Writes a DATA_BLOB of `data` in LocalAlloc memory to `out`.
fn out_blob(out: u64, data: &[u8]) -> Result<(), u32> {
    let p = kernel32::heap_alloc(data.len().max(1), false).ok_or(ERROR_NOT_ENOUGH_MEMORY)?;
    // SAFETY: a fresh block of data.len() bytes; `out` is the caller's DATA_BLOB.
    unsafe {
        core::ptr::copy_nonoverlapping(data.as_ptr(), p as *mut u8, data.len());
        teb::put(out, 0, data.len() as u32);
        teb::put(out, 8, p);
    }
    Ok(())
}

/// CryptProtectData: the data encrypted (ChaCha20) and authenticated
/// (HMAC-SHA256) under the user's or the machine's master key and the
/// optional entropy. The blob is "OSXD", a version, the flags, the
/// description's length, a random nonce, the description (UTF-16), the
/// ciphertext and the MAC of all of that. There is no UI to prompt with.
extern "win64" fn CryptProtectData(input: u64, desc: *const u16, entropy: u64, _reserved: u64, _prompt: u64, flags: u32, out: u64) -> i32 {
    if input == 0 || out == 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let machine = flags & CRYPTPROTECT_LOCAL_MACHINE != 0;
    let master = match master_key(machine) {
        Ok(k) => k,
        Err(e) => return fail(e, 0),
    };
    // SAFETY: the caller's DATA_BLOBs and NUL-terminated description.
    let (mut data, entropy, desc) = unsafe {
        let d: Vec<u8> = path::wide_to_string(desc).unwrap_or_default().encode_utf16().flat_map(u16::to_le_bytes).collect();
        (blob_at(input), if entropy == 0 { Vec::new() } else { blob_at(entropy) }, d)
    };
    if desc.len() > u16::MAX as usize {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let mut nonce = [0u8; 12];
    if !crate::ucrt::entropy(&mut nonce) {
        return fail(NTE_BAD_DATA, 0);
    }
    let (enc, mac) = blob_keys(&master, &entropy);
    chacha20(&enc, &nonce, &mut data);
    let mut b = Vec::with_capacity(20 + desc.len() + data.len() + 32);
    b.extend_from_slice(MAGIC);
    b.push(1);
    b.push(machine as u8);
    b.extend_from_slice(&(desc.len() as u16).to_le_bytes());
    b.extend_from_slice(&nonce);
    b.extend_from_slice(&desc);
    b.extend_from_slice(&data);
    let tag = hmac(&mac, &[&b]);
    b.extend_from_slice(&tag);
    match out_blob(out, &b) {
        Ok(()) => 1,
        Err(e) => fail(e, 0),
    }
}

extern "win64" fn CryptUnprotectData(input: u64, desc: *mut u64, entropy: u64, _reserved: u64, _prompt: u64, _flags: u32, out: u64) -> i32 {
    if input == 0 || out == 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: the caller's DATA_BLOBs.
    let (b, entropy) = unsafe { (blob_at(input), if entropy == 0 { Vec::new() } else { blob_at(entropy) }) };
    if b.len() < 20 + 32 || &b[..4] != MAGIC || b[4] != 1 {
        return fail(ERROR_INVALID_DATA, 0);
    }
    let desc_len = u16::from_le_bytes([b[6], b[7]]) as usize;
    if 20 + desc_len + 32 > b.len() {
        return fail(ERROR_INVALID_DATA, 0);
    }
    let master = match master_key(b[5] != 0) {
        Ok(k) => k,
        Err(e) => return fail(e, 0),
    };
    let (enc, mac) = blob_keys(&master, &entropy);
    let (body, tag) = b.split_at(b.len() - 32);
    // Compared in constant time.
    if hmac(&mac, &[body]).iter().zip(tag).fold(0u8, |d, (x, y)| d | (x ^ y)) != 0 {
        return fail(NTE_BAD_DATA, 0);
    }
    let nonce: [u8; 12] = body[8..20].try_into().unwrap();
    let mut data = body[20 + desc_len..].to_vec();
    chacha20(&enc, &nonce, &mut data);
    if let Err(e) = out_blob(out, &data) {
        return fail(e, 0);
    }
    if !desc.is_null() {
        let mut units: Vec<u8> = body[20..20 + desc_len].to_vec();
        units.extend_from_slice(&[0, 0]);
        let Some(p) = kernel32::heap_alloc(units.len(), false) else { return fail(ERROR_NOT_ENOUGH_MEMORY, 0) };
        // SAFETY: a fresh block; the caller's out pointer.
        unsafe {
            core::ptr::copy_nonoverlapping(units.as_ptr(), p as *mut u8, units.len());
            *desc = p;
        }
    }
    1
}

const CRYPTPROTECTMEMORY_BLOCK_SIZE: u32 = 16;
const CRYPTPROTECTMEMORY_SAME_PROCESS: u32 = 0;
const CRYPTPROTECTMEMORY_CROSS_PROCESS: u32 = 1;
const CRYPTPROTECTMEMORY_SAME_LOGON: u32 = 2;

/// The key of a CryptProtectMemory scope: a random key of this process,
/// or one derived from the machine's or the user's master key.
fn memory_key(flags: u32) -> Result<[u8; 32], u32> {
    static PROCESS: Mutex<Option<[u8; 32]>> = Mutex::new(None);
    match flags {
        CRYPTPROTECTMEMORY_SAME_PROCESS => {
            let mut k = PROCESS.lock().unwrap();
            if k.is_none() {
                let mut v = [0u8; 32];
                if !crate::ucrt::entropy(&mut v) {
                    return Err(NTE_BAD_DATA);
                }
                *k = Some(v);
            }
            Ok(k.unwrap())
        }
        CRYPTPROTECTMEMORY_CROSS_PROCESS => Ok(sha256(&[b"osyx-protect-memory", &master_key(true)?])),
        CRYPTPROTECTMEMORY_SAME_LOGON => Ok(sha256(&[b"osyx-protect-memory", &master_key(false)?])),
        _ => Err(ERROR_INVALID_PARAMETER),
    }
}

/// Encrypts or decrypts `size` bytes in place (a multiple of 16) with the
/// scope's ChaCha20 keystream; the size never changes.
fn protect_memory(data: *mut u8, size: u32, flags: u32) -> i32 {
    if data.is_null() || size % CRYPTPROTECTMEMORY_BLOCK_SIZE != 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let key = match memory_key(flags) {
        Ok(k) => k,
        Err(e) => return fail(e, 0),
    };
    // SAFETY: the caller's `size` bytes.
    chacha20(&key, b"osyx-memory\0", unsafe { core::slice::from_raw_parts_mut(data, size as usize) });
    1
}

extern "win64" fn CryptProtectMemory(data: *mut u8, size: u32, flags: u32) -> i32 {
    protect_memory(data, size, flags)
}

extern "win64" fn CryptUnprotectMemory(data: *mut u8, size: u32, flags: u32) -> i32 {
    protect_memory(data, size, flags)
}

pub(super) fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        CertOpenSystemStoreW,
        CertOpenSystemStoreA,
        CertAddStoreToCollection,
        CertRemoveStoreFromCollection,
        CertControlStore,
        CertAddEncodedCertificateToStore,
        CertCompareCertificateName,
        CertVerifyTimeValidity,
        CertGetIntendedKeyUsage,
        CertGetNameStringA,
        CryptVerifyCertificateSignatureEx,
        CryptProtectData,
        CryptUnprotectData,
        CryptProtectMemory,
        CryptUnprotectMemory,
    ]
}
