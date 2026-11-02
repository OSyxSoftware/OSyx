//! The content stream interpreter (ISO 32000-1, 8 and 9): graphics state,
//! paths and clipping, colours and patterns, shadings, text, images,
//! form XObjects and annotation appearances, drawn onto a [`Canvas`].

use super::color::{self, Function, Space};
use super::document::{Document, Page};
use super::font::{Font, FontSource};
use super::image::{self, Image};
use super::object::{parse_from, Dict, Lexer, Object, Stream, Token};
use super::raster::{Canvas, Mask, Matrix, Path, Rule, Seg, StrokeStyle};
#[allow(unused_imports)]
use super::Float;
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;

/// What fills or strokes: a colour, or a pattern.
#[derive(Clone)]
enum Paint {
    Color([u8; 3]),
    Shading(Rc<Shading>, Matrix),
    Tiling(Rc<Tile>, Matrix),
}

#[derive(Clone)]
struct GState {
    ctm: Matrix,
    clip: Option<Rc<Mask>>,
    fill_space: Space,
    stroke_space: Space,
    fill: Paint,
    stroke: Paint,
    line_width: f64,
    cap: u8,
    join: u8,
    miter: f64,
    dash: Vec<f64>,
    phase: f64,
    fill_alpha: f32,
    stroke_alpha: f32,
    font: Option<Rc<Font>>,
    size: f64,
    char_space: f64,
    word_space: f64,
    hscale: f64,
    leading: f64,
    rise: f64,
    render_mode: i64,
}

impl GState {
    fn new(ctm: Matrix) -> GState {
        GState {
            ctm,
            clip: None,
            fill_space: Space::Gray,
            stroke_space: Space::Gray,
            fill: Paint::Color([0; 3]),
            stroke: Paint::Color([0; 3]),
            line_width: 1.0,
            cap: 0,
            join: 0,
            miter: 10.0,
            dash: Vec::new(),
            phase: 0.0,
            fill_alpha: 1.0,
            stroke_alpha: 1.0,
            font: None,
            size: 0.0,
            char_space: 0.0,
            word_space: 0.0,
            hscale: 1.0,
            leading: 0.0,
            rise: 0.0,
            render_mode: 0,
        }
    }
}

/// An axial (2), radial (3) or function-based (1) shading.
struct Shading {
    kind: i64,
    coords: Vec<f64>,
    domain: [f64; 2],
    func: Option<Function>,
    space: Space,
    extend: [bool; 2],
    /// Function-based shadings: domain matrix.
    matrix: Matrix,
    background: Option<[u8; 3]>,
}

impl Shading {
    /// Colour at a point in shading space, `None` outside (unextended).
    fn color_at(&self, x: f64, y: f64) -> Option<[u8; 3]> {
        let eval = |t: f64| -> [u8; 3] {
            let out = self.func.as_ref().map_or_else(Vec::new, |f| f.eval(&[t]));
            self.space.to_rgb(&out)
        };
        match self.kind {
            2 => {
                let c = &self.coords;
                let (x0, y0, x1, y1) = (c[0], c[1], c[2], c[3]);
                let (dx, dy) = (x1 - x0, y1 - y0);
                let den = dx * dx + dy * dy;
                let mut s = if den == 0.0 { 0.0 } else { ((x - x0) * dx + (y - y0) * dy) / den };
                if s < 0.0 {
                    if !self.extend[0] {
                        return self.background;
                    }
                    s = 0.0;
                }
                if s > 1.0 {
                    if !self.extend[1] {
                        return self.background;
                    }
                    s = 1.0;
                }
                Some(eval(self.domain[0] + s * (self.domain[1] - self.domain[0])))
            }
            3 => {
                let c = &self.coords;
                let (x0, y0, r0, x1, y1, r1) = (c[0], c[1], c[2], c[3], c[4], c[5]);
                // Solve for the largest s with the point on circle(s).
                let (cdx, cdy, dr) = (x1 - x0, y1 - y0, r1 - r0);
                let (pdx, pdy) = (x - x0, y - y0);
                let a = cdx * cdx + cdy * cdy - dr * dr;
                let b = pdx * cdx + pdy * cdy + r0 * dr;
                let cc = pdx * pdx + pdy * pdy - r0 * r0;
                let mut cands = Vec::new();
                if a.abs() < 1e-9 {
                    if b != 0.0 {
                        cands.push(cc / (2.0 * b));
                    }
                } else {
                    let disc = b * b - a * cc;
                    if disc >= 0.0 {
                        let sq = libm::sqrt(disc);
                        cands.push((b + sq) / a);
                        cands.push((b - sq) / a);
                    }
                }
                cands.sort_by(|p, q| q.partial_cmp(p).unwrap_or(core::cmp::Ordering::Equal));
                for s in cands {
                    if r0 + s * dr < 0.0 {
                        continue;
                    }
                    let t = if s < 0.0 {
                        if !self.extend[0] {
                            continue;
                        }
                        0.0
                    } else if s > 1.0 {
                        if !self.extend[1] {
                            continue;
                        }
                        1.0
                    } else {
                        s
                    };
                    return Some(eval(self.domain[0] + t * (self.domain[1] - self.domain[0])));
                }
                self.background
            }
            1 => {
                let inv = self.matrix.invert()?;
                let (u, v) = inv.apply(x, y);
                let d = &self.coords;
                if d.len() == 4 && (u < d[0] || u > d[1] || v < d[2] || v > d[3]) {
                    return self.background;
                }
                let out = self.func.as_ref().map_or_else(Vec::new, |f| f.eval(&[u, v]));
                Some(self.space.to_rgb(&out))
            }
            _ => self.background,
        }
    }
}

/// A rendered cell of a tiling pattern.
struct Tile {
    img: Canvas,
    alpha: Vec<u8>,
    /// Pattern space to cell pixels.
    scale: f64,
    step: (f64, f64),
    origin: (f64, f64),
}

/// Largest page bitmap (about 64 million pixels, 192 MiB of RGB).
const MAX_PIXELS: usize = 1 << 26;

pub struct Renderer<'a> {
    doc: &'a Document,
    fonts: FontSource<'a>,
    pub canvas: Canvas,
    gs: GState,
    stack: Vec<GState>,
    resources: Vec<Dict>,
    path: Path,
    pending_clip: Option<Rule>,
    tm: Matrix,
    tlm: Matrix,
    text_clip: Option<Path>,
    font_cache: BTreeMap<Vec<u8>, Rc<Font>>,
    font_by_ref: BTreeMap<u32, Rc<Font>>,
    depth: u32,
    /// Operators executed (to bound pathological files).
    ops: u64,
}

fn n(o: &[Object], i: usize) -> f64 {
    o.get(i).and_then(Object::as_f64).unwrap_or(0.0)
}

impl<'a> Renderer<'a> {
    /// A renderer for `page` at `scale` device pixels per point.
    pub fn new(
        doc: &'a Document,
        page: &Page,
        scale: f64,
        fonts: FontSource<'a>,
    ) -> Result<Renderer<'a>, alloc::string::String> {
        let b = page.bbox;
        let (pw, ph) = (b[2] - b[0], b[3] - b[1]);
        let (w, h) = if page.rotate % 180 == 0 { (pw, ph) } else { (ph, pw) };
        let (cw, ch) = ((w * scale).ceil().max(1.0), (h * scale).ceil().max(1.0));
        if !(cw * ch <= MAX_PIXELS as f64) {
            return Err(alloc::format!("the page is too large to draw at this zoom ({cw} x {ch} pixels)"));
        }
        let (cw, ch) = (cw as usize, ch as usize);
        // User space (y up, origin at the box corner) to device (y down),
        // with the page rotation.
        let base = Matrix([1.0, 0.0, 0.0, 1.0, -b[0], -b[1]]);
        let rot = match page.rotate {
            90 => Matrix([0.0, -1.0, 1.0, 0.0, 0.0, pw]),
            180 => Matrix([-1.0, 0.0, 0.0, -1.0, pw, ph]),
            270 => Matrix([0.0, 1.0, -1.0, 0.0, ph, 0.0]),
            _ => Matrix::IDENTITY,
        };
        let flip = Matrix([scale, 0.0, 0.0, -scale, 0.0, h * scale]);
        let ctm = base.then(&rot).then(&flip);
        Ok(Renderer {
            doc,
            fonts,
            canvas: Canvas::new(cw, ch),
            gs: GState::new(ctm),
            stack: Vec::new(),
            resources: vec![page.resources.clone()],
            path: Path::default(),
            pending_clip: None,
            tm: Matrix::IDENTITY,
            tlm: Matrix::IDENTITY,
            text_clip: None,
            font_cache: BTreeMap::new(),
            font_by_ref: BTreeMap::new(),
            depth: 0,
            ops: 0,
        })
    }

    /// Draws the page's contents and annotation appearances.
    pub fn render_page(&mut self, page: &Page) {
        let contents = self.doc.get_key(&page.dict, "Contents");
        let mut data = Vec::new();
        match &contents {
            Object::Stream(s) => data = self.doc.stream_data(s),
            Object::Array(a) => {
                for c in a {
                    if let Object::Stream(s) = self.doc.resolve(c) {
                        data.extend_from_slice(&self.doc.stream_data(&s));
                        data.push(b'\n');
                    }
                }
            }
            _ => {}
        }
        self.run(&data);
        self.annotations(page);
    }

    fn annotations(&mut self, page: &Page) {
        let annots = self.doc.get_key(&page.dict, "Annots");
        for a in annots.as_array().unwrap_or(&[]) {
            let a = self.doc.resolve(a);
            let Some(ad) = a.as_dict() else { continue };
            let flags = self.doc.get_key(ad, "F").as_i64().unwrap_or(0);
            if flags & 2 != 0 || self.doc.get_key(ad, "Subtype").as_name() == Some(b"Popup") {
                continue;
            }
            let Some(rect) = self.doc.rect(ad, "Rect") else { continue };
            let ap = self.doc.get_key(ad, "AP");
            let Some(apd) = ap.as_dict() else { continue };
            let mut n = self.doc.get_key(apd, "N");
            if let Some(states) = n.as_dict().filter(|_| !matches!(n, Object::Stream(_))) {
                let st = self.doc.get_key(ad, "AS");
                n = st.as_name().and_then(|s| states.get(s)).map_or(Object::Null, |o| self.doc.resolve(o));
            }
            let Object::Stream(form) = n else { continue };
            // Map the form's transformed BBox onto the annotation rectangle.
            let bbox = self.doc.rect(&form.dict, "BBox").unwrap_or([0.0, 0.0, 1.0, 1.0]);
            let m = self.matrix_of(&form.dict, "Matrix");
            let corners = [(bbox[0], bbox[1]), (bbox[2], bbox[1]), (bbox[0], bbox[3]), (bbox[2], bbox[3])]
                .map(|(x, y)| m.apply(x, y));
            let (minx, maxx) = corners.iter().fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.0), a.1.max(p.0)));
            let (miny, maxy) = corners.iter().fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.1), a.1.max(p.1)));
            let sx = if maxx > minx { (rect[2] - rect[0]) / (maxx - minx) } else { 1.0 };
            let sy = if maxy > miny { (rect[3] - rect[1]) / (maxy - miny) } else { 1.0 };
            let a_m = Matrix([sx, 0.0, 0.0, sy, rect[0] - minx * sx, rect[1] - miny * sy]);
            self.save();
            self.gs.ctm = a_m.then(&self.gs.ctm);
            self.form(&form);
            self.restore();
        }
    }

    fn matrix_of(&self, d: &Dict, k: &str) -> Matrix {
        let v: Vec<f64> = self
            .doc
            .get_key(d, k)
            .as_array()
            .unwrap_or(&[])
            .iter()
            .filter_map(|o| self.doc.resolve(o).as_f64())
            .collect();
        if v.len() == 6 {
            Matrix([v[0], v[1], v[2], v[3], v[4], v[5]])
        } else {
            Matrix::IDENTITY
        }
    }

    fn resource(&self, cat: &str, name: &[u8]) -> Option<Object> {
        for r in self.resources.iter().rev() {
            if let Some(d) = self.doc.get_key(r, cat).as_dict() {
                if let Some(o) = d.get(name) {
                    return Some(o.clone());
                }
            }
        }
        None
    }

    fn save(&mut self) {
        if self.stack.len() < 256 {
            self.stack.push(self.gs.clone());
        }
    }

    fn restore(&mut self) {
        if let Some(g) = self.stack.pop() {
            self.gs = g;
        }
    }

    pub fn run(&mut self, data: &[u8]) {
        let mut lx = Lexer::new(data);
        let mut args: Vec<Object> = Vec::new();
        let base_depth = self.stack.len();
        while let Some(t) = lx.next() {
            self.ops += 1;
            if self.ops > 20_000_000 {
                return;
            }
            match t {
                Token::Word(op) => {
                    if op == b"BI" {
                        self.inline_image(&mut lx);
                        args.clear();
                        continue;
                    }
                    self.op(&op, &args);
                    args.clear();
                }
                t => {
                    if let Some(o) = parse_from(&mut lx, t, 0) {
                        if args.len() < 64 {
                            args.push(o);
                        }
                    }
                }
            }
        }
        // Unbalanced q inside a content stream.
        while self.stack.len() > base_depth {
            self.restore();
        }
    }

    fn device_path(&self, path: &Path) -> Path {
        path.clone()
    }

    fn set_color(&mut self, stroke: bool, args: &[Object]) {
        let space = if stroke { self.gs.stroke_space.clone() } else { self.gs.fill_space.clone() };
        let paint = if let Space::Pattern(base) = &space {
            // The last operand names a pattern.
            match args.last().and_then(Object::as_name).and_then(|nm| self.resource("Pattern", nm)) {
                Some(p) => self.pattern(&p, base.as_deref(), args),
                None => Paint::Color([0; 3]),
            }
        } else {
            let comps: Vec<f64> = args.iter().filter_map(Object::as_f64).collect();
            Paint::Color(space.to_rgb(&comps))
        };
        if stroke {
            self.gs.stroke = paint;
        } else {
            self.gs.fill = paint;
        }
    }

    fn pattern(&mut self, p: &Object, base: Option<&Space>, args: &[Object]) -> Paint {
        let po = self.doc.resolve(p);
        let Some(pd) = po.as_dict() else { return Paint::Color([0; 3]) };
        // Pattern space is the default space of the page (the CTM at the
        // start of the content stream): the base of the stack.
        let base_ctm = self.stack.first().map_or(self.gs.ctm, |g| g.ctm);
        let m = self.matrix_of(pd, "Matrix").then(&base_ctm);
        match self.doc.get_key(pd, "PatternType").as_i64() {
            Some(2) => {
                let sh = self.doc.get_key(pd, "Shading");
                match self.shading(&sh) {
                    Some(s) => Paint::Shading(Rc::new(s), m),
                    None => Paint::Color([128; 3]),
                }
            }
            Some(1) => {
                let Object::Stream(s) = po.clone() else { return Paint::Color([0; 3]) };
                let paint_type = self.doc.get_key(pd, "PaintType").as_i64().unwrap_or(1);
                let color = if paint_type == 2 {
                    let comps: Vec<f64> = args.iter().filter_map(Object::as_f64).collect();
                    base.map_or([0; 3], |b| b.to_rgb(&comps))
                } else {
                    [0; 3]
                };
                match self.tile(&s, m, paint_type == 2, color) {
                    Some(t) => Paint::Tiling(Rc::new(t), m),
                    None => Paint::Color(color),
                }
            }
            _ => Paint::Color([0; 3]),
        }
    }

    fn shading(&self, o: &Object) -> Option<Shading> {
        let so = self.doc.resolve(o);
        let d = so.as_dict()?;
        let kind = self.doc.get_key(d, "ShadingType").as_i64()?;
        let space = color::space(self.doc, d.get(&b"ColorSpace"[..])?, 0)?;
        let coords: Vec<f64> = self
            .doc
            .get_key(d, if kind == 1 { "Domain" } else { "Coords" })
            .as_array()
            .unwrap_or(&[])
            .iter()
            .filter_map(|x| self.doc.resolve(x).as_f64())
            .collect();
        let dom: Vec<f64> = self
            .doc
            .get_key(d, "Domain")
            .as_array()
            .unwrap_or(&[])
            .iter()
            .filter_map(|x| self.doc.resolve(x).as_f64())
            .collect();
        let ext: Vec<bool> = self
            .doc
            .get_key(d, "Extend")
            .as_array()
            .unwrap_or(&[])
            .iter()
            .map(|x| matches!(x, Object::Bool(true)))
            .collect();
        let func = d.get(&b"Function"[..]).and_then(|f| color::function(self.doc, f, 0));
        let background = self.doc.get_key(d, "Background").as_array().map(|a| {
            let c: Vec<f64> = a.iter().filter_map(|x| self.doc.resolve(x).as_f64()).collect();
            space.to_rgb(&c)
        });
        if (kind == 2 && coords.len() < 4) || (kind == 3 && coords.len() < 6) || !(1..=3).contains(&kind) {
            // Mesh shadings (4 to 7): the average colour of the function or
            // the background stands in.
            let c = func.as_ref().map(|f| space.to_rgb(&f.eval(&[0.5]))).or(background)?;
            return Some(Shading {
                kind: 0,
                coords,
                domain: [0.0, 1.0],
                func: None,
                space,
                extend: [true; 2],
                matrix: Matrix::IDENTITY,
                background: Some(c),
            });
        }
        Some(Shading {
            kind,
            coords,
            domain: if dom.len() >= 2 { [dom[0], dom[1]] } else { [0.0, 1.0] },
            func,
            space,
            extend: [ext.first().copied().unwrap_or(false), ext.get(1).copied().unwrap_or(false)],
            matrix: self.matrix_of(d, "Matrix"),
            background,
        })
    }

    /// Renders one cell of a tiling pattern at device resolution.
    fn tile(&mut self, s: &Rc<Stream>, m: Matrix, uncolored: bool, color: [u8; 3]) -> Option<Tile> {
        let d = &s.dict;
        let bbox = self.doc.rect(d, "BBox")?;
        let xs = self.doc.get_key(d, "XStep").as_f64().unwrap_or(bbox[2] - bbox[0]);
        let ys = self.doc.get_key(d, "YStep").as_f64().unwrap_or(bbox[3] - bbox[1]);
        if xs == 0.0 || ys == 0.0 || self.depth > 4 {
            return None;
        }
        let scale = m.scale().clamp(0.05, 8.0);
        let (tw, th) = ((xs.abs() * scale).ceil(), (ys.abs() * scale).ceil());
        // Checked in floating point, before any cast can wrap.
        if !(tw >= 1.0 && th >= 1.0 && tw * th <= 4_000_000.0) {
            return None;
        }
        let (tw, th) = (tw as usize, th as usize);
        if tw == 0 || th == 0 {
            return None;
        }
        let data = self.doc.stream_data(s);
        // Coverage: draw the cell onto white and onto black.
        let mut sub = self.new_blank(tw, th, scale, &bbox, d, uncolored, color);
        sub.run(&data);
        let white = sub.canvas;
        let mut sub2 = self.new_blank(tw, th, scale, &bbox, d, uncolored, color);
        sub2.canvas.px.fill([0; 3]);
        sub2.run(&data);
        let alpha = white
            .px
            .iter()
            .zip(&sub2.canvas.px)
            .map(|(w, b)| 255 - (w[0] as i32 - b[0] as i32).clamp(0, 255) as u8)
            .collect();
        Some(Tile { img: white, alpha, scale, step: (xs, ys), origin: (bbox[0], bbox[1]) })
    }

    #[allow(clippy::too_many_arguments)]
    fn new_blank(
        &self,
        tw: usize,
        th: usize,
        scale: f64,
        bbox: &[f64; 4],
        d: &Dict,
        uncolored: bool,
        color: [u8; 3],
    ) -> Renderer<'a> {
        let res = self.doc.get_key(d, "Resources").as_dict().cloned().unwrap_or_default();
        let mut gs = GState::new(Matrix([scale, 0.0, 0.0, -scale, -bbox[0] * scale, th as f64 + bbox[1] * scale]));
        if uncolored {
            gs.fill = Paint::Color(color);
            gs.stroke = Paint::Color(color);
        }
        Renderer {
            doc: self.doc,
            fonts: self.fonts,
            canvas: Canvas::new(tw, th),
            gs,
            stack: Vec::new(),
            resources: vec![res],
            path: Path::default(),
            pending_clip: None,
            tm: Matrix::IDENTITY,
            tlm: Matrix::IDENTITY,
            text_clip: None,
            font_cache: BTreeMap::new(),
            font_by_ref: BTreeMap::new(),
            depth: self.depth + 1,
            ops: 0,
        }
    }

    /// Fills a device-space path with `paint`.
    fn paint_path(&mut self, path: &Path, rule: Rule, stroke: Option<StrokeStyle>, paint: &Paint, alpha: f32) {
        let clip = self.gs.clip.clone();
        let clip = clip.as_deref();
        let mut f = paint_fn(paint);
        match stroke {
            Some(st) => self.canvas.stroke(path, &st, clip, alpha, &mut *f),
            None => self.canvas.fill(path, rule, clip, alpha, &mut *f),
        }
    }

    fn stroke_style(&self) -> StrokeStyle {
        let s = self.gs.ctm.scale();
        StrokeStyle {
            width: (self.gs.line_width * s).max(1.0),
            cap: self.gs.cap,
            join: self.gs.join,
            miter_limit: self.gs.miter,
            dash: self.gs.dash.iter().map(|d| d * s).collect(),
            phase: self.gs.phase * s,
        }
    }

    fn end_path(&mut self) {
        if let Some(rule) = self.pending_clip.take() {
            let m = Mask::from_path(&self.path, rule, self.canvas.w, self.canvas.h);
            self.intersect_clip(m);
        }
        self.path = Path::default();
    }

    fn intersect_clip(&mut self, m: Mask) {
        let new = match &self.gs.clip {
            Some(c) => c.intersect(&m),
            None => m,
        };
        self.gs.clip = Some(Rc::new(new));
    }

    fn point(&self, x: f64, y: f64) -> (f64, f64) {
        self.gs.ctm.apply(x, y)
    }

    fn op(&mut self, op: &[u8], a: &[Object]) {
        match op {
            b"q" => self.save(),
            b"Q" => self.restore(),
            b"cm" => {
                if a.len() >= 6 {
                    let m = Matrix([n(a, 0), n(a, 1), n(a, 2), n(a, 3), n(a, 4), n(a, 5)]);
                    self.gs.ctm = m.then(&self.gs.ctm);
                }
            }
            b"w" => self.gs.line_width = n(a, 0),
            b"J" => self.gs.cap = n(a, 0).clamp(0.0, 2.0) as u8,
            b"j" => self.gs.join = n(a, 0).clamp(0.0, 2.0) as u8,
            b"M" => self.gs.miter = n(a, 0).max(1.0),
            b"d" => {
                self.gs.dash = a
                    .first()
                    .and_then(Object::as_array)
                    .map(|d| d.iter().filter_map(Object::as_f64).collect())
                    .unwrap_or_default();
                self.gs.phase = n(a, 1);
            }
            b"gs" => {
                if let Some(nm) = a.first().and_then(Object::as_name) {
                    if let Some(g) = self.resource("ExtGState", nm) {
                        self.ext_gstate(&self.doc.resolve(&g));
                    }
                }
            }
            b"m" => {
                let (x, y) = self.point(n(a, 0), n(a, 1));
                self.path.segs.push(Seg::Move(x, y));
            }
            b"l" => {
                let (x, y) = self.point(n(a, 0), n(a, 1));
                self.path.segs.push(Seg::Line(x, y));
            }
            b"c" => {
                let (x1, y1) = self.point(n(a, 0), n(a, 1));
                let (x2, y2) = self.point(n(a, 2), n(a, 3));
                let (x3, y3) = self.point(n(a, 4), n(a, 5));
                self.path.segs.push(Seg::Cubic(x1, y1, x2, y2, x3, y3));
            }
            b"v" | b"y" => {
                let cur = self.current_point();
                let (p1, p2) = (self.point(n(a, 0), n(a, 1)), self.point(n(a, 2), n(a, 3)));
                let seg = if op == b"v" {
                    Seg::Cubic(cur.0, cur.1, p1.0, p1.1, p2.0, p2.1)
                } else {
                    Seg::Cubic(p1.0, p1.1, p2.0, p2.1, p2.0, p2.1)
                };
                self.path.segs.push(seg);
            }
            b"h" => self.path.segs.push(Seg::Close),
            b"re" => {
                let (x, y, w, h) = (n(a, 0), n(a, 1), n(a, 2), n(a, 3));
                let p = [self.point(x, y), self.point(x + w, y), self.point(x + w, y + h), self.point(x, y + h)];
                self.path.segs.push(Seg::Move(p[0].0, p[0].1));
                for q in &p[1..] {
                    self.path.segs.push(Seg::Line(q.0, q.1));
                }
                self.path.segs.push(Seg::Close);
            }
            b"S" | b"s" => {
                if op == b"s" {
                    self.path.segs.push(Seg::Close);
                }
                self.stroke_current();
                self.end_path();
            }
            b"f" | b"F" | b"f*" => {
                let rule = if op == b"f*" { Rule::EvenOdd } else { Rule::NonZero };
                self.fill_current(rule);
                self.end_path();
            }
            b"B" | b"B*" | b"b" | b"b*" => {
                if op[0] == b'b' {
                    self.path.segs.push(Seg::Close);
                }
                let rule = if op.ends_with(b"*") { Rule::EvenOdd } else { Rule::NonZero };
                self.fill_current(rule);
                self.stroke_current();
                self.end_path();
            }
            b"n" => self.end_path(),
            b"W" => self.pending_clip = Some(Rule::NonZero),
            b"W*" => self.pending_clip = Some(Rule::EvenOdd),
            b"CS" | b"cs" => {
                let sp = a.first().map(|o| self.color_space(o)).unwrap_or(Space::Gray);
                let init = sp.initial();
                let c =
                    if matches!(sp, Space::Pattern(_)) { Paint::Color([0; 3]) } else { Paint::Color(sp.to_rgb(&init)) };
                if op == b"CS" {
                    self.gs.stroke_space = sp;
                    self.gs.stroke = c;
                } else {
                    self.gs.fill_space = sp;
                    self.gs.fill = c;
                }
            }
            b"SC" | b"SCN" => self.set_color(true, a),
            b"sc" | b"scn" => self.set_color(false, a),
            b"G" | b"g" | b"RG" | b"rg" | b"K" | b"k" => {
                let sp = match op {
                    b"G" | b"g" => Space::Gray,
                    b"RG" | b"rg" => Space::Rgb,
                    _ => Space::Cmyk,
                };
                let comps: Vec<f64> = a.iter().filter_map(Object::as_f64).collect();
                let c = Paint::Color(sp.to_rgb(&comps));
                if op[0].is_ascii_uppercase() {
                    self.gs.stroke_space = sp;
                    self.gs.stroke = c;
                } else {
                    self.gs.fill_space = sp;
                    self.gs.fill = c;
                }
            }
            b"sh" => {
                if let Some(s) = a.first().and_then(Object::as_name).and_then(|nm| self.resource("Shading", nm)) {
                    if let Some(sh) = self.shading(&s) {
                        let full = Path {
                            segs: vec![
                                Seg::Move(0.0, 0.0),
                                Seg::Line(self.canvas.w as f64, 0.0),
                                Seg::Line(self.canvas.w as f64, self.canvas.h as f64),
                                Seg::Line(0.0, self.canvas.h as f64),
                                Seg::Close,
                            ],
                        };
                        let paint = Paint::Shading(Rc::new(sh), self.gs.ctm);
                        let alpha = self.gs.fill_alpha;
                        self.paint_path(&full, Rule::NonZero, None, &paint, alpha);
                    }
                }
            }
            b"BT" => {
                self.tm = Matrix::IDENTITY;
                self.tlm = Matrix::IDENTITY;
            }
            b"ET" => {
                if let Some(p) = self.text_clip.take() {
                    let m = Mask::from_path(&p, Rule::NonZero, self.canvas.w, self.canvas.h);
                    self.intersect_clip(m);
                }
            }
            b"Tc" => self.gs.char_space = n(a, 0),
            b"Tw" => self.gs.word_space = n(a, 0),
            b"Tz" => self.gs.hscale = n(a, 0) / 100.0,
            b"TL" => self.gs.leading = n(a, 0),
            b"Ts" => self.gs.rise = n(a, 0),
            b"Tr" => self.gs.render_mode = n(a, 0) as i64,
            b"Tf" => {
                self.gs.size = n(a, 1);
                if let Some(nm) = a.first().and_then(Object::as_name) {
                    self.gs.font = self.font(nm);
                }
            }
            b"Td" => {
                self.tlm = Matrix([1.0, 0.0, 0.0, 1.0, n(a, 0), n(a, 1)]).then(&self.tlm);
                self.tm = self.tlm;
            }
            b"TD" => {
                self.gs.leading = -n(a, 1);
                self.tlm = Matrix([1.0, 0.0, 0.0, 1.0, n(a, 0), n(a, 1)]).then(&self.tlm);
                self.tm = self.tlm;
            }
            b"Tm" => {
                self.tlm = Matrix([n(a, 0), n(a, 1), n(a, 2), n(a, 3), n(a, 4), n(a, 5)]);
                self.tm = self.tlm;
            }
            b"T*" => self.next_line(),
            b"Tj" => {
                if let Some(s) = a.first().and_then(Object::as_str) {
                    self.show(s);
                }
            }
            b"'" => {
                self.next_line();
                if let Some(s) = a.first().and_then(Object::as_str) {
                    self.show(s);
                }
            }
            b"\"" => {
                self.gs.word_space = n(a, 0);
                self.gs.char_space = n(a, 1);
                self.next_line();
                if let Some(s) = a.get(2).and_then(Object::as_str) {
                    self.show(s);
                }
            }
            b"TJ" => {
                for item in a.first().and_then(Object::as_array).unwrap_or(&[]) {
                    match item {
                        Object::Str(s) => self.show(s),
                        o => {
                            if let Some(v) = o.as_f64() {
                                let tx = -v / 1000.0 * self.gs.size * self.gs.hscale;
                                let vertical = self.gs.font.as_ref().is_some_and(|f| f.vertical);
                                let t = if vertical {
                                    Matrix([1.0, 0.0, 0.0, 1.0, 0.0, -v / 1000.0 * self.gs.size])
                                } else {
                                    Matrix([1.0, 0.0, 0.0, 1.0, tx, 0.0])
                                };
                                self.tm = t.then(&self.tm);
                            }
                        }
                    }
                }
            }
            b"Do" => {
                if let Some(nm) = a.first().and_then(Object::as_name) {
                    if let Some(x) = self.resource("XObject", nm) {
                        self.xobject(&x);
                    }
                }
            }
            b"d0" | b"d1" => {}
            _ => {}
        }
    }

    fn current_point(&self) -> (f64, f64) {
        let mut start = (0.0, 0.0);
        let mut cur = (0.0, 0.0);
        for s in &self.path.segs {
            match *s {
                Seg::Move(x, y) => {
                    start = (x, y);
                    cur = (x, y);
                }
                Seg::Line(x, y) | Seg::Cubic(_, _, _, _, x, y) => cur = (x, y),
                Seg::Close => cur = start,
            }
        }
        cur
    }

    fn fill_current(&mut self, rule: Rule) {
        let path = self.device_path(&self.path);
        let paint = self.gs.fill.clone();
        let alpha = self.gs.fill_alpha;
        self.paint_path(&path, rule, None, &paint, alpha);
    }

    fn stroke_current(&mut self) {
        let path = self.device_path(&self.path);
        let paint = self.gs.stroke.clone();
        let st = self.stroke_style();
        let alpha = self.gs.stroke_alpha;
        self.paint_path(&path, Rule::NonZero, Some(st), &paint, alpha);
    }

    fn color_space(&self, o: &Object) -> Space {
        if let Some(sp) = color::space(self.doc, o, 0) {
            return sp;
        }
        if let Some(nm) = o.as_name() {
            if let Some(cs) = self.resource("ColorSpace", nm) {
                if let Some(sp) = color::space(self.doc, &cs, 0) {
                    return sp;
                }
            }
        }
        Space::Gray
    }

    fn ext_gstate(&mut self, g: &Object) {
        let Some(d) = g.as_dict() else { return };
        for (k, v) in d {
            let v = self.doc.resolve(v);
            match k.as_slice() {
                b"LW" => self.gs.line_width = v.as_f64().unwrap_or(1.0),
                b"LC" => self.gs.cap = v.as_i64().unwrap_or(0).clamp(0, 2) as u8,
                b"LJ" => self.gs.join = v.as_i64().unwrap_or(0).clamp(0, 2) as u8,
                b"ML" => self.gs.miter = v.as_f64().unwrap_or(10.0),
                b"D" => {
                    if let Some(arr) = v.as_array() {
                        self.gs.dash = arr
                            .first()
                            .and_then(Object::as_array)
                            .map(|d| d.iter().filter_map(Object::as_f64).collect())
                            .unwrap_or_default();
                        self.gs.phase = arr.get(1).and_then(Object::as_f64).unwrap_or(0.0);
                    }
                }
                b"CA" => self.gs.stroke_alpha = v.as_f64().unwrap_or(1.0).clamp(0.0, 1.0) as f32,
                b"ca" => self.gs.fill_alpha = v.as_f64().unwrap_or(1.0).clamp(0.0, 1.0) as f32,
                b"Font" => {
                    if let Some(arr) = v.as_array() {
                        if let Some(Object::Ref(r)) = arr.first() {
                            self.gs.font = self.font_ref(r.num);
                        }
                        self.gs.size = arr.get(1).and_then(Object::as_f64).unwrap_or(self.gs.size);
                    }
                }
                _ => {}
            }
        }
    }

    fn font(&mut self, name: &[u8]) -> Option<Rc<Font>> {
        let o = self.resource("Font", name)?;
        if let Object::Ref(r) = o {
            return self.font_ref(r.num);
        }
        if let Some(f) = self.font_cache.get(name) {
            return Some(f.clone());
        }
        let d = self.doc.resolve(&o);
        let f = Rc::new(Font::load(self.doc, d.as_dict()?, self.fonts));
        self.font_cache.insert(name.to_vec(), f.clone());
        Some(f)
    }

    fn font_ref(&mut self, num: u32) -> Option<Rc<Font>> {
        if let Some(f) = self.font_by_ref.get(&num) {
            return Some(f.clone());
        }
        let d = self.doc.get(num)?;
        let f = Rc::new(Font::load(self.doc, d.as_dict()?, self.fonts));
        self.font_by_ref.insert(num, f.clone());
        Some(f)
    }

    fn next_line(&mut self) {
        self.tlm = Matrix([1.0, 0.0, 0.0, 1.0, 0.0, -self.gs.leading]).then(&self.tlm);
        self.tm = self.tlm;
    }

    fn show(&mut self, s: &[u8]) {
        let Some(font) = self.gs.font.clone() else { return };
        let size = self.gs.size;
        let h = self.gs.hscale;
        let mode = self.gs.render_mode;
        for (code, k) in font.decode(s) {
            let trm = Matrix([size * h, 0.0, 0.0, size, 0.0, self.gs.rise]).then(&self.tm).then(&self.gs.ctm);
            let w0 = if font.type3.is_some() { font.width(k) * font.matrix[0] } else { font.width(k) / 1000.0 };
            if mode != 3 && mode != 7 {
                if font.type3.is_some() {
                    self.type3_glyph(&font, code, &trm);
                } else if let Some(g) = font.glyph(k) {
                    let dev = g.transform(&trm);
                    if matches!(mode, 0 | 2 | 4 | 6) {
                        let paint = self.gs.fill.clone();
                        let alpha = self.gs.fill_alpha;
                        self.paint_path(&dev, Rule::NonZero, None, &paint, alpha);
                    }
                    if matches!(mode, 1 | 2 | 5 | 6) {
                        let paint = self.gs.stroke.clone();
                        let st = self.stroke_style();
                        let alpha = self.gs.stroke_alpha;
                        self.paint_path(&dev, Rule::NonZero, Some(st), &paint, alpha);
                    }
                    if mode >= 4 {
                        self.text_clip.get_or_insert_with(Path::default).segs.extend(dev.segs);
                    }
                }
            } else if mode == 7 {
                if let Some(g) = font.glyph(k) {
                    self.text_clip.get_or_insert_with(Path::default).segs.extend(g.transform(&trm).segs);
                }
            }
            // Advance.
            let word = if font.single_byte && code == 32 { self.gs.word_space } else { 0.0 };
            if font.vertical {
                let ty = -(w0 * size) + self.gs.char_space + word;
                self.tm = Matrix([1.0, 0.0, 0.0, 1.0, 0.0, ty]).then(&self.tm);
            } else {
                let tx = (w0 * size + self.gs.char_space + word) * h;
                self.tm = Matrix([1.0, 0.0, 0.0, 1.0, tx, 0.0]).then(&self.tm);
            }
        }
    }

    fn type3_glyph(&mut self, font: &Rc<Font>, code: u32, trm: &Matrix) {
        if self.depth > 6 {
            return;
        }
        let Some((procs, res)) = &font.type3 else { return };
        let name = font.code_name(code);
        let Some(proc_) = procs.get(&name) else { return };
        let data = self.doc.stream_data(proc_);
        let fm = Matrix(font.matrix);
        self.save();
        self.gs.ctm = fm.then(trm);
        self.resources.push(res.clone());
        let (tm, tlm) = (self.tm, self.tlm);
        self.depth += 1;
        self.run(&data);
        self.depth -= 1;
        self.tm = tm;
        self.tlm = tlm;
        self.resources.pop();
        self.restore();
    }

    fn xobject(&mut self, x: &Object) {
        let Object::Stream(s) = self.doc.resolve(x) else { return };
        match self.doc.get_key(&s.dict, "Subtype").as_name() {
            Some(b"Image") => {
                let res_space = |o: &Object| -> Option<Space> { o.as_name().map(|_| Space::Gray) };
                let cs = s.dict.get(&b"ColorSpace"[..]).cloned();
                // Named colour spaces resolve through the resources.
                let mut d = s.dict.clone();
                if let Some(Object::Name(nm)) = &cs {
                    if color::space(self.doc, &Object::Name(nm.clone()), 0).is_none() {
                        if let Some(o) = self.resource("ColorSpace", nm) {
                            d.insert(b"ColorSpace".to_vec(), o);
                        }
                    }
                }
                if let Some(img) = image::decode(self.doc, &d, &s.data, &res_space) {
                    self.draw_image(&img);
                }
            }
            Some(b"Form") => self.form(&s),
            _ => {}
        }
    }

    fn form(&mut self, s: &Rc<Stream>) {
        if self.depth > 12 {
            return;
        }
        let d = &s.dict;
        let m = self.matrix_of(d, "Matrix");
        self.save();
        self.gs.ctm = m.then(&self.gs.ctm);
        if let Some(b) = self.doc.rect(d, "BBox") {
            let p = [self.point(b[0], b[1]), self.point(b[2], b[1]), self.point(b[2], b[3]), self.point(b[0], b[3])];
            let path = Path {
                segs: vec![
                    Seg::Move(p[0].0, p[0].1),
                    Seg::Line(p[1].0, p[1].1),
                    Seg::Line(p[2].0, p[2].1),
                    Seg::Line(p[3].0, p[3].1),
                    Seg::Close,
                ],
            };
            let mask = Mask::from_path(&path, Rule::NonZero, self.canvas.w, self.canvas.h);
            self.intersect_clip(mask);
        }
        let res = self.doc.get_key(d, "Resources").as_dict().cloned();
        let pushed = res.is_some();
        if let Some(r) = res {
            self.resources.push(r);
        }
        let data = self.doc.stream_data(s);
        let (path, tm, tlm) = (core::mem::take(&mut self.path), self.tm, self.tlm);
        self.depth += 1;
        self.run(&data);
        self.depth -= 1;
        self.path = path;
        self.tm = tm;
        self.tlm = tlm;
        if pushed {
            self.resources.pop();
        }
        self.restore();
    }

    fn inline_image(&mut self, lx: &mut Lexer) {
        let mut d = Dict::new();
        loop {
            let Some(t) = lx.next() else { return };
            match t {
                Token::Word(w) if w == b"ID" => break,
                Token::Name(k) => {
                    let Some(v) = super::object::parse_object(lx, 0) else { return };
                    d.insert(k, v);
                }
                _ => {}
            }
        }
        // One white-space byte, then data up to "EI" between white space.
        let data = lx.data;
        let start = lx.pos + 1;
        let mut end = start;
        while end + 2 <= data.len() {
            if &data[end..end + 2] == b"EI"
                && (end == 0 || data[end - 1].is_ascii_whitespace())
                && data.get(end + 2).is_none_or(|c| c.is_ascii_whitespace())
            {
                break;
            }
            end += 1;
        }
        lx.pos = (end + 2).min(data.len());
        let mut raw_end = end;
        if raw_end > start && data[raw_end - 1].is_ascii_whitespace() {
            raw_end -= 1;
        }
        let raw = data[start.min(raw_end)..raw_end].to_vec();
        // Inline colour spaces may name resources.
        if let Some(Object::Name(nm)) = d.get(&b"CS"[..]).or_else(|| d.get(&b"ColorSpace"[..])).cloned() {
            if color::space(self.doc, &Object::Name(nm.clone()), 0).is_none() {
                if let Some(o) = self.resource("ColorSpace", &nm) {
                    d.insert(b"ColorSpace".to_vec(), o);
                    d.remove(&b"CS"[..]);
                }
            }
        }
        let none = |_: &Object| None;
        if let Some(img) = image::decode(self.doc, &d, &raw, &none) {
            self.draw_image(&img);
        }
    }

    /// Draws an image into the unit square of the CTM.
    fn draw_image(&mut self, img: &Image) {
        let ctm = self.gs.ctm;
        // Image space: (0,0) top-left of the image is (0,1) in user space.
        let to_img = Matrix([img.w as f64, 0.0, 0.0, -(img.h as f64), 0.0, img.h as f64]);
        let Some(inv) = ctm.invert() else { return };
        let inv = inv.then(&to_img);
        let corners = [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)].map(|(x, y)| ctm.apply(x, y));
        let x0 = corners.iter().fold(f64::MAX, |a, p| a.min(p.0)).floor().max(0.0) as usize;
        let x1 = (corners.iter().fold(f64::MIN, |a, p| a.max(p.0)).ceil().max(0.0) as usize).min(self.canvas.w);
        let y0 = corners.iter().fold(f64::MAX, |a, p| a.min(p.1)).floor().max(0.0) as usize;
        let y1 = (corners.iter().fold(f64::MIN, |a, p| a.max(p.1)).ceil().max(0.0) as usize).min(self.canvas.h);
        // Shrunk images: average over the source pixels a device pixel
        // covers (a box filter on a reduced copy).
        let dev_w = libm::hypot(ctm.0[0], ctm.0[1]);
        let dev_h = libm::hypot(ctm.0[2], ctm.0[3]);
        let factor = ((img.w as f64 / dev_w.max(1e-9)).min(img.h as f64 / dev_h.max(1e-9)) as usize).clamp(1, 64);
        let reduced;
        let src: &Image = if factor >= 2 && !img.stencil {
            reduced = shrink(img, factor);
            &reduced
        } else {
            img
        };
        let sx = src.w as f64 / img.w as f64;
        let sy = src.h as f64 / img.h as f64;
        let fill = match &self.gs.fill {
            Paint::Color(c) => *c,
            _ => [0; 3],
        };
        let alpha = self.gs.fill_alpha;
        let clip = self.gs.clip.clone();
        let smooth = !img.stencil && (dev_w > img.w as f64 * 1.5 || dev_h > img.h as f64 * 1.5);
        for y in y0..y1 {
            if let Some(c) = &clip {
                if y < c.y0 || y >= c.y1 {
                    continue;
                }
            }
            for x in x0..x1 {
                let (u, v) = inv.apply(x as f64 + 0.5, y as f64 + 0.5);
                if u < 0.0 || v < 0.0 || u >= img.w as f64 || v >= img.h as f64 {
                    continue;
                }
                let (u, v) = (u * sx, v * sy);
                let p = if smooth {
                    bilinear(src, u - 0.5, v - 0.5)
                } else {
                    src.px[(v as usize).min(src.h - 1) * src.w + (u as usize).min(src.w - 1)]
                };
                let mut a = p[3] as f32 * alpha;
                if let Some(c) = &clip {
                    a = a * c.at(x, y) as f32 / 255.0;
                }
                let col = if img.stencil { fill } else { [p[0], p[1], p[2]] };
                self.canvas.blend(x, y, col, (a + 0.5) as u32);
            }
        }
    }
}

fn shrink(img: &Image, f: usize) -> Image {
    let (w, h) = (img.w.div_ceil(f), img.h.div_ceil(f));
    let mut px = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0u32; 4];
            let mut cnt = 0u32;
            for yy in y * f..((y + 1) * f).min(img.h) {
                for xx in x * f..((x + 1) * f).min(img.w) {
                    let p = img.px[yy * img.w + xx];
                    let a = p[3] as u32;
                    for k in 0..3 {
                        acc[k] += p[k] as u32 * a;
                    }
                    acc[3] += a;
                    cnt += 1;
                }
            }
            let a = acc[3].max(1);
            px.push([(acc[0] / a) as u8, (acc[1] / a) as u8, (acc[2] / a) as u8, (acc[3] / cnt.max(1)) as u8]);
        }
    }
    Image { w, h, px, stencil: img.stencil }
}

fn bilinear(img: &Image, u: f64, v: f64) -> [u8; 4] {
    let x0 = libm::floor(u).clamp(0.0, (img.w - 1) as f64) as usize;
    let y0 = libm::floor(v).clamp(0.0, (img.h - 1) as f64) as usize;
    let x1 = (x0 + 1).min(img.w - 1);
    let y1 = (y0 + 1).min(img.h - 1);
    let fx = (u - libm::floor(u)).clamp(0.0, 1.0) as f32;
    let fy = (v - libm::floor(v)).clamp(0.0, 1.0) as f32;
    let p = |x: usize, y: usize| img.px[y * img.w + x];
    let (a, b, c, d) = (p(x0, y0), p(x1, y0), p(x0, y1), p(x1, y1));
    let mut out = [0u8; 4];
    for k in 0..4 {
        let top = a[k] as f32 + (b[k] as f32 - a[k] as f32) * fx;
        let bot = c[k] as f32 + (d[k] as f32 - c[k] as f32) * fx;
        out[k] = (top + (bot - top) * fy + 0.5) as u8;
    }
    out
}

/// A per-pixel colour source for a paint.
fn paint_fn(p: &Paint) -> alloc::boxed::Box<dyn FnMut(usize, usize) -> Option<[u8; 3]> + '_> {
    match p {
        Paint::Color(c) => {
            let c = *c;
            alloc::boxed::Box::new(move |_, _| Some(c))
        }
        Paint::Shading(sh, m) => {
            let inv = m.invert().unwrap_or(Matrix::IDENTITY);
            alloc::boxed::Box::new(move |x, y| {
                let (u, v) = inv.apply(x as f64 + 0.5, y as f64 + 0.5);
                sh.color_at(u, v)
            })
        }
        Paint::Tiling(t, m) => {
            let inv = m.invert().unwrap_or(Matrix::IDENTITY);
            alloc::boxed::Box::new(move |x, y| {
                let (u, v) = inv.apply(x as f64 + 0.5, y as f64 + 0.5);
                let fu = (u - t.origin.0).rem_euclid(t.step.0.abs());
                let fv = (v - t.origin.1).rem_euclid(t.step.1.abs());
                let tx = ((fu * t.scale) as usize).min(t.img.w - 1);
                let ty = (t.img.h - 1).saturating_sub(((fv * t.scale) as usize).min(t.img.h - 1));
                let i = ty * t.img.w + tx;
                let c = t.img.px[i];
                (t.alpha[i] >= 128).then_some(c)
            })
        }
    }
}
