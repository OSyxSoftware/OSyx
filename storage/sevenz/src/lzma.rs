//! LZMA (the original format of the LZMA SDK) as a pull decoder: output is
//! produced on request in caller-sized pieces, so a stream of hundreds of
//! megabytes needs only its dictionary in memory.

use crate::{Error, Result, Stream};
use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

const PROB_INIT: u16 = 1024;
const STATES: usize = 12;
const POS_STATES_MAX: usize = 16;
const END_POS_MODEL: u32 = 14;
const FULL_DISTANCES: usize = 128;
const ALIGN_BITS: u32 = 4;

/// Byte input for the range decoder, fed from another stream.
pub(crate) struct ByteIn {
    src: Box<dyn Stream>,
    buf: Vec<u8>,
    at: usize,
    len: usize,
}

impl ByteIn {
    pub(crate) fn new(src: Box<dyn Stream>) -> ByteIn {
        ByteIn { src, buf: vec![0; 64 * 1024], at: 0, len: 0 }
    }

    pub(crate) fn byte(&mut self) -> Result<u8> {
        if self.at == self.len {
            self.len = self.src.read(&mut self.buf)?;
            self.at = 0;
            if self.len == 0 {
                return Err(Error::Truncated("compressed stream ends early"));
            }
        }
        let b = self.buf[self.at];
        self.at += 1;
        Ok(b)
    }
}

pub(crate) struct RangeDecoder {
    pub(crate) input: ByteIn,
    range: u32,
    code: u32,
}

impl RangeDecoder {
    /// `lzma`: the stream is LZMA's, whose first byte must be 0.
    pub(crate) fn new(mut input: ByteIn, lzma: bool) -> Result<RangeDecoder> {
        let mut code = 0u32;
        for i in 0..5 {
            let b = input.byte()?;
            if i == 0 && lzma && b != 0 {
                return Err(Error::Corrupt("LZMA stream does not start with 0"));
            }
            code = (code << 8) | b as u32;
        }
        Ok(RangeDecoder { input, range: 0xFFFF_FFFF, code })
    }

    #[inline]
    fn normalize(&mut self) -> Result<()> {
        if self.range < 1 << 24 {
            self.range <<= 8;
            self.code = (self.code << 8) | self.input.byte()? as u32;
        }
        Ok(())
    }

    #[inline]
    pub(crate) fn bit(&mut self, p: &mut u16) -> Result<u32> {
        let bound = (self.range >> 11) * *p as u32;
        let bit = if self.code < bound {
            self.range = bound;
            *p += (2048 - *p) >> 5;
            0
        } else {
            self.range -= bound;
            self.code -= bound;
            *p -= *p >> 5;
            1
        };
        self.normalize()?;
        Ok(bit)
    }

    fn direct(&mut self, bits: u32) -> Result<u32> {
        let mut res = 0u32;
        for _ in 0..bits {
            self.range >>= 1;
            self.code = self.code.wrapping_sub(self.range);
            let t = 0u32.wrapping_sub(self.code >> 31);
            self.code = self.code.wrapping_add(self.range & t);
            if self.code == self.range {
                return Err(Error::Corrupt("LZMA range decoder"));
            }
            self.normalize()?;
            res = (res << 1).wrapping_add(t.wrapping_add(1));
        }
        Ok(res)
    }

    fn tree(&mut self, probs: &mut [u16], bits: u32) -> Result<u32> {
        let mut m = 1usize;
        for _ in 0..bits {
            m = (m << 1) + self.bit(&mut probs[m])? as usize;
        }
        Ok(m as u32 - (1 << bits))
    }

    fn reverse_tree(&mut self, probs: &mut [u16], bits: u32) -> Result<u32> {
        let mut m = 1usize;
        let mut sym = 0u32;
        for i in 0..bits {
            let b = self.bit(&mut probs[m])?;
            m = (m << 1) + b as usize;
            sym |= b << i;
        }
        Ok(sym)
    }
}

struct LenDecoder {
    choice: u16,
    choice2: u16,
    low: [[u16; 8]; POS_STATES_MAX],
    mid: [[u16; 8]; POS_STATES_MAX],
    high: [u16; 256],
}

impl LenDecoder {
    fn new() -> LenDecoder {
        LenDecoder {
            choice: PROB_INIT,
            choice2: PROB_INIT,
            low: [[PROB_INIT; 8]; POS_STATES_MAX],
            mid: [[PROB_INIT; 8]; POS_STATES_MAX],
            high: [PROB_INIT; 256],
        }
    }

    fn decode(&mut self, rc: &mut RangeDecoder, pos_state: usize) -> Result<u32> {
        if rc.bit(&mut self.choice)? == 0 {
            return rc.tree(&mut self.low[pos_state], 3);
        }
        if rc.bit(&mut self.choice2)? == 0 {
            return Ok(8 + rc.tree(&mut self.mid[pos_state], 3)?);
        }
        Ok(16 + rc.tree(&mut self.high, 8)?)
    }
}

/// An LZMA stream of a known decompressed size.
pub struct Lzma {
    rc: RangeDecoder,
    lc: u32,
    lp: u32,
    pb: u32,
    dict: Vec<u8>,
    /// Next write position in `dict` (circular).
    pos: usize,
    /// Bytes produced so far.
    total: u64,
    size: u64,
    full: bool,
    state: usize,
    reps: [u32; 4],
    /// Match bytes still to copy.
    pending: u32,
    literal: Vec<u16>,
    is_match: [u16; STATES * POS_STATES_MAX],
    is_rep: [u16; STATES],
    is_rep_g0: [u16; STATES],
    is_rep_g1: [u16; STATES],
    is_rep_g2: [u16; STATES],
    is_rep0_long: [u16; STATES * POS_STATES_MAX],
    pos_slot: [[u16; 64]; 4],
    pos_decoders: [u16; 1 + FULL_DISTANCES - END_POS_MODEL as usize],
    align: [u16; 16],
    len: LenDecoder,
    rep_len: LenDecoder,
}

impl Lzma {
    /// `props`: the 5 property bytes (lc/lp/pb, dictionary size).
    pub fn new(props: &[u8], input: Box<dyn Stream>, size: u64) -> Result<Lzma> {
        if props.len() < 5 || props[0] >= 9 * 5 * 5 {
            return Err(Error::Unsupported("LZMA properties"));
        }
        let d = props[0] as u32;
        let (lc, lp, pb) = (d % 9, (d / 9) % 5, d / 45);
        let dict_size = u32::from_le_bytes([props[1], props[2], props[3], props[4]]).max(4096);
        // Never more than the output itself needs.
        let dict_len = (dict_size as u64).min(size.max(1)) as usize;
        let rc = RangeDecoder::new(ByteIn::new(input), true)?;
        Ok(Lzma {
            rc,
            lc,
            lp,
            pb,
            dict: vec![0; dict_len],
            pos: 0,
            total: 0,
            size,
            full: false,
            state: 0,
            reps: [0; 4],
            pending: 0,
            literal: vec![PROB_INIT; 0x300 << (lc + lp)],
            is_match: [PROB_INIT; STATES * POS_STATES_MAX],
            is_rep: [PROB_INIT; STATES],
            is_rep_g0: [PROB_INIT; STATES],
            is_rep_g1: [PROB_INIT; STATES],
            is_rep_g2: [PROB_INIT; STATES],
            is_rep0_long: [PROB_INIT; STATES * POS_STATES_MAX],
            pos_slot: [[PROB_INIT; 64]; 4],
            pos_decoders: [PROB_INIT; 1 + FULL_DISTANCES - END_POS_MODEL as usize],
            align: [PROB_INIT; 16],
            len: LenDecoder::new(),
            rep_len: LenDecoder::new(),
        })
    }

    /// The byte `dist` positions back (1 = the last one).
    #[inline]
    fn back(&self, dist: usize) -> u8 {
        let i = if dist <= self.pos { self.pos - dist } else { self.dict.len() + self.pos - dist };
        self.dict[i]
    }

    #[inline]
    fn put(&mut self, b: u8, out: &mut [u8], n: &mut usize) {
        self.dict[self.pos] = b;
        self.pos += 1;
        if self.pos == self.dict.len() {
            self.pos = 0;
            self.full = true;
        }
        self.total += 1;
        out[*n] = b;
        *n += 1;
    }

    fn check_dist(&self, dist: u32) -> Result<()> {
        let avail = if self.full { self.dict.len() as u64 } else { self.pos as u64 };
        if dist as u64 >= avail {
            return Err(Error::Corrupt("LZMA match distance beyond the data"));
        }
        Ok(())
    }

    fn distance(&mut self, len: u32) -> Result<u32> {
        let len_state = len.min(3) as usize;
        let slot = self.rc.tree(&mut self.pos_slot[len_state], 6)?;
        if slot < 4 {
            return Ok(slot);
        }
        let direct = (slot >> 1) - 1;
        let mut dist = (2 | (slot & 1)) << direct;
        if slot < END_POS_MODEL {
            let base = (dist - slot) as usize;
            dist += self.rc.reverse_tree(&mut self.pos_decoders[base..], direct)?;
        } else {
            dist = dist.wrapping_add(self.rc.direct(direct - ALIGN_BITS)? << ALIGN_BITS);
            dist = dist.wrapping_add(self.rc.reverse_tree(&mut self.align, ALIGN_BITS)?);
        }
        Ok(dist)
    }
}

impl Stream for Lzma {
    fn read(&mut self, out: &mut [u8]) -> Result<usize> {
        let mut n = 0;
        while n < out.len() && self.total < self.size {
            if self.pending > 0 {
                let b = self.back(self.reps[0] as usize + 1);
                self.put(b, out, &mut n);
                self.pending -= 1;
                continue;
            }
            let pos_state = (self.total as usize) & ((1 << self.pb) - 1);
            let s = self.state;
            if self.rc.bit(&mut self.is_match[(s << 4) + pos_state])? == 0 {
                let prev = if self.total > 0 { self.back(1) } else { 0 };
                let lit_state = (((self.total as usize) & ((1 << self.lp) - 1)) << self.lc) + ((prev as usize) >> (8 - self.lc));
                let base = 0x300 * lit_state;
                let mut sym = 1usize;
                if s >= 7 {
                    let mut match_byte = self.back(self.reps[0] as usize + 1) as usize;
                    while sym < 0x100 {
                        let match_bit = (match_byte >> 7) & 1;
                        match_byte <<= 1;
                        let bit = self.rc.bit(&mut self.literal[base + ((1 + match_bit) << 8) + sym])? as usize;
                        sym = (sym << 1) | bit;
                        if match_bit != bit {
                            break;
                        }
                    }
                }
                while sym < 0x100 {
                    sym = (sym << 1) | self.rc.bit(&mut self.literal[base + sym])? as usize;
                }
                self.put((sym - 0x100) as u8, out, &mut n);
                self.state = if s < 4 { 0 } else if s < 10 { s - 3 } else { s - 6 };
                continue;
            }
            let len;
            if self.rc.bit(&mut self.is_rep[s])? != 0 {
                if self.total == 0 {
                    return Err(Error::Corrupt("LZMA repeat before any data"));
                }
                if self.rc.bit(&mut self.is_rep_g0[s])? == 0 {
                    if self.rc.bit(&mut self.is_rep0_long[(s << 4) + pos_state])? == 0 {
                        self.check_dist(self.reps[0])?;
                        self.state = if s < 7 { 9 } else { 11 };
                        let b = self.back(self.reps[0] as usize + 1);
                        self.put(b, out, &mut n);
                        continue;
                    }
                } else {
                    let dist;
                    if self.rc.bit(&mut self.is_rep_g1[s])? == 0 {
                        dist = self.reps[1];
                    } else {
                        if self.rc.bit(&mut self.is_rep_g2[s])? == 0 {
                            dist = self.reps[2];
                        } else {
                            dist = self.reps[3];
                            self.reps[3] = self.reps[2];
                        }
                        self.reps[2] = self.reps[1];
                    }
                    self.reps[1] = self.reps[0];
                    self.reps[0] = dist;
                }
                len = self.rep_len.decode(&mut self.rc, pos_state)?;
                self.state = if s < 7 { 8 } else { 11 };
            } else {
                self.reps[3] = self.reps[2];
                self.reps[2] = self.reps[1];
                self.reps[1] = self.reps[0];
                len = self.len.decode(&mut self.rc, pos_state)?;
                self.state = if s < 7 { 7 } else { 10 };
                let dist = self.distance(len)?;
                if dist == 0xFFFF_FFFF {
                    // End marker.
                    if self.total != self.size {
                        return Err(Error::Corrupt("LZMA end marker before the declared size"));
                    }
                    break;
                }
                self.reps[0] = dist;
            }
            self.check_dist(self.reps[0])?;
            self.pending = len + 2;
        }
        Ok(n)
    }
}
