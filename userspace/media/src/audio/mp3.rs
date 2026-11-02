//! MPEG-1, MPEG-2 and MPEG-2.5 Audio Layer III (ISO/IEC 11172-3 and
//! 13818-3): frame headers, the bit reservoir, scale factors, Huffman
//! decoding, requantisation, mid/side and intensity stereo, alias
//! reduction, the hybrid filter bank (IMDCT) and polyphase synthesis.
//! Gapless playback follows the LAME tag (encoder delay and padding), and
//! seeking primes the decoder with the frames the reservoir needs, so a
//! seek gives exactly the samples a straight decode would.

use super::mp3_tables::{BIG, BIG16, BIG24, LINBITS, QUAD_A, SYNTH_D};
use super::{id3v2_len, Decoder, Info};
use crate::bits::MsbReader;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::f32::consts::FRAC_1_SQRT_2;
use core::f64::consts::PI;

/// Delay of the decoder itself (filter banks), in samples.
const DECODER_DELAY: u64 = 529;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Version {
    V1,
    V2,
    V25,
}

#[derive(Clone, Copy, Debug)]
struct Header {
    version: Version,
    crc: bool,
    rate: u32,
    /// Index into the scale factor band tables (0..9).
    sr_index: usize,
    /// 0 stereo, 1 joint stereo, 2 dual channel, 3 mono.
    mode: u8,
    mode_ext: u8,
    len: usize,
}

impl Header {
    fn channels(&self) -> usize {
        if self.mode == 3 {
            1
        } else {
            2
        }
    }

    fn granules(&self) -> usize {
        if self.version == Version::V1 {
            2
        } else {
            1
        }
    }

    fn samples(&self) -> usize {
        self.granules() * 576
    }

    fn side_len(&self) -> usize {
        match (self.version, self.channels()) {
            (Version::V1, 1) => 17,
            (Version::V1, _) => 32,
            (_, 1) => 9,
            _ => 17,
        }
    }
}

const BITRATE_V1: [u32; 15] = [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320];
const BITRATE_V2: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];

fn parse_header(b: &[u8]) -> Option<Header> {
    if b.len() < 4 || b[0] != 0xFF || b[1] & 0xE0 != 0xE0 {
        return None;
    }
    let version = match (b[1] >> 3) & 3 {
        0 => Version::V25,
        2 => Version::V2,
        3 => Version::V1,
        _ => return None,
    };
    // Layer III only.
    if (b[1] >> 1) & 3 != 1 {
        return None;
    }
    let crc = b[1] & 1 == 0;
    let br_index = (b[2] >> 4) as usize;
    let rate_index = ((b[2] >> 2) & 3) as usize;
    if br_index == 0 || br_index == 15 || rate_index == 3 {
        return None;
    }
    let padding = ((b[2] >> 1) & 1) as usize;
    let (bitrate, rate, sr_index) = match version {
        Version::V1 => (BITRATE_V1[br_index], [44100, 48000, 32000][rate_index], rate_index),
        Version::V2 => (BITRATE_V2[br_index], [22050, 24000, 16000][rate_index], 3 + rate_index),
        Version::V25 => (BITRATE_V2[br_index], [11025, 12000, 8000][rate_index], 6 + rate_index),
    };
    let factor = if version == Version::V1 { 144_000 } else { 72_000 };
    let len = (factor * bitrate / rate) as usize + padding;
    Some(Header { version, crc, rate, sr_index, mode: b[3] >> 6, mode_ext: (b[3] >> 4) & 3, len })
}

/// The first frame header at or after `from` that is followed by another
/// frame of the same kind (or the end of the data).
pub fn find_frame(data: &[u8], from: usize) -> Option<usize> {
    let mut p = from;
    while p + 4 <= data.len() {
        if data[p] == 0xFF {
            if let Some(h) = parse_header(&data[p..]) {
                let next = p + h.len;
                let ok = match data.get(next..next + 4) {
                    Some(n) => parse_header(n).is_some_and(|n| n.version == h.version && n.rate == h.rate),
                    None => next <= data.len() + 1,
                };
                if ok {
                    return Some(p);
                }
            }
        }
        p += 1;
    }
    None
}

#[derive(Clone, Copy, Default)]
struct GrChannel {
    part2_3_length: usize,
    big_values: usize,
    global_gain: i32,
    scalefac_compress: u32,
    block_type: u8,
    mixed: bool,
    table_select: [u8; 3],
    subblock_gain: [i32; 3],
    region1_start: usize,
    region2_start: usize,
    preflag: bool,
    scalefac_scale: bool,
    count1table_b: bool,
}

struct SideInfo {
    main_data_begin: usize,
    scfsi: [[bool; 4]; 2],
    gr: [[GrChannel; 2]; 2],
}

/// Scale factor band widths (ISO/IEC 11172-3 Table B.8, 13818-3 Table B.2).
const LONG_WIDTHS: [[usize; 22]; 9] = [
    [4, 4, 4, 4, 4, 4, 6, 6, 8, 8, 10, 12, 16, 20, 24, 28, 34, 42, 50, 54, 76, 158],
    [4, 4, 4, 4, 4, 4, 6, 6, 6, 8, 10, 12, 16, 18, 22, 28, 34, 40, 46, 54, 54, 192],
    [4, 4, 4, 4, 4, 4, 6, 6, 8, 10, 12, 16, 20, 24, 30, 38, 46, 56, 68, 84, 102, 26],
    [6, 6, 6, 6, 6, 6, 8, 10, 12, 14, 16, 20, 24, 28, 32, 38, 46, 52, 60, 68, 58, 54],
    [6, 6, 6, 6, 6, 6, 8, 10, 12, 14, 16, 18, 22, 26, 32, 38, 46, 54, 62, 70, 76, 36],
    [6, 6, 6, 6, 6, 6, 8, 10, 12, 14, 16, 20, 24, 28, 32, 38, 46, 52, 60, 68, 58, 54],
    [6, 6, 6, 6, 6, 6, 8, 10, 12, 14, 16, 20, 24, 28, 32, 38, 46, 52, 60, 68, 58, 54],
    [6, 6, 6, 6, 6, 6, 8, 10, 12, 14, 16, 20, 24, 28, 32, 38, 46, 52, 60, 68, 58, 54],
    [12, 12, 12, 12, 12, 12, 16, 20, 24, 28, 32, 40, 48, 56, 64, 76, 90, 2, 2, 2, 2, 2],
];
const SHORT_WIDTHS: [[usize; 13]; 9] = [
    [4, 4, 4, 4, 6, 8, 10, 12, 14, 18, 22, 30, 56],
    [4, 4, 4, 4, 6, 6, 10, 12, 14, 16, 20, 26, 66],
    [4, 4, 4, 4, 6, 8, 12, 16, 20, 26, 34, 42, 12],
    [4, 4, 4, 6, 6, 8, 10, 14, 18, 26, 32, 42, 18],
    [4, 4, 4, 6, 8, 10, 12, 14, 18, 24, 32, 44, 12],
    [4, 4, 4, 6, 8, 10, 12, 14, 18, 24, 30, 40, 18],
    [4, 4, 4, 6, 8, 10, 12, 14, 18, 24, 30, 40, 18],
    [4, 4, 4, 6, 8, 10, 12, 14, 18, 24, 30, 40, 18],
    [8, 8, 8, 12, 16, 20, 24, 28, 36, 2, 2, 2, 26],
];
const PRETAB: [i32; 22] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 3, 3, 3, 2, 0];
const SLEN: [(u32, u32); 16] = [
    (0, 0),
    (0, 1),
    (0, 2),
    (0, 3),
    (3, 0),
    (1, 1),
    (1, 2),
    (1, 3),
    (2, 1),
    (2, 2),
    (2, 3),
    (3, 1),
    (3, 2),
    (3, 3),
    (4, 2),
    (4, 3),
];
/// Scale factors per slen group for MPEG-2 (13818-3 Table B.3 of 2.4.3.2):
/// [table][long, short, mixed][group].
const NR_OF_SFB: [[[usize; 4]; 3]; 6] = [
    [[6, 5, 5, 5], [9, 9, 9, 9], [6, 9, 9, 9]],
    [[6, 5, 7, 3], [9, 9, 12, 6], [6, 9, 12, 6]],
    [[11, 10, 0, 0], [18, 18, 0, 0], [15, 18, 0, 0]],
    [[7, 7, 7, 0], [12, 12, 12, 0], [6, 15, 12, 0]],
    [[6, 6, 6, 3], [12, 9, 9, 6], [6, 12, 9, 6]],
    [[8, 8, 5, 0], [15, 12, 9, 0], [6, 18, 9, 0]],
];

/// Scale factors of one granule and channel. `*_max` hold the largest
/// value the field could store (an "illegal" intensity position).
#[derive(Clone, Copy, Default)]
struct Scalefactors {
    long: [i32; 22],
    long_max: [i32; 22],
    short: [[i32; 3]; 13],
    short_max: [[i32; 3]; 13],
}

/// A Huffman code as a binary tree: node `[zero, one]`, leaves are
/// `-(value + 1)`.
struct Tree(Vec<[i32; 2]>);

impl Tree {
    fn new(codes: &[u32], lens: &[u8], value: impl Fn(usize) -> u32) -> Tree {
        let mut nodes = vec![[0i32; 2]];
        for (i, (&c, &l)) in codes.iter().zip(lens).enumerate() {
            let mut n = 0usize;
            for b in (0..l).rev() {
                let bit = ((c >> b) & 1) as usize;
                if b == 0 {
                    nodes[n][bit] = -(value(i) as i32) - 1;
                } else {
                    if nodes[n][bit] <= 0 {
                        nodes.push([0, 0]);
                        nodes[n][bit] = (nodes.len() - 1) as i32;
                    }
                    n = nodes[n][bit] as usize;
                }
            }
        }
        Tree(nodes)
    }

    fn decode(&self, r: &mut MsbReader) -> Option<u32> {
        let mut n = 0usize;
        for _ in 0..32 {
            let bit = r.read_lenient(1) as usize;
            let next = self.0[n][bit];
            if next < 0 {
                return Some((-next - 1) as u32);
            }
            if next == 0 {
                return None;
            }
            n = next as usize;
        }
        None
    }
}

struct Tables {
    big: Vec<Option<Tree>>,
    quad_a: Tree,
    pow43: Vec<f32>,
    /// 36-point IMDCT cosines, `[i * 18 + k]`.
    cos36: Vec<f32>,
    /// 12-point IMDCT cosines, `[i * 6 + k]`.
    cos12: Vec<f32>,
    /// Windows for block types 0, 1, 3 (36 values) and 2 (12 values).
    win: [[f32; 36]; 4],
    /// Synthesis matrix, `[i * 32 + k]`.
    synth: Vec<f32>,
    cs: [f32; 8],
    ca: [f32; 8],
}

impl Tables {
    fn new() -> Tables {
        let pair = |wrap: usize| move |i: usize| (((i / wrap) as u32) << 4) | (i % wrap) as u32;
        let mut big: Vec<Option<Tree>> =
            BIG.iter().map(|&(c, l, w)| (w > 0).then(|| Tree::new(c, l, pair(w)))).collect();
        big.push(Some(Tree::new(BIG16.0, BIG16.1, pair(16))));
        big.push(Some(Tree::new(BIG24.0, BIG24.1, pair(16))));
        let quad_a = Tree::new(&QUAD_A.0, &QUAD_A.1, |i| i as u32);
        let pow43 = (0..8207).map(|i| libm::pow(i as f64, 4.0 / 3.0) as f32).collect();
        let mut cos36 = vec![0.0; 36 * 18];
        for i in 0..36 {
            for k in 0..18 {
                cos36[i * 18 + k] = libm::cos(PI / 72.0 * (2 * i + 1 + 18) as f64 * (2 * k + 1) as f64) as f32;
            }
        }
        let mut cos12 = vec![0.0; 12 * 6];
        for i in 0..12 {
            for k in 0..6 {
                cos12[i * 6 + k] = libm::cos(PI / 24.0 * (2 * i + 1 + 6) as f64 * (2 * k + 1) as f64) as f32;
            }
        }
        let s36 = |i: usize| libm::sin(PI / 36.0 * (i as f64 + 0.5)) as f32;
        let s12 = |i: usize| libm::sin(PI / 12.0 * (i as f64 + 0.5)) as f32;
        let mut win = [[0.0f32; 36]; 4];
        for i in 0..36 {
            win[0][i] = s36(i);
            win[1][i] = match i {
                0..=17 => s36(i),
                18..=23 => 1.0,
                24..=29 => s12(i - 18),
                _ => 0.0,
            };
            win[3][i] = match i {
                0..=5 => 0.0,
                6..=11 => s12(i - 6),
                12..=17 => 1.0,
                _ => s36(i),
            };
        }
        for i in 0..12 {
            win[2][i] = s12(i);
        }
        let mut synth = vec![0.0; 64 * 32];
        for i in 0..64 {
            for k in 0..32 {
                synth[i * 32 + k] = libm::cos((16 + i) as f64 * (2 * k + 1) as f64 * PI / 64.0) as f32;
            }
        }
        const CI: [f64; 8] = [-0.6, -0.535, -0.33, -0.185, -0.095, -0.041, -0.0142, -0.0037];
        let mut cs = [0.0; 8];
        let mut ca = [0.0; 8];
        for i in 0..8 {
            let d = libm::sqrt(1.0 + CI[i] * CI[i]);
            cs[i] = (1.0 / d) as f32;
            ca[i] = (CI[i] / d) as f32;
        }
        Tables { big, quad_a, pow43, cos36, cos12, win, synth, cs, ca }
    }

    fn pow43(&self, v: u32) -> f32 {
        match self.pow43.get(v as usize) {
            Some(&x) => x,
            None => libm::powf(v as f32, 4.0 / 3.0),
        }
    }
}

/// Per-channel state carried between granules.
struct ChannelState {
    overlap: [[f32; 18]; 32],
    v: [f32; 1024],
    v_off: usize,
}

impl ChannelState {
    fn new() -> ChannelState {
        ChannelState { overlap: [[0.0; 18]; 32], v: [0.0; 1024], v_off: 0 }
    }
}

pub struct Mp3Decoder {
    data: Vec<u8>,
    info: Info,
    /// Byte offsets of the audio frames.
    frames: Vec<usize>,
    /// Samples per frame.
    spf: usize,
    next: usize,
    reservoir: Vec<u8>,
    state: [ChannelState; 2],
    tables: Tables,
    /// Samples to drop at the start of the stream (gapless).
    start_skip: u64,
    /// The first sample of the decoded stream still wanted: `start_skip`,
    /// or the target of the last seek.
    from: u64,
    /// Position in the decoded stream (before gapless trimming).
    stream_pos: u64,
    /// End of the wanted samples in the decoded stream.
    stream_end: u64,
    /// Scale factors of granule 0, reused through `scfsi`.
    prev_sf: [Scalefactors; 2],
}

impl Mp3Decoder {
    pub fn new(data: Vec<u8>) -> Result<Mp3Decoder, String> {
        let mut p = id3v2_len(&data).unwrap_or(0);
        let mut frames = Vec::new();
        let mut first: Option<Header> = None;
        let (mut delay, mut padding, mut tagged) = (0u64, 0u64, false);
        while let Some(at) = find_frame(&data, p) {
            let h = parse_header(&data[at..]).unwrap();
            if let Some(f) = first {
                if h.version != f.version || h.rate != f.rate {
                    p = at + 1;
                    continue;
                }
            } else {
                first = Some(h);
                // A Xing/Info/VBRI frame describes the stream and has no audio.
                let side = at + 4 + if h.crc { 2 } else { 0 } + h.side_len();
                let end = (at + h.len).min(data.len());
                let body = &data[side.min(end)..end];
                if body.starts_with(b"Xing") || body.starts_with(b"Info") || data.get(at + 36..at + 40) == Some(b"VBRI")
                {
                    for tag in [&b"LAME"[..], b"Lavc", b"Lavf"] {
                        if let Some(q) = body.windows(4).position(|w| w == tag) {
                            if let Some(t) = body.get(q + 21..q + 24) {
                                delay = ((t[0] as u64) << 4) | (t[1] as u64 >> 4);
                                padding = (((t[1] & 0x0F) as u64) << 8) | t[2] as u64;
                                tagged = true;
                            }
                            break;
                        }
                    }
                    p = at + h.len;
                    continue;
                }
            }
            frames.push(at);
            p = at + h.len;
        }
        let h = first.ok_or("no MPEG audio frames found")?;
        let spf = h.samples();
        let decoded = (frames.len() * spf) as u64;
        let (start_skip, stream_end) = if tagged {
            let start = delay + DECODER_DELAY;
            (start, (decoded + DECODER_DELAY).saturating_sub(padding).min(decoded).max(start))
        } else {
            (0, decoded)
        };
        let version = match h.version {
            Version::V1 => "MPEG-1",
            Version::V2 => "MPEG-2",
            Version::V25 => "MPEG-2.5",
        };
        Ok(Mp3Decoder {
            info: Info {
                rate: h.rate,
                channels: h.channels(),
                frames: Some(stream_end - start_skip),
                codec: format!("MP3 ({version} Layer III)"),
            },
            data,
            frames,
            spf,
            next: 0,
            reservoir: Vec::new(),
            state: [ChannelState::new(), ChannelState::new()],
            tables: Tables::new(),
            start_skip,
            from: start_skip,
            stream_pos: 0,
            stream_end,
            prev_sf: [Scalefactors::default(); 2],
        })
    }

    /// Decodes frame `self.next`: `spf` interleaved frames (silence when the
    /// reservoir does not reach back far enough, or the frame is damaged).
    fn decode_frame(&mut self) -> Vec<f32> {
        let at = self.frames[self.next];
        self.next += 1;
        let h = parse_header(&self.data[at..]).unwrap();
        let ch = h.channels();
        let mut out = vec![0.0f32; self.spf * ch];
        let side_at = at + 4 + if h.crc { 2 } else { 0 };
        let main_at = side_at + h.side_len();
        let end = (at + h.len).min(self.data.len());
        if main_at > end {
            return out;
        }
        let side = read_side_info(&self.data[side_at..main_at], &h);
        let frame_main = &self.data[main_at..end];
        let ok = side.main_data_begin <= self.reservoir.len();
        let mut main = Vec::new();
        if ok {
            main.extend_from_slice(&self.reservoir[self.reservoir.len() - side.main_data_begin..]);
            main.extend_from_slice(frame_main);
        }
        self.reservoir.extend_from_slice(frame_main);
        if self.reservoir.len() > 4096 {
            let cut = self.reservoir.len() - 4096;
            self.reservoir.drain(..cut);
        }
        if !ok {
            // The data this frame needs is gone (stream start or a seek):
            // keep the filter bank state moving with silence.
            for gr in 0..h.granules() {
                for c in 0..ch {
                    let mut xr = [0.0f32; 576];
                    let pcm = self.hybrid(c, &mut xr, &GrChannel::default());
                    for (i, s) in pcm.iter().enumerate() {
                        out[(gr * 576 + i) * ch + c] = *s;
                    }
                }
            }
            return out;
        }
        let mut r = MsbReader::new(&main);
        for gr in 0..h.granules() {
            let mut xr = [[0.0f32; 576]; 2];
            let mut nonzero = [0usize; 2];
            let mut sfs = [Scalefactors::default(); 2];
            for c in 0..ch {
                let g = side.gr[gr][c];
                let start = r.bit_pos();
                let sf = if h.version == Version::V1 {
                    read_scalefactors_v1(&mut r, &g, &side.scfsi[c], gr, &self.prev_sf[c])
                } else {
                    read_scalefactors_v2(&mut r, &g, c == 1 && h.mode_ext & 1 != 0 && h.mode == 1)
                };
                if gr == 0 {
                    self.prev_sf[c] = sf;
                }
                sfs[c] = sf;
                let mut is = [0i32; 576];
                let end = start + g.part2_3_length;
                nonzero[c] = read_huffman(&mut r, &g, end, &mut is, &self.tables);
                r.seek_bits(end);
                requantize(&is, &g, &sf, h.sr_index, nonzero[c], &mut xr[c], &self.tables);
            }
            if h.mode == 1 && ch == 2 {
                stereo(&mut xr, &side.gr[gr], &sfs[1], &h, nonzero[1]);
            }
            for c in 0..ch {
                let g = side.gr[gr][c];
                if g.block_type == 2 {
                    reorder(&mut xr[c], h.sr_index, g.mixed);
                }
                alias_reduce(&mut xr[c], &g, &self.tables);
                let pcm = self.hybrid(c, &mut xr[c], &g);
                for (i, s) in pcm.iter().enumerate() {
                    out[(gr * 576 + i) * ch + c] = *s;
                }
            }
        }
        out
    }

    /// IMDCT, windowing, overlap-add, frequency inversion and polyphase
    /// synthesis of one granule: 576 output samples.
    fn hybrid(&mut self, c: usize, xr: &mut [f32; 576], g: &GrChannel) -> [f32; 576] {
        let t = &self.tables;
        let st = &mut self.state[c];
        let mut time = [[0.0f32; 32]; 18];
        for sb in 0..32 {
            let x = &xr[sb * 18..sb * 18 + 18];
            let mut raw = [0.0f32; 36];
            let bt = if g.mixed && sb < 2 { 0 } else { g.block_type };
            if bt == 2 {
                for w in 0..3 {
                    for i in 0..12 {
                        let mut s = 0.0;
                        for k in 0..6 {
                            s += x[3 * k + w] * t.cos12[i * 6 + k];
                        }
                        raw[6 + 6 * w + i] += s * t.win[2][i];
                    }
                }
            } else {
                let win = &t.win[match bt {
                    1 => 1,
                    3 => 3,
                    _ => 0,
                }];
                for i in 0..36 {
                    let mut s = 0.0;
                    for k in 0..18 {
                        s += x[k] * t.cos36[i * 18 + k];
                    }
                    raw[i] = s * win[i];
                }
            }
            for i in 0..18 {
                let mut v = raw[i] + st.overlap[sb][i];
                st.overlap[sb][i] = raw[18 + i];
                if sb & 1 == 1 && i & 1 == 1 {
                    v = -v;
                }
                time[i][sb] = v;
            }
        }
        let mut out = [0.0f32; 576];
        for (slot, s) in time.iter().enumerate() {
            st.v_off = (st.v_off + 1024 - 64) & 1023;
            for i in 0..64 {
                let row = &t.synth[i * 32..i * 32 + 32];
                let mut sum = 0.0;
                for k in 0..32 {
                    sum += row[k] * s[k];
                }
                st.v[(st.v_off + i) & 1023] = sum;
            }
            for j in 0..32 {
                let mut sum = 0.0;
                for i in 0..8 {
                    sum += st.v[(st.v_off + i * 128 + j) & 1023] * SYNTH_D[i * 64 + j];
                    sum += st.v[(st.v_off + i * 128 + 96 + j) & 1023] * SYNTH_D[i * 64 + 32 + j];
                }
                out[slot * 32 + j] = sum;
            }
        }
        out
    }

    fn reset_state(&mut self) {
        self.state = [ChannelState::new(), ChannelState::new()];
        self.reservoir.clear();
        self.prev_sf = [Scalefactors::default(); 2];
    }
}

fn read_side_info(d: &[u8], h: &Header) -> SideInfo {
    let mut r = MsbReader::new(d);
    let ch = h.channels();
    let v1 = h.version == Version::V1;
    let mut si = SideInfo { main_data_begin: 0, scfsi: [[false; 4]; 2], gr: [[GrChannel::default(); 2]; 2] };
    si.main_data_begin = r.read_lenient(if v1 { 9 } else { 8 }) as usize;
    r.read_lenient(match (v1, ch) {
        (true, 1) => 5,
        (true, _) => 3,
        (false, 1) => 1,
        (false, _) => 2,
    });
    if v1 {
        for c in 0..ch {
            for b in 0..4 {
                si.scfsi[c][b] = r.read_lenient(1) != 0;
            }
        }
    }
    for gr in 0..h.granules() {
        for c in 0..ch {
            let g = &mut si.gr[gr][c];
            g.part2_3_length = r.read_lenient(12) as usize;
            g.big_values = (r.read_lenient(9) as usize).min(288);
            g.global_gain = r.read_lenient(8) as i32;
            g.scalefac_compress = r.read_lenient(if v1 { 4 } else { 9 });
            if r.read_lenient(1) != 0 {
                g.block_type = r.read_lenient(2) as u8;
                g.mixed = r.read_lenient(1) != 0;
                for i in 0..2 {
                    g.table_select[i] = r.read_lenient(5) as u8;
                }
                for i in 0..3 {
                    g.subblock_gain[i] = r.read_lenient(3) as i32;
                }
                // Block type 0 with window switching is invalid; treat as long.
                g.region1_start = if h.version == Version::V25 {
                    let n = if g.block_type == 2 && !g.mixed { 6 } else { 8 };
                    LONG_WIDTHS[h.sr_index][..n].iter().sum()
                } else if v1 || g.block_type == 2 {
                    36
                } else {
                    54
                };
                g.region2_start = 576;
            } else {
                for i in 0..3 {
                    g.table_select[i] = r.read_lenient(5) as u8;
                }
                let r0 = r.read_lenient(4) as usize + 1;
                let r1 = r.read_lenient(3) as usize + 1;
                g.region1_start = LONG_WIDTHS[h.sr_index][..r0.min(22)].iter().sum();
                g.region2_start = LONG_WIDTHS[h.sr_index][..(r0 + r1).min(22)].iter().sum();
            }
            if v1 {
                g.preflag = r.read_lenient(1) != 0;
            }
            g.scalefac_scale = r.read_lenient(1) != 0;
            g.count1table_b = r.read_lenient(1) != 0;
        }
    }
    si
}

fn read_scalefactors_v1(
    r: &mut MsbReader,
    g: &GrChannel,
    scfsi: &[bool; 4],
    gr: usize,
    prev: &Scalefactors,
) -> Scalefactors {
    let (s1, s2) = SLEN[g.scalefac_compress as usize & 15];
    let mut sf = Scalefactors::default();
    let max = |s: u32| (1i32 << s) - 1;
    if g.block_type == 2 {
        let mut first_short = 0;
        if g.mixed {
            for b in 0..8 {
                sf.long[b] = r.read_lenient(s1) as i32;
            }
            first_short = 3;
        }
        for b in first_short..12 {
            let s = if b < 6 { s1 } else { s2 };
            for w in 0..3 {
                sf.short[b][w] = r.read_lenient(s) as i32;
                sf.short_max[b][w] = max(s);
            }
        }
    } else {
        const GROUPS: [(usize, usize); 4] = [(0, 6), (6, 11), (11, 16), (16, 21)];
        for (i, &(a, b)) in GROUPS.iter().enumerate() {
            let s = if i < 2 { s1 } else { s2 };
            for band in a..b {
                if gr == 1 && scfsi[i] {
                    sf.long[band] = prev.long[band];
                } else {
                    sf.long[band] = r.read_lenient(s) as i32;
                }
                sf.long_max[band] = max(s);
            }
        }
    }
    sf
}

fn read_scalefactors_v2(r: &mut MsbReader, g: &GrChannel, intensity_right: bool) -> Scalefactors {
    let sfc = g.scalefac_compress;
    let (slen, tab) = if !intensity_right {
        if sfc < 400 {
            ([(sfc >> 4) / 5, (sfc >> 4) % 5, (sfc & 15) >> 2, sfc & 3], 0)
        } else if sfc < 500 {
            let s = sfc - 400;
            ([(s >> 2) / 5, (s >> 2) % 5, s & 3, 0], 1)
        } else {
            let s = sfc - 500;
            ([s / 3, s % 3, 0, 0], 2)
        }
    } else {
        let s = sfc >> 1;
        if s < 180 {
            ([s / 36, (s % 36) / 6, (s % 36) % 6, 0], 3)
        } else if s < 244 {
            let s = s - 180;
            ([(s & 63) >> 4, (s & 15) >> 2, s & 3, 0], 4)
        } else {
            let s = s - 244;
            ([s / 3, s % 3, 0, 0], 5)
        }
    };
    let blk = match (g.block_type, g.mixed) {
        (2, false) => 1,
        (2, true) => 2,
        _ => 0,
    };
    let nr = NR_OF_SFB[tab][blk];
    let mut vals = Vec::with_capacity(39);
    for i in 0..4 {
        for _ in 0..nr[i] {
            vals.push((r.read_lenient(slen[i]) as i32, (1i32 << slen[i]) - 1));
        }
    }
    let mut sf = Scalefactors::default();
    let mut it = vals.into_iter();
    match blk {
        0 => {
            for b in 0..21 {
                let (v, m) = it.next().unwrap_or((0, 0));
                sf.long[b] = v;
                sf.long_max[b] = m;
            }
        }
        _ => {
            let mut first_short = 0;
            if blk == 2 {
                for b in 0..6 {
                    let (v, m) = it.next().unwrap_or((0, 0));
                    sf.long[b] = v;
                    sf.long_max[b] = m;
                }
                first_short = 3;
            }
            for b in first_short..12 {
                for w in 0..3 {
                    let (v, m) = it.next().unwrap_or((0, 0));
                    sf.short[b][w] = v;
                    sf.short_max[b][w] = m;
                }
            }
        }
    }
    sf
}

/// Huffman-coded spectral values into `is`; returns the number of lines up
/// to the last one that may be non-zero.
fn read_huffman(r: &mut MsbReader, g: &GrChannel, end: usize, is: &mut [i32; 576], t: &Tables) -> usize {
    let big = (g.big_values * 2).min(576);
    let mut i = 0;
    while i < big {
        let region = if i < g.region1_start {
            0
        } else if i < g.region2_start {
            1
        } else {
            2
        };
        let sel = g.table_select[region] as usize;
        let tree = match sel {
            0..=15 => t.big[sel].as_ref(),
            16..=23 => t.big[16].as_ref(),
            _ => t.big[17].as_ref(),
        };
        let Some(tree) = tree else {
            // Table 0 (or the unused 4 and 14): all zero.
            is[i] = 0;
            is[i + 1] = 0;
            i += 2;
            continue;
        };
        let v = tree.decode(r).unwrap_or(0);
        let lin = LINBITS[sel];
        let mut val = |x: u32| -> i32 {
            let mut x = x as i32;
            if x == 15 && lin > 0 {
                x += r.read_lenient(lin) as i32;
            }
            if x != 0 && r.read_lenient(1) != 0 {
                -x
            } else {
                x
            }
        };
        is[i] = val(v >> 4);
        is[i + 1] = val(v & 15);
        i += 2;
    }
    // count1 region: quadruples of -1, 0, 1.
    while i + 4 <= 576 && r.bit_pos() < end {
        let q = if g.count1table_b { 15 - r.read_lenient(4) } else { t.quad_a.decode(r).unwrap_or(0) };
        let mut vals = [0i32; 4];
        for (k, v) in vals.iter_mut().enumerate() {
            if (q >> (3 - k)) & 1 != 0 {
                *v = if r.read_lenient(1) != 0 { -1 } else { 1 };
            }
        }
        if r.bit_pos() > end {
            // The last quadruple ran past the part: it is not data.
            break;
        }
        is[i..i + 4].copy_from_slice(&vals);
        i += 4;
    }
    for x in is[i..].iter_mut() {
        *x = 0;
    }
    i
}

fn requantize(
    is: &[i32; 576],
    g: &GrChannel,
    sf: &Scalefactors,
    sr: usize,
    nonzero: usize,
    xr: &mut [f32; 576],
    t: &Tables,
) {
    let shift = if g.scalefac_scale { 2 } else { 1 };
    let gain = g.global_gain - 210;
    let mut line = |i: usize, quarter: i32, xr: &mut [f32; 576]| {
        let v = is[i];
        if v == 0 {
            xr[i] = 0.0;
            return;
        }
        let m = t.pow43(v.unsigned_abs()) * libm::exp2f(quarter as f32 * 0.25);
        xr[i] = if v < 0 { -m } else { m };
    };
    let mut i = 0;
    if g.block_type != 2 || g.mixed {
        // Long bands (all of them, or the first 36 lines of a mixed block).
        let limit = if g.block_type == 2 { 36 } else { 576 };
        for (b, &w) in LONG_WIDTHS[sr].iter().enumerate() {
            if i >= limit || i >= nonzero {
                break;
            }
            let pre = if g.preflag { PRETAB[b] } else { 0 };
            let q = gain - ((sf.long[b] + pre) << shift);
            for _ in 0..w {
                if i < 576 {
                    line(i, q, xr);
                }
                i += 1;
            }
        }
        if g.block_type != 2 {
            for x in xr[i.min(576)..].iter_mut() {
                *x = 0.0;
            }
            return;
        }
        i = 36;
    }
    let first = if g.mixed { 3 } else { 0 };
    for (b, &w) in SHORT_WIDTHS[sr].iter().enumerate().skip(first) {
        for win in 0..3 {
            let q = gain - 8 * g.subblock_gain[win] - (sf.short[b][win] << shift);
            for _ in 0..w {
                if i < 576 {
                    if i < nonzero {
                        line(i, q, xr);
                    } else {
                        xr[i] = 0.0;
                    }
                }
                i += 1;
            }
        }
    }
}

/// Mid/side and intensity stereo, in place on both channels (before short
/// blocks are reordered).
fn stereo(xr: &mut [[f32; 576]; 2], g: &[GrChannel; 2], sf_r: &Scalefactors, h: &Header, nonzero_r: usize) {
    let ms = h.mode_ext & 2 != 0;
    let intensity = h.mode_ext & 1 != 0;
    // Which lines carry intensity stereo, with their position.
    let mut is_pos: [Option<(i32, bool)>; 576] = [None; 576];
    if intensity {
        let gr = &g[1];
        let sr = h.sr_index;
        let mut i = 0;
        let long_end = if gr.block_type == 2 {
            if gr.mixed {
                36
            } else {
                0
            }
        } else {
            576
        };
        if long_end > 0 {
            // Long bands above the last non-zero right channel line.
            let mut last_band = -1i32;
            let mut j = 0;
            for (b, &w) in LONG_WIDTHS[sr].iter().enumerate() {
                if j < nonzero_r && (j..j + w).any(|k| k < 576 && xr[1][k] != 0.0) {
                    last_band = b as i32;
                }
                j += w;
            }
            for (b, &w) in LONG_WIDTHS[sr].iter().enumerate() {
                if i >= long_end {
                    break;
                }
                if b as i32 > last_band {
                    // The last band uses the position of the one before.
                    let src = if b == 21 { 20 } else { b };
                    let pos = sf_r.long[src];
                    let illegal = if h.version == Version::V1 { pos >= 7 } else { pos == sf_r.long_max[src] };
                    for k in i..(i + w).min(576) {
                        is_pos[k] = Some((pos, illegal));
                    }
                }
                i += w;
            }
            i = long_end;
        }
        if gr.block_type == 2 {
            let first = if gr.mixed { 3 } else { 0 };
            // Last non-zero band of each window.
            let mut last = [-1i32; 3];
            let mut j = i;
            for (b, &w) in SHORT_WIDTHS[sr].iter().enumerate().skip(first) {
                for win in 0..3 {
                    if (j..j + w).any(|k| k < 576 && xr[1][k] != 0.0) {
                        last[win] = b as i32;
                    }
                    j += w;
                }
            }
            for (b, &w) in SHORT_WIDTHS[sr].iter().enumerate().skip(first) {
                for win in 0..3 {
                    if b as i32 > last[win] {
                        let src = if b == 12 { 11 } else { b };
                        let pos = sf_r.short[src][win];
                        let max = sf_r.short_max[src][win];
                        let illegal = if h.version == Version::V1 { pos >= 7 } else { pos == max };
                        for k in i..(i + w).min(576) {
                            is_pos[k] = Some((pos, illegal));
                        }
                    }
                    i += w;
                }
            }
        }
    }
    let lsf_k = if g[1].scalefac_compress & 1 != 0 { FRAC_1_SQRT_2 } else { 0.840_896_4 };
    for k in 0..576 {
        let (l, r) = (xr[0][k], xr[1][k]);
        match is_pos[k] {
            Some((pos, false)) => {
                if h.version == Version::V1 {
                    let ratio = libm::tanf(pos as f32 * core::f32::consts::PI / 12.0);
                    if pos == 6 {
                        xr[0][k] = l;
                        xr[1][k] = 0.0;
                    } else {
                        xr[0][k] = l * ratio / (1.0 + ratio);
                        xr[1][k] = l / (1.0 + ratio);
                    }
                } else if pos == 0 {
                    xr[1][k] = l;
                } else if pos & 1 == 1 {
                    xr[0][k] = l * libm::powf(lsf_k, ((pos + 1) / 2) as f32);
                    xr[1][k] = l;
                } else {
                    xr[1][k] = l * libm::powf(lsf_k, (pos / 2) as f32);
                }
            }
            _ => {
                if ms {
                    xr[0][k] = (l + r) * FRAC_1_SQRT_2;
                    xr[1][k] = (l - r) * FRAC_1_SQRT_2;
                }
            }
        }
    }
}

/// Short blocks are coded band by band, window by window; the filter bank
/// wants the three windows of each line next to each other.
fn reorder(xr: &mut [f32; 576], sr: usize, mixed: bool) {
    let src = *xr;
    let first = if mixed { 3 } else { 0 };
    let mut i = if mixed { 36 } else { 0 };
    for &w in SHORT_WIDTHS[sr].iter().skip(first) {
        for win in 0..3 {
            for j in 0..w {
                if i + win * w + j < 576 && i + 3 * j + win < 576 {
                    xr[i + 3 * j + win] = src[i + win * w + j];
                }
            }
        }
        i += 3 * w;
    }
}

fn alias_reduce(xr: &mut [f32; 576], g: &GrChannel, t: &Tables) {
    let bands = match (g.block_type, g.mixed) {
        (2, false) => return,
        (2, true) => 1,
        _ => 31,
    };
    for sb in 1..=bands {
        for i in 0..8 {
            let (a, b) = (18 * sb - 1 - i, 18 * sb + i);
            let (x, y) = (xr[a], xr[b]);
            xr[a] = x * t.cs[i] - y * t.ca[i];
            xr[b] = y * t.cs[i] + x * t.ca[i];
        }
    }
}

impl Decoder for Mp3Decoder {
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
        let k = (target / self.spf as u64) as usize;
        // Prime from far enough back that the reservoir holds what frame
        // k - 1 needs (at most 511 bytes) and the filter banks have
        // settled, then drop everything before the target.
        let mut j = k.saturating_sub(1);
        let mut bytes = 0;
        while j > 0 && (bytes < 512 || j + 2 > k) {
            j -= 1;
            let at = self.frames[j];
            bytes += parse_header(&self.data[at..]).map_or(0, |h| h.len);
        }
        self.reset_state();
        self.next = j.min(self.frames.len());
        self.stream_pos = (j * self.spf) as u64;
        while self.next < k.min(self.frames.len()) {
            self.decode_frame();
            self.stream_pos += self.spf as u64;
        }
        self.from = target;
        Ok(frame)
    }
}
