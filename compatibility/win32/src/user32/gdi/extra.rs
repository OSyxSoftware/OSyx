//! gdi32 functions the Steam client's `vgui2_s.dll` imports: font files
//! and font enumeration, ANSI text, character widths, polylines, the DC
//! brush and pen colours, DIB copies to a device, rectangle regions and
//! the viewport and window origins.
use super::*;

// ---- font files -----------------------------------------------------------------------------

const FR_PRIVATE: u32 = 0x10;
const FR_NOT_ENUM: u32 = 0x20;

/// A font file added with AddFontResource: its families and how many
/// times it was added (each add needs a remove, as on Windows).
struct FontFile {
    path: String,
    families: Vec<String>,
    count: u32,
    enumerable: bool,
}

static FONT_FILES: Mutex<Vec<FontFile>> = Mutex::new(Vec::new());

/// The family names (nameID 1) of the fonts in an sfnt or TrueType
/// collection file; None when it is not one.
fn font_families(data: &[u8]) -> Option<Vec<String>> {
    let offsets: Vec<usize> = if data.get(..4) == Some(b"ttcf") {
        let n = be32(data, 8)? as usize;
        (0..n.min(256)).map(|i| be32(data, 12 + 4 * i).map(|o| o as usize)).collect::<Option<_>>()?
    } else {
        alloc::vec![0]
    };
    let mut out = Vec::new();
    for base in offsets {
        let font = data.get(base..)?;
        let tag = be32(font, 0)?;
        if !matches!(tag, 0x0001_0000 | 0x4F54_544F | 0x7472_7565) {
            return None; // not TrueType, CFF ('OTTO') or 'true'
        }
        // The collection's table offsets are from the file start.
        let (start, len) = sfnt_table_at(data, base, u32::from_be_bytes(*b"name"))?;
        out.push(family_name(data.get(start..start + len)?)?);
    }
    Some(out)
}

/// A table of the font whose offset table is at `base` (offsets in the
/// file, as both single fonts and collections store them).
fn sfnt_table_at(data: &[u8], base: usize, tag: u32) -> Option<(usize, usize)> {
    let n = be16(data, base + 4)? as usize;
    for i in 0..n {
        let rec = base + 12 + 16 * i;
        if be32(data, rec)? == tag {
            let (off, len) = (be32(data, rec + 8)? as usize, be32(data, rec + 12)? as usize);
            return (off.checked_add(len)? <= data.len()).then_some((off, len));
        }
    }
    None
}

/// nameID 1 from a 'name' table: Windows Unicode English first, then any
/// Windows Unicode record, then Mac Roman.
fn family_name(t: &[u8]) -> Option<String> {
    let count = be16(t, 2)? as usize;
    let strings = be16(t, 4)? as usize;
    let mut best: Option<(u8, String)> = None;
    for i in 0..count {
        let r = 6 + 12 * i;
        let (platform, encoding, lang, id) = (be16(t, r)?, be16(t, r + 2)?, be16(t, r + 4)?, be16(t, r + 6)?);
        if id != 1 {
            continue;
        }
        let (len, off) = (be16(t, r + 8)? as usize, be16(t, r + 10)? as usize);
        let Some(raw) = t.get(strings + off..strings + off + len) else { continue };
        let (rank, text) = match (platform, encoding) {
            (3, 1 | 10) | (0, _) => {
                let units: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
                (if platform == 3 && lang == 0x409 { 0 } else { 1 }, String::from_utf16_lossy(&units))
            }
            (1, 0) => (2, raw.iter().map(|&b| b as char).collect()),
            _ => continue,
        };
        if best.as_ref().is_none_or(|b| rank < b.0) {
            best = Some((rank, text));
        }
    }
    best.map(|b| b.1).filter(|s| !s.is_empty())
}

/// AddFontResourceEx: the fonts of the file become available to font
/// enumeration (text is drawn with the system faces GDI maps them to).
/// Returns how many fonts the file holds; 0 when it is not a font file.
fn add_font(name: Option<String>, flags: u32) -> i32 {
    let Some(name) = name.filter(|n| !n.is_empty()) else { return fail(ERROR_INVALID_PARAMETER, 0) };
    if flags & !(FR_PRIVATE | FR_NOT_ENUM) != 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let Ok((path, true)) = crate::kernel32::locate(&name) else { return fail(ERROR_FILE_NOT_FOUND, 0) };
    let key = path.to_ascii_lowercase();
    let mut files = FONT_FILES.lock().unwrap();
    if let Some(f) = files.iter_mut().find(|f| f.path == key) {
        f.count += 1;
        return f.families.len() as i32;
    }
    let Ok(data) = os_std::fs::read(&path) else { return fail(ERROR_FILE_NOT_FOUND, 0) };
    let Some(families) = font_families(&data) else { return 0 };
    let n = families.len() as i32;
    files.push(FontFile { path: key, families, count: 1, enumerable: flags & FR_NOT_ENUM == 0 });
    n
}

fn remove_font(name: Option<String>) -> Bool {
    let Some(name) = name else { return FALSE };
    let Ok((path, _)) = crate::kernel32::locate(&name) else { return FALSE };
    let key = path.to_ascii_lowercase();
    let mut files = FONT_FILES.lock().unwrap();
    let Some(i) = files.iter().position(|f| f.path == key) else { return FALSE };
    files[i].count -= 1;
    if files[i].count == 0 {
        files.remove(i);
    }
    TRUE
}

fn ansi(p: *const u8) -> Option<String> {
    // SAFETY: NULL or a NUL-terminated string.
    unsafe { path::ansi_to_string(p) }
}

fn wide_str(p: *const u16) -> Option<String> {
    // SAFETY: NULL or a NUL-terminated string.
    unsafe { path::wide_to_string(p) }
}

extern "win64" fn AddFontResourceW(name: *const u16) -> i32 {
    add_font(wide_str(name), 0)
}
extern "win64" fn AddFontResourceA(name: *const u8) -> i32 {
    add_font(ansi(name), 0)
}
extern "win64" fn AddFontResourceExW(name: *const u16, flags: u32, _reserved: u64) -> i32 {
    add_font(wide_str(name), flags)
}
extern "win64" fn AddFontResourceExA(name: *const u8, flags: u32, _reserved: u64) -> i32 {
    add_font(ansi(name), flags)
}
extern "win64" fn RemoveFontResourceW(name: *const u16) -> Bool {
    remove_font(wide_str(name))
}
extern "win64" fn RemoveFontResourceA(name: *const u8) -> Bool {
    remove_font(ansi(name))
}
extern "win64" fn RemoveFontResourceExW(name: *const u16, _flags: u32, _reserved: u64) -> Bool {
    remove_font(wide_str(name))
}
extern "win64" fn RemoveFontResourceExA(name: *const u8, _flags: u32, _reserved: u64) -> Bool {
    remove_font(ansi(name))
}

// ---- font enumeration -----------------------------------------------------------------------

/// A face to enumerate: family, the GDI face drawing it, styles
/// ((style name, weight)), character sets, pitch and family bits.
struct Face {
    family: String,
    kind: FontKind,
    styles: &'static [(&'static str, i32)],
    charsets: &'static [u8],
    fixed: bool,
}

/// Character sets, their script names and code-page bits (fsCsb[0]).
const CHARSETS: &[(u8, &str, u32)] = &[
    (0, "Western", 1 << 0),
    (238, "Central European", 1 << 1),
    (204, "Cyrillic", 1 << 2),
    (161, "Greek", 1 << 3),
    (162, "Turkish", 1 << 4),
    (186, "Baltic", 1 << 7),
];

fn faces() -> Vec<Face> {
    let mut v = alloc::vec![
        Face { family: String::from("Cantarell"), kind: FontKind::Sans, styles: &[("Regular", 400), ("Bold", 700)], charsets: &[0, 238, 204, 161, 162, 186], fixed: false },
        Face { family: String::from("Source Code Pro"), kind: FontKind::Mono, styles: &[("Regular", 400)], charsets: &[0, 238, 204, 161, 162], fixed: true },
    ];
    for f in FONT_FILES.lock().unwrap().iter().filter(|f| f.enumerable) {
        for family in &f.families {
            if !v.iter().any(|x| x.family.eq_ignore_ascii_case(family)) {
                v.push(Face { family: family.clone(), kind: FontKind::Sans, styles: &[("Regular", 400)], charsets: &[0], fixed: false });
            }
        }
    }
    v
}

/// ENUMLOGFONTEX and NEWTEXTMETRICEX for one face, style and charset, in
/// the ANSI or wide layout.
fn enum_records(f: &Face, style: (&str, i32), charset: u8, wide: bool) -> (Vec<u8>, Vec<u8>) {
    let k = if style.1 >= 700 && matches!(f.kind, FontKind::Sans) { FontKind::SansBold } else { f.kind };
    let h = line_height(k) as i32;
    let (avg, max) = (measure(k, "x") as i32, measure(k, "W") as i32);
    let pitch_family: u8 = if f.fixed { 0x31 } else { 0x22 };
    let text = |b: &mut Vec<u8>, at: usize, chars: usize, s: &str| {
        if wide {
            for (i, u) in s.encode_utf16().take(chars - 1).enumerate() {
                b[at + 2 * i..at + 2 * i + 2].copy_from_slice(&u.to_le_bytes());
            }
        } else {
            for (i, c) in s.bytes().take(chars - 1).enumerate() {
                b[at + i] = c;
            }
        }
    };
    let cw = if wide { 2 } else { 1 };
    // LOGFONT, then elfFullName[64], elfStyle[32], elfScript[32].
    let lf_size = 28 + 32 * cw;
    let mut elf = alloc::vec![0u8; lf_size + 128 * cw];
    elf[0..4].copy_from_slice(&h.to_le_bytes());
    elf[4..8].copy_from_slice(&avg.to_le_bytes());
    elf[16..20].copy_from_slice(&style.1.to_le_bytes());
    elf[23] = charset;
    elf[24] = 3; // OUT_STROKE_PRECIS
    elf[25] = 2; // CLIP_STROKE_PRECIS
    elf[26] = 1; // DRAFT_QUALITY
    elf[27] = pitch_family;
    text(&mut elf, 28, 32, &f.family);
    let full = if style.0 == "Regular" { f.family.clone() } else { alloc::format!("{} {}", f.family, style.0) };
    text(&mut elf, lf_size, 64, &full);
    text(&mut elf, lf_size + 64 * cw, 32, style.0);
    let script = CHARSETS.iter().find(|c| c.0 == charset).map_or("Western", |c| c.1);
    text(&mut elf, lf_size + 96 * cw, 32, script);
    // TEXTMETRIC (11 LONGs, 4 chars, 5 bytes, padded), then ntmFlags,
    // ntmSizeEM, ntmCellHeight, ntmAvgWidth, and FONTSIGNATURE.
    let tm_size = if wide { 60 } else { 56 };
    let mut ntm = alloc::vec![0u8; tm_size + 16 + 24];
    let longs = [h, h * 4 / 5, h - h * 4 / 5, 0, 0, avg, max, style.1, 0, 96, 96];
    for (i, v) in longs.iter().enumerate() {
        ntm[4 * i..4 * i + 4].copy_from_slice(&v.to_le_bytes());
    }
    // tmFirstChar, tmLastChar, tmDefaultChar, tmBreakChar.
    let chars: [u16; 4] = [0x20, if wide { 0xFFFC } else { 0xFF }, 0x1F, 0x20];
    for (i, c) in chars.iter().enumerate() {
        if wide {
            ntm[44 + 2 * i..46 + 2 * i].copy_from_slice(&c.to_le_bytes());
        } else {
            ntm[44 + i] = *c as u8;
        }
    }
    let flags_at = 44 + 4 * cw;
    // tmItalic, tmUnderlined, tmStruckOut, then pitch and family:
    // TMPF_FIXED_PITCH means variable pitch; vector and TrueType outlines.
    ntm[flags_at + 3] = (pitch_family & 0xF0) | 0x06 | if f.fixed { 0 } else { 0x01 };
    ntm[flags_at + 4] = charset;
    let ntm_flags: u32 = if style.1 >= 700 { 0x20 } else { 0x40 }; // NTM_BOLD / NTM_REGULAR
    let at = tm_size;
    ntm[at..at + 4].copy_from_slice(&ntm_flags.to_le_bytes());
    ntm[at + 4..at + 8].copy_from_slice(&1000u32.to_le_bytes()); // units per em
    ntm[at + 8..at + 12].copy_from_slice(&((h as u32) * 1000 / (h as u32 * 4 / 5).max(1)).to_le_bytes());
    ntm[at + 12..at + 16].copy_from_slice(&((avg as u32) * 1000 / (h as u32 * 4 / 5).max(1)).to_le_bytes());
    let csb: u32 = f.charsets.iter().filter_map(|c| CHARSETS.iter().find(|x| x.0 == *c)).map(|x| x.2).fold(0, |a, b| a | b);
    ntm[at + 16..at + 20].copy_from_slice(&0x0000_0003u32.to_le_bytes()); // Latin-1, Latin Extended-A
    ntm[at + 32..at + 36].copy_from_slice(&csb.to_le_bytes());
    (elf, ntm)
}

/// Calls EnumFontFamExProc(elf, ntm, FontType, lParam).
fn call_enum(proc_: u64, elf: &[u8], ntm: &[u8], lparam: u64) -> i32 {
    const TRUETYPE_FONTTYPE: u32 = 4;
    if crate::wow64::is_active() {
        // LOGFONT and TEXTMETRIC hold no pointers: x86 shares the layout.
        crate::wow64::call32(proc_ as u32, &[elf.as_ptr() as u32, ntm.as_ptr() as u32, TRUETYPE_FONTTYPE, lparam as u32]) as i32
    } else {
        // SAFETY: the program's FONTENUMPROC.
        let f: extern "win64" fn(*const u8, *const u8, u32, u64) -> i32 = unsafe { core::mem::transmute(proc_) };
        f(elf.as_ptr(), ntm.as_ptr(), TRUETYPE_FONTTYPE, lparam)
    }
}

/// EnumFontFamiliesEx: each family once per character set (an empty face
/// name), or every style of the named family. DEFAULT_CHARSET (1) asks for
/// every character set. Returns the callback's last answer, 1 when nothing
/// was enumerated.
fn enum_font_families(dc: u64, lf: *const u8, proc_: u64, lparam: u64, wide: bool) -> i32 {
    if lf.is_null() || proc_ == 0 || !with(|g| g.dcs.contains_key(&dc)) {
        return 0;
    }
    // SAFETY: the caller's LOGFONT: lfCharSet @23, lfFaceName @28.
    let (charset, face) = unsafe {
        (*lf.add(23), if wide { path::wide_to_string(lf.add(28) as *const u16) } else { path::ansi_to_string(lf.add(28)) })
    };
    let face = face.unwrap_or_default();
    let mut last = 1;
    for f in faces() {
        if !face.is_empty() && !f.family.eq_ignore_ascii_case(&face) {
            continue;
        }
        let sets: Vec<u8> = if charset == 1 { f.charsets.to_vec() } else { f.charsets.iter().copied().filter(|&c| c == charset).collect() };
        for set in sets {
            let styles: &[(&str, i32)] = if face.is_empty() { &f.styles[..1] } else { f.styles };
            for &style in styles {
                let (elf, ntm) = enum_records(&f, style, set, wide);
                last = call_enum(proc_, &elf, &ntm, lparam);
                if last == 0 {
                    return 0;
                }
            }
        }
    }
    last
}

extern "win64" fn EnumFontFamiliesExW(dc: u64, lf: *const u8, proc_: u64, lparam: u64, _flags: u32) -> i32 {
    enum_font_families(dc, lf, proc_, lparam, true)
}

extern "win64" fn EnumFontFamiliesExA(dc: u64, lf: *const u8, proc_: u64, lparam: u64, _flags: u32) -> i32 {
    enum_font_families(dc, lf, proc_, lparam, false)
}

// ---- text -----------------------------------------------------------------------------------

/// ExtTextOutA: the text in the ANSI code page (UTF-8); a byte-wise dx
/// array is summed per character.
#[allow(clippy::too_many_arguments)]
extern "win64" fn ExtTextOutA(dc: u64, x: i32, y: i32, flags: u32, rect: *const u8, text: *const u8, n: u32, dx: *const i32) -> Bool {
    if n > 8192 || (n > 0 && text.is_null()) {
        return FALSE;
    }
    // SAFETY: the caller's `n` bytes and optional advances.
    let bytes = if n == 0 { &[][..] } else { unsafe { core::slice::from_raw_parts(text, n as usize) } };
    let s = String::from_utf8_lossy(bytes);
    let units: Vec<u16> = s.encode_utf16().collect();
    let advances: Option<Vec<i32>> = (!dx.is_null() && flags & 0x10 == 0).then(|| {
        // SAFETY: as above: one advance per byte (two with ETO_PDY).
        let pdy = flags & 0x2000 != 0;
        let src = unsafe { core::slice::from_raw_parts(dx, n as usize * if pdy { 2 } else { 1 }) };
        let mut out = Vec::new();
        let mut at = 0;
        for c in s.chars() {
            let len = if s.len() == bytes.len() { c.len_utf8() } else { 1 };
            let (mut ax, mut ay) = (0, 0);
            for k in at..(at + len).min(n as usize) {
                if pdy {
                    ax += src[2 * k];
                    ay += src[2 * k + 1];
                } else {
                    ax += src[k];
                }
            }
            at += len;
            for (i, _) in c.encode_utf16(&mut [0; 2]).iter().enumerate() {
                let (vx, vy) = if i == 0 { (ax, ay) } else { (0, 0) };
                out.push(vx);
                if pdy {
                    out.push(vy);
                }
            }
        }
        out
    });
    ExtTextOutW(dc, x, y, flags, rect, units.as_ptr(), units.len() as u32, advances.as_ref().map_or(core::ptr::null(), |a| a.as_ptr()))
}

fn dc_font(dc: u64) -> Option<FontKind> {
    with(|g| g.dcs.get(&dc).map(|d| font_of(&g.gdi, d.font)))
}

/// ABC widths: the DC's faces are drawn without overhang, so A and C are
/// 0 and B is the advance.
fn abc_widths(dc: u64, first: u32, last: u32, out: *mut i32) -> Bool {
    if out.is_null() || first > last {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    let Some(k) = dc_font(dc) else { return fail(ERROR_INVALID_HANDLE, FALSE) };
    for (i, c) in (first..=last).enumerate() {
        let ch = char::from_u32(c).unwrap_or('\u{fffd}');
        let mut buf = [0; 4];
        let w = measure(k, ch.encode_utf8(&mut buf)) as i32;
        // SAFETY: the caller's array of last-first+1 ABC (12 bytes each).
        unsafe {
            *out.add(3 * i) = 0;
            *out.add(3 * i + 1) = w;
            *out.add(3 * i + 2) = 0;
        }
    }
    TRUE
}

extern "win64" fn GetCharABCWidthsW(dc: u64, first: u32, last: u32, out: *mut i32) -> Bool {
    abc_widths(dc, first, last, out)
}

extern "win64" fn GetCharABCWidthsA(dc: u64, first: u32, last: u32, out: *mut i32) -> Bool {
    if last > 255 {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    abc_widths(dc, first, last, out)
}

extern "win64" fn GetCharWidth32W(dc: u64, first: u32, last: u32, widths: *mut i32) -> Bool {
    if widths.is_null() || first > last {
        return fail(ERROR_INVALID_PARAMETER, FALSE);
    }
    let Some(k) = dc_font(dc) else { return fail(ERROR_INVALID_HANDLE, FALSE) };
    for (i, c) in (first..=last).enumerate() {
        let ch = char::from_u32(c).unwrap_or('\u{fffd}');
        let mut buf = [0; 4];
        // SAFETY: the caller's array of last-first+1 widths.
        unsafe { *widths.add(i) = measure(k, ch.encode_utf8(&mut buf)) as i32 };
    }
    TRUE
}

// ---- lines --------------------------------------------------------------------------------------

fn points<'a>(pts: *const i32, n: i32) -> Option<&'a [i32]> {
    if pts.is_null() || n < 0 {
        return None;
    }
    // SAFETY: the caller's `n` POINTs.
    Some(unsafe { core::slice::from_raw_parts(pts, 2 * n as usize) })
}

/// Polyline: lines through the points with the current pen; the current
/// position is left as it was. In a path the lines become a figure.
extern "win64" fn Polyline(dc: u64, pts: *const i32, n: i32) -> Bool {
    let Some(p) = points(pts, n).filter(|_| n >= 2) else { return fail(ERROR_INVALID_PARAMETER, FALSE) };
    let mut old = [0i32; 2];
    if super::MoveToEx(dc, p[0], p[1], old.as_mut_ptr()) == 0 {
        return FALSE;
    }
    for q in p[2..].chunks_exact(2) {
        super::LineTo(dc, q[0], q[1]);
    }
    super::MoveToEx(dc, old[0], old[1], core::ptr::null_mut());
    TRUE
}

/// PolylineTo: lines from the current position through the points; the
/// last point becomes the current position.
extern "win64" fn PolylineTo(dc: u64, pts: *const i32, n: i32) -> Bool {
    let Some(p) = points(pts, n) else { return fail(ERROR_INVALID_PARAMETER, FALSE) };
    for q in p.chunks_exact(2) {
        if super::LineTo(dc, q[0], q[1]) == 0 {
            return FALSE;
        }
    }
    TRUE
}

// ---- DC brush and pen ----------------------------------------------------------------------------

const CLR_INVALID: u32 = 0xFFFF_FFFF;

extern "win64" fn SetDCBrushColor(dc: u64, c: u32) -> u32 {
    dc_set(dc, |d| to_colorref(core::mem::replace(&mut d.dc_brush, colorref(c)))).unwrap_or(CLR_INVALID)
}

extern "win64" fn GetDCBrushColor(dc: u64) -> u32 {
    with(|g| g.dcs.get(&dc).map_or(CLR_INVALID, |d| to_colorref(d.dc_brush)))
}

extern "win64" fn SetDCPenColor(dc: u64, c: u32) -> u32 {
    dc_set(dc, |d| to_colorref(core::mem::replace(&mut d.dc_pen, colorref(c)))).unwrap_or(CLR_INVALID)
}

extern "win64" fn GetDCPenColor(dc: u64) -> u32 {
    with(|g| g.dcs.get(&dc).map_or(CLR_INVALID, |d| to_colorref(d.dc_pen)))
}

// ---- DIBs and regions ----------------------------------------------------------------------------

/// SetDIBitsToDevice: `lines` scan lines of a DIB starting at `start`
/// (counted as the DIB stores them), copied 1:1 to the device.
#[allow(clippy::too_many_arguments)]
extern "win64" fn SetDIBitsToDevice(
    dc: u64,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    sx: i32,
    sy: i32,
    start: u32,
    lines: u32,
    bits: *const u8,
    info: *const u8,
    usage: u32,
) -> i32 {
    if info.is_null() || bits.is_null() || lines == 0 {
        return 0;
    }
    // SAFETY: the caller's BITMAPINFOHEADER: biSize, biHeight @8,
    // biBitCount @14, biCompression @16, biClrUsed @32.
    let (size, height, bpp, compression, used) = unsafe {
        let rd = |o: usize| core::ptr::read_unaligned(info.add(o) as *const u32);
        let size = rd(0) as usize;
        if size < 40 {
            return 0;
        }
        (size, rd(8) as i32, core::ptr::read_unaligned(info.add(14) as *const u16), rd(16), rd(32))
    };
    // A header for just the lines given, followed by the caller's masks
    // and colour table unchanged.
    let colors = if bpp <= 8 { if used > 0 { used.min(256) } else { 1 << bpp } } else { used.min(256) } as usize;
    let masks = if compression == 3 && size == 40 { 12 } else { 0 };
    let total = size + masks + 4 * colors;
    // SAFETY: the header and what may follow it in the caller's block.
    let mut copy = unsafe { core::slice::from_raw_parts(info, total) }.to_vec();
    let rows = lines.min(height.unsigned_abs().saturating_sub(start)) as i32;
    if rows <= 0 {
        return 0;
    }
    copy[8..12].copy_from_slice(&(if height < 0 { -rows } else { rows }).to_le_bytes());
    // StretchDIBits takes the source row from the top of the band. A
    // bottom-up DIB gives ySrc as the rectangle's lower-left corner, in
    // scan lines from the bottom of the whole DIB.
    let top = if height < 0 { sy - start as i32 } else { start as i32 + rows - sy - h as i32 };
    let copied = StretchDIBits(dc, x, y, w as i32, h as i32, sx, top, w as i32, h as i32, bits, copy.as_ptr(), usage, 0xcc0020);
    if copied == 0 { 0 } else { rows }
}

extern "win64" fn SetRectRgn(rgn: u64, l: i32, t: i32, r: i32, b: i32) -> Bool {
    with(|g| match g.regions.get_mut(&rgn) {
        Some(region) => {
            *region = Region::from_rect(Rect::new(l.min(r), t.min(b), l.abs_diff(r), t.abs_diff(b)));
            TRUE
        }
        None => FALSE,
    })
}

// ---- viewport and window origins ------------------------------------------------------------

fn put_point(out: *mut i32, p: (i32, i32)) {
    if !out.is_null() {
        // SAFETY: the caller's POINT.
        unsafe {
            *out = p.0;
            *out.add(1) = p.1;
        }
    }
}

/// Changes an origin with `f`, reporting the old one; the DC's transform
/// follows.
fn origin(dc: u64, viewport: bool, old: *mut i32, f: impl FnOnce((i32, i32)) -> (i32, i32)) -> Bool {
    dc_set(dc, |d| {
        let o = if viewport { &mut d.viewport_org } else { &mut d.window_org };
        put_point(old, *o);
        *o = f(*o);
        retransform(d);
        TRUE
    })
    .unwrap_or(FALSE)
}

extern "win64" fn SetViewportOrgEx(dc: u64, x: i32, y: i32, old: *mut i32) -> Bool {
    origin(dc, true, old, |_| (x, y))
}
extern "win64" fn OffsetViewportOrgEx(dc: u64, x: i32, y: i32, old: *mut i32) -> Bool {
    origin(dc, true, old, |o| (o.0 + x, o.1 + y))
}
extern "win64" fn GetViewportOrgEx(dc: u64, out: *mut i32) -> Bool {
    origin(dc, true, out, |o| o)
}
extern "win64" fn SetWindowOrgEx(dc: u64, x: i32, y: i32, old: *mut i32) -> Bool {
    origin(dc, false, old, |_| (x, y))
}
extern "win64" fn OffsetWindowOrgEx(dc: u64, x: i32, y: i32, old: *mut i32) -> Bool {
    origin(dc, false, old, |o| (o.0 + x, o.1 + y))
}
extern "win64" fn GetWindowOrgEx(dc: u64, out: *mut i32) -> Bool {
    origin(dc, false, out, |o| o)
}

pub(super) fn exports() -> Vec<(&'static str, u64)> {
    export_table![
        AddFontResourceW,
        AddFontResourceA,
        AddFontResourceExW,
        AddFontResourceExA,
        RemoveFontResourceW,
        RemoveFontResourceA,
        RemoveFontResourceExW,
        RemoveFontResourceExA,
        EnumFontFamiliesExW,
        EnumFontFamiliesExA,
        ExtTextOutA,
        GetCharABCWidthsW,
        GetCharABCWidthsA,
        GetCharWidth32W,
        Polyline,
        PolylineTo,
        SetDCBrushColor,
        GetDCBrushColor,
        SetDCPenColor,
        GetDCPenColor,
        SetDIBitsToDevice,
        SetRectRgn,
        SetViewportOrgEx,
        OffsetViewportOrgEx,
        GetViewportOrgEx,
        SetWindowOrgEx,
        OffsetWindowOrgEx,
        GetWindowOrgEx,
    ]
}
