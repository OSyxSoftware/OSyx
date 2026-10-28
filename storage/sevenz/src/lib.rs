//! Reads 7z archives (as used by the official Windows installers of
//! Mozilla-based browsers, a 7-Zip self-extractor stub followed by the
//! archive) without running any code from them.
//!
//! Supported: the original header and LZMA-encoded headers, folders built
//! from the Copy, LZMA, BCJ (x86) and BCJ2 coders in any binding, empty
//! files and directories, per-file CRC32 checks. Not supported: LZMA2,
//! PPMd, BZip2, Deflate, encryption, external or multi-volume archives.
//!
//! Data is streamed: [`extract`] hands every file to a [`Sink`] in archive
//! order, with only the decoders' dictionaries in memory.
#![no_std]

extern crate alloc;

mod lzma;

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;

pub use lzma::Lzma;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Io,
    NotArchive,
    Truncated(&'static str),
    Corrupt(&'static str),
    Unsupported(&'static str),
    Crc,
    /// The sink refused data.
    Sink,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Io => f.write_str("read error"),
            Error::NotArchive => f.write_str("no 7z archive found"),
            Error::Truncated(w) => write!(f, "truncated archive: {w}"),
            Error::Corrupt(w) => write!(f, "corrupt archive: {w}"),
            Error::Unsupported(w) => write!(f, "unsupported archive feature: {w}"),
            Error::Crc => f.write_str("CRC mismatch"),
            Error::Sink => f.write_str("output error"),
        }
    }
}

pub type Result<T> = core::result::Result<T, Error>;

/// A byte stream (decoder output or packed data).
pub trait Stream {
    /// Fills `out` as far as possible; 0 at the end.
    fn read(&mut self, out: &mut [u8]) -> Result<usize>;
}

/// The archive file, read at arbitrary offsets.
pub trait Source {
    fn size(&self) -> u64;
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize>;
}

impl Source for Vec<u8> {
    fn size(&self) -> u64 {
        self.as_slice().len() as u64
    }
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let at = (offset as usize).min(self.as_slice().len());
        let n = buf.len().min(self.as_slice().len() - at);
        buf[..n].copy_from_slice(&self[at..at + n]);
        Ok(n)
    }
}

/// Where extracted entries go.
pub trait Sink {
    fn dir(&mut self, path: &str) -> Result<()>;
    fn file_start(&mut self, path: &str, size: u64) -> Result<()>;
    fn file_data(&mut self, data: &[u8]) -> Result<()>;
    fn file_end(&mut self) -> Result<()>;
}

// ---- CRC32 -----------------------------------------------------------------------

const fn crc_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

static CRC: [u32; 256] = crc_table();

fn crc_update(mut c: u32, data: &[u8]) -> u32 {
    for &b in data {
        c = CRC[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c
}

pub fn crc32(data: &[u8]) -> u32 {
    !crc_update(!0, data)
}

// ---- packed streams --------------------------------------------------------------

type Shared = Rc<RefCell<dyn Source>>;

struct Packed {
    src: Shared,
    at: u64,
    end: u64,
}

impl Stream for Packed {
    fn read(&mut self, out: &mut [u8]) -> Result<usize> {
        let n = (out.len() as u64).min(self.end - self.at) as usize;
        if n == 0 {
            return Ok(0);
        }
        let got = self.src.borrow_mut().read_at(self.at, &mut out[..n])?;
        if got == 0 {
            return Err(Error::Truncated("packed stream"));
        }
        self.at += got as u64;
        Ok(got)
    }
}

/// A stream limited to `left` bytes.
struct Limited {
    inner: Box<dyn Stream>,
    left: u64,
}

impl Stream for Limited {
    fn read(&mut self, out: &mut [u8]) -> Result<usize> {
        let n = (out.len() as u64).min(self.left) as usize;
        if n == 0 {
            return Ok(0);
        }
        let got = self.inner.read(&mut out[..n])?;
        self.left -= got as u64;
        Ok(got)
    }
}

/// Buffered byte reader over a stream (for BCJ2's inputs).
struct Bytes {
    src: Box<dyn Stream>,
    buf: Vec<u8>,
    at: usize,
    len: usize,
}

impl Bytes {
    fn new(src: Box<dyn Stream>, size: usize) -> Bytes {
        Bytes { src, buf: vec![0; size], at: 0, len: 0 }
    }
    /// The next byte, or `None` at the end.
    fn next(&mut self) -> Result<Option<u8>> {
        if self.at == self.len {
            self.len = self.src.read(&mut self.buf)?;
            self.at = 0;
            if self.len == 0 {
                return Ok(None);
            }
        }
        self.at += 1;
        Ok(Some(self.buf[self.at - 1]))
    }
    fn be32(&mut self) -> Result<u32> {
        let mut v = 0u32;
        for _ in 0..4 {
            v = (v << 8) | self.next()?.ok_or(Error::Truncated("BCJ2 call/jump stream"))? as u32;
        }
        Ok(v)
    }
}

// ---- BCJ2 and BCJ ------------------------------------------------------------------

/// The BCJ2 x86 branch converter (original 7-Zip version, 4 inputs: main,
/// call, jump, range-coded selector).
struct Bcj2 {
    main: Bytes,
    call: Bytes,
    jump: Bytes,
    rc: lzma::RangeDecoder,
    probs: [u16; 2 + 256],
    prev: u8,
    out_pos: u32,
    size: u64,
    total: u64,
    pending: [u8; 4],
    pending_at: usize,
    pending_len: usize,
}

fn is_jump(b0: u8, b1: u8) -> bool {
    (b1 & 0xFE) == 0xE8 || (b0 == 0x0F && (b1 & 0xF0) == 0x80)
}

impl Bcj2 {
    fn new(inputs: Vec<Box<dyn Stream>>, size: u64) -> Result<Bcj2> {
        let mut it = inputs.into_iter();
        let (main, call, jump, rc) = match (it.next(), it.next(), it.next(), it.next()) {
            (Some(a), Some(b), Some(c), Some(d)) => (a, b, c, d),
            _ => return Err(Error::Corrupt("BCJ2 needs 4 inputs")),
        };
        Ok(Bcj2 {
            main: Bytes::new(main, 256 * 1024),
            call: Bytes::new(call, 16 * 1024),
            jump: Bytes::new(jump, 16 * 1024),
            rc: lzma::RangeDecoder::new(lzma::ByteIn::new(rc), false)?,
            probs: [1024; 2 + 256],
            prev: 0,
            out_pos: 0,
            size,
            total: 0,
            pending: [0; 4],
            pending_at: 0,
            pending_len: 0,
        })
    }
}

impl Stream for Bcj2 {
    fn read(&mut self, out: &mut [u8]) -> Result<usize> {
        let mut n = 0;
        while n < out.len() && self.total < self.size {
            if self.pending_at < self.pending_len {
                out[n] = self.pending[self.pending_at];
                self.pending_at += 1;
                n += 1;
                self.total += 1;
                continue;
            }
            let Some(b) = self.main.next()? else {
                return Err(Error::Truncated("BCJ2 main stream"));
            };
            out[n] = b;
            n += 1;
            self.total += 1;
            self.out_pos = self.out_pos.wrapping_add(1);
            if !is_jump(self.prev, b) {
                self.prev = b;
                continue;
            }
            let p = match b {
                0xE8 => &mut self.probs[self.prev as usize],
                0xE9 => &mut self.probs[256],
                _ => &mut self.probs[257],
            };
            if self.rc.bit(p)? == 0 {
                self.prev = b;
                continue;
            }
            let src = if b == 0xE8 { self.call.be32()? } else { self.jump.be32()? };
            let dest = src.wrapping_sub(self.out_pos.wrapping_add(4));
            self.pending = dest.to_le_bytes();
            self.pending_at = 0;
            self.pending_len = 4;
            self.out_pos = self.out_pos.wrapping_add(4);
            self.prev = (dest >> 24) as u8;
        }
        Ok(n)
    }
}

/// The BCJ x86 filter (decoding), applied to a whole stream.
struct Bcj {
    src: Box<dyn Stream>,
    buf: Vec<u8>,
    /// Bytes in `buf`; the first `ready` are converted.
    len: usize,
    ready: usize,
    at: usize,
    ip: u32,
    state: u32,
    eof: bool,
}

impl Bcj {
    fn new(src: Box<dyn Stream>) -> Bcj {
        Bcj { src, buf: vec![0; 64 * 1024], len: 0, ready: 0, at: 0, ip: 0, state: 0, eof: false }
    }

    /// The x86 BCJ converter of the LZMA SDK (decoding direction); returns
    /// how many bytes are final.
    fn convert(&mut self) -> usize {
        const MASK_TO_ALLOWED: [bool; 8] = [true, true, true, false, true, false, false, false];
        const MASK_TO_BIT: [u32; 8] = [0, 1, 2, 2, 3, 3, 3, 3];
        let data = &mut self.buf[..self.len];
        let size = data.len();
        if size < 5 {
            return 0;
        }
        let ip = self.ip;
        let mut prev_pos: isize = -5;
        let mut prev_mask = self.state & 7;
        let mut i = 0usize;
        loop {
            while i + 5 <= size && (data[i] & 0xFE) != 0xE8 {
                i += 1;
            }
            if i + 5 > size {
                break;
            }
            let d = i as isize - prev_pos;
            prev_pos = i as isize;
            if d > 5 {
                prev_mask = 0;
            } else {
                for _ in 0..d {
                    prev_mask &= 0x77;
                    prev_mask <<= 1;
                }
            }
            let b4 = data[i + 4];
            if (b4 == 0 || b4 == 0xFF) && MASK_TO_ALLOWED[((prev_mask >> 1) & 7) as usize] && (prev_mask >> 1) < 0x10 {
                let mut src = u32::from_le_bytes([data[i + 1], data[i + 2], data[i + 3], data[i + 4]]);
                let mut dest;
                loop {
                    dest = src.wrapping_sub(ip.wrapping_add(i as u32 + 5));
                    if prev_mask == 0 {
                        break;
                    }
                    let idx = MASK_TO_BIT[(prev_mask >> 1) as usize] * 8;
                    let b = (dest >> (24 - idx)) as u8;
                    if !(b == 0 || b == 0xFF) {
                        break;
                    }
                    src = dest ^ ((1u32 << (32 - idx)) - 1);
                }
                let b4n = (!(((dest >> 24) & 1).wrapping_sub(1))) as u8;
                data[i + 1] = dest as u8;
                data[i + 2] = (dest >> 8) as u8;
                data[i + 3] = (dest >> 16) as u8;
                data[i + 4] = b4n;
                i += 5;
                prev_mask = 0;
            } else {
                i += 1;
                prev_mask |= 1;
                if b4 == 0 || b4 == 0xFF {
                    prev_mask |= 0x10;
                }
            }
        }
        let d = i as isize - prev_pos;
        self.state = if d > 5 { 0 } else { (prev_mask << (d - 1)) & 7 };
        self.ip = ip.wrapping_add(i as u32);
        i
    }
}

impl Stream for Bcj {
    fn read(&mut self, out: &mut [u8]) -> Result<usize> {
        loop {
            if self.at < self.ready {
                let n = out.len().min(self.ready - self.at);
                out[..n].copy_from_slice(&self.buf[self.at..self.at + n]);
                self.at += n;
                return Ok(n);
            }
            if self.eof {
                // Unconverted tail (fewer than 5 bytes) passes unchanged.
                if self.ready < self.len {
                    self.ready = self.len;
                    continue;
                }
                return Ok(0);
            }
            // Keep the unconverted tail, refill.
            self.buf.copy_within(self.ready..self.len, 0);
            self.len -= self.ready;
            self.ready = 0;
            self.at = 0;
            while self.len < self.buf.len() {
                let n = self.src.read(&mut self.buf[self.len..])?;
                if n == 0 {
                    self.eof = true;
                    break;
                }
                self.len += n;
            }
            self.ready = self.convert();
        }
    }
}

// ---- headers ----------------------------------------------------------------------

struct Reader<'a> {
    b: &'a [u8],
    p: usize,
}

impl Reader<'_> {
    fn byte(&mut self) -> Result<u8> {
        let v = *self.b.get(self.p).ok_or(Error::Truncated("header"))?;
        self.p += 1;
        Ok(v)
    }
    fn bytes(&mut self, n: usize) -> Result<&[u8]> {
        let end = self.p.checked_add(n).filter(|&e| e <= self.b.len()).ok_or(Error::Truncated("header"))?;
        let v = &self.b[self.p..end];
        self.p = end;
        Ok(v)
    }
    fn num(&mut self) -> Result<u64> {
        let first = self.byte()?;
        let mut mask = 0x80u8;
        let mut v = 0u64;
        for i in 0..8 {
            if first & mask == 0 {
                let hi = (first & mask.wrapping_sub(1)) as u64;
                return Ok(v | (hi << (8 * i)));
            }
            v |= (self.byte()? as u64) << (8 * i);
            mask >>= 1;
        }
        Ok(v)
    }
    fn small(&mut self, max: u64) -> Result<usize> {
        let v = self.num()?;
        if v > max {
            return Err(Error::Corrupt("count out of range"));
        }
        Ok(v as usize)
    }
    fn bits(&mut self, n: usize) -> Result<Vec<bool>> {
        let mut out = Vec::with_capacity(n);
        let (mut b, mut m) = (0u8, 0u8);
        for _ in 0..n {
            if m == 0 {
                b = self.byte()?;
                m = 0x80;
            }
            out.push(b & m != 0);
            m >>= 1;
        }
        Ok(out)
    }
    fn digests(&mut self, n: usize) -> Result<Vec<Option<u32>>> {
        let all = self.byte()?;
        let defined = if all != 0 { vec![true; n] } else { self.bits(n)? };
        let mut out = Vec::with_capacity(n);
        for d in defined {
            out.push(if d { Some(u32::from_le_bytes(self.bytes(4)?.try_into().expect("4 bytes"))) } else { None });
        }
        Ok(out)
    }
}

#[derive(Clone, Debug)]
struct Coder {
    id: Vec<u8>,
    ins: usize,
    outs: usize,
    props: Vec<u8>,
}

#[derive(Clone, Debug, Default)]
struct Folder {
    coders: Vec<Coder>,
    /// (in index, out index)
    bonds: Vec<(usize, usize)>,
    /// In indices fed by packed streams, in pack order.
    packed: Vec<usize>,
    unpack: Vec<u64>,
    crc: Option<u32>,
}

impl Folder {
    /// The out stream no bond consumes: the folder's result.
    fn main_out(&self) -> Result<usize> {
        (0..self.unpack.len()).find(|o| !self.bonds.iter().any(|b| b.1 == *o)).ok_or(Error::Corrupt("folder has no main output"))
    }
    fn size(&self) -> Result<u64> {
        Ok(self.unpack[self.main_out()?])
    }
}

#[derive(Debug, Default)]
struct Streams {
    pack_pos: u64,
    pack_sizes: Vec<u64>,
    folders: Vec<Folder>,
    /// Files per folder and their sizes and CRCs.
    sub_counts: Vec<usize>,
    sub_sizes: Vec<u64>,
    sub_crcs: Vec<Option<u32>>,
}

const MAX_ITEMS: u64 = 1 << 20;

fn read_streams(r: &mut Reader<'_>) -> Result<Streams> {
    let mut s = Streams::default();
    loop {
        match r.byte()? {
            0 => break,
            6 => {
                s.pack_pos = r.num()?;
                let n = r.small(MAX_ITEMS)?;
                loop {
                    match r.byte()? {
                        0 => break,
                        9 => s.pack_sizes = (0..n).map(|_| r.num()).collect::<Result<_>>()?,
                        10 => {
                            r.digests(n)?;
                        }
                        _ => return Err(Error::Unsupported("pack info property")),
                    }
                }
            }
            7 => {
                if r.byte()? != 11 {
                    return Err(Error::Corrupt("expected folders"));
                }
                let n = r.small(MAX_ITEMS)?;
                if r.byte()? != 0 {
                    return Err(Error::Unsupported("external folders"));
                }
                for _ in 0..n {
                    let mut f = Folder::default();
                    let nc = r.small(64)?;
                    for _ in 0..nc {
                        let flag = r.byte()?;
                        if flag & 0x80 != 0 {
                            return Err(Error::Unsupported("alternative coder methods"));
                        }
                        let id = r.bytes((flag & 0x0F) as usize)?.to_vec();
                        let (ins, outs) = if flag & 0x10 != 0 { (r.small(64)?, r.small(64)?) } else { (1, 1) };
                        let props = if flag & 0x20 != 0 {
                            let n = r.small(1 << 16)?;
                            r.bytes(n)?.to_vec()
                        } else {
                            Vec::new()
                        };
                        f.coders.push(Coder { id, ins, outs, props });
                    }
                    let total_out: usize = f.coders.iter().map(|c| c.outs).sum();
                    let total_in: usize = f.coders.iter().map(|c| c.ins).sum();
                    for _ in 1..total_out {
                        f.bonds.push((r.small(256)?, r.small(256)?));
                    }
                    let np = total_in.checked_sub(f.bonds.len()).ok_or(Error::Corrupt("bonds"))?;
                    if np == 1 {
                        let free = (0..total_in).find(|i| !f.bonds.iter().any(|b| b.0 == *i)).ok_or(Error::Corrupt("bonds"))?;
                        f.packed.push(free);
                    } else {
                        for _ in 0..np {
                            f.packed.push(r.small(256)?);
                        }
                    }
                    s.folders.push(f);
                }
                if r.byte()? != 12 {
                    return Err(Error::Corrupt("expected unpack sizes"));
                }
                for f in &mut s.folders {
                    let outs: usize = f.coders.iter().map(|c| c.outs).sum();
                    f.unpack = (0..outs).map(|_| r.num()).collect::<Result<_>>()?;
                }
                loop {
                    match r.byte()? {
                        0 => break,
                        10 => {
                            let d = r.digests(s.folders.len())?;
                            for (f, c) in s.folders.iter_mut().zip(d) {
                                f.crc = c;
                            }
                        }
                        _ => return Err(Error::Unsupported("unpack info property")),
                    }
                }
            }
            8 => {
                s.sub_counts = vec![1; s.folders.len()];
                let mut t = r.byte()?;
                if t == 13 {
                    for c in &mut s.sub_counts {
                        *c = r.small(MAX_ITEMS)?;
                    }
                    t = r.byte()?;
                }
                let mut sizes = Vec::new();
                for (i, &c) in s.sub_counts.iter().enumerate() {
                    if c == 0 {
                        continue;
                    }
                    let mut sum = 0u64;
                    if t == 9 {
                        for _ in 1..c {
                            let v = r.num()?;
                            sum = sum.checked_add(v).ok_or(Error::Corrupt("sizes"))?;
                            sizes.push(v);
                        }
                    }
                    let total = s.folders[i].size()?;
                    sizes.push(total.checked_sub(sum).ok_or(Error::Corrupt("file sizes exceed the folder"))?);
                }
                if t == 9 {
                    t = r.byte()?;
                }
                // CRCs of streams whose folder has none (or holds several files).
                let need: usize = s
                    .folders
                    .iter()
                    .zip(&s.sub_counts)
                    .map(|(f, &c)| if c == 1 && f.crc.is_some() { 0 } else { c })
                    .sum();
                let mut crcs = Vec::new();
                while t != 0 {
                    if t == 10 {
                        let d = r.digests(need)?;
                        let mut it = d.into_iter();
                        for (f, &c) in s.folders.iter().zip(&s.sub_counts) {
                            if c == 1 && f.crc.is_some() {
                                crcs.push(f.crc);
                            } else {
                                for _ in 0..c {
                                    crcs.push(it.next().flatten());
                                }
                            }
                        }
                    } else {
                        let n = r.num()? as usize;
                        r.bytes(n)?;
                    }
                    t = r.byte()?;
                }
                s.sub_sizes = sizes;
                s.sub_crcs = crcs;
            }
            _ => return Err(Error::Unsupported("streams info property")),
        }
    }
    if s.sub_counts.is_empty() {
        s.sub_counts = vec![1; s.folders.len()];
        s.sub_sizes = s.folders.iter().map(|f| f.size()).collect::<Result<_>>()?;
        s.sub_crcs = s.folders.iter().map(|f| f.crc).collect();
    }
    Ok(s)
}

/// Builds the decoder chain for `folder`; `first_pack` is the index of its
/// first packed stream.
fn folder_stream(src: &Shared, base: u64, s: &Streams, folder: usize, first_pack: usize) -> Result<Box<dyn Stream>> {
    let f = &s.folders[folder];
    let mut pack_offsets = Vec::new();
    let mut off = base + s.pack_pos;
    for (i, &size) in s.pack_sizes.iter().enumerate() {
        pack_offsets.push((off, size));
        off += size;
        let _ = i;
    }
    fn build_out(src: &Shared, f: &Folder, packs: &[(u64, u64)], first_pack: usize, out: usize) -> Result<Box<dyn Stream>> {
        let (mut in_base, mut out_base) = (0, 0);
        for c in &f.coders {
            if out < out_base + c.outs {
                if c.outs != 1 {
                    return Err(Error::Unsupported("coder with several outputs"));
                }
                let mut inputs = Vec::new();
                for i in in_base..in_base + c.ins {
                    inputs.push(build_in(src, f, packs, first_pack, i)?);
                }
                let size = f.unpack[out];
                let stream: Box<dyn Stream> = match c.id.as_slice() {
                    [0x00] => inputs.pop().ok_or(Error::Corrupt("copy input"))?,
                    [0x03, 0x01, 0x01] => Box::new(Lzma::new(&c.props, inputs.pop().ok_or(Error::Corrupt("LZMA input"))?, size)?),
                    [0x03, 0x03, 0x01, 0x03] => Box::new(Bcj::new(inputs.pop().ok_or(Error::Corrupt("BCJ input"))?)),
                    [0x03, 0x03, 0x01, 0x1B] => Box::new(Bcj2::new(inputs, size)?),
                    [0x21] => return Err(Error::Unsupported("LZMA2")),
                    [0x06, 0xF1, 0x07, 0x01] => return Err(Error::Unsupported("encryption")),
                    _ => return Err(Error::Unsupported("compression method")),
                };
                return Ok(Box::new(Limited { inner: stream, left: size }));
            }
            in_base += c.ins;
            out_base += c.outs;
        }
        Err(Error::Corrupt("out stream index"))
    }
    fn build_in(src: &Shared, f: &Folder, packs: &[(u64, u64)], first_pack: usize, i: usize) -> Result<Box<dyn Stream>> {
        if let Some(b) = f.bonds.iter().find(|b| b.0 == i) {
            return build_out(src, f, packs, first_pack, b.1);
        }
        let k = f.packed.iter().position(|&p| p == i).ok_or(Error::Corrupt("unbound input"))?;
        let (at, size) = *packs.get(first_pack + k).ok_or(Error::Corrupt("pack stream index"))?;
        if at.checked_add(size).is_none_or(|e| e > src.borrow().size()) {
            return Err(Error::Truncated("packed stream beyond the end"));
        }
        Ok(Box::new(Packed { src: src.clone(), at, end: at + size }))
    }
    build_out(src, f, &pack_offsets, first_pack, f.main_out()?)
}

fn read_all(mut s: Box<dyn Stream>, size: usize) -> Result<Vec<u8>> {
    let mut v = vec![0u8; size];
    let mut done = 0;
    while done < size {
        let n = s.read(&mut v[done..])?;
        if n == 0 {
            return Err(Error::Truncated("header stream"));
        }
        done += n;
    }
    Ok(v)
}

/// One archive entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub dir: bool,
    pub size: u64,
}

/// Position of the 7z signature in `src` (a self-extractor starts with
/// its stub), searching the first `limit` bytes.
pub fn find(src: &mut dyn Source, limit: u64) -> Result<u64> {
    const SIG: [u8; 6] = [b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C];
    let mut buf = vec![0u8; 64 * 1024 + 5];
    let mut at = 0u64;
    while at < limit.min(src.size()) {
        let n = src.read_at(at, &mut buf)?;
        if n < 6 {
            break;
        }
        if let Some(i) = buf[..n].windows(6).position(|w| w == SIG) {
            return Ok(at + i as u64);
        }
        at += (n - 5) as u64;
    }
    Err(Error::NotArchive)
}

struct Archive {
    base: u64,
    streams: Streams,
    entries: Vec<Entry>,
    /// Whether each entry has data (in substream order).
    has_stream: Vec<bool>,
}

fn open(src: &Shared, base: u64) -> Result<Archive> {
    let mut sh = [0u8; 32];
    if src.borrow_mut().read_at(base, &mut sh)? != 32 || sh[..6] != [b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C] {
        return Err(Error::NotArchive);
    }
    if crc32(&sh[12..32]) != u32::from_le_bytes(sh[8..12].try_into().expect("4 bytes")) {
        return Err(Error::Crc);
    }
    let next_off = u64::from_le_bytes(sh[12..20].try_into().expect("8"));
    let next_size = u64::from_le_bytes(sh[20..28].try_into().expect("8"));
    let next_crc = u32::from_le_bytes(sh[28..32].try_into().expect("4"));
    if next_size > 64 << 20 {
        return Err(Error::Unsupported("header larger than 64 MiB"));
    }
    let at = base + 32 + next_off;
    let mut header = vec![0u8; next_size as usize];
    if src.borrow_mut().read_at(at, &mut header)? != header.len() {
        return Err(Error::Truncated("header"));
    }
    if crc32(&header) != next_crc {
        return Err(Error::Crc);
    }
    // An encoded header: decode it first.
    loop {
        let mut r = Reader { b: &header, p: 0 };
        match r.byte()? {
            0x17 => {
                let s = read_streams(&mut r)?;
                let f = s.folders.first().ok_or(Error::Corrupt("encoded header"))?;
                let size = f.size()? as usize;
                if size > 64 << 20 {
                    return Err(Error::Unsupported("header larger than 64 MiB"));
                }
                let crc = f.crc;
                let data = read_all(folder_stream(src, base + 32, &s, 0, 0)?, size)?;
                if crc.is_some_and(|c| c != crc32(&data)) {
                    return Err(Error::Crc);
                }
                header = data;
            }
            0x01 => break,
            _ => return Err(Error::Corrupt("header type")),
        }
    }
    let mut r = Reader { b: &header, p: 0 };
    r.byte()?;
    let mut t = r.byte()?;
    for skip in [2u8, 3] {
        if t == skip {
            // Archive properties / additional streams: not used.
            return Err(Error::Unsupported("archive properties"));
        }
    }
    let mut streams = Streams::default();
    if t == 4 {
        streams = read_streams(&mut r)?;
        t = r.byte()?;
    }
    let mut entries = Vec::new();
    let mut has_stream = Vec::new();
    if t == 5 {
        let n = r.small(MAX_ITEMS)?;
        let mut empty_stream = vec![false; n];
        let mut empty_file: Vec<bool> = Vec::new();
        let mut names: Vec<String> = Vec::new();
        let mut attrs: Vec<Option<u32>> = vec![None; n];
        loop {
            let id = r.byte()?;
            if id == 0 {
                break;
            }
            let size = r.small(64 << 20)?;
            let data = r.bytes(size)?;
            let mut pr = Reader { b: data, p: 0 };
            match id {
                0x0E => empty_stream = pr.bits(n)?,
                0x0F => empty_file = pr.bits(empty_stream.iter().filter(|e| **e).count())?,
                0x11 => {
                    if pr.byte()? != 0 {
                        return Err(Error::Unsupported("external names"));
                    }
                    let rest = &data[1..];
                    let units: Vec<u16> = rest.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
                    for name in units.split(|&u| u == 0).take(n) {
                        names.push(String::from_utf16_lossy(name));
                    }
                }
                0x15 => {
                    let all = pr.byte()?;
                    let defined = if all != 0 { vec![true; n] } else { pr.bits(n)? };
                    if pr.byte()? != 0 {
                        return Err(Error::Unsupported("external attributes"));
                    }
                    for (i, d) in defined.into_iter().enumerate() {
                        if d {
                            attrs[i] = Some(u32::from_le_bytes(pr.bytes(4)?.try_into().expect("4")));
                        }
                    }
                }
                _ => {}
            }
        }
        if names.len() != n {
            return Err(Error::Corrupt("file names"));
        }
        let mut empty_index = 0;
        let mut sub = 0;
        for (i, name) in names.into_iter().enumerate() {
            let path = name.replace('\\', "/");
            if path.is_empty() || path.starts_with('/') || path.split('/').any(|c| c == ".." || c.is_empty()) || path.contains(':') {
                return Err(Error::Corrupt("unsafe path in the archive"));
            }
            if empty_stream[i] {
                let is_file = empty_file.get(empty_index).copied().unwrap_or(false);
                empty_index += 1;
                let dir = !is_file && attrs[i].is_none_or(|a| a & 0x10 != 0);
                entries.push(Entry { path, dir, size: 0 });
                has_stream.push(false);
            } else {
                let size = *streams.sub_sizes.get(sub).ok_or(Error::Corrupt("more files than streams"))?;
                sub += 1;
                entries.push(Entry { path, dir: false, size });
                has_stream.push(true);
            }
        }
    }
    Ok(Archive { base, streams, entries, has_stream })
}

/// The entries of the archive at `base` in `src`.
pub fn list<S: Source + 'static>(src: S, base: u64) -> Result<Vec<Entry>> {
    let shared: Shared = Rc::new(RefCell::new(src));
    Ok(open(&shared, base)?.entries)
}

/// Extracts every entry of the archive at `base` into `sink`, checking the
/// CRC32 of each file that has one. Returns the bytes written.
pub fn extract<S: Source + 'static>(src: S, base: u64, sink: &mut dyn Sink) -> Result<u64> {
    let shared: Shared = Rc::new(RefCell::new(src));
    let a = open(&shared, base)?;
    let mut total = 0u64;
    // Directories and empty files first need no data.
    let mut sub = 0usize;
    let mut folder = 0usize;
    let mut left_in_folder = 0usize;
    let mut first_pack = 0usize;
    let mut stream: Option<Box<dyn Stream>> = None;
    let mut buf = vec![0u8; 256 * 1024];
    for (e, has) in a.entries.iter().zip(&a.has_stream) {
        if !has {
            if e.dir {
                sink.dir(&e.path)?;
            } else {
                sink.file_start(&e.path, 0)?;
                sink.file_end()?;
            }
            continue;
        }
        while left_in_folder == 0 {
            if folder > 0 || stream.is_some() {
                first_pack += a.streams.folders[folder].packed.len();
                folder += 1;
            }
            if folder >= a.streams.folders.len() {
                return Err(Error::Corrupt("more files than folders"));
            }
            left_in_folder = a.streams.sub_counts[folder];
            stream = Some(folder_stream(&shared, a.base + 32, &a.streams, folder, first_pack)?);
        }
        let s = stream.as_mut().expect("stream set above");
        sink.file_start(&e.path, e.size)?;
        let mut crc = !0u32;
        let mut left = e.size;
        while left > 0 {
            let want = (left as usize).min(buf.len());
            let n = s.read(&mut buf[..want])?;
            if n == 0 {
                return Err(Error::Truncated("file data"));
            }
            crc = crc_update(crc, &buf[..n]);
            sink.file_data(&buf[..n])?;
            left -= n as u64;
            total += n as u64;
        }
        if let Some(Some(want)) = a.streams.sub_crcs.get(sub) {
            if !crc != *want {
                return Err(Error::Crc);
            }
        }
        sink.file_end()?;
        sub += 1;
        left_in_folder -= 1;
    }
    Ok(total)
}
