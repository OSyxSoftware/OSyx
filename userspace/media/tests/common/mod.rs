//! Helpers shared by the integration tests.

/// A one-page PDF of `w` by `h` points drawing one image XObject over the
/// whole page: its dictionary entries `image_dict` (without /Length) and
/// data. `extra` streams become objects 6, 7, ... for the dictionary to
/// refer to.
pub fn pdf_with_image(w: usize, h: usize, image_dict: &str, data: &[u8], extra: &[&[u8]]) -> Vec<u8> {
    let content = format!("q {w} 0 0 {h} 0 0 cm /Im0 Do Q");
    let stream = |dict: &str, data: &[u8]| {
        let mut o = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
        o.extend_from_slice(data);
        o.extend_from_slice(b"\nendstream");
        o
    };
    let mut objs: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Resources << /XObject << /Im0 5 0 R >> >> /Contents 4 0 R >>"
        )
        .into_bytes(),
        stream("", content.as_bytes()),
        stream(&format!("/Type /XObject /Subtype /Image {image_dict}"), data),
    ];
    for e in extra {
        objs.push(stream("", e));
    }
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes(),
    );
    out
}
