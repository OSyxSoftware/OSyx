//! Kernel-owned framebuffer status screen.
//!
//! After hand-off the kernel redraws the boot screen itself (proving it owns
//! the framebuffer without firmware help) and keeps a live status area:
//! self-test results, memory statistics and an uptime-driven spinner.

use crate::mem::phys_to_virt;
use crate::sync::IrqSpinLock;
use alloc::vec;
use alloc::vec::Vec;
use osyx_bootinfo::{FramebufferInfo, PixelFormat};
use osyx_gfx::splash::{self, Assets, Layout};
use osyx_gfx::{Color, Rect, Surface};

struct Display {
    fb: *mut u32,
    /// Physical framebuffer (for mapping it into the owner process).
    phys: u64,
    size: u64,
    stride: u32,
    rgb: bool,
    width: u32,
    height: u32,
    frame: Vec<u32>,
    bg: Vec<u32>,
    layout: Layout,
    assets: Assets<'static>,
    /// pid of the process that owns the display (0: the kernel draws).
    owner: u64,
}

// SAFETY: the framebuffer pointer is only dereferenced under the DISPLAY lock.
unsafe impl Send for Display {}

static DISPLAY: IrqSpinLock<Option<Display>> = IrqSpinLock::new(None);

impl Display {
    fn surfaces(&mut self) -> (Surface<'_>, Surface<'_>) {
        let (w, h) = (self.width, self.height);
        (
            Surface::new(&mut self.frame, w, h, w).expect("frame geometry"),
            Surface::new(&mut self.bg, w, h, w).expect("bg geometry"),
        )
    }

    fn present(&mut self, r: Rect) {
        if self.owner != 0 {
            return; // a user-space compositor draws now
        }
        self.force_present(r);
    }

    fn force_present(&mut self, r: Rect) {
        let (fb, stride, rgb) = (self.fb, self.stride, self.rgb);
        let (frame, _) = self.surfaces();
        // SAFETY: `fb` maps the whole framebuffer (stride * height pixels)
        // through the HHDM; checked in `init`.
        unsafe { osyx_gfx::present(&frame, r, fb, stride, rgb) };
    }
}

/// Takes over the framebuffer. Returns false if there is none or it is unusable.
pub fn init(info: &FramebufferInfo, footer_right: &str) -> bool {
    let rgb = match info.format {
        PixelFormat::Rgb => true,
        PixelFormat::Bgr => false,
        _ => return false,
    };
    let needed = info.stride as u64 * info.height as u64 * 4;
    if info.base == 0 || info.width == 0 || info.stride < info.width || info.size < needed {
        return false;
    }
    if info.base + info.size > crate::mem::hhdm_size() {
        return false;
    }
    let Ok(assets) = Assets::embedded() else { return false };
    let (w, h) = (info.width, info.height);
    let mut d = Display {
        fb: phys_to_virt(info.base) as *mut u32,
        phys: info.base,
        size: needed,
        owner: 0,
        stride: info.stride,
        rgb,
        width: w,
        height: h,
        frame: vec![0; (w * h) as usize],
        bg: vec![0; (w * h) as usize],
        layout: Layout::new(w, h, &assets),
        assets,
    };
    let (l, a) = (d.layout, d.assets);
    {
        let (_, mut bg) = d.surfaces();
        splash::draw_background(&mut bg, &l, &a, concat!("OSyx kernel ", env!("CARGO_PKG_VERSION")), footer_right);
    }
    d.frame.copy_from_slice(&d.bg);
    {
        let (mut f, bg) = d.surfaces();
        splash::draw_logo(&mut f, &bg, &l, &a, 255);
        splash::draw_progress(&mut f, &bg, &l, 1000, splash::ACCENT);
    }
    d.present(Rect::new(0, 0, w, h));
    *DISPLAY.lock() = Some(d);
    true
}

/// Shows a status line and up to three detail lines.
pub fn status(text: &str, color: Color, details: &[&str]) {
    let mut g = DISPLAY.lock();
    let Some(d) = g.as_mut() else { return };
    let (l, a) = (d.layout, d.assets);
    let n = details.len().min(3);
    let r = {
        let (mut f, bg) = d.surfaces();
        splash::draw_status(&mut f, &bg, &l, &a, text, color).union(&splash::draw_details(
            &mut f,
            &bg,
            &l,
            &a,
            &details[..n],
            splash::TEXT_DIM,
        ))
    };
    d.present(r);
}

/// Advances the activity spinner (called from the idle loop).
pub fn spin(phase: u32) {
    let mut g = DISPLAY.lock();
    let Some(d) = g.as_mut() else { return };
    let l = d.layout;
    let r = {
        let (mut f, bg) = d.surfaces();
        splash::draw_spinner(&mut f, &bg, &l, phase, 255, splash::ACCENT)
    };
    d.present(r);
}

/// Best-effort panic screen; never blocks. Drawn even over a user-space
/// compositor: the system is halting.
pub fn panic_screen(message: &str) {
    let Some(mut g) = DISPLAY.try_lock() else { return };
    let Some(d) = g.as_mut() else { return };
    let (l, a) = (d.layout, d.assets);
    let owned = d.owner != 0;
    let r = {
        let (mut f, bg) = d.surfaces();
        splash::draw_progress(&mut f, &bg, &l, 1000, splash::ERROR)
            .union(&splash::draw_status(&mut f, &bg, &l, &a, "Kernel panic", splash::ERROR))
            .union(&splash::draw_details(&mut f, &bg, &l, &a, &[message, "System halted"], splash::TEXT))
            .union(&splash::draw_spinner(&mut f, &bg, &l, 0, 0, splash::ACCENT))
    };
    let full = Rect::new(0, 0, d.width, d.height);
    d.force_present(if owned { full } else { r });
}

/// Geometry of the framebuffer for `display_info`.
pub fn info() -> Option<osyx_abi::ipc::DisplayInfo> {
    let g = DISPLAY.lock();
    let d = g.as_ref()?;
    Some(osyx_abi::ipc::DisplayInfo {
        width: d.width,
        height: d.height,
        stride: d.stride,
        format: if d.rgb { osyx_abi::ipc::pixel::RGBX } else { osyx_abi::ipc::pixel::BGRX },
        size: d.size,
        owner: d.owner,
    })
}

/// Makes `pid` the display owner; returns the framebuffer as a device
/// memory object to map into it. The current owner may acquire again (after
/// `execve`, for example).
pub fn acquire(pid: u64) -> Result<alloc::sync::Arc<crate::shm::SharedMemory>, osyx_abi::Errno> {
    let mut g = DISPLAY.lock();
    let d = g.as_mut().ok_or(osyx_abi::Errno::NotSupported)?;
    if d.owner != 0 && d.owner != pid {
        return Err(osyx_abi::Errno::Busy);
    }
    let first = d.owner == 0;
    d.owner = pid;
    if first {
        crate::kprintln!("display: process {} owns the framebuffer ({}x{})", pid, d.width, d.height);
    }
    Ok(crate::shm::SharedMemory::device(d.phys, d.size))
}

/// Gives the display back to the kernel if `pid` owns it; the status
/// screen is drawn again (the diagnostic fallback when a compositor dies).
pub fn release(pid: u64) -> bool {
    let mut g = DISPLAY.lock();
    let Some(d) = g.as_mut() else { return false };
    if d.owner != pid || pid == 0 {
        return false;
    }
    d.owner = 0;
    let full = Rect::new(0, 0, d.width, d.height);
    d.present(full);
    crate::kprintln!("display: released by process {}, kernel status screen restored", pid);
    true
}

/// Process exit hook.
pub fn owner_exited(pid: u64) {
    release(pid);
}
