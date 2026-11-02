//! AAC (ISO/IEC 14496-3) in ADTS streams (`.aac`) and MP4/M4A files: the
//! Low Complexity profile with every tool it allows (M/S and intensity
//! stereo, noise substitution, temporal noise shaping, pulses, sine and KBD
//! windows). HE-AAC files play their AAC core: the spectral band
//! replication on top is left out, so they sound duller but play.

use super::aac_tables::*;
use super::mdct::Imdct;
use super::{id3v2_len, Decoder, Info};
use crate::bits::MsbReader;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

const RATES: [u32; 13] = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];

const ZERO_HCB: u8 = 0;
const ESC_HCB: u8 = 11;
const NOISE_HCB: u8 = 13;
const INTENSITY_HCB2: u8 = 14;
const INTENSITY_HCB: u8 = 15;

const LONG_START: u8 = 1;
const EIGHT_SHORT: u8 = 2;
const LONG_STOP: u8 = 3;

const ID_SCE: u32 = 0;
const ID_CPE: u32 = 1;
const ID_CCE: u32 = 2;
const ID_LFE: u32 = 3;
const ID_DSE: u32 = 4;
const ID_PCE: u32 = 5;
const ID_FIL: u32 = 6;
const ID_END: u32 = 7;

/// Most channels a file may have (7.1 plus a few from a PCE).
const MAX_CHANNELS: usize = 16;

type R<T> = Result<T, &'static str>;

/// A Huffman decoder: a table indexed by the next `root` bits, with
/// second-level tables for longer codes.
struct Vlc {
    root: u32,
    table: Vec<u32>,
}

/// Table entry: a second-level table (offset << 5 | bits).
const SUB: u32 = 1 << 31;

impl Vlc {
    fn new<C: Copy + Into<u32>>(codes: &[C], bits: &[u8], root: u32) -> Vlc {
        let mut table = vec![0u32; 1 << root];
        let mut extra = vec![0u32; 1 << root];
        for (s, (&c, &l)) in codes.iter().zip(bits).enumerate() {
            let (c, l) = (c.into(), l as u32);
            if l <= root {
                let at = (c << (root - l)) as usize;
                for e in &mut table[at..at + (1 << (root - l))] {
                    *e = (s as u32) << 8 | l;
                }
            } else {
                let p = (c >> (l - root)) as usize;
                extra[p] = extra[p].max(l - root);
            }
        }
        for p in 0..1usize << root {
            if extra[p] > 0 {
                table[p] = SUB | (table.len() as u32) << 5 | extra[p];
                table.resize(table.len() + (1 << extra[p]), 0);
            }
        }
        for (s, (&c, &l)) in codes.iter().zip(bits).enumerate() {
            let (c, l) = (c.into(), l as u32);
            if l > root {
                let e = table[(c >> (l - root)) as usize];
                let (off, x) = ((e & !SUB) >> 5, e & 31);
                let rest = c & ((1 << (l - root)) - 1);
                let at = (off + (rest << (x - (l - root)))) as usize;
                for e in &mut table[at..at + (1 << (x - (l - root)))] {
                    *e = (s as u32) << 8 | l;
                }
            }
        }
        Vlc { root, table }
    }

    fn read(&self, r: &mut MsbReader) -> R<u32> {
        let mut e = self.table[r.peek_unchecked(self.root) as usize];
        if e & SUB != 0 {
            let (off, x) = ((e & !SUB) >> 5, e & 31);
            let idx = r.peek_unchecked(self.root + x) & ((1 << x) - 1);
            e = self.table[(off + idx) as usize];
        }
        if e == 0 {
            return Err("bad Huffman code");
        }
        r.skip((e & 0xFF) as usize).ok_or("frame ends early")?;
        Ok(e >> 8)
    }
}

/// Bits of a stream: what the decoder needs from the AudioSpecificConfig
/// or the ADTS header.
#[derive(Clone, Copy, Debug)]
struct Config {
    /// Audio object type of the core (2 = LC).
    aot: u32,
    sf_index: usize,
    rate: u32,
    /// Channel configuration, 0 when a program config element says.
    config: u8,
    channels: usize,
    /// Spectral band replication on top (HE-AAC).
    sbr: bool,
}

fn sf_index_of(rate: u32) -> usize {
    const LIMITS: [u32; 11] = [92017, 75132, 55426, 46009, 37566, 27713, 23004, 18783, 13856, 11502, 9391];
    LIMITS.iter().position(|&l| rate >= l).unwrap_or(11)
}

fn read_aot(r: &mut MsbReader) -> R<u32> {
    let a = r.read(5).ok_or("short config")?;
    Ok(if a == 31 { 32 + r.read(6).ok_or("short config")? } else { a })
}

fn read_rate(r: &mut MsbReader) -> R<(usize, u32)> {
    let i = r.read(4).ok_or("short config")? as usize;
    if i == 15 {
        let rate = r.read(24).ok_or("short config")?;
        Ok((sf_index_of(rate), rate))
    } else if i < 13 {
        Ok((i, RATES[i]))
    } else {
        Err("reserved sampling frequency")
    }
}

/// Channels of a program config element (4.4.1.1). Leaves the reader after it.
fn read_pce(r: &mut MsbReader) -> R<usize> {
    let mut rd = |n| r.read(n).ok_or("short program config");
    let _tag = rd(4)?;
    let _profile = rd(2)?;
    let _rate = rd(4)?;
    let (front, side, back) = (rd(4)?, rd(4)?, rd(4)?);
    let (lfe, assoc, cc) = (rd(2)?, rd(3)?, rd(4)?);
    for _ in 0..2 {
        if rd(1)? == 1 {
            rd(4)?;
        }
    }
    if rd(1)? == 1 {
        rd(3)?;
    }
    let mut channels = lfe as usize;
    for _ in 0..front + side + back {
        channels += 1 + rd(1)? as usize;
        rd(4)?;
    }
    for _ in 0..lfe + assoc {
        rd(4)?;
    }
    for _ in 0..cc {
        rd(5)?;
    }
    r.align();
    let comment = r.read(8).ok_or("short program config")?;
    r.skip(comment as usize * 8).ok_or("short program config")?;
    Ok(channels)
}

/// AudioSpecificConfig (1.6.2.1).
fn parse_asc(asc: &[u8]) -> R<Config> {
    let mut r = MsbReader::new(asc);
    let mut aot = read_aot(&mut r)?;
    let (mut sf_index, mut rate) = read_rate(&mut r)?;
    let config = r.read(4).ok_or("short config")? as u8;
    let mut sbr = false;
    if aot == 5 || aot == 29 {
        sbr = true;
        read_rate(&mut r)?;
        aot = read_aot(&mut r)?;
    }
    if !matches!(aot, 1..=4 | 6 | 7 | 17 | 19..=23) {
        return Err("unsupported audio object type");
    }
    if r.read(1) != Some(0) {
        return Err("AAC with 960-sample frames is not supported");
    }
    if r.read(1) == Some(1) {
        r.skip(14);
    }
    r.read(1);
    let channels = match config {
        0 => read_pce(&mut r)?,
        1..=6 => config as usize,
        7 => 8,
        _ => return Err("unsupported channel configuration"),
    };
    if rate == 0 {
        return Err("no sampling rate");
    }
    if sf_index > 12 {
        sf_index = 11;
        rate = 8000;
    }
    Ok(Config { aot, sf_index, rate, config, channels, sbr })
}

#[derive(Clone, Copy, Debug)]
struct Adts {
    crc: bool,
    aot: u32,
    sf_index: usize,
    config: u8,
    len: usize,
    blocks: usize,
}

fn adts_header(d: &[u8]) -> Option<Adts> {
    if d.len() < 7 || d[0] != 0xFF || d[1] & 0xF6 != 0xF0 {
        return None;
    }
    let sf_index = ((d[2] >> 2) & 15) as usize;
    let len = ((d[3] as usize & 3) << 11) | (d[4] as usize) << 3 | (d[5] as usize >> 5);
    let crc = d[1] & 1 == 0;
    if sf_index > 12 || len < if crc { 9 } else { 7 } {
        return None;
    }
    Some(Adts {
        crc,
        aot: (d[2] >> 6) as u32 + 1,
        sf_index,
        config: ((d[2] & 1) << 2) | (d[3] >> 6),
        len,
        blocks: (d[6] & 3) as usize + 1,
    })
}

/// Whether `data` (after any ID3v2 tag) starts with two ADTS frames in a row.
pub fn is_adts(data: &[u8]) -> bool {
    let p = id3v2_len(data).unwrap_or(0);
    let Some(h) = data.get(p..).and_then(adts_header) else { return false };
    match data.get(p + h.len..) {
        Some([]) => true,
        Some(rest) => adts_header(rest).is_some_and(|n| n.sf_index == h.sf_index),
        None => false,
    }
}

/// Whether `data` is an ISO base media file (MP4, M4A, 3GP, MOV).
pub fn is_mp4(data: &[u8]) -> bool {
    data.len() >= 12 && matches!(&data[4..8], b"ftyp" | b"moov" | b"mdat" | b"wide" | b"free" | b"skip")
}

fn be16(d: &[u8], at: usize) -> Option<u32> {
    Some(u16::from_be_bytes(d.get(at..at + 2)?.try_into().ok()?) as u32)
}

fn be32(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

fn be64(d: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(d.get(at..at + 8)?.try_into().ok()?))
}

/// The boxes in `d[start..end]`: (type, start of the box, body start, end).
fn boxes(d: &[u8], start: usize, end: usize) -> Vec<([u8; 4], usize, usize, usize)> {
    let mut out = Vec::new();
    let mut p = start;
    let end = end.min(d.len());
    while p + 8 <= end {
        let size = be32(d, p).unwrap() as u64;
        let kind: [u8; 4] = d[p + 4..p + 8].try_into().unwrap();
        let (body, size) = match size {
            0 => (p + 8, (end - p) as u64),
            1 => match be64(d, p + 8) {
                Some(s) => (p + 16, s),
                None => break,
            },
            s => (p + 8, s),
        };
        if size < (body - p) as u64 || p as u64 + size > end as u64 {
            break;
        }
        let e = p + size as usize;
        out.push((kind, p, body, e));
        p = e;
    }
    out
}

fn child(d: &[u8], start: usize, end: usize, kind: &[u8; 4]) -> Option<(usize, usize)> {
    boxes(d, start, end).into_iter().find(|b| &b.0 == kind).map(|b| (b.2, b.3))
}

/// An expandable descriptor length (14496-1 8.3.3).
fn desc_len(d: &[u8], p: &mut usize) -> Option<usize> {
    let mut n = 0usize;
    for _ in 0..4 {
        let b = *d.get(*p)?;
        *p += 1;
        n = (n << 7) | (b & 0x7F) as usize;
        if b & 0x80 == 0 {
            break;
        }
    }
    Some(n)
}

/// The AudioSpecificConfig inside an `esds` body.
fn esds_asc(d: &[u8], start: usize, end: usize) -> Option<&[u8]> {
    let mut p = start + 4;
    if *d.get(p)? != 3 {
        return None;
    }
    p += 1;
    desc_len(d, &mut p)?;
    let flags = *d.get(p + 2)?;
    p += 3;
    if flags & 0x80 != 0 {
        p += 2;
    }
    if flags & 0x40 != 0 {
        p += 1 + *d.get(p)? as usize;
    }
    if flags & 0x20 != 0 {
        p += 2;
    }
    if *d.get(p)? != 4 {
        return None;
    }
    p += 1;
    desc_len(d, &mut p)?;
    let oti = *d.get(p)?;
    // MPEG-4 audio, or MPEG-2 AAC Main, LC and SSR.
    if !matches!(oti, 0x40 | 0x66..=0x68) {
        return None;
    }
    p += 13;
    if *d.get(p)? != 5 {
        return None;
    }
    p += 1;
    let n = desc_len(d, &mut p)?;
    d.get(p..(p + n).min(end))
}

/// Where the audio of an MP4 file is: its config and the byte range of
/// every AAC frame, with gapless trimming when the file says.
struct Mp4 {
    asc: Vec<u8>,
    frames: Vec<(usize, usize)>,
    timescale: u32,
    /// Samples to drop at the start and the samples to keep, in the media
    /// time scale.
    skip: u64,
    keep: Option<u64>,
}

fn parse_mp4(d: &[u8]) -> Result<Mp4, String> {
    let top = boxes(d, 0, d.len());
    let moov = top.iter().find(|b| &b.0 == b"moov").ok_or("no movie box in this MP4 file")?;
    let mvhd = child(d, moov.2, moov.3, b"mvhd");
    let movie_scale = mvhd.and_then(|(s, _)| be32(d, s + if d.get(s) == Some(&1) { 20 } else { 12 })).unwrap_or(0);
    let mut other = None;
    for trak in boxes(d, moov.2, moov.3).into_iter().filter(|b| &b.0 == b"trak") {
        let (ts, te) = (trak.2, trak.3);
        let Some((ms, me)) = child(d, ts, te, b"mdia") else { continue };
        let Some((hs, _)) = child(d, ms, me, b"hdlr") else { continue };
        if d.get(hs + 8..hs + 12) != Some(b"soun") {
            continue;
        }
        let stbl = child(d, ms, me, b"minf").and_then(|(s, e)| child(d, s, e, b"stbl"));
        let Some((ss, se)) = stbl else { continue };
        let Some((ds, de)) = child(d, ss, se, b"stsd") else { continue };
        let Some(entry) = boxes(d, ds + 8, de).into_iter().next() else { continue };
        if &entry.0 != b"mp4a" {
            other = Some(match &entry.0 {
                b"alac" => "Apple Lossless",
                b"ac-3" | b"ec-3" => "Dolby Digital",
                b"Opus" => "Opus",
                b"fLaC" => "FLAC",
                b".mp3" => "MP3",
                _ => "a codec OSyx does not know",
            });
            continue;
        }
        // AudioSampleEntry: 28 bytes, 16 or 36 more for QuickTime versions 1 and 2.
        let version = be16(d, entry.2 + 8).unwrap_or(0);
        let kids = entry.2 + 28 + [0, 16, 36].get(version as usize).copied().unwrap_or(0);
        let mut asc = None;
        let mut stack = vec![(kids, entry.3)];
        while let Some((s, e)) = stack.pop() {
            for b in boxes(d, s, e) {
                match &b.0 {
                    b"esds" => asc = asc.or_else(|| esds_asc(d, b.2, b.3)),
                    b"wave" => stack.push((b.2, b.3)),
                    _ => {}
                }
            }
        }
        let asc = asc.ok_or("no AAC configuration in this MP4 file")?.to_vec();
        let mdhd = child(d, ms, me, b"mdhd").ok_or("no media header")?;
        let timescale = be32(d, mdhd.0 + if d.get(mdhd.0) == Some(&1) { 20 } else { 12 }).unwrap_or(0);
        let track_id =
            child(d, ts, te, b"tkhd").and_then(|(s, _)| be32(d, s + if d.get(s) == Some(&1) { 20 } else { 12 }));

        let mut frames = sample_table(d, ss, se).ok_or("damaged sample table")?;
        // Fragmented files keep their samples in movie fragments.
        let trex = child(d, moov.2, moov.3, b"mvex").and_then(|(s, e)| {
            boxes(d, s, e).into_iter().find(|b| &b.0 == b"trex" && be32(d, b.2 + 4) == track_id).map(|b| b.2)
        });
        let default_size = trex.and_then(|t| be32(d, t + 16)).unwrap_or(0);
        for moof in top.iter().filter(|b| &b.0 == b"moof") {
            for traf in boxes(d, moof.2, moof.3).into_iter().filter(|b| &b.0 == b"traf") {
                fragment(d, moof.1, traf.2, traf.3, track_id, default_size, &mut frames);
            }
        }

        // Gapless playback: the edit list, else iTunes' iTunSMPB.
        let (mut skip, mut keep) = (0, None);
        let elst = child(d, ts, te, b"edts").and_then(|(s, e)| child(d, s, e, b"elst"));
        if let Some((es, _)) = elst {
            let v1 = d.get(es) == Some(&1);
            let n = be32(d, es + 4).unwrap_or(0);
            let mut p = es + 8;
            for _ in 0..n.min(16) {
                let (dur, time) = if v1 {
                    (be64(d, p), be64(d, p + 8).map(|t| t as i64))
                } else {
                    (be32(d, p).map(|v| v as u64), be32(d, p + 4).map(|t| t as i32 as i64))
                };
                p += if v1 { 20 } else { 12 };
                let (Some(dur), Some(time)) = (dur, time) else { break };
                if time == -1 {
                    continue; // an empty edit: a delay before the track
                }
                skip = time as u64;
                if movie_scale > 0 && dur > 0 {
                    keep = Some(dur * timescale as u64 / movie_scale as u64);
                }
                break;
            }
        } else if let Some((pre, pad, total)) = itunsmpb(d, moov.2, moov.3) {
            skip = pre;
            keep = Some(total).filter(|&t| t > 0).or(Some((frames.len() as u64 * 1024).saturating_sub(pre + pad)));
        }
        return Ok(Mp4 { asc, frames, timescale, skip, keep });
    }
    Err(match other {
        Some(c) => format!("this MP4 file holds {c}, which OSyx cannot play yet"),
        None => String::from("no sound track in this MP4 file"),
    })
}

/// Byte ranges of the samples in a sample table (stsz, stsc, stco/co64).
fn sample_table(d: &[u8], s: usize, e: usize) -> Option<Vec<(usize, usize)>> {
    let mut frames = Vec::new();
    let Some((zs, _)) = child(d, s, e, b"stsz") else { return Some(frames) };
    let fixed = be32(d, zs + 4)? as usize;
    let count = be32(d, zs + 8)? as usize;
    let size = |i: usize| if fixed != 0 { Some(fixed) } else { be32(d, zs + 12 + 4 * i).map(|v| v as usize) };
    let (cs, wide) = match child(d, s, e, b"stco") {
        Some((cs, _)) => (cs, false),
        None => (child(d, s, e, b"co64")?.0, true),
    };
    let chunks = be32(d, cs + 4)? as usize;
    let chunk = |i: usize| {
        if wide {
            be64(d, cs + 8 + 8 * i).map(|v| v as usize)
        } else {
            be32(d, cs + 8 + 4 * i).map(|v| v as usize)
        }
    };
    let (ps, _) = child(d, s, e, b"stsc")?;
    let runs = be32(d, ps + 4)? as usize;
    let mut sample = 0;
    for r in 0..runs {
        let first = be32(d, ps + 8 + 12 * r)? as usize;
        let per = be32(d, ps + 12 + 12 * r)? as usize;
        let next = if r + 1 < runs { be32(d, ps + 8 + 12 * (r + 1))? as usize } else { chunks + 1 };
        for c in first.max(1)..next.min(chunks + 1) {
            let mut at = chunk(c - 1)?;
            for _ in 0..per {
                if sample >= count {
                    return Some(frames);
                }
                let n = size(sample)?;
                frames.push((at, n));
                at += n;
                sample += 1;
            }
        }
    }
    Some(frames)
}

/// Appends the samples of one track fragment.
fn fragment(
    d: &[u8],
    moof: usize,
    s: usize,
    e: usize,
    track: Option<u32>,
    default_size: u32,
    out: &mut Vec<(usize, usize)>,
) -> Option<()> {
    let (hs, _) = child(d, s, e, b"tfhd")?;
    let flags = be32(d, hs)? & 0xFF_FFFF;
    if track.is_some() && be32(d, hs + 4) != track {
        return Some(());
    }
    let mut p = hs + 8;
    let mut base = moof;
    if flags & 1 != 0 {
        base = be64(d, p)? as usize;
        p += 8;
    }
    p += if flags & 2 != 0 { 4 } else { 0 };
    p += if flags & 8 != 0 { 4 } else { 0 };
    let size = if flags & 0x10 != 0 { be32(d, p)? } else { default_size };
    let mut at = base;
    for trun in boxes(d, s, e).into_iter().filter(|b| &b.0 == b"trun") {
        let t = trun.2;
        let f = be32(d, t)? & 0xFF_FFFF;
        let n = be32(d, t + 4)? as usize;
        let mut q = t + 8;
        if f & 1 != 0 {
            at = (base as i64 + be32(d, q)? as i32 as i64) as usize;
            q += 4;
        }
        if f & 4 != 0 {
            q += 4;
        }
        let stride = [0x100, 0x200, 0x400, 0x800].iter().filter(|&&b| f & b != 0).count() * 4;
        for i in 0..n.min(1 << 24) {
            let sz = if f & 0x200 != 0 { be32(d, q + i * stride + if f & 0x100 != 0 { 4 } else { 0 })? } else { size };
            out.push((at, sz as usize));
            at += sz as usize;
        }
    }
    Some(())
}

/// iTunes' gapless data: (priming, padding, length) from the iTunSMPB text.
fn itunsmpb(d: &[u8], s: usize, e: usize) -> Option<(u64, u64, u64)> {
    let area = d.get(s..e)?;
    let at = area.windows(8).position(|w| w == b"iTunSMPB")?;
    let rest = &area[at..];
    let data = rest.windows(4).position(|w| w == b"data")?;
    let text = rest.get(data + 12..)?;
    let text = &text[..text.iter().position(|&b| b == 0).unwrap_or(text.len()).min(128)];
    let text = core::str::from_utf8(text).ok()?;
    let mut f = text.split_ascii_whitespace().map(|w| u64::from_str_radix(w, 16).ok());
    let _ = f.next()?;
    Some((f.next()??, f.next()??, f.next()??))
}

#[derive(Clone, Copy, Default)]
struct IcsInfo {
    seq: u8,
    kbd: bool,
    max_sfb: usize,
    groups: usize,
    group_len: [u8; 8],
}

impl IcsInfo {
    fn short(&self) -> bool {
        self.seq == EIGHT_SHORT
    }
}

#[derive(Clone, Copy, Default)]
struct TnsFilter {
    len: u8,
    order: u8,
    down: bool,
    lpc: [f32; 13],
}

#[derive(Clone, Copy, Default)]
struct Tns {
    n: [u8; 8],
    f: [[TnsFilter; 3]; 8],
}

/// One channel's spectrum for a frame.
struct Ics {
    info: IcsInfo,
    cb: [u8; 8 * 64],
    /// Scale factor, intensity position or noise energy per band.
    sf: [i32; 8 * 64],
    spec: [f32; 1024],
    tns: Option<Tns>,
}

impl Ics {
    fn new() -> Ics {
        Ics { info: IcsInfo::default(), cb: [0; 512], sf: [0; 512], spec: [0.0; 1024], tns: None }
    }
}

struct ChannelState {
    overlap: [f32; 1024],
    kbd: bool,
}

struct Tables {
    sf: Vlc,
    spectral: Vec<Vlc>,
    pow43: Vec<f32>,
    /// Rising halves of the windows: [sine, KBD] for 2048 and 256 lines.
    long: [Vec<f32>; 2],
    short: [Vec<f32>; 2],
}

fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term, mut k) = (1.0, 1.0, 1.0);
    while term > sum * 1e-15 {
        term *= (x / 2.0) * (x / 2.0) / (k * k);
        sum += term;
        k += 1.0;
    }
    sum
}

/// Rising half (n values) of a Kaiser-Bessel-derived window of 2n points.
fn kbd(alpha: f64, n: usize) -> Vec<f32> {
    let x = alpha * core::f64::consts::PI / n as f64;
    let a2 = 4.0 * x * x;
    let kernel: Vec<f64> = (0..=n).map(|i| bessel_i0(libm::sqrt((i * (n - i)) as f64 * a2))).collect();
    let total: f64 = kernel.iter().sum();
    let mut sum = 0.0;
    (0..n)
        .map(|i| {
            sum += kernel[i];
            libm::sqrt(sum / total) as f32
        })
        .collect()
}

fn sine(n: usize) -> Vec<f32> {
    (0..n).map(|i| libm::sin(core::f64::consts::PI / (2 * n) as f64 * (i as f64 + 0.5)) as f32).collect()
}

impl Tables {
    fn new() -> Tables {
        Tables {
            sf: Vlc::new(&SCALEFACTOR.0, &SCALEFACTOR.1, 9),
            spectral: SPECTRAL.iter().map(|(c, b)| Vlc::new(c, b, 9)).collect(),
            pow43: (0..8192).map(|i| libm::pow(i as f64, 4.0 / 3.0) as f32).collect(),
            long: [sine(1024), kbd(4.0, 1024)],
            short: [sine(128), kbd(6.0, 128)],
        }
    }
}

/// The decoder proper: raw data blocks in, PCM out.
struct Core {
    cfg: Config,
    t: Tables,
    long: Imdct,
    short: Imdct,
    chans: Vec<ChannelState>,
    /// Noise substitution's generator (the same one FFmpeg uses).
    random: u32,
    ics: [Ics; 2],
    buf: Vec<f32>,
    tmp: Vec<f32>,
}

fn group_info(info: &IcsInfo, sf_index: usize) -> (&'static [u16], usize) {
    if info.short() {
        (SWB_SHORT[sf_index], NUM_SWB_SHORT[sf_index] as usize)
    } else {
        (SWB_LONG[sf_index], NUM_SWB_LONG[sf_index] as usize)
    }
}

impl Core {
    fn new(cfg: Config) -> Core {
        Core {
            cfg,
            t: Tables::new(),
            long: Imdct::new(1024, 1.0 / (1024.0 * 32768.0)),
            short: Imdct::new(128, 1.0 / (128.0 * 32768.0)),
            chans: (0..cfg.channels).map(|_| ChannelState { overlap: [0.0; 1024], kbd: false }).collect(),
            random: 0x1f2e_3d4c,
            ics: [Ics::new(), Ics::new()],
            buf: vec![0.0; 2048],
            tmp: vec![0.0; 256],
        }
    }

    fn reset(&mut self) {
        for c in &mut self.chans {
            c.overlap = [0.0; 1024];
            c.kbd = false;
        }
    }

    fn ics_info(&self, r: &mut MsbReader) -> R<IcsInfo> {
        let mut rd = |n| r.read(n).ok_or("frame ends early");
        rd(1)?;
        let seq = rd(2)? as u8;
        let kbd = rd(1)? == 1;
        let mut info = IcsInfo { seq, kbd, groups: 1, group_len: [1, 0, 0, 0, 0, 0, 0, 0], ..Default::default() };
        if seq == EIGHT_SHORT {
            info.max_sfb = rd(4)? as usize;
            for _ in 0..7 {
                if rd(1)? == 1 {
                    info.group_len[info.groups - 1] += 1;
                } else {
                    info.group_len[info.groups] = 1;
                    info.groups += 1;
                }
            }
        } else {
            info.max_sfb = rd(6)? as usize;
            if rd(1)? == 1 {
                return Err("prediction is not allowed in AAC-LC");
            }
        }
        if info.max_sfb > group_info(&info, self.cfg.sf_index).1 {
            return Err("too many scale factor bands");
        }
        Ok(info)
    }

    /// individual_channel_stream (4.4.2.7) into `self.ics[k]`.
    fn decode_ics(&mut self, r: &mut MsbReader, k: usize, common: Option<IcsInfo>) -> R<()> {
        let global_gain = r.read(8).ok_or("frame ends early")? as i32;
        let info = match common {
            Some(i) => i,
            None => self.ics_info(r)?,
        };
        let (swb, num_swb) = group_info(&info, self.cfg.sf_index);
        let ics = &mut self.ics[k];
        ics.info = info;
        let max_sfb = info.max_sfb;

        // Section data.
        let (bits, esc) = if info.short() { (3, 7) } else { (5, 31) };
        for g in 0..info.groups {
            let mut k = 0;
            while k < max_sfb {
                let cb = r.read(4).ok_or("frame ends early")? as u8;
                if cb == 12 {
                    return Err("reserved codebook");
                }
                let mut end = k;
                loop {
                    let incr = r.read(bits).ok_or("frame ends early")? as usize;
                    end += incr;
                    if incr != esc {
                        break;
                    }
                }
                if end > max_sfb {
                    return Err("section past the last band");
                }
                ics.cb[g * max_sfb + k..g * max_sfb + end].fill(cb);
                k = end;
            }
        }

        // Scale factors.
        let (mut sf, mut is, mut noise) = (global_gain, 0i32, global_gain - 90);
        let mut first_noise = true;
        for i in 0..info.groups * max_sfb {
            ics.sf[i] = match ics.cb[i] {
                ZERO_HCB => 0,
                INTENSITY_HCB | INTENSITY_HCB2 => {
                    is += self.t.sf.read(r)? as i32 - 60;
                    is.clamp(-155, 100)
                }
                NOISE_HCB => {
                    if first_noise {
                        first_noise = false;
                        noise += r.read(9).ok_or("frame ends early")? as i32 - 256;
                    } else {
                        noise += self.t.sf.read(r)? as i32 - 60;
                    }
                    noise.clamp(-100, 155)
                }
                _ => {
                    sf += self.t.sf.read(r)? as i32 - 60;
                    if !(0..=255).contains(&sf) {
                        return Err("scale factor out of range");
                    }
                    sf
                }
            };
        }

        // Pulses (long windows only).
        let mut pulses = [(0usize, 0i32); 4];
        let mut npulse = 0;
        if r.read(1).ok_or("frame ends early")? == 1 {
            if info.short() {
                return Err("pulses in short windows");
            }
            npulse = r.read(2).ok_or("frame ends early")? as usize + 1;
            let start = r.read(6).ok_or("frame ends early")? as usize;
            if start >= num_swb {
                return Err("pulse past the last band");
            }
            let mut at = swb[start] as usize;
            for p in pulses.iter_mut().take(npulse) {
                at += r.read(5).ok_or("frame ends early")? as usize;
                if at >= 1024 {
                    return Err("pulse past the spectrum");
                }
                *p = (at, r.read(4).ok_or("frame ends early")? as i32);
            }
        }

        // Temporal noise shaping.
        ics.tns = None;
        if r.read(1).ok_or("frame ends early")? == 1 {
            let mut t = Tns::default();
            let (nb, lb, ob, max_order) = if info.short() { (1, 4, 3, 7) } else { (2, 6, 5, 12) };
            let windows = if info.short() { 8 } else { 1 };
            for w in 0..windows {
                t.n[w] = r.read(nb).ok_or("frame ends early")? as u8;
                if t.n[w] == 0 {
                    continue;
                }
                let res = r.read(1).ok_or("frame ends early")?;
                for f in 0..t.n[w] as usize {
                    let flt = &mut t.f[w][f];
                    flt.len = r.read(lb).ok_or("frame ends early")? as u8;
                    flt.order = r.read(ob).ok_or("frame ends early")? as u8;
                    if flt.order > max_order {
                        return Err("TNS order too high");
                    }
                    if flt.order == 0 {
                        continue;
                    }
                    flt.down = r.read(1).ok_or("frame ends early")? == 1;
                    let compress = r.read(1).ok_or("frame ends early")?;
                    let len = res + 3 - compress;
                    let half_pi = core::f64::consts::FRAC_PI_2;
                    let iq_pos = ((1 << (res + 2)) as f64 - 0.5) / half_pi;
                    let iq_neg = ((1 << (res + 2)) as f64 + 0.5) / half_pi;
                    let mut refl = [0f64; 12];
                    for c in refl.iter_mut().take(flt.order as usize) {
                        let v = r.read(len).ok_or("frame ends early")?;
                        let v = ((v << (32 - len)) as i32 >> (32 - len)) as f64;
                        *c = libm::sin(v / if v >= 0.0 { iq_pos } else { iq_neg });
                    }
                    // Reflection to direct-form coefficients (step-up).
                    let mut a = [0f64; 13];
                    a[0] = 1.0;
                    for m in 1..=flt.order as usize {
                        let mut b = a;
                        for i in 1..m {
                            b[i] = a[i] + refl[m - 1] * a[m - i];
                        }
                        b[m] = refl[m - 1];
                        a = b;
                    }
                    for (l, v) in flt.lpc.iter_mut().zip(a) {
                        *l = v as f32;
                    }
                }
            }
            ics.tns = Some(t);
        }

        if r.read(1).ok_or("frame ends early")? == 1 {
            return Err("gain control is not allowed in AAC-LC");
        }

        // Spectral data, quantized; window w's lines start at w * 128.
        let mut q = [0i32; 1024];
        let mut win = 0;
        for g in 0..info.groups {
            let len = info.group_len[g] as usize;
            for sfb in 0..max_sfb {
                let cb = ics.cb[g * max_sfb + sfb];
                if cb == ZERO_HCB || cb >= NOISE_HCB {
                    continue;
                }
                let vlc = &self.t.spectral[cb as usize - 1];
                let (lo, hi) = (swb[sfb] as usize, swb[sfb + 1] as usize);
                for w in win..win + len {
                    let line = &mut q[w * 128..];
                    let mut k = lo;
                    while k < hi {
                        let s = vlc.read(r)? as i32;
                        match cb {
                            1..=4 => {
                                let v = [s / 27, s / 9 % 3, s / 3 % 3, s % 3];
                                for (j, &x) in v.iter().enumerate() {
                                    line[k + j] = if cb <= 2 { x - 1 } else { x };
                                }
                                if cb > 2 {
                                    for j in 0..4 {
                                        if line[k + j] != 0 && r.read(1).ok_or("frame ends early")? == 1 {
                                            line[k + j] = -line[k + j];
                                        }
                                    }
                                }
                                k += 4;
                            }
                            _ => {
                                let (m, off) = match cb {
                                    5 | 6 => (9, 4),
                                    7 | 8 => (8, 0),
                                    9 | 10 => (13, 0),
                                    _ => (17, 0),
                                };
                                line[k] = s / m - off;
                                line[k + 1] = s % m - off;
                                if cb > 6 {
                                    for j in 0..2 {
                                        if line[k + j] != 0 && r.read(1).ok_or("frame ends early")? == 1 {
                                            line[k + j] = -line[k + j];
                                        }
                                    }
                                }
                                if cb == ESC_HCB {
                                    for j in 0..2 {
                                        if line[k + j].abs() == 16 {
                                            let mut n = 0;
                                            while r.read(1).ok_or("frame ends early")? == 1 {
                                                n += 1;
                                                if n > 8 {
                                                    return Err("escape too long");
                                                }
                                            }
                                            let v = (1 << (n + 4)) + r.read(n + 4).ok_or("frame ends early")? as i32;
                                            line[k + j] = if line[k + j] < 0 { -v } else { v };
                                        }
                                    }
                                }
                                k += 2;
                            }
                        }
                    }
                }
            }
            win += len;
        }
        for &(at, amp) in &pulses[..npulse] {
            q[at] += if q[at] > 0 { amp } else { -amp };
        }

        // Dequantize, and fill noise bands, in the order FFmpeg does.
        ics.spec = [0.0; 1024];
        let mut win = 0;
        for g in 0..info.groups {
            let len = info.group_len[g] as usize;
            for sfb in 0..max_sfb {
                let i = g * max_sfb + sfb;
                let (lo, hi) = (swb[sfb] as usize, swb[sfb + 1] as usize);
                match ics.cb[i] {
                    ZERO_HCB | INTENSITY_HCB | INTENSITY_HCB2 => {}
                    NOISE_HCB => {
                        let gain = libm::exp2f(0.25 * ics.sf[i] as f32);
                        for w in win..win + len {
                            let band = &mut ics.spec[w * 128 + lo..w * 128 + hi];
                            let mut energy = 0.0f32;
                            for v in band.iter_mut() {
                                self.random = self.random.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                                *v = self.random as i32 as f32;
                                energy += *v * *v;
                            }
                            let scale = gain / libm::sqrtf(energy);
                            band.iter_mut().for_each(|v| *v *= scale);
                        }
                    }
                    _ => {
                        let gain = libm::exp2f(0.25 * (ics.sf[i] - 100) as f32);
                        for w in win..win + len {
                            for j in w * 128 + lo..w * 128 + hi {
                                let v = q[j];
                                let m = self
                                    .t
                                    .pow43
                                    .get(v.unsigned_abs() as usize)
                                    .copied()
                                    .unwrap_or_else(|| libm::powf(v.unsigned_abs() as f32, 4.0 / 3.0));
                                ics.spec[j] = if v < 0 { -m * gain } else { m * gain };
                            }
                        }
                    }
                }
            }
            win += len;
        }
        Ok(())
    }

    /// M/S stereo (only with a common window) and intensity stereo of a
    /// channel pair, in the second channel's band layout.
    fn stereo(&mut self, ms: u8, mask: &[bool; 512]) {
        let [a, b] = &mut self.ics;
        let info = b.info;
        let (swb, _) = group_info(&info, self.cfg.sf_index);
        let mut win = 0;
        for g in 0..info.groups {
            let len = info.group_len[g] as usize;
            for sfb in 0..info.max_sfb {
                let i = g * info.max_sfb + sfb;
                let (lo, hi) = (swb[sfb] as usize, swb[sfb + 1] as usize);
                if ms != 0 && mask[i] && a.cb[i] < NOISE_HCB && b.cb[i] < NOISE_HCB {
                    for w in win..win + len {
                        for j in w * 128 + lo..w * 128 + hi {
                            let (m, s) = (a.spec[j], b.spec[j]);
                            a.spec[j] = m + s;
                            b.spec[j] = m - s;
                        }
                    }
                }
                if b.cb[i] == INTENSITY_HCB || b.cb[i] == INTENSITY_HCB2 {
                    let mut c = if b.cb[i] == INTENSITY_HCB { 1.0 } else { -1.0 };
                    if ms != 0 && mask[i] {
                        c = -c;
                    }
                    let scale = c * libm::exp2f(-0.25 * b.sf[i] as f32);
                    for w in win..win + len {
                        for j in w * 128 + lo..w * 128 + hi {
                            b.spec[j] = a.spec[j] * scale;
                        }
                    }
                }
            }
            win += len;
        }
    }

    /// Temporal noise shaping (4.6.9.3) of `self.ics[k]`.
    fn tns(&mut self, k: usize) {
        let ics = &mut self.ics[k];
        let Some(t) = ics.tns else { return };
        let info = ics.info;
        let (swb, num_swb) = group_info(&info, self.cfg.sf_index);
        let max_bands = if info.short() { TNS_MAX_BANDS_SHORT } else { TNS_MAX_BANDS_LONG }[self.cfg.sf_index] as usize;
        let limit = max_bands.min(info.max_sfb);
        let windows = if info.short() { 8 } else { 1 };
        for w in 0..windows {
            let mut bottom = num_swb;
            for f in &t.f[w][..t.n[w] as usize] {
                let top = bottom;
                bottom = top.saturating_sub(f.len as usize);
                if f.order == 0 {
                    continue;
                }
                let start = swb[bottom.min(limit)] as usize;
                let end = swb[top.min(limit)] as usize;
                if end <= start {
                    continue;
                }
                let x = &mut ics.spec[w * 128 + start..w * 128 + end];
                let order = f.order as usize;
                let n = x.len();
                for m in 0..n {
                    let at = if f.down { n - 1 - m } else { m };
                    let mut y = x[at];
                    for i in 1..=order.min(m) {
                        let prev = if f.down { at + i } else { at - i };
                        y -= f.lpc[i] * x[prev];
                    }
                    x[at] = y;
                }
            }
        }
    }

    /// Inverse MDCT, windowing and overlap-add of `self.ics[k]` into `out`.
    fn synth(&mut self, k: usize, slot: usize, out: &mut [f32]) {
        let info = self.ics[k].info;
        let st = &mut self.chans[slot];
        let (lp, lc) = (&self.t.long[st.kbd as usize], &self.t.long[info.kbd as usize]);
        let (sp, sc) = (&self.t.short[st.kbd as usize], &self.t.short[info.kbd as usize]);
        let z = &mut self.buf;
        if info.seq == EIGHT_SHORT {
            z.fill(0.0);
            for w in 0..8 {
                self.short.run(&self.ics[k].spec[w * 128..w * 128 + 128], &mut self.tmp);
                let rise = if w == 0 { sp } else { sc };
                let at = 448 + w * 128;
                for n in 0..128 {
                    z[at + n] += self.tmp[n] * rise[n];
                    z[at + 128 + n] += self.tmp[128 + n] * sc[127 - n];
                }
            }
        } else {
            self.long.run(&self.ics[k].spec, z);
            if info.seq == LONG_STOP {
                z[..448].fill(0.0);
                for n in 0..128 {
                    z[448 + n] *= sp[n];
                }
            } else {
                for n in 0..1024 {
                    z[n] *= lp[n];
                }
            }
            if info.seq == LONG_START {
                for n in 0..128 {
                    z[1472 + n] *= sc[127 - n];
                }
                z[1600..].fill(0.0);
            } else {
                for n in 0..1024 {
                    z[1024 + n] *= lc[1023 - n];
                }
            }
        }
        for n in 0..1024 {
            out[n] = z[n] + st.overlap[n];
            st.overlap[n] = z[1024 + n];
        }
        st.kbd = info.kbd;
    }

    /// Decodes one raw_data_block into `pcm` (1024 samples per channel, in
    /// the order the channels appear in the stream).
    fn block(&mut self, r: &mut MsbReader, pcm: &mut [Vec<f32>]) -> R<()> {
        let mut slot = 0;
        loop {
            let id = r.read(3).ok_or("frame ends early")?;
            match id {
                ID_SCE | ID_LFE => {
                    r.read(4).ok_or("frame ends early")?;
                    self.decode_ics(r, 0, None)?;
                    if slot < self.chans.len() {
                        self.tns(0);
                        self.synth(0, slot, &mut pcm[slot]);
                    }
                    slot += 1;
                }
                ID_CPE => {
                    r.read(4).ok_or("frame ends early")?;
                    let common = r.read(1).ok_or("frame ends early")? == 1;
                    let mut mask = [false; 512];
                    let mut ms = 0;
                    let info = if common {
                        let info = self.ics_info(r)?;
                        ms = r.read(2).ok_or("frame ends early")? as u8;
                        match ms {
                            1 => {
                                for m in mask.iter_mut().take(info.groups * info.max_sfb) {
                                    *m = r.read(1).ok_or("frame ends early")? == 1;
                                }
                            }
                            2 => mask.fill(true),
                            3 => return Err("reserved M/S mode"),
                            _ => {}
                        }
                        Some(info)
                    } else {
                        None
                    };
                    self.decode_ics(r, 0, info)?;
                    self.decode_ics(r, 1, info)?;
                    self.stereo(ms, &mask);
                    for k in 0..2 {
                        if slot < self.chans.len() {
                            self.tns(k);
                            self.synth(k, slot, &mut pcm[slot]);
                        }
                        slot += 1;
                    }
                }
                ID_CCE => return Err("coupling channels are not supported"),
                ID_DSE => {
                    r.read(4).ok_or("frame ends early")?;
                    let align = r.read(1).ok_or("frame ends early")? == 1;
                    let mut n = r.read(8).ok_or("frame ends early")? as usize;
                    if n == 255 {
                        n += r.read(8).ok_or("frame ends early")? as usize;
                    }
                    if align {
                        r.align();
                    }
                    r.skip(n * 8).ok_or("frame ends early")?;
                }
                ID_PCE => {
                    read_pce(r)?;
                }
                ID_FIL => {
                    let mut n = r.read(4).ok_or("frame ends early")? as usize;
                    if n == 15 {
                        n += r.read(8).ok_or("frame ends early")? as usize - 1;
                    }
                    r.skip(n * 8).ok_or("frame ends early")?;
                }
                ID_END => break,
                _ => unreachable!(),
            }
        }
        r.align();
        Ok(())
    }
}

/// Channel order of a configuration, from the stream's order (C, L, R, ...)
/// to the WAVE order (FL FR FC LFE BL BR SL SR).
fn wave_order(config: u8, channels: usize) -> Vec<usize> {
    match config {
        3 => vec![1, 2, 0],
        4 => vec![1, 2, 0, 3],
        5 => vec![1, 2, 0, 3, 4],
        6 => vec![1, 2, 0, 5, 3, 4],
        7 => vec![1, 2, 0, 7, 5, 6, 3, 4],
        _ => (0..channels).collect(),
    }
}

#[derive(Clone, Copy)]
struct Frame {
    at: usize,
    len: usize,
}

pub struct AacDecoder {
    data: Vec<u8>,
    info: Info,
    frames: Vec<Frame>,
    adts: bool,
    core: Core,
    order: Vec<usize>,
    pcm: Vec<Vec<f32>>,
    next: usize,
    /// Decoded samples to drop at the start (encoder delay).
    start_skip: u64,
    /// The first wanted sample of the decoded stream.
    from: u64,
    stream_pos: u64,
    stream_end: u64,
}

impl AacDecoder {
    pub fn new(data: Vec<u8>) -> Result<AacDecoder, String> {
        let (cfg, frames, adts, start_skip, keep) = if is_mp4(&data) {
            let m = parse_mp4(&data)?;
            let cfg = parse_asc(&m.asc)?;
            let frames: Vec<Frame> = m
                .frames
                .iter()
                .filter(|&&(at, len)| at.checked_add(len).is_some_and(|e| e <= data.len()))
                .map(|&(at, len)| Frame { at, len })
                .collect();
            let scale = |v: u64| if m.timescale > 0 { v * cfg.rate as u64 / m.timescale as u64 } else { v };
            (cfg, frames, false, scale(m.skip), m.keep.map(scale))
        } else {
            let mut p = id3v2_len(&data).unwrap_or(0);
            let mut frames = Vec::new();
            let mut first: Option<Adts> = None;
            while p + 7 <= data.len() {
                let Some(h) = adts_header(&data[p..]) else {
                    // Resynchronise on the next frame of the same stream.
                    p += 1;
                    match data[p..].windows(2).position(|w| w[0] == 0xFF && w[1] & 0xF6 == 0xF0) {
                        Some(o) => p += o,
                        None => break,
                    }
                    continue;
                };
                if first.is_some_and(|f| f.sf_index != h.sf_index) || p + h.len > data.len() {
                    p += 1;
                    continue;
                }
                first.get_or_insert(h);
                frames.push(Frame { at: p, len: h.len });
                p += h.len;
            }
            let h = first.ok_or("no AAC frames found")?;
            let channels = match h.config {
                0 => {
                    // A program config element at the start of the first frame.
                    let f = frames[0];
                    let mut r = MsbReader::new(&data[f.at + if h.crc { 9 } else { 7 }..f.at + f.len]);
                    if r.read(3) == Some(ID_PCE) {
                        read_pce(&mut r)?
                    } else {
                        2
                    }
                }
                7 => 8,
                c => c as usize,
            };
            let cfg = Config {
                aot: h.aot,
                sf_index: h.sf_index,
                rate: RATES[h.sf_index],
                config: h.config,
                channels,
                sbr: false,
            };
            (cfg, frames, true, 0, None)
        };
        if cfg.aot != 2 {
            return Err(String::from(match cfg.aot {
                1 => "AAC Main profile files are not supported",
                4 => "AAC LTP files are not supported",
                _ => "this AAC profile is not supported",
            }));
        }
        if cfg.channels == 0 || cfg.channels > MAX_CHANNELS {
            return Err(format!("unsupported channel count {}", cfg.channels));
        }
        let blocks: u64 = if adts {
            frames.iter().map(|f| adts_header(&data[f.at..]).map_or(1, |h| h.blocks) as u64).sum()
        } else {
            frames.len() as u64
        };
        let decoded = blocks * 1024;
        let start_skip = start_skip.min(decoded);
        let stream_end = keep.map_or(decoded, |k| (start_skip + k).min(decoded));
        let profile = if cfg.sbr { "HE-AAC, played without SBR" } else { "AAC-LC" };
        let container = if adts { "ADTS" } else { "MP4" };
        Ok(AacDecoder {
            info: Info {
                rate: cfg.rate,
                channels: cfg.channels,
                frames: Some(stream_end - start_skip),
                codec: format!("AAC ({profile}, {container})"),
            },
            data,
            frames,
            adts,
            order: wave_order(cfg.config, cfg.channels),
            pcm: vec![vec![0.0; 1024]; cfg.channels],
            core: Core::new(cfg),
            next: 0,
            start_skip,
            from: start_skip,
            stream_pos: 0,
            stream_end,
        })
    }

    /// Decodes frame `self.next` into interleaved samples (silence for a
    /// damaged frame).
    fn decode_frame(&mut self) -> Vec<f32> {
        let f = self.frames[self.next];
        self.next += 1;
        let ch = self.info.channels;
        let body = &self.data[f.at..f.at + f.len];
        let (start, blocks, crc) = if self.adts {
            match adts_header(body) {
                Some(h) => (if h.crc { 7 + 2 * h.blocks } else { 7 }, h.blocks, h.crc && h.blocks > 1),
                None => (body.len(), 1, false),
            }
        } else {
            (0, 1, false)
        };
        let mut out = vec![0.0f32; blocks * 1024 * ch];
        let mut r = MsbReader::new(body.get(start..).unwrap_or(&[]));
        for b in 0..blocks {
            for c in &mut self.pcm {
                c.fill(0.0);
            }
            if self.core.block(&mut r, &mut self.pcm).is_err() {
                self.core.reset();
                for c in &mut self.pcm {
                    c.fill(0.0);
                }
            }
            if crc {
                r.skip(16);
            }
            let base = b * 1024 * ch;
            for (o, &src) in self.order.iter().enumerate() {
                for (i, s) in self.pcm[src].iter().enumerate() {
                    out[base + i * ch + o] = *s;
                }
            }
        }
        out
    }

    fn blocks_of(&self, i: usize) -> u64 {
        if self.adts {
            adts_header(&self.data[self.frames[i].at..]).map_or(1, |h| h.blocks) as u64
        } else {
            1
        }
    }
}

impl Decoder for AacDecoder {
    fn info(&self) -> &Info {
        &self.info
    }

    fn next_block(&mut self) -> Result<Option<Vec<f32>>, String> {
        let ch = self.info.channels;
        loop {
            if self.next >= self.frames.len() || self.stream_pos >= self.stream_end {
                return Ok(None);
            }
            let block = self.decode_frame();
            let n = (block.len() / ch) as u64;
            let (a, b) = (self.stream_pos, self.stream_pos + n);
            self.stream_pos = b;
            let lo = self.from.max(a);
            let hi = self.stream_end.min(b);
            if lo >= hi {
                continue;
            }
            return Ok(Some(block[(lo - a) as usize * ch..(hi - a) as usize * ch].to_vec()));
        }
    }

    fn seek(&mut self, frame: u64) -> Result<u64, String> {
        let frame = frame.min(self.info.frames.unwrap_or(0));
        let target = frame + self.start_skip;
        // Find the frame holding the target, then decode the one before it
        // to rebuild the overlap.
        let (mut k, mut pos) = (0, 0u64);
        while k < self.frames.len() {
            let n = self.blocks_of(k) * 1024;
            if pos + n > target {
                break;
            }
            pos += n;
            k += 1;
        }
        self.core.reset();
        self.next = k.saturating_sub(1);
        self.stream_pos = pos - if k > 0 { self.blocks_of(k - 1) * 1024 } else { 0 };
        if k > 0 {
            self.decode_frame();
            self.stream_pos = pos;
        }
        self.from = target;
        Ok(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vlc_decodes_every_code() {
        for (i, (codes, bits)) in SPECTRAL.iter().enumerate() {
            let vlc = Vlc::new(codes, bits, 9);
            for (s, (&c, &l)) in codes.iter().zip(bits.iter()).enumerate() {
                let v = (c as u32) << (32 - l);
                let bytes = v.to_be_bytes();
                let mut r = MsbReader::new(&bytes);
                assert_eq!(vlc.read(&mut r), Ok(s as u32), "book {} symbol {s}", i + 1);
                assert_eq!(r.bit_pos(), l as usize);
            }
        }
        let vlc = Vlc::new(&SCALEFACTOR.0, &SCALEFACTOR.1, 9);
        for (s, (&c, &l)) in SCALEFACTOR.0.iter().zip(SCALEFACTOR.1.iter()).enumerate() {
            let v = c << (32 - l);
            let bytes = v.to_be_bytes();
            let mut r = MsbReader::new(&bytes);
            assert_eq!(vlc.read(&mut r), Ok(s as u32), "scale factor {s}");
        }
    }

    #[test]
    fn kbd_is_a_power_complementary_window() {
        for (w, n) in [(kbd(4.0, 1024), 1024), (kbd(6.0, 128), 128), (sine(1024), 1024)] {
            for i in 0..n {
                let s = w[i] * w[i] + w[n - 1 - i] * w[n - 1 - i];
                assert!((s - 1.0).abs() < 1e-5, "{i}: {s}");
            }
        }
    }
}
