//! The JPEG 2000 codestream (T.800 Annexes A, B, E, G): marker segments,
//! tiles and tile-parts, resolutions, precincts and code-blocks, packet
//! headers in every progression order (with POC changes, SOP and EPH
//! markers, PPM and PPT packed headers), then dequantization, the inverse
//! wavelet transform, the component transforms and the DC level shift.

use super::dwt;
use super::t1::{self, Orient};
use alloc::vec;
use alloc::vec::Vec;

type R<T> = Result<T, &'static str>;

/// Largest image, in samples over all components.
const MAX_SAMPLES: u64 = 1 << 28;

/// A decoded component on its own sampling grid.
pub struct Component {
    /// Size and offset on the component grid (the image area divided by
    /// the subsampling factors).
    pub w: usize,
    pub h: usize,
    pub x0: usize,
    pub y0: usize,
    /// Subsampling factors.
    pub dx: usize,
    pub dy: usize,
    pub prec: u32,
    pub signed: bool,
    pub data: Vec<i32>,
}

pub struct Codestream {
    /// Image area on the reference grid.
    pub x0: usize,
    pub y0: usize,
    pub x1: usize,
    pub y1: usize,
    pub comps: Vec<Component>,
}

#[derive(Clone)]
struct CodComp {
    levels: u8,
    xcb: u8,
    ycb: u8,
    style: u8,
    reversible: bool,
    /// (PPx, PPy) per resolution; empty for 2^15.
    precincts: Vec<(u8, u8)>,
}

#[derive(Clone)]
struct Cod {
    sop: bool,
    eph: bool,
    order: u8,
    layers: u16,
    mct: bool,
    comp: CodComp,
}

#[derive(Clone)]
struct Quant {
    style: u8,
    guard: u8,
    /// (exponent, mantissa) per sub-band.
    steps: Vec<(u8, u16)>,
}

#[derive(Clone, Copy)]
struct Poc {
    rs: u8,
    cs: u16,
    lye: u16,
    re: u8,
    ce: u16,
    order: u8,
}

/// Coding parameters of the main header or of one tile.
#[derive(Clone, Default)]
struct Params {
    cod: Option<Cod>,
    coc: Vec<Option<CodComp>>,
    qcd: Option<Quant>,
    qcc: Vec<Option<Quant>>,
    rgn: Vec<Option<u8>>,
    poc: Vec<Poc>,
}

struct SizComp {
    prec: u32,
    signed: bool,
    dx: usize,
    dy: usize,
}

struct Siz {
    x1: usize,
    y1: usize,
    x0: usize,
    y0: usize,
    tw: usize,
    th: usize,
    tx0: usize,
    ty0: usize,
    comps: Vec<SizComp>,
}

#[derive(Default)]
struct TileData {
    params: Params,
    /// The tile-part bodies after SOD, in order.
    body: Vec<u8>,
    /// Packet headers from PPT or PPM.
    headers: Vec<u8>,
    packed: bool,
}

fn be16(d: &[u8], p: usize) -> R<u16> {
    Ok(u16::from_be_bytes(d.get(p..p + 2).ok_or("short JPEG 2000 marker")?.try_into().unwrap()))
}

fn be32(d: &[u8], p: usize) -> R<u32> {
    Ok(u32::from_be_bytes(d.get(p..p + 4).ok_or("short JPEG 2000 marker")?.try_into().unwrap()))
}

fn byte(d: &[u8], p: usize) -> R<u8> {
    d.get(p).copied().ok_or("short JPEG 2000 marker")
}

fn ceil_div(a: usize, b: usize) -> usize {
    a.div_ceil(b)
}

/// SPcod or SPcoc from `d[p..end]`.
fn cod_comp(d: &[u8], p: usize, end: usize, custom: bool) -> R<CodComp> {
    let levels = byte(d, p)?;
    let xcb = (byte(d, p + 1)? & 0xF) + 2;
    let ycb = (byte(d, p + 2)? & 0xF) + 2;
    if levels > 32 || xcb > 10 || ycb > 10 || xcb + ycb > 12 {
        return Err("bad JPEG 2000 coding style");
    }
    let style = byte(d, p + 3)?;
    let reversible = byte(d, p + 4)? == 1;
    let mut precincts = Vec::new();
    if custom {
        for i in 0..=levels as usize {
            let v = *d.get(p + 5 + i).filter(|_| p + 5 + i < end).ok_or("short JPEG 2000 precinct sizes")?;
            precincts.push((v & 0xF, v >> 4));
        }
    }
    Ok(CodComp { levels, xcb, ycb, style, reversible, precincts })
}

fn quant(d: &[u8], p: usize, end: usize) -> R<Quant> {
    let s = byte(d, p)?;
    let style = s & 0x1F;
    let guard = s >> 5;
    let mut steps = Vec::new();
    let mut q = p + 1;
    match style {
        0 => {
            while q < end {
                steps.push((byte(d, q)? >> 3, 0));
                q += 1;
            }
        }
        1 | 2 => {
            while q + 1 < end {
                let v = be16(d, q)?;
                steps.push(((v >> 11) as u8, v & 0x7FF));
                q += 2;
            }
        }
        _ => return Err("bad JPEG 2000 quantization style"),
    }
    if steps.is_empty() {
        return Err("JPEG 2000 quantization without step sizes");
    }
    Ok(Quant { style, guard, steps })
}

fn comp_index(d: &[u8], p: usize, ncomps: usize) -> R<(usize, usize)> {
    if ncomps < 257 {
        Ok((byte(d, p)? as usize, p + 1))
    } else {
        Ok((be16(d, p)? as usize, p + 2))
    }
}

/// Reads a marker segment that may appear in the main or a tile-part
/// header into `params`.
fn coding_marker(code: u16, d: &[u8], p: usize, end: usize, ncomps: usize, params: &mut Params) -> R<()> {
    fn set<T: Clone>(v: &mut Vec<Option<T>>, c: usize, x: T, ncomps: usize) {
        if c < ncomps {
            if v.len() < ncomps {
                v.resize(ncomps, None);
            }
            v[c] = Some(x);
        }
    }
    match code {
        0xFF52 => {
            let scod = byte(d, p)?;
            let comp = cod_comp(d, p + 5, end, scod & 1 != 0)?;
            params.cod = Some(Cod {
                sop: scod & 2 != 0,
                eph: scod & 4 != 0,
                order: byte(d, p + 1)?,
                layers: be16(d, p + 2)?,
                mct: byte(d, p + 4)? != 0,
                comp,
            });
        }
        0xFF53 => {
            let (c, q) = comp_index(d, p, ncomps)?;
            let scoc = byte(d, q)?;
            let cc = cod_comp(d, q + 1, end, scoc & 1 != 0)?;
            set(&mut params.coc, c, cc, ncomps);
        }
        0xFF5C => params.qcd = Some(quant(d, p, end)?),
        0xFF5D => {
            let (c, q) = comp_index(d, p, ncomps)?;
            let qc = quant(d, q, end)?;
            set(&mut params.qcc, c, qc, ncomps);
        }
        0xFF5E => {
            let (c, q) = comp_index(d, p, ncomps)?;
            if byte(d, q)? == 0 {
                let shift = byte(d, q + 1)?;
                set(&mut params.rgn, c, shift, ncomps);
            }
        }
        0xFF5F => {
            let wide = ncomps >= 257;
            let entry = if wide { 9 } else { 7 };
            let mut q = p;
            params.poc.clear();
            while q + entry <= end {
                let rs = byte(d, q)?;
                let (cs, q2) = if wide { (be16(d, q + 1)?, q + 3) } else { (byte(d, q + 1)? as u16, q + 2) };
                let lye = be16(d, q2)?;
                let re = byte(d, q2 + 2)?;
                let (ce, q3) = if wide { (be16(d, q2 + 3)?, q2 + 5) } else { (byte(d, q2 + 3)? as u16, q2 + 4) };
                let ce = if ce == 0 { 256 } else { ce };
                params.poc.push(Poc { rs, cs, lye, re, ce, order: byte(d, q3)? });
                q += entry;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Parses a codestream starting at SOC and decodes every tile.
pub fn decode(d: &[u8]) -> R<Codestream> {
    if be16(d, 0)? != 0xFF4F {
        return Err("not a JPEG 2000 codestream");
    }
    let mut p = 2;
    let mut siz: Option<Siz> = None;
    let mut main = Params::default();
    let mut ppm: Vec<u8> = Vec::new();
    // Main header.
    loop {
        let code = be16(d, p)?;
        if code == 0xFF90 {
            break;
        }
        let len = be16(d, p + 2)? as usize;
        let (s, end) = (p + 4, p + 2 + len);
        if len < 2 || end > d.len() {
            return Err("bad JPEG 2000 marker length");
        }
        match code {
            0xFF51 => {
                let n = be16(d, s + 34)? as usize;
                if n == 0 || n > 16384 {
                    return Err("bad JPEG 2000 component count");
                }
                let mut comps = Vec::with_capacity(n);
                for i in 0..n {
                    let q = s + 36 + 3 * i;
                    let ssiz = byte(d, q)?;
                    let (dx, dy) = (byte(d, q + 1)? as usize, byte(d, q + 2)? as usize);
                    if dx == 0 || dy == 0 || (ssiz & 0x7F) >= 30 {
                        return Err("bad JPEG 2000 component");
                    }
                    comps.push(SizComp { prec: (ssiz & 0x7F) as u32 + 1, signed: ssiz & 0x80 != 0, dx, dy });
                }
                let v = |i: usize| -> R<usize> { Ok(be32(d, s + 2 + 4 * i)? as usize) };
                let sz = Siz {
                    x1: v(0)?,
                    y1: v(1)?,
                    x0: v(2)?,
                    y0: v(3)?,
                    tw: v(4)?,
                    th: v(5)?,
                    tx0: v(6)?,
                    ty0: v(7)?,
                    comps,
                };
                if sz.x1 <= sz.x0 || sz.y1 <= sz.y0 || sz.tw == 0 || sz.th == 0 || sz.tx0 > sz.x0 || sz.ty0 > sz.y0 {
                    return Err("bad JPEG 2000 image size");
                }
                let area = (sz.x1 - sz.x0) as u64 * (sz.y1 - sz.y0) as u64;
                let samples: u64 = sz.comps.iter().map(|c| area / (c.dx * c.dy) as u64).sum();
                if samples > MAX_SAMPLES {
                    return Err("JPEG 2000 image too large");
                }
                siz = Some(sz);
            }
            0xFF60 => ppm.extend_from_slice(&d[s + 1..end]),
            _ => {
                let n = siz.as_ref().ok_or("JPEG 2000 marker before SIZ")?.comps.len();
                coding_marker(code, d, s, end, n, &mut main)?;
            }
        }
        p = end;
    }
    let siz = siz.ok_or("JPEG 2000 without SIZ")?;
    if main.cod.is_none() || main.qcd.is_none() {
        return Err("JPEG 2000 without COD or QCD");
    }
    let ntx = ceil_div(siz.x1 - siz.tx0, siz.tw);
    let nty = ceil_div(siz.y1 - siz.ty0, siz.th);
    if ntx * nty > 65535 {
        return Err("too many JPEG 2000 tiles");
    }
    let mut tiles: Vec<TileData> = (0..ntx * nty).map(|_| TileData::default()).collect();
    let mut ppm_pos = 0;
    // Tile-parts.
    while p + 12 <= d.len() && be16(d, p)? == 0xFF90 {
        let sot = p;
        let index = be16(d, p + 4)? as usize;
        let psot = be32(d, p + 6)? as usize;
        let end = if psot == 0 { d.len() } else { (sot + psot).min(d.len()) };
        let tile = tiles.get_mut(index).ok_or("bad JPEG 2000 tile index")?;
        p += 2 + be16(d, p + 2)? as usize;
        loop {
            let code = be16(d, p)?;
            if code == 0xFF93 {
                p += 2;
                break;
            }
            let len = be16(d, p + 2)? as usize;
            let (s, mend) = (p + 4, p + 2 + len);
            if len < 2 || mend > end {
                return Err("bad JPEG 2000 marker length");
            }
            if code == 0xFF61 {
                tile.headers.extend_from_slice(&d[s + 1..mend]);
                tile.packed = true;
            } else {
                coding_marker(code, d, s, mend, siz.comps.len(), &mut tile.params)?;
            }
            p = mend;
        }
        let body_end = if psot == 0 && d.ends_with(&[0xFF, 0xD9]) { d.len() - 2 } else { end };
        tile.body.extend_from_slice(&d[p.min(body_end)..body_end]);
        if !ppm.is_empty() {
            let n = be32(&ppm, ppm_pos)? as usize;
            let chunk = ppm.get(ppm_pos + 4..ppm_pos + 4 + n).ok_or("short JPEG 2000 PPM")?;
            tile.headers.extend_from_slice(chunk);
            tile.packed = true;
            ppm_pos += 4 + n;
        }
        p = end;
    }
    let mut comps: Vec<Component> = siz
        .comps
        .iter()
        .map(|c| {
            let (x0, y0) = (ceil_div(siz.x0, c.dx), ceil_div(siz.y0, c.dy));
            let (w, h) = (ceil_div(siz.x1, c.dx) - x0, ceil_div(siz.y1, c.dy) - y0);
            Component { w, h, x0, y0, dx: c.dx, dy: c.dy, prec: c.prec, signed: c.signed, data: vec![0; w * h] }
        })
        .collect();
    for (t, tile) in tiles.iter().enumerate() {
        let (tpx, tpy) = (t % ntx, t / ntx);
        let rect = (
            (siz.tx0 + tpx * siz.tw).max(siz.x0),
            (siz.ty0 + tpy * siz.th).max(siz.y0),
            (siz.tx0 + (tpx + 1) * siz.tw).min(siz.x1),
            (siz.ty0 + (tpy + 1) * siz.th).min(siz.y1),
        );
        decode_tile(&siz, &main, tile, rect, &mut comps)?;
    }
    Ok(Codestream { x0: siz.x0, y0: siz.y0, x1: siz.x1, y1: siz.y1, comps })
}

// ---------------------------------------------------------------------------
// Tile structure.

/// Inclusion and zero bit-plane tag trees (B.10.2).
struct TagTree {
    /// (offset, width) of each level, leaves first.
    levels: Vec<(usize, usize)>,
    value: Vec<u32>,
    low: Vec<u32>,
}

impl TagTree {
    fn new(w: usize, h: usize) -> TagTree {
        let mut levels = Vec::new();
        let (mut lw, mut lh, mut n) = (w.max(1), h.max(1), 0);
        loop {
            levels.push((n, lw));
            n += lw * lh;
            if lw == 1 && lh == 1 {
                break;
            }
            lw = lw.div_ceil(2);
            lh = lh.div_ceil(2);
        }
        TagTree { levels, value: vec![u32::MAX; n], low: vec![0; n] }
    }

    /// Whether leaf (x, y) is below `threshold`, reading bits as needed.
    fn decode(&mut self, x: usize, y: usize, threshold: u32, bits: &mut Bits) -> bool {
        let mut low = 0;
        let mut leaf = 0;
        for (l, &(off, lw)) in self.levels.iter().enumerate().rev() {
            let i = off + (y >> l) * lw + (x >> l);
            leaf = i;
            if low > self.low[i] {
                self.low[i] = low;
            } else {
                low = self.low[i];
            }
            while low < threshold && low < self.value[i] {
                if bits.bit() != 0 {
                    self.value[i] = low;
                } else {
                    low += 1;
                }
            }
            self.low[i] = low;
        }
        self.value[leaf] < threshold
    }
}

/// Packet header bits, with a stuffed zero after each 0xFF.
struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
    buf: u32,
    ct: u32,
}

impl<'a> Bits<'a> {
    fn new(d: &'a [u8], pos: usize) -> Bits<'a> {
        Bits { d, pos, buf: 0, ct: 0 }
    }

    fn byte_in(&mut self) {
        self.buf = (self.buf << 8) & 0xFFFF;
        self.ct = if self.buf == 0xFF00 { 7 } else { 8 };
        self.buf |= *self.d.get(self.pos).unwrap_or(&0) as u32;
        self.pos += 1;
    }

    fn bit(&mut self) -> u32 {
        if self.ct == 0 {
            self.byte_in();
        }
        self.ct -= 1;
        (self.buf >> self.ct) & 1
    }

    fn bits(&mut self, n: u32) -> u32 {
        (0..n).fold(0, |v, _| (v << 1) | self.bit())
    }

    fn align(&mut self) {
        if self.buf & 0xFF == 0xFF {
            self.byte_in();
        }
        self.ct = 0;
    }

    /// Skips a two-byte marker at the byte position, if there.
    fn skip_marker(&mut self, code: u8) {
        if self.d.get(self.pos) == Some(&0xFF) && self.d.get(self.pos + 1) == Some(&code) {
            self.pos += 2;
        }
    }
}

struct CodeBlock {
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
    included: bool,
    lblock: u32,
    zero: u32,
    passes: u32,
    seg_left: u32,
    segs: Vec<(Vec<u8>, u32)>,
}

struct Band {
    orient: Orient,
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
    xcb: u32,
    ycb: u32,
    /// Code-block grid: first index and count in each direction.
    cbx0: usize,
    cby0: usize,
    ncbx: usize,
    blocks: Vec<CodeBlock>,
    planes: u32,
    step: f32,
}

/// The code-blocks of one band inside one precinct.
struct PrecinctBand {
    cbx0: usize,
    cby0: usize,
    cbx1: usize,
    cby1: usize,
    incl: TagTree,
    zbp: TagTree,
}

struct Resolution {
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
    ppx: u32,
    ppy: u32,
    pw: usize,
    ph: usize,
    bands: Vec<Band>,
    /// Per precinct, per band.
    precincts: Vec<Vec<PrecinctBand>>,
}

struct TileComp {
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
    dx: usize,
    dy: usize,
    cod: CodComp,
    roi: u32,
    res: Vec<Resolution>,
}

fn band_rect(t: (usize, usize, usize, usize), nb: u32, xob: usize, yob: usize) -> (usize, usize, usize, usize) {
    if nb == 0 {
        return t;
    }
    let f = |a: usize, ob: usize| -> usize {
        let a = a as i64 - ((ob as i64) << (nb - 1));
        let s = 1i64 << nb;
        (a + s - 1).div_euclid(s).max(0) as usize
    };
    (f(t.0, xob), f(t.1, yob), f(t.2, xob), f(t.3, yob))
}

#[allow(clippy::too_many_arguments)]
fn build_tile_comp(rect: (usize, usize, usize, usize), sc: &SizComp, cod: CodComp, q: &Quant, roi: u32) -> R<TileComp> {
    let (tcx0, tcy0, tcx1, tcy1) =
        (ceil_div(rect.0, sc.dx), ceil_div(rect.1, sc.dy), ceil_div(rect.2, sc.dx), ceil_div(rect.3, sc.dy));
    let nl = cod.levels as u32;
    let mut res = Vec::with_capacity(nl as usize + 1);
    for r in 0..=nl {
        let s = 1usize << (nl - r);
        let (x0, y0, x1, y1) = (ceil_div(tcx0, s), ceil_div(tcy0, s), ceil_div(tcx1, s), ceil_div(tcy1, s));
        let (ppx, ppy) = cod.precincts.get(r as usize).map(|&(a, b)| (a as u32, b as u32)).unwrap_or((15, 15));
        if r > 0 && (ppx == 0 || ppy == 0) {
            return Err("bad JPEG 2000 precinct size");
        }
        let pw = if x1 > x0 { ceil_div(x1, 1 << ppx) - (x0 >> ppx) } else { 0 };
        let ph = if y1 > y0 { ceil_div(y1, 1 << ppy) - (y0 >> ppy) } else { 0 };
        if pw * ph > 1 << 20 {
            return Err("too many JPEG 2000 precincts");
        }
        let kinds: &[(Orient, usize, usize)] =
            if r == 0 { &[(Orient::LL, 0, 0)] } else { &[(Orient::HL, 1, 0), (Orient::LH, 0, 1), (Orient::HH, 1, 1)] };
        let nb = if r == 0 { nl } else { nl - r + 1 };
        let sub = (r > 0) as u32;
        let xcb = (cod.xcb as u32).min(ppx - sub);
        let ycb = (cod.ycb as u32).min(ppy - sub);
        let mut bands = Vec::new();
        for &(orient, xob, yob) in kinds {
            let (bx0, by0, bx1, by1) = band_rect((tcx0, tcy0, tcx1, tcy1), nb, xob, yob);
            let (cbx0, cby0) = (bx0 >> xcb, by0 >> ycb);
            let ncbx = if bx1 > bx0 { ceil_div(bx1, 1 << xcb) - cbx0 } else { 0 };
            let ncby = if by1 > by0 { ceil_div(by1, 1 << ycb) - cby0 } else { 0 };
            if ncbx * ncby > 1 << 22 {
                return Err("too many JPEG 2000 code-blocks");
            }
            let mut blocks = Vec::with_capacity(ncbx * ncby);
            for j in 0..ncby {
                for i in 0..ncbx {
                    let (cx, cy) = ((cbx0 + i) << xcb, (cby0 + j) << ycb);
                    blocks.push(CodeBlock {
                        x0: cx.max(bx0),
                        y0: cy.max(by0),
                        x1: (cx + (1 << xcb)).min(bx1),
                        y1: (cy + (1 << ycb)).min(by1),
                        included: false,
                        lblock: 3,
                        zero: 0,
                        passes: 0,
                        seg_left: 0,
                        segs: Vec::new(),
                    });
                }
            }
            // Quantization (E.1).
            let bi = if r == 0 { 0 } else { 1 + 3 * (r as usize - 1) + bands.len() };
            let (eps, mu) = match q.style {
                1 => {
                    let (e0, m0) = q.steps[0];
                    ((e0 as i32 - nl as i32 + nb as i32).max(0) as u32, m0)
                }
                _ => {
                    let (e, m) = *q.steps.get(bi).or(q.steps.last()).unwrap();
                    (e as u32, m)
                }
            };
            let planes = (q.guard as u32 + eps).saturating_sub(1) + roi;
            let gain = match orient {
                Orient::LL => 0,
                Orient::HL | Orient::LH => 1,
                Orient::HH => 2,
            };
            let step = libm::ldexpf(1.0 + mu as f32 / 2048.0, sc.prec as i32 + gain - eps as i32);
            bands.push(Band {
                orient,
                x0: bx0,
                y0: by0,
                x1: bx1,
                y1: by1,
                xcb,
                ycb,
                cbx0,
                cby0,
                ncbx,
                blocks,
                planes,
                step,
            });
        }
        let mut precincts = Vec::with_capacity(pw * ph);
        let ppx_b = ppx - sub;
        let ppy_b = ppy - sub;
        for py in 0..ph {
            for px in 0..pw {
                let (gx, gy) = ((x0 >> ppx) + px, (y0 >> ppy) + py);
                let pb = bands
                    .iter()
                    .map(|b| {
                        let (sx0, sy0) = ((gx << ppx_b).max(b.x0), (gy << ppy_b).max(b.y0));
                        let (sx1, sy1) = (((gx + 1) << ppx_b).min(b.x1), ((gy + 1) << ppy_b).min(b.y1));
                        let (c0, c1) = if sx1 > sx0 { (sx0 >> b.xcb, ceil_div(sx1, 1 << b.xcb)) } else { (0, 0) };
                        let (d0, d1) = if sy1 > sy0 { (sy0 >> b.ycb, ceil_div(sy1, 1 << b.ycb)) } else { (0, 0) };
                        let (w, h) = (c1 - c0, d1 - d0);
                        PrecinctBand {
                            cbx0: c0,
                            cby0: d0,
                            cbx1: c1,
                            cby1: d1,
                            incl: TagTree::new(w, h),
                            zbp: TagTree::new(w, h),
                        }
                    })
                    .collect();
                precincts.push(pb);
            }
        }
        res.push(Resolution { x0, y0, x1, y1, ppx, ppy, pw, ph, bands, precincts });
    }
    Ok(TileComp { x0: tcx0, y0: tcy0, x1: tcx1, y1: tcy1, dx: sc.dx, dy: sc.dy, cod, roi, res })
}

// ---------------------------------------------------------------------------
// Packets.

fn read_passes(b: &mut Bits) -> u32 {
    if b.bit() == 0 {
        return 1;
    }
    if b.bit() == 0 {
        return 2;
    }
    let v = b.bits(2);
    if v < 3 {
        return v + 3;
    }
    let v = b.bits(5);
    if v < 31 {
        return v + 6;
    }
    b.bits(7) + 37
}

/// One packet: its header from `hdr` and its body from `body` at `pos`.
#[allow(clippy::too_many_arguments)]
fn read_packet(
    tc: &mut TileComp,
    r: usize,
    p: usize,
    layer: u32,
    hdr: &mut Bits,
    inline: bool,
    body: &[u8],
    pos: &mut usize,
    eph: bool,
) {
    let style = tc.cod.style;
    let res = &mut tc.res[r];
    // (band, block, its (segment, passes, length) portions).
    type Portions = Vec<(usize, u32, usize)>;
    let mut items: Vec<(usize, usize, Portions)> = Vec::new();
    if hdr.bit() != 0 {
        for (bi, band) in res.bands.iter_mut().enumerate() {
            let Some(pb) = res.precincts.get_mut(p).and_then(|v| v.get_mut(bi)) else { continue };
            for cy in pb.cby0..pb.cby1 {
                for cx in pb.cbx0..pb.cbx1 {
                    let k = (cy - band.cby0) * band.ncbx + (cx - band.cbx0);
                    let Some(cb) = band.blocks.get_mut(k) else { continue };
                    let (lx, ly) = (cx - pb.cbx0, cy - pb.cby0);
                    let first = !cb.included;
                    let inc = if first { pb.incl.decode(lx, ly, layer + 1, hdr) } else { hdr.bit() != 0 };
                    if !inc {
                        continue;
                    }
                    if first {
                        let mut t = 1;
                        while !pb.zbp.decode(lx, ly, t, hdr) && t < 64 {
                            t += 1;
                        }
                        cb.zero = t - 1;
                        cb.included = true;
                    }
                    let mut n = read_passes(hdr);
                    while hdr.bit() != 0 && cb.lblock < 32 {
                        cb.lblock += 1;
                    }
                    let mut portions = Vec::new();
                    while n > 0 {
                        if cb.seg_left == 0 {
                            cb.segs.push((Vec::new(), 0));
                            cb.seg_left = t1::segment_len(cb.passes, style);
                        }
                        let m = n.min(cb.seg_left);
                        let bits = cb.lblock + (31 - m.leading_zeros());
                        let len = hdr.bits(bits.min(32)) as usize;
                        portions.push((cb.segs.len() - 1, m, len));
                        cb.seg_left -= m;
                        cb.passes += m;
                        n -= m;
                    }
                    items.push((bi, k, portions));
                }
            }
        }
    }
    hdr.align();
    if eph {
        hdr.skip_marker(0x92);
    }
    if inline {
        *pos = hdr.pos;
    }
    for (bi, k, portions) in items {
        let cb = &mut res.bands[bi].blocks[k];
        for (s, m, len) in portions {
            let end = (*pos + len).min(body.len());
            let chunk = &body[(*pos).min(end)..end];
            cb.segs[s].0.extend_from_slice(chunk);
            cb.segs[s].1 += m;
            *pos += len;
        }
    }
}

/// The packets of a tile in decoding order: (layer, resolution, component,
/// precinct), following the progression order and any POC changes.
fn packet_order(
    tcs: &[TileComp],
    rect: (usize, usize, usize, usize),
    cod: &Cod,
    pocs: &[Poc],
) -> Vec<(u32, usize, usize, usize)> {
    let nc = tcs.len();
    let max_res = tcs.iter().map(|t| t.res.len()).max().unwrap_or(0);
    let default = [Poc { rs: 0, cs: 0, lye: cod.layers, re: max_res as u8, ce: nc as u16, order: cod.order }];
    let pocs = if pocs.is_empty() { &default[..] } else { pocs };
    let mut next: Vec<Vec<Vec<u32>>> =
        tcs.iter().map(|t| t.res.iter().map(|r| vec![0; r.pw * r.ph]).collect()).collect();
    let mut out = Vec::new();
    let mut emit = |out: &mut Vec<_>, l: u32, r: usize, c: usize, p: usize| {
        if let Some(n) = next.get_mut(c).and_then(|v| v.get_mut(r)).and_then(|v| v.get_mut(p)) {
            if *n == l {
                *n += 1;
                out.push((l, r, c, p));
            }
        }
    };
    for poc in pocs {
        let (rs, re) = (poc.rs as usize, (poc.re as usize).min(max_res));
        let (cs, ce) = (poc.cs as usize, (poc.ce as usize).min(nc));
        let lye = (poc.lye as u32).min(cod.layers as u32);
        let np = |c: usize, r: usize| tcs[c].res.get(r).map_or(0, |x| x.pw * x.ph);
        match poc.order {
            0 => {
                for l in 0..lye {
                    for r in rs..re {
                        for c in cs..ce {
                            for p in 0..np(c, r) {
                                emit(&mut out, l, r, c, p);
                            }
                        }
                    }
                }
            }
            1 => {
                for r in rs..re {
                    for l in 0..lye {
                        for c in cs..ce {
                            for p in 0..np(c, r) {
                                emit(&mut out, l, r, c, p);
                            }
                        }
                    }
                }
            }
            order @ 2..=4 => {
                // Position-driven orders (B.12.1.3 to B.12.1.5).
                let mut sx = u64::MAX;
                let mut sy = u64::MAX;
                for t in &tcs[cs..ce] {
                    let nl = t.res.len() as u32 - 1;
                    for (r, res) in t.res.iter().enumerate() {
                        let lv = nl - r as u32;
                        sx = sx.min((t.dx as u64) << (res.ppx + lv).min(40));
                        sy = sy.min((t.dy as u64) << (res.ppy + lv).min(40));
                    }
                }
                if sx == u64::MAX {
                    continue;
                }
                let precinct = |c: usize, r: usize, x: u64, y: u64| -> Option<usize> {
                    let t = &tcs[c];
                    let res = t.res.get(r)?;
                    let lv = t.res.len() as u32 - 1 - r as u32;
                    if res.pw == 0 || res.ph == 0 || res.x0 == res.x1 || res.y0 == res.y1 {
                        return None;
                    }
                    let (rpx, rpy) = ((res.ppx + lv).min(40), (res.ppy + lv).min(40));
                    let (dx, dy) = (t.dx as u64, t.dy as u64);
                    let ok_y = y.is_multiple_of(dy << rpy)
                        || (y == rect.1 as u64 && !((res.y0 as u64) << lv).is_multiple_of(1 << rpy));
                    let ok_x = x.is_multiple_of(dx << rpx)
                        || (x == rect.0 as u64 && !((res.x0 as u64) << lv).is_multiple_of(1 << rpx));
                    if !ok_x || !ok_y {
                        return None;
                    }
                    let px = (x.div_ceil(dx << lv) >> res.ppx) as usize - (res.x0 >> res.ppx);
                    let py = (y.div_ceil(dy << lv) >> res.ppy) as usize - (res.y0 >> res.ppy);
                    (px < res.pw && py < res.ph).then_some(px + py * res.pw)
                };
                let positions = || {
                    let mut v = Vec::new();
                    let mut y = rect.1 as u64;
                    while y < rect.3 as u64 {
                        let mut x = rect.0 as u64;
                        while x < rect.2 as u64 {
                            v.push((x, y));
                            x += sx - x % sx;
                        }
                        y += sy - y % sy;
                    }
                    v
                };
                match order {
                    2 => {
                        for r in rs..re {
                            for &(x, y) in &positions() {
                                for c in cs..ce {
                                    if let Some(p) = precinct(c, r, x, y) {
                                        for l in 0..lye {
                                            emit(&mut out, l, r, c, p);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    3 => {
                        for &(x, y) in &positions() {
                            for c in cs..ce {
                                for r in rs..re {
                                    if let Some(p) = precinct(c, r, x, y) {
                                        for l in 0..lye {
                                            emit(&mut out, l, r, c, p);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    _ => {
                        for c in cs..ce {
                            for &(x, y) in &positions() {
                                for r in rs..re {
                                    if let Some(p) = precinct(c, r, x, y) {
                                        for l in 0..lye {
                                            emit(&mut out, l, r, c, p);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Reconstruction.

enum Samples {
    Int(Vec<i32>),
    Float(Vec<f32>),
}

impl Samples {
    fn to_float(&self) -> Vec<f32> {
        match self {
            Samples::Int(v) => v.iter().map(|&x| x as f32).collect(),
            Samples::Float(v) => v.clone(),
        }
    }
}

/// Tier-1 for every code-block, dequantization, and the inverse wavelet
/// transform of one tile-component.
fn reconstruct(tc: &TileComp) -> Samples {
    let reversible = tc.cod.reversible;
    let mut ll_i: Vec<i32> = Vec::new();
    let mut ll_f: Vec<f32> = Vec::new();
    for (r, res) in tc.res.iter().enumerate() {
        let mut bands_i: Vec<Vec<i32>> = Vec::new();
        let mut bands_f: Vec<Vec<f32>> = Vec::new();
        for band in &res.bands {
            let bw = band.x1 - band.x0;
            let mut vi = if reversible { vec![0i32; bw * (band.y1 - band.y0)] } else { Vec::new() };
            let mut vf = if reversible { Vec::new() } else { vec![0f32; bw * (band.y1 - band.y0)] };
            for cb in &band.blocks {
                if cb.segs.is_empty() || cb.x1 <= cb.x0 || cb.y1 <= cb.y0 {
                    continue;
                }
                let (w, h) = (cb.x1 - cb.x0, cb.y1 - cb.y0);
                let mut coef = t1::decode(w, h, band.orient, tc.cod.style, band.planes, cb.zero, &cb.segs);
                if tc.roi > 0 {
                    let thresh = 1i32 << tc.roi.min(30);
                    for v in coef.iter_mut() {
                        if v.abs() >= thresh {
                            *v = v.signum() * (v.abs() >> tc.roi);
                        }
                    }
                }
                for y in 0..h {
                    let row = (cb.y0 - band.y0 + y) * bw + (cb.x0 - band.x0);
                    for x in 0..w {
                        let s = coef[y * w + x];
                        if reversible {
                            vi[row + x] = s / 2;
                        } else {
                            vf[row + x] = s as f32 * band.step * 0.5;
                        }
                    }
                }
            }
            bands_i.push(vi);
            bands_f.push(vf);
        }
        if r == 0 {
            ll_i = core::mem::take(&mut bands_i[0]);
            ll_f = core::mem::take(&mut bands_f[0]);
            continue;
        }
        let (u0, u1, v0, v1) = (res.x0, res.x1, res.y0, res.y1);
        let (w, h) = (u1 - u0, v1 - v0);
        if reversible {
            let mut a = dwt::interleave(&ll_i, &bands_i[0], &bands_i[1], &bands_i[2], u0, u1, v0, v1);
            dwt::synth_int(&mut a, w, h, u0, v0);
            ll_i = a;
        } else {
            let mut a = dwt::interleave(&ll_f, &bands_f[0], &bands_f[1], &bands_f[2], u0, u1, v0, v1);
            dwt::synth_float(&mut a, w, h, u0, v0);
            ll_f = a;
        }
    }
    if reversible {
        Samples::Int(ll_i)
    } else {
        Samples::Float(ll_f)
    }
}

fn decode_tile(
    siz: &Siz,
    main: &Params,
    tile: &TileData,
    rect: (usize, usize, usize, usize),
    comps: &mut [Component],
) -> R<()> {
    let tp = &tile.params;
    let cod = tp.cod.as_ref().or(main.cod.as_ref()).unwrap();
    let nc = siz.comps.len();
    let mut tcs = Vec::with_capacity(nc);
    for (c, sc) in siz.comps.iter().enumerate() {
        let get = |v: &Vec<Option<CodComp>>| v.get(c).cloned().flatten();
        let cc = get(&tp.coc)
            .or_else(|| tp.cod.as_ref().map(|d| d.comp.clone()))
            .or_else(|| get(&main.coc))
            .unwrap_or_else(|| cod.comp.clone());
        let getq = |v: &Vec<Option<Quant>>| v.get(c).cloned().flatten();
        let q =
            getq(&tp.qcc).or_else(|| tp.qcd.clone()).or_else(|| getq(&main.qcc)).or_else(|| main.qcd.clone()).unwrap();
        let roi = tp.rgn.get(c).copied().flatten().or(main.rgn.get(c).copied().flatten()).unwrap_or(0) as u32;
        tcs.push(build_tile_comp(rect, sc, cc, &q, roi)?);
    }
    let pocs = if tp.poc.is_empty() { &main.poc } else { &tp.poc };
    let order = packet_order(&tcs, rect, cod, pocs);
    let body = &tile.body[..];
    let mut pos = 0usize;
    let mut packed = Bits::new(&tile.headers, 0);
    for (l, r, c, p) in order {
        if pos >= body.len() && !tile.packed {
            break;
        }
        if cod.sop && body.get(pos) == Some(&0xFF) && body.get(pos + 1) == Some(&0x91) {
            pos += 6;
        }
        if tile.packed {
            read_packet(&mut tcs[c], r, p, l, &mut packed, false, body, &mut pos, cod.eph);
        } else {
            let mut hdr = Bits::new(body, pos);
            read_packet(&mut tcs[c], r, p, l, &mut hdr, true, body, &mut pos, cod.eph);
        }
    }
    // Reconstruct each component.
    let mut samples: Vec<Samples> = tcs.iter().map(reconstruct).collect();
    // Inverse component transform (G.2, G.3).
    if cod.mct && nc >= 3 {
        let same = (1..3)
            .all(|i| tcs[i].x1 - tcs[i].x0 == tcs[0].x1 - tcs[0].x0 && tcs[i].y1 - tcs[i].y0 == tcs[0].y1 - tcs[0].y0);
        if same {
            match (&mut samples[..3], tcs[0].cod.reversible) {
                ([Samples::Int(y0), Samples::Int(y1), Samples::Int(y2)], true) => {
                    for i in 0..y0.len() {
                        let g = y0[i] - ((y1[i] + y2[i]) >> 2);
                        let (r, b) = (y2[i] + g, y1[i] + g);
                        (y0[i], y1[i], y2[i]) = (r, g, b);
                    }
                }
                (s, _) => {
                    let (mut a, mut b, mut c) = (s[0].to_float(), s[1].to_float(), s[2].to_float());
                    for i in 0..a.len() {
                        let (y, cb, cr) = (a[i], b[i], c[i]);
                        a[i] = y + 1.402 * cr;
                        b[i] = y - 0.344_136 * cb - 0.714_136 * cr;
                        c[i] = y + 1.772 * cb;
                    }
                    s[0] = Samples::Float(a);
                    s[1] = Samples::Float(b);
                    s[2] = Samples::Float(c);
                }
            }
        }
    }
    // DC level shift, clamping, and placement in the component.
    for (c, (tc, s)) in tcs.iter().zip(samples).enumerate() {
        let comp = &mut comps[c];
        let w = tc.x1 - tc.x0;
        let (lo, hi, shift) = if comp.signed {
            (-(1i64 << (comp.prec - 1)), (1i64 << (comp.prec - 1)) - 1, 0)
        } else {
            (0, (1i64 << comp.prec) - 1, 1i64 << (comp.prec - 1))
        };
        for y in tc.y0..tc.y1 {
            let Some(dy) = y.checked_sub(comp.y0).filter(|&v| v < comp.h) else { continue };
            for x in tc.x0..tc.x1 {
                let Some(dx) = x.checked_sub(comp.x0).filter(|&v| v < comp.w) else { continue };
                let i = (y - tc.y0) * w + (x - tc.x0);
                let v = match &s {
                    Samples::Int(v) => v.get(i).copied().unwrap_or(0) as i64,
                    Samples::Float(v) => libm::rintf(v.get(i).copied().unwrap_or(0.0)) as i64,
                };
                comp.data[dy * comp.w + dx] = (v + shift).clamp(lo, hi) as i32;
            }
        }
    }
    Ok(())
}
