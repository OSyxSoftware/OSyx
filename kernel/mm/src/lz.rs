//! Page compression for compressed swap: a small LZ77 coder for one 4 KiB
//! page, without allocation.
//!
//! Format: a sequence of tokens. A control byte below 0x80 starts a literal
//! run of `ctrl + 1` bytes (1..=128) that follow. A control byte with the
//! top bit set is a match of `(ctrl & 0x7F) + MIN_MATCH` bytes (3..=130)
//! copied from `offset` bytes back, `offset` (1..=4096) following as a
//! little-endian u16. Matches may overlap their own output (runs).

use crate::PAGE_SIZE;

const PAGE: usize = PAGE_SIZE as usize;
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 0x7F + MIN_MATCH;
const MAX_LITERALS: usize = 128;
const HASH_BITS: u32 = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LzError {
    /// The input does not decode to exactly one page.
    Corrupt,
}

fn hash(b: &[u8]) -> usize {
    let v = u32::from_le_bytes([b[0], b[1], b[2], 0]);
    (v.wrapping_mul(2_654_435_761) >> (32 - HASH_BITS)) as usize
}

/// Compresses `src` into `dst`; returns the compressed length, or `None`
/// when the result would not fit in `dst` (the page does not compress well
/// enough to be worth keeping).
pub fn compress(src: &[u8; PAGE], dst: &mut [u8]) -> Option<usize> {
    let mut table = [u16::MAX; 1 << HASH_BITS];
    let mut out = 0usize;
    let mut lit_start = 0usize;
    let mut i = 0usize;
    let flush_literals = |dst: &mut [u8], out: &mut usize, from: usize, to: usize| -> Option<()> {
        let mut at = from;
        while at < to {
            let n = (to - at).min(MAX_LITERALS);
            if *out + 1 + n > dst.len() {
                return None;
            }
            dst[*out] = (n - 1) as u8;
            dst[*out + 1..*out + 1 + n].copy_from_slice(&src[at..at + n]);
            *out += 1 + n;
            at += n;
        }
        Some(())
    };
    while i + MIN_MATCH <= PAGE {
        let h = hash(&src[i..]);
        let cand = table[h];
        table[h] = i as u16;
        let mut len = 0;
        let mut off = 0;
        // A run of the previous byte (offset 1) beats the hash candidate for
        // the frequent zero and fill patterns.
        for (c, o) in [(cand as usize, i.wrapping_sub(cand as usize)), (i.wrapping_sub(1), 1)] {
            if c == u16::MAX as usize || c >= i || o == 0 || o > PAGE {
                continue;
            }
            let mut l = 0;
            while i + l < PAGE && l < MAX_MATCH && src[c + l] == src[i + l] {
                l += 1;
            }
            if l > len {
                len = l;
                off = o;
            }
        }
        if len >= MIN_MATCH {
            flush_literals(dst, &mut out, lit_start, i)?;
            if out + 3 > dst.len() {
                return None;
            }
            dst[out] = 0x80 | (len - MIN_MATCH) as u8;
            dst[out + 1..out + 3].copy_from_slice(&(off as u16).to_le_bytes());
            out += 3;
            i += len;
            lit_start = i;
        } else {
            i += 1;
        }
    }
    flush_literals(dst, &mut out, lit_start, PAGE)?;
    Some(out)
}

/// Decodes `src` into exactly one page.
pub fn decompress(src: &[u8], dst: &mut [u8; PAGE]) -> Result<(), LzError> {
    let mut i = 0usize;
    let mut out = 0usize;
    while i < src.len() {
        let ctrl = src[i];
        i += 1;
        if ctrl & 0x80 == 0 {
            let n = ctrl as usize + 1;
            if i + n > src.len() || out + n > PAGE {
                return Err(LzError::Corrupt);
            }
            dst[out..out + n].copy_from_slice(&src[i..i + n]);
            i += n;
            out += n;
        } else {
            let n = (ctrl & 0x7F) as usize + MIN_MATCH;
            if i + 2 > src.len() {
                return Err(LzError::Corrupt);
            }
            let off = u16::from_le_bytes([src[i], src[i + 1]]) as usize;
            i += 2;
            if off == 0 || off > out || out + n > PAGE {
                return Err(LzError::Corrupt);
            }
            if off == 1 {
                let value = dst[out - 1];
                dst[out..out + n].fill(value);
            } else {
                // Copy a non-overlapping seed, then double the already
                // produced pattern until the match is complete.
                let seed = off.min(n);
                dst.copy_within(out - off..out - off + seed, out);
                let mut copied = seed;
                while copied < n {
                    let chunk = copied.min(n - copied);
                    dst.copy_within(out..out + chunk, out + copied);
                    copied += chunk;
                }
            }
            out += n;
        }
    }
    if out != PAGE {
        return Err(LzError::Corrupt);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(page: &[u8; PAGE]) -> usize {
        let mut buf = [0u8; PAGE];
        let n = compress(page, &mut buf).expect("compressible");
        let mut back = [0xAAu8; PAGE];
        decompress(&buf[..n], &mut back).unwrap();
        assert!(back[..] == page[..]);
        n
    }

    #[test]
    fn zero_and_patterned_pages_shrink() {
        assert!(roundtrip(&[0u8; PAGE]) < 120);
        let mut p = [0u8; PAGE];
        for (i, b) in p.iter_mut().enumerate() {
            *b = (i % 13) as u8;
        }
        assert!(roundtrip(&p) < 200);
        // Text-like data with repeats.
        let text = b"OSyx compressed memory keeps cold pages small. ";
        for (i, b) in p.iter_mut().enumerate() {
            *b = text[i % text.len()];
        }
        assert!(roundtrip(&p) < 400);
        // Mostly zero with a few values (a typical heap page).
        let mut q = [0u8; PAGE];
        for i in (0..PAGE).step_by(97) {
            q[i] = i as u8;
        }
        roundtrip(&q);
    }

    #[test]
    fn overlapping_matches_reproduce_patterns() {
        for period in 1..=150 {
            let mut page = [0u8; PAGE];
            for (i, byte) in page.iter_mut().enumerate() {
                *byte = ((i % period) * 37) as u8;
            }
            roundtrip(&page);
        }
    }

    #[test]
    fn random_pages_do_not_fit_a_small_buffer() {
        let mut p = [0u8; PAGE];
        let mut x = 0x1234_5678u32;
        for b in p.iter_mut() {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            *b = x as u8;
        }
        let mut small = [0u8; PAGE * 3 / 4];
        assert!(compress(&p, &mut small).is_none());
        // With room, random data still round-trips (as literals).
        let mut big = [0u8; PAGE + PAGE / 64 + 16];
        let n = compress(&p, &mut big).unwrap();
        let mut back = [0u8; PAGE];
        decompress(&big[..n], &mut back).unwrap();
        assert!(back[..] == p[..]);
    }

    #[test]
    fn corrupt_input_is_rejected() {
        let mut back = [0u8; PAGE];
        assert_eq!(decompress(&[0x80, 1, 0], &mut back), Err(LzError::Corrupt));
        assert_eq!(decompress(&[5, 1, 2], &mut back), Err(LzError::Corrupt));
        assert_eq!(decompress(&[], &mut back), Err(LzError::Corrupt));
        // Every truncation of a valid stream fails cleanly.
        let mut p = [0u8; PAGE];
        for (i, b) in p.iter_mut().enumerate() {
            *b = (i / 7) as u8;
        }
        let mut buf = [0u8; PAGE];
        let n = compress(&p, &mut buf).unwrap();
        for cut in 0..n {
            assert!(decompress(&buf[..cut], &mut back).is_err());
        }
    }
}
