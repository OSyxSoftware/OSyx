//! Renders a compositor scene to a PPM image on the host (for checking the
//! look of decorations, shadows and the cursor without booting OSyx).
//!
//! cargo run -p prism-core --example preview -- out.ppm

use osyx_gfx::Surface;
use prism_core::{Compositor, Config, PixelSource};
use prism_proto::{window_flags, Request, Role};
use std::io::Write;

fn gradient(w: u32, h: u32) -> Vec<u32> {
    let mut v = vec![0u32; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let r = 10 + (x * 20 / w);
            let g = 16 + (y * 30 / h);
            let b = 40 + ((x + y) * 60 / (w + h));
            v[(y * w + x) as usize] = (r << 16) | (g << 8) | b;
        }
    }
    v
}

fn main() {
    let (w, h) = (1280u32, 800u32);
    let mut c = Compositor::new(Config::new(w, h));
    c.set_shell_pid(Some(1));
    let send = |c: &mut Compositor, id, r: Request, buf: Option<Box<dyn PixelSource>>| {
        c.handle(id, &r.encode(0), buf, 0).unwrap();
    };
    let hello = |c: &mut Compositor, id, flags| {
        send(c, id, Request::Hello { min_version: 1, max_version: 1, flags, name: "preview".into() }, None);
    };
    let shell = c.connect(1);
    hello(&mut c, shell, 1);
    let window = |c: &mut Compositor, id, local, role, flags, geo: (i32, i32, u32, u32), title: &str, pixels: Vec<u32>| {
        send(c, id, Request::CreateSurface { surface: local }, None);
        send(
            c,
            id,
            Request::CreateWindow {
                window: local,
                surface: local,
                parent: 0,
                role,
                flags,
                x: geo.0,
                y: geo.1,
                width: geo.2,
                height: geo.3,
                min_width: 0,
                min_height: 0,
                title: title.into(),
            },
            None,
        );
        send(
            c,
            id,
            Request::AttachBuffer { surface: local, buffer: local, width: geo.2, height: geo.3, stride: geo.2 * 4, format: 1, offset: 0 },
            Some(Box::new(pixels)),
        );
        send(c, id, Request::Commit { surface: local }, None);
    };
    window(&mut c, shell, 1, Role::Desktop, 0, (0, 0, w, h), "desktop", gradient(w, h));
    let panel = vec![0x161B26u32; (w * 48) as usize];
    window(&mut c, shell, 2, Role::Panel, 0, (0, h as i32 - 48, w, 48), "panel", panel);
    let a = c.connect(2);
    hello(&mut c, a, 0);
    window(&mut c, a, 1, Role::Normal, window_flags::DECORATED | window_flags::RESIZABLE, (120, 110, 560, 360), "Files — /home/user", vec![0x1E2430; 560 * 360]);
    let b = c.connect(3);
    hello(&mut c, b, 0);
    window(&mut c, b, 1, Role::Normal, window_flags::DECORATED | window_flags::RESIZABLE, (520, 260, 600, 380), "AXIS Terminal", vec![0x0F131B; 600 * 380]);
    // Move the pointer over the second window's close button.
    let mut fb = vec![0u32; (w * h) as usize];
    let mut s = Surface::new(&mut fb, w, h, w).unwrap();
    c.render(&mut s, 1000);
    let mut s = Surface::new(&mut fb, w, h, w).unwrap();
    c.render(&mut s, 2000);
    let path = std::env::args().nth(1).unwrap_or_else(|| "preview.ppm".into());
    let mut f = std::fs::File::create(&path).unwrap();
    write!(f, "P6\n{w} {h}\n255\n").unwrap();
    for p in fb {
        f.write_all(&[(p >> 16) as u8, (p >> 8) as u8, p as u8]).unwrap();
    }
    println!("wrote {path}; stats {:?}", c.stats());
}
