//! `ncrypt.dll`: the Microsoft Software Key Storage Provider.
//!
//! Keys are RSA (512 to 16384 bits) and the NIST curves P-256 and P-384
//! for ECDSA and ECDH, made with RustCrypto: generated, imported and
//! exported (BCrypt key blobs and PKCS#8) and used to sign hashes. Named
//! keys persist across runs, one file per key under
//! %APPDATA%\Microsoft\Crypto\Keys (machine keys under
//! C:\ProgramData\Microsoft\Crypto\Keys); unnamed keys live as long as
//! their handle. crypt32 uses the same keys for self-signed certificates
//! and CryptAcquireCertificatePrivateKey.
#![allow(non_snake_case)]

use alloc::{boxed::Box, collections::BTreeMap, string::String, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering};
use os_std::sync::Mutex;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use rsa::{
    BigUint, RsaPrivateKey, RsaPublicKey,
    traits::{PrivateKeyParts, PublicKeyParts},
};

type SecStatus = u32;
const NTE_BAD_DATA: SecStatus = 0x8009_0005;
const NTE_BAD_KEY_STATE: SecStatus = 0x8009_000B;
const NTE_EXISTS: SecStatus = 0x8009_000F;
const NTE_BAD_KEYSET: SecStatus = 0x8009_0016;
pub(crate) const NTE_INVALID_HANDLE: SecStatus = 0x8009_0026;
const NTE_INVALID_PARAMETER: SecStatus = 0x8009_0027;
const NTE_BUFFER_TOO_SMALL: SecStatus = 0x8009_0028;
const NTE_NOT_SUPPORTED: SecStatus = 0x8009_0029;
const NTE_FAIL: SecStatus = 0x8009_0020;
/// An unknown provider name: Windows answers with this NTSTATUS.
const STATUS_NOT_FOUND: SecStatus = 0xC000_0225;

const NCRYPT_ALLOW_PLAINTEXT_EXPORT_FLAG: u32 = 0x2;
const NCRYPT_MACHINE_KEY_FLAG: u32 = 0x20;
const NCRYPT_OVERWRITE_KEY_FLAG: u32 = 0x80;
const NCRYPT_DO_NOT_FINALIZE_FLAG: u32 = 0x400;
const NCRYPTBUFFER_PKCS_KEY_NAME: u32 = 45;
const BCRYPT_PAD_PKCS1: u32 = 0x2;
const BCRYPT_PAD_PSS: u32 = 0x8;

pub(crate) const KSP_NAME: &str = "Microsoft Software Key Storage Provider";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Algo {
    Rsa,
    EcdsaP256,
    EcdsaP384,
    EcdhP256,
    EcdhP384,
}

impl Algo {
    const ALL: [Algo; 5] = [Algo::Rsa, Algo::EcdsaP256, Algo::EcdsaP384, Algo::EcdhP256, Algo::EcdhP384];

    fn name(self) -> &'static str {
        match self {
            Algo::Rsa => "RSA",
            Algo::EcdsaP256 => "ECDSA_P256",
            Algo::EcdsaP384 => "ECDSA_P384",
            Algo::EcdhP256 => "ECDH_P256",
            Algo::EcdhP384 => "ECDH_P384",
        }
    }

    fn from_name(s: &str) -> Option<Algo> {
        Algo::ALL.into_iter().find(|a| a.name().eq_ignore_ascii_case(s))
    }

    fn group(self) -> &'static str {
        match self {
            Algo::Rsa => "RSA",
            Algo::EcdsaP256 | Algo::EcdsaP384 => "ECDSA",
            Algo::EcdhP256 | Algo::EcdhP384 => "ECDH",
        }
    }

    fn p384(self) -> bool {
        matches!(self, Algo::EcdsaP384 | Algo::EcdhP384)
    }

    /// The fixed size of curve keys, in bits.
    fn curve_bits(self) -> u32 {
        if self.p384() { 384 } else { 256 }
    }

    /// The four BCRYPT_ECCKEY_BLOB magics: (public, private).
    fn ecc_magic(self) -> (u32, u32) {
        match self {
            Algo::EcdsaP256 => (0x3153_4345, 0x3253_4345),
            Algo::EcdsaP384 => (0x3353_4345, 0x3453_4345),
            Algo::EcdhP256 => (0x314B_4345, 0x324B_4345),
            Algo::EcdhP384 => (0x334B_4345, 0x344B_4345),
            Algo::Rsa => (0, 0),
        }
    }
}

#[derive(Clone)]
enum Material {
    Rsa(Box<RsaPrivateKey>),
    RsaPublic(Box<RsaPublicKey>),
    P256(p256::SecretKey),
    P256Public(p256::PublicKey),
    P384(p384::SecretKey),
    P384Public(p384::PublicKey),
}

impl Material {
    fn private(&self) -> bool {
        matches!(self, Material::Rsa(_) | Material::P256(_) | Material::P384(_))
    }
}

#[derive(Clone)]
struct Key {
    algo: Algo,
    name: Option<String>,
    machine: bool,
    /// RSA modulus size in bits (curve keys have a fixed size).
    length: u32,
    export_policy: u32,
    key_usage: u32,
    material: Option<Material>,
    /// NCryptFinalizeKey ran (or the key was opened or imported finished):
    /// only then is it usable and persisted.
    finalized: bool,
    /// Properties set by the caller that the provider does not interpret.
    props: BTreeMap<String, Vec<u8>>,
}

impl Key {
    /// The material of a finished key.
    fn ready(&self) -> Result<&Material, SecStatus> {
        self.material.as_ref().filter(|_| self.finalized).ok_or(NTE_BAD_KEY_STATE)
    }
}

enum Object {
    Provider,
    Key(Box<Key>),
}

static OBJECTS: Mutex<BTreeMap<u64, Object>> = Mutex::new(BTreeMap::new());
static NEXT: AtomicU64 = AtomicU64::new(0x6e00_0010);

fn insert(o: Object) -> u64 {
    let h = NEXT.fetch_add(0x10, Ordering::Relaxed);
    OBJECTS.lock().unwrap().insert(h, o);
    h
}

fn with_key<T>(h: u64, f: impl FnOnce(&mut Key) -> Result<T, SecStatus>) -> Result<T, SecStatus> {
    match OBJECTS.lock().unwrap().get_mut(&h) {
        Some(Object::Key(k)) => f(k),
        _ => Err(NTE_INVALID_HANDLE),
    }
}

fn is_provider(h: u64) -> bool {
    matches!(OBJECTS.lock().unwrap().get(&h), Some(Object::Provider))
}

fn status(r: Result<(), SecStatus>) -> SecStatus {
    r.err().unwrap_or(0)
}

/// The kernel's CSPRNG for key generation, PSS salts and serial numbers.
pub(crate) struct Rng;

impl rand_core::RngCore for Rng {
    fn next_u32(&mut self) -> u32 {
        let mut b = [0; 4];
        self.fill_bytes(&mut b);
        u32::from_le_bytes(b)
    }
    fn next_u64(&mut self) -> u64 {
        let mut b = [0; 8];
        self.fill_bytes(&mut b);
        u64::from_le_bytes(b)
    }
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        assert!(crate::ucrt::entropy(dest), "ncrypt: no entropy source");
    }
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        if crate::ucrt::entropy(dest) {
            Ok(())
        } else {
            Err(rand_core::Error::from(core::num::NonZeroU32::new(rand_core::Error::CUSTOM_START).unwrap()))
        }
    }
}

impl rand_core::CryptoRng for Rng {}

/// Reads a NUL-terminated UTF-16 argument.
fn wide(p: *const u16) -> Option<String> {
    // SAFETY: NULL or the caller's NUL-terminated string.
    unsafe { crate::path::wide_to_string(p) }
}

fn wide_bytes(s: &str) -> Vec<u8> {
    s.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect()
}

/// Copies `value` to an output buffer the CNG way: a NULL buffer asks for
/// the size, a short one fails with NTE_BUFFER_TOO_SMALL.
fn output(value: &[u8], out: *mut u8, cap: u32, result: *mut u32) -> SecStatus {
    if result.is_null() {
        return NTE_INVALID_PARAMETER;
    }
    // SAFETY: the caller's size pointer and buffer of `cap` bytes.
    unsafe {
        *result = value.len() as u32;
        if out.is_null() {
            return 0;
        }
        if (cap as usize) < value.len() {
            return NTE_BUFFER_TOO_SMALL;
        }
        core::ptr::copy_nonoverlapping(value.as_ptr(), out, value.len());
    }
    0
}

fn input<'a>(p: *const u8, len: u32) -> &'a [u8] {
    if p.is_null() || len == 0 {
        return &[];
    }
    // SAFETY: the caller's `len` bytes.
    unsafe { core::slice::from_raw_parts(p, len as usize) }
}

// ---- persistence -----------------------------------------------------------------------------

fn key_dir(machine: bool) -> String {
    if machine {
        crate::path::to_osyx("C:\\ProgramData\\Microsoft\\Crypto\\Keys", "C:\\").unwrap_or_else(|_| String::from("/ProgramData/Microsoft/Crypto/Keys"))
    } else {
        let home = os_std::env::var("HOME").unwrap_or_else(|_| String::from("/home/user"));
        alloc::format!("{}/AppData/Roaming/Microsoft/Crypto/Keys", home.trim_end_matches('/'))
    }
}

/// Key names are case-insensitive; the file is named by the hex of the
/// lowercased name so any name is a valid file name.
fn key_file(name: &str, machine: bool) -> String {
    let hex: String = name.to_lowercase().bytes().map(|b| alloc::format!("{b:02x}")).collect();
    alloc::format!("{}/{hex}.key", key_dir(machine))
}

const FILE_MAGIC: &[u8; 8] = b"OSYXKEY1";

fn pkcs8(m: &Material) -> Result<Vec<u8>, SecStatus> {
    use rsa::pkcs8::EncodePrivateKey;
    let doc = match m {
        Material::Rsa(k) => k.to_pkcs8_der(),
        Material::P256(k) => k.to_pkcs8_der(),
        Material::P384(k) => k.to_pkcs8_der(),
        _ => return Err(NTE_NOT_SUPPORTED),
    };
    doc.map(|d| d.as_bytes().to_vec()).map_err(|_| NTE_FAIL)
}

fn from_pkcs8(der: &[u8]) -> Result<(Algo, Material), SecStatus> {
    use rsa::pkcs8::DecodePrivateKey;
    if let Ok(k) = RsaPrivateKey::from_pkcs8_der(der) {
        return Ok((Algo::Rsa, Material::Rsa(Box::new(k))));
    }
    if let Ok(k) = p256::SecretKey::from_pkcs8_der(der) {
        return Ok((Algo::EcdsaP256, Material::P256(k)));
    }
    if let Ok(k) = p384::SecretKey::from_pkcs8_der(der) {
        return Ok((Algo::EcdsaP384, Material::P384(k)));
    }
    Err(NTE_BAD_DATA)
}

fn save(k: &Key) -> Result<(), SecStatus> {
    let Some(name) = &k.name else { return Ok(()) };
    let m = k.ready()?;
    let mut data = FILE_MAGIC.to_vec();
    data.push(Algo::ALL.iter().position(|&a| a == k.algo).unwrap_or(0) as u8);
    for v in [k.export_policy, k.key_usage, k.length] {
        data.extend_from_slice(&v.to_le_bytes());
    }
    data.extend_from_slice(&pkcs8(m)?);
    let _ = os_std::fs::create_dir_all(key_dir(k.machine));
    os_std::fs::write(key_file(name, k.machine), data).map_err(|_| NTE_FAIL)
}

fn load(name: &str, machine: bool) -> Result<Key, SecStatus> {
    let data = os_std::fs::read(key_file(name, machine)).map_err(|_| NTE_BAD_KEYSET)?;
    if data.len() < 21 || &data[..8] != FILE_MAGIC {
        return Err(NTE_BAD_KEYSET);
    }
    let algo = *Algo::ALL.get(data[8] as usize).ok_or(NTE_BAD_KEYSET)?;
    let word = |i: usize| u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]);
    let (_, m) = from_pkcs8(&data[21..])?;
    Ok(Key {
        algo,
        name: Some(String::from(name)),
        machine,
        export_policy: word(9),
        key_usage: word(13),
        length: word(17),
        material: Some(m),
        finalized: true,
        props: BTreeMap::new(),
    })
}

// ---- key material -------------------------------------------------------------------------------

fn generate(algo: Algo, length: u32) -> Result<Material, SecStatus> {
    Ok(match algo {
        Algo::Rsa => Material::Rsa(Box::new(RsaPrivateKey::new(&mut Rng, length as usize).map_err(|_| NTE_FAIL)?)),
        Algo::EcdsaP256 | Algo::EcdhP256 => Material::P256(p256::SecretKey::random(&mut Rng)),
        Algo::EcdsaP384 | Algo::EcdhP384 => Material::P384(p384::SecretKey::random(&mut Rng)),
    })
}

/// `n` big-endian, left-padded to `len` bytes.
fn be(n: &BigUint, len: usize) -> Vec<u8> {
    let b = n.to_bytes_be();
    let mut v = alloc::vec![0; len.saturating_sub(b.len())];
    v.extend_from_slice(&b);
    v
}

/// The uncompressed public point X || Y.
fn point(m: &Material) -> Option<Vec<u8>> {
    let p = match m {
        Material::P256(k) => k.public_key().to_encoded_point(false).as_bytes().to_vec(),
        Material::P256Public(k) => k.to_encoded_point(false).as_bytes().to_vec(),
        Material::P384(k) => k.public_key().to_encoded_point(false).as_bytes().to_vec(),
        Material::P384Public(k) => k.to_encoded_point(false).as_bytes().to_vec(),
        _ => return None,
    };
    Some(p[1..].to_vec())
}

fn rsa_public(m: &Material) -> Option<RsaPublicKey> {
    match m {
        Material::Rsa(k) => Some(k.to_public_key()),
        Material::RsaPublic(k) => Some((**k).clone()),
        _ => None,
    }
}

fn rsa_header(magic: u32, n: &RsaPublicKey, prime: usize) -> (Vec<u8>, usize) {
    let e = n.e().to_bytes_be();
    let modulus = n.size();
    let mut v = Vec::new();
    for x in [magic, n.n().bits() as u32, e.len() as u32, modulus as u32, prime as u32, prime as u32] {
        v.extend_from_slice(&x.to_le_bytes());
    }
    v.extend_from_slice(&e);
    v.extend_from_slice(&be(n.n(), modulus));
    (v, modulus)
}

fn export(k: &Key, kind: &str) -> Result<Vec<u8>, SecStatus> {
    let m = k.ready()?;
    let rsa = k.algo == Algo::Rsa;
    let kind = match kind {
        "PUBLICBLOB" if rsa => "RSAPUBLICBLOB",
        "PUBLICBLOB" => "ECCPUBLICBLOB",
        "PRIVATEBLOB" if rsa => "RSAPRIVATEBLOB",
        "PRIVATEBLOB" => "ECCPRIVATEBLOB",
        other => other,
    };
    let private = matches!(kind, "RSAPRIVATEBLOB" | "RSAFULLPRIVATEBLOB" | "ECCPRIVATEBLOB" | "PKCS8_PRIVATEKEY");
    if private && (!m.private() || k.export_policy & NCRYPT_ALLOW_PLAINTEXT_EXPORT_FLAG == 0) {
        return Err(NTE_NOT_SUPPORTED);
    }
    match kind {
        "RSAPUBLICBLOB" if rsa => Ok(rsa_header(0x3141_5352, &rsa_public(m).ok_or(NTE_BAD_KEY_STATE)?, 0).0),
        "RSAPRIVATEBLOB" | "RSAFULLPRIVATEBLOB" if rsa => {
            let Material::Rsa(key) = m else { return Err(NTE_NOT_SUPPORTED) };
            let full = kind == "RSAFULLPRIVATEBLOB";
            let (p, q) = (&key.primes()[0], &key.primes()[1]);
            let prime = key.size().div_ceil(2);
            let (mut v, modulus) = rsa_header(if full { 0x3341_5352 } else { 0x3241_5352 }, &key.to_public_key(), prime);
            v.extend_from_slice(&be(p, prime));
            v.extend_from_slice(&be(q, prime));
            if full {
                let one = BigUint::from(1u32);
                let d = key.d();
                v.extend_from_slice(&be(&(d % (p - &one)), prime));
                v.extend_from_slice(&be(&(d % (q - &one)), prime));
                // q^-1 mod p by Fermat: p is prime.
                v.extend_from_slice(&be(&q.modpow(&(p - BigUint::from(2u32)), p), prime));
                v.extend_from_slice(&be(d, modulus));
            }
            Ok(v)
        }
        "ECCPUBLICBLOB" | "ECCPRIVATEBLOB" if !rsa => {
            let (public, secret) = k.algo.ecc_magic();
            let xy = point(m).ok_or(NTE_BAD_KEY_STATE)?;
            let size = xy.len() / 2;
            let mut v = Vec::new();
            v.extend_from_slice(&(if private { secret } else { public }).to_le_bytes());
            v.extend_from_slice(&(size as u32).to_le_bytes());
            v.extend_from_slice(&xy);
            if private {
                match m {
                    Material::P256(s) => v.extend_from_slice(&s.to_bytes()),
                    Material::P384(s) => v.extend_from_slice(&s.to_bytes()),
                    _ => return Err(NTE_NOT_SUPPORTED),
                }
            }
            Ok(v)
        }
        "PKCS8_PRIVATEKEY" => pkcs8(m),
        _ => Err(NTE_INVALID_PARAMETER),
    }
}

fn word(d: &[u8], i: usize) -> Option<u32> {
    d.get(i..i + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// A key from a blob: (algorithm, material).
fn import(kind: &str, d: &[u8]) -> Result<(Algo, Material), SecStatus> {
    if d.is_empty() {
        return Err(NTE_INVALID_PARAMETER);
    }
    if kind == "PKCS8_PRIVATEKEY" {
        return from_pkcs8(d);
    }
    let magic = word(d, 0).ok_or(NTE_BAD_DATA)?;
    if matches!(kind, "RSAPUBLICBLOB" | "RSAPRIVATEBLOB" | "RSAFULLPRIVATEBLOB" | "PUBLICBLOB" | "PRIVATEBLOB")
        && matches!(magic, 0x3141_5352 | 0x3241_5352 | 0x3341_5352)
    {
        let f = |i| word(d, i).map(|v| v as usize).ok_or(NTE_BAD_DATA);
        let (e_len, n_len, p_len, q_len) = (f(8)?, f(12)?, f(16)?, f(20)?);
        let mut at = 24;
        let mut take = |n: usize| -> Result<BigUint, SecStatus> {
            let s = d.get(at..at + n).ok_or(NTE_BAD_DATA)?;
            at += n;
            Ok(BigUint::from_bytes_be(s))
        };
        let (e, n) = (take(e_len)?, take(n_len)?);
        if magic == 0x3141_5352 {
            let k = RsaPublicKey::new(n, e).map_err(|_| NTE_BAD_DATA)?;
            return Ok((Algo::Rsa, Material::RsaPublic(Box::new(k))));
        }
        let (p, q) = (take(p_len)?, take(q_len)?);
        let k = RsaPrivateKey::from_p_q(p, q, e).map_err(|_| NTE_BAD_DATA)?;
        if *k.n() != n {
            return Err(NTE_BAD_DATA);
        }
        return Ok((Algo::Rsa, Material::Rsa(Box::new(k))));
    }
    if matches!(kind, "ECCPUBLICBLOB" | "ECCPRIVATEBLOB" | "PUBLICBLOB" | "PRIVATEBLOB") {
        let (algo, private) = Algo::ALL
            .into_iter()
            .filter(|a| *a != Algo::Rsa)
            .find_map(|a| {
                let (p, s) = a.ecc_magic();
                (magic == p || magic == s).then_some((a, magic == s))
            })
            .ok_or(NTE_BAD_DATA)?;
        let size = word(d, 4).ok_or(NTE_BAD_DATA)? as usize;
        if size * 8 != algo.curve_bits() as usize {
            return Err(NTE_BAD_DATA);
        }
        let xy = d.get(8..8 + 2 * size).ok_or(NTE_BAD_DATA)?;
        let mut sec1 = alloc::vec![4u8];
        sec1.extend_from_slice(xy);
        let m = match (algo.p384(), private) {
            (false, false) => Material::P256Public(p256::PublicKey::from_sec1_bytes(&sec1).map_err(|_| NTE_BAD_DATA)?),
            (true, false) => Material::P384Public(p384::PublicKey::from_sec1_bytes(&sec1).map_err(|_| NTE_BAD_DATA)?),
            (false, true) => Material::P256(p256::SecretKey::from_slice(d.get(8 + 2 * size..8 + 3 * size).ok_or(NTE_BAD_DATA)?).map_err(|_| NTE_BAD_DATA)?),
            (true, true) => Material::P384(p384::SecretKey::from_slice(d.get(8 + 2 * size..8 + 3 * size).ok_or(NTE_BAD_DATA)?).map_err(|_| NTE_BAD_DATA)?),
        };
        // A private blob's point must be the secret's.
        if private && point(&m).as_deref() != Some(xy) {
            return Err(NTE_BAD_DATA);
        }
        return Ok((algo, m));
    }
    Err(NTE_INVALID_PARAMETER)
}

/// DigestInfo prefixes for PKCS #1 v1.5 signatures.
fn digest_info(alg: &str) -> Option<&'static [u8]> {
    Some(match alg.to_ascii_uppercase().as_str() {
        "MD5" => &[0x30, 0x20, 0x30, 0x0c, 0x06, 0x08, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x02, 0x05, 0x05, 0x00, 0x04, 0x10],
        "SHA1" => &[0x30, 0x21, 0x30, 0x09, 0x06, 0x05, 0x2b, 0x0e, 0x03, 0x02, 0x1a, 0x05, 0x00, 0x04, 0x14],
        "SHA256" => &[0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01, 0x05, 0x00, 0x04, 0x20],
        "SHA384" => &[0x30, 0x41, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x02, 0x05, 0x00, 0x04, 0x30],
        "SHA512" => &[0x30, 0x51, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x03, 0x05, 0x00, 0x04, 0x40],
        _ => return None,
    })
}

enum Padding<'a> {
    /// PKCS #1 v1.5 with the DigestInfo of the named hash (None: raw).
    Pkcs1(Option<&'a str>),
    Pss(&'a str, usize),
    /// ECDSA.
    None,
}

/// The signature of a hash: RSA as the modulus-sized integer, ECDSA as
/// r || s (IEEE P1363), as NCryptSignHash returns them.
fn sign(m: &Material, pad: Padding<'_>, hash: &[u8]) -> Result<Vec<u8>, SecStatus> {
    use p256::ecdsa::signature::hazmat::PrehashSigner;
    match (m, pad) {
        (Material::Rsa(k), Padding::Pkcs1(alg)) => {
            let scheme = match alg {
                None => rsa::Pkcs1v15Sign::new_unprefixed(),
                Some(a) => {
                    let prefix = digest_info(a).ok_or(NTE_NOT_SUPPORTED)?;
                    rsa::Pkcs1v15Sign { hash_len: Some(prefix[prefix.len() - 1] as usize), prefix: Box::from(prefix) }
                }
            };
            k.sign(scheme, hash).map_err(|_| NTE_INVALID_PARAMETER)
        }
        (Material::Rsa(k), Padding::Pss(alg, salt)) => {
            let r = match alg.to_ascii_uppercase().as_str() {
                "SHA1" => k.sign_with_rng(&mut Rng, rsa::Pss::new_with_salt::<sha1::Sha1>(salt), hash),
                "SHA256" => k.sign_with_rng(&mut Rng, rsa::Pss::new_with_salt::<sha2::Sha256>(salt), hash),
                "SHA384" => k.sign_with_rng(&mut Rng, rsa::Pss::new_with_salt::<sha2::Sha384>(salt), hash),
                "SHA512" => k.sign_with_rng(&mut Rng, rsa::Pss::new_with_salt::<sha2::Sha512>(salt), hash),
                _ => return Err(NTE_NOT_SUPPORTED),
            };
            r.map_err(|_| NTE_INVALID_PARAMETER)
        }
        (Material::P256(k), Padding::None) => {
            let s: p256::ecdsa::Signature = p256::ecdsa::SigningKey::from(k).sign_prehash(hash).map_err(|_| NTE_INVALID_PARAMETER)?;
            Ok(s.to_bytes().to_vec())
        }
        (Material::P384(k), Padding::None) => {
            let s: p384::ecdsa::Signature = p384::ecdsa::SigningKey::from(k).sign_prehash(hash).map_err(|_| NTE_INVALID_PARAMETER)?;
            Ok(s.to_bytes().to_vec())
        }
        (Material::RsaPublic(_) | Material::P256Public(_) | Material::P384Public(_), _) => Err(NTE_NOT_SUPPORTED),
        _ => Err(NTE_INVALID_PARAMETER),
    }
}

fn signature_len(m: &Material) -> usize {
    match m {
        Material::Rsa(k) => k.size(),
        Material::RsaPublic(k) => k.size(),
        Material::P256(_) | Material::P256Public(_) => 64,
        Material::P384(_) | Material::P384Public(_) => 96,
    }
}

// ---- what crypt32 uses ---------------------------------------------------------------------

/// A key's identity and public half, for certificates.
pub(crate) struct Facts {
    pub name: Option<String>,
    pub machine: bool,
    pub rsa: bool,
    /// SubjectPublicKeyInfo: algorithm OID, its parameters' encoding and
    /// the subjectPublicKey bits.
    pub alg_oid: &'static str,
    pub alg_params: Vec<u8>,
    pub public: Vec<u8>,
}

pub(crate) fn facts(h: u64) -> Result<Facts, SecStatus> {
    with_key(h, |k| {
        let m = k.ready()?;
        let (alg_oid, alg_params, public) = match rsa_public(m) {
            Some(p) => {
                // RSAPublicKey ::= SEQUENCE { modulus, publicExponent }
                let n = crate::crypt32::der_uint(&p.n().to_bytes_be());
                let e = crate::crypt32::der_uint(&p.e().to_bytes_be());
                ("1.2.840.113549.1.1.1", alloc::vec![0x05, 0x00], crate::crypt32::der(0x30, &[n, e].concat()))
            }
            None => {
                let curve = if k.algo.p384() { "1.3.132.0.34" } else { "1.2.840.10045.3.1.7" };
                let mut p = alloc::vec![4u8];
                p.extend_from_slice(&point(m).ok_or(NTE_BAD_KEY_STATE)?);
                ("1.2.840.10045.2.1", crate::crypt32::der_oid(curve).unwrap_or_default(), p)
            }
        };
        Ok(Facts { name: k.name.clone(), machine: k.machine, rsa: k.algo == Algo::Rsa, alg_oid, alg_params, public })
    })
}

/// Signs `hash` (made with the named algorithm) for a certificate: RSA
/// PKCS #1 v1.5, ECDSA as r || s.
pub(crate) fn sign_hash(h: u64, hash_alg: &str, hash: &[u8]) -> Result<Vec<u8>, SecStatus> {
    with_key(h, |k| {
        let m = k.ready()?;
        sign(m, if k.algo == Algo::Rsa { Padding::Pkcs1(Some(hash_alg)) } else { Padding::None }, hash)
    })
}

pub(crate) fn open_named(name: &str, machine: bool) -> Result<u64, SecStatus> {
    load(name, machine).map(|k| insert(Object::Key(Box::new(k))))
}

pub(crate) fn free(h: u64) {
    OBJECTS.lock().unwrap().remove(&h);
}

pub(crate) fn is_key(h: u64) -> bool {
    matches!(OBJECTS.lock().unwrap().get(&h), Some(Object::Key(_)))
}

// ---- the API ---------------------------------------------------------------------------------

extern "win64" fn NCryptOpenStorageProvider(out: *mut u64, name: *const u16, _flags: u32) -> SecStatus {
    if out.is_null() {
        return NTE_INVALID_PARAMETER;
    }
    // The smart-card and TPM providers need hardware OSyx does not have.
    if wide(name).is_some_and(|n| !n.eq_ignore_ascii_case(KSP_NAME)) {
        return STATUS_NOT_FOUND;
    }
    // SAFETY: the caller's out pointer.
    unsafe { *out = insert(Object::Provider) };
    0
}

/// NCryptIsAlgSupported: whether the provider makes keys of the algorithm
/// (RSA, ECDSA and ECDH on P-256 and P-384); NTE_NOT_SUPPORTED otherwise.
extern "win64" fn NCryptIsAlgSupported(provider: u64, alg: *const u16, _flags: u32) -> SecStatus {
    if !is_provider(provider) {
        return NTE_INVALID_HANDLE;
    }
    match wide(alg) {
        None => NTE_INVALID_PARAMETER,
        Some(a) if Algo::from_name(&a).is_some() => 0,
        Some(_) => NTE_NOT_SUPPORTED,
    }
}

extern "win64" fn NCryptCreatePersistedKey(provider: u64, out: *mut u64, alg: *const u16, name: *const u16, _spec: u32, flags: u32) -> SecStatus {
    if !is_provider(provider) {
        return NTE_INVALID_HANDLE;
    }
    if out.is_null() {
        return NTE_INVALID_PARAMETER;
    }
    let Some(algo) = wide(alg).and_then(|a| Algo::from_name(&a)) else { return NTE_NOT_SUPPORTED };
    let name = wide(name).filter(|n| !n.is_empty());
    let machine = flags & NCRYPT_MACHINE_KEY_FLAG != 0;
    if flags & NCRYPT_OVERWRITE_KEY_FLAG == 0 && name.as_ref().is_some_and(|n| os_std::fs::metadata(key_file(n, machine)).is_ok()) {
        return NTE_EXISTS;
    }
    let length = if algo == Algo::Rsa { 1024 } else { algo.curve_bits() };
    let key = Key { algo, name, machine, length, export_policy: 0, key_usage: 0, material: None, finalized: false, props: BTreeMap::new() };
    // SAFETY: the caller's out pointer.
    unsafe { *out = insert(Object::Key(Box::new(key))) };
    0
}

extern "win64" fn NCryptOpenKey(provider: u64, out: *mut u64, name: *const u16, _spec: u32, flags: u32) -> SecStatus {
    if !is_provider(provider) {
        return NTE_INVALID_HANDLE;
    }
    let Some(name) = wide(name) else { return NTE_INVALID_PARAMETER };
    if out.is_null() {
        return NTE_INVALID_PARAMETER;
    }
    match open_named(&name, flags & NCRYPT_MACHINE_KEY_FLAG != 0) {
        // SAFETY: the caller's out pointer.
        Ok(h) => unsafe {
            *out = h;
            0
        },
        Err(e) => e,
    }
}

extern "win64" fn NCryptFinalizeKey(key: u64, _flags: u32) -> SecStatus {
    status(with_key(key, |k| {
        if k.finalized {
            return Err(NTE_BAD_KEY_STATE);
        }
        // An imported key keeps its material; a created one gets new.
        if k.material.is_none() {
            k.material = Some(generate(k.algo, k.length)?);
        }
        k.finalized = true;
        save(k)
    }))
}

extern "win64" fn NCryptDeleteKey(key: u64, _flags: u32) -> SecStatus {
    let Some(Object::Key(k)) = OBJECTS.lock().unwrap().remove(&key) else { return NTE_INVALID_HANDLE };
    if let Some(n) = &k.name {
        let _ = os_std::fs::remove_file(key_file(n, k.machine));
    }
    0
}

extern "win64" fn NCryptFreeObject(h: u64) -> SecStatus {
    if OBJECTS.lock().unwrap().remove(&h).is_some() { 0 } else { NTE_INVALID_HANDLE }
}

fn u32_value(d: &[u8]) -> Result<u32, SecStatus> {
    word(d, 0).filter(|_| d.len() == 4).ok_or(NTE_INVALID_PARAMETER)
}

extern "win64" fn NCryptSetProperty(h: u64, name: *const u16, value: *const u8, len: u32, _flags: u32) -> SecStatus {
    let Some(name) = wide(name) else { return NTE_INVALID_PARAMETER };
    let d = input(value, len);
    if is_provider(h) {
        return NTE_NOT_SUPPORTED;
    }
    status(with_key(h, |k| {
        match name.as_str() {
            "Length" => {
                let bits = u32_value(d)?;
                if k.material.is_some() {
                    return Err(NTE_BAD_KEY_STATE);
                }
                if k.algo != Algo::Rsa {
                    return if bits == k.algo.curve_bits() { Ok(()) } else { Err(NTE_NOT_SUPPORTED) };
                }
                if !(512..=16384).contains(&bits) || bits % 64 != 0 {
                    return Err(NTE_NOT_SUPPORTED);
                }
                k.length = bits;
            }
            "Export Policy" => k.export_policy = u32_value(d)?,
            "Key Usage" => k.key_usage = u32_value(d)?,
            "Algorithm Name" | "Algorithm Group" | "Name" | "Unique Name" | "Key Type" | "Lengths" => return Err(NTE_NOT_SUPPORTED),
            _ => {
                k.props.insert(name.clone(), d.to_vec());
            }
        }
        // A persisted key keeps its policy and usage.
        if k.finalized && matches!(name.as_str(), "Export Policy" | "Key Usage") {
            save(k)?;
        }
        Ok(())
    }))
}

extern "win64" fn NCryptGetProperty(h: u64, name: *const u16, out: *mut u8, cap: u32, result: *mut u32, _flags: u32) -> SecStatus {
    let Some(name) = wide(name) else { return NTE_INVALID_PARAMETER };
    if is_provider(h) {
        let v = match name.as_str() {
            "Name" => wide_bytes(KSP_NAME),
            "Impl Type" => 2u32.to_le_bytes().to_vec(), // NCRYPT_IMPL_SOFTWARE_FLAG
            "Version" => 0x0001_0000u32.to_le_bytes().to_vec(),
            _ => return NTE_NOT_SUPPORTED,
        };
        return output(&v, out, cap, result);
    }
    let v = with_key(h, |k| {
        let u = |v: u32| v.to_le_bytes().to_vec();
        Ok(match name.as_str() {
            "Length" => u(match k.material.as_ref().map(rsa_public) {
                Some(Some(p)) => p.n().bits() as u32,
                _ => k.length,
            }),
            "Export Policy" => u(k.export_policy),
            "Key Usage" => u(k.key_usage),
            "Algorithm Name" => wide_bytes(k.algo.name()),
            "Algorithm Group" => wide_bytes(k.algo.group()),
            "Name" | "Unique Name" => wide_bytes(k.name.as_deref().ok_or(NTE_NOT_SUPPORTED)?),
            "Key Type" => u(if k.machine { NCRYPT_MACHINE_KEY_FLAG } else { 0 }),
            "Lengths" => {
                let (min, max, inc, def) = if k.algo == Algo::Rsa { (512, 16384, 64, 1024) } else { let b = k.algo.curve_bits(); (b, b, 0, b) };
                [min, max, inc, def].iter().flat_map(|v: &u32| v.to_le_bytes()).collect()
            }
            "ECCCurveName" if k.algo != Algo::Rsa => wide_bytes(if k.algo.p384() { "nistP384" } else { "nistP256" }),
            other => k.props.get(other).cloned().ok_or(NTE_NOT_SUPPORTED)?,
        })
    });
    match v {
        Ok(v) => output(&v, out, cap, result),
        Err(e) => e,
    }
}

#[allow(clippy::too_many_arguments)]
extern "win64" fn NCryptExportKey(
    key: u64,
    wrap: u64,
    kind: *const u16,
    _params: *const u8,
    out: *mut u8,
    cap: u32,
    result: *mut u32,
    _flags: u32,
) -> SecStatus {
    let Some(kind) = wide(kind) else { return NTE_INVALID_PARAMETER };
    if wrap != 0 {
        // Key wrapping needs a symmetric export key; none is offered.
        return NTE_NOT_SUPPORTED;
    }
    match with_key(key, |k| export(k, &kind)) {
        Ok(v) => output(&v, out, cap, result),
        Err(e) => e,
    }
}

/// The key name in an NCryptBufferDesc (NCRYPTBUFFER_PKCS_KEY_NAME).
fn buffer_key_name(desc: *const u8) -> Option<String> {
    if desc.is_null() {
        return None;
    }
    // SAFETY: the caller's NCryptBufferDesc {version, count, buffers}
    // with NCryptBuffer {cbBuffer, BufferType, pvBuffer} entries.
    unsafe {
        let count = *(desc.add(4) as *const u32) as usize;
        let buffers = *(desc.add(8) as *const u64) as *const u8;
        (0..count).find_map(|i| {
            let b = buffers.add(16 * i);
            (*(b.add(4) as *const u32) == NCRYPTBUFFER_PKCS_KEY_NAME).then(|| wide(*(b.add(8) as *const u64) as *const u16)).flatten()
        })
    }
}

#[allow(clippy::too_many_arguments)]
extern "win64" fn NCryptImportKey(
    provider: u64,
    unwrap: u64,
    kind: *const u16,
    params: *const u8,
    out: *mut u64,
    data: *const u8,
    len: u32,
    flags: u32,
) -> SecStatus {
    if !is_provider(provider) {
        return NTE_INVALID_HANDLE;
    }
    let Some(kind) = wide(kind) else { return NTE_INVALID_PARAMETER };
    if out.is_null() {
        return NTE_INVALID_PARAMETER;
    }
    if unwrap != 0 {
        return NTE_NOT_SUPPORTED;
    }
    let (algo, m) = match import(&kind, input(data, len)) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let length = match rsa_public(&m) {
        Some(p) => p.n().bits() as u32,
        None => algo.curve_bits(),
    };
    let key = Key {
        algo,
        name: buffer_key_name(params),
        machine: flags & NCRYPT_MACHINE_KEY_FLAG != 0,
        length,
        export_policy: 0,
        key_usage: 0,
        material: Some(m),
        finalized: flags & NCRYPT_DO_NOT_FINALIZE_FLAG == 0,
        props: BTreeMap::new(),
    };
    if key.finalized {
        if let Err(e) = save(&key) {
            return e;
        }
    }
    // SAFETY: the caller's out pointer.
    unsafe { *out = insert(Object::Key(Box::new(key))) };
    0
}

#[allow(clippy::too_many_arguments)]
extern "win64" fn NCryptSignHash(
    key: u64,
    padding: *const u8,
    hash: *const u8,
    hash_len: u32,
    out: *mut u8,
    cap: u32,
    result: *mut u32,
    flags: u32,
) -> SecStatus {
    let r = with_key(key, |k| {
        let m = k.ready()?;
        if out.is_null() {
            return Ok(alloc::vec![0; signature_len(m)]);
        }
        // SAFETY: the caller's BCRYPT_PKCS1_PADDING_INFO {pszAlgId} or
        // BCRYPT_PSS_PADDING_INFO {pszAlgId, cbSalt}.
        let alg = (!padding.is_null()).then(|| wide(unsafe { *(padding as *const *const u16) })).flatten();
        let pad = if k.algo != Algo::Rsa {
            Padding::None
        } else if flags & BCRYPT_PAD_PSS != 0 {
            // SAFETY: as above.
            let salt = if padding.is_null() { 0 } else { (unsafe { *(padding.add(8) as *const u32) }) as usize };
            Padding::Pss(alg.as_deref().ok_or(NTE_INVALID_PARAMETER)?, salt)
        } else if flags & BCRYPT_PAD_PKCS1 != 0 {
            Padding::Pkcs1(alg.as_deref())
        } else {
            return Err(NTE_INVALID_PARAMETER);
        };
        // The PSS and PKCS #1 branches borrow `alg`; sign before it drops.
        sign(m, pad, input(hash, hash_len))
    });
    match r {
        Ok(v) => output(&v, out, cap, result),
        Err(e) => e,
    }
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        NCryptIsAlgSupported,
        NCryptOpenStorageProvider,
        NCryptCreatePersistedKey,
        NCryptOpenKey,
        NCryptFinalizeKey,
        NCryptDeleteKey,
        NCryptFreeObject,
        NCryptSetProperty,
        NCryptGetProperty,
        NCryptExportKey,
        NCryptImportKey,
        NCryptSignHash,
    ]
}
