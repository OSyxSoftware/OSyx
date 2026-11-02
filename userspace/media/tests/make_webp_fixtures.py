"""Writes WebP test files with libwebp (through Pillow) and libwebp's own
RGBA decodings of them, for tests/webp.rs:

    python userspace/media/tests/make_webp_fixtures.py

Each NAME.webp gets NAME.ref.gz: gzip of width, height and frame count
(u32 little-endian each), then each frame's duration (u32) and RGBA.
"""

import gzip
import io
import os
import struct

import numpy as np
from PIL import Image, ImageDraw

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "data", "webp")


def picture(w, h, seed=0, alpha=False):
    rng = np.random.default_rng(seed)
    y, x = np.mgrid[0:h, 0:w]
    a = np.stack([
        (np.sin(x / 9 + seed) * 60 + 120 + y) % 256,
        (x * 3 + y * 2 + seed * 40) % 256,
        (np.cos((x + y) / 13) * 80 + 128) + rng.normal(0, 6, x.shape),
    ], -1).clip(0, 255).astype(np.uint8)
    img = Image.fromarray(a, "RGB")
    d = ImageDraw.Draw(img)
    if w > 16 and h > 16:
        d.rectangle((w // 4, h // 5, w // 2, h // 2), fill=(250, 20, 30))
        d.ellipse((w // 3, h // 3, w - 4, h - 3), outline=(10, 10, 10), width=3)
        d.text((3, 3), "OSyx WebP", fill=(255, 255, 255))
    if alpha:
        m = Image.new("L", (w, h), 0)
        md = ImageDraw.Draw(m)
        md.ellipse((2, 2, w - 3, h - 3), fill=255)
        md.rectangle((0, h // 2, w // 3, h - 1), fill=96)
        img = img.convert("RGBA")
        img.putalpha(m)
    return img


def palette_picture(w, h):
    img = Image.new("P", (w, h))
    img.putpalette([v for i in range(16) for v in (i * 16, 255 - i * 15, (i * 70) % 256)])
    d = ImageDraw.Draw(img)
    for i in range(16):
        d.rectangle((i * w // 16, 0, (i + 1) * w // 16, h), fill=i)
    d.ellipse((5, 5, w - 5, h - 5), fill=3)
    return img.convert("RGB")


def save(name, img, **opts):
    buf = io.BytesIO()
    img.save(buf, "WEBP", **opts)
    data = buf.getvalue()
    with open(os.path.join(OUT, name + ".webp"), "wb") as f:
        f.write(data)
    back = Image.open(io.BytesIO(data))
    frames = []
    for i in range(getattr(back, "n_frames", 1)):
        back.seek(i)
        back.load()
        frames.append((back.info.get("duration", 0) if getattr(back, "n_frames", 1) > 1 else 0,
                       np.asarray(back.convert("RGBA"), dtype=np.uint8)))
    h, w = frames[0][1].shape[:2]
    ref = struct.pack("<3I", w, h, len(frames))
    for dur, px in frames:
        ref += struct.pack("<I", int(dur)) + px.tobytes()
    with open(os.path.join(OUT, name + ".ref.gz"), "wb") as f:
        f.write(gzip.compress(ref, 9, mtime=0))
    print(name, w, h, len(frames), len(data))


def main():
    os.makedirs(OUT, exist_ok=True)
    save("lossy-q80", picture(160, 120), quality=80)
    save("lossy-q20-odd", picture(157, 93, 1), quality=20)
    save("lossy-q100-m6", picture(64, 48, 2), quality=100, method=6)
    save("lossy-tiny", picture(1, 1, 3), quality=75)
    save("lossy-thin", picture(3, 41, 4), quality=60)
    save("lossy-big", picture(320, 240, 5), quality=85, method=4)
    save("lossy-alpha", picture(130, 97, 6, alpha=True), quality=70)
    save("lossy-alpha-q50", picture(130, 97, 7, alpha=True), quality=50, alpha_quality=40, method=6)
    save("lossy-alpha-raw", picture(64, 64, 8, alpha=True), quality=70, alpha_quality=100, method=0)
    save("lossless", picture(160, 120, 9), lossless=True)
    save("lossless-m0", picture(97, 61, 10), lossless=True, method=0, quality=0)
    save("lossless-m6", picture(97, 61, 11), lossless=True, method=6, quality=100)
    save("lossless-alpha", picture(130, 97, 12, alpha=True), lossless=True, exact=True)
    save("lossless-palette", palette_picture(123, 45), lossless=True)
    save("lossless-two-colours", Image.new("RGB", (37, 11), (200, 10, 10)), lossless=True)
    frames = [picture(96, 64, s, alpha=(s % 2 == 0)) for s in range(4)]
    save("anim-lossy", frames[0], save_all=True, append_images=frames[1:], duration=[100, 80, 120, 50], loop=0, quality=70)
    save("anim-lossless", frames[0], save_all=True, append_images=frames[1:], duration=70, lossless=True)
    moving = []
    for i in range(5):
        img = Image.new("RGBA", (80, 60), (0, 0, 0, 0))
        d = ImageDraw.Draw(img)
        d.ellipse((i * 12, 10, i * 12 + 25, 35), fill=(255, 40 * i, 0, 255))
        d.rectangle((0, 50, 79, 59), fill=(30, 200, 60, 128))
        moving.append(img)
    save("anim-partial", moving[0], save_all=True, append_images=moving[1:], duration=40, lossless=True, minimize_size=True)
    save("anim-partial-lossy", moving[0], save_all=True, append_images=moving[1:], duration=40, quality=60, allow_mixed=True)


main()
