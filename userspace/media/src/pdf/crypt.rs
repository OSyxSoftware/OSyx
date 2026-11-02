//! The standard security handler (ISO 32000-2, 7.6.4): RC4 and AES-128
//! (revisions 2 to 4) and AES-256 (revisions 5 and 6), opened with the
//! empty user password, which is how most "protected" files are made (an
//! owner password only restricts printing or copying).

use super::object::{key, Dict, Object, Ref};
use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use aes::{Aes128, Aes256};
use alloc::string::String;
use alloc::vec::Vec;
use sha2::{Digest, Sha256, Sha384, Sha512};

const PAD: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08, 0x2E, 0x2E, 0x00,
    0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Method {
    Identity,
    Rc4,
    Aes128,
    Aes256,
}

pub struct Decryptor {
    file_key: Vec<u8>,
    strings: Method,
    streams: Method,
    encrypt_metadata: bool,
}

pub fn md5(data: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14,
        20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6,
        10, 15, 21,
    ];
    let k: [u32; 64] = core::array::from_fn(|i| (libm::fabs(libm::sin((i + 1) as f64)) * 4_294_967_296.0) as u32);
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_le_bytes());
    let mut h = [0x67452301u32, 0xEFCDAB89, 0x98BADCFE, 0x10325476];
    for chunk in msg.chunks_exact(64) {
        let m: [u32; 16] = core::array::from_fn(|i| u32::from_le_bytes(chunk[4 * i..4 * i + 4].try_into().unwrap()));
        let [mut a, mut b, mut c, mut d] = h;
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let t = d;
            d = c;
            c = b;
            b = b.wrapping_add(a.wrapping_add(f).wrapping_add(k[i]).wrapping_add(m[g]).rotate_left(S[i]));
            a = t;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
    }
    let mut out = [0u8; 16];
    for (i, w) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&w.to_le_bytes());
    }
    out
}

pub fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut s: [u8; 256] = core::array::from_fn(|i| i as u8);
    let mut j = 0u8;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len().max(1)]);
        s.swap(i, j as usize);
    }
    let (mut i, mut j) = (0u8, 0u8);
    data.iter()
        .map(|&b| {
            i = i.wrapping_add(1);
            j = j.wrapping_add(s[i as usize]);
            s.swap(i as usize, j as usize);
            b ^ s[s[i as usize].wrapping_add(s[j as usize]) as usize]
        })
        .collect()
}

/// AES-CBC decryption with the IV in front and PKCS#7 padding removed.
fn aes_cbc_decrypt(key: &[u8], data: &[u8], unpad: bool) -> Vec<u8> {
    if data.len() < 32 {
        return Vec::new();
    }
    let (iv, body) = data.split_at(16);
    let body = &body[..body.len() / 16 * 16];
    let mut out = Vec::with_capacity(body.len());
    let mut prev: [u8; 16] = iv.try_into().unwrap();
    let mut run = |dec: &dyn Fn(&mut [u8; 16])| {
        for blk in body.chunks_exact(16) {
            let mut b: [u8; 16] = blk.try_into().unwrap();
            dec(&mut b);
            for i in 0..16 {
                b[i] ^= prev[i];
            }
            out.extend_from_slice(&b);
            prev = blk.try_into().unwrap();
        }
    };
    if key.len() == 32 {
        let c = Aes256::new(GenericArray::from_slice(key));
        run(&|b| c.decrypt_block(GenericArray::from_mut_slice(b)));
    } else {
        let c = Aes128::new(GenericArray::from_slice(&key[..16]));
        run(&|b| c.decrypt_block(GenericArray::from_mut_slice(b)));
    }
    if unpad {
        if let Some(&p) = out.last() {
            if (1..=16).contains(&p) && out.len() >= p as usize {
                out.truncate(out.len() - p as usize);
            }
        }
    }
    out
}

/// AES-128-CBC encryption without padding (revision 6 hashing).
fn aes128_cbc_encrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Vec<u8> {
    let c = Aes128::new(GenericArray::from_slice(&key[..16]));
    let mut prev: [u8; 16] = iv[..16].try_into().unwrap();
    let mut out = Vec::with_capacity(data.len());
    for blk in data.chunks_exact(16) {
        let mut b: [u8; 16] = core::array::from_fn(|i| blk[i] ^ prev[i]);
        c.encrypt_block(GenericArray::from_mut_slice(&mut b));
        out.extend_from_slice(&b);
        prev = b;
    }
    out
}

/// Algorithm 2.B (revision 6) or plain SHA-256 (revision 5).
fn hash_r6(pw: &[u8], salt: &[u8], udata: &[u8], r: i64) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(pw);
    h.update(salt);
    h.update(udata);
    let mut k: Vec<u8> = h.finalize().to_vec();
    if r < 6 {
        return k[..32].try_into().unwrap();
    }
    let mut i = 0usize;
    loop {
        let mut k1 = Vec::with_capacity(64 * (pw.len() + k.len() + udata.len()));
        for _ in 0..64 {
            k1.extend_from_slice(pw);
            k1.extend_from_slice(&k);
            k1.extend_from_slice(udata);
        }
        let e = aes128_cbc_encrypt(&k[..16], &k[16..32], &k1);
        let m = e[..16].iter().map(|&b| b as u32).sum::<u32>() % 3;
        k = match m {
            0 => Sha256::digest(&e).to_vec(),
            1 => Sha384::digest(&e).to_vec(),
            _ => Sha512::digest(&e).to_vec(),
        };
        i += 1;
        if i >= 64 && (*e.last().unwrap() as usize) <= i - 32 {
            break;
        }
    }
    k[..32].try_into().unwrap()
}

fn bytes(d: &Dict, k: &str) -> Vec<u8> {
    key(d, k).and_then(Object::as_str).map(|s| s.to_vec()).unwrap_or_default()
}

fn crypt_filter_method(d: &Dict, name: Option<&[u8]>) -> Method {
    let Some(name) = name else { return Method::Identity };
    if name == b"Identity" {
        return Method::Identity;
    }
    let cf = key(d, "CF").and_then(Object::as_dict).and_then(|cf| cf.get(name)).and_then(Object::as_dict);
    match cf.and_then(|c| key(c, "CFM")).and_then(Object::as_name) {
        Some(b"V2") => Method::Rc4,
        Some(b"AESV2") => Method::Aes128,
        Some(b"AESV3") => Method::Aes256,
        Some(b"None") => Method::Identity,
        _ => Method::Rc4,
    }
}

impl Decryptor {
    pub fn new(d: &Dict, id: &[u8]) -> Result<Decryptor, String> {
        if key(d, "Filter").and_then(Object::as_name) != Some(b"Standard") {
            return Err(String::from("this PDF uses a security handler other than the standard one"));
        }
        let v = key(d, "V").and_then(Object::as_i64).unwrap_or(0);
        let r = key(d, "R").and_then(Object::as_i64).unwrap_or(2);
        let o = bytes(d, "O");
        let u = bytes(d, "U");
        let p = key(d, "P").and_then(Object::as_i64).unwrap_or(0) as i32;
        let encrypt_metadata = !matches!(key(d, "EncryptMetadata"), Some(Object::Bool(false)));
        let (strings, streams) = if v >= 4 {
            (
                crypt_filter_method(d, key(d, "StrF").and_then(Object::as_name)),
                crypt_filter_method(d, key(d, "StmF").and_then(Object::as_name)),
            )
        } else {
            (Method::Rc4, Method::Rc4)
        };
        let file_key = if r >= 5 {
            let ue = bytes(d, "UE");
            if u.len() < 48 || ue.len() < 32 {
                return Err(String::from("damaged encryption dictionary"));
            }
            if hash_r6(b"", &u[32..40], &[], r) != u[..32] {
                return Err(String::from("this PDF is protected by a password"));
            }
            let ik = hash_r6(b"", &u[40..48], &[], r);
            let mut iv_and = alloc::vec![0u8; 16];
            iv_and.extend_from_slice(&ue[..32]);
            let k = aes_cbc_decrypt(&ik, &iv_and, false);
            k[..32].to_vec()
        } else {
            let n = if r == 2 {
                5
            } else {
                (key(d, "Length").and_then(Object::as_i64).unwrap_or(40) / 8).clamp(5, 16) as usize
            };
            let mut buf = PAD.to_vec();
            buf.extend_from_slice(&o[..o.len().min(32)]);
            buf.extend_from_slice(&p.to_le_bytes());
            buf.extend_from_slice(id);
            if r >= 4 && !encrypt_metadata {
                buf.extend_from_slice(&[0xFF; 4]);
            }
            let mut k = md5(&buf).to_vec();
            if r >= 3 {
                for _ in 0..50 {
                    k = md5(&k[..n]).to_vec();
                }
            }
            k.truncate(n);
            // Check the empty user password (Algorithms 4 and 5).
            let ok = if r == 2 {
                rc4(&k, &PAD) == u[..u.len().min(32)]
            } else {
                let mut b = PAD.to_vec();
                b.extend_from_slice(id);
                let mut x = rc4(&k, &md5(&b));
                for i in 1..=19u8 {
                    let ki: Vec<u8> = k.iter().map(|&c| c ^ i).collect();
                    x = rc4(&ki, &x);
                }
                u.len() >= 16 && x[..16] == u[..16]
            };
            if !ok {
                return Err(String::from("this PDF is protected by a password"));
            }
            k
        };
        Ok(Decryptor { file_key, strings, streams, encrypt_metadata })
    }

    fn object_key(&self, r: Ref, aes: bool) -> Vec<u8> {
        let mut b = self.file_key.clone();
        b.extend_from_slice(&r.num.to_le_bytes()[..3]);
        b.extend_from_slice(&r.gen.to_le_bytes());
        if aes {
            b.extend_from_slice(b"sAlT");
        }
        let h = md5(&b);
        h[..(self.file_key.len() + 5).min(16)].to_vec()
    }

    fn apply(&self, m: Method, r: Ref, data: &[u8]) -> Vec<u8> {
        match m {
            Method::Identity => data.to_vec(),
            Method::Rc4 => rc4(&self.object_key(r, false), data),
            Method::Aes128 => aes_cbc_decrypt(&self.object_key(r, true), data, true),
            Method::Aes256 => aes_cbc_decrypt(&self.file_key, data, true),
        }
    }

    pub fn decrypt_string(&self, r: Ref, s: &[u8]) -> Vec<u8> {
        self.apply(self.strings, r, s)
    }

    pub fn decrypt_stream(&self, r: Ref, d: &Dict, data: &[u8]) -> Vec<u8> {
        if !self.encrypt_metadata && key(d, "Type").and_then(Object::as_name) == Some(b"Metadata") {
            return data.to_vec();
        }
        // A stream may name its own crypt filter (usually Identity).
        if let Some(f) = key(d, "Filter").and_then(Object::as_array) {
            if f.first().and_then(Object::as_name) == Some(b"Crypt") {
                return data.to_vec();
            }
        }
        self.apply(self.streams, r, data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digests_and_ciphers() {
        assert_eq!(
            md5(b"abc"),
            [0x90, 0x01, 0x50, 0x98, 0x3c, 0xd2, 0x4f, 0xb0, 0xd6, 0x96, 0x3f, 0x7d, 0x28, 0xe1, 0x7f, 0x72]
        );
        assert_eq!(rc4(b"Key", b"Plaintext"), [0xBB, 0xF3, 0x16, 0xE8, 0xD9, 0x40, 0xAF, 0x0A, 0xD3]);
    }
}
