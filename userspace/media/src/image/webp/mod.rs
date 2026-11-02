//! WebP (RFC 9649): lossy VP8 pictures with optional alpha (ALPH, raw or
//! lossless-compressed, with its spatial filters), lossless VP8L pictures,
//! and animations (ANIM/ANMF), composited the way libwebp's animation
//! decoder does.

pub mod vp8l;

use super::{check_size, Frame, Picture, MAX_FRAMES};
use crate::vp8;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

type R<T> = Result<T, String>;

fn le24(d: &[u8]) -> usize {
    d[0] as usize | (d[1] as usize) << 8 | (d[2] as usize) << 16
}

/// The RIFF chunks after the header: (fourcc, payload).
fn chunks(d: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    let mut p = 0;
    while p + 8 <= d.len() {
        let id: [u8; 4] = d[p..p + 4].try_into().unwrap();
        let len = u32::from_le_bytes(d[p + 4..p + 8].try_into().unwrap()) as usize;
        let end = (p + 8).saturating_add(len).min(d.len());
        out.push((id, &d[p + 8..end]));
        p = end + (len & 1);
    }
    out
}

/// One still image: VP8 or VP8L with any ALPH, as RGBA.
fn still(parts: &[([u8; 4], &[u8])]) -> R<(usize, usize, bool, Vec<[u8; 4]>)> {
    let alph = parts.iter().find(|(id, _)| id == b"ALPH").map(|(_, d)| *d);
    if let Some((_, d)) = parts.iter().find(|(id, _)| id == b"VP8L") {
        let (w, h, alpha, argb) = vp8l::decode(d).map_err(String::from)?;
        let px = argb.iter().map(|&p| [(p >> 16) as u8, (p >> 8) as u8, p as u8, (p >> 24) as u8]).collect();
        return Ok((w, h, alpha, px));
    }
    let (_, d) = parts.iter().find(|(id, _)| id == b"VP8 ").ok_or("WebP without picture data")?;
    let mut dec = vp8::Decoder::new();
    dec.decode(d).map_err(String::from)?;
    let f = dec.frame().ok_or("empty VP8 frame")?;
    let (w, h) = (f.w, f.h);
    check_size(w, h)?;
    let mut px = vp8::to_rgba(&f);
    if let Some(a) = alph {
        let alpha = decode_alpha(a, w, h)?;
        for (p, a) in px.iter_mut().zip(alpha) {
            p[3] = a;
        }
    }
    Ok((w, h, alph.is_some(), px))
}

/// An ALPH chunk: the header byte, then raw or VP8L-compressed levels,
/// then the inverse of the spatial filter.
fn decode_alpha(d: &[u8], w: usize, h: usize) -> R<Vec<u8>> {
    let hdr = *d.first().ok_or("empty WebP alpha")?;
    let method = hdr & 3;
    let filter = (hdr >> 2) & 3;
    let mut a = match method {
        0 => {
            let raw = d.get(1..1 + w * h).ok_or("short WebP alpha")?;
            raw.to_vec()
        }
        1 => {
            let mut b = vp8l::Bits::new(&d[1..]);
            let px = vp8l::decode_stream(&mut b, w, h).map_err(String::from)?;
            px.iter().map(|&p| (p >> 8) as u8).collect()
        }
        _ => return Err("unknown WebP alpha compression".into()),
    };
    // Inverse filters, row by row (libwebp's unfilters).
    for y in 0..h {
        let (done, rest) = a.split_at_mut(y * w);
        let row = &mut rest[..w];
        let prev = if y > 0 { Some(&done[(y - 1) * w..]) } else { None };
        match (filter, prev) {
            (0, _) => {}
            (_, None) | (1, Some(_)) => {
                let mut pred = prev.map_or(0, |p| p[0]);
                for v in row.iter_mut() {
                    *v = v.wrapping_add(pred);
                    pred = *v;
                }
            }
            (2, Some(p)) => {
                for (v, &t) in row.iter_mut().zip(p) {
                    *v = v.wrapping_add(t);
                }
            }
            (_, Some(p)) => {
                let (mut left, mut top_left) = (p[0], p[0]);
                for (x, v) in row.iter_mut().enumerate() {
                    let top = p[x];
                    let g = (left as i32 + top as i32 - top_left as i32).clamp(0, 255) as u8;
                    left = v.wrapping_add(g);
                    *v = left;
                    top_left = top;
                }
            }
        }
    }
    Ok(a)
}

/// libwebp's non-premultiplied "src over dst".
fn blend(src: [u8; 4], dst: [u8; 4]) -> [u8; 4] {
    let sa = src[3] as u32;
    if sa == 255 {
        return src;
    }
    if sa == 0 {
        return dst;
    }
    let da = (dst[3] as u32 * (256 - sa)) >> 8;
    let ba = sa + da;
    let scale = (1u32 << 24) / ba;
    let c = |k: usize| (((src[k] as u32 * sa + dst[k] as u32 * da) * scale) >> 24) as u8;
    [c(0), c(1), c(2), ba as u8]
}

struct AnimFrame {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    delay: u32,
    blend: bool,
    dispose_bg: bool,
    alpha: bool,
    px: Vec<[u8; 4]>,
}

pub fn decode(data: &[u8]) -> R<Picture> {
    if data.len() < 12 || &data[0..4] != b"RIFF" || &data[8..12] != b"WEBP" {
        return Err("not a WebP file".into());
    }
    let riff_len = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
    let body = &data[12..(8 + riff_len).clamp(12, data.len())];
    let parts = chunks(body);
    let first = parts.first().ok_or("empty WebP file")?;
    let animated = first.0 == *b"VP8X" && first.1.first().is_some_and(|f| f & 2 != 0);
    if !animated {
        let (w, h, alpha, px) = still(&parts)?;
        let kind = if parts.iter().any(|(id, _)| id == b"VP8L") { "lossless" } else { "lossy" };
        let format = alloc::format!("WebP, {kind}{}", if alpha { " with alpha" } else { "" });
        return Ok(Picture { w, h, frames: vec![Frame { px, delay_ms: 0 }], format });
    }
    let x = first.1;
    if x.len() < 10 {
        return Err("short WebP VP8X chunk".into());
    }
    let (cw, ch) = (le24(&x[4..]) + 1, le24(&x[7..]) + 1);
    check_size(cw, ch)?;
    let mut frames = Vec::new();
    for (id, d) in &parts {
        if id != b"ANMF" || d.len() < 16 {
            continue;
        }
        if frames.len() >= MAX_FRAMES {
            break;
        }
        let flags = d[15];
        let (w, h, alpha, px) = still(&chunks(&d[16..]))?;
        frames.push(AnimFrame {
            x: le24(&d[0..]) * 2,
            y: le24(&d[3..]) * 2,
            w: (le24(&d[6..]) + 1).min(w),
            h: (le24(&d[9..]) + 1).min(h),
            delay: le24(&d[12..]) as u32,
            blend: flags & 2 == 0,
            dispose_bg: flags & 1 != 0,
            alpha,
            px,
        });
        let f = frames.last_mut().unwrap();
        if f.w != w {
            // The frame's own size wins over a smaller ANMF size.
            let src = core::mem::take(&mut f.px);
            f.px = (0..f.h).flat_map(|r| src[r * w..r * w + f.w].to_vec()).collect();
        }
    }
    if frames.is_empty() {
        return Err("WebP animation without frames".into());
    }
    // Composite as WebPAnimDecoder does: key frames start from a cleared
    // canvas; other frames blend over the previous canvas, after its
    // frame was disposed of.
    let full = |f: &AnimFrame| f.x == 0 && f.y == 0 && f.w == cw && f.h == ch;
    let mut disposed = vec![[0u8; 4]; cw * ch];
    let mut out = Vec::with_capacity(frames.len());
    let mut prev_key = false;
    for i in 0..frames.len() {
        let f = &frames[i];
        let prev = i.checked_sub(1).map(|j| &frames[j]);
        let key = match prev {
            None => true,
            Some(p) => ((!f.alpha || !f.blend) && full(f)) || (p.dispose_bg && (full(p) || prev_key)),
        };
        let mut canvas = if key { vec![[0u8; 4]; cw * ch] } else { disposed.clone() };
        for y in 0..f.h {
            let cy = f.y + y;
            if cy >= ch {
                break;
            }
            for x in 0..f.w {
                let cx = f.x + x;
                if cx >= cw {
                    break;
                }
                let src = f.px[y * f.w + x];
                let inside_prev =
                    prev.is_some_and(|p| p.dispose_bg && cx >= p.x && cx < p.x + p.w && cy >= p.y && cy < p.y + p.h);
                canvas[cy * cw + cx] =
                    if f.blend && !key && !inside_prev { blend(src, disposed[cy * cw + cx]) } else { src };
            }
        }
        disposed.copy_from_slice(&canvas);
        if f.dispose_bg {
            for y in f.y..(f.y + f.h).min(ch) {
                for x in f.x..(f.x + f.w).min(cw) {
                    disposed[y * cw + x] = [0; 4];
                }
            }
        }
        prev_key = key;
        out.push(Frame { px: canvas, delay_ms: f.delay });
    }
    let format = alloc::format!("WebP animation, {} frames", out.len());
    Ok(Picture { w: cw, h: ch, frames: out, format })
}
