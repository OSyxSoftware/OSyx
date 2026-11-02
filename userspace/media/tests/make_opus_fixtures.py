"""Writes the Ogg Opus test files in tests/data and their reference decodings.

Run with a Python that has PyAV (FFmpeg 7 with libopus) and numpy:

    python userspace/media/tests/make_opus_fixtures.py

Files are encoded with libopus and decoded again with libopus (not
FFmpeg's own Opus decoder; its 16-bit output, so with soft clipping), with
pre-skip and end trimming applied, into
`<name>.ref.gz`: gzip-compressed signed 16-bit little-endian interleaved
samples at 48 kHz, like `make_fixtures.py`.
"""

import gzip
import os

import av
import numpy as np

HERE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "data")
LAYOUTS = {1: "mono", 2: "stereo", 6: "5.1"}


def signal(rate, channels, seconds=0.6):
    """Speech-like bursts, tones and a chirp: lets the encoder pick SILK,
    hybrid and CELT frames."""
    t = np.arange(int(rate * seconds)) / rate
    rng = np.random.default_rng(11)
    out = []
    for c in range(channels):
        f0 = 140 + 30 * c
        voice = sum(np.sin(2 * np.pi * f0 * k * t) / k for k in range(1, 12))
        env = 0.5 + 0.5 * np.sin(2 * np.pi * 3 * t)
        s = 0.12 * voice * env
        s += 0.15 * np.sin(2 * np.pi * (500 + 4000 * t / seconds) * t)
        s += 0.03 * rng.standard_normal(len(t))
        s[int(len(t) * 0.7):int(len(t) * 0.7) + 60] += 0.4
        out.append(np.clip(s, -0.99, 0.99))
    return np.stack(out).astype(np.float32)


def write(name, rate, channels, bit_rate, options):
    path = os.path.join(HERE, f"{name}.opus")
    out = av.open(path, "w", format="ogg")
    st = out.add_stream("libopus", rate=rate, layout=LAYOUTS[channels])
    st.bit_rate = bit_rate
    st.codec_context.options = options
    pcm = signal(rate, channels)
    n = st.codec_context.frame_size or 960
    for i in range(0, pcm.shape[1], n):
        chunk = np.ascontiguousarray(pcm[:, i:i + n])
        f = av.AudioFrame.from_ndarray(chunk, format="fltp", layout=LAYOUTS[channels])
        f.rate = rate
        f.pts = i
        for p in st.encode(f):
            out.mux(p)
    for p in st.encode(None):
        out.mux(p)
    out.close()

    inp = av.open(path)
    s = inp.streams.audio[0]
    dec = av.CodecContext.create("libopus", "r")
    dec.extradata = s.codec_context.extradata
    blocks = []

    def take(fr):
        x = fr.to_ndarray()
        # FFmpeg's libopus wrapper hands out 16-bit samples (opus_decode).
        blocks.append(x / 32768.0 if x.dtype.kind == "i" else x.astype(np.float64))

    for pkt in inp.demux(s):
        if pkt.size == 0:
            continue
        for fr in dec.decode(pkt):
            take(fr)
    for fr in dec.decode(None):
        take(fr)
    inp.close()
    assert dec.sample_rate == 48000, (name, dec.sample_rate)
    data = np.concatenate([b if b.shape[0] == channels else b.reshape(channels, -1, order="F") for b in blocks], axis=1).T
    data = np.clip(np.round(data * 32768.0), -32768, 32767)
    with open(os.path.join(HERE, f"{name}.ref.gz"), "wb") as f:
        f.write(gzip.compress(data.astype("<i2").tobytes(), mtime=0))
    print(f"{name}.opus: {os.path.getsize(path)} bytes, {data.shape[0]} frames x {data.shape[1]}")


def main():
    os.makedirs(HERE, exist_ok=True)
    # Voice at a low rate: SILK and hybrid frames, 20 ms.
    write("opus-mono-voip", 48000, 1, 12000, {"application": "voip"})
    # Music: CELT, stereo, 20 ms frames.
    write("opus-stereo-audio", 48000, 2, 96000, {"application": "audio"})
    # Low-delay CELT with 2.5 ms frames.
    write("opus-stereo-lowdelay", 48000, 2, 64000, {"application": "lowdelay", "frame_duration": "2.5"})
    # 40 ms packets of SILK at 16 kHz input.
    write("opus-mono-16k-40ms", 16000, 1, 16000, {"application": "voip", "frame_duration": "40"})
    # 5.1: channel mapping family 1 with four streams.
    write("opus-5.1", 48000, 6, 192000, {"application": "audio", "mapping_family": "1"})


if __name__ == "__main__":
    main()
