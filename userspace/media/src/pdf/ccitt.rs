//! CCITTFaxDecode (ITU-T T.4 and T.6): Group 3 one-dimensional (K = 0),
//! mixed one- and two-dimensional (K > 0) and Group 4 (K < 0) coding, with
//! EOL codes, byte-aligned rows and BlackIs1. The output is packed rows of
//! `Columns` bits, 1 for white (0 for white with BlackIs1), as the PDF
//! filter defines it.

use super::ccitt_tables::{BLACK, WHITE};
use crate::bits::MsbReader;
use alloc::vec;
use alloc::vec::Vec;

pub struct Params {
    pub k: i64,
    pub columns: usize,
    pub rows: Option<usize>,
    pub byte_align: bool,
    pub black_is_1: bool,
}

/// A code tree: node `[zero, one]`, leaves `-(value + 1)`.
struct Tree(Vec<[i32; 2]>);

impl Tree {
    fn new(codes: &[(u16, &str)]) -> Tree {
        let mut t = Tree(vec![[0, 0]]);
        for &(v, bits) in codes {
            t.add(v as i32, bits);
        }
        t
    }

    fn add(&mut self, v: i32, bits: &str) {
        let mut n = 0usize;
        let b = bits.as_bytes();
        for (i, &c) in b.iter().enumerate() {
            let bit = (c == b'1') as usize;
            if i + 1 == b.len() {
                self.0[n][bit] = -v - 1;
            } else {
                if self.0[n][bit] <= 0 {
                    self.0.push([0, 0]);
                    self.0[n][bit] = (self.0.len() - 1) as i32;
                }
                n = self.0[n][bit] as usize;
            }
        }
    }

    fn decode(&self, r: &mut MsbReader) -> Option<i32> {
        let mut n = 0usize;
        loop {
            let next = self.0[n][r.read(1)? as usize];
            if next < 0 {
                return Some(-next - 1);
            }
            if next == 0 {
                return None;
            }
            n = next as usize;
        }
    }
}

/// Two-dimensional modes.
const PASS: i32 = 100;
const HORIZONTAL: i32 = 101;
const EOL: i32 = 102;
const EXT: i32 = 103;

fn mode_tree() -> Tree {
    let mut t = Tree(vec![[0, 0]]);
    for (v, bits) in [
        (3, "1"),
        (4, "011"),
        (2, "010"),
        (HORIZONTAL, "001"),
        (PASS, "0001"),
        (5, "000011"),
        (1, "000010"),
        (6, "0000011"),
        (0, "0000010"),
        (EXT, "0000001"),
        (EOL, "000000000001"),
    ] {
        t.add(v, bits);
    }
    t
}

/// One run: makeup codes (64 and up) followed by a terminating code.
fn run(r: &mut MsbReader, t: &Tree) -> Option<usize> {
    let mut total = 0usize;
    loop {
        let v = t.decode(r)? as usize;
        total += v;
        if v < 64 {
            return Some(total);
        }
    }
}

/// Skips fill bits and one EOL code if they come next; true when skipped.
fn skip_eol(r: &mut MsbReader) -> bool {
    let save = r.clone();
    let mut zeros = 0;
    while let Some(b) = r.read(1) {
        if b == 1 {
            if zeros >= 11 {
                return true;
            }
            break;
        }
        zeros += 1;
    }
    *r = save;
    false
}

pub fn decode(data: &[u8], p: &Params) -> Vec<u8> {
    decode_with_end(data, p).0
}

/// MMR (Group 4) data of JBIG2: `rows` rows of `columns` pixels, 1 bits
/// black, and the bytes taken, including an end-of-block code after the
/// rows when there is one.
pub fn decode_mmr(data: &[u8], columns: usize, rows: usize) -> (Vec<u8>, usize) {
    let p = Params { k: -1, columns, rows: Some(rows), byte_align: false, black_is_1: true };
    decode_with_end(data, &p)
}

/// `decode`, also returning how many bytes of `data` it used.
fn decode_with_end(data: &[u8], p: &Params) -> (Vec<u8>, usize) {
    // JBIG2 collective bitmaps can be far wider than any page.
    let cols = p.columns.clamp(1, 1 << 24);
    let row_bytes = cols.div_ceil(8);
    let white = Tree::new(&WHITE);
    let black = Tree::new(&BLACK);
    let modes = mode_tree();
    let mut r = MsbReader::new(data);
    let mut out = Vec::new();
    // Changing elements of the reference line (an imaginary white line).
    let mut reference: Vec<usize> = vec![cols, cols];
    // At most 2^28 pixels.
    let max_rows = p.rows.unwrap_or(usize::MAX).min((1 << 28) / cols);
    let mut rows = 0;
    while rows < max_rows && r.bits_left() > 0 {
        if p.byte_align && p.k < 0 {
            r.align();
        }
        // Group 3: optional EOL, then (K > 0) a tag bit for 1D or 2D.
        let mut two_d = p.k < 0;
        if p.k >= 0 {
            let mut eols = 0;
            while skip_eol(&mut r) {
                eols += 1;
                if eols >= 6 {
                    break;
                }
            }
            if eols >= 6 && rows > 0 {
                break; // RTC: end of the page
            }
            if p.byte_align {
                r.align();
            }
            if p.k > 0 {
                match r.read(1) {
                    Some(b) => two_d = b == 0,
                    None => break,
                }
            }
        }
        let mut changes: Vec<usize> = Vec::with_capacity(64);
        let mut ok = true;
        if two_d {
            let mut a0: isize = -1;
            let mut color = 0; // 0 white, 1 black
            let mut steps = 0;
            while (a0 as usize) < cols || a0 < 0 {
                // Damaged data can stall a0: a row has at most `cols`
                // changes.
                steps += 1;
                if steps > 2 * cols + 8 {
                    ok = false;
                    break;
                }
                // b1: the first change on the reference line right of a0
                // to the opposite colour; b2 the one after it.
                let start = if a0 < 0 { 0 } else { a0 as usize + 1 };
                let mut i = reference.partition_point(|&x| x < start);
                if i % 2 != color {
                    i += 1;
                }
                let b1 = reference.get(i).copied().unwrap_or(cols).min(cols);
                let b2 = reference.get(i + 1).copied().unwrap_or(cols).min(cols);
                let Some(m) = modes.decode(&mut r) else {
                    ok = false;
                    break;
                };
                match m {
                    PASS => a0 = b2 as isize,
                    HORIZONTAL => {
                        let (t1, t2) = if color == 0 { (&white, &black) } else { (&black, &white) };
                        let (Some(r1), Some(r2)) = (run(&mut r, t1), run(&mut r, t2)) else {
                            ok = false;
                            break;
                        };
                        let base = a0.max(0) as usize;
                        let a1 = (base + r1).min(cols);
                        let a2 = (a1 + r2).min(cols);
                        changes.push(a1);
                        changes.push(a2);
                        a0 = a2 as isize;
                    }
                    0..=6 => {
                        let a1 = (b1 as isize + m as isize - 3).clamp(0, cols as isize) as usize;
                        changes.push(a1);
                        a0 = a1 as isize;
                        color ^= 1;
                    }
                    _ => {
                        ok = false;
                        break;
                    }
                }
            }
        } else {
            let mut a0 = 0usize;
            let mut color = 0;
            while a0 < cols {
                let Some(n) = run(&mut r, if color == 0 { &white } else { &black }) else {
                    ok = false;
                    break;
                };
                a0 = (a0 + n).min(cols);
                changes.push(a0);
                color ^= 1;
            }
        }
        if !ok && changes.is_empty() {
            break; // end of data or EOFB
        }
        // Paint the row: runs alternate white, black from column 0.
        let mut row = vec![0u8; row_bytes];
        let mut x = 0usize;
        let mut color = 0;
        let mut edges = changes.clone();
        edges.push(cols);
        for &e in &edges {
            let e = e.min(cols);
            if color == 0 {
                for c in x..e.max(x) {
                    row[c / 8] |= 0x80 >> (c % 8);
                }
            }
            x = e.max(x);
            color ^= 1;
        }
        if p.black_is_1 {
            for b in row.iter_mut() {
                *b = !*b;
            }
        }
        out.extend_from_slice(&row);
        changes.push(cols);
        changes.push(cols);
        reference = changes;
        rows += 1;
        if !ok {
            break;
        }
    }
    // An end-of-block (two EOLs) after the last row belongs to this data.
    if p.k < 0 && r.bits_left() >= 24 && r.peek_unchecked(24) == 0x001001 {
        r.skip(24);
    }
    let used = r.bit_pos().div_ceil(8).min(data.len());
    (out, used)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    /// Bits of `rows` (1 = white) against the reference made with libtiff.
    fn check(data: &[u8], reference: &[u8], cols: usize, k: i64) {
        let rows = reference.len() / cols.div_ceil(8);
        let out = decode(data, &Params { k, columns: cols, rows: Some(rows), byte_align: false, black_is_1: false });
        assert_eq!(out.len(), reference.len(), "K={k}: rows decoded");
        let bad = out.iter().zip(reference).filter(|(a, b)| a != b).count();
        assert_eq!(bad, 0, "K={k}: {bad} bytes differ");
    }

    #[test]
    fn group4_and_group3_match_libtiff() {
        let d = |n: &str| -> Vec<u8> {
            std::fs::read(alloc::format!("{}/tests/data/ccitt/{n}", env!("CARGO_MANIFEST_DIR"))).unwrap()
        };
        let cols = u32::from_le_bytes(d("size")[..4].try_into().unwrap()) as usize;
        check(&d("g4.bin"), &d("ref.bin"), cols, -1);
        check(&d("g3.bin"), &d("ref.bin"), cols, 0);
        check(&d("g3-2d.bin"), &d("ref.bin"), cols, 4);
    }
}
