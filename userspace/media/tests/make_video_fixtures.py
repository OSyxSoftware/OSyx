"""Writes VP8 and WebM test files with libvpx, libopus and libvorbis
(through PyAV), and FFmpeg's own decodings of the video, for
tests/video.rs:

    python userspace/media/tests/make_video_fixtures.py

NAME.ivf (or NAME.webm) gets NAME.yuv.gz: gzip of width, height and frame
count (u32 little-endian each), then each shown frame's I420 planes.
"""

import gzip
import io
import os
import struct

import av
import numpy as np

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "data", "video")


def scene(w, h, n):
    """A moving picture: a drifting gradient, a bouncing box and noise."""
    rng = np.random.default_rng(7)
    y, x = np.mgrid[0:h, 0:w]
    for i in range(n):
        r = (np.sin((x + 3 * i) / 11) * 70 + 128).astype(np.int32)
        g = ((x + y + 5 * i) % 256).astype(np.int32)
        b = (np.cos((y - 2 * i) / 7) * 60 + 120).astype(np.int32)
        bx, by = (7 * i) % (w - 20), (5 * i) % (h - 16)
        r[by:by + 16, bx:bx + 20] = 250
        g[by:by + 16, bx:bx + 20] = 30
        noise = rng.integers(-8, 8, (h, w))
        img = np.stack([r + noise, g, b - noise], -1).clip(0, 255).astype(np.uint8)
        yield av.VideoFrame.from_ndarray(img, format="rgb24")


def encode_ivf(name, w, h, n, **opts):
    buf = io.BytesIO()
    out = av.open(buf, "w", format="ivf")
    st = out.add_stream("libvpx", rate=25)
    st.width, st.height, st.pix_fmt = w, h, "yuv420p"
    st.options = {k: str(v) for k, v in opts.items()}
    for f in scene(w, h, n):
        for p in st.encode(f.reformat(format="yuv420p")):
            out.mux(p)
    for p in st.encode():
        out.mux(p)
    out.close()
    data = buf.getvalue()
    with open(os.path.join(OUT, name + ".ivf"), "wb") as f:
        f.write(data)
    return data


def reference(name, data, fmt):
    inp = av.open(io.BytesIO(data), format=fmt)
    vs = inp.streams.video[0]
    vs.codec_context.options = {}
    frames = []
    for fr in inp.decode(vs):
        w, h = fr.width, fr.height
        planes = [np.frombuffer(bytes(p), np.uint8).reshape(-1, p.line_size) for p in fr.planes]
        cw, ch = (w + 1) // 2, (h + 1) // 2
        frames.append(planes[0][:h, :w].tobytes() + planes[1][:ch, :cw].tobytes() + planes[2][:ch, :cw].tobytes())
    ref = struct.pack("<3I", w, h, len(frames)) + b"".join(frames)
    with open(os.path.join(OUT, name + ".yuv.gz"), "wb") as f:
        f.write(gzip.compress(ref, 9, mtime=0))
    print(name, w, h, len(frames), len(data))


def main():
    os.makedirs(OUT, exist_ok=True)
    # Golden and alternate reference frames, several token partitions.
    d = encode_ivf("vp8-altref", 96, 64, 30, **{"auto-alt-ref": 1, "lag-in-frames": 16, "arnr-maxframes": 5,
                                                   "b": "300k", "token-partitions": 2, "g": 12})
    reference("vp8-altref", d, "ivf")
    # Odd size, low bitrate (split MVs, segment noise), error resilience.
    d = encode_ivf("vp8-odd", 73, 45, 20, b="40k", **{"error-resilient": 1, "cpu-used": 0, "noise-sensitivity": 1})
    reference("vp8-odd", d, "ivf")
    # High quality, sharpness and static threshold.
    d = encode_ivf("vp8-hq", 64, 48, 12, crf=4, b="2M", sharpness=5, **{"static-thresh": 100})
    reference("vp8-hq", d, "ivf")


main()
