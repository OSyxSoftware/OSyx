//! crypt32: encoding and certificate creation.
//!
//! DER for the structures CryptEncodeObjectEx knows, X.500 name strings
//! (CertStrToName), text to binary (CryptStringToBinary), certificate
//! hashes, certificate context properties, and self-signed certificates
//! signed with ncrypt keys. String types, error codes and encodings follow
//! what Windows produces for the same input.
use super::*;
use alloc::vec;

const CRYPT_E_INVALID_NUMERIC_STRING: u32 = 0x8009_2020;
const CRYPT_E_INVALID_PRINTABLE_STRING: u32 = 0x8009_2021;
const CRYPT_E_INVALID_IA5_STRING: u32 = 0x8009_2022;
const CRYPT_E_INVALID_X500_STRING: u32 = 0x8009_2023;
const E_INVALIDARG: u32 = 0x8007_0057;
const ERROR_INVALID_DATA: u32 = 13;
const NTE_BAD_ALGID: u32 = 0x8009_0008;
const NTE_BAD_FLAGS: u32 = 0x8009_0009;
const STATUS_NOT_FOUND: u32 = 0xC000_0225;

// ---- DER writing -----------------------------------------------------------------------------

/// One element: tag, definite length, contents.
pub(crate) fn der(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut v = vec![tag];
    let n = body.len();
    if n < 0x80 {
        v.push(n as u8);
    } else {
        let b = (n as u32).to_be_bytes();
        let skip = b.iter().take_while(|&&x| x == 0).count();
        v.push(0x80 | (4 - skip) as u8);
        v.extend_from_slice(&b[skip..]);
    }
    v.extend_from_slice(body);
    v
}

/// A non-negative INTEGER from a big-endian magnitude.
pub(crate) fn der_uint(be: &[u8]) -> Vec<u8> {
    let s = &be[be.iter().take_while(|&&x| x == 0).count()..];
    let mut body = Vec::new();
    if s.first().is_none_or(|&b| b & 0x80 != 0) {
        body.push(0);
    }
    body.extend_from_slice(s);
    der(INT, &body)
}

/// The minimal form of a big-endian two's-complement integer.
fn twos(be: &[u8]) -> Vec<u8> {
    let mut s = be;
    while s.len() > 1 && ((s[0] == 0 && s[1] & 0x80 == 0) || (s[0] == 0xff && s[1] & 0x80 != 0)) {
        s = &s[1..];
    }
    if s.is_empty() { vec![0] } else { s.to_vec() }
}

fn base128(out: &mut Vec<u8>, mut v: u64) {
    let mut tmp = [0u8; 10];
    let mut i = tmp.len();
    loop {
        i -= 1;
        tmp[i] = (v & 0x7f) as u8 | if i == tmp.len() - 1 { 0 } else { 0x80 };
        v >>= 7;
        if v == 0 {
            break;
        }
    }
    out.extend_from_slice(&tmp[i..]);
}

/// The contents of an OBJECT IDENTIFIER in dotted form.
fn oid_body(s: &str) -> Option<Vec<u8>> {
    let parts: Vec<u64> = s.split('.').map(|p| p.parse().ok()).collect::<Option<_>>()?;
    if parts.len() < 2 || parts[0] > 2 || (parts[0] < 2 && parts[1] >= 40) {
        return None;
    }
    let mut body = Vec::new();
    base128(&mut body, parts[0] * 40 + parts[1]);
    for &p in &parts[2..] {
        base128(&mut body, p);
    }
    Some(body)
}

pub(crate) fn der_oid(s: &str) -> Option<Vec<u8>> {
    oid_body(s).map(|b| der(OID, &b))
}

/// An AlgorithmIdentifier. CryptoAPI writes NULL parameters when there
/// are none, except for the ECDSA signature algorithms.
fn alg_id(oid: &str, params: &[u8]) -> Result<Vec<u8>, u32> {
    let mut body = der_oid(oid).ok_or(CRYPT_E_ASN1_BADTAG)?;
    if !params.is_empty() {
        body.extend_from_slice(params);
    } else if !oid.starts_with("1.2.840.10045.4.") {
        body.extend_from_slice(&[0x05, 0x00]);
    }
    Ok(der(SEQ, &body))
}

/// UTCTime for 1950 to 2049, GeneralizedTime otherwise (RFC 5280).
fn der_time(ft: u64) -> Vec<u8> {
    let [y, mo, _, d, h, mi, s, _] = crate::sysext::system_time(ft);
    if (1950..2050).contains(&y) {
        der(0x17, alloc::format!("{:02}{mo:02}{d:02}{h:02}{mi:02}{s:02}Z", y % 100).as_bytes())
    } else {
        der(0x18, alloc::format!("{y:04}{mo:02}{d:02}{h:02}{mi:02}{s:02}Z").as_bytes())
    }
}

// ---- reading the caller's structures ----------------------------------------------------------

fn rd32(p: u64, off: u64) -> u32 {
    // SAFETY: a field of a structure the caller passed.
    unsafe { *((p + off) as *const u32) }
}

fn rd64(p: u64, off: u64) -> u64 {
    // SAFETY: as above.
    unsafe { *((p + off) as *const u64) }
}

/// A {cbData, pbData} blob at `p`.
fn rd_blob<'a>(p: u64) -> &'a [u8] {
    let (n, d) = (rd32(p, 0) as usize, rd64(p, 8));
    if n == 0 || d == 0 {
        return &[];
    }
    // SAFETY: the blob's bytes.
    unsafe { core::slice::from_raw_parts(d as *const u8, n) }
}

fn rd_cstr(p: u64) -> Option<String> {
    // SAFETY: NULL or a NUL-terminated string.
    unsafe { path::ansi_to_string(p as *const u8) }
}

/// UTF-16 units: `bytes` long, or NUL-terminated when `bytes` is 0.
fn rd_wide(p: u64, bytes: u32) -> Vec<u16> {
    if p == 0 {
        return Vec::new();
    }
    // SAFETY: the caller's string.
    unsafe {
        if bytes == 0 {
            let mut n = 0;
            while *(p as *const u16).add(n) != 0 {
                n += 1;
            }
            core::slice::from_raw_parts(p as *const u16, n).to_vec()
        } else {
            core::slice::from_raw_parts(p as *const u16, bytes as usize / 2).to_vec()
        }
    }
}

/// An AlgorithmIdentifier from a CRYPT_ALGORITHM_IDENTIFIER (24 bytes).
fn rd_alg(p: u64) -> Result<Vec<u8>, u32> {
    let oid = rd_cstr(rd64(p, 0)).ok_or(E_INVALIDARG)?;
    alg_id(&oid, rd_blob(p + 8))
}

/// A BIT STRING from a CRYPT_BIT_BLOB, its bytes in order or reversed.
fn rd_bits(p: u64, reverse: bool) -> Vec<u8> {
    let mut body = vec![rd32(p, 16) as u8];
    let mut d = rd_blob(p).to_vec();
    if reverse {
        d.reverse();
    }
    body.extend_from_slice(&d);
    der(0x03, &body)
}

// ---- names -------------------------------------------------------------------------------------

const OID_C: &str = "2.5.4.6";
const OID_DC: &str = "0.9.2342.19200300.100.1.25";

/// X.500 key names CertStrToName accepts.
const X500_KEYS: &[(&str, &str)] = &[
    ("CN", "2.5.4.3"),
    ("SN", "2.5.4.4"),
    ("SERIALNUMBER", "2.5.4.5"),
    ("C", "2.5.4.6"),
    ("L", "2.5.4.7"),
    ("S", "2.5.4.8"),
    ("ST", "2.5.4.8"),
    ("STREET", "2.5.4.9"),
    ("O", "2.5.4.10"),
    ("OU", "2.5.4.11"),
    ("T", "2.5.4.12"),
    ("Title", "2.5.4.12"),
    ("Description", "2.5.4.13"),
    ("PostalCode", "2.5.4.17"),
    ("POBox", "2.5.4.18"),
    ("Phone", "2.5.4.20"),
    ("X21Address", "2.5.4.24"),
    ("G", "2.5.4.42"),
    ("GivenName", "2.5.4.42"),
    ("I", "2.5.4.43"),
    ("Initials", "2.5.4.43"),
    ("dnQualifier", "2.5.4.46"),
    ("E", "1.2.840.113549.1.9.1"),
    ("Email", "1.2.840.113549.1.9.1"),
    ("UnstructuredName", "1.2.840.113549.1.9.2"),
    ("UnstructuredAddress", "1.2.840.113549.1.9.8"),
    ("DC", OID_DC),
];

fn x500_oid(key: &str) -> Option<String> {
    if let Some((_, o)) = X500_KEYS.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)) {
        return Some(String::from(*o));
    }
    let dotted = key.strip_prefix("OID.").or_else(|| key.strip_prefix("oid.")).unwrap_or(key);
    oid_body(dotted).map(|_| String::from(dotted))
}

fn printable(c: char) -> bool {
    c.is_ascii_alphanumeric() || " '()+,-./:=?".contains(c)
}

// CERT_RDN_* value types.
const RDN_ANY: u32 = 0;
const RDN_ENCODED: u32 = 1;
const RDN_NUMERIC: u32 = 3;
const RDN_PRINTABLE: u32 = 4;
const RDN_T61: u32 = 5;
const RDN_IA5: u32 = 7;
const RDN_UNIVERSAL: u32 = 11;
const RDN_BMP: u32 = 12;
const RDN_UTF8: u32 = 13;

fn rdn_tag(kind: u32) -> Option<u8> {
    Some(match kind {
        2 => 0x04,
        3 => 0x12,
        4 => 0x13,
        5 => 0x14,
        6 => 0x15,
        7 => 0x16,
        8 => 0x19,
        9 => 0x1a,
        10 => 0x1b,
        11 => 0x1c,
        12 => 0x1e,
        13 => 0x0c,
        _ => return None,
    })
}

// CertStrToName flags.
const STR_SEMICOLON: u32 = 0x4000_0000;
const STR_NO_QUOTING: u32 = 0x1000_0000;
const STR_NO_PLUS: u32 = 0x2000_0000;
const STR_CRLF: u32 = 0x0800_0000;
const STR_COMMA: u32 = 0x0400_0000;
const STR_REVERSE: u32 = 0x0200_0000;
const STR_ENABLE_T61: u32 = 0x0002_0000;
const STR_ENABLE_UTF8: u32 = 0x0004_0000;
const STR_FORCE_UTF8: u32 = 0x0008_0000;

/// The string type CryptoAPI picks for an attribute value.
fn default_kind(oid: &str, text: &[char], flags: u32) -> u32 {
    if oid == OID_EMAIL || oid == OID_DC {
        RDN_IA5
    } else if matches!(oid, OID_C | "2.5.4.5" | "2.5.4.46") {
        RDN_PRINTABLE
    } else if flags & STR_FORCE_UTF8 != 0 {
        RDN_UTF8
    } else if text.iter().all(|&c| printable(c)) {
        RDN_PRINTABLE
    } else if flags & STR_ENABLE_UTF8 != 0 {
        RDN_UTF8
    } else if flags & STR_ENABLE_T61 != 0 && text.iter().all(|&c| (c as u32) < 0x100) {
        RDN_T61
    } else {
        RDN_BMP
    }
}

/// A string value of type `kind`; Err((code, index of the bad character)).
fn encode_text(kind: u32, text: &[char]) -> Result<Vec<u8>, (u32, usize)> {
    let tag = rdn_tag(kind).ok_or((E_INVALIDARG, 0))?;
    let check = |ok: fn(char) -> bool, code: u32| match text.iter().position(|&c| !ok(c)) {
        Some(i) => Err((code, i)),
        None => Ok(()),
    };
    let body: Vec<u8> = match kind {
        RDN_NUMERIC => {
            check(|c| c.is_ascii_digit() || c == ' ', CRYPT_E_INVALID_NUMERIC_STRING)?;
            text.iter().map(|&c| c as u8).collect()
        }
        RDN_PRINTABLE => {
            check(printable, CRYPT_E_INVALID_PRINTABLE_STRING)?;
            text.iter().map(|&c| c as u8).collect()
        }
        RDN_IA5 => {
            check(|c| c.is_ascii(), CRYPT_E_INVALID_IA5_STRING)?;
            text.iter().map(|&c| c as u8).collect()
        }
        RDN_UNIVERSAL => text.iter().flat_map(|&c| (c as u32).to_be_bytes()).collect(),
        RDN_BMP => text.iter().collect::<String>().encode_utf16().flat_map(u16::to_be_bytes).collect(),
        RDN_UTF8 => text.iter().collect::<String>().into_bytes(),
        // The 8-bit string types keep the low byte of each character.
        _ => text.iter().map(|&c| c as u32 as u8).collect(),
    };
    Ok(der(tag, &body))
}

fn chars(units: &[u16]) -> Vec<char> {
    char::decode_utf16(units.iter().copied()).map(|c| c.unwrap_or('\u{fffd}')).collect()
}

/// A Name from RDNs of (OID, value encoding); each RDN is a DER SET.
fn name(rdns: &[Vec<(String, Vec<u8>)>]) -> Result<Vec<u8>, u32> {
    let mut out = Vec::new();
    for rdn in rdns {
        let mut atvs = Vec::new();
        for (oid, value) in rdn {
            atvs.push(der(SEQ, &[der_oid(oid).ok_or(E_INVALIDARG)?, value.clone()].concat()));
        }
        atvs.sort();
        out.extend_from_slice(&der(SET, &atvs.concat()));
    }
    Ok(der(SEQ, &out))
}

/// CertStrToName: Err((code, index of the error in `s`)).
fn str_to_name(s: &[u16], kind: u32) -> Result<Vec<u8>, (u32, Option<usize>)> {
    if !matches!(kind & 0xffff, 2 | 3) {
        return Err((E_INVALIDARG, None));
    }
    let flags = kind;
    let is = |i: usize, c: char| s.get(i) == Some(&(c as u16));
    let separator = |i: usize| -> bool {
        let Some(&u) = s.get(i) else { return false };
        let c = u as u8 as char;
        if u > 0x7f {
            return false;
        }
        if c == '+' {
            return flags & STR_NO_PLUS == 0;
        }
        if flags & STR_SEMICOLON != 0 {
            c == ';'
        } else if flags & STR_COMMA != 0 {
            c == ','
        } else if flags & STR_CRLF != 0 {
            c == '\r' || c == '\n'
        } else {
            c == ',' || c == ';'
        }
    };
    let space = |i: usize| s.get(i).is_some_and(|&u| u == b' ' as u16 || u == b'\t' as u16 || u == b'\r' as u16 || u == b'\n' as u16);
    let mut rdns: Vec<Vec<(String, Vec<u8>)>> = Vec::new();
    let mut continues = false;
    let mut i = 0;
    loop {
        while space(i) || (separator(i) && !is(i, '+')) {
            i += 1;
        }
        if i >= s.len() {
            break;
        }
        let key_start = i;
        while i < s.len() && !is(i, '=') {
            i += 1;
        }
        if i >= s.len() {
            return Err((CRYPT_E_INVALID_X500_STRING, Some(key_start)));
        }
        let key = String::from_utf16_lossy(&s[key_start..i]);
        let oid = x500_oid(key.trim()).ok_or((CRYPT_E_INVALID_X500_STRING, Some(key_start)))?;
        i += 1;
        while space(i) && !separator(i) {
            i += 1;
        }
        let value_start = i;
        let mut value: Vec<u16> = Vec::new();
        if is(i, '"') && flags & STR_NO_QUOTING == 0 {
            i += 1;
            loop {
                if i >= s.len() {
                    return Err((CRYPT_E_INVALID_X500_STRING, Some(value_start)));
                }
                if is(i, '"') {
                    if is(i + 1, '"') {
                        value.push(b'"' as u16);
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                value.push(s[i]);
                i += 1;
            }
            while space(i) && !separator(i) {
                i += 1;
            }
            if i < s.len() && !separator(i) {
                return Err((CRYPT_E_INVALID_X500_STRING, Some(i)));
            }
        } else {
            while i < s.len() && !separator(i) {
                if is(i, '"') && flags & STR_NO_QUOTING == 0 {
                    return Err((CRYPT_E_INVALID_X500_STRING, Some(i)));
                }
                value.push(s[i]);
                i += 1;
            }
            while value.last().is_some_and(|&u| u == b' ' as u16 || u == b'\t' as u16 || u == b'\r' as u16 || u == b'\n' as u16) {
                value.pop();
            }
        }
        let text = chars(&value);
        let enc = encode_text(default_kind(&oid, &text, flags), &text).map_err(|(e, _)| (e, Some(value_start)))?;
        if continues {
            rdns.last_mut().unwrap().push((oid, enc));
        } else {
            rdns.push(vec![(oid, enc)]);
        }
        continues = is(i, '+') && separator(i);
        if i < s.len() {
            i += 1;
        }
    }
    if flags & STR_REVERSE != 0 {
        rdns.reverse();
    }
    name(&rdns).map_err(|e| (e, None))
}

/// The CryptoAPI output convention: a NULL buffer asks for the size, a
/// short one fails with ERROR_MORE_DATA and the size.
fn put_bytes(data: &[u8], out: *mut u8, size: *mut u32) -> i32 {
    if size.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: the caller's size pointer and buffer of *size bytes.
    unsafe {
        let cap = *size as usize;
        *size = data.len() as u32;
        if out.is_null() {
            return 1;
        }
        if cap < data.len() {
            return fail(ERROR_MORE_DATA, 0);
        }
        core::ptr::copy_nonoverlapping(data.as_ptr(), out, data.len());
    }
    1
}

fn str_to_name_api(s: Vec<u16>, kind: u32, out: *mut u8, size: *mut u32, err_at: impl FnOnce(Option<usize>)) -> i32 {
    match str_to_name(&s, kind) {
        Ok(d) => {
            err_at(None);
            put_bytes(&d, out, size)
        }
        Err((e, at)) => {
            err_at(at);
            fail(e, 0)
        }
    }
}

extern "win64" fn CertStrToNameW(_encoding: u32, x500: *const u16, kind: u32, _reserved: u64, out: *mut u8, size: *mut u32, error: *mut *const u16) -> i32 {
    // SAFETY: NULL or the caller's NUL-terminated string.
    let Some(s) = (unsafe { path::wide_to_string(x500) }) else { return fail(E_INVALIDARG, 0) };
    let units: Vec<u16> = s.encode_utf16().collect();
    str_to_name_api(units, kind, out, size, |at| {
        if !error.is_null() {
            // SAFETY: the caller's out pointer; `at` is inside the string.
            unsafe { *error = at.map_or(core::ptr::null(), |i| x500.add(i)) };
        }
    })
}

extern "win64" fn CertStrToNameA(_encoding: u32, x500: *const u8, kind: u32, _reserved: u64, out: *mut u8, size: *mut u32, error: *mut *const u8) -> i32 {
    // SAFETY: NULL or the caller's NUL-terminated string.
    let Some(units) = (unsafe { path::ansi_to_wide(x500) }) else { return fail(E_INVALIDARG, 0) };
    let units: Vec<u16> = units.into_iter().take_while(|&u| u != 0).collect();
    // The error offset counts bytes of the ANSI string: one per character
    // in a single-byte code page, the UTF-8 length otherwise.
    // SAFETY: the caller's NUL-terminated string.
    let single = unsafe { (0..).take_while(|&k| *x500.add(k) != 0).count() } == units.len();
    let prefix = units.clone();
    str_to_name_api(units, kind, out, size, |at| {
        if !error.is_null() {
            let off = at.map(|i| if single { i } else { String::from_utf16_lossy(&prefix[..i]).len() });
            // SAFETY: the caller's out pointer; `off` is inside the string.
            unsafe { *error = off.map_or(core::ptr::null(), |i| x500.add(i)) };
        }
    })
}

/// A CERT_NAME_INFO (X509_NAME or X509_UNICODE_NAME). Unicode values
/// that do not fit their type fail with the RDN, attribute and character
/// index packed as CryptoAPI does.
fn rd_name(p: u64, unicode: bool) -> Result<Vec<u8>, (u32, u32)> {
    let mut rdns = Vec::new();
    for r in 0..rd32(p, 0) as u64 {
        let rdn = rd64(p, 8) + 16 * r;
        let mut atvs = Vec::new();
        for a in 0..rd32(rdn, 0) as u64 {
            let at = rd64(rdn, 8) + 32 * a;
            let oid = rd_cstr(rd64(at, 0)).ok_or((E_INVALIDARG, 0))?;
            let kind = rd32(at, 8);
            let value = if kind == RDN_ENCODED {
                rd_blob(at + 16).to_vec()
            } else if kind == RDN_ANY && !unicode {
                return Err((E_INVALIDARG, 0));
            } else {
                // Unicode types are UTF-16 in both structures; X509_NAME
                // gives the 8-bit types as bytes.
                let wide = unicode || matches!(kind, RDN_UNIVERSAL | RDN_BMP | RDN_UTF8);
                let text: Vec<char> = if wide { chars(&rd_wide(rd64(at, 24), rd32(at, 16))) } else { rd_blob(at + 16).iter().map(|&b| b as char).collect() };
                let k = if kind == RDN_ANY { default_kind(&oid, &text, 0) } else { kind };
                encode_text(k, &text).map_err(|(e, i)| (e, (r as u32) << 22 | (a as u32) << 16 | i as u32))?
            };
            atvs.push((oid, value));
        }
        rdns.push(atvs);
    }
    name(&rdns).map_err(|e| (e, 0))
}

// ---- CryptEncodeObjectEx -----------------------------------------------------------------------

const X509_CERT: u64 = 1;
const X509_CERT_TO_BE_SIGNED: u64 = 2;
const X509_EXTENSIONS: u64 = 5;
const X509_NAME: u64 = 7;
const X509_PUBLIC_KEY_INFO: u64 = 8;
const X509_ALTERNATE_NAME: u64 = 12;
const X509_KEY_USAGE: u64 = 14;
const X509_BASIC_CONSTRAINTS2: u64 = 15;
const PKCS_UTC_TIME: u64 = 17;
const X509_UNICODE_NAME: u64 = 20;
const X509_OCTET_STRING: u64 = 25;
const X509_BITS: u64 = 26;
const X509_INTEGER: u64 = 27;
const X509_MULTI_BYTE_INTEGER: u64 = 28;
const X509_ENUMERATED: u64 = 29;
const X509_CHOICE_OF_TIME: u64 = 30;
const X509_SEQUENCE_OF_ANY: u64 = 34;
const X509_ENHANCED_KEY_USAGE: u64 = 36;
const X509_MULTI_BYTE_UINT: u64 = 38;
const X509_ECC_SIGNATURE: u64 = 47;
const X509_BITS_WITHOUT_TRAILING_ZEROES: u64 = 51;
const CNG_RSA_PUBLIC_KEY_BLOB: u64 = 72;
const X509_OBJECT_IDENTIFIER: u64 = 73;
const X509_ALGORITHM_IDENTIFIER: u64 = 74;

/// The predefined structure an OID string stands for.
fn struct_for_oid(oid: &str) -> Option<u64> {
    Some(match oid {
        "2.5.29.15" => X509_KEY_USAGE,
        "2.5.29.19" => X509_BASIC_CONSTRAINTS2,
        "2.5.29.37" => X509_ENHANCED_KEY_USAGE,
        "2.5.29.7" | "2.5.29.8" | "2.5.29.17" | "2.5.29.18" => X509_ALTERNATE_NAME,
        "2.5.29.14" => X509_OCTET_STRING,
        "1.2.840.113549.1.9.14" | "1.3.6.1.4.1.311.2.1.14" => X509_EXTENSIONS,
        "1.2.840.113549.1.9.5" => PKCS_UTC_TIME,
        _ => return None,
    })
}

/// CERT_ALT_NAME_ENTRY choices.
fn alt_name(e: u64) -> Result<Vec<u8>, (u32, u32)> {
    let ia5 = |tag: u8| -> Result<Vec<u8>, (u32, u32)> {
        let text = chars(&rd_wide(rd64(e, 8), 0));
        if let Some(i) = text.iter().position(|c| !c.is_ascii()) {
            return Err((CRYPT_E_INVALID_IA5_STRING, i as u32));
        }
        Ok(der(tag, &text.iter().map(|&c| c as u8).collect::<Vec<_>>()))
    };
    match rd32(e, 0) {
        1 => {
            let other = rd64(e, 8);
            let oid = der_oid(&rd_cstr(rd64(other, 0)).ok_or((E_INVALIDARG, 0))?).ok_or((E_INVALIDARG, 0))?;
            Ok(der(0xa0, &[oid, der(0xa0, rd_blob(other + 8))].concat()))
        }
        2 => ia5(0x81),
        3 => ia5(0x82),
        5 => Ok(der(0xa4, rd_blob(e + 8))),
        7 => ia5(0x86),
        8 => Ok(der(0x87, rd_blob(e + 8))),
        9 => Ok(der(0x88, &oid_body(&rd_cstr(rd64(e, 8)).ok_or((E_INVALIDARG, 0))?).ok_or((E_INVALIDARG, 0))?)),
        _ => Err((E_INVALIDARG, 0)),
    }
}

/// X509_KEY_USAGE: the bit string without trailing zero bits.
fn trimmed_bits(d: &[u8]) -> Vec<u8> {
    let n = d.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    let unused = if n == 0 { 0 } else { d[n - 1].trailing_zeros() as u8 };
    der(0x03, &[&[unused], &d[..n]].concat())
}

fn tbs(p: u64) -> Result<Vec<u8>, u32> {
    let mut b = Vec::new();
    let version = rd32(p, 0);
    if version != 0 {
        b.extend_from_slice(&der(0xa0, &der(INT, &[version as u8])));
    }
    let mut serial = rd_blob(p + 8).to_vec();
    serial.reverse();
    b.extend_from_slice(&der(INT, &twos(&serial)));
    b.extend_from_slice(&rd_alg(p + 24)?);
    b.extend_from_slice(rd_blob(p + 48));
    b.extend_from_slice(&der(SEQ, &[der_time(rd64(p, 64)), der_time(rd64(p, 72))].concat()));
    b.extend_from_slice(rd_blob(p + 80));
    b.extend_from_slice(&der(SEQ, &[rd_alg(p + 96)?, rd_bits(p + 120, false)].concat()));
    for (off, tag) in [(144, 0x81u8), (168, 0x82)] {
        if rd32(p + off, 0) != 0 {
            b.extend_from_slice(&der(tag, &[&[rd32(p + off, 16) as u8], rd_blob(p + off)].concat()));
        }
    }
    if rd32(p, 192) != 0 {
        b.extend_from_slice(&der(0xa3, &extensions(rd32(p, 192), rd64(p, 200))?));
    }
    Ok(der(SEQ, &b))
}

fn extensions(n: u32, at: u64) -> Result<Vec<u8>, u32> {
    let mut all = Vec::new();
    for i in 0..n as u64 {
        let e = at + 32 * i;
        let mut body = der_oid(&rd_cstr(rd64(e, 0)).ok_or(E_INVALIDARG)?).ok_or(E_INVALIDARG)?;
        if rd32(e, 8) != 0 {
            body.extend_from_slice(&[0x01, 0x01, 0xff]);
        }
        body.extend_from_slice(&der(0x04, rd_blob(e + 16)));
        all.extend_from_slice(&der(SEQ, &body));
    }
    Ok(der(SEQ, &all))
}

/// Encodes structure `kind` at `v`: Err((code, value for *pcbEncoded)).
fn encode(kind: u64, v: u64) -> Result<Vec<u8>, (u32, u32)> {
    let e = |c: u32| (c, 0);
    if v == 0 {
        return Err(e(E_INVALIDARG));
    }
    Ok(match kind {
        X509_CERT => {
            let sig_alg = rd_alg(v + 16).map_err(e)?;
            der(SEQ, &[rd_blob(v).to_vec(), sig_alg, rd_bits(v + 40, true)].concat())
        }
        X509_CERT_TO_BE_SIGNED => tbs(v).map_err(e)?,
        X509_EXTENSIONS => extensions(rd32(v, 0), rd64(v, 8)).map_err(e)?,
        X509_NAME => rd_name(v, false)?,
        X509_UNICODE_NAME => rd_name(v, true)?,
        X509_PUBLIC_KEY_INFO => der(SEQ, &[rd_alg(v).map_err(e)?, rd_bits(v + 24, false)].concat()),
        X509_ALGORITHM_IDENTIFIER => rd_alg(v).map_err(e)?,
        X509_ALTERNATE_NAME => {
            let mut all = Vec::new();
            for i in 0..rd32(v, 0) as u64 {
                all.extend_from_slice(&alt_name(rd64(v, 8) + 24 * i).map_err(|(c, at)| (c, (i as u32) << 16 | at))?);
            }
            der(SEQ, &all)
        }
        X509_KEY_USAGE | X509_BITS_WITHOUT_TRAILING_ZEROES => trimmed_bits(rd_blob(v)),
        X509_BITS => rd_bits(v, false),
        X509_BASIC_CONSTRAINTS2 => {
            let mut body = Vec::new();
            if rd32(v, 0) != 0 {
                body.extend_from_slice(&[0x01, 0x01, 0xff]);
            }
            if rd32(v, 4) != 0 {
                body.extend_from_slice(&der(INT, &twos(&rd32(v, 8).to_be_bytes())));
            }
            der(SEQ, &body)
        }
        X509_OCTET_STRING => der(0x04, rd_blob(v)),
        X509_INTEGER => der(INT, &twos(&(rd32(v, 0) as i32).to_be_bytes())),
        X509_ENUMERATED => der(0x0a, &twos(&(rd32(v, 0) as i32).to_be_bytes())),
        X509_MULTI_BYTE_INTEGER => {
            let mut d = rd_blob(v).to_vec();
            d.reverse();
            der(INT, &twos(&d))
        }
        X509_MULTI_BYTE_UINT => {
            let mut d = rd_blob(v).to_vec();
            d.reverse();
            der_uint(&d)
        }
        X509_ECC_SIGNATURE => {
            let int = |p: u64| {
                let mut d = rd_blob(p).to_vec();
                d.reverse();
                der_uint(&d)
            };
            der(SEQ, &[int(v), int(v + 16)].concat())
        }
        PKCS_UTC_TIME => {
            let t = der_time(rd64(v, 0));
            if t[0] != 0x17 {
                return Err(e(CRYPT_E_BAD_ENCODE));
            }
            t
        }
        X509_CHOICE_OF_TIME => der_time(rd64(v, 0)),
        X509_SEQUENCE_OF_ANY => {
            let mut all = Vec::new();
            for i in 0..rd32(v, 0) as u64 {
                all.extend_from_slice(rd_blob(rd64(v, 8) + 16 * i));
            }
            der(SEQ, &all)
        }
        X509_ENHANCED_KEY_USAGE => {
            let mut all = Vec::new();
            for i in 0..rd32(v, 0) as u64 {
                let oid = rd_cstr(rd64(rd64(v, 8) + 8 * i, 0)).ok_or(e(E_INVALIDARG))?;
                all.extend_from_slice(&der_oid(&oid).ok_or(e(E_INVALIDARG))?);
            }
            der(SEQ, &all)
        }
        X509_OBJECT_IDENTIFIER => der_oid(&rd_cstr(rd64(v, 0)).ok_or(e(E_INVALIDARG))?).ok_or(e(E_INVALIDARG))?,
        CNG_RSA_PUBLIC_KEY_BLOB => {
            // BCRYPT_RSAKEY_BLOB header, exponent, modulus (big-endian).
            let (el, nl) = (rd32(v, 8) as usize, rd32(v, 12) as usize);
            // SAFETY: the blob the header describes.
            let body = unsafe { core::slice::from_raw_parts((v + 24) as *const u8, el + nl) };
            der(SEQ, &[der_uint(&body[el..]), der_uint(&body[..el])].concat())
        }
        _ => return Err(e(ERROR_FILE_NOT_FOUND)),
    })
}

const CRYPT_E_BAD_ENCODE: u32 = 0x8009_3002;
const CRYPT_ENCODE_ALLOC_FLAG: u32 = 0x8000;

fn struct_kind(kind: *const u8) -> Option<u64> {
    let k = kind as u64;
    if k <= 0xffff { Some(k) } else { rd_cstr(k).as_deref().and_then(struct_for_oid) }
}

#[allow(clippy::too_many_arguments)]
extern "win64" fn CryptEncodeObjectEx(encoding: u32, kind: *const u8, info: u64, flags: u32, para: u64, out: *mut u8, size: *mut u32) -> i32 {
    if size.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let Some(k) = struct_kind(kind).filter(|_| encoding & X509_ASN_ENCODING != 0) else {
        // SAFETY: the caller's size pointer.
        unsafe { *size = 0 };
        return fail(ERROR_FILE_NOT_FOUND, 0);
    };
    let d = match encode(k, info) {
        Ok(d) => d,
        Err((e, at)) => {
            // SAFETY: as above.
            unsafe { *size = at };
            return fail(e, 0);
        }
    };
    if flags & CRYPT_ENCODE_ALLOC_FLAG == 0 {
        return put_bytes(&d, out, size);
    }
    // CRYPT_ENCODE_PARA {cbSize, pfnAlloc, pfnFree} or LocalAlloc.
    let alloc_fn = if para != 0 && rd32(para, 0) >= 16 { rd64(para, 8) } else { 0 };
    let p = if alloc_fn != 0 {
        // SAFETY: the caller's PFN_CRYPT_ALLOC.
        let f: extern "win64" fn(usize) -> u64 = unsafe { core::mem::transmute(alloc_fn) };
        f(d.len())
    } else {
        kernel32::heap_alloc(d.len(), false).unwrap_or(0)
    };
    if p == 0 || out.is_null() {
        return fail(ERROR_NOT_ENOUGH_MEMORY, 0);
    }
    // SAFETY: a fresh block of d.len() bytes; `out` is the caller's
    // pointer to a pointer.
    unsafe {
        core::ptr::copy_nonoverlapping(d.as_ptr(), p as *mut u8, d.len());
        *(out as *mut u64) = p;
        *size = d.len() as u32;
    }
    1
}

extern "win64" fn CryptEncodeObject(encoding: u32, kind: *const u8, info: u64, out: *mut u8, size: *mut u32) -> i32 {
    CryptEncodeObjectEx(encoding, kind, info, 0, 0, out, size)
}

// ---- text to binary ------------------------------------------------------------------------------

fn b64_value(c: u16) -> Option<u32> {
    let c = char::from_u32(c as u32)?;
    Some(match c {
        'A'..='Z' => c as u32 - 'A' as u32,
        'a'..='z' => c as u32 - 'a' as u32 + 26,
        '0'..='9' => c as u32 - '0' as u32 + 52,
        '+' => 62,
        '/' => 63,
        _ => return None,
    })
}

fn white(c: u16) -> bool {
    c == b' ' as u16 || c == b'\t' as u16 || c == b'\r' as u16 || c == b'\n' as u16
}

fn unbase64(s: &[u16]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut acc, mut n, mut pad) = (0u32, 0, 0);
    for &c in s {
        if white(c) {
            continue;
        }
        if c == b'=' as u16 {
            pad += 1;
            continue;
        }
        if pad > 0 {
            return None; // data after padding
        }
        acc = acc << 6 | b64_value(c)?;
        n += 1;
        if n == 4 {
            out.extend_from_slice(&acc.to_be_bytes()[1..]);
            (acc, n) = (0, 0);
        }
    }
    match n {
        0 => {}
        2 => out.push((acc >> 4) as u8),
        3 => out.extend_from_slice(&((acc >> 2) as u16).to_be_bytes()),
        _ => return None,
    }
    Some(out)
}

/// "-----BEGIN x-----" ... "-----END x-----": (skip, decoded body).
fn unheader(s: &[u16]) -> Option<(usize, Vec<u8>)> {
    let text = String::from_utf16_lossy(s);
    let begin = text.find("-----BEGIN")?;
    let open_end = begin + 10 + text[begin + 10..].find("-----")? + 5;
    let end = open_end + text[open_end..].find("-----END")?;
    let skip = text[..begin].encode_utf16().count();
    let units: Vec<u16> = text[open_end..end].encode_utf16().collect();
    Some((skip, unbase64(&units)?))
}

fn unhex(s: &[u16]) -> Option<Vec<u8>> {
    let mut digits = Vec::new();
    for &c in s {
        if white(c) {
            continue;
        }
        digits.push(char::from_u32(c as u32)?.to_digit(16)? as u8);
    }
    if digits.len() % 2 != 0 {
        return None;
    }
    Some(digits.chunks(2).map(|p| p[0] << 4 | p[1]).collect())
}

/// Hex dumps with an address column (and, for HEXASCIIADDR, a text one).
fn unhex_addr(s: &[u16], ascii: bool) -> Option<Vec<u8>> {
    let text = String::from_utf16_lossy(s);
    let mut out = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let mut words = line.split_whitespace();
        let addr = words.next()?;
        if addr.len() < 4 || !addr.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        for w in words {
            if w.len() != 2 || !w.chars().all(|c| c.is_ascii_hexdigit()) {
                if ascii {
                    break; // the text column
                }
                return None;
            }
            out.push(u8::from_str_radix(w, 16).ok()?);
        }
    }
    Some(out)
}

const CRYPT_STRING_BASE64_ANY: u32 = 6;
const CRYPT_STRING_ANY: u32 = 7;
const CRYPT_STRING_HEX_ANY: u32 = 8;
const CRYPT_STRING_HEXADDR: u32 = 0xa;
const CRYPT_STRING_HEXASCIIADDR: u32 = 0xb;

/// (bytes, skipped characters, format found).
fn string_to_binary(s: &[u16], raw: &[u8], flags: u32) -> Option<(Vec<u8>, u32, u32)> {
    let header = |f: u32| unheader(s).map(|(skip, d)| (d, skip as u32, f));
    match flags {
        CRYPT_STRING_BASE64HEADER | CRYPT_STRING_BASE64REQUESTHEADER | CRYPT_STRING_BASE64X509CRLHEADER => header(flags),
        CRYPT_STRING_BASE64 => unbase64(s).map(|d| (d, 0, flags)),
        CRYPT_STRING_BINARY => Some((raw.to_vec(), 0, flags)),
        CRYPT_STRING_HEX | CRYPT_STRING_HEXRAW => unhex(s).map(|d| (d, 0, flags)),
        CRYPT_STRING_HEXADDR => unhex_addr(s, false).map(|d| (d, 0, flags)),
        CRYPT_STRING_HEXASCIIADDR => unhex_addr(s, true).map(|d| (d, 0, flags)),
        CRYPT_STRING_BASE64_ANY => header(CRYPT_STRING_BASE64HEADER).or_else(|| unbase64(s).map(|d| (d, 0, CRYPT_STRING_BASE64))),
        CRYPT_STRING_ANY => string_to_binary(s, raw, CRYPT_STRING_BASE64_ANY).or(Some((raw.to_vec(), 0, CRYPT_STRING_BINARY))),
        CRYPT_STRING_HEX_ANY => unhex_addr(s, false)
            .filter(|_| s.iter().any(|&c| c == b'\n' as u16))
            .map(|d| (d, 0, CRYPT_STRING_HEXADDR))
            .or_else(|| unhex(s).map(|d| (d, 0, CRYPT_STRING_HEX))),
        _ => None,
    }
}

fn string_to_binary_api(s: &[u16], raw: &[u8], flags: u32, out: *mut u8, size: *mut u32, skip: *mut u32, found: *mut u32) -> i32 {
    if size.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let Some((d, sk, f)) = string_to_binary(s, raw, flags) else { return fail(ERROR_INVALID_DATA, 0) };
    // SAFETY: the caller's out pointers and buffer of *size bytes.
    unsafe {
        if !out.is_null() {
            // Windows leaves the size alone when the buffer is short.
            if (*size as usize) < d.len() {
                return fail(ERROR_MORE_DATA, 0);
            }
            core::ptr::copy_nonoverlapping(d.as_ptr(), out, d.len());
        }
        *size = d.len() as u32;
        if !skip.is_null() {
            *skip = sk;
        }
        if !found.is_null() {
            *found = f;
        }
    }
    1
}

/// The characters of a counted string, or up to its NUL when `len` is 0.
unsafe fn counted<T: Copy + PartialEq + Default>(p: *const T, len: u32) -> &'static [T] {
    // SAFETY: per the caller.
    unsafe {
        let n = if len == 0 { (0..).take_while(|&k| *p.add(k) != T::default()).count() } else { len as usize };
        core::slice::from_raw_parts(p, n)
    }
}

extern "win64" fn CryptStringToBinaryW(s: *const u16, len: u32, flags: u32, out: *mut u8, size: *mut u32, skip: *mut u32, found: *mut u32) -> i32 {
    if s.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: the caller's string.
    let units = unsafe { counted(s, len) };
    let raw: Vec<u8> = units.iter().flat_map(|u| u.to_le_bytes()).collect();
    string_to_binary_api(units, &raw, flags, out, size, skip, found)
}

extern "win64" fn CryptStringToBinaryA(s: *const u8, len: u32, flags: u32, out: *mut u8, size: *mut u32, skip: *mut u32, found: *mut u32) -> i32 {
    if s.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: the caller's string.
    let bytes = unsafe { counted(s, len) };
    let units: Vec<u16> = bytes.iter().map(|&b| b as u16).collect();
    string_to_binary_api(&units, bytes, flags, out, size, skip, found)
}

extern "win64" fn CryptBinaryToStringA(data: *const u8, len: u32, flags: u32, out: *mut u8, size: *mut u32) -> i32 {
    if size.is_null() || (data.is_null() && len != 0) {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    // SAFETY: the caller's `len` bytes.
    let bytes = if len == 0 { &[][..] } else { unsafe { core::slice::from_raw_parts(data, len as usize) } };
    let s = match binary_string(bytes, flags) {
        Ok(s) => s,
        Err(e) => return fail(e, 0),
    };
    // SAFETY: the caller's size pointer and buffer of *size characters.
    unsafe {
        if out.is_null() {
            *size = s.len() as u32 + 1;
            return 1;
        }
        if (*size as usize) < s.len() + 1 {
            *size = s.len() as u32 + 1;
            return fail(ERROR_MORE_DATA, 0);
        }
        core::ptr::copy_nonoverlapping(s.as_ptr(), out, s.len());
        *out.add(s.len()) = 0;
        *size = s.len() as u32;
    }
    1
}

// ---- hashes ----------------------------------------------------------------------------------------

fn bytes<'a>(p: *const u8, n: u32) -> &'a [u8] {
    if p.is_null() || n == 0 {
        return &[];
    }
    // SAFETY: the caller's `n` bytes.
    unsafe { core::slice::from_raw_parts(p, n as usize) }
}

#[allow(clippy::too_many_arguments)]
extern "win64" fn CryptHashCertificate2(alg: *const u16, _flags: u32, _reserved: u64, data: *const u8, len: u32, out: *mut u8, size: *mut u32) -> i32 {
    // SAFETY: NULL or the caller's NUL-terminated algorithm name.
    let name = unsafe { path::wide_to_string(alg) }.unwrap_or_default();
    match crate::bcrypt::hash_named(&name, bytes(data, len)) {
        Some(h) => put_bytes(&h, out, size),
        None => fail(STATUS_NOT_FOUND, 0),
    }
}

/// CryptHashCertificate: legacy ALG_IDs (SHA-1 when 0).
#[allow(clippy::too_many_arguments)]
extern "win64" fn CryptHashCertificate(_prov: u64, alg: u32, _flags: u32, data: *const u8, len: u32, out: *mut u8, size: *mut u32) -> i32 {
    let name = match alg {
        0 | 0x8004 => "SHA1",
        0x8003 => "MD5",
        0x800c => "SHA256",
        0x800d => "SHA384",
        0x800e => "SHA512",
        _ => return fail(NTE_BAD_ALGID, 0),
    };
    put_bytes(&crate::bcrypt::hash_named(name, bytes(data, len)).unwrap_or_default(), out, size)
}

// ---- certificate properties ---------------------------------------------------------------------

const CERT_KEY_PROV_INFO_PROP_ID: u32 = 2;
const CERT_SHA1_HASH_PROP_ID: u32 = 3;
const CERT_MD5_HASH_PROP_ID: u32 = 4;
const CERT_KEY_CONTEXT_PROP_ID: u32 = 5;
const CERT_KEY_IDENTIFIER_PROP_ID: u32 = 20;
const CERT_DATE_STAMP_PROP_ID: u32 = 27;
const CERT_NCRYPT_KEY_HANDLE_PROP_ID: u32 = 78;
const CERT_HCRYPTPROV_OR_NCRYPT_KEY_HANDLE_PROP_ID: u32 = 79;
const CERT_HCRYPTPROV_TRANSFER_PROP_ID: u32 = 80;
const CERT_SHA256_HASH_PROP_ID: u32 = 107;

/// A key provider: CRYPT_KEY_PROV_INFO without its parameters' layout.
#[derive(Clone)]
struct ProvInfo {
    container: Option<String>,
    provider: Option<String>,
    prov_type: u32,
    flags: u32,
    /// (dwParam, data, dwFlags)
    params: Vec<(u32, Vec<u8>, u32)>,
    key_spec: u32,
}

#[derive(Clone)]
enum Prop {
    Bytes(Vec<u8>),
    ProvInfo(ProvInfo),
}

/// Properties by certificate: (store, index) for a store's certificate,
/// (0, context address) for one in no store.
static PROPS: Mutex<BTreeMap<(u64, u64), BTreeMap<u32, Prop>>> = Mutex::new(BTreeMap::new());

fn prop_key(ctx: u64) -> Option<(u64, u64)> {
    let h = HANDLES.lock().unwrap();
    let c = h.contexts.get(&ctx)?;
    Some(if c.store == 0 { (0, ctx) } else { (c.store, c.index as u64) })
}

pub(super) fn forget_context(ctx: u64) {
    PROPS.lock().unwrap().remove(&(0, ctx));
}

pub(super) fn forget_store(store: u64) {
    PROPS.lock().unwrap().retain(|k, _| k.0 != store);
}

/// CertAddCertificateContextToStore copies the source's properties.
pub(super) fn copy_props(from: u64, store: u64, index: usize) {
    let Some(k) = prop_key(from) else { return };
    let mut p = PROPS.lock().unwrap();
    if let Some(v) = p.get(&k).cloned() {
        p.insert((store, index as u64), v);
    }
}

fn set_prop(ctx: u64, id: u32, value: Option<Prop>) -> bool {
    let Some(k) = prop_key(ctx) else { return false };
    let mut p = PROPS.lock().unwrap();
    match value {
        Some(v) => {
            p.entry(k).or_default().insert(id, v);
        }
        None => {
            if let Some(m) = p.get_mut(&k) {
                m.remove(&id);
            }
        }
    }
    true
}

fn get_prop(ctx: u64, id: u32) -> Option<Prop> {
    let k = prop_key(ctx)?;
    PROPS.lock().unwrap().get(&k).and_then(|m| m.get(&id).cloned())
}

fn wide_z(s: &str) -> Vec<u8> {
    s.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect()
}

/// CRYPT_KEY_PROV_INFO (48 bytes) with its strings and parameters after.
fn prov_info_block(i: &ProvInfo) -> Block {
    let mut b = Block::new(48);
    if let Some(c) = &i.container {
        b.point(0, &wide_z(c));
    }
    if let Some(p) = &i.provider {
        b.point(8, &wide_z(p));
    }
    b.put32(16, i.prov_type);
    b.put32(20, i.flags);
    b.put32(24, i.params.len() as u32);
    if !i.params.is_empty() {
        // CRYPT_KEY_PROV_PARAM {dwParam, pbData, cbData, dwFlags}: 24 bytes.
        let arr = b.reserve(24 * i.params.len());
        b.fixups.push((32, arr));
        for (k, (param, data, flags)) in i.params.iter().enumerate() {
            let e = arr + 24 * k;
            b.put32(e, *param);
            if !data.is_empty() {
                b.point(e + 8, data);
            }
            b.put32(e + 16, data.len() as u32);
            b.put32(e + 20, *flags);
        }
    }
    b.put32(40, i.key_spec);
    b
}

fn rd_prov_info(p: u64) -> ProvInfo {
    let s = |q: u64| (q != 0).then(|| String::from_utf16_lossy(&rd_wide(q, 0)));
    let params = (0..rd32(p, 24) as u64)
        .map(|k| {
            let e = rd64(p, 32) + 24 * k;
            let (d, n) = (rd64(e, 8), rd32(e, 16));
            // SAFETY: the parameter's bytes.
            let data = if d == 0 || n == 0 { Vec::new() } else { unsafe { core::slice::from_raw_parts(d as *const u8, n as usize).to_vec() } };
            (rd32(e, 0), data, rd32(e, 20))
        })
        .collect();
    ProvInfo { container: s(rd64(p, 0)), provider: s(rd64(p, 8)), prov_type: rd32(p, 16), flags: rd32(p, 20), params, key_spec: rd32(p, 40) }
}

extern "win64" fn CertSetCertificateContextProperty(ctx: u64, id: u32, _flags: u32, data: u64) -> i32 {
    if context_der(ctx).is_none() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let value = (data != 0).then(|| match id {
        CERT_KEY_PROV_INFO_PROP_ID => Prop::ProvInfo(rd_prov_info(data)),
        CERT_NCRYPT_KEY_HANDLE_PROP_ID | CERT_HCRYPTPROV_OR_NCRYPT_KEY_HANDLE_PROP_ID | CERT_HCRYPTPROV_TRANSFER_PROP_ID => {
            Prop::Bytes(rd64(data, 0).to_le_bytes().to_vec())
        }
        CERT_DATE_STAMP_PROP_ID => Prop::Bytes(rd64(data, 0).to_le_bytes().to_vec()),
        CERT_KEY_CONTEXT_PROP_ID => {
            // CERT_KEY_CONTEXT {cbSize, hCryptProv / hNCryptKey, dwKeySpec}.
            // SAFETY: the caller's structure of cbSize bytes.
            Prop::Bytes(unsafe { core::slice::from_raw_parts(data as *const u8, (rd32(data, 0) as usize).clamp(4, 24)).to_vec() })
        }
        _ => Prop::Bytes(rd_blob(data).to_vec()),
    });
    if set_prop(ctx, id, value) { 1 } else { fail(ERROR_INVALID_PARAMETER, 0) }
}

/// Hash properties are computed when not set.
fn computed_prop(der: &[u8], id: u32) -> Option<Vec<u8>> {
    match id {
        CERT_SHA1_HASH_PROP_ID => crate::bcrypt::hash_named("SHA1", der),
        CERT_MD5_HASH_PROP_ID => crate::bcrypt::hash_named("MD5", der),
        CERT_SHA256_HASH_PROP_ID => crate::bcrypt::hash_named("SHA256", der),
        CERT_KEY_IDENTIFIER_PROP_ID => {
            let c = parse_cert(der).ok()?;
            // The subject key identifier extension, else SHA-1 of the key.
            match c.extensions.iter().find(|e| e.0 == "2.5.29.14").and_then(|e| tlv(&e.2).ok()) {
                Some((t, _)) if t.tag == 0x04 => Some(t.body.to_vec()),
                _ => crate::bcrypt::hash_named("SHA1", &c.key.0),
            }
        }
        _ => None,
    }
}

extern "win64" fn CertGetCertificateContextProperty(ctx: u64, id: u32, out: *mut u8, size: *mut u32) -> i32 {
    let Some(der) = context_der(ctx) else { return fail(ERROR_INVALID_PARAMETER, 0) };
    if size.is_null() {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    match get_prop(ctx, id) {
        Some(Prop::Bytes(b)) => put_bytes(&b, out, size),
        Some(Prop::ProvInfo(i)) => {
            let block = prov_info_block(&i);
            let n = block.bytes.len();
            // SAFETY: the caller's size pointer and buffer of *size bytes.
            unsafe {
                let cap = *size as usize;
                *size = n as u32;
                if out.is_null() {
                    return 1;
                }
                if cap < n {
                    return fail(ERROR_MORE_DATA, 0);
                }
                let bytes = block.finish(out as u64);
                core::ptr::copy_nonoverlapping(bytes.as_ptr(), out, n);
            }
            1
        }
        None => match computed_prop(&der, id) {
            Some(b) => put_bytes(&b, out, size),
            None => fail(CRYPT_E_NOT_FOUND, 0),
        },
    }
}

const CRYPT_ACQUIRE_CACHE_FLAG: u32 = 0x1;
const CERT_NCRYPT_KEY_SPEC: u32 = 0xffff_ffff;
const CRYPT_MACHINE_KEYSET: u32 = 0x20;

/// The certificate's private key: its CNG key container (opened anew),
/// else a key handle set as a property.
pub(super) fn acquire_private_key(ctx: u64, flags: u32, key: *mut u64, spec: *mut u32, caller_free: *mut i32) -> i32 {
    let (handle, free) = match get_prop(ctx, CERT_KEY_PROV_INFO_PROP_ID) {
        Some(Prop::ProvInfo(i)) if i.prov_type == 0 && i.container.is_some() => {
            match crate::ncrypt::open_named(i.container.as_deref().unwrap_or_default(), i.flags & CRYPT_MACHINE_KEYSET != 0) {
                Ok(h) if flags & CRYPT_ACQUIRE_CACHE_FLAG != 0 => {
                    set_prop(ctx, CERT_NCRYPT_KEY_HANDLE_PROP_ID, Some(Prop::Bytes(h.to_le_bytes().to_vec())));
                    (h, false)
                }
                Ok(h) => (h, true),
                Err(e) => return fail(e, 0),
            }
        }
        _ => match get_prop(ctx, CERT_NCRYPT_KEY_HANDLE_PROP_ID).or_else(|| get_prop(ctx, CERT_HCRYPTPROV_OR_NCRYPT_KEY_HANDLE_PROP_ID)) {
            Some(Prop::Bytes(b)) if b.len() == 8 => (u64::from_le_bytes(b.try_into().unwrap()), false),
            _ => return fail(CRYPT_E_NO_KEY_PROPERTY, 0),
        },
    };
    // SAFETY: the caller's out pointers.
    unsafe {
        if !key.is_null() {
            *key = handle;
        }
        if !spec.is_null() {
            *spec = CERT_NCRYPT_KEY_SPEC;
        }
        if !caller_free.is_null() {
            *caller_free = free as i32;
        }
    }
    1
}

// ---- self-signed certificates ----------------------------------------------------------------------

const CERT_CREATE_SELFSIGN_NO_SIGN: u32 = 1;
const CERT_CREATE_SELFSIGN_NO_KEY_INFO: u32 = 2;

/// The hash a signature algorithm OID uses.
fn sig_hash(oid: &str, rsa: bool) -> Option<&'static str> {
    let h = match oid {
        "1.2.840.113549.1.1.4" => "MD5",
        "1.2.840.113549.1.1.5" | "1.3.14.3.2.29" | "1.2.840.10045.4.1" => "SHA1",
        "1.2.840.113549.1.1.11" | "1.2.840.10045.4.3.2" => "SHA256",
        "1.2.840.113549.1.1.12" | "1.2.840.10045.4.3.3" => "SHA384",
        "1.2.840.113549.1.1.13" | "1.2.840.10045.4.3.4" => "SHA512",
        _ => return None,
    };
    (oid.starts_with("1.2.840.10045.") != rsa).then_some(h)
}

/// ECDSA r || s as the DER Ecdsa-Sig-Value certificates carry.
fn ecdsa_der(rs: &[u8]) -> Vec<u8> {
    let (r, s) = rs.split_at(rs.len() / 2);
    der(SEQ, &[der_uint(r), der_uint(s)].concat())
}

fn self_signed(key: u64, subject: &[u8], flags: u32, sig_oid: Option<String>, start: Option<u64>, end: Option<u64>, ext: Option<(u32, u64)>) -> Result<(Vec<u8>, crate::ncrypt::Facts), u32> {
    let f = crate::ncrypt::facts(key)?;
    // Windows names the key in the certificate's provider information,
    // so an ephemeral (unnamed) key cannot sign one.
    if f.name.is_none() && flags & CERT_CREATE_SELFSIGN_NO_KEY_INFO == 0 {
        return Err(NTE_BAD_FLAGS);
    }
    let sig_oid = sig_oid.unwrap_or_else(|| String::from(if f.rsa { "1.2.840.113549.1.1.5" } else { "1.2.840.10045.4.1" }));
    let hash = sig_hash(&sig_oid, f.rsa).ok_or(NTE_BAD_ALGID)?;
    let sig_alg = alg_id(&sig_oid, &[])?;
    let mut serial = [0u8; 16];
    crate::ucrt::entropy(&mut serial);
    serial[0] &= 0x7f;
    // Whole seconds now, and a year of 365.25 days later, as Windows does.
    let start = start.unwrap_or_else(|| crate::sysext::now_filetime() / 10_000_000 * 10_000_000);
    let end = end.unwrap_or(start + 315_576_000_000_000);
    let spki = der(SEQ, &[alg_id(f.alg_oid, &f.alg_params)?, der(0x03, &[&[0u8], &f.public[..]].concat())].concat());
    let mut body = Vec::new();
    body.extend_from_slice(&der(0xa0, &der(INT, &[2])));
    body.extend_from_slice(&der(INT, &twos(&serial)));
    body.extend_from_slice(&sig_alg);
    body.extend_from_slice(subject);
    body.extend_from_slice(&der(SEQ, &[der_time(start), der_time(end)].concat()));
    body.extend_from_slice(subject);
    body.extend_from_slice(&spki);
    if let Some((n, at)) = ext.filter(|e| e.0 > 0) {
        body.extend_from_slice(&der(0xa3, &extensions(n, at)?));
    }
    let tbs = der(SEQ, &body);
    let signature = if flags & CERT_CREATE_SELFSIGN_NO_SIGN != 0 {
        Vec::new()
    } else {
        let digest = crate::bcrypt::hash_named(hash, &tbs).ok_or(NTE_BAD_ALGID)?;
        let s = crate::ncrypt::sign_hash(key, hash, &digest)?;
        if f.rsa { s } else { ecdsa_der(&s) }
    };
    let cert = der(SEQ, &[tbs, sig_alg, der(0x03, &[&[0u8], &signature[..]].concat())].concat());
    Ok((cert, f))
}

#[allow(clippy::too_many_arguments)]
extern "win64" fn CertCreateSelfSignCertificate(
    key: u64,
    subject: u64,
    flags: u32,
    prov_info: u64,
    sig_alg: u64,
    start: *const [u16; 8],
    end: *const [u16; 8],
    ext: u64,
) -> u64 {
    if subject == 0 {
        return fail(E_INVALIDARG, 0);
    }
    // A key from the provider information when no handle is given.
    let (key, opened) = if key != 0 {
        if !crate::ncrypt::is_key(key) {
            return fail(crate::ncrypt::NTE_INVALID_HANDLE, 0);
        }
        (key, false)
    } else {
        let i = (prov_info != 0).then(|| rd_prov_info(prov_info));
        let Some(name) = i.as_ref().and_then(|i| i.container.clone()) else { return fail(crate::ncrypt::NTE_INVALID_HANDLE, 0) };
        match crate::ncrypt::open_named(&name, i.is_some_and(|i| i.flags & CRYPT_MACHINE_KEYSET != 0)) {
            Ok(h) => (h, true),
            Err(e) => return fail(e, 0),
        }
    };
    let time = |p: *const [u16; 8]| {
        let mut ft = 0u64;
        (!p.is_null() && crate::sysext::SystemTimeToFileTime(p, &mut ft) != 0).then_some(ft)
    };
    let sig_oid = (sig_alg != 0).then(|| rd_cstr(rd64(sig_alg, 0))).flatten();
    let ext = (ext != 0).then(|| (rd32(ext, 0), rd64(ext, 8)));
    let made = self_signed(key, rd_blob(subject), flags, sig_oid, time(start), time(end), ext);
    let r = made.and_then(|(cert, f)| {
        let ctx = make_context(0, 0, &cert)?;
        if flags & CERT_CREATE_SELFSIGN_NO_KEY_INFO == 0 {
            let info = match (prov_info != 0).then(|| rd_prov_info(prov_info)) {
                Some(i) => i,
                None => ProvInfo {
                    container: f.name.clone(),
                    provider: Some(String::from(crate::ncrypt::KSP_NAME)),
                    prov_type: 0,
                    flags: if f.machine { CRYPT_MACHINE_KEYSET } else { 0 },
                    params: Vec::new(),
                    key_spec: f.rsa as u32, // AT_KEYEXCHANGE for RSA, 0 for curves
                },
            };
            set_prop(ctx, CERT_KEY_PROV_INFO_PROP_ID, Some(Prop::ProvInfo(info)));
        }
        Ok(ctx)
    });
    if opened {
        crate::ncrypt::free(key);
    }
    r.unwrap_or_else(|e| fail(e, 0))
}

pub(super) fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        CertStrToNameA,
        CertStrToNameW,
        CryptEncodeObject,
        CryptEncodeObjectEx,
        CryptStringToBinaryA,
        CryptStringToBinaryW,
        CryptBinaryToStringA,
        CryptHashCertificate,
        CryptHashCertificate2,
        CertSetCertificateContextProperty,
        CertGetCertificateContextProperty,
        CertCreateSelfSignCertificate,
    ]
}
