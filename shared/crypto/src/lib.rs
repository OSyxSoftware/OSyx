//! Cryptographic primitives for the kernel random number generator:
//!
//! * [`chacha20_block`]: the ChaCha20 block function of RFC 8439.
//! * [`Sha256`]: SHA-256 (FIPS 180-4), used to condense raw entropy.
//! * [`Drbg`]: a ChaCha20 generator with fast key erasure: every request
//!   produces one block stream whose first 32 bytes replace the key, so a
//!   later compromise of the state does not reveal earlier output.
//!
//! Written from the specifications and checked against their published
//! test vectors. Not constant-time-audited beyond what the algorithms give
//! by construction (no secret-dependent branches or table lookups).

#![no_std]

#[inline(always)]
fn quarter(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(16);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(12);
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(8);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(7);
}

/// One 64-byte ChaCha20 block for `key`, `counter` and `nonce` (RFC 8439 §2.3).
pub fn chacha20_block(key: &[u8; 32], counter: u32, nonce: &[u8; 12]) -> [u8; 64] {
    let mut init = [0u32; 16];
    init[0] = 0x6170_7865;
    init[1] = 0x3320_646e;
    init[2] = 0x7962_2d32;
    init[3] = 0x6b20_6574;
    for i in 0..8 {
        init[4 + i] = u32::from_le_bytes(key[4 * i..4 * i + 4].try_into().expect("4 bytes"));
    }
    init[12] = counter;
    for i in 0..3 {
        init[13 + i] = u32::from_le_bytes(nonce[4 * i..4 * i + 4].try_into().expect("4 bytes"));
    }
    let mut s = init;
    for _ in 0..10 {
        quarter(&mut s, 0, 4, 8, 12);
        quarter(&mut s, 1, 5, 9, 13);
        quarter(&mut s, 2, 6, 10, 14);
        quarter(&mut s, 3, 7, 11, 15);
        quarter(&mut s, 0, 5, 10, 15);
        quarter(&mut s, 1, 6, 11, 12);
        quarter(&mut s, 2, 7, 8, 13);
        quarter(&mut s, 3, 4, 9, 14);
    }
    let mut out = [0u8; 64];
    for i in 0..16 {
        out[4 * i..4 * i + 4].copy_from_slice(&s[i].wrapping_add(init[i]).to_le_bytes());
    }
    out
}

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98,
    0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
    0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8,
    0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
    0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819,
    0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
    0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
    0xc67178f2,
];

#[derive(Clone)]
pub struct Sha256 {
    h: [u32; 8],
    buf: [u8; 64],
    buf_len: usize,
    total: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    pub fn new() -> Sha256 {
        Sha256 {
            h: [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19],
            buf: [0; 64],
            buf_len: 0,
            total: 0,
        }
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(block[4 * i..4 * i + 4].try_into().expect("4 bytes"));
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, v) in self.h.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *x = x.wrapping_add(v);
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total = self.total.wrapping_add(data.len() as u64);
        while !data.is_empty() {
            let n = (64 - self.buf_len).min(data.len());
            self.buf[self.buf_len..self.buf_len + n].copy_from_slice(&data[..n]);
            self.buf_len += n;
            data = &data[n..];
            if self.buf_len == 64 {
                let block = self.buf;
                self.compress(&block);
                self.buf_len = 0;
            }
        }
    }

    pub fn finish(mut self) -> [u8; 32] {
        let bits = self.total.wrapping_mul(8);
        self.update(&[0x80]);
        while self.buf_len != 56 {
            self.update(&[0]);
        }
        let mut block = self.buf;
        block[56..64].copy_from_slice(&bits.to_be_bytes());
        self.compress(&block);
        let mut out = [0u8; 32];
        for (i, v) in self.h.iter().enumerate() {
            out[4 * i..4 * i + 4].copy_from_slice(&v.to_be_bytes());
        }
        out
    }

    pub fn digest(data: &[u8]) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(data);
        h.finish()
    }
}

/// ChaCha20 deterministic random bit generator with fast key erasure.
pub struct Drbg {
    key: [u8; 32],
    seeded: bool,
    /// Requests served since the last reseed (diagnostics).
    pub requests: u64,
}

impl Default for Drbg {
    fn default() -> Self {
        Self::new()
    }
}

impl Drbg {
    pub const fn new() -> Drbg {
        Drbg { key: [0; 32], seeded: false, requests: 0 }
    }

    pub fn is_seeded(&self) -> bool {
        self.seeded
    }

    /// Mixes `seed` (already condensed entropy) into the key:
    /// key = SHA-256(old key || seed).
    pub fn reseed(&mut self, seed: &[u8; 32]) {
        let mut h = Sha256::new();
        h.update(b"OSyx DRBG reseed");
        h.update(&self.key);
        h.update(seed);
        self.key = h.finish();
        self.seeded = true;
        self.requests = 0;
    }

    /// Fills `out`; `None` (and nothing written) if never seeded.
    pub fn fill(&mut self, out: &mut [u8]) -> Option<()> {
        if !self.seeded {
            return None;
        }
        const NONCE: [u8; 12] = *b"OSyx-random\0";
        // Block 0 becomes the next key (and its second half is discarded);
        // output starts at block 1.
        let first = chacha20_block(&self.key, 0, &NONCE);
        for (counter, chunk) in (1u32..).zip(out.chunks_mut(64)) {
            let b = chacha20_block(&self.key, counter, &NONCE);
            chunk.copy_from_slice(&b[..chunk.len()]);
        }
        self.key.copy_from_slice(&first[..32]);
        self.requests += 1;
        Some(())
    }
}

/// Requests longer than this are split by callers (the block counter is 32 bits).
pub const MAX_REQUEST: usize = 64 * 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> [u8; 64] {
        let s: String = s.split_whitespace().collect();
        let mut out = [0u8; 64];
        for (i, b) in out.iter_mut().enumerate().take(s.len() / 2) {
            *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap();
        }
        out
    }

    extern crate std;
    use std::string::String;

    #[test]
    fn rfc8439_block_vector() {
        // RFC 8439 section 2.3.2.
        let key: [u8; 32] = core::array::from_fn(|i| i as u8);
        let nonce = [0, 0, 0, 0x09, 0, 0, 0, 0x4a, 0, 0, 0, 0];
        let want = hex(
            "10f1e7e4d13b5915500fdd1fa32071c4c7d1f4c733c068030422aa9ac3d46c4e
             d2826446079faa0914c2d705d98b02a2b5129cd1de164eb9cbd083e8a2503c4e",
        );
        assert_eq!(chacha20_block(&key, 1, &nonce), want);
    }

    #[test]
    fn rfc8439_zero_key_vector() {
        // RFC 8439 appendix A.1, test vector #1.
        let want = hex(
            "76b8e0ada0f13d90405d6ae55386bd28bdd219b8a08ded1aa836efcc8b770dc7
             da41597c5157488d7724e03fb8d84a376a43b8f41518a11cc387b669b2ee6586",
        );
        assert_eq!(chacha20_block(&[0; 32], 0, &[0; 12]), want);
    }

    #[test]
    fn sha256_vectors() {
        let h = |s: &[u8]| Sha256::digest(s).iter().map(|b| std::format!("{b:02x}")).collect::<String>();
        assert_eq!(h(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(h(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(
            h(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        // One million 'a' fed in odd-sized pieces.
        let mut s = Sha256::new();
        let chunk = [b'a'; 997];
        let mut left = 1_000_000;
        while left > 0 {
            let n = left.min(chunk.len());
            s.update(&chunk[..n]);
            left -= n;
        }
        let d = s.finish().iter().map(|b| std::format!("{b:02x}")).collect::<String>();
        assert_eq!(d, "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
    }

    #[test]
    fn drbg_refuses_until_seeded_and_never_repeats() {
        let mut d = Drbg::new();
        let mut a = [0u8; 100];
        assert!(d.fill(&mut a).is_none());
        assert_eq!(a, [0; 100]);
        d.reseed(&[7; 32]);
        d.fill(&mut a).unwrap();
        let mut b = [0u8; 100];
        d.fill(&mut b).unwrap();
        assert_ne!(a, b, "key erasure changes the stream");
        assert_ne!(a, [0; 100]);
        // Same seed, same stream: deterministic given the state.
        let mut e = Drbg::new();
        e.reseed(&[7; 32]);
        let mut c = [0u8; 100];
        e.fill(&mut c).unwrap();
        assert_eq!(a, c);
        // Different seed, different stream.
        let mut f = Drbg::new();
        f.reseed(&[8; 32]);
        f.fill(&mut c).unwrap();
        assert_ne!(a, c);
        // Rough sanity: bits are balanced over a larger sample.
        let mut big = std::vec![0u8; 1 << 16];
        d.fill(&mut big).unwrap();
        let ones: u32 = big.iter().map(|b| b.count_ones()).sum();
        let total = big.len() as f64 * 8.0;
        assert!((ones as f64 / total - 0.5).abs() < 0.01);
    }
}
