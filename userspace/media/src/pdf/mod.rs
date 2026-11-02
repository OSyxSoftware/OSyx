//! PDF documents for the viewer built into Files: parsing (including
//! damaged and encrypted files), and rendering pages to RGB pixels.

pub(crate) mod ccitt;
mod ccitt_tables;
mod color;
mod crypt;
pub mod document;
mod encoding;
mod filter;
pub mod font;
mod image;
mod metrics;
pub mod object;
pub mod raster;
mod render;
mod type1;

use alloc::string::String;
use alloc::vec::Vec;
pub use font::FontSource;

/// Rounding functions for `f64` without `std`.
pub(crate) trait Float {
    fn floor(self) -> f64;
    fn ceil(self) -> f64;
    fn round(self) -> f64;
    fn powi(self, n: i32) -> f64;
    fn rem_euclid(self, m: f64) -> f64;
}

impl Float for f64 {
    fn floor(self) -> f64 {
        libm::floor(self)
    }
    fn ceil(self) -> f64 {
        libm::ceil(self)
    }
    fn round(self) -> f64 {
        libm::round(self)
    }
    fn powi(self, n: i32) -> f64 {
        libm::pow(self, n as f64)
    }
    fn rem_euclid(self, m: f64) -> f64 {
        let r = libm::fmod(self, m);
        if r < 0.0 {
            r + m.abs()
        } else {
            r
        }
    }
}

pub struct Pdf {
    doc: document::Document,
}

/// A rendered page.
pub struct Bitmap {
    pub w: usize,
    pub h: usize,
    pub px: Vec<[u8; 3]>,
}

impl Pdf {
    pub fn open(data: Vec<u8>) -> Result<Pdf, String> {
        Ok(Pdf { doc: document::Document::open(data)? })
    }

    pub fn pages(&self) -> usize {
        self.doc.pages.len()
    }

    /// Width and height of page `i` in points, rotation applied.
    pub fn page_size(&self, i: usize) -> (f64, f64) {
        let p = &self.doc.pages[i.min(self.doc.pages.len() - 1)];
        let (w, h) = (p.bbox[2] - p.bbox[0], p.bbox[3] - p.bbox[1]);
        if p.rotate % 180 == 0 {
            (w, h)
        } else {
            (h, w)
        }
    }

    pub fn title(&self) -> Option<String> {
        self.doc.title()
    }

    /// Renders page `i` at `scale` pixels per point. `fonts` supplies the
    /// system fonts used for fonts a file does not embed. Fails when the
    /// bitmap would be unreasonably large.
    pub fn render(&self, i: usize, scale: f64, fonts: FontSource) -> Result<Bitmap, String> {
        let page = &self.doc.pages[i.min(self.doc.pages.len() - 1)];
        let mut r = render::Renderer::new(&self.doc, page, scale, fonts)?;
        r.render_page(page);
        Ok(Bitmap { w: r.canvas.w, h: r.canvas.h, px: r.canvas.px })
    }
}
