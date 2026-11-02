//! Colour spaces (ISO 32000-1, 8.6) and functions (7.10): device, CIE-based
//! (as their device equivalents), ICC-based (by component count), Lab,
//! Indexed, Separation and DeviceN through their tint transforms; sampled,
//! exponential, stitching and PostScript calculator functions.

use super::document::Document;
use super::object::{key, Object};
#[allow(unused_imports)]
use super::Float;
use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;

#[derive(Clone, Debug)]
pub enum Space {
    Gray,
    Rgb,
    Cmyk,
    Lab {
        white: [f64; 3],
        range: [f64; 4],
    },
    Indexed {
        base: Box<Space>,
        hival: usize,
        lut: Vec<u8>,
    },
    /// Separation (one component) and DeviceN.
    Tint {
        n: usize,
        alt: Box<Space>,
        func: Rc<Function>,
        none: bool,
    },
    Pattern(Option<Box<Space>>),
}

impl Space {
    pub fn components(&self) -> usize {
        match self {
            Space::Gray | Space::Indexed { .. } => 1,
            Space::Rgb | Space::Lab { .. } => 3,
            Space::Cmyk => 4,
            Space::Tint { n, .. } => *n,
            Space::Pattern(b) => b.as_ref().map_or(0, |b| b.components()),
        }
    }

    /// The initial colour (8.6.5).
    pub fn initial(&self) -> Vec<f64> {
        match self {
            Space::Cmyk => vec![0.0, 0.0, 0.0, 1.0],
            Space::Lab { range, .. } => vec![0.0, 0.0f64.clamp(range[0], range[1]), 0.0f64.clamp(range[2], range[3])],
            Space::Tint { n, .. } => vec![1.0; *n],
            _ => vec![0.0; self.components()],
        }
    }

    /// Decode array default for images (8.9.5.2).
    pub fn default_decode(&self, bpc: u32) -> Vec<f64> {
        match self {
            Space::Indexed { .. } => vec![0.0, ((1u64 << bpc) - 1) as f64],
            Space::Lab { range, .. } => vec![0.0, 100.0, range[0], range[1], range[2], range[3]],
            _ => (0..self.components()).flat_map(|_| [0.0, 1.0]).collect(),
        }
    }

    pub fn to_rgb(&self, c: &[f64]) -> [u8; 3] {
        let g = |i: usize| c.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0);
        let b = |v: f64| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        match self {
            Space::Gray => [b(g(0)); 3],
            Space::Rgb => [b(g(0)), b(g(1)), b(g(2))],
            Space::Cmyk => cmyk(g(0), g(1), g(2), g(3)),
            Space::Lab { white, .. } => lab(
                c.first().copied().unwrap_or(0.0),
                c.get(1).copied().unwrap_or(0.0),
                c.get(2).copied().unwrap_or(0.0),
                white,
            ),
            Space::Indexed { base, hival, lut } => {
                let i = (c.first().copied().unwrap_or(0.0).round().max(0.0) as usize).min(*hival);
                let n = base.components();
                let comps: Vec<f64> = (0..n).map(|k| *lut.get(i * n + k).unwrap_or(&0) as f64 / 255.0).collect();
                // Lab bases store scaled values.
                match &**base {
                    Space::Lab { range, .. } => {
                        let l = comps[0] * 100.0;
                        let a = range[0] + comps[1] * (range[1] - range[0]);
                        let bb = range[2] + comps[2] * (range[3] - range[2]);
                        base.to_rgb(&[l, a, bb])
                    }
                    _ => base.to_rgb(&comps),
                }
            }
            Space::Tint { alt, func, none, .. } => {
                if *none {
                    return [255; 3];
                }
                let out = func.eval(c);
                alt.to_rgb(&out)
            }
            Space::Pattern(_) => [0; 3],
        }
    }
}

pub fn cmyk(c: f64, m: f64, y: f64, k: f64) -> [u8; 3] {
    let f = |v: f64| ((1.0 - v.min(1.0)) * (1.0 - k) * 255.0 + 0.5) as u8;
    [f(c), f(m), f(y)]
}

fn lab(l: f64, a: f64, b: f64, white: &[f64; 3]) -> [u8; 3] {
    let fy = (l + 16.0) / 116.0;
    let fx = fy + a / 500.0;
    let fz = fy - b / 200.0;
    let g = |t: f64| if t > 6.0 / 29.0 { t * t * t } else { 3.0 * (6.0f64 / 29.0).powi(2) * (t - 4.0 / 29.0) };
    let (x, y, z) = (white[0] * g(fx), white[1] * g(fy), white[2] * g(fz));
    // XYZ (D65-ish white) to linear sRGB, then gamma.
    let r = 3.2406 * x - 1.5372 * y - 0.4986 * z;
    let gg = -0.9689 * x + 1.8758 * y + 0.0415 * z;
    let bb = 0.0557 * x - 0.2040 * y + 1.0570 * z;
    let gamma = |v: f64| {
        let v = v.clamp(0.0, 1.0);
        let s = if v <= 0.0031308 { 12.92 * v } else { 1.055 * libm::pow(v, 1.0 / 2.4) - 0.055 };
        (s * 255.0 + 0.5) as u8
    };
    [gamma(r), gamma(gg), gamma(bb)]
}

/// Reads a colour space object.
pub fn space(doc: &Document, o: &Object, depth: u32) -> Option<Space> {
    if depth > 8 {
        return None;
    }
    let o = doc.resolve(o);
    let (name, arr): (&[u8], &[Object]) = match &o {
        Object::Name(n) => (n, &[]),
        Object::Array(a) => (a.first()?.as_name()?, &a[1..]),
        _ => return None,
    };
    Some(match name {
        b"DeviceGray" | b"G" | b"CalGray" => Space::Gray,
        b"DeviceRGB" | b"RGB" | b"CalRGB" => Space::Rgb,
        b"DeviceCMYK" | b"CMYK" => Space::Cmyk,
        b"Lab" => {
            let d = doc.resolve(arr.first()?);
            let d = d.as_dict()?;
            let wp: Vec<f64> =
                doc.get_key(d, "WhitePoint").as_array().unwrap_or(&[]).iter().filter_map(Object::as_f64).collect();
            let r: Vec<f64> =
                doc.get_key(d, "Range").as_array().unwrap_or(&[]).iter().filter_map(Object::as_f64).collect();
            Space::Lab {
                white: if wp.len() == 3 { [wp[0], wp[1], wp[2]] } else { [0.9505, 1.0, 1.089] },
                range: if r.len() == 4 { [r[0], r[1], r[2], r[3]] } else { [-100.0, 100.0, -100.0, 100.0] },
            }
        }
        b"ICCBased" => {
            let s = doc.resolve(arr.first()?);
            let d = s.as_dict()?;
            if let Some(alt) = d.get(&b"Alternate"[..]) {
                if let Some(sp) = space(doc, alt, depth + 1) {
                    return Some(sp);
                }
            }
            match doc.get_key(d, "N").as_i64() {
                Some(1) => Space::Gray,
                Some(4) => Space::Cmyk,
                _ => Space::Rgb,
            }
        }
        b"Indexed" | b"I" => {
            let base = space(doc, arr.first()?, depth + 1)?;
            let hival = doc.resolve(arr.get(1)?).as_i64()?.clamp(0, 255) as usize;
            let lut = match doc.resolve(arr.get(2)?) {
                Object::Str(s) => s,
                Object::Stream(s) => doc.stream_data(&s),
                _ => Vec::new(),
            };
            Space::Indexed { base: Box::new(base), hival, lut }
        }
        b"Separation" | b"DeviceN" => {
            let n = if name == b"Separation" { 1 } else { doc.resolve(arr.first()?).as_array()?.len() };
            let names = doc.resolve(arr.first()?);
            let none = match &names {
                Object::Name(n) => n == b"None",
                Object::Array(a) => a.iter().all(|x| x.as_name() == Some(b"None")),
                _ => false,
            };
            let alt = space(doc, arr.get(1)?, depth + 1).unwrap_or(Space::Gray);
            let func = function(doc, arr.get(2)?, 0)?;
            // A separation named All (or the process colours) prints in
            // every plate: approximate as the alternate.
            Space::Tint { n, alt: Box::new(alt), func: Rc::new(func), none }
        }
        b"Pattern" => Space::Pattern(arr.first().and_then(|b| space(doc, b, depth + 1)).map(Box::new)),
        _ => return None,
    })
}

#[derive(Debug)]
pub enum Function {
    Sampled {
        domain: Vec<f64>,
        range: Vec<f64>,
        size: Vec<usize>,
        bps: u32,
        encode: Vec<f64>,
        decode: Vec<f64>,
        samples: Vec<u8>,
    },
    Exponential {
        domain: Vec<f64>,
        c0: Vec<f64>,
        c1: Vec<f64>,
        n: f64,
    },
    Stitching {
        domain: Vec<f64>,
        funcs: Vec<Function>,
        bounds: Vec<f64>,
        encode: Vec<f64>,
    },
    PostScript {
        domain: Vec<f64>,
        range: Vec<f64>,
        code: Vec<Ps>,
    },
    /// An array of single-output functions.
    Many(Vec<Function>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Ps {
    Num(f64),
    Op(Vec<u8>),
    If(Vec<Ps>),
    IfElse(Vec<Ps>, Vec<Ps>),
}

fn nums(doc: &Document, o: Option<&Object>) -> Vec<f64> {
    o.map(|o| doc.resolve(o))
        .and_then(|o| o.as_array().map(|a| a.iter().map(|x| doc.resolve(x).as_f64().unwrap_or(0.0)).collect()))
        .unwrap_or_default()
}

pub fn function(doc: &Document, o: &Object, depth: u32) -> Option<Function> {
    if depth > 8 {
        return None;
    }
    let o = doc.resolve(o);
    if let Object::Array(a) = &o {
        return Some(Function::Many(a.iter().filter_map(|f| function(doc, f, depth + 1)).collect()));
    }
    let d = o.as_dict()?;
    let domain = nums(doc, d.get(&b"Domain"[..]));
    match doc.get_key(d, "FunctionType").as_i64()? {
        0 => {
            let Object::Stream(s) = &o else { return None };
            let size: Vec<usize> = nums(doc, key(d, "Size")).into_iter().map(|v| v.max(1.0) as usize).collect();
            let range = nums(doc, key(d, "Range"));
            let mut encode = nums(doc, key(d, "Encode"));
            if encode.len() < 2 * size.len() {
                encode = size.iter().flat_map(|&s| [0.0, (s - 1) as f64]).collect();
            }
            let mut decode = nums(doc, key(d, "Decode"));
            if decode.is_empty() {
                decode = range.clone();
            }
            Some(Function::Sampled {
                domain,
                range,
                size,
                bps: doc.get_key(d, "BitsPerSample").as_i64().unwrap_or(8).clamp(1, 32) as u32,
                encode,
                decode,
                samples: doc.stream_data(s),
            })
        }
        2 => {
            let c0 = nums(doc, key(d, "C0"));
            let c1 = nums(doc, key(d, "C1"));
            Some(Function::Exponential {
                domain,
                c0: if c0.is_empty() { vec![0.0] } else { c0 },
                c1: if c1.is_empty() { vec![1.0] } else { c1 },
                n: doc.get_key(d, "N").as_f64().unwrap_or(1.0),
            })
        }
        3 => {
            let fs = doc.get_key(d, "Functions");
            let funcs = fs.as_array()?.iter().filter_map(|f| function(doc, f, depth + 1)).collect();
            Some(Function::Stitching {
                domain,
                funcs,
                bounds: nums(doc, key(d, "Bounds")),
                encode: nums(doc, key(d, "Encode")),
            })
        }
        4 => {
            let Object::Stream(s) = &o else { return None };
            let data = doc.stream_data(s);
            let mut lx = super::object::Lexer::new(&data);
            let code = ps_block(&mut lx).unwrap_or_default();
            Some(Function::PostScript { domain, range: nums(doc, key(d, "Range")), code })
        }
        _ => None,
    }
}

fn ps_block(lx: &mut super::object::Lexer) -> Option<Vec<Ps>> {
    use super::object::Token;
    // Skip to the opening brace.
    loop {
        match lx.next()? {
            Token::BraceStart => break,
            _ => continue,
        }
    }
    let mut out = Vec::new();
    loop {
        match lx.next()? {
            Token::BraceEnd => return Some(out),
            Token::BraceStart => {
                lx.pos -= 1;
                let a = ps_block(lx)?;
                let save = lx.pos;
                match lx.next() {
                    Some(Token::BraceStart) => {
                        lx.pos -= 1;
                        let b = ps_block(lx)?;
                        // "ifelse"
                        let _ = lx.next();
                        out.push(Ps::IfElse(a, b));
                    }
                    _ => {
                        lx.pos = save;
                        let _ = lx.next(); // "if"
                        out.push(Ps::If(a));
                    }
                }
            }
            Token::Int(i) => out.push(Ps::Num(i as f64)),
            Token::Real(r) => out.push(Ps::Num(r)),
            Token::Word(w) => out.push(Ps::Op(w)),
            _ => {}
        }
    }
}

fn run_ps(code: &[Ps], st: &mut Vec<f64>, depth: u32) {
    if depth > 16 {
        return;
    }
    for op in code {
        match op {
            Ps::Num(v) => st.push(*v),
            Ps::If(a) => {
                if st.pop().unwrap_or(0.0) != 0.0 {
                    run_ps(a, st, depth + 1);
                }
            }
            Ps::IfElse(a, b) => {
                if st.pop().unwrap_or(0.0) != 0.0 {
                    run_ps(a, st, depth + 1);
                } else {
                    run_ps(b, st, depth + 1);
                }
            }
            Ps::Op(w) => {
                let p1 = |st: &mut Vec<f64>| st.pop().unwrap_or(0.0);
                let b2 = |v: bool| if v { 1.0 } else { 0.0 };
                match w.as_slice() {
                    b"add" | b"sub" | b"mul" | b"div" | b"idiv" | b"mod" | b"exp" | b"atan" | b"eq" | b"ne" | b"gt"
                    | b"ge" | b"lt" | b"le" | b"and" | b"or" | b"xor" | b"bitshift" => {
                        let b = p1(st);
                        let a = p1(st);
                        st.push(match w.as_slice() {
                            b"add" => a + b,
                            b"sub" => a - b,
                            b"mul" => a * b,
                            b"div" => {
                                if b != 0.0 {
                                    a / b
                                } else {
                                    0.0
                                }
                            }
                            b"idiv" => {
                                if b as i64 != 0 {
                                    ((a as i64 as i128) / (b as i64 as i128)) as f64
                                } else {
                                    0.0
                                }
                            }
                            b"mod" => {
                                if b as i64 != 0 {
                                    ((a as i64 as i128) % (b as i64 as i128)) as f64
                                } else {
                                    0.0
                                }
                            }
                            b"exp" => libm::pow(a, b),
                            b"atan" => {
                                let d = libm::atan2(a, b).to_degrees();
                                if d < 0.0 {
                                    d + 360.0
                                } else {
                                    d
                                }
                            }
                            b"eq" => b2(a == b),
                            b"ne" => b2(a != b),
                            b"gt" => b2(a > b),
                            b"ge" => b2(a >= b),
                            b"lt" => b2(a < b),
                            b"le" => b2(a <= b),
                            b"and" => ((a as i64) & (b as i64)) as f64,
                            b"or" => ((a as i64) | (b as i64)) as f64,
                            b"xor" => ((a as i64) ^ (b as i64)) as f64,
                            _ => {
                                let s = b as i64;
                                (if s >= 0 { (a as i64) << s.min(63) } else { (a as i64) >> (-s).min(63) }) as f64
                            }
                        });
                    }
                    b"abs" | b"neg" | b"ceiling" | b"floor" | b"round" | b"truncate" | b"sqrt" | b"sin" | b"cos"
                    | b"ln" | b"log" | b"cvi" | b"cvr" | b"not" => {
                        let a = p1(st);
                        st.push(match w.as_slice() {
                            b"abs" => a.abs(),
                            b"neg" => -a,
                            b"ceiling" => libm::ceil(a),
                            b"floor" => libm::floor(a),
                            b"round" => libm::floor(a + 0.5),
                            b"truncate" | b"cvi" => libm::trunc(a),
                            b"sqrt" => libm::sqrt(a.max(0.0)),
                            b"sin" => libm::sin(a.to_radians()),
                            b"cos" => libm::cos(a.to_radians()),
                            b"ln" => libm::log(a),
                            b"log" => libm::log10(a),
                            b"not" => {
                                if a == 0.0 {
                                    1.0
                                } else if a == 1.0 {
                                    0.0
                                } else {
                                    !(a as i64) as f64
                                }
                            }
                            _ => a,
                        });
                    }
                    b"true" => st.push(1.0),
                    b"false" => st.push(0.0),
                    b"pop" => {
                        st.pop();
                    }
                    b"dup" => {
                        let a = *st.last().unwrap_or(&0.0);
                        st.push(a);
                    }
                    b"exch" => {
                        let n = st.len();
                        if n >= 2 {
                            st.swap(n - 1, n - 2);
                        }
                    }
                    b"copy" => {
                        let n = p1(st).max(0.0) as usize;
                        let l = st.len();
                        if n <= l {
                            let tail: Vec<f64> = st[l - n..].to_vec();
                            st.extend(tail);
                        }
                    }
                    b"index" => {
                        let n = p1(st).max(0.0) as usize;
                        let l = st.len();
                        st.push(if n < l { st[l - 1 - n] } else { 0.0 });
                    }
                    b"roll" => {
                        let j = p1(st) as i64;
                        let n = p1(st).max(0.0) as usize;
                        let l = st.len();
                        if n > 0 && n <= l {
                            let s = &mut st[l - n..];
                            let j = j.rem_euclid(n as i64) as usize;
                            s.rotate_right(j);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

fn clip_domain(x: f64, d: &[f64], i: usize) -> f64 {
    match (d.get(2 * i), d.get(2 * i + 1)) {
        (Some(&a), Some(&b)) => x.clamp(a.min(b), a.max(b)),
        _ => x,
    }
}

impl Function {
    pub fn eval(&self, x: &[f64]) -> Vec<f64> {
        match self {
            Function::Many(fs) => fs.iter().flat_map(|f| f.eval(x)).collect(),
            Function::Exponential { domain, c0, c1, n } => {
                let t = clip_domain(x.first().copied().unwrap_or(0.0), domain, 0);
                let p = if *n == 1.0 { t } else { libm::pow(t, *n) };
                c0.iter().zip(c1).map(|(a, b)| a + p * (b - a)).collect()
            }
            Function::Stitching { domain, funcs, bounds, encode } => {
                let t = clip_domain(x.first().copied().unwrap_or(0.0), domain, 0);
                let (d0, d1) = (domain.first().copied().unwrap_or(0.0), domain.get(1).copied().unwrap_or(1.0));
                let k = bounds.iter().position(|&b| t < b).unwrap_or(bounds.len()).min(funcs.len().saturating_sub(1));
                let lo = if k == 0 { d0 } else { bounds[k - 1] };
                let hi = if k < bounds.len() { bounds[k] } else { d1 };
                let (e0, e1) =
                    (encode.get(2 * k).copied().unwrap_or(0.0), encode.get(2 * k + 1).copied().unwrap_or(1.0));
                let u = if hi > lo { e0 + (t - lo) * (e1 - e0) / (hi - lo) } else { e0 };
                funcs.get(k).map_or_else(Vec::new, |f| f.eval(&[u]))
            }
            Function::Sampled { domain, range, size, bps, encode, decode, samples } => {
                let m = size.len();
                let n = range.len() / 2;
                // Multilinear interpolation over the sample grid.
                let mut pos = Vec::with_capacity(m);
                for i in 0..m {
                    let xi = clip_domain(x.get(i).copied().unwrap_or(0.0), domain, i);
                    let (d0, d1) =
                        (domain.get(2 * i).copied().unwrap_or(0.0), domain.get(2 * i + 1).copied().unwrap_or(1.0));
                    let e = encode[2 * i]
                        + (xi - d0) * (encode[2 * i + 1] - encode[2 * i]) / if d1 != d0 { d1 - d0 } else { 1.0 };
                    pos.push(e.clamp(0.0, (size[i] - 1) as f64));
                }
                let maxv = ((1u64 << bps) - 1) as f64;
                let sample = |idx: &[usize], j: usize| -> f64 {
                    let mut off = 0usize;
                    let mut stride = 1usize;
                    for (i, &v) in idx.iter().enumerate() {
                        off += v * stride;
                        stride *= size[i];
                    }
                    let bit = (off * n + j) * *bps as usize;
                    let mut v = 0u64;
                    for b in 0..*bps as usize {
                        let byte = samples.get((bit + b) / 8).copied().unwrap_or(0);
                        v = v << 1 | ((byte >> (7 - (bit + b) % 8)) & 1) as u64;
                    }
                    v as f64
                };
                let mut out = vec![0.0; n];
                for (j, o) in out.iter_mut().enumerate() {
                    let mut acc = 0.0;
                    for corner in 0..(1usize << m.min(8)) {
                        let mut w = 1.0;
                        let mut idx = Vec::with_capacity(m);
                        for i in 0..m {
                            let f = libm::floor(pos[i]) as usize;
                            let t = pos[i] - f as f64;
                            let hi = corner >> i & 1 == 1;
                            idx.push(if hi { (f + 1).min(size[i] - 1) } else { f });
                            w *= if hi { t } else { 1.0 - t };
                        }
                        if w > 0.0 {
                            acc += w * sample(&idx, j);
                        }
                    }
                    let (d0, d1) =
                        (decode.get(2 * j).copied().unwrap_or(0.0), decode.get(2 * j + 1).copied().unwrap_or(1.0));
                    let v = d0 + acc * (d1 - d0) / maxv;
                    *o = clip_domain(v, range, j);
                }
                out
            }
            Function::PostScript { domain, range, code } => {
                let mut st: Vec<f64> = x.iter().enumerate().map(|(i, &v)| clip_domain(v, domain, i)).collect();
                run_ps(code, &mut st, 0);
                let n = range.len() / 2;
                let start = st.len().saturating_sub(n);
                st[start..].iter().enumerate().map(|(i, &v)| clip_domain(v, range, i)).collect()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postscript_functions() {
        let src = b"{ 360 mul sin 2 div exch 360 mul sin 2 div add dup 0 gt { pop 1 } { pop 0 } ifelse }";
        let mut lx = crate::pdf::object::Lexer::new(src);
        let code = ps_block(&mut lx).unwrap();
        let f = Function::PostScript { domain: vec![0.0, 1.0, 0.0, 1.0], range: vec![0.0, 1.0], code };
        assert_eq!(f.eval(&[0.25, 0.25]), [1.0]);
        assert_eq!(f.eval(&[0.75, 0.75]), [0.0]);
    }

    #[test]
    fn exponential_and_stitching() {
        let a = Function::Exponential { domain: vec![0.0, 1.0], c0: vec![0.0], c1: vec![1.0], n: 1.0 };
        let b = Function::Exponential { domain: vec![0.0, 1.0], c0: vec![1.0], c1: vec![0.0], n: 1.0 };
        let s = Function::Stitching {
            domain: vec![0.0, 1.0],
            funcs: vec![a, b],
            bounds: vec![0.5],
            encode: vec![0.0, 1.0, 0.0, 1.0],
        };
        assert_eq!(s.eval(&[0.25]), [0.5]);
        assert_eq!(s.eval(&[0.75]), [0.5]);
        assert_eq!(s.eval(&[0.5]), [1.0]);
    }

    #[test]
    fn device_colours() {
        assert_eq!(Space::Cmyk.to_rgb(&[0.0, 0.0, 0.0, 1.0]), [0, 0, 0]);
        assert_eq!(Space::Cmyk.to_rgb(&[1.0, 0.0, 0.0, 0.0]), [0, 255, 255]);
        assert_eq!(Space::Gray.to_rgb(&[0.5]), [128, 128, 128]);
    }
}
