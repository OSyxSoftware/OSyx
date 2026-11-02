//! Conversion of decoded samples to the sound card's format: 48 kHz,
//! stereo, signed 16-bit (`/dev/audio`). Mono is copied to both sides,
//! more channels are folded down with the ITU-R BS.775 coefficients, and
//! other rates go through a windowed-sinc polyphase resampler.

use alloc::vec;
use alloc::vec::Vec;

/// The rate `/dev/audio` plays.
pub const OUT_RATE: u32 = 48_000;
/// Zero crossings of the sinc on each side of the centre.
const HALF: usize = 16;
const TAPS: usize = 2 * HALF;
/// Filter phases between two input samples (interpolated in between).
const PHASES: usize = 256;

pub struct Converter {
    in_rate: u32,
    channels: usize,
    /// Input frames per output frame.
    step: f64,
    /// Position of the next output frame in `buf`.
    pos: f64,
    /// Stereo input waiting to be resampled, including `HALF - 1` frames of
    /// history before `pos`.
    buf: Vec<[f32; 2]>,
    /// `(PHASES + 1) * TAPS` filter coefficients.
    table: Vec<f32>,
    /// Linear gain applied before conversion to 16 bits.
    pub gain: f32,
}

impl Converter {
    pub fn new(in_rate: u32, channels: usize) -> Converter {
        let mut c = Converter {
            in_rate,
            channels: channels.max(1),
            step: in_rate as f64 / OUT_RATE as f64,
            pos: 0.0,
            buf: Vec::new(),
            table: Vec::new(),
            gain: 1.0,
        };
        if in_rate != OUT_RATE {
            // Below the lower Nyquist frequency, with a little room for the
            // transition band.
            let cutoff = (OUT_RATE as f64 / in_rate as f64).min(1.0) * 0.94;
            c.table = vec![0.0; (PHASES + 1) * TAPS];
            for p in 0..=PHASES {
                let frac = p as f64 / PHASES as f64;
                let mut sum = 0.0;
                for j in 0..TAPS {
                    let d = frac + (HALF as f64 - 1.0) - j as f64;
                    let v = kernel(d, cutoff);
                    c.table[p * TAPS + j] = v as f32;
                    sum += v;
                }
                // Unity gain at DC for every phase.
                for j in 0..TAPS {
                    c.table[p * TAPS + j] /= sum as f32;
                }
            }
        }
        c.reset();
        c
    }

    /// Forgets buffered input (after a seek).
    pub fn reset(&mut self) {
        self.buf.clear();
        if self.in_rate != OUT_RATE {
            self.buf.resize(HALF - 1, [0.0; 2]);
            self.pos = (HALF - 1) as f64;
        } else {
            self.pos = 0.0;
        }
    }

    /// Converts a block of interleaved samples; returns interleaved stereo
    /// 16-bit samples (some input may stay buffered until the next call).
    pub fn push(&mut self, block: &[f32]) -> Vec<i16> {
        self.downmix(block);
        self.run()
    }

    /// Flushes what is buffered at the end of the stream.
    pub fn finish(&mut self) -> Vec<i16> {
        if self.in_rate == OUT_RATE {
            return Vec::new();
        }
        self.buf.extend(core::iter::repeat_n([0.0; 2], HALF));
        let out = self.run();
        self.reset();
        out
    }

    fn downmix(&mut self, block: &[f32]) {
        let ch = self.channels;
        for f in block.chunks_exact(ch) {
            let frame = match ch {
                1 => [f[0], f[0]],
                2 => [f[0], f[1]],
                _ => {
                    let w = downmix_weights(ch);
                    let (mut l, mut r) = (0.0, 0.0);
                    for (x, (wl, wr)) in f.iter().zip(w) {
                        l += x * wl;
                        r += x * wr;
                    }
                    let norm = 1.0 / w.iter().map(|p| p.0).sum::<f32>();
                    [l * norm, r * norm]
                }
            };
            self.buf.push(frame);
        }
    }

    fn run(&mut self) -> Vec<i16> {
        let to16 = |v: f32, g: f32| -> i16 {
            let s = libm::roundf(v * g * 32768.0);
            s.clamp(-32768.0, 32767.0) as i16
        };
        let g = self.gain;
        if self.in_rate == OUT_RATE {
            let out = self.buf.iter().flat_map(|f| [to16(f[0], g), to16(f[1], g)]).collect();
            self.buf.clear();
            return out;
        }
        let mut out = Vec::new();
        loop {
            let i0 = self.pos as usize;
            if i0 + HALF >= self.buf.len() {
                break;
            }
            let frac = self.pos - i0 as f64;
            let ph = frac * PHASES as f64;
            let p = (ph as usize).min(PHASES - 1);
            let t = (ph - p as f64) as f32;
            let (a, b) = (&self.table[p * TAPS..(p + 1) * TAPS], &self.table[(p + 1) * TAPS..(p + 2) * TAPS]);
            let base = i0 + 1 - HALF;
            let (mut l, mut r) = (0.0f32, 0.0f32);
            for j in 0..TAPS {
                let Some(x) = self.buf.get(base + j) else { break };
                let h = a[j] + (b[j] - a[j]) * t;
                l += x[0] * h;
                r += x[1] * h;
            }
            out.push(to16(l, g));
            out.push(to16(r, g));
            self.pos += self.step;
        }
        // Keep the history the next output frames need.
        let keep_from = (self.pos as usize).saturating_sub(HALF - 1).min(self.buf.len());
        if keep_from > 0 {
            self.buf.drain(..keep_from);
            self.pos -= keep_from as f64;
        }
        out
    }
}

/// (left, right) weights of each input channel, ITU-R BS.775 style, for
/// the layouts the decoders produce by channel count (WAVE and FLAC order;
/// Vorbis is reordered to match): 3 FL FR FC, 4 FL FR BL BR, 5 FL FR FC BL
/// BR, 6 FL FR FC LFE BL BR, 7 FL FR FC LFE BC SL SR, 8 FL FR FC LFE BL BR
/// SL SR. The LFE is left out.
fn downmix_weights(ch: usize) -> &'static [(f32, f32)] {
    const K: f32 = core::f32::consts::FRAC_1_SQRT_2;
    match ch {
        3 => &[(1.0, 0.0), (0.0, 1.0), (K, K)],
        4 => &[(1.0, 0.0), (0.0, 1.0), (K, 0.0), (0.0, K)],
        5 => &[(1.0, 0.0), (0.0, 1.0), (K, K), (K, 0.0), (0.0, K)],
        6 => &[(1.0, 0.0), (0.0, 1.0), (K, K), (0.0, 0.0), (K, 0.0), (0.0, K)],
        7 => &[(1.0, 0.0), (0.0, 1.0), (K, K), (0.0, 0.0), (0.5, 0.5), (K, 0.0), (0.0, K)],
        _ => &[(1.0, 0.0), (0.0, 1.0), (K, K), (0.0, 0.0), (K, 0.0), (0.0, K), (K, 0.0), (0.0, K)],
    }
}

/// Blackman-windowed sinc with cutoff `fc` (relative to the input
/// Nyquist frequency), at distance `x` input samples from the centre.
fn kernel(x: f64, fc: f64) -> f64 {
    use core::f64::consts::PI;
    if x.abs() >= HALF as f64 {
        return 0.0;
    }
    let s = if x.abs() < 1e-9 { fc } else { libm::sin(PI * fc * x) / (PI * x) };
    let w = x / HALF as f64;
    let window = 0.42 + 0.5 * libm::cos(PI * w) + 0.08 * libm::cos(2.0 * PI * w);
    s * window
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmix_keeps_sides_apart() {
        // Quad: back left only reaches the left side.
        let mut c = Converter::new(48_000, 4);
        let out = c.push(&[0.0, 0.0, 0.5, 0.0]);
        assert!(out[0] > 0 && out[1] == 0, "{out:?}");
        // 5 channels: back right only reaches the right side.
        let mut c = Converter::new(48_000, 5);
        let out = c.push(&[0.0, 0.0, 0.0, 0.0, 0.5]);
        assert!(out[0] == 0 && out[1] > 0, "{out:?}");
    }

    #[test]
    fn same_rate_is_exact() {
        let mut c = Converter::new(48_000, 2);
        let out = c.push(&[0.5, -0.5, 1.0, -1.0]);
        assert_eq!(out, [16384, -16384, 32767, -32768]);
        let mut m = Converter::new(48_000, 1);
        assert_eq!(m.push(&[0.25]), [8192, 8192]);
    }

    #[test]
    fn length_and_tone_survive_resampling() {
        for rate in [8_000u32, 22_050, 44_100, 96_000] {
            let mut c = Converter::new(rate, 1);
            let n = rate as usize / 2;
            let tone: Vec<f32> = (0..n)
                .map(|i| 0.5 * libm::sinf(2.0 * core::f32::consts::PI * 440.0 * i as f32 / rate as f32))
                .collect();
            let mut out = Vec::new();
            for chunk in tone.chunks(1000) {
                out.extend(c.push(chunk));
            }
            out.extend(c.finish());
            let frames = out.len() / 2;
            assert!(frames.abs_diff(24_000) <= 2, "{rate}: {frames} frames");
            // Compare with the ideal tone at 48 kHz away from the edges.
            let mut err = 0.0f64;
            let mut sig = 0.0f64;
            for i in 1000..frames - 1000 {
                let want = 0.5 * libm::sin(2.0 * core::f64::consts::PI * 440.0 * i as f64 / 48_000.0);
                let got = out[2 * i] as f64 / 32768.0;
                err += (got - want) * (got - want);
                sig += want * want;
            }
            let snr = 10.0 * libm::log10(sig / err);
            assert!(snr > 60.0, "{rate}: SNR {snr:.1} dB");
        }
    }
}
