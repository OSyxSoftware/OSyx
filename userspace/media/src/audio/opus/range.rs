//! The range decoder of RFC 6716 section 4.1, with raw bits read from the
//! end of the frame.

/// Bits of fractional precision of `tell_frac`.
pub const BITRES: u32 = 3;

const SYM_BITS: u32 = 8;
const CODE_BITS: u32 = 32;
const SYM_MAX: u32 = (1 << SYM_BITS) - 1;
const CODE_TOP: u32 = 1 << (CODE_BITS - 1);
const CODE_BOT: u32 = CODE_TOP >> SYM_BITS;
const CODE_EXTRA: u32 = (CODE_BITS - 2) % SYM_BITS + 1;
const UINT_BITS: u32 = 8;
const WINDOW: u32 = 32;

/// Number of bits needed to hold `v` (0 for 0).
#[inline]
pub fn ilog(v: u32) -> u32 {
    32 - v.leading_zeros()
}

pub struct RangeDecoder<'a> {
    buf: &'a [u8],
    /// Bytes of `buf` that belong to this decoder.
    storage: usize,
    offs: usize,
    end_offs: usize,
    end_window: u32,
    nend_bits: u32,
    nbits_total: i32,
    rng: u32,
    val: u32,
    ext: u32,
    rem: u32,
    pub error: bool,
}

impl<'a> RangeDecoder<'a> {
    pub fn new(buf: &'a [u8]) -> RangeDecoder<'a> {
        let mut d = RangeDecoder {
            buf,
            storage: buf.len(),
            offs: 0,
            end_offs: 0,
            end_window: 0,
            nend_bits: 0,
            nbits_total: (CODE_BITS + 1 - ((CODE_BITS - CODE_EXTRA) / SYM_BITS) * SYM_BITS) as i32,
            rng: 1 << CODE_EXTRA,
            val: 0,
            ext: 0,
            rem: 0,
            error: false,
        };
        d.rem = d.read_byte();
        d.val = d.rng - 1 - (d.rem >> (SYM_BITS - CODE_EXTRA));
        d.normalize();
        d
    }

    /// The frame's size in bytes (raw bits are read back from its end).
    pub fn storage(&self) -> usize {
        self.storage
    }

    /// Shrinks the frame by `n` bytes at its end (CELT's redundancy).
    pub fn shrink(&mut self, n: usize) {
        self.storage -= n.min(self.storage);
    }

    /// The final range, which an encoder reports to check decoders.
    pub fn range(&self) -> u32 {
        self.rng
    }

    fn read_byte(&mut self) -> u32 {
        if self.offs < self.storage {
            self.offs += 1;
            self.buf[self.offs - 1] as u32
        } else {
            0
        }
    }

    fn read_byte_from_end(&mut self) -> u32 {
        if self.end_offs < self.storage {
            self.end_offs += 1;
            self.buf[self.storage - self.end_offs] as u32
        } else {
            0
        }
    }

    fn normalize(&mut self) {
        while self.rng <= CODE_BOT {
            self.nbits_total += SYM_BITS as i32;
            self.rng <<= SYM_BITS;
            let sym = self.rem;
            self.rem = self.read_byte();
            let sym = ((sym << SYM_BITS) | self.rem) >> (SYM_BITS - CODE_EXTRA);
            self.val = ((self.val << SYM_BITS).wrapping_add(SYM_MAX & !sym)) & (CODE_TOP - 1);
        }
    }

    pub fn decode(&mut self, ft: u32) -> u32 {
        self.ext = self.rng / ft;
        let s = self.val / self.ext;
        ft - (s + 1).min(ft)
    }

    pub fn decode_bin(&mut self, bits: u32) -> u32 {
        self.ext = self.rng >> bits;
        let s = self.val / self.ext;
        (1 << bits) - (s + 1).min(1 << bits)
    }

    pub fn update(&mut self, fl: u32, fh: u32, ft: u32) {
        let s = self.ext.wrapping_mul(ft - fh);
        self.val = self.val.wrapping_sub(s);
        self.rng = if fl > 0 { self.ext.wrapping_mul(fh - fl) } else { self.rng.wrapping_sub(s) };
        self.normalize();
    }

    /// A bit that is 1 with probability 1 / 2^logp.
    pub fn bit_logp(&mut self, logp: u32) -> bool {
        let r = self.rng;
        let d = self.val;
        let s = r >> logp;
        let ret = d < s;
        if !ret {
            self.val = d - s;
        }
        self.rng = if ret { s } else { r - s };
        self.normalize();
        ret
    }

    /// A symbol from an inverse cumulative table with total 2^ftb.
    pub fn icdf(&mut self, icdf: &[u8], ftb: u32) -> usize {
        let mut s = self.rng;
        let d = self.val;
        let r = s >> ftb;
        let mut ret = 0;
        let mut t;
        loop {
            t = s;
            s = r.wrapping_mul(icdf[ret] as u32);
            if d >= s {
                break;
            }
            ret += 1;
        }
        self.val = d - s;
        self.rng = t - s;
        self.normalize();
        ret
    }

    /// A uniform integer in 0..ft.
    pub fn uint(&mut self, ft: u32) -> u32 {
        let ft1 = ft - 1;
        let mut ftb = ilog(ft1);
        if ftb > UINT_BITS {
            ftb -= UINT_BITS;
            let f = (ft1 >> ftb) + 1;
            let s = self.decode(f);
            self.update(s, s + 1, f);
            let t = (s << ftb) | self.bits(ftb);
            if t <= ft1 {
                return t;
            }
            self.error = true;
            ft1
        } else {
            let s = self.decode(ft);
            self.update(s, s + 1, ft);
            s
        }
    }

    /// `bits` raw bits from the end of the frame.
    pub fn bits(&mut self, bits: u32) -> u32 {
        let mut window = self.end_window as u64;
        let mut available = self.nend_bits;
        if available < bits {
            loop {
                window |= (self.read_byte_from_end() as u64) << available;
                available += SYM_BITS;
                if available > WINDOW - SYM_BITS {
                    break;
                }
            }
        }
        let ret = (window as u32) & (((1u64 << bits) - 1) as u32);
        window >>= bits;
        available -= bits;
        self.end_window = window as u32;
        self.nend_bits = available;
        self.nbits_total += bits as i32;
        ret
    }

    /// Counts the bits up to `tell` as read (a silent CELT frame).
    pub fn skip_to(&mut self, tell: i32) {
        self.nbits_total += tell - self.tell();
    }

    /// Bits used so far, rounded up.
    pub fn tell(&self) -> i32 {
        self.nbits_total - ilog(self.rng) as i32
    }

    /// Bits used so far in 1/8 bits.
    pub fn tell_frac(&self) -> u32 {
        let nbits = (self.nbits_total as u32) << BITRES;
        let mut l = ilog(self.rng);
        let mut r = self.rng >> (l - 16);
        for _ in 0..BITRES {
            r = (r * r) >> 15;
            let b = r >> 16;
            l = (l << 1) | b;
            r >>= b;
        }
        nbits - l
    }

    /// A Laplace-distributed value (CELT coarse energy).
    pub fn laplace(&mut self, fs: u32, decay: u32) -> i32 {
        const MINP: u32 = 1;
        const NMIN: u32 = 16;
        let mut val = 0i32;
        let fm = self.decode_bin(15);
        let mut fl = 0;
        let mut fs = fs;
        if fm >= fs {
            val += 1;
            fl = fs;
            fs = (((32768 - MINP * 2 * NMIN - fs) * (16384 - decay)) >> 15) + MINP;
            while fs > MINP && fm >= fl + 2 * fs {
                fs *= 2;
                fl += fs;
                fs = ((fs - 2 * MINP) * decay) >> 15;
                fs += MINP;
                val += 1;
            }
            if fs <= MINP {
                let di = (fm - fl) >> 1;
                val += di as i32;
                fl += 2 * di * MINP;
            }
            if fm < fl + fs {
                val = -val;
            } else {
                fl += fs;
            }
        }
        self.update(fl, (fl + fs).min(32768), 32768);
        val
    }
}
