//! Host-side preview of the boot screen: renders the same layers the UEFI
//! loader draws and writes a binary PPM. This is a design aid only; it does not
//! exercise UEFI or real framebuffer code.
//!
//! cargo run -p osyx-gfx --example preview -- <width> <height> <out.ppm> [error]

use osyx_gfx::splash::{self, Assets, Layout};
use osyx_gfx::Surface;
use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let w: u32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1280);
    let h: u32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(800);
    let out = args.get(3).cloned().unwrap_or_else(|| "splash-preview.ppm".into());
    let error = args.get(4).map(|s| s == "error").unwrap_or(false);

    let a = Assets::embedded().expect("embedded assets");
    let l = Layout::new(w, h, &a);
    let mut bgbuf = vec![0u32; (w * h) as usize];
    let mut bg = Surface::new(&mut bgbuf, w, h, w).unwrap();
    splash::draw_background(&mut bg, &l, &a, "OSyx Boot Manager 0.1.0", &format!("{w}x{h}  BGR"));
    let mut fbuf = bg.pixels().to_vec();
    let mut f = Surface::new(&mut fbuf, w, h, w).unwrap();
    splash::draw_logo(&mut f, &bg, &l, &a, 255);
    if error {
        splash::draw_progress(&mut f, &bg, &l, 500, splash::ERROR);
        splash::draw_status(&mut f, &bg, &l, &a, "Boot failed: kernel image is invalid", splash::ERROR);
        splash::draw_details(
            &mut f,
            &bg,
            &l,
            &a,
            &["\\EFI\\OSYX\\KERNEL.ELF: not an ELF64 file", "Press any key to return to firmware"],
            splash::TEXT_DIM,
        );
    } else {
        splash::draw_progress(&mut f, &bg, &l, 666, splash::ACCENT);
        splash::draw_status(&mut f, &bg, &l, &a, "Loading kernel", splash::TEXT);
        splash::draw_spinner(&mut f, &bg, &l, 5 * 16, 255, splash::TEXT);
    }

    let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
    for y in 0..h {
        for &p in f.row(y) {
            ppm.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8]);
        }
    }
    std::fs::File::create(&out).and_then(|mut file| file.write_all(&ppm)).expect("write ppm");
    println!("wrote {out}");
}
