//! Matroska and WebM (EBML) demuxing: segment information, tracks, and
//! every block of every cluster with its time, key frame flag and place in
//! the file, laced blocks (Xiph, EBML, fixed) split into frames.

use alloc::string::String;
use alloc::vec::Vec;

type R<T> = Result<T, &'static str>;

const EBML: u32 = 0x1A45_DFA3;
const DOC_TYPE: u32 = 0x4282;
const SEGMENT: u32 = 0x1853_8067;
const INFO: u32 = 0x1549_A966;
const TIMECODE_SCALE: u32 = 0x2A_D7B1;
const DURATION: u32 = 0x4489;
const TRACKS: u32 = 0x1654_AE6B;
const TRACK_ENTRY: u32 = 0xAE;
const TRACK_NUMBER: u32 = 0xD7;
const TRACK_TYPE: u32 = 0x83;
const CODEC_ID: u32 = 0x86;
const CODEC_PRIVATE: u32 = 0x63A2;
const CODEC_DELAY: u32 = 0x56AA;
const SEEK_PREROLL: u32 = 0x56BB;
const DEFAULT_DURATION: u32 = 0x23_E383;
const VIDEO: u32 = 0xE0;
const PIXEL_WIDTH: u32 = 0xB0;
const PIXEL_HEIGHT: u32 = 0xBA;
const AUDIO: u32 = 0xE1;
const SAMPLING_FREQUENCY: u32 = 0xB5;
const CHANNELS: u32 = 0x9F;
const CLUSTER: u32 = 0x1F43_B675;
const TIMECODE: u32 = 0xE7;
const SIMPLE_BLOCK: u32 = 0xA3;
const BLOCK_GROUP: u32 = 0xA0;
const BLOCK: u32 = 0xA1;
const REFERENCE_BLOCK: u32 = 0xFB;
const DISCARD_PADDING: u32 = 0x75A2;
const CONTENT_ENCODINGS: u32 = 0x6D80;

/// Top-level elements that end a cluster of unknown size.
const LEVEL1: [u32; 9] = [CLUSTER, INFO, TRACKS, 0x114D_9B74, 0x1C53_BB6B, 0x1941_A469, 0x1043_A770, 0x1254_C367, EBML];

/// One frame of a track: its time and its bytes in the file.
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    pub ns: i64,
    pub key: bool,
    /// Time to drop from the end of the decoded frame (DiscardPadding).
    pub discard_ns: i64,
    pub start: usize,
    pub len: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Video { w: u32, h: u32 },
    Audio { rate: f64, channels: u32 },
    Other,
}

#[derive(Clone, Debug)]
pub struct Track {
    pub number: u64,
    pub kind: Kind,
    pub codec: String,
    pub private: Vec<u8>,
    pub codec_delay_ns: u64,
    pub seek_preroll_ns: u64,
    pub default_duration_ns: u64,
    /// Compressed or encrypted content: not playable here.
    pub encoded: bool,
    pub frames: Vec<Frame>,
}

pub struct Demuxed {
    pub doc_type: String,
    pub duration_ns: Option<u64>,
    pub tracks: Vec<Track>,
}

/// A variable-length integer; `marker` keeps the length bit (IDs).
fn vint(d: &[u8], p: &mut usize, marker: bool) -> R<(u64, bool)> {
    let b = *d.get(*p).ok_or("EBML data ends early")?;
    let len = b.leading_zeros() as usize + 1;
    if len > 8 {
        return Err("bad EBML number");
    }
    let bytes = d.get(*p..*p + len).ok_or("EBML data ends early")?;
    let mut v = if marker { b as u64 } else { (b as u64) & ((1u64 << (8 - len)) - 1) };
    let mut all_ones = v == (1u64 << (8 - len)) - 1;
    for &x in &bytes[1..] {
        v = (v << 8) | x as u64;
        all_ones &= x == 0xFF;
    }
    *p += len;
    Ok((v, !marker && all_ones))
}

/// An element header at `p`: (id, data start, data end). Unknown sizes
/// run to `limit`.
fn header(d: &[u8], p: usize, limit: usize) -> R<(u32, usize, usize, bool)> {
    let mut q = p;
    let (id, _) = vint(d, &mut q, true)?;
    let (size, unknown) = vint(d, &mut q, false)?;
    let end = if unknown { limit } else { q.checked_add(size as usize).ok_or("bad EBML size")?.min(limit) };
    Ok((id as u32, q, end, unknown))
}

/// The children of `d[start..end]`.
fn children(d: &[u8], start: usize, end: usize) -> R<Vec<(u32, usize, usize)>> {
    let mut out = Vec::new();
    let mut p = start;
    while p < end {
        let (id, s, e, _) = header(d, p, end)?;
        out.push((id, s, e));
        p = e;
    }
    Ok(out)
}

fn uint(d: &[u8]) -> u64 {
    d.iter().take(8).fold(0, |v, &b| (v << 8) | b as u64)
}

fn float(d: &[u8]) -> f64 {
    match d.len() {
        4 => f32::from_bits(uint(d) as u32) as f64,
        8 => f64::from_bits(uint(d)),
        _ => 0.0,
    }
}

fn track_entry(d: &[u8], s: usize, e: usize) -> R<Track> {
    let mut t = Track {
        number: 0,
        kind: Kind::Other,
        codec: String::new(),
        private: Vec::new(),
        codec_delay_ns: 0,
        seek_preroll_ns: 0,
        default_duration_ns: 0,
        encoded: false,
        frames: Vec::new(),
    };
    let mut kind = 0;
    for (id, cs, ce) in children(d, s, e)? {
        let v = &d[cs..ce];
        match id {
            TRACK_NUMBER => t.number = uint(v),
            TRACK_TYPE => kind = uint(v),
            CODEC_ID => t.codec = String::from_utf8_lossy(v).trim_end_matches('\0').into(),
            CODEC_PRIVATE => t.private = v.to_vec(),
            CODEC_DELAY => t.codec_delay_ns = uint(v),
            SEEK_PREROLL => t.seek_preroll_ns = uint(v),
            DEFAULT_DURATION => t.default_duration_ns = uint(v),
            CONTENT_ENCODINGS => t.encoded = true,
            VIDEO => {
                let (mut w, mut h) = (0, 0);
                for (id, a, b) in children(d, cs, ce)? {
                    match id {
                        PIXEL_WIDTH => w = uint(&d[a..b]) as u32,
                        PIXEL_HEIGHT => h = uint(&d[a..b]) as u32,
                        _ => {}
                    }
                }
                t.kind = Kind::Video { w, h };
            }
            AUDIO => {
                let (mut rate, mut channels) = (8000.0, 1);
                for (id, a, b) in children(d, cs, ce)? {
                    match id {
                        SAMPLING_FREQUENCY => rate = float(&d[a..b]),
                        CHANNELS => channels = uint(&d[a..b]) as u32,
                        _ => {}
                    }
                }
                t.kind = Kind::Audio { rate, channels };
            }
            _ => {}
        }
    }
    if !matches!((kind, &t.kind), (1, Kind::Video { .. }) | (2, Kind::Audio { .. })) {
        t.kind = match kind {
            1 => Kind::Video { w: 0, h: 0 },
            2 => Kind::Audio { rate: 8000.0, channels: 1 },
            _ => Kind::Other,
        };
    }
    Ok(t)
}

/// Splits a block's frames; `d[s..e]` is the block after its header.
fn lace(d: &[u8], s: usize, e: usize, flags: u8) -> R<Vec<(usize, usize)>> {
    let lacing = (flags >> 1) & 3;
    if lacing == 0 {
        return Ok(alloc::vec![(s, e - s)]);
    }
    let n = *d.get(s).ok_or("short Matroska block")? as usize + 1;
    let mut p = s + 1;
    let mut sizes = Vec::with_capacity(n);
    match lacing {
        1 => {
            for _ in 0..n - 1 {
                let mut size = 0usize;
                loop {
                    let b = *d.get(p).ok_or("short Matroska lacing")?;
                    p += 1;
                    size += b as usize;
                    if b != 255 {
                        break;
                    }
                }
                sizes.push(size);
            }
        }
        3 => {
            let (first, _) = vint(d, &mut p, false)?;
            sizes.push(first as usize);
            let mut prev = first as i64;
            for _ in 1..n - 1 {
                let q = p;
                let (raw, _) = vint(d, &mut p, false)?;
                let bits = 7 * (p - q) as u32;
                let delta = raw as i64 - ((1i64 << (bits - 1)) - 1);
                prev += delta;
                if prev < 0 {
                    return Err("bad Matroska lacing");
                }
                sizes.push(prev as usize);
            }
        }
        _ => {
            let each = (e.saturating_sub(p)) / n;
            sizes.resize(n - 1, each);
        }
    }
    let used: usize = sizes.iter().sum();
    let last = e.checked_sub(p + used).ok_or("bad Matroska lacing")?;
    sizes.push(last);
    let mut out = Vec::with_capacity(n);
    for size in sizes {
        out.push((p, size));
        p += size;
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn block(
    d: &[u8],
    s: usize,
    e: usize,
    cluster_ts: i64,
    simple: bool,
    key_hint: bool,
    discard_ns: i64,
    scale: u64,
    tracks: &mut [Track],
) -> R<()> {
    let mut p = s;
    let (number, _) = vint(d, &mut p, false)?;
    let rel = i16::from_be_bytes(d.get(p..p + 2).ok_or("short Matroska block")?.try_into().unwrap()) as i64;
    let flags = *d.get(p + 2).ok_or("short Matroska block")?;
    p += 3;
    let Some(t) = tracks.iter_mut().find(|t| t.number == number) else { return Ok(()) };
    let key = if simple { flags & 0x80 != 0 } else { key_hint };
    let ns = (cluster_ts + rel) * scale as i64;
    for (k, (start, len)) in lace(d, p, e, flags)?.into_iter().enumerate() {
        t.frames.push(Frame { ns: ns + k as i64 * t.default_duration_ns as i64, key, discard_ns, start, len });
    }
    Ok(())
}

pub fn demux(d: &[u8]) -> R<Demuxed> {
    let (id, s, e, _) = header(d, 0, d.len())?;
    if id != EBML {
        return Err("not a Matroska file");
    }
    let mut doc_type = String::from("matroska");
    for (id, a, b) in children(d, s, e)? {
        if id == DOC_TYPE {
            doc_type = String::from_utf8_lossy(&d[a..b]).trim_end_matches('\0').into();
        }
    }
    let (id, mut s, seg_end, _) = header(d, e, d.len())?;
    if id != SEGMENT {
        return Err("Matroska file without a segment");
    }
    let mut scale = 1_000_000u64;
    let mut duration = None;
    let mut tracks: Vec<Track> = Vec::new();
    while s < seg_end {
        let (id, cs, ce, unknown) = match header(d, s, seg_end) {
            Ok(h) => h,
            // A truncated file: keep what was read.
            Err(_) => break,
        };
        match id {
            INFO => {
                for (id, a, b) in children(d, cs, ce)? {
                    match id {
                        TIMECODE_SCALE => scale = uint(&d[a..b]).max(1),
                        DURATION => duration = Some(float(&d[a..b])),
                        _ => {}
                    }
                }
            }
            TRACKS => {
                for (id, a, b) in children(d, cs, ce)? {
                    if id == TRACK_ENTRY {
                        tracks.push(track_entry(d, a, b)?);
                    }
                }
            }
            CLUSTER => {
                let mut ts = 0i64;
                let mut q = cs;
                let mut end = ce;
                while q < end {
                    let Ok((id, bs, be, _)) = header(d, q, end) else { break };
                    if unknown && LEVEL1.contains(&id) {
                        end = q;
                        break;
                    }
                    match id {
                        TIMECODE => ts = uint(&d[bs..be]) as i64,
                        SIMPLE_BLOCK => block(d, bs, be, ts, true, true, 0, scale, &mut tracks)?,
                        BLOCK_GROUP => {
                            let kids = children(d, bs, be).unwrap_or_default();
                            let key = !kids.iter().any(|k| k.0 == REFERENCE_BLOCK);
                            let discard = kids.iter().find(|k| k.0 == DISCARD_PADDING).map_or(0, |&(_, a, b)| {
                                // A signed integer.
                                let v = uint(&d[a..b]);
                                let bits = 8 * (b - a).min(8) as u32;
                                if bits > 0 && bits < 64 && v >> (bits - 1) & 1 == 1 {
                                    v as i64 - (1i64 << bits)
                                } else {
                                    v as i64
                                }
                            });
                            if let Some(&(_, a, b)) = kids.iter().find(|k| k.0 == BLOCK) {
                                block(d, a, b, ts, false, key, discard, scale, &mut tracks)?;
                            }
                        }
                        _ => {}
                    }
                    q = be;
                }
                if unknown {
                    s = end;
                    continue;
                }
            }
            _ => {}
        }
        s = ce;
    }
    let duration_ns = duration.map(|v| (v * scale as f64) as u64);
    Ok(Demuxed { doc_type, duration_ns, tracks })
}

/// The packets of a Xiph-laced CodecPrivate (Vorbis headers).
pub fn xiph_headers(p: &[u8]) -> R<Vec<&[u8]>> {
    let n = *p.first().ok_or("empty codec data")? as usize + 1;
    let mut q = 1;
    let mut sizes = Vec::new();
    for _ in 0..n - 1 {
        let mut size = 0;
        loop {
            let b = *p.get(q).ok_or("short codec data")?;
            q += 1;
            size += b as usize;
            if b != 255 {
                break;
            }
        }
        sizes.push(size);
    }
    let used: usize = sizes.iter().sum();
    sizes.push(p.len().checked_sub(q + used).ok_or("short codec data")?);
    let mut out = Vec::new();
    for s in sizes {
        out.push(&p[q..q + s]);
        q += s;
    }
    Ok(out)
}
