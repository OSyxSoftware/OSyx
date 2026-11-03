//! `msimg32.dll` (and gdi32's Gdi* aliases): AlphaBlend, TransparentBlt
//! and GradientFill on device contexts.
use super::*;

const AC_SRC_OVER: u8 = 0;
const AC_SRC_ALPHA: u8 = 1;
const GRADIENT_FILL_RECT_H: u32 = 0;
const GRADIENT_FILL_RECT_V: u32 = 1;
const GRADIENT_FILL_TRIANGLE: u32 = 2;

fn chan(c: Color, shift: u32) -> u32 {
    (c >> shift) & 255
}

/// A stretched copy: calls `f(dst_x, dst_y, src_pixel)` for each pixel of
/// the destination rectangle inside the clip (nearest source pixel).
#[allow(clippy::too_many_arguments)]
fn stretch(
    dst: u64,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    source: &Snapshot,
    sx: i32,
    sy: i32,
    sw: i32,
    sh: i32,
    mut f: impl FnMut(Color, Color) -> Option<Color>,
) -> bool {
    draw(dst, |s, d, _, clip| {
        let (x, y) = d.transform.point(x, y);
        let r = Rect::new(x, y, w as u32, h as u32).intersect(&clip);
        for dy in r.y..r.y + r.h as i32 {
            for dx in r.x..r.x + r.w as i32 {
                let fx = ((dx - x) as i64 * sw as i64 / w as i64) as i32;
                let fy = ((dy - y) as i64 * sh as i64 / h as i64) as i32;
                let Some(src) = source.pixel(sx + fx, sy + fy) else { continue };
                let under = s.get(dx, dy).unwrap_or(0);
                if let Some(c) = f(src, under) {
                    s.put(dx, dy, c);
                }
            }
        }
        (true, r)
    })
    .unwrap_or(false)
}

/// The source rectangle must lie inside the source surface.
fn source(src: u64, sx: i32, sy: i32, sw: i32, sh: i32) -> Result<Snapshot, u32> {
    let s = snapshot(src, Rect::new(sx, sy, sw as u32, sh as u32)).ok_or(ERROR_INVALID_HANDLE)?;
    if sx < 0 || sy < 0 || sx as i64 + sw as i64 > s.width as i64 || sy as i64 + sh as i64 > s.height as i64 {
        return Err(ERROR_INVALID_PARAMETER);
    }
    Ok(s)
}

/// AlphaBlend: BLENDFUNCTION {BlendOp, BlendFlags, SourceConstantAlpha,
/// AlphaFormat} packed in a DWORD. With AC_SRC_ALPHA the source is
/// premultiplied: dst = src * k + dst * (1 - srcA * k), k = SCA / 255.
#[allow(clippy::too_many_arguments)]
extern "win64" fn AlphaBlend(dst: u64, x: i32, y: i32, w: i32, h: i32, src: u64, sx: i32, sy: i32, sw: i32, sh: i32, blend: u32) -> Bool {
    let [op, flags, constant, format] = blend.to_le_bytes();
    if op != AC_SRC_OVER || flags != 0 || w < 0 || h < 0 || sw < 0 || sh < 0 {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    if w == 0 || h == 0 || sw == 0 || sh == 0 {
        return TRUE;
    }
    let s = match source(src, sx, sy, sw, sh) {
        Ok(s) => s,
        Err(e) => return fail(e, FALSE),
    };
    let k = constant as u32;
    let per_pixel = format & AC_SRC_ALPHA != 0;
    let ok = stretch(dst, x, y, w, h, &s, sx, sy, sw, sh, |c, under| {
        let src_a = if per_pixel { chan(c, 24) * k / 255 } else { k };
        let mut out = 0u32;
        for shift in [0, 8, 16, 24] {
            // Without AC_SRC_ALPHA the source counts as opaque.
            let sc = if shift == 24 && !per_pixel { 255 } else { chan(c, shift) };
            // Premultiplied colour channels scale by the constant alpha;
            // without per-pixel alpha the constant alone weighs them.
            let v = sc * k / 255 + chan(under, shift) * (255 - src_a) / 255;
            out |= v.min(255) << shift;
        }
        Some(out)
    });
    if ok { TRUE } else { fail(ERROR_INVALID_HANDLE, FALSE) }
}

/// TransparentBlt: a stretched copy that skips source pixels of the
/// transparent colour (COLORREF).
#[allow(clippy::too_many_arguments)]
extern "win64" fn TransparentBlt(dst: u64, x: i32, y: i32, w: i32, h: i32, src: u64, sx: i32, sy: i32, sw: i32, sh: i32, transparent: u32) -> Bool {
    if w < 0 || h < 0 || sw < 0 || sh < 0 {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    if w == 0 || h == 0 || sw == 0 || sh == 0 {
        return TRUE;
    }
    let s = match source(src, sx, sy, sw, sh) {
        Ok(s) => s,
        Err(e) => return fail(e, FALSE),
    };
    let key = colorref(transparent) & 0xFF_FFFF;
    let ok = stretch(dst, x, y, w, h, &s, sx, sy, sw, sh, |c, _| (c & 0xFF_FFFF != key).then_some(c));
    if ok { TRUE } else { fail(ERROR_INVALID_HANDLE, FALSE) }
}

/// TRIVERTEX: x, y, then 16-bit Red, Green, Blue, Alpha.
#[derive(Clone, Copy)]
struct Vertex {
    x: i32,
    y: i32,
    c: [i64; 4],
}

fn vertex(p: *const u8, i: u32) -> Vertex {
    // SAFETY: the caller's TRIVERTEX array (16 bytes each).
    unsafe {
        let v = p.add(16 * i as usize);
        let rd16 = |o: usize| core::ptr::read_unaligned(v.add(o) as *const u16) as i64;
        Vertex {
            x: core::ptr::read_unaligned(v as *const i32),
            y: core::ptr::read_unaligned(v.add(4) as *const i32),
            c: [rd16(8), rd16(10), rd16(12), rd16(14)],
        }
    }
}

/// An OSyx colour (alpha in the top byte) from 16-bit channels.
fn color16(c: [i64; 4]) -> Color {
    let b = |v: i64| (v.clamp(0, 0xFFFF) >> 8) as u32;
    b(c[3]) << 24 | b(c[0]) << 16 | b(c[1]) << 8 | b(c[2])
}

extern "win64" fn GradientFill(dc: u64, vertices: *const u8, nv: u32, mesh: *const u32, nm: u32, mode: u32) -> Bool {
    if vertices.is_null() || mesh.is_null() || !matches!(mode, GRADIENT_FILL_RECT_H | GRADIENT_FILL_RECT_V | GRADIENT_FILL_TRIANGLE) {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    let per = if mode == GRADIENT_FILL_TRIANGLE { 3 } else { 2 };
    // SAFETY: the caller's mesh array of `nm` entries.
    let indices = unsafe { core::slice::from_raw_parts(mesh, (nm * per) as usize) };
    if indices.iter().any(|&i| i >= nv) {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    let ok = draw(dc, |s, d, _, clip| {
        let mut drawn = Rect::EMPTY;
        let at = |i: u32| {
            let v = vertex(vertices, i);
            let (x, y) = d.transform.point(v.x, v.y);
            Vertex { x, y, ..v }
        };
        for m in indices.chunks(per as usize) {
            if mode == GRADIENT_FILL_TRIANGLE {
                let (a, b, c) = (at(m[0]), at(m[1]), at(m[2]));
                let area = (b.x - a.x) as i64 * (c.y - a.y) as i64 - (c.x - a.x) as i64 * (b.y - a.y) as i64;
                if area == 0 {
                    continue;
                }
                let (x0, x1) = (a.x.min(b.x).min(c.x), a.x.max(b.x).max(c.x));
                let (y0, y1) = (a.y.min(b.y).min(c.y), a.y.max(b.y).max(c.y));
                let r = Rect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32).intersect(&clip);
                for py in r.y..r.y + r.h as i32 {
                    for px in r.x..r.x + r.w as i32 {
                        // Barycentric weights of the pixel centre.
                        let edge = |p: &Vertex, q: &Vertex| (q.x - p.x) as i64 * (py - p.y) as i64 - (px - p.x) as i64 * (q.y - p.y) as i64;
                        let (wa, wb, wc) = (edge(&b, &c), edge(&c, &a), edge(&a, &b));
                        let inside = if area > 0 { wa >= 0 && wb >= 0 && wc >= 0 } else { wa <= 0 && wb <= 0 && wc <= 0 };
                        if !inside {
                            continue;
                        }
                        let mut col = [0i64; 4];
                        for (k, v) in col.iter_mut().enumerate() {
                            *v = (a.c[k] * wa + b.c[k] * wb + c.c[k] * wc) / area;
                        }
                        s.put(px, py, color16(col));
                    }
                }
                drawn = drawn.union(&r);
            } else {
                let (ul, lr) = (at(m[0]), at(m[1]));
                let (x0, x1) = (ul.x.min(lr.x), ul.x.max(lr.x));
                let (y0, y1) = (ul.y.min(lr.y), ul.y.max(lr.y));
                let r = Rect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32).intersect(&clip);
                let horizontal = mode == GRADIENT_FILL_RECT_H;
                let span = if horizontal { (x1 - x0).max(1) } else { (y1 - y0).max(1) } as i64;
                for py in r.y..r.y + r.h as i32 {
                    for px in r.x..r.x + r.w as i32 {
                        let t = if horizontal { px - x0 } else { py - y0 } as i64;
                        let (from, to) = if (horizontal && ul.x <= lr.x) || (!horizontal && ul.y <= lr.y) { (ul.c, lr.c) } else { (lr.c, ul.c) };
                        let mut col = [0i64; 4];
                        for (k, v) in col.iter_mut().enumerate() {
                            *v = from[k] + (to[k] - from[k]) * t / span;
                        }
                        s.put(px, py, color16(col));
                    }
                }
                drawn = drawn.union(&r);
            }
        }
        (true, drawn)
    })
    .unwrap_or(false);
    if ok { TRUE } else { fail(ERROR_INVALID_HANDLE, FALSE) }
}

pub(super) fn msimg32_exports() -> Vec<(&'static str, u64)> {
    export_table![AlphaBlend, TransparentBlt, GradientFill]
}

pub(super) fn gdi32_exports() -> Vec<(&'static str, u64)> {
    alloc::vec![
        ("GdiAlphaBlend", AlphaBlend as *const () as u64),
        ("GdiTransparentBlt", TransparentBlt as *const () as u64),
        ("GdiGradientFill", GradientFill as *const () as u64),
    ]
}
