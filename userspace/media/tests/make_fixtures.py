"""Writes the audio test files in tests/data and their reference decodings.

Run with a Python that has `soundfile` (libsndfile 1.2 or newer, which brings
LAME, mpg123, libvorbis and libFLAC) and `numpy`:

    python userspace/media/tests/make_fixtures.py

Every `<name>.<ext>` gets a `<name>.ref.gz`: what libsndfile decodes from the
file, as gzip-compressed signed 16-bit (rounded from floating point) little-endian interleaved samples. The
tests compare the OSyx decoders against those references.
"""

import gzip
import os

import numpy as np
import soundfile as sf

HERE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "data")


def signal(rate, channels, seconds=0.25):
    """Tones, a chirp, a click and a little noise: exercises long and short blocks."""
    t = np.arange(int(rate * seconds)) / rate
    rng = np.random.default_rng(7)
    out = []
    for c in range(channels):
        s = 0.35 * np.sin(2 * np.pi * (220 + 110 * c) * t)
        s += 0.2 * np.sin(2 * np.pi * (300 + 2400 * t / seconds) * t)
        s += 0.05 * rng.standard_normal(len(t))
        click = int(len(t) * 0.6)
        s[click:click + 40] += 0.5
        s[: int(rate * 0.01)] *= np.linspace(0, 1, int(rate * 0.01))
        out.append(np.clip(s, -0.99, 0.99))
    return np.stack(out, axis=1)


def write(name, ext, rate, channels, fmt, subtype, **kw):
    path = os.path.join(HERE, f"{name}.{ext}")
    sf.write(path, signal(rate, channels), rate, format=fmt, subtype=subtype, **kw)
    # Read as floating point: libsndfile does not scale floats it hands
    # out as integers.
    data, r = sf.read(path, dtype="float64", always_2d=True)
    assert r == rate, (name, r)
    data = np.clip(np.round(data * 32768.0), -32768, 32767)
    with open(os.path.join(HERE, f"{name}.ref.gz"), "wb") as f:
        f.write(gzip.compress(data.astype("<i2").tobytes(), mtime=0))
    print(f"{name}.{ext}: {os.path.getsize(path)} bytes, {data.shape[0]} frames x {data.shape[1]}")


def main():
    os.makedirs(HERE, exist_ok=True)
    write("pcm16-stereo-44100", "wav", 44100, 2, "WAV", "PCM_16")
    write("pcm8-mono-8000", "wav", 8000, 1, "WAV", "PCM_U8")
    write("pcm24-stereo-48000", "wav", 48000, 2, "WAVEX", "PCM_24")
    write("float-mono-22050", "wav", 22050, 1, "WAV", "FLOAT")
    write("ulaw-mono-8000", "wav", 8000, 1, "WAV", "ULAW")
    write("alaw-mono-8000", "wav", 8000, 1, "WAV", "ALAW")
    write("ima-adpcm-stereo-22050", "wav", 22050, 2, "WAV", "IMA_ADPCM")
    write("ms-adpcm-mono-11025", "wav", 11025, 1, "WAV", "MS_ADPCM")
    write("flac16-stereo-44100", "flac", 44100, 2, "FLAC", "PCM_16")
    write("flac24-mono-96000", "flac", 96000, 1, "FLAC", "PCM_24")
    write("flac8-stereo-8000", "flac", 8000, 2, "FLAC", "PCM_S8")
    write("mp3-stereo-44100", "mp3", 44100, 2, "MP3", "MPEG_LAYER_III")
    write("mp3-mono-22050", "mp3", 22050, 1, "MP3", "MPEG_LAYER_III")
    write("mp3-mono-8000", "mp3", 8000, 1, "MP3", "MPEG_LAYER_III")
    write("vorbis-stereo-44100", "ogg", 44100, 2, "OGG", "VORBIS")
    write("vorbis-mono-22050", "ogg", 22050, 1, "OGG", "VORBIS")


if __name__ == "__main__":
    main()
