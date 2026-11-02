"""Writes the picture test files in tests/data/image and their reference
decodings (Pillow), as gzip-compressed RGBA rows of the first frame, plus
the frame count and size in the file name of a .ref.gz:

    python userspace/media/tests/make_image_fixtures.py
"""

import gzip
import io
import os

import numpy as np
from PIL import Image

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "data", "image")


def base(w=37, h=23):
    y, x = np.mgrid[0:h, 0:w]
    r = (x * 255 // (w - 1)).astype(np.uint8)
    g = (y * 255 // (h - 1)).astype(np.uint8)
    b = ((x + y) * 7 % 256).astype(np.uint8)
    a = np.where((x - w // 2) ** 2 + (y - h // 2) ** 2 < (h // 2) ** 2, 255, (x * 9) % 256).astype(np.uint8)
    return np.stack([r, g, b, a], -1)


def save(name, img, **kw):
    img.save(os.path.join(OUT, name), **kw)
    reference(name)


def reference(name):
    path = os.path.join(OUT, name)
    ref = Image.open(path)
    frames = getattr(ref, "n_frames", 1)
    ref.seek(0)
    rgba = np.asarray(ref.convert("RGBA"))
    with open(path + ".ref.gz", "wb") as f:
        head = np.array([rgba.shape[1], rgba.shape[0], frames], "<u4").tobytes()
        f.write(gzip.compress(head + rgba.tobytes(), mtime=0))
    print(name, os.path.getsize(path), rgba.shape, frames)


def write_png(name, arr, color, interlace):
    """A PNG with filter type 4 (Paeth) rows, 8 or 16 bits per sample."""
    import struct
    import zlib

    h, w, c = arr.shape
    depth = 16 if arr.dtype == np.uint16 else 8
    be = arr.astype(">u2").tobytes() if depth == 16 else arr.astype(np.uint8).tobytes()
    bpp = c * depth // 8
    rows = np.frombuffer(be, np.uint8).reshape(h, w * bpp)

    def filtered(img):
        out = bytearray()
        prev = np.zeros(img.shape[1], np.int32)
        for row in img.astype(np.int32):
            out.append(4)
            for i in range(len(row)):
                a = row[i - bpp] if i >= bpp else 0
                b = prev[i]
                cc = prev[i - bpp] if i >= bpp else 0
                p = a + b - cc
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - cc)
                pred = a if pa <= pb and pa <= pc else (b if pb <= pc else cc)
                out.append((row[i] - pred) & 0xFF)
            prev = row
        return bytes(out)

    raw = b""
    if interlace:
        for x0, y0, dx, dy in [(0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2)]:
            sub = arr[y0::dy, x0::dx]
            if sub.size == 0:
                continue
            sb = sub.astype(">u2").tobytes() if depth == 16 else sub.astype(np.uint8).tobytes()
            raw += filtered(np.frombuffer(sb, np.uint8).reshape(sub.shape[0], sub.shape[1] * bpp))
    else:
        raw = filtered(rows)

    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)

    png = bytes([0x89]) + b"PNG" + bytes([13, 10, 26, 10]) + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, depth, color, 0, 0, 1 if interlace else 0))
    png += chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b"")
    path = os.path.join(OUT, name)
    open(path, "wb").write(png)
    ref = Image.open(path)
    rgba = np.asarray(ref.convert("RGBA")) if depth == 8 else None
    if depth == 16:
        a8 = (arr >> 8).astype(np.uint8)
        if c == 1:
            a8 = np.concatenate([a8, a8, a8, np.full_like(a8, 255)], -1)
        rgba = a8
    with open(path + ".ref.gz", "wb") as f:
        f.write(gzip.compress(np.array([w, h, 1], "<u4").tobytes() + np.ascontiguousarray(rgba).tobytes(), mtime=0))
    print(name, len(png), rgba.shape)


def main():
    os.makedirs(OUT, exist_ok=True)
    rgba = Image.fromarray(base(), "RGBA")
    save("rgba8.png", rgba)
    save("rgb8.png", rgba.convert("RGB"))
    save("gray8.png", rgba.convert("L"))
    save("graya8.png", rgba.convert("LA"))
    save("gray1.png", rgba.convert("1"))
    save("pal4.png", rgba.convert("RGB").quantize(16), bits=4)
    pal = rgba.quantize(64)
    save("pal-trns.png", pal, transparency=3)
    # Pillow writes neither 16-bit colour nor Adam7: write those by hand.
    write_png("rgba16-adam7.png", (base().astype(np.uint16) * 257 + 3), 6, interlace=True)
    write_png("gray16.png", (base()[..., 0].astype(np.uint16) * 257 + 5)[..., None], 0, interlace=False)
    write_png("rgb8-adam7.png", base()[..., :3], 2, interlace=True)
    save("rgb24.bmp", rgba.convert("RGB"))
    save("pal8.bmp", rgba.convert("RGB").quantize(200))
    save("mono.bmp", rgba.convert("1"))
    save("photo.jpg", rgba.convert("RGB"), quality=92)
    save("gray.jpg", rgba.convert("L"), quality=92)
    icon = rgba.resize((32, 32))
    save("icon-png.ico", icon, sizes=[(32, 32)])
    save("icon-bmp.ico", icon, sizes=[(32, 32)], bitmap_format="bmp")
    frames = [Image.fromarray(np.roll(base(), k * 5, axis=1), "RGBA").convert("RGB").quantize(32) for k in range(4)]
    frames[0].save(os.path.join(OUT, "anim.gif"), save_all=True, append_images=frames[1:], duration=[100, 200, 300, 400], loop=0)
    reference("anim.gif")
    save("interlaced.gif", rgba.convert("RGB").quantize(64), interlace=True)


if __name__ == "__main__":
    main()
