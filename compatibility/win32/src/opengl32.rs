//! `opengl32.dll`: OpenGL 1.1 in software, for programs that draw 2D
//! interfaces with the fixed-function pipeline.
//!
//! WGL contexts render into a back buffer the size of their window's
//! client area; SwapBuffers copies it into the window and presents it.
//! Implemented: the projection, modelview and texture matrix stacks,
//! immediate mode (points, lines, triangles, quads, strips, fans,
//! polygons) with colour and texture coordinates, 2D textures (RGBA, BGRA,
//! RGB, BGR, alpha, luminance; nearest and linear filtering, repeat and
//! clamp), the texture environment (modulate, replace, decal), blending,
//! the scissor test and glClear. There is no depth buffer: depth-test
//! state is accepted and every fragment passes, which is what 2D drawing
//! sees. Selection and feedback render modes are not offered.
#![allow(non_snake_case)]

use alloc::{collections::BTreeMap, sync::Arc, vec, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering};
use os_std::sync::Mutex;

type Enum = u32;

const GL_NO_ERROR: Enum = 0;
const GL_INVALID_ENUM: Enum = 0x0500;
const GL_INVALID_VALUE: Enum = 0x0501;
const GL_INVALID_OPERATION: Enum = 0x0502;
const GL_STACK_OVERFLOW: Enum = 0x0503;
const GL_STACK_UNDERFLOW: Enum = 0x0504;

const GL_POINTS: Enum = 0;
const GL_LINES: Enum = 1;
const GL_LINE_LOOP: Enum = 2;
const GL_LINE_STRIP: Enum = 3;
const GL_TRIANGLES: Enum = 4;
const GL_TRIANGLE_STRIP: Enum = 5;
const GL_TRIANGLE_FAN: Enum = 6;
const GL_QUADS: Enum = 7;
const GL_QUAD_STRIP: Enum = 8;
const GL_POLYGON: Enum = 9;

const GL_MODELVIEW: Enum = 0x1700;
const GL_PROJECTION: Enum = 0x1701;
const GL_TEXTURE: Enum = 0x1702;

const GL_BLEND: Enum = 0x0BE2;
const GL_TEXTURE_2D: Enum = 0x0DE1;
const GL_SCISSOR_TEST: Enum = 0x0C11;

const GL_TEXTURE_MAG_FILTER: Enum = 0x2800;
const GL_TEXTURE_MIN_FILTER: Enum = 0x2801;
const GL_TEXTURE_WRAP_S: Enum = 0x2802;
const GL_TEXTURE_WRAP_T: Enum = 0x2803;
const GL_NEAREST: i32 = 0x2600;
const GL_NEAREST_MIPMAP_NEAREST: i32 = 0x2700;
const GL_NEAREST_MIPMAP_LINEAR: i32 = 0x2702;
const GL_REPEAT: i32 = 0x2901;

const GL_TEXTURE_ENV_MODE: Enum = 0x2200;
const GL_MODULATE: i32 = 0x2100;
const GL_DECAL: i32 = 0x2101;
const GL_REPLACE: i32 = 0x1E01;

const GL_UNPACK_ROW_LENGTH: Enum = 0x0CF2;
const GL_UNPACK_SKIP_ROWS: Enum = 0x0CF3;
const GL_UNPACK_SKIP_PIXELS: Enum = 0x0CF4;
const GL_UNPACK_ALIGNMENT: Enum = 0x0CF5;

const GL_COLOR_BUFFER_BIT: u32 = 0x4000;
const GL_RENDER: Enum = 0x1C00;

const MAX_TEXTURE_SIZE: u32 = 4096;
const STACK_DEPTH: usize = 32;

/// Rounding without std (the values here are pixel coordinates, well
/// inside i64).
trait Round {
    fn floor(self) -> Self;
    fn ceil(self) -> Self;
    fn round(self) -> Self;
}

impl Round for f32 {
    fn floor(self) -> f32 {
        let t = self as i64 as f32;
        if t > self { t - 1. } else { t }
    }
    fn ceil(self) -> f32 {
        let t = self as i64 as f32;
        if t < self { t + 1. } else { t }
    }
    fn round(self) -> f32 {
        Round::floor(self + 0.5)
    }
}

type Mat = [f32; 16];
const IDENTITY: Mat = [1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.];

/// Column-major product a * b.
fn mul(a: &Mat, b: &Mat) -> Mat {
    let mut r = [0f32; 16];
    for c in 0..4 {
        for row in 0..4 {
            r[c * 4 + row] = (0..4).map(|k| a[k * 4 + row] * b[c * 4 + k]).sum();
        }
    }
    r
}

fn apply(m: &Mat, v: [f32; 4]) -> [f32; 4] {
    let mut r = [0f32; 4];
    for (row, out) in r.iter_mut().enumerate() {
        *out = (0..4).map(|k| m[k * 4 + row] * v[k]).sum();
    }
    r
}

#[derive(Clone)]
struct Texture {
    w: u32,
    h: u32,
    /// 0xAARRGGBB texels, rows from the bottom (GL order).
    px: Vec<u32>,
    min_linear: bool,
    mag_linear: bool,
    repeat_s: bool,
    repeat_t: bool,
}

impl Texture {
    fn new() -> Texture {
        // Defaults: NEAREST_MIPMAP_LINEAR min (a level-0 texture samples
        // its base level), LINEAR mag, REPEAT wrap.
        Texture { w: 0, h: 0, px: Vec::new(), min_linear: false, mag_linear: true, repeat_s: true, repeat_t: true }
    }

    fn texel(&self, x: i64, y: i64) -> [f32; 4] {
        let wrap = |v: i64, n: u32, repeat: bool| if repeat { v.rem_euclid(n as i64) } else { v.clamp(0, n as i64 - 1) } as u32;
        let c = self.px[(wrap(y, self.h, self.repeat_t) * self.w + wrap(x, self.w, self.repeat_s)) as usize];
        [(c >> 16 & 255) as f32 / 255., (c >> 8 & 255) as f32 / 255., (c & 255) as f32 / 255., (c >> 24) as f32 / 255.]
    }

    fn sample(&self, s: f32, t: f32) -> [f32; 4] {
        let (u, v) = (s * self.w as f32, t * self.h as f32);
        // 2D drawing scales 1:1 or magnifies; the magnification filter
        // decides, the minification one when the texture is shrunk.
        if !self.mag_linear && !self.min_linear {
            return self.texel(u.floor() as i64, v.floor() as i64);
        }
        let (x, y) = (u - 0.5, v - 0.5);
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (x0, y0) = (x0 as i64, y0 as i64);
        let (a, b, c, d) = (self.texel(x0, y0), self.texel(x0 + 1, y0), self.texel(x0, y0 + 1), self.texel(x0 + 1, y0 + 1));
        let mut r = [0f32; 4];
        for k in 0..4 {
            r[k] = (a[k] * (1. - fx) + b[k] * fx) * (1. - fy) + (c[k] * (1. - fx) + d[k] * fx) * fy;
        }
        r
    }
}

#[derive(Default)]
struct Textures {
    next: u32,
    map: BTreeMap<u32, Texture>,
}

#[derive(Clone, Copy, Default)]
struct Vertex {
    /// Window coordinates (y down) and the attributes.
    x: f32,
    y: f32,
    color: [f32; 4],
    tex: [f32; 2],
}

struct Context {
    dc: u64,
    hwnd: u64,
    textures: Arc<Mutex<Textures>>,
    w: u32,
    h: u32,
    /// 0xAARRGGBB, top row first.
    color: Vec<u32>,
    viewport: [i32; 4],
    scissor: [i32; 4],
    matrix_mode: Enum,
    stacks: [Vec<Mat>; 3],
    blend: bool,
    texture_2d: bool,
    scissor_test: bool,
    blend_func: (Enum, Enum),
    clear_color: [f32; 4],
    cur_color: [f32; 4],
    cur_tex: [f32; 2],
    prim: Option<Enum>,
    verts: Vec<Vertex>,
    tex_env: i32,
    bound: u32,
    unpack: [i32; 4],
    line_width: f32,
    error: Enum,
}

impl Context {
    fn new(dc: u64, hwnd: u64, textures: Arc<Mutex<Textures>>) -> Context {
        Context {
            dc,
            hwnd,
            textures,
            w: 0,
            h: 0,
            color: Vec::new(),
            viewport: [0; 4],
            scissor: [0; 4],
            matrix_mode: GL_MODELVIEW,
            stacks: [vec![IDENTITY], vec![IDENTITY], vec![IDENTITY]],
            blend: false,
            texture_2d: false,
            scissor_test: false,
            blend_func: (1, 0),
            clear_color: [0.; 4],
            cur_color: [1.; 4],
            cur_tex: [0.; 2],
            prim: None,
            verts: Vec::new(),
            tex_env: GL_MODULATE,
            bound: 0,
            unpack: [0, 0, 0, 4],
            line_width: 1.,
            error: GL_NO_ERROR,
        }
    }

    fn set_error(&mut self, e: Enum) {
        if self.error == GL_NO_ERROR {
            self.error = e;
        }
    }

    /// Sizes the back buffer to the window's client area; the first time,
    /// the viewport and scissor box start as the whole window.
    fn fit(&mut self) {
        let (w, h) = crate::user32::client_size(self.hwnd).unwrap_or((self.w, self.h));
        if (w, h) != (self.w, self.h) {
            let first = self.w == 0 && self.h == 0;
            self.w = w;
            self.h = h;
            self.color = vec![0; (w * h) as usize];
            if first {
                self.viewport = [0, 0, w as i32, h as i32];
                self.scissor = [0, 0, w as i32, h as i32];
            }
        }
    }

    fn stack(&mut self) -> &mut Vec<Mat> {
        let i = match self.matrix_mode {
            GL_PROJECTION => 1,
            GL_TEXTURE => 2,
            _ => 0,
        };
        &mut self.stacks[i]
    }

    fn top(&mut self) -> &mut Mat {
        self.stack().last_mut().expect("matrix stacks are never empty")
    }

    fn mult(&mut self, m: &Mat) {
        let t = *self.top();
        *self.top() = mul(&t, m);
    }

    /// The pixels fragments may touch: the window, the scissor box.
    fn bounds(&self) -> (i32, i32, i32, i32) {
        let (mut x0, mut y0, mut x1, mut y1) = (0, 0, self.w as i32, self.h as i32);
        if self.scissor_test {
            let [sx, sy, sw, sh] = self.scissor;
            // GL's box is from the bottom; rows here are from the top.
            let top = self.h as i32 - (sy + sh);
            x0 = x0.max(sx);
            x1 = x1.min(sx + sw);
            y0 = y0.max(top);
            y1 = y1.min(top + sh);
        }
        (x0, y0, x1, y1)
    }

    fn vertex(&mut self, x: f32, y: f32, z: f32) {
        if self.prim.is_none() {
            return; // outside glBegin/glEnd: undefined, ignored
        }
        let mvp = mul(&self.stacks[1].last().copied().unwrap_or(IDENTITY), &self.stacks[0].last().copied().unwrap_or(IDENTITY));
        let c = apply(&mvp, [x, y, z, 1.]);
        let w = if c[3] == 0. { 1. } else { c[3] };
        let [vx, vy, vw, vh] = self.viewport;
        let wx = vx as f32 + (c[0] / w + 1.) * vw as f32 / 2.;
        let wy = vy as f32 + (c[1] / w + 1.) * vh as f32 / 2.;
        let t = apply(&self.stacks[2].last().copied().unwrap_or(IDENTITY), [self.cur_tex[0], self.cur_tex[1], 0., 1.]);
        self.verts.push(Vertex { x: wx, y: self.h as f32 - wy, color: self.cur_color, tex: [t[0], t[1]] });
    }

    fn blend_factor(f: Enum, src: [f32; 4], dst: [f32; 4]) -> [f32; 4] {
        match f {
            0 => [0.; 4],
            1 => [1.; 4],
            0x0300 => src,                                                   // SRC_COLOR
            0x0301 => src.map(|v| 1. - v),                                   // ONE_MINUS_SRC_COLOR
            0x0302 => [src[3]; 4],                                           // SRC_ALPHA
            0x0303 => [1. - src[3]; 4],                                      // ONE_MINUS_SRC_ALPHA
            0x0304 => [dst[3]; 4],                                           // DST_ALPHA
            0x0305 => [1. - dst[3]; 4],                                      // ONE_MINUS_DST_ALPHA
            0x0306 => dst,                                                   // DST_COLOR
            0x0307 => dst.map(|v| 1. - v),                                   // ONE_MINUS_DST_COLOR
            0x0308 => {
                // SRC_ALPHA_SATURATE
                let f = src[3].min(1. - dst[3]);
                [f, f, f, 1.]
            }
            _ => [1.; 4],
        }
    }

    fn fragment(&mut self, x: i32, y: i32, mut c: [f32; 4], tex: Option<(&Texture, [f32; 2])>) {
        if let Some((t, [s, tt])) = tex {
            let texel = t.sample(s, tt);
            c = match self.tex_env {
                GL_REPLACE => texel,
                GL_DECAL => {
                    let a = texel[3];
                    [c[0] * (1. - a) + texel[0] * a, c[1] * (1. - a) + texel[1] * a, c[2] * (1. - a) + texel[2] * a, c[3]]
                }
                _ => [c[0] * texel[0], c[1] * texel[1], c[2] * texel[2], c[3] * texel[3]],
            };
        }
        let i = (y as u32 * self.w + x as u32) as usize;
        let d = self.color[i];
        let dst = [(d >> 16 & 255) as f32 / 255., (d >> 8 & 255) as f32 / 255., (d & 255) as f32 / 255., (d >> 24) as f32 / 255.];
        if self.blend {
            let (sf, df) = (Self::blend_factor(self.blend_func.0, c, dst), Self::blend_factor(self.blend_func.1, c, dst));
            for k in 0..4 {
                c[k] = c[k] * sf[k] + dst[k] * df[k];
            }
        }
        let b = |v: f32| (v.clamp(0., 1.) * 255. + 0.5) as u32;
        self.color[i] = b(c[3]) << 24 | b(c[0]) << 16 | b(c[1]) << 8 | b(c[2]);
    }

    fn texture(&self) -> Option<Texture> {
        if !self.texture_2d {
            return None;
        }
        self.textures.lock().unwrap().map.get(&self.bound).filter(|t| t.w > 0 && t.h > 0).cloned()
    }

    fn triangle(&mut self, a: Vertex, b: Vertex, c: Vertex, tex: Option<&Texture>) {
        let area = (b.x - a.x) * (c.y - a.y) - (c.x - a.x) * (b.y - a.y);
        if area == 0. {
            return;
        }
        let (bx0, by0, bx1, by1) = self.bounds();
        let x0 = (a.x.min(b.x).min(c.x).floor() as i32).max(bx0);
        let x1 = (a.x.max(b.x).max(c.x).ceil() as i32).min(bx1);
        let y0 = (a.y.min(b.y).min(c.y).floor() as i32).max(by0);
        let y1 = (a.y.max(b.y).max(c.y).ceil() as i32).min(by1);
        for y in y0..y1 {
            for x in x0..x1 {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let wa = ((b.x - px) * (c.y - py) - (c.x - px) * (b.y - py)) / area;
                let wb = ((c.x - px) * (a.y - py) - (a.x - px) * (c.y - py)) / area;
                let wc = 1. - wa - wb;
                // Top-left fill rule approximated: half-open on the far
                // edges so shared edges are not drawn twice.
                if wa < 0. || wb < 0. || wc < 0. || (wa == 0. && a.x > px) || (wb == 0. && b.x > px) {
                    continue;
                }
                let mut col = [0f32; 4];
                for k in 0..4 {
                    col[k] = a.color[k] * wa + b.color[k] * wb + c.color[k] * wc;
                }
                let st = [a.tex[0] * wa + b.tex[0] * wb + c.tex[0] * wc, a.tex[1] * wa + b.tex[1] * wb + c.tex[1] * wc];
                self.fragment(x, y, col, tex.map(|t| (t, st)));
            }
        }
    }

    fn line(&mut self, a: Vertex, b: Vertex, tex: Option<&Texture>) {
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let steps = dx.abs().max(dy.abs()).ceil().max(1.) as i32;
        let width = self.line_width.round().max(1.) as i32;
        let (bx0, by0, bx1, by1) = self.bounds();
        for i in 0..steps {
            let t = i as f32 / steps as f32;
            let (x, y) = (a.x + dx * t, a.y + dy * t);
            let mut col = [0f32; 4];
            for k in 0..4 {
                col[k] = a.color[k] + (b.color[k] - a.color[k]) * t;
            }
            let st = [a.tex[0] + (b.tex[0] - a.tex[0]) * t, a.tex[1] + (b.tex[1] - a.tex[1]) * t];
            // Wide lines spread across the minor axis.
            for o in 0..width {
                let off = o - width / 2;
                let (px, py) = if dx.abs() >= dy.abs() { (x.floor() as i32, y.floor() as i32 + off) } else { (x.floor() as i32 + off, y.floor() as i32) };
                if px >= bx0 && px < bx1 && py >= by0 && py < by1 {
                    self.fragment(px, py, col, tex.map(|t| (t, st)));
                }
            }
        }
    }

    fn point(&mut self, v: Vertex, tex: Option<&Texture>) {
        let (bx0, by0, bx1, by1) = self.bounds();
        let (x, y) = (v.x.floor() as i32, v.y.floor() as i32);
        if x >= bx0 && x < bx1 && y >= by0 && y < by1 {
            self.fragment(x, y, v.color, tex.map(|t| (t, v.tex)));
        }
    }

    fn end(&mut self) {
        let Some(prim) = self.prim.take() else {
            self.set_error(GL_INVALID_OPERATION);
            return;
        };
        let v = core::mem::take(&mut self.verts);
        let tex = self.texture();
        let t = tex.as_ref();
        match prim {
            GL_POINTS => v.iter().for_each(|&p| self.point(p, t)),
            GL_LINES => v.chunks_exact(2).for_each(|p| self.line(p[0], p[1], t)),
            GL_LINE_STRIP | GL_LINE_LOOP => {
                for p in v.windows(2) {
                    self.line(p[0], p[1], t);
                }
                if prim == GL_LINE_LOOP && v.len() > 2 {
                    self.line(v[v.len() - 1], v[0], t);
                }
            }
            GL_TRIANGLES => v.chunks_exact(3).for_each(|p| self.triangle(p[0], p[1], p[2], t)),
            GL_TRIANGLE_STRIP => {
                for i in 2..v.len() {
                    self.triangle(v[i - 2], v[i - 1], v[i], t);
                }
            }
            GL_TRIANGLE_FAN | GL_POLYGON => {
                for i in 2..v.len() {
                    self.triangle(v[0], v[i - 1], v[i], t);
                }
            }
            GL_QUADS => v.chunks_exact(4).for_each(|q| {
                self.triangle(q[0], q[1], q[2], t);
                self.triangle(q[0], q[2], q[3], t);
            }),
            GL_QUAD_STRIP => {
                let mut i = 3;
                while i < v.len() {
                    self.triangle(v[i - 3], v[i - 2], v[i], t);
                    self.triangle(v[i - 3], v[i], v[i - 1], t);
                    i += 2;
                }
            }
            _ => {}
        }
    }
}

static CONTEXTS: Mutex<BTreeMap<u64, Arc<Mutex<Context>>>> = Mutex::new(BTreeMap::new());
/// The current context of each thread.
static CURRENT: Mutex<BTreeMap<u64, u64>> = Mutex::new(BTreeMap::new());
static NEXT: AtomicU64 = AtomicU64::new(0x4c00_0010);

fn current() -> Option<Arc<Mutex<Context>>> {
    let h = *CURRENT.lock().unwrap().get(&os_sys::gettid())?;
    CONTEXTS.lock().unwrap().get(&h).cloned()
}

/// Runs `f` on the current context; without one, GL calls do nothing.
fn with<R: Default>(f: impl FnOnce(&mut Context) -> R) -> R {
    match current() {
        Some(c) => f(&mut c.lock().unwrap()),
        None => R::default(),
    }
}

/// The finished frame of the context drawing into `hwnd`, for
/// SwapBuffers: (width, height, 0xAARRGGBB pixels from the top row).
pub(crate) fn frame(hwnd: u64) -> Option<(u32, u32, Vec<u32>)> {
    let contexts: Vec<_> = CONTEXTS.lock().unwrap().values().cloned().collect();
    contexts.iter().find_map(|c| {
        let c = c.lock().unwrap();
        (c.hwnd == hwnd && c.w > 0).then(|| (c.w, c.h, c.color.clone()))
    })
}

// ---- WGL -----------------------------------------------------------------------------------

extern "win64" fn wglCreateContext(dc: u64) -> u64 {
    let Some(hwnd) = crate::user32::dc_window(dc) else { return crate::kernel32::fail(crate::error::ERROR_INVALID_HANDLE, 0) };
    let h = NEXT.fetch_add(0x10, Ordering::Relaxed);
    let ctx = Context::new(dc, hwnd, Arc::new(Mutex::new(Textures::default())));
    CONTEXTS.lock().unwrap().insert(h, Arc::new(Mutex::new(ctx)));
    h
}

extern "win64" fn wglDeleteContext(h: u64) -> i32 {
    if CONTEXTS.lock().unwrap().remove(&h).is_none() {
        return crate::kernel32::fail(crate::error::ERROR_INVALID_HANDLE, 0);
    }
    CURRENT.lock().unwrap().retain(|_, c| *c != h);
    1
}

extern "win64" fn wglMakeCurrent(dc: u64, h: u64) -> i32 {
    let tid = os_sys::gettid();
    if h == 0 {
        CURRENT.lock().unwrap().remove(&tid);
        return 1;
    }
    let Some(c) = CONTEXTS.lock().unwrap().get(&h).cloned() else { return crate::kernel32::fail(crate::error::ERROR_INVALID_HANDLE, 0) };
    // A context is current in one thread at a time.
    if CURRENT.lock().unwrap().iter().any(|(t, x)| *x == h && *t != tid) {
        return crate::kernel32::fail(170, 0); // ERROR_BUSY
    }
    {
        let mut c = c.lock().unwrap();
        if dc != 0 {
            if let Some(hwnd) = crate::user32::dc_window(dc) {
                c.dc = dc;
                c.hwnd = hwnd;
            }
        }
        c.fit();
    }
    CURRENT.lock().unwrap().insert(tid, h);
    1
}

extern "win64" fn wglGetCurrentContext() -> u64 {
    CURRENT.lock().unwrap().get(&os_sys::gettid()).copied().unwrap_or(0)
}

extern "win64" fn wglGetCurrentDC() -> u64 {
    current().map_or(0, |c| c.lock().unwrap().dc)
}

/// wglShareLists: the second context uses the first one's texture names.
extern "win64" fn wglShareLists(from: u64, to: u64) -> i32 {
    let contexts = CONTEXTS.lock().unwrap();
    let (Some(a), Some(b)) = (contexts.get(&from), contexts.get(&to)) else { return crate::kernel32::fail(crate::error::ERROR_INVALID_HANDLE, 0) };
    let shared = a.lock().unwrap().textures.clone();
    let mut b = b.lock().unwrap();
    if !b.textures.lock().unwrap().map.is_empty() {
        return crate::kernel32::fail(crate::error::ERROR_INVALID_PARAMETER, 0); // the target already has objects
    }
    b.textures = shared;
    1
}

/// No extension functions are offered; core 1.1 functions are exports,
/// which wglGetProcAddress does not return on Windows either.
extern "win64" fn wglGetProcAddress(_name: *const u8) -> u64 {
    0
}

// ---- GL ------------------------------------------------------------------------------------

extern "win64" fn glGetError() -> Enum {
    with(|c| core::mem::replace(&mut c.error, GL_NO_ERROR))
}

extern "win64" fn glGetString(name: Enum) -> *const u8 {
    let s: &'static [u8] = match name {
        0x1F00 => b"OSyx\0",
        0x1F01 => b"OSyx software rasterizer\0",
        0x1F02 => b"1.1.0 OSyx\0",
        0x1F03 => b"GL_EXT_bgra\0",
        _ => {
            with(|c| c.set_error(GL_INVALID_ENUM));
            return core::ptr::null();
        }
    };
    if current().is_none() {
        return core::ptr::null();
    }
    s.as_ptr()
}

extern "win64" fn glMatrixMode(mode: Enum) {
    with(|c| if matches!(mode, GL_MODELVIEW | GL_PROJECTION | GL_TEXTURE) { c.matrix_mode = mode } else { c.set_error(GL_INVALID_ENUM) })
}

extern "win64" fn glLoadIdentity() {
    with(|c| *c.top() = IDENTITY)
}

extern "win64" fn glPushMatrix() {
    with(|c| {
        let t = *c.top();
        if c.stack().len() >= STACK_DEPTH { c.set_error(GL_STACK_OVERFLOW) } else { c.stack().push(t) }
    })
}

extern "win64" fn glPopMatrix() {
    with(|c| if c.stack().len() <= 1 { c.set_error(GL_STACK_UNDERFLOW) } else { c.stack().pop(); })
}

extern "win64" fn glOrtho(l: f64, r: f64, b: f64, t: f64, n: f64, f: f64) {
    with(|c| {
        if l == r || b == t || n == f {
            return c.set_error(GL_INVALID_VALUE);
        }
        let m: Mat = [
            (2. / (r - l)) as f32, 0., 0., 0.,
            0., (2. / (t - b)) as f32, 0., 0.,
            0., 0., (-2. / (f - n)) as f32, 0.,
            (-(r + l) / (r - l)) as f32, (-(t + b) / (t - b)) as f32, (-(f + n) / (f - n)) as f32, 1.,
        ];
        c.mult(&m);
    })
}

extern "win64" fn glTranslatef(x: f32, y: f32, z: f32) {
    with(|c| c.mult(&[1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., x, y, z, 1.]))
}

extern "win64" fn glScalef(x: f32, y: f32, z: f32) {
    with(|c| c.mult(&[x, 0., 0., 0., 0., y, 0., 0., 0., 0., z, 0., 0., 0., 0., 1.]))
}

extern "win64" fn glViewport(x: i32, y: i32, w: i32, h: i32) {
    with(|c| if w < 0 || h < 0 { c.set_error(GL_INVALID_VALUE) } else { c.viewport = [x, y, w, h] })
}

extern "win64" fn glScissor(x: i32, y: i32, w: i32, h: i32) {
    with(|c| if w < 0 || h < 0 { c.set_error(GL_INVALID_VALUE) } else { c.scissor = [x, y, w, h] })
}

fn capability(c: &mut Context, cap: Enum, on: bool) {
    match cap {
        GL_BLEND => c.blend = on,
        GL_TEXTURE_2D => c.texture_2d = on,
        GL_SCISSOR_TEST => c.scissor_test = on,
        // Fixed-function state without visible effect here: depth, alpha
        // and stencil tests (no such buffers), dithering, culling of 2D
        // geometry, lighting, fog, smoothing hints.
        0x0B71 | 0x0BC0 | 0x0B90 | 0x0BD0 | 0x0B44 | 0x0B50 | 0x0B60 | 0x0B20 | 0x0B41 | 0x0B10 | 0x0BF2 | 0x0C60 | 0x0C61 | 0x8037 => {}
        _ => c.set_error(GL_INVALID_ENUM),
    }
}

extern "win64" fn glEnable(cap: Enum) {
    with(|c| capability(c, cap, true))
}

extern "win64" fn glDisable(cap: Enum) {
    with(|c| capability(c, cap, false))
}

/// Vertex arrays are not part of this subset's drawing; their client
/// state is accepted.
extern "win64" fn glEnableClientState(_array: Enum) {}
extern "win64" fn glDisableClientState(_array: Enum) {}

extern "win64" fn glBlendFunc(s: Enum, d: Enum) {
    with(|c| c.blend_func = (s, d))
}

extern "win64" fn glShadeModel(_mode: Enum) {}

extern "win64" fn glHint(_target: Enum, _mode: Enum) {}

extern "win64" fn glLineWidth(w: f32) {
    with(|c| if w <= 0. { c.set_error(GL_INVALID_VALUE) } else { c.line_width = w })
}

extern "win64" fn glClearColor(r: f32, g: f32, b: f32, a: f32) {
    with(|c| c.clear_color = [r, g, b, a].map(|v| v.clamp(0., 1.)))
}

extern "win64" fn glClear(mask: u32) {
    with(|c| {
        if mask & !(GL_COLOR_BUFFER_BIT | 0x0100 | 0x0400 | 0x0200) != 0 {
            return c.set_error(GL_INVALID_VALUE);
        }
        if mask & GL_COLOR_BUFFER_BIT == 0 {
            return;
        }
        let b = |v: f32| (v * 255. + 0.5) as u32;
        let [r, g, bl, a] = c.clear_color;
        let px = b(a) << 24 | b(r) << 16 | b(g) << 8 | b(bl);
        let (x0, y0, x1, y1) = c.bounds();
        for y in y0.max(0)..y1 {
            for x in x0.max(0)..x1 {
                let i = (y as u32 * c.w + x as u32) as usize;
                c.color[i] = px;
            }
        }
    })
}

extern "win64" fn glFlush() {}
extern "win64" fn glFinish() {}

extern "win64" fn glBegin(mode: Enum) {
    with(|c| {
        if c.prim.is_some() {
            return c.set_error(GL_INVALID_OPERATION);
        }
        if mode > GL_POLYGON {
            return c.set_error(GL_INVALID_ENUM);
        }
        c.fit();
        c.prim = Some(mode);
    })
}

extern "win64" fn glEnd() {
    with(|c| c.end())
}

extern "win64" fn glVertex2f(x: f32, y: f32) {
    with(|c| c.vertex(x, y, 0.))
}
extern "win64" fn glVertex2i(x: i32, y: i32) {
    with(|c| c.vertex(x as f32, y as f32, 0.))
}
extern "win64" fn glVertex3f(x: f32, y: f32, z: f32) {
    with(|c| c.vertex(x, y, z))
}

extern "win64" fn glColor4ub(r: u8, g: u8, b: u8, a: u8) {
    with(|c| c.cur_color = [r, g, b, a].map(|v| v as f32 / 255.))
}
extern "win64" fn glColor3ub(r: u8, g: u8, b: u8) {
    glColor4ub(r, g, b, 255)
}
extern "win64" fn glColor4f(r: f32, g: f32, b: f32, a: f32) {
    with(|c| c.cur_color = [r, g, b, a])
}
extern "win64" fn glColor3f(r: f32, g: f32, b: f32) {
    glColor4f(r, g, b, 1.)
}
extern "win64" fn glColor4fv(v: *const f32) {
    if !v.is_null() {
        // SAFETY: the caller's four floats.
        let v = unsafe { core::ptr::read_unaligned(v as *const [f32; 4]) };
        glColor4f(v[0], v[1], v[2], v[3]);
    }
}

extern "win64" fn glTexCoord2f(s: f32, t: f32) {
    with(|c| c.cur_tex = [s, t])
}
extern "win64" fn glTexCoord2i(s: i32, t: i32) {
    glTexCoord2f(s as f32, t as f32)
}

/// Only GL_RENDER: selection and feedback are not offered.
extern "win64" fn glRenderMode(mode: Enum) -> i32 {
    with(|c| {
        if c.prim.is_some() {
            c.set_error(GL_INVALID_OPERATION);
        } else if mode != GL_RENDER {
            c.set_error(GL_INVALID_ENUM);
        }
        0
    })
}

extern "win64" fn glPixelStorei(name: Enum, v: i32) {
    with(|c| {
        let slot = match name {
            GL_UNPACK_ROW_LENGTH => 0,
            GL_UNPACK_SKIP_ROWS => 1,
            GL_UNPACK_SKIP_PIXELS => 2,
            GL_UNPACK_ALIGNMENT => {
                if !matches!(v, 1 | 2 | 4 | 8) {
                    return c.set_error(GL_INVALID_VALUE);
                }
                3
            }
            // Pack state (glReadPixels) has no reader here.
            0x0D02..=0x0D05 => return,
            _ => return c.set_error(GL_INVALID_ENUM),
        };
        if v < 0 {
            return c.set_error(GL_INVALID_VALUE);
        }
        c.unpack[slot] = v;
    })
}

extern "win64" fn glTexEnvf(target: Enum, name: Enum, v: f32) {
    glTexEnvi(target, name, v as i32)
}

extern "win64" fn glTexEnvi(target: Enum, name: Enum, v: i32) {
    with(|c| {
        if target != 0x2300 || name != GL_TEXTURE_ENV_MODE {
            return c.set_error(GL_INVALID_ENUM);
        }
        if !matches!(v, GL_MODULATE | GL_DECAL | GL_REPLACE | 0x0BE2) {
            return c.set_error(GL_INVALID_ENUM);
        }
        c.tex_env = v;
    })
}

extern "win64" fn glTexParameteri(target: Enum, name: Enum, v: i32) {
    with(|c| {
        if target != GL_TEXTURE_2D {
            return c.set_error(GL_INVALID_ENUM);
        }
        let bound = c.bound;
        let mut store = c.textures.lock().unwrap();
        let t = store.map.entry(bound).or_insert_with(Texture::new);
        let linear = !matches!(v, GL_NEAREST | GL_NEAREST_MIPMAP_NEAREST | GL_NEAREST_MIPMAP_LINEAR);
        match name {
            GL_TEXTURE_MIN_FILTER => t.min_linear = linear,
            GL_TEXTURE_MAG_FILTER => t.mag_linear = linear,
            GL_TEXTURE_WRAP_S => t.repeat_s = v == GL_REPEAT,
            GL_TEXTURE_WRAP_T => t.repeat_t = v == GL_REPEAT,
            _ => {
                drop(store);
                c.set_error(GL_INVALID_ENUM);
            }
        }
    })
}

extern "win64" fn glTexParameterf(target: Enum, name: Enum, v: f32) {
    glTexParameteri(target, name, v as i32)
}

extern "win64" fn glGenTextures(n: i32, out: *mut u32) {
    with(|c| {
        if n < 0 {
            return c.set_error(GL_INVALID_VALUE);
        }
        let mut store = c.textures.lock().unwrap();
        for i in 0..n as usize {
            // Names bound without glGenTextures stay theirs: skip them
            // (and 0, which is no texture).
            let name = loop {
                store.next = store.next.wrapping_add(1);
                if store.next != 0 && !store.map.contains_key(&store.next) {
                    break store.next;
                }
            };
            store.map.insert(name, Texture::new());
            // SAFETY: the caller's array of `n` names.
            unsafe { *out.add(i) = name };
        }
    })
}

extern "win64" fn glDeleteTextures(n: i32, names: *const u32) {
    with(|c| {
        if n < 0 {
            return c.set_error(GL_INVALID_VALUE);
        }
        for i in 0..n as usize {
            // SAFETY: the caller's array of `n` names.
            let name = unsafe { *names.add(i) };
            if name != 0 {
                c.textures.lock().unwrap().map.remove(&name);
                if c.bound == name {
                    c.bound = 0;
                }
            }
        }
    })
}

extern "win64" fn glIsTexture(name: u32) -> u8 {
    with(|c| (name != 0 && c.textures.lock().unwrap().map.contains_key(&name)) as u8)
}

extern "win64" fn glBindTexture(target: Enum, name: u32) {
    with(|c| {
        if target != GL_TEXTURE_2D {
            return c.set_error(GL_INVALID_ENUM);
        }
        c.textures.lock().unwrap().map.entry(name).or_insert_with(Texture::new);
        c.bound = name;
    })
}

/// Texels of a client image as 0xAARRGGBB, honouring the unpack state.
fn unpack(c: &Context, w: u32, h: u32, format: Enum, kind: Enum, data: *const u8) -> Result<Vec<u32>, Enum> {
    if kind != 0x1401 {
        return Err(GL_INVALID_ENUM); // only GL_UNSIGNED_BYTE
    }
    let bpp = match format {
        0x1908 | 0x80E1 => 4, // RGBA, BGRA
        0x1907 | 0x80E0 => 3, // RGB, BGR
        0x190A => 2,          // LUMINANCE_ALPHA
        0x1906 | 0x1909 => 1, // ALPHA, LUMINANCE
        _ => return Err(GL_INVALID_ENUM),
    };
    if data.is_null() {
        return Ok(vec![0; (w * h) as usize]);
    }
    let [row_length, skip_rows, skip_pixels, align] = c.unpack.map(|v| v as usize);
    let row_px = if row_length > 0 { row_length } else { w as usize };
    let stride = (row_px * bpp).next_multiple_of(align);
    let mut out = Vec::with_capacity((w * h) as usize);
    for y in 0..h as usize {
        for x in 0..w as usize {
            // SAFETY: the caller's image of the size the unpack state gives.
            let p = unsafe { data.add((skip_rows + y) * stride + (skip_pixels + x) * bpp) };
            let b = |i: usize| unsafe { *p.add(i) } as u32;
            let (r, g, bl, a) = match format {
                0x1908 => (b(0), b(1), b(2), b(3)),
                0x80E1 => (b(2), b(1), b(0), b(3)),
                0x1907 => (b(0), b(1), b(2), 255),
                0x80E0 => (b(2), b(1), b(0), 255),
                0x190A => (b(0), b(0), b(0), b(1)),
                0x1906 => (0, 0, 0, b(0)),
                _ => (b(0), b(0), b(0), 255),
            };
            out.push(a << 24 | r << 16 | g << 8 | bl);
        }
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
extern "win64" fn glTexImage2D(target: Enum, level: i32, _internal: i32, w: i32, h: i32, border: i32, format: Enum, kind: Enum, data: *const u8) {
    with(|c| {
        if target != GL_TEXTURE_2D {
            return c.set_error(GL_INVALID_ENUM);
        }
        if level < 0 || w < 0 || h < 0 || border != 0 || w as u32 > MAX_TEXTURE_SIZE || h as u32 > MAX_TEXTURE_SIZE {
            return c.set_error(GL_INVALID_VALUE);
        }
        // Mipmap levels beyond the base are kept by nobody: the base
        // level is what this rasterizer samples.
        if level > 0 {
            return;
        }
        let px = match unpack(c, w as u32, h as u32, format, kind, data) {
            Ok(p) => p,
            Err(e) => return c.set_error(e),
        };
        let bound = c.bound;
        let mut store = c.textures.lock().unwrap();
        let t = store.map.entry(bound).or_insert_with(Texture::new);
        t.w = w as u32;
        t.h = h as u32;
        t.px = px;
    })
}

#[allow(clippy::too_many_arguments)]
extern "win64" fn glTexSubImage2D(target: Enum, level: i32, x: i32, y: i32, w: i32, h: i32, format: Enum, kind: Enum, data: *const u8) {
    with(|c| {
        if target != GL_TEXTURE_2D {
            return c.set_error(GL_INVALID_ENUM);
        }
        if level > 0 {
            return;
        }
        let bound = c.bound;
        let size = c.textures.lock().unwrap().map.get(&bound).map(|t| (t.w as i32, t.h as i32));
        let Some((tw, th)) = size else { return c.set_error(GL_INVALID_OPERATION) };
        if level < 0 || x < 0 || y < 0 || w < 0 || h < 0 || x + w > tw || y + h > th {
            return c.set_error(GL_INVALID_VALUE);
        }
        let px = match unpack(c, w as u32, h as u32, format, kind, data) {
            Ok(p) => p,
            Err(e) => return c.set_error(e),
        };
        let mut store = c.textures.lock().unwrap();
        let t = store.map.get_mut(&bound).expect("checked above");
        for row in 0..h as usize {
            let at = (y as usize + row) * t.w as usize + x as usize;
            t.px[at..at + w as usize].copy_from_slice(&px[row * w as usize..(row + 1) * w as usize]);
        }
    })
}

extern "win64" fn glGetIntegerv(name: Enum, out: *mut i32) {
    with(|c| {
        let v: Vec<i32> = match name {
            0x0BA2 => c.viewport.to_vec(),                // GL_VIEWPORT
            0x0C10 => c.scissor.to_vec(),                 // GL_SCISSOR_BOX
            0x0D33 => vec![MAX_TEXTURE_SIZE as i32],      // GL_MAX_TEXTURE_SIZE
            0x0D3A => vec![4096, 4096],                   // GL_MAX_VIEWPORT_DIMS
            0x8069 => vec![c.bound as i32],               // GL_TEXTURE_BINDING_2D
            0x0BA0 => vec![c.matrix_mode as i32],         // GL_MATRIX_MODE
            0x0CF5 => vec![c.unpack[3]],                  // GL_UNPACK_ALIGNMENT
            0x0C40 => vec![GL_RENDER as i32],             // GL_RENDER_MODE
            0x0BE1 => vec![c.blend_func.0 as i32],        // GL_BLEND_SRC
            0x0BE0 => vec![c.blend_func.1 as i32],        // GL_BLEND_DST
            0x0D32 => vec![STACK_DEPTH as i32],           // GL_MAX_MODELVIEW_STACK_DEPTH
            0x0D36 => vec![STACK_DEPTH as i32],           // GL_MAX_PROJECTION_STACK_DEPTH
            0x0D50..=0x0D53 => vec![8],                   // GL_RED/GREEN/BLUE/ALPHA_BITS
            0x0D56 => vec![0],                            // GL_DEPTH_BITS
            _ => return c.set_error(GL_INVALID_ENUM),
        };
        if !out.is_null() {
            for (i, x) in v.iter().enumerate() {
                // SAFETY: the caller's array for this query.
                unsafe { *out.add(i) = *x };
            }
        }
    })
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        wglCreateContext,
        wglDeleteContext,
        wglMakeCurrent,
        wglGetCurrentContext,
        wglGetCurrentDC,
        wglShareLists,
        wglGetProcAddress,
        glGetError,
        glGetString,
        glMatrixMode,
        glLoadIdentity,
        glPushMatrix,
        glPopMatrix,
        glOrtho,
        glTranslatef,
        glScalef,
        glViewport,
        glScissor,
        glEnable,
        glDisable,
        glEnableClientState,
        glDisableClientState,
        glBlendFunc,
        glShadeModel,
        glHint,
        glLineWidth,
        glClearColor,
        glClear,
        glFlush,
        glFinish,
        glBegin,
        glEnd,
        glVertex2f,
        glVertex2i,
        glVertex3f,
        glColor4ub,
        glColor3ub,
        glColor4f,
        glColor3f,
        glColor4fv,
        glTexCoord2f,
        glTexCoord2i,
        glRenderMode,
        glPixelStorei,
        glTexEnvf,
        glTexEnvi,
        glTexParameteri,
        glTexParameterf,
        glGenTextures,
        glDeleteTextures,
        glIsTexture,
        glBindTexture,
        glTexImage2D,
        glTexSubImage2D,
        glGetIntegerv,
    ]
}
