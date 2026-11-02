//! The PDF renderer against PDFium's renderings of the same files
//! (`make_pdf_fixtures.py`). Set `OSYX_PDF_DUMP=<dir>` to also write both
//! images as PPM files for a look.

use osyx_media::pdf::Pdf;

fn path(name: &str) -> String {
    format!("{}/tests/data/pdf/{name}", env!("CARGO_MANIFEST_DIR"))
}

fn system_font(name: &str) -> Option<Vec<u8>> {
    std::fs::read(format!("{}/../../graphics/assets/fonts/windows/{name}", env!("CARGO_MANIFEST_DIR"))).ok()
}

/// PDFium's rendering: width, height, RGB rows.
fn reference(stem: &str, page: usize) -> (usize, usize, Vec<u8>) {
    let gz = std::fs::read(path(&format!("{stem}.p{page}.ref.gz"))).unwrap();
    let raw = miniz_oxide::inflate::decompress_to_vec(&gz[10..gz.len() - 8]).unwrap();
    let w = u32::from_le_bytes(raw[0..4].try_into().unwrap()) as usize;
    let h = u32::from_le_bytes(raw[4..8].try_into().unwrap()) as usize;
    (w, h, raw[8..].to_vec())
}

fn dump(name: &str, w: usize, h: usize, rgb: &[u8]) {
    if let Ok(dir) = std::env::var("OSYX_PDF_DUMP") {
        let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
        out.extend_from_slice(rgb);
        std::fs::write(format!("{dir}/{name}.ppm"), out).unwrap();
    }
}

/// (mean absolute difference per channel, share of pixels off by more
/// than 64 in some channel).
fn compare(stem: &str) -> (f64, f64) {
    let pdf = Pdf::open(std::fs::read(path(&format!("{stem}.pdf"))).unwrap()).unwrap();
    let fonts = |n: &str| system_font(n);
    let bmp = pdf.render(0, 1.0, &fonts).unwrap();
    let (w, h, want) = reference(stem, 0);
    assert!(bmp.w.abs_diff(w) <= 1 && bmp.h.abs_diff(h) <= 1, "{stem}: {}x{} against {w}x{h}", bmp.w, bmp.h);
    let got: Vec<u8> = bmp.px.iter().flatten().copied().collect();
    dump(&format!("{stem}-osyx"), bmp.w, bmp.h, &got);
    dump(&format!("{stem}-pdfium"), w, h, &want);
    let (mut sum, mut bad, mut n) = (0u64, 0usize, 0usize);
    for y in 0..h.min(bmp.h) {
        for x in 0..w.min(bmp.w) {
            let a = &got[(y * bmp.w + x) * 3..][..3];
            let b = &want[(y * w + x) * 3..][..3];
            let d: Vec<u32> = a.iter().zip(b).map(|(p, q)| p.abs_diff(*q) as u32).collect();
            sum += d.iter().sum::<u32>() as u64;
            if d.iter().any(|&v| v > 64) {
                bad += 1;
            }
            n += 1;
        }
    }
    let r = (sum as f64 / (3 * n) as f64, bad as f64 / n as f64);
    println!("{stem}: mean difference {:.2}, {:.2}% of pixels off", r.0, r.1 * 100.0);
    r
}

#[test]
fn embedded_fonts_and_graphics() {
    for stem in ["shapes", "objstm", "aes256", "truetype42", "chart"] {
        let (mean, off) = compare(stem);
        assert!(mean < 4.0 && off < 0.03, "{stem}: mean {mean:.2}, off {:.2}%", off * 100.0);
    }
}

#[test]
fn standard_fonts_use_system_substitutes() {
    // Different typefaces than PDFium's: only the layout is compared.
    for stem in ["standard-fonts", "rc4"] {
        let (mean, off) = compare(stem);
        assert!(mean < 10.0 && off < 0.08, "{stem}: mean {mean:.2}, off {:.2}%", off * 100.0);
    }
}

#[test]
fn damaged_files_are_rebuilt() {
    let mut data = std::fs::read(path("shapes.pdf")).unwrap();
    // Wreck the cross-reference offsets.
    let x = data.windows(9).rposition(|w| w == b"startxref").unwrap();
    for b in &mut data[x + 10..x + 14] {
        *b = b'9';
    }
    let pdf = Pdf::open(data).unwrap();
    assert_eq!(pdf.pages(), 1);
    let fonts = |n: &str| system_font(n);
    let bmp = pdf.render(0, 0.5, &fonts).unwrap();
    assert!(bmp.px.iter().any(|p| p[2] > 150 && p[0] < 80), "the blue rectangle is drawn");
}

#[test]
fn hostile_inputs_fail_cleanly() {
    let fonts = |n: &str| system_font(n);
    // A page far too large to draw.
    let mut data = std::fs::read(path("shapes.pdf")).unwrap();
    let i = data.windows(9).position(|w| w == b"/MediaBox").unwrap();
    let end = i + data[i..].iter().position(|&c| c == b']').unwrap() + 1;
    let huge = b"/MediaBox [0 0 300 9999999]";
    data.splice(i..end, huge.iter().copied());
    let pdf = Pdf::open(data).unwrap();
    assert!(pdf.render(0, 1.0, &fonts).is_err());
    // A /XRefStm offset past the end of the file.
    let mut data = std::fs::read(path("standard-fonts.pdf")).unwrap();
    let t = data.windows(7).rposition(|w| w == b"trailer").unwrap();
    data.splice(t + 7..t + 7, b" << /XRefStm 99999999 >> ".iter().copied());
    assert_eq!(Pdf::open(data).unwrap().pages(), 1);
}
