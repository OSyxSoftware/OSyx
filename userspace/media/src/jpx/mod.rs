//! JPEG 2000 (ISO/IEC 15444-1): raw codestreams and JP2/JPX files, for the
//! picture viewer and PDF's JPXDecode. The JP2 boxes give the colour space
//! (enumerated, CIELab included, or by ICC profile class), palettes with
//! their component mapping, and channel definitions with opacity.

mod codestream;
mod dwt;
mod t1;

use alloc::vec;
use alloc::vec::Vec;

type R<T> = Result<T, &'static str>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Space {
    Gray,
    Rgb,
    Cmyk,
}

/// A decoded picture with 8-bit channels.
pub struct Image {
    pub w: usize,
    pub h: usize,
    pub space: Space,
    /// Colour channels, interleaved pixel by pixel.
    pub color: Vec<u8>,
    /// Colour channels per pixel.
    pub n: usize,
    /// Opacity, straight.
    pub alpha: Option<Vec<u8>>,
}

impl Image {
    pub fn to_rgba(&self) -> Vec<[u8; 4]> {
        let mut out = Vec::with_capacity(self.w * self.h);
        for i in 0..self.w * self.h {
            let c = &self.color[i * self.n..(i + 1) * self.n];
            let a = self.alpha.as_ref().map_or(255, |a| a[i]);
            let rgb = match self.space {
                Space::Gray => [c[0]; 3],
                Space::Rgb => [c[0], c[1], c[2]],
                Space::Cmyk => {
                    let k = 255 - c[3] as u32;
                    let f = |v: u8| ((255 - v as u32) * k / 255) as u8;
                    [f(c[0]), f(c[1]), f(c[2])]
                }
            };
            out.push([rgb[0], rgb[1], rgb[2], a]);
        }
        out
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Colr {
    None,
    Srgb,
    Gray,
    Sycc,
    Cmyk,
    /// Ranges and offsets of L*, a* and b* (RL, OL, RA, OA, RB, OB), when
    /// given.
    Lab(Option<[u32; 6]>),
}

struct Palette {
    /// Bit depth (with sign in bit 7) per column.
    depths: Vec<u8>,
    /// Entries, row by row.
    entries: Vec<i32>,
    columns: usize,
}

#[derive(Default)]
struct Header {
    colr: Option<Colr>,
    palette: Option<Palette>,
    /// (component, palette column or None).
    cmap: Vec<(usize, Option<usize>)>,
    /// (channel, type, association).
    cdef: Vec<(usize, u16, u16)>,
}

/// The boxes in `d`: (type, contents).
fn boxes(d: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    let mut p = 0;
    while p + 8 <= d.len() {
        let len = u32::from_be_bytes(d[p..p + 4].try_into().unwrap()) as u64;
        let kind: [u8; 4] = d[p + 4..p + 8].try_into().unwrap();
        let (start, end) = match len {
            0 => (p + 8, d.len()),
            1 => {
                let Some(l) = d.get(p + 8..p + 16) else { break };
                let l = u64::from_be_bytes(l.try_into().unwrap());
                (p + 16, (p as u64).saturating_add(l).min(d.len() as u64) as usize)
            }
            l => (p + 8, (p as u64 + l).min(d.len() as u64) as usize),
        };
        if end < start {
            break;
        }
        out.push((kind, &d[start..end]));
        p = end;
    }
    out
}

fn icc_space(profile: &[u8]) -> Colr {
    match profile.get(16..20) {
        Some(b"GRAY") => Colr::Gray,
        Some(b"CMYK") => Colr::Cmyk,
        Some(b"RGB ") => Colr::Srgb,
        _ => Colr::None,
    }
}

fn read_header(jp2h: &[u8], h: &mut Header) {
    for (kind, c) in boxes(jp2h) {
        match &kind {
            b"colr" if h.colr.is_none() && !c.is_empty() => {
                h.colr = Some(match c[0] {
                    1 if c.len() >= 7 => match u32::from_be_bytes(c[3..7].try_into().unwrap()) {
                        16 => Colr::Srgb,
                        17 => Colr::Gray,
                        18 => Colr::Sycc,
                        12 => Colr::Cmyk,
                        14 => Colr::Lab(c.get(7..31).map(|e| {
                            let mut v = [0u32; 6];
                            for (i, x) in v.iter_mut().enumerate() {
                                *x = u32::from_be_bytes(e[4 * i..4 * i + 4].try_into().unwrap());
                            }
                            v
                        })),
                        _ => Colr::None,
                    },
                    2 | 3 => icc_space(c.get(3..).unwrap_or(&[])),
                    _ => Colr::None,
                });
            }
            b"pclr" if c.len() >= 3 => {
                let ne = u16::from_be_bytes([c[0], c[1]]) as usize;
                let npc = c[2] as usize;
                let Some(depths) = c.get(3..3 + npc) else { continue };
                let mut entries = Vec::with_capacity(ne * npc);
                let mut p = 3 + npc;
                'rows: for _ in 0..ne {
                    for &dpt in depths {
                        let bytes = ((dpt & 0x7F) as usize + 1).div_ceil(8);
                        let Some(b) = c.get(p..p + bytes) else { break 'rows };
                        let v = b.iter().fold(0i64, |v, &x| (v << 8) | x as i64);
                        entries.push(v as i32);
                        p += bytes;
                    }
                }
                h.palette = Some(Palette { depths: depths.to_vec(), entries, columns: npc });
            }
            b"cmap" => {
                h.cmap = c
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|e| (u16::from_be_bytes([e[0], e[1]]) as usize, (e[2] == 1).then_some(e[3] as usize)))
                    .collect();
            }
            b"cdef" if c.len() >= 2 => {
                h.cdef = c[2..]
                    .as_chunks::<6>()
                    .0
                    .iter()
                    .map(|e| {
                        (
                            u16::from_be_bytes([e[0], e[1]]) as usize,
                            u16::from_be_bytes([e[2], e[3]]),
                            u16::from_be_bytes([e[4], e[5]]),
                        )
                    })
                    .collect();
            }
            _ => {}
        }
    }
}

/// Scales a sample of `prec` bits to 8 bits.
fn to8(v: i32, prec: u32, signed: bool) -> u8 {
    let v = if signed { v + (1 << (prec - 1)) } else { v };
    let max = (1i64 << prec) - 1;
    let v = (v as i64).clamp(0, max);
    if prec >= 8 {
        (v >> (prec - 8)) as u8
    } else {
        (v * 255 / max) as u8
    }
}

/// Decodes a JP2 or JPX file or a raw codestream.
pub fn decode(data: &[u8]) -> R<Image> {
    let mut header = Header::default();
    let stream: &[u8] = if data.starts_with(&[0xFF, 0x4F, 0xFF, 0x51]) {
        data
    } else {
        let mut cs = None;
        for (kind, c) in boxes(data) {
            match &kind {
                b"jp2h" => read_header(c, &mut header),
                b"jp2c" if cs.is_none() => cs = Some(c),
                _ => {}
            }
        }
        cs.ok_or("no JPEG 2000 codestream")?
    };
    let cs = codestream::decode(stream)?;
    let (w, h) = (cs.x1 - cs.x0, cs.y1 - cs.y0);
    // Every channel at full size, 8 bits.
    let plane = |c: &codestream::Component, map: &dyn Fn(i32) -> u8| -> Vec<u8> {
        let mut out = vec![0u8; w * h];
        if c.w == 0 || c.h == 0 {
            return out;
        }
        let cols: Vec<usize> =
            (0..w).map(|x| ((cs.x0 + x) / c.dx).saturating_sub(c.x0).min(c.w.saturating_sub(1))).collect();
        for y in 0..h {
            let sy = ((cs.y0 + y) / c.dy).saturating_sub(c.y0).min(c.h.saturating_sub(1));
            let row = &c.data[sy * c.w..(sy + 1) * c.w];
            for (o, &sx) in out[y * w..(y + 1) * w].iter_mut().zip(&cols) {
                *o = map(row[sx]);
            }
        }
        out
    };
    let mut channels: Vec<Vec<u8>> = Vec::new();
    // The precision of each channel.
    let mut precs: Vec<u32> = Vec::new();
    match (&header.palette, header.cmap.is_empty()) {
        (Some(pal), false) => {
            for &(comp, col) in &header.cmap {
                let c = cs.comps.get(comp).ok_or("bad JPEG 2000 component mapping")?;
                precs.push(match col {
                    Some(col) if col < pal.columns => (pal.depths[col] & 0x7F) as u32 + 1,
                    _ => c.prec,
                });
                channels.push(match col {
                    Some(col) if col < pal.columns => {
                        let d = pal.depths[col];
                        let rows = pal.entries.len() / pal.columns.max(1);
                        plane(c, &|v| {
                            let i = (v.max(0) as usize).min(rows.saturating_sub(1));
                            let e = pal.entries.get(i * pal.columns + col).copied().unwrap_or(0);
                            to8(e, (d & 0x7F) as u32 + 1, d & 0x80 != 0)
                        })
                    }
                    _ => plane(c, &|v| to8(v, c.prec, c.signed)),
                });
            }
        }
        _ => {
            for c in &cs.comps {
                precs.push(c.prec);
                channels.push(plane(c, &|v| to8(v, c.prec, c.signed)));
            }
        }
    }
    let colr = header.colr.unwrap_or(Colr::None);
    let (space, ncolor) = match colr {
        Colr::Gray => (Space::Gray, 1),
        Colr::Srgb | Colr::Sycc | Colr::Lab(_) => (Space::Rgb, 3),
        Colr::Cmyk => (Space::Cmyk, 4),
        _ => match channels.len() {
            1 | 2 => (Space::Gray, 1),
            _ => (Space::Rgb, 3),
        },
    };
    // Channel definitions put colours in order and pick out opacity.
    let mut order: Vec<usize> = Vec::new();
    let mut alpha_ch = None;
    if header.cdef.is_empty() {
        order.extend(0..ncolor.min(channels.len()));
        if channels.len() > ncolor {
            alpha_ch = Some(ncolor);
        }
    } else {
        for assoc in 1..=ncolor as u16 {
            if let Some(&(ch, _, _)) = header.cdef.iter().find(|&&(_, t, a)| t == 0 && a == assoc) {
                order.push(ch);
            }
        }
        alpha_ch = header.cdef.iter().find(|&&(_, t, _)| t == 1 || t == 2).map(|&(ch, _, _)| ch);
    }
    if order.len() < ncolor || order.iter().chain(alpha_ch.iter()).any(|&c| c >= channels.len()) {
        return Err("JPEG 2000 channels do not match the colour space");
    }
    let mut color = vec![0u8; w * h * ncolor];
    for (k, &ch) in order.iter().enumerate() {
        for (i, &v) in channels[ch].iter().enumerate() {
            color[i * ncolor + k] = v;
        }
    }
    if colr == Colr::Sycc {
        for px in color.as_chunks_mut::<3>().0 {
            let (y, cb, cr) = (px[0] as f32, px[1] as f32 - 128.0, px[2] as f32 - 128.0);
            let c = |v: f32| libm::rintf(v).clamp(0.0, 255.0) as u8;
            px[0] = c(y + 1.402 * cr);
            px[1] = c(y - 0.344_136 * cb - 0.714_136 * cr);
            px[2] = c(y + 1.772 * cb);
        }
    }
    if let Colr::Lab(ep) = colr {
        let n: Vec<f32> = order.iter().map(|&ch| ((1u64 << precs[ch]) - 1) as f32).collect();
        // T.801 M.11.7.4 defaults.
        let ep = ep.unwrap_or_else(|| {
            let (pa, pb) = (precs[order[1]], precs[order[2]]);
            [100, 0, 170, 1 << (pa - 1), 200, (1 << pb.saturating_sub(2)) + (1 << pb.saturating_sub(3))]
        });
        for px in color.as_chunks_mut::<3>().0 {
            let rgb = lab_to_srgb(
                ep[0] as f32 * px[0] as f32 / 255.0 - ep[0] as f32 * ep[1] as f32 / n[0],
                ep[2] as f32 * (px[1] as f32 / 255.0 - ep[3] as f32 / n[1]),
                ep[4] as f32 * (px[2] as f32 / 255.0 - ep[5] as f32 / n[2]),
            );
            px.copy_from_slice(&rgb);
        }
    }
    let mut alpha = alpha_ch.map(|ch| channels[ch].clone());
    // Premultiplied opacity: divide it out.
    if let (Some(a), Some(&(_, 2, _))) = (alpha.as_mut(), header.cdef.iter().find(|&&(_, t, _)| t == 2)) {
        for (i, &av) in a.iter().enumerate() {
            if av > 0 {
                for v in &mut color[i * ncolor..(i + 1) * ncolor] {
                    *v = ((*v as u32 * 255 + av as u32 / 2) / av as u32).min(255) as u8;
                }
            }
        }
    }
    Ok(Image { w, h, space, color, n: ncolor, alpha: alpha.take() })
}

/// CIELab under D50 to sRGB, with Bradford adaptation to D65.
fn lab_to_srgb(l: f32, a: f32, b: f32) -> [u8; 3] {
    let fy = (l + 16.0) / 116.0;
    let (fx, fz) = (fy + a / 500.0, fy - b / 200.0);
    let inv = |t: f32| if t > 6.0 / 29.0 { t * t * t } else { 3.0 * (6.0f32 / 29.0) * (6.0 / 29.0) * (t - 4.0 / 29.0) };
    let (x, y, z) = (0.9642 * inv(fx), inv(fy), 0.8249 * inv(fz));
    let lin = [
        3.133_856 * x - 1.616_867 * y - 0.490_615 * z,
        -0.978_768 * x + 1.916_142 * y + 0.033_454 * z,
        0.071_945 * x - 0.228_991 * y + 1.405_243 * z,
    ];
    lin.map(|v| {
        let v = v.clamp(0.0, 1.0);
        let e = if v <= 0.003_130_8 { 12.92 * v } else { 1.055 * libm::powf(v, 1.0 / 2.4) - 0.055 };
        libm::rintf(e * 255.0) as u8
    })
}
