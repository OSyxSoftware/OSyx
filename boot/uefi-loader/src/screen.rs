//! Boot screen driver: owns the GOP framebuffer plus two RAM surfaces
//! (static background and composed frame) and animates the splash layers.

use crate::efi::{self, BootServices, GraphicsOutput, Status};
use crate::fmt_buf;
use osyx_bootinfo::{FramebufferInfo, PixelFormat};
use osyx_gfx::splash::{self, Assets, Layout};
use osyx_gfx::{Color, Rect, Surface};

/// Frame period for animations, in microseconds.
const FRAME_US: usize = 16_000;
/// Frames used to ease the progress bar from one stage to the next.
const PROGRESS_FRAMES: u32 = 10;
const FADE_FRAMES: u32 = 14;
const MAX_W: u32 = 1920;
const MAX_H: u32 = 1080;

pub struct Screen {
    fb: *mut u32,
    fb_size: u64,
    width: u32,
    height: u32,
    stride: u32,
    rgb: bool,
    frame: &'static mut [u32],
    bg: &'static mut [u32],
    layout: Layout,
    assets: Assets<'static>,
    shown_permille: u32,
    phase: u32,
    fast: bool,
}

#[derive(Debug)]
pub enum ScreenError {
    NoGop,
    UnsupportedFormat,
    Assets,
    OutOfMemory,
}

impl Screen {
    /// Locates GOP, selects a mode and allocates the back buffers.
    /// `want` is an optional preferred resolution from the command line.
    pub fn init(bs: &BootServices, want: Option<(u32, u32)>, fast: bool) -> Result<Screen, ScreenError> {
        let mut gop: *mut GraphicsOutput = core::ptr::null_mut();
        // SAFETY: valid boot-services call with a correctly typed out pointer.
        let st = unsafe { (bs.locate_protocol)(&efi::GOP_GUID, core::ptr::null_mut(), &mut gop as *mut _ as *mut _) };
        if st.is_error() || gop.is_null() {
            return Err(ScreenError::NoGop);
        }
        // SAFETY: firmware returned a valid protocol instance.
        let gop = unsafe { &mut *gop };
        select_mode(bs, gop, want);
        // SAFETY: `mode` and `mode.info` stay valid while the protocol exists.
        let (mode, info) = unsafe { (&*gop.mode, &*(*gop.mode).info) };
        let rgb = match info.pixel_format {
            efi::PIXEL_RGB => true,
            efi::PIXEL_BGR => false,
            _ => return Err(ScreenError::UnsupportedFormat),
        };
        let (width, height, stride) = (info.horizontal_resolution, info.vertical_resolution, info.pixels_per_scan_line);
        let pixels = width as usize * height as usize;
        let frame = alloc_pixels(bs, pixels)?;
        let bg = alloc_pixels(bs, pixels)?;
        let assets = Assets::embedded().map_err(|_| ScreenError::Assets)?;
        let layout = Layout::new(width, height, &assets);
        let mut s = Screen {
            fb: mode.frame_buffer_base as *mut u32,
            fb_size: mode.frame_buffer_size as u64,
            width,
            height,
            stride,
            rgb,
            frame,
            bg,
            layout,
            assets,
            shown_permille: 0,
            phase: 0,
            fast,
        };
        s.draw_static();
        Ok(s)
    }

    pub fn info(&self) -> FramebufferInfo {
        FramebufferInfo {
            base: self.fb as u64,
            size: self.fb_size,
            width: self.width,
            height: self.height,
            stride: self.stride,
            format: if self.rgb { PixelFormat::Rgb } else { PixelFormat::Bgr },
        }
    }

    fn surfaces(&mut self) -> (Surface<'_>, Surface<'_>) {
        let (w, h) = (self.width, self.height);
        // Both buffers hold exactly w*h pixels (allocated in `init`).
        let f = Surface::new(self.frame, w, h, w).expect("frame geometry");
        let b = Surface::new(self.bg, w, h, w).expect("bg geometry");
        (f, b)
    }

    fn draw_static(&mut self) {
        let right = fmt_buf!(32, "{}x{}  {}", self.width, self.height, if self.rgb { "RGB" } else { "BGR" });
        let left = concat!("OSyx Boot Manager ", env!("CARGO_PKG_VERSION"));
        let (l, a) = (self.layout, self.assets);
        {
            let (_, mut bg) = self.surfaces();
            splash::draw_background(&mut bg, &l, &a, left, right.as_str());
        }
        self.frame.copy_from_slice(self.bg);
        let full = Rect::new(0, 0, self.width, self.height);
        self.present(full);
    }

    /// Copies `r` of the composed frame to the hardware framebuffer.
    fn present(&mut self, r: Rect) {
        let (fb, stride, rgb) = (self.fb, self.stride, self.rgb);
        let (frame, _) = self.surfaces();
        // SAFETY: the GOP framebuffer holds at least stride * height pixels of
        // the mode whose dimensions the frame surface uses.
        unsafe { osyx_gfx::present(&frame, r, fb, stride, rgb) };
    }

    fn stall(&self, bs: &BootServices) {
        if !self.fast {
            // SAFETY: plain boot-services call.
            unsafe { (bs.stall)(FRAME_US) };
        }
    }

    /// Fades the logo in.
    pub fn intro(&mut self, bs: &BootServices) {
        let frames = if self.fast { 1 } else { FADE_FRAMES };
        for i in 1..=frames {
            let alpha = 255 * i * i / (frames * frames);
            let (l, a) = (self.layout, self.assets);
            let r = {
                let (mut f, bg) = self.surfaces();
                splash::draw_logo(&mut f, &bg, &l, &a, alpha)
            };
            self.present(r);
            self.stall(bs);
        }
        self.set_progress_now(0, splash::ACCENT);
    }

    /// Shows `text` as the current activity and eases the bar to `permille`
    /// (the fraction of boot stages already completed).
    pub fn stage(&mut self, bs: &BootServices, permille: u32, text: &str) {
        let (l, a) = (self.layout, self.assets);
        let r = {
            let (mut f, bg) = self.surfaces();
            splash::draw_status(&mut f, &bg, &l, &a, text, splash::TEXT)
        };
        self.present(r);
        let from = self.shown_permille;
        let frames = if self.fast { 1 } else { PROGRESS_FRAMES };
        for i in 1..=frames {
            // Ease-out: 1 - (1 - t)^2.
            let t = 256 * i / frames;
            let eased = 256 - (((256 - t) * (256 - t)) >> 8);
            let p = from + ((permille.saturating_sub(from) * eased) >> 8);
            self.set_progress_now(p, splash::ACCENT);
            self.spin(4);
            self.stall(bs);
        }
        self.shown_permille = permille;
    }

    fn set_progress_now(&mut self, permille: u32, c: Color) {
        let l = self.layout;
        let r = {
            let (mut f, bg) = self.surfaces();
            splash::draw_progress(&mut f, &bg, &l, permille, c)
        };
        self.present(r);
    }

    fn spin(&mut self, step: u32) {
        self.phase = self.phase.wrapping_add(step);
        let (l, phase) = (self.layout, self.phase);
        let r = {
            let (mut f, bg) = self.surfaces();
            splash::draw_spinner(&mut f, &bg, &l, phase, 255, splash::TEXT)
        };
        self.present(r);
    }

    /// Paints diagnostic square `slot` (counted from the right edge) straight
    /// into the framebuffer. Firmware free, so usable around ExitBootServices.
    pub fn marker(&mut self, slot: u32, rgb: u32) {
        let (size, gap) = (20u32, 28u32);
        let Some(x0) = self.width.checked_sub(gap * (slot + 1)) else { return };
        let px = osyx_gfx::to_device(rgb, self.rgb);
        for y in 8..8 + size {
            for x in x0..x0 + size {
                // SAFETY: (x, y) lies inside the visible mode, so inside the framebuffer.
                unsafe { self.fb.add((y * self.stride + x) as usize).write_volatile(px) };
            }
        }
    }

    /// Final frame drawn after ExitBootServices: no firmware calls allowed.
    pub fn handoff(&mut self) {
        let (l, a) = (self.layout, self.assets);
        let r = {
            let (mut f, bg) = self.surfaces();
            splash::draw_progress(&mut f, &bg, &l, 1000, splash::ACCENT).union(&splash::draw_status(
                &mut f,
                &bg,
                &l,
                &a,
                "Entering kernel",
                splash::TEXT,
            ))
        };
        self.present(r);
    }

    pub fn error(&mut self, title: &str, details: &[&str]) {
        let (l, a, p) = (self.layout, self.assets, self.shown_permille.max(40));
        let r = {
            let (mut f, bg) = self.surfaces();
            splash::draw_progress(&mut f, &bg, &l, p, splash::ERROR)
                .union(&splash::draw_status(&mut f, &bg, &l, &a, title, splash::ERROR))
                .union(&splash::draw_details(&mut f, &bg, &l, &a, details, splash::TEXT_DIM))
                .union(&splash::draw_spinner(&mut f, &bg, &l, 0, 0, splash::TEXT))
        };
        self.present(r);
    }
}

fn alloc_pixels(bs: &BootServices, count: usize) -> Result<&'static mut [u32], ScreenError> {
    let mut p: *mut u8 = core::ptr::null_mut();
    // SAFETY: pool allocation; the memory stays valid (as LoaderData) after
    // ExitBootServices, which the final hand-off frame relies on.
    let st = unsafe { (bs.allocate_pool)(efi::memtype::LOADER_DATA, count * 4, &mut p) };
    if st.is_error() || p.is_null() {
        return Err(ScreenError::OutOfMemory);
    }
    // SAFETY: pool memory is 8-byte aligned and `count * 4` bytes long; it is
    // zeroed here before being exposed as initialized u32s.
    unsafe {
        core::ptr::write_bytes(p, 0, count * 4);
        Ok(core::slice::from_raw_parts_mut(p as *mut u32, count))
    }
}

/// Picks the requested resolution if available, otherwise the largest RGB/BGR
/// mode no bigger than 1920x1080. Keeps the current mode if nothing better.
fn select_mode(bs: &BootServices, gop: &mut GraphicsOutput, want: Option<(u32, u32)>) {
    // SAFETY: mode pointer valid for the protocol lifetime.
    let (max_mode, current) = unsafe { ((*gop.mode).max_mode, (*gop.mode).mode) };
    let mut best: Option<(u32, u64)> = None;
    for m in 0..max_mode {
        let mut size = 0usize;
        let mut info: *const efi::GopModeInfo = core::ptr::null();
        // SAFETY: QueryMode with valid out pointers; `info` is read only on success.
        let st: Status = unsafe { (gop.query_mode)(gop, m, &mut size, &mut info) };
        if st.is_error() || info.is_null() {
            continue;
        }
        // SAFETY: QueryMode succeeded, so `info` points to a callee-allocated
        // mode description that we copy out and then release.
        let (fmt, w, h) = unsafe {
            let i = &*info;
            let v = (i.pixel_format, i.horizontal_resolution, i.vertical_resolution);
            (bs.free_pool)(info as *mut u8);
            v
        };
        if fmt != efi::PIXEL_RGB && fmt != efi::PIXEL_BGR {
            continue;
        }
        let score = if want == Some((w, h)) {
            u64::MAX
        } else if w <= MAX_W && h <= MAX_H {
            w as u64 * h as u64
        } else {
            0
        };
        if best.is_none_or(|(_, s)| score > s) {
            best = Some((m, score));
        }
    }
    if let Some((m, _)) = best {
        if m != current {
            // SAFETY: valid mode number returned by QueryMode.
            let st = unsafe { (gop.set_mode)(gop, m) };
            if st.is_error() {
                crate::log!("GOP SetMode({m}) failed: {}; keeping mode {current}", st.name());
            }
        }
    }
}
