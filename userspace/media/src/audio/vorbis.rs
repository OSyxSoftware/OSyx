//! Vorbis I (the Xiph.Org specification): identification and setup
//! headers, codebooks with scalar and vector lookup, floor types 0 and 1,
//! residue types 0, 1 and 2, channel coupling, the windowed inverse MDCT
//! (through an FFT) and overlap-add. Positions come from the Ogg granule
//! positions, so the start and the end of the stream are trimmed exactly
//! and seeking lands on the requested sample.

use super::ogg::Packets;
use super::vorbis_table::FLOOR1_INVERSE_DB;
use super::{Decoder, Info};
use crate::bits::LsbReader;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::f32::consts::PI;

/// Entries of one codebook, and values of its expanded vector table.
const MAX_ENTRIES: usize = 1 << 20;
const MAX_VQ: usize = 1 << 22;
/// Entries and vector values of all codebooks of one stream together.
const MAX_SETUP: usize = 1 << 24;

fn ilog(v: u32) -> u32 {
    32 - v.leading_zeros()
}

fn float32_unpack(x: u32) -> f32 {
    let mantissa = (x & 0x1F_FFFF) as f32;
    let exp = ((x & 0x7FE0_0000) >> 21) as i32 - 788;
    let v = mantissa * libm::exp2f(exp as f32);
    if x & 0x8000_0000 != 0 {
        -v
    } else {
        v
    }
}

/// The largest r with r^dim <= entries.
fn lookup1_values(entries: u32, dim: u32) -> u32 {
    let mut r = libm::floor(libm::pow(entries as f64, 1.0 / dim as f64)) as u32;
    while (r as u64 + 1).pow(dim) <= entries as u64 {
        r += 1;
    }
    while r > 0 && (r as u64).pow(dim) > entries as u64 {
        r -= 1;
    }
    r
}

struct Codebook {
    dim: usize,
    /// Decoding tree: `[zero, one]`, leaves are `-(entry + 1)`.
    tree: Vec<[i32; 2]>,
    /// The only used entry and its length, when there is just one.
    single: Option<(u32, u32)>,
    /// `entries * dim` vector values, empty without a lookup table.
    vq: Vec<f32>,
}

impl Codebook {
    /// Reads a codebook; `budget` is what the setup header may still
    /// allocate for entries and vector tables, shared by all its books.
    fn read(r: &mut LsbReader, budget: &mut usize) -> Result<Codebook, String> {
        let bad = || String::from("corrupt Vorbis codebook");
        if r.read(24).ok_or_else(bad)? != 0x564342 {
            return Err(bad());
        }
        let dim = r.read(16).ok_or_else(bad)? as usize;
        let entries = r.read(24).ok_or_else(bad)? as usize;
        // Real codebooks stay far below these; they keep a crafted setup
        // header from asking for gigabytes.
        if entries > MAX_ENTRIES || entries.saturating_mul(dim) > MAX_VQ {
            return Err(bad());
        }
        // Lengths, tree nodes and the expanded vectors of this book.
        let cost = entries.saturating_mul(3).saturating_add(entries.saturating_mul(dim));
        *budget = budget.checked_sub(cost).ok_or_else(bad)?;
        let mut lens = vec![0u32; entries];
        if r.bit().ok_or_else(bad)? {
            // Ordered: runs of increasing lengths.
            let mut len = r.read(5).ok_or_else(bad)? + 1;
            let mut e = 0;
            while e < entries {
                let n = r.read(ilog((entries - e) as u32)).ok_or_else(bad)? as usize;
                if e + n > entries || len > 32 {
                    return Err(bad());
                }
                lens[e..e + n].fill(len);
                e += n;
                len += 1;
            }
        } else {
            let sparse = r.bit().ok_or_else(bad)?;
            for l in lens.iter_mut() {
                if !sparse || r.bit().ok_or_else(bad)? {
                    *l = r.read(5).ok_or_else(bad)? + 1;
                }
            }
        }
        let lookup = r.read(4).ok_or_else(bad)?;
        let mut vq = Vec::new();
        if lookup == 1 || lookup == 2 {
            if dim == 0 {
                return Err(bad());
            }
            let min = float32_unpack(r.read(32).ok_or_else(bad)?);
            let delta = float32_unpack(r.read(32).ok_or_else(bad)?);
            let bits = r.read(4).ok_or_else(bad)? + 1;
            let seq = r.bit().ok_or_else(bad)?;
            let values = if lookup == 1 { lookup1_values(entries as u32, dim as u32) as usize } else { entries * dim };
            if values.saturating_mul(bits as usize) > r.bits_left() {
                return Err(bad());
            }
            let mut mult = Vec::with_capacity(values);
            for _ in 0..values {
                mult.push(r.read(bits).ok_or_else(bad)? as f32);
            }
            vq = vec![0.0; entries * dim];
            for e in 0..entries {
                let mut last = 0.0;
                let mut div = 1usize;
                for i in 0..dim {
                    let off = if lookup == 1 { (e / div) % values.max(1) } else { e * dim + i };
                    let v = mult.get(off).copied().unwrap_or(0.0) * delta + min + last;
                    if seq {
                        last = v;
                    }
                    vq[e * dim + i] = v;
                    div = div.saturating_mul(values);
                }
            }
        } else if lookup != 0 {
            return Err(bad());
        }
        // Codeword assignment (the reference decoder's marker algorithm).
        let used: Vec<usize> = (0..entries).filter(|&e| lens[e] > 0).collect();
        let single = (used.len() == 1).then(|| (used[0] as u32, lens[used[0]]));
        let mut tree = vec![[0i32; 2]];
        let mut marker = [0u32; 33];
        for &e in &used {
            let len = lens[e] as usize;
            let mut code = marker[len];
            if len < 32 && code >> len != 0 {
                if single.is_some() {
                    break;
                }
                return Err(String::from("over-specified Vorbis codebook"));
            }
            // Insert into the tree, most significant bit first.
            let mut n = 0usize;
            for b in (0..len).rev() {
                let bit = ((code >> b) & 1) as usize;
                if b == 0 {
                    tree[n][bit] = -(e as i32) - 1;
                } else {
                    if tree[n][bit] <= 0 {
                        tree.push([0, 0]);
                        tree[n][bit] = (tree.len() - 1) as i32;
                    }
                    n = tree[n][bit] as usize;
                }
            }
            for j in (1..=len).rev() {
                if marker[j] & 1 != 0 {
                    if j == 1 {
                        marker[1] += 1;
                    } else {
                        marker[j] = marker[j - 1] << 1;
                    }
                    break;
                }
                marker[j] += 1;
            }
            for j in len + 1..33 {
                if marker[j] >> 1 == code {
                    code = marker[j];
                    marker[j] = marker[j - 1] << 1;
                } else {
                    break;
                }
            }
        }
        Ok(Codebook { dim, tree, single, vq })
    }

    fn decode(&self, r: &mut LsbReader) -> Option<u32> {
        if let Some((e, len)) = self.single {
            r.read(len)?;
            return Some(e);
        }
        let mut n = 0usize;
        loop {
            let next = self.tree[n][r.read(1)? as usize];
            if next < 0 {
                return Some((-next - 1) as u32);
            }
            if next == 0 {
                return None;
            }
            n = next as usize;
        }
    }

    fn vector(&self, e: u32) -> &[f32] {
        let s = e as usize * self.dim;
        self.vq.get(s..s + self.dim).unwrap_or(&[])
    }
}

enum Floor {
    Zero { order: usize, rate: u32, bark_size: u32, amp_bits: u32, amp_offset: u32, books: Vec<usize> },
    One(Floor1),
}

struct Floor1 {
    partition_class: Vec<usize>,
    class_dim: Vec<usize>,
    class_sub: Vec<u32>,
    class_master: Vec<usize>,
    sub_books: Vec<Vec<i32>>,
    multiplier: i32,
    xs: Vec<i32>,
    /// `xs` indices sorted by position.
    sorted: Vec<usize>,
    /// Low and high neighbours of every point from 2 on.
    neighbours: Vec<(usize, usize)>,
}

struct Residue {
    kind: u32,
    begin: usize,
    end: usize,
    psize: usize,
    classes: usize,
    classbook: usize,
    books: Vec<[i32; 8]>,
}

struct Mapping {
    coupling: Vec<(usize, usize)>,
    mux: Vec<usize>,
    /// (floor, residue) per submap.
    submaps: Vec<(usize, usize)>,
}

struct Mode {
    long: bool,
    mapping: usize,
}

struct Setup {
    channels: usize,
    blocksize: [usize; 2],
    books: Vec<Codebook>,
    floors: Vec<Floor>,
    residues: Vec<Residue>,
    mappings: Vec<Mapping>,
    modes: Vec<Mode>,
}

fn read_setup(packet: &[u8], channels: usize, blocksize: [usize; 2]) -> Result<Setup, String> {
    let bad = || String::from("corrupt Vorbis setup header");
    if packet.len() < 7 || packet[0] != 5 || &packet[1..7] != b"vorbis" {
        return Err(bad());
    }
    let mut r = LsbReader::new(&packet[7..]);
    let rd = |r: &mut LsbReader, n: u32| r.read(n).ok_or_else(bad);
    let mut books = Vec::new();
    let mut budget = MAX_SETUP;
    for _ in 0..rd(&mut r, 8)? + 1 {
        books.push(Codebook::read(&mut r, &mut budget)?);
    }
    for _ in 0..rd(&mut r, 6)? + 1 {
        if rd(&mut r, 16)? != 0 {
            return Err(bad());
        }
    }
    let book = |i: u32, books: &Vec<Codebook>| if (i as usize) < books.len() { Ok(i as usize) } else { Err(bad()) };
    let mut floors = Vec::new();
    for _ in 0..rd(&mut r, 6)? + 1 {
        match rd(&mut r, 16)? {
            0 => {
                let order = rd(&mut r, 8)? as usize;
                let rate = rd(&mut r, 16)?;
                let bark_size = rd(&mut r, 16)?;
                let amp_bits = rd(&mut r, 6)?;
                let amp_offset = rd(&mut r, 8)?;
                let n = rd(&mut r, 4)? + 1;
                let mut fb = Vec::new();
                for _ in 0..n {
                    fb.push(book(rd(&mut r, 8)?, &books)?);
                }
                floors.push(Floor::Zero { order, rate, bark_size, amp_bits, amp_offset, books: fb });
            }
            1 => {
                let parts = rd(&mut r, 5)? as usize;
                let mut partition_class = Vec::with_capacity(parts);
                for _ in 0..parts {
                    partition_class.push(rd(&mut r, 4)? as usize);
                }
                let classes = partition_class.iter().copied().max().map_or(0, |m| m + 1);
                let (mut class_dim, mut class_sub, mut class_master, mut sub_books) = (vec![], vec![], vec![], vec![]);
                for _ in 0..classes {
                    class_dim.push(rd(&mut r, 3)? as usize + 1);
                    let sub = rd(&mut r, 2)?;
                    class_sub.push(sub);
                    class_master.push(if sub > 0 { book(rd(&mut r, 8)?, &books)? } else { 0 });
                    let mut sb = Vec::new();
                    for _ in 0..1 << sub {
                        let b = rd(&mut r, 8)? as i32 - 1;
                        if b >= books.len() as i32 {
                            return Err(bad());
                        }
                        sb.push(b);
                    }
                    sub_books.push(sb);
                }
                let multiplier = rd(&mut r, 2)? as i32 + 1;
                let rangebits = rd(&mut r, 4)?;
                let mut xs = vec![0, 1 << rangebits];
                for &c in &partition_class {
                    for _ in 0..class_dim[c] {
                        xs.push(rd(&mut r, rangebits)? as i32);
                    }
                }
                if xs.len() > 65 {
                    return Err(bad());
                }
                let mut sorted: Vec<usize> = (0..xs.len()).collect();
                sorted.sort_by_key(|&i| xs[i]);
                // The X values must be unique (a zero-width line divides by 0).
                if sorted.windows(2).any(|w| xs[w[0]] == xs[w[1]]) {
                    return Err(bad());
                }
                let mut neighbours = vec![(0, 0); xs.len()];
                for i in 2..xs.len() {
                    let (mut lo, mut hi) = (0usize, 1usize);
                    let (mut lx, mut hx) = (-1, i32::MAX);
                    for j in 0..i {
                        if xs[j] < xs[i] && xs[j] > lx {
                            lx = xs[j];
                            lo = j;
                        }
                        if xs[j] > xs[i] && xs[j] < hx {
                            hx = xs[j];
                            hi = j;
                        }
                    }
                    neighbours[i] = (lo, hi);
                }
                floors.push(Floor::One(Floor1 {
                    partition_class,
                    class_dim,
                    class_sub,
                    class_master,
                    sub_books,
                    multiplier,
                    xs,
                    sorted,
                    neighbours,
                }));
            }
            _ => return Err(bad()),
        }
    }
    let mut residues = Vec::new();
    for _ in 0..rd(&mut r, 6)? + 1 {
        let kind = rd(&mut r, 16)?;
        if kind > 2 {
            return Err(bad());
        }
        let begin = rd(&mut r, 24)? as usize;
        let end = rd(&mut r, 24)? as usize;
        let psize = rd(&mut r, 24)? as usize + 1;
        let classes = rd(&mut r, 6)? as usize + 1;
        let classbook = book(rd(&mut r, 8)?, &books)?;
        let mut cascade = Vec::with_capacity(classes);
        for _ in 0..classes {
            let low = rd(&mut r, 3)?;
            let high = if rd(&mut r, 1)? != 0 { rd(&mut r, 5)? } else { 0 };
            cascade.push(high * 8 + low);
        }
        let mut rbooks = Vec::with_capacity(classes);
        for c in cascade {
            let mut b = [-1i32; 8];
            for (j, slot) in b.iter_mut().enumerate() {
                if c & (1 << j) != 0 {
                    *slot = book(rd(&mut r, 8)?, &books)? as i32;
                }
            }
            rbooks.push(b);
        }
        residues.push(Residue { kind, begin, end, psize, classes, classbook, books: rbooks });
    }
    let mut mappings = Vec::new();
    for _ in 0..rd(&mut r, 6)? + 1 {
        if rd(&mut r, 16)? != 0 {
            return Err(bad());
        }
        let submaps = if rd(&mut r, 1)? != 0 { rd(&mut r, 4)? as usize + 1 } else { 1 };
        let mut coupling = Vec::new();
        if rd(&mut r, 1)? != 0 {
            let bits = ilog(channels as u32 - 1);
            for _ in 0..rd(&mut r, 8)? + 1 {
                let m = rd(&mut r, bits)? as usize;
                let a = rd(&mut r, bits)? as usize;
                if m == a || m >= channels || a >= channels {
                    return Err(bad());
                }
                coupling.push((m, a));
            }
        }
        if rd(&mut r, 2)? != 0 {
            return Err(bad());
        }
        let mut mux = vec![0; channels];
        if submaps > 1 {
            for m in mux.iter_mut() {
                *m = rd(&mut r, 4)? as usize;
                if *m >= submaps {
                    return Err(bad());
                }
            }
        }
        let mut sm = Vec::new();
        for _ in 0..submaps {
            rd(&mut r, 8)?;
            let f = rd(&mut r, 8)? as usize;
            let res = rd(&mut r, 8)? as usize;
            if f >= floors.len() || res >= residues.len() {
                return Err(bad());
            }
            sm.push((f, res));
        }
        mappings.push(Mapping { coupling, mux, submaps: sm });
    }
    let mut modes = Vec::new();
    for _ in 0..rd(&mut r, 6)? + 1 {
        let long = rd(&mut r, 1)? != 0;
        rd(&mut r, 16)?;
        rd(&mut r, 16)?;
        let mapping = rd(&mut r, 8)? as usize;
        if mapping >= mappings.len() {
            return Err(bad());
        }
        modes.push(Mode { long, mapping });
    }
    if !rd(&mut r, 1).is_ok_and(|b| b == 1) {
        return Err(bad());
    }
    Ok(Setup { channels, blocksize, books, floors, residues, mappings, modes })
}

/// A decoded floor: `None` when the channel is unused in this packet.
fn decode_floor(f: &Floor, r: &mut LsbReader, books: &[Codebook], n2: usize) -> Option<Vec<f32>> {
    match f {
        Floor::One(f) => {
            if !r.bit()? {
                return None;
            }
            let range = [256, 128, 86, 64][(f.multiplier - 1) as usize];
            let rb = ilog(range as u32 - 1);
            let mut ys = Vec::with_capacity(f.xs.len());
            ys.push(r.read(rb)? as i32);
            ys.push(r.read(rb)? as i32);
            for &class in &f.partition_class {
                let cbits = f.class_sub[class];
                let csub = (1 << cbits) - 1;
                let mut cval = if cbits > 0 { books[f.class_master[class]].decode(r)? } else { 0 };
                for _ in 0..f.class_dim[class] {
                    let b = f.sub_books[class][(cval & csub) as usize];
                    cval >>= cbits;
                    ys.push(if b >= 0 { books[b as usize].decode(r)? as i32 } else { 0 });
                }
            }
            Some(floor1_curve(f, &ys, range, n2))
        }
        Floor::Zero { order, rate, bark_size, amp_bits, amp_offset, books: fb } => {
            let amplitude = r.read(*amp_bits)?;
            if amplitude == 0 {
                return None;
            }
            let bn = r.read(ilog(fb.len() as u32))? as usize;
            let book = books.get(*fb.get(bn)?)?;
            let mut coefs = Vec::with_capacity(*order);
            let mut last = 0.0;
            while coefs.len() < *order {
                let e = book.decode(r)?;
                let v = book.vector(e);
                if v.is_empty() {
                    return None;
                }
                for &x in v {
                    coefs.push(x + last);
                }
                last = *coefs.last().unwrap();
            }
            coefs.truncate(*order);
            Some(floor0_curve(&coefs, amplitude, *amp_bits, *amp_offset, *rate, *bark_size, n2))
        }
    }
}

fn render_point(x0: i32, y0: i32, x1: i32, y1: i32, x: i32) -> i32 {
    let dy = y1 - y0;
    let adx = x1 - x0;
    let off = dy.abs() * (x - x0) / adx;
    if dy < 0 {
        y0 - off
    } else {
        y0 + off
    }
}

fn render_line(x0: i32, y0: i32, x1: i32, y1: i32, v: &mut [f32]) {
    let dy = y1 - y0;
    let adx = x1 - x0;
    let base = dy / adx;
    let sy = if dy < 0 { base - 1 } else { base + 1 };
    let ady = dy.abs() - base.abs() * adx;
    let (mut y, mut err) = (y0, 0);
    let n = v.len() as i32;
    if x0 < n {
        v[x0 as usize] = FLOOR1_INVERSE_DB[y.clamp(0, 255) as usize];
    }
    for x in x0 + 1..x1.min(n) {
        err += ady;
        if err >= adx {
            err -= adx;
            y += sy;
        } else {
            y += base;
        }
        v[x as usize] = FLOOR1_INVERSE_DB[y.clamp(0, 255) as usize];
    }
}

fn floor1_curve(f: &Floor1, ys: &[i32], range: i32, n2: usize) -> Vec<f32> {
    let count = f.xs.len();
    let mut step2 = vec![false; count];
    let mut fy = vec![0i32; count];
    step2[0] = true;
    step2[1] = true;
    fy[0] = ys[0];
    fy[1] = ys[1];
    for i in 2..count {
        let (lo, hi) = f.neighbours[i];
        let predicted = render_point(f.xs[lo], fy[lo], f.xs[hi], fy[hi], f.xs[i]);
        let val = ys[i];
        let highroom = range - predicted;
        let lowroom = predicted;
        let room = if highroom < lowroom { highroom * 2 } else { lowroom * 2 };
        if val != 0 {
            step2[lo] = true;
            step2[hi] = true;
            step2[i] = true;
            fy[i] = if val >= room {
                if highroom > lowroom {
                    val - lowroom + predicted
                } else {
                    predicted - val + highroom - 1
                }
            } else if val & 1 == 1 {
                predicted - (val + 1) / 2
            } else {
                predicted + val / 2
            };
        } else {
            fy[i] = predicted;
        }
    }
    let mut v = vec![0.0f32; n2];
    let first = f.sorted[0];
    let (mut lx, mut ly) = (0, fy[first].clamp(0, range - 1) * f.multiplier);
    let mut hx = 0;
    for &i in &f.sorted[1..] {
        if step2[i] {
            let hy = fy[i].clamp(0, range - 1) * f.multiplier;
            hx = f.xs[i];
            render_line(lx, ly, hx, hy, &mut v);
            lx = hx;
            ly = hy;
        }
    }
    if (hx as usize) < n2 {
        let fill = FLOOR1_INVERSE_DB[ly.clamp(0, 255) as usize];
        for x in v.iter_mut().skip(hx.max(0) as usize) {
            *x = fill;
        }
    }
    v
}

fn bark(x: f64) -> f64 {
    13.1 * libm::atan(0.00074 * x) + 2.24 * libm::atan(0.0000000185 * x * x) + 0.0001 * x
}

fn floor0_curve(
    coefs: &[f32],
    amplitude: u32,
    amp_bits: u32,
    amp_offset: u32,
    rate: u32,
    bark_size: u32,
    n: usize,
) -> Vec<f32> {
    let map: Vec<i64> = (0..n)
        .map(|i| {
            let m = libm::floor(
                bark(rate as f64 * i as f64 / (2.0 * n as f64)) * bark_size as f64 / bark(0.5 * rate as f64),
            ) as i64;
            m.min(bark_size as i64 - 1)
        })
        .collect();
    let cos_c: Vec<f64> = coefs.iter().map(|&c| libm::cos(c as f64)).collect();
    let order = coefs.len();
    let mut out = vec![0.0f32; n];
    let mut i = 0;
    while i < n {
        let omega = core::f64::consts::PI * map[i] as f64 / bark_size as f64;
        let cw = libm::cos(omega);
        let (mut p, mut q);
        if order % 2 == 1 {
            p = 1.0 - cw * cw;
            q = 0.25;
            for j in 0..(order - 1) / 2 {
                p *= 4.0 * (cos_c[2 * j + 1] - cw) * (cos_c[2 * j + 1] - cw);
            }
            for j in 0..(order + 1) / 2 {
                q *= 4.0 * (cos_c[2 * j] - cw) * (cos_c[2 * j] - cw);
            }
        } else {
            p = (1.0 - cw) / 2.0;
            q = (1.0 + cw) / 2.0;
            for j in 0..order / 2 {
                p *= 4.0 * (cos_c[2 * j + 1] - cw) * (cos_c[2 * j + 1] - cw);
                q *= 4.0 * (cos_c[2 * j] - cw) * (cos_c[2 * j] - cw);
            }
        }
        let linear = libm::exp(
            0.11512925
                * (amplitude as f64 * amp_offset as f64 / (((1u64 << amp_bits) - 1) as f64 * libm::sqrt(p + q))
                    - amp_offset as f64),
        ) as f32;
        let m = map[i];
        while i < n && map[i] == m {
            out[i] = linear;
            i += 1;
        }
    }
    out
}

/// Decodes one residue into `vecs` (one per channel of the submap).
fn decode_residue(
    res: &Residue,
    r: &mut LsbReader,
    books: &[Codebook],
    vecs: &mut [Vec<f32>],
    skip: &[bool],
    n2: usize,
) {
    let ch = vecs.len();
    if res.kind == 2 {
        if skip.iter().all(|&s| s) {
            return;
        }
        let mut big = vec![vec![0.0f32; n2 * ch]];
        residue_partitions(res, r, books, &mut big, &[false], n2 * ch, 1);
        for (i, x) in big[0].iter().enumerate() {
            vecs[i % ch][i / ch] = *x;
        }
    } else {
        residue_partitions(res, r, books, vecs, skip, n2, res.kind);
    }
}

fn residue_partitions(
    res: &Residue,
    r: &mut LsbReader,
    books: &[Codebook],
    vecs: &mut [Vec<f32>],
    skip: &[bool],
    size: usize,
    format: u32,
) {
    let begin = res.begin.min(size);
    let end = res.end.min(size);
    let to_read = end - begin;
    if to_read == 0 {
        return;
    }
    let partitions = to_read / res.psize;
    let classbook = &books[res.classbook];
    let per_word = classbook.dim.max(1);
    let mut classes = vec![vec![0usize; partitions + per_word]; vecs.len()];
    for pass in 0..8 {
        let mut p = 0;
        while p < partitions {
            if pass == 0 {
                for (c, cls) in classes.iter_mut().enumerate() {
                    if skip[c] {
                        continue;
                    }
                    let Some(mut temp) = classbook.decode(r) else { return };
                    for i in (0..per_word).rev() {
                        cls[p + i] = temp as usize % res.classes;
                        temp /= res.classes as u32;
                    }
                }
            }
            for _ in 0..per_word {
                if p >= partitions {
                    break;
                }
                for c in 0..vecs.len() {
                    if skip[c] {
                        continue;
                    }
                    let b = res.books[classes[c][p]][pass];
                    if b < 0 {
                        continue;
                    }
                    let book = &books[b as usize];
                    let off = begin + p * res.psize;
                    let v = &mut vecs[c];
                    if book.dim == 0 {
                        continue;
                    }
                    if format == 0 {
                        let step = res.psize / book.dim;
                        for i in 0..step {
                            let Some(e) = book.decode(r) else { return };
                            for (j, x) in book.vector(e).iter().enumerate() {
                                v[off + i + j * step] += x;
                            }
                        }
                    } else {
                        let mut i = 0;
                        while i < res.psize {
                            let Some(e) = book.decode(r) else { return };
                            for x in book.vector(e) {
                                if i < res.psize {
                                    v[off + i] += x;
                                }
                                i += 1;
                            }
                        }
                    }
                }
                p += 1;
            }
        }
    }
}

/// In-place radix-2 FFT of `re`/`im` (length a power of two),
/// exp(-2 pi i jk / n).
fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * core::f64::consts::PI / len as f64;
        let (wr, wi) = (libm::cos(ang), libm::sin(ang));
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0f64, 0.0f64);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let tr = re[b] * cr as f32 - im[b] * ci as f32;
                let ti = re[b] * ci as f32 + im[b] * cr as f32;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let nr = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = nr;
            }
        }
        len <<= 1;
    }
}

/// The inverse MDCT of the Vorbis specification:
/// y[n] = sum X[k] cos(2 pi / N (n + 1/2 + N/4)(k + 1/2)), n < N = 2 X.len().
pub fn imdct(x: &[f32]) -> Vec<f32> {
    let m = x.len();
    let h = m / 2;
    // DCT-IV of size m through a complex FFT of size m / 2.
    let mut re = vec![0.0f32; h];
    let mut im = vec![0.0f32; h];
    for k in 0..h {
        let (a, b) = (x[2 * k], x[m - 1 - 2 * k]);
        let ang = -core::f64::consts::PI * (4 * k + 1) as f64 / (4 * m) as f64;
        let (c, s) = (libm::cos(ang) as f32, libm::sin(ang) as f32);
        re[k] = a * c - b * s;
        im[k] = a * s + b * c;
    }
    fft(&mut re, &mut im);
    let mut u = vec![0.0f32; m];
    for j in 0..h {
        let ang = -core::f64::consts::PI * j as f64 / m as f64;
        let (c, s) = (libm::cos(ang) as f32, libm::sin(ang) as f32);
        let tr = re[j] * c - im[j] * s;
        let ti = re[j] * s + im[j] * c;
        u[2 * j] = tr;
        u[m - 1 - 2 * j] = -ti;
    }
    let mut y = vec![0.0f32; 2 * m];
    for (n, out) in y.iter_mut().enumerate() {
        let k = n + m / 2;
        *out = if k < m {
            u[k]
        } else if k < 2 * m {
            -u[2 * m - 1 - k]
        } else {
            -u[k - 2 * m]
        };
    }
    y
}

/// Vorbis channel order to the WAVE order (FL FR FC LFE BL BR SL SR).
fn wave_order(ch: usize) -> &'static [usize] {
    match ch {
        3 => &[0, 2, 1],
        5 => &[0, 2, 1, 3, 4],
        6 => &[0, 2, 1, 5, 3, 4],
        7 => &[0, 2, 1, 6, 5, 3, 4],
        8 => &[0, 2, 1, 7, 5, 6, 3, 4],
        _ => &[],
    }
}

pub struct VorbisDecoder {
    data: Vec<u8>,
    info: Info,
    setup: Setup,
    packets: Packets,
    /// Page index of the first audio packet.
    first_audio_page: usize,
    /// Windowed output of the previous packet and its block size.
    prev: Option<(Vec<f32>, usize, bool)>,
    /// Decoded samples not handed out yet, and the stream position of the
    /// first of them when known.
    pending: Vec<f32>,
    pos: Option<u64>,
    /// The next sample to hand out.
    want: u64,
    /// Total length in samples (the last granule position).
    total: u64,
    /// Windows by block size and the sizes of the neighbouring blocks.
    window_cache: Vec<(usize, bool, bool, bool, Vec<f32>)>,
}

impl VorbisDecoder {
    pub fn new(data: Vec<u8>, serial: u32) -> Result<VorbisDecoder, String> {
        let mut packets = Packets::new(&data, serial);
        let id = packets.next(&data).ok_or("Vorbis stream without headers")?;
        let d = &id.data;
        if d.len() < 30 || d[0] != 1 || &d[1..7] != b"vorbis" || u32::from_le_bytes(d[7..11].try_into().unwrap()) != 0 {
            return Err(String::from("unsupported Vorbis identification header"));
        }
        let channels = d[11] as usize;
        let rate = u32::from_le_bytes(d[12..16].try_into().unwrap());
        let bs = [1usize << (d[28] & 15), 1usize << (d[28] >> 4)];
        if channels == 0 || rate == 0 || bs[0] < 64 || bs[1] < bs[0] || bs[1] > 8192 {
            return Err(String::from("invalid Vorbis identification header"));
        }
        let comment = packets.next(&data).ok_or("Vorbis stream without comment header")?;
        if comment.data.first() != Some(&3) {
            return Err(String::from("Vorbis comment header missing"));
        }
        let setup_packet = packets.next(&data).ok_or("Vorbis stream without setup header")?;
        let setup = read_setup(&setup_packet.data, channels, bs)?;
        let first_audio_page = setup_packet.page + 1;
        let total = packets.pages.iter().rev().find(|p| p.granule >= 0).map_or(0, |p| p.granule as u64);
        let mut dec = VorbisDecoder {
            info: Info { rate, channels, frames: Some(total), codec: String::from("Ogg Vorbis") },
            data,
            setup,
            packets,
            first_audio_page,
            prev: None,
            pending: Vec::new(),
            pos: None,
            want: 0,
            total,
            window_cache: Vec::new(),
        };
        dec.restart();
        Ok(dec)
    }

    /// Starts over at the first audio packet.
    fn restart(&mut self) {
        self.packets.seek_page(&self.data, self.first_audio_page);
        self.prev = None;
        self.pending.clear();
        self.pos = None;
        self.want = 0;
    }

    fn window(&mut self, n: usize, long: bool, prev_long: bool, next_long: bool) -> Vec<f32> {
        if let Some(w) =
            self.window_cache.iter().find(|w| w.0 == n && w.1 == long && w.2 == prev_long && w.3 == next_long)
        {
            return w.4.clone();
        }
        let bs0 = self.setup.blocksize[0];
        let (ls, le) = if long && !prev_long { (n / 4 - bs0 / 4, n / 4 + bs0 / 4) } else { (0, n / 2) };
        let (rs, re) = if long && !next_long { (3 * n / 4 - bs0 / 4, 3 * n / 4 + bs0 / 4) } else { (n / 2, n) };
        let slope = |i: usize, len: usize| {
            let x = libm::sinf((i as f32 + 0.5) / len as f32 * PI / 2.0);
            libm::sinf(PI / 2.0 * x * x)
        };
        let mut w = vec![0.0f32; n];
        for (i, v) in w.iter_mut().enumerate() {
            *v = if i < ls {
                0.0
            } else if i < le {
                slope(i - ls, le - ls)
            } else if i < rs {
                1.0
            } else if i < re {
                slope(re - 1 - i, re - rs)
            } else {
                0.0
            };
        }
        self.window_cache.push((n, long, prev_long, next_long, w.clone()));
        w
    }

    /// Decodes one audio packet; returns the finished samples (interleaved).
    fn decode_packet(&mut self, packet: &[u8]) -> Vec<f32> {
        let s = &self.setup;
        let ch = s.channels;
        let mut r = LsbReader::new(packet);
        if r.read(1) != Some(0) {
            return Vec::new();
        }
        let Some(mode_n) = r.read(ilog(s.modes.len() as u32 - 1)) else { return Vec::new() };
        let Some(mode) = s.modes.get(mode_n as usize) else { return Vec::new() };
        let long = mode.long;
        let n = s.blocksize[long as usize];
        let (prev_long, next_long) =
            if long { (r.bit().unwrap_or(false), r.bit().unwrap_or(false)) } else { (false, false) };
        let map = &s.mappings[mode.mapping];
        let n2 = n / 2;
        // Floors.
        let mut floors: Vec<Option<Vec<f32>>> = Vec::with_capacity(ch);
        for c in 0..ch {
            let (f, _) = map.submaps[map.mux[c]];
            floors.push(decode_floor(&s.floors[f], &mut r, &s.books, n2));
        }
        let mut no_residue: Vec<bool> = floors.iter().map(|f| f.is_none()).collect();
        for &(m, a) in &map.coupling {
            if !no_residue[m] || !no_residue[a] {
                no_residue[m] = false;
                no_residue[a] = false;
            }
        }
        // Residues, per submap.
        let mut spectra = vec![vec![0.0f32; n2]; ch];
        for (i, &(_, res)) in map.submaps.iter().enumerate() {
            let chans: Vec<usize> = (0..ch).filter(|&c| map.mux[c] == i).collect();
            let mut vecs: Vec<Vec<f32>> = chans.iter().map(|_| vec![0.0; n2]).collect();
            let skip: Vec<bool> = chans.iter().map(|&c| no_residue[c]).collect();
            decode_residue(&s.residues[res], &mut r, &s.books, &mut vecs, &skip, n2);
            for (k, &c) in chans.iter().enumerate() {
                spectra[c] = core::mem::take(&mut vecs[k]);
            }
        }
        // Inverse coupling, last step first.
        for &(m, a) in map.coupling.iter().rev() {
            for i in 0..n2 {
                let (mv, av) = (spectra[m][i], spectra[a][i]);
                let (nm, na) = if mv > 0.0 {
                    if av > 0.0 {
                        (mv, mv - av)
                    } else {
                        (mv + av, mv)
                    }
                } else if av > 0.0 {
                    (mv, mv + av)
                } else {
                    (mv - av, mv)
                };
                spectra[m][i] = nm;
                spectra[a][i] = na;
            }
        }
        let window = self.window(n, long, prev_long, next_long);
        let mut cur = vec![vec![0.0f32; n]; ch];
        for c in 0..ch {
            let Some(fl) = &floors[c] else { continue };
            let spec: Vec<f32> = spectra[c].iter().zip(fl).map(|(a, b)| a * b).collect();
            let t = imdct(&spec);
            for i in 0..n {
                cur[c][i] = t[i] * window[i];
            }
        }
        // Overlap-add: from the centre of the previous window to the
        // centre of this one.
        let mut out = Vec::new();
        if let Some((prev, pn, _)) = self.prev.take() {
            // The previous block starts at 0, this one at `c0` (negative
            // after a short block followed by a long one).
            let c0 = (3 * pn / 4) as isize - (n / 4) as isize;
            let (from, to) = ((pn / 2) as isize, c0 + (n / 2) as isize);
            out.reserve((to - from).max(0) as usize * ch);
            for t in from..to {
                for c in 0..ch {
                    let mut v = 0.0;
                    if t < pn as isize {
                        v += prev[c * pn + t as usize];
                    }
                    if t >= c0 {
                        v += cur[c][(t - c0) as usize];
                    }
                    out.push(v);
                }
            }
        }
        let mut flat = Vec::with_capacity(ch * n);
        for c in cur.iter() {
            flat.extend_from_slice(c);
        }
        self.prev = Some((flat, n, long));
        let order = wave_order(ch);
        if !order.is_empty() {
            for f in out.chunks_exact_mut(ch) {
                let src: Vec<f32> = f.to_vec();
                for (dst, &from) in order.iter().enumerate() {
                    f[dst] = src[from];
                }
            }
        }
        out
    }

    /// Fills `pending` with the next decoded samples, keeping `pos` (the
    /// stream position of `pending[0]`) up to date. False at the end.
    fn fill(&mut self) -> bool {
        let ch = self.setup.channels;
        loop {
            let Some(p) = self.packets.next(&self.data) else { return false };
            if p.data.first().is_some_and(|b| b & 1 == 1) {
                // A header packet in the audio (chained streams): skip.
                continue;
            }
            let out = self.decode_packet(&p.data);
            self.pending.extend_from_slice(&out);
            if let Some(g) = p.granule {
                // The samples handed out so far end at `g`.
                let have = (self.pending.len() / ch) as u64;
                let g = g as u64;
                if self.pos.is_none() {
                    // Positions become known: on the last page the
                    // granule may cut the final packet short.
                    self.pos = Some(g.saturating_sub(have));
                    if g < have && !self.packets.pages.get(p.page).is_some_and(|pg| pg.last) {
                        // A stream that starts mid-way: drop the excess.
                        let extra = (have - g) as usize * ch;
                        self.pending.drain(..extra.min(self.pending.len()));
                        self.pos = Some(0);
                    }
                }
                let start = self.pos.unwrap();
                if start + have > g {
                    // The last page ends the stream before the packet does.
                    let keep = g.saturating_sub(start) as usize * ch;
                    self.pending.truncate(keep);
                }
            }
            if self.pos.is_some() && !self.pending.is_empty() {
                return true;
            }
        }
    }
}

impl Decoder for VorbisDecoder {
    fn info(&self) -> &Info {
        &self.info
    }

    fn next_block(&mut self) -> Result<Option<Vec<f32>>, String> {
        let ch = self.setup.channels;
        loop {
            if self.pos.is_some() && !self.pending.is_empty() {
                let pos = self.pos.unwrap();
                let frames = (self.pending.len() / ch) as u64;
                let block: Vec<f32> = core::mem::take(&mut self.pending);
                self.pos = Some(pos + frames);
                // Drop what lies before the sample wanted (after a seek).
                if pos + frames <= self.want {
                    continue;
                }
                let skip = self.want.saturating_sub(pos) as usize * ch;
                let end = (self.total.saturating_sub(pos) as usize * ch).min(block.len());
                self.want = pos + frames;
                if skip >= end {
                    continue;
                }
                return Ok(Some(block[skip..end].to_vec()));
            }
            if !self.fill() {
                return Ok(None);
            }
        }
    }

    fn seek(&mut self, frame: u64) -> Result<u64, String> {
        let frame = frame.min(self.total);
        // Start a little before the target: the first packet decoded only
        // primes the overlap, and positions are known once a page ends.
        let back = (2 * self.setup.blocksize[1]) as u64;
        let pages = &self.packets.pages;
        let mut start = self.first_audio_page;
        if frame > back {
            let goal = frame - back;
            let (mut lo, mut hi) = (self.first_audio_page, pages.len());
            while lo < hi {
                let mid = (lo + hi) / 2;
                // The last page at or before `mid` with a granule position.
                let g = pages[..=mid].iter().rev().find(|p| p.granule >= 0).map_or(0, |p| p.granule as u64);
                if g <= goal {
                    lo = mid + 1;
                } else {
                    hi = mid;
                }
            }
            start = lo.saturating_sub(1).max(self.first_audio_page);
            // Begin after a page with a granule position.
            while start > self.first_audio_page && pages[start - 1].granule < 0 {
                start -= 1;
            }
        }
        self.packets.seek_page(&self.data, start);
        self.prev = None;
        self.pending.clear();
        self.pos = None;
        if start == self.first_audio_page {
            self.restart();
        }
        self.want = frame;
        Ok(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imdct_matches_the_definition() {
        for m in [32usize, 128] {
            let x: Vec<f32> = (0..m).map(|i| libm::sinf(i as f32 * 0.37) + (i % 5) as f32 * 0.1).collect();
            let fast = imdct(&x);
            let n = 2 * m;
            for (j, &y) in fast.iter().enumerate() {
                let mut s = 0.0f64;
                for (k, &v) in x.iter().enumerate() {
                    s += v as f64
                        * libm::cos(
                            2.0 * core::f64::consts::PI / n as f64
                                * (j as f64 + 0.5 + n as f64 / 4.0)
                                * (k as f64 + 0.5),
                        );
                }
                assert!((s - y as f64).abs() < 1e-3, "m {m} n {j}: {s} vs {y}");
            }
        }
    }

    #[test]
    fn helpers() {
        assert_eq!(ilog(0), 0);
        assert_eq!(ilog(7), 3);
        assert_eq!(lookup1_values(81, 4), 3);
        assert_eq!(lookup1_values(80, 4), 2);
        assert_eq!(float32_unpack(0x6080_0000 | 1), libm::exp2f(-16.0));
        assert_eq!(float32_unpack(0xE080_0003), -3.0 * libm::exp2f(-16.0));
    }
}
