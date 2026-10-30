//! Benchmarks for the integer 2D renderer used by the boot screen and the
//! kernel console.

use divan::{black_box, Bencher};
use osyx_gfx::{rgb, Font, Image, Rect, Surface};

fn main() {
    divan::main();
}

const W: u32 = 1280;
const H: u32 = 800;

static MONO16: &[u8] = include_bytes!("../../assets/mono16.osxf");
static LOGO: &[u8] = include_bytes!("../../assets/logo.osxi");

fn with_surface(bencher: Bencher, f: impl Fn(&mut Surface)) {
    let mut pixels = vec![0u32; (W * H) as usize];
    bencher.bench_local(|| {
        let mut s = Surface::new(&mut pixels, W, H, W).unwrap();
        f(black_box(&mut s));
    });
}

#[divan::bench]
fn fill_full_screen(bencher: Bencher) {
    with_surface(bencher, |s| s.fill(rgb(0x20, 0x30, 0x40)));
}

#[divan::bench]
fn fill_rect_window(bencher: Bencher) {
    with_surface(bencher, |s| s.fill_rect(Rect::new(100, 80, 640, 480), rgb(0xee, 0xee, 0xee)));
}

#[divan::bench]
fn blend_rect_window(bencher: Bencher) {
    with_surface(bencher, |s| s.blend_rect(Rect::new(100, 80, 640, 480), rgb(0, 0, 0), 96));
}

#[divan::bench]
fn gradient_full_screen(bencher: Bencher) {
    with_surface(bencher, |s| s.gradient_v(s.bounds(), rgb(0x10, 0x10, 0x30), rgb(0x40, 0x20, 0x60)));
}

#[divan::bench]
fn radial_glow(bencher: Bencher) {
    with_surface(bencher, |s| s.radial_glow(640, 400, 300, rgb(0x60, 0x80, 0xff), 200));
}

#[divan::bench]
fn rounded_rect(bencher: Bencher) {
    with_surface(bencher, |s| s.rounded_rect(Rect::new(200, 150, 400, 300), 16, rgb(0x30, 0x30, 0x30), 220));
}

#[divan::bench]
fn draw_text_line(bencher: Bencher) {
    let font = Font::parse(MONO16).unwrap();
    with_surface(bencher, |s| {
        font.draw(s, 10, 10, black_box("OSyx Boot Manager - starting kernel, please wait..."), rgb(255, 255, 255), 255);
    });
}

#[divan::bench]
fn draw_logo_scaled(bencher: Bencher) {
    let logo = Image::parse(LOGO).unwrap();
    with_surface(bencher, |s| {
        logo.draw_scaled(s, Rect::new(512, 272, 256, 256), 255);
    });
}
