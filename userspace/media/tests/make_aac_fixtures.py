"""Writes the AAC test files in tests/data and their reference decodings.

Run with a Python that has PyAV (FFmpeg 7, which brings its native AAC
encoder and decoder) and numpy:

    python userspace/media/tests/make_aac_fixtures.py

Every `<name>.<ext>` gets a `<name>.ref.gz` with what FFmpeg decodes from the
file (through its demuxer, so MP4 edit lists apply), as gzip-compressed
signed 16-bit little-endian interleaved samples, like `make_fixtures.py`.
"""

import gzip
import os

import av
import numpy as np

HERE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "data")
LAYOUTS = {1: "mono", 2: "stereo", 6: "5.1"}


def signal(rate, channels, seconds=0.5):
    """Tones, a chirp, a click and a little noise: exercises long and short
    windows, and gives the encoder noisy bands to substitute."""
    t = np.arange(int(rate * seconds)) / rate
    rng = np.random.default_rng(7)
    out = []
    for c in range(channels):
        s = 0.3 * np.sin(2 * np.pi * (220 + 110 * c) * t)
        s += 0.15 * np.sin(2 * np.pi * (300 + 2400 * t / seconds) * t)
        s += 0.08 * rng.standard_normal(len(t))
        click = int(len(t) * (0.4 + 0.05 * c))
        s[click:click + 40] += 0.5
        s[: int(rate * 0.01)] *= np.linspace(0, 1, int(rate * 0.01))
        out.append(np.clip(s, -0.99, 0.99))
    return np.stack(out).astype(np.float32)


def write(name, ext, fmt, rate, channels, bit_rate, options=None, container_options=None):
    path = os.path.join(HERE, f"{name}.{ext}")
    out = av.open(path, "w", format=fmt, options=container_options or {})
    st = out.add_stream("aac", rate=rate, layout=LAYOUTS[channels])
    st.bit_rate = bit_rate
    if options:
        st.codec_context.options = options
    pcm = signal(rate, channels)
    n = st.codec_context.frame_size or 1024
    for i in range(0, pcm.shape[1], n):
        f = av.AudioFrame.from_ndarray(np.ascontiguousarray(pcm[:, i:i + n]), format="fltp", layout=LAYOUTS[channels])
        f.rate = rate
        f.pts = i
        for p in st.encode(f):
            out.mux(p)
    for p in st.encode(None):
        out.mux(p)
    out.close()

    inp = av.open(path)
    s = inp.streams.audio[0]
    blocks = [f.to_ndarray().astype(np.float64) for f in inp.decode(s)]
    assert s.codec_context.sample_rate == rate, (name, s.codec_context.sample_rate)
    inp.close()
    data = np.concatenate(blocks, axis=1).T
    assert data.shape[1] == channels, (name, data.shape)
    data = np.clip(np.round(data * 32768.0), -32768, 32767)
    with open(os.path.join(HERE, f"{name}.ref.gz"), "wb") as f:
        f.write(gzip.compress(data.astype("<i2").tobytes(), mtime=0))
    print(f"{name}.{ext}: {os.path.getsize(path)} bytes, {data.shape[0]} frames x {data.shape[1]}")


def main():
    os.makedirs(HERE, exist_ok=True)
    # FFmpeg's defaults: TNS, intensity stereo, M/S and noise substitution.
    write("aac-stereo-44100", "m4a", "ipod", 44100, 2, 128000)
    write("aac-mono-22050", "aac", "adts", 22050, 1, 48000)
    # No noise substitution: seeking lands on exactly the same samples.
    write("aac-stereo-48000", "aac", "adts", 48000, 2, 96000, {"aac_pns": "0"})
    write("aac-5.1-48000", "m4a", "ipod", 48000, 6, 256000, {"aac_pns": "0"})
    write("aac-mono-8000", "mp4", "mp4", 8000, 1, 16000, None, {"movflags": "frag_keyframe+empty_moov"})


if __name__ == "__main__":
    main()
