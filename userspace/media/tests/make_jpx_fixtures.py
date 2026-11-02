"""Writes JPEG 2000 test files with OpenJPEG (through Pillow) and OpenJPEG's
own decodings of them, for tests/jpx.rs:

    python userspace/media/tests/make_jpx_fixtures.py

Each NAME.j2k or NAME.jp2 gets NAME.ref.gz: gzip of width, height and
channel count (u32 little-endian each), then the 8-bit samples (16-bit
pictures keep their high bytes).
"""

import gzip
import io
import os
import struct

import numpy as np
from PIL import Image, ImageDraw

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "data", "jpx", "gen")


def picture(w, h, mode):
    img = Image.new("RGB", (w, h), (240, 230, 200))
    d = ImageDraw.Draw(img)
    for i in range(0, w + h, 13):
        d.line((i, 0, i - h, h), fill=(i * 7 % 256, 40 + i % 200, 255 - i % 256), width=1 + i % 4)
    if w > 20 and h > 20:
        d.ellipse((w // 5, h // 6, w * 3 // 4, h * 5 // 6), outline=(10, 20, 160), width=5)
        d.rectangle((w // 2, h // 3, w - 7, h - 9), fill=(200, 30, 40))
        d.text((5, 5), "OSyx JPEG 2000", fill=(0, 0, 0))
    if mode == "RGBA":
        a = Image.linear_gradient("L").resize((w, h))
        img = img.convert("RGBA")
        img.putalpha(a)
        return img
    if mode == "I;16":
        g = np.asarray(img.convert("L"), dtype=np.uint16) * 257 + np.arange(w, dtype=np.uint16)[None, :] % 251
        return Image.frombytes("I;16", (w, h), g.astype("<u2").tobytes())
    return img.convert(mode)


CASES = [
    # name, size, mode, save options
    ("rev53-rgb", (157, 93), "RGB", dict()),
    ("irr97-rgb", (157, 93), "RGB", dict(irreversible=True, quality_mode="rates", quality_layers=[8])),
    ("irr97-rgb-layers", (160, 120), "RGB", dict(irreversible=True, quality_layers=[40, 15, 5])),
    ("irr97-gray-res1", (77, 41), "L", dict(irreversible=True, num_resolutions=1)),
    ("rev-gray-res8", (300, 211), "L", dict(num_resolutions=8)),
    ("rev-rgba-tiles", (203, 117), "RGBA", dict(tile_size=(64, 48))),
    ("rev-offsets", (131, 99), "RGB", dict(offset=(17, 9), tile_offset=(5, 3), tile_size=(40, 50))),
    ("irr-offsets-tiles", (131, 99), "RGB", dict(irreversible=True, offset=(3, 11), tile_offset=(1, 2), tile_size=(33, 29))),
    ("rev-precincts-rpcl", (190, 150), "RGB", dict(precinct_size=(32, 32), progression="RPCL", codeblock_size=(16, 16))),
    ("rev-precincts-pcrl", (190, 150), "RGB", dict(precinct_size=(64, 32), progression="PCRL", codeblock_size=(8, 32))),
    ("rev-precincts-cprl", (190, 150), "RGB", dict(precinct_size=(32, 64), progression="CPRL", codeblock_size=(4, 8))),
    ("rev-rlcp-layers", (190, 150), "RGB", dict(progression="RLCP", quality_layers=[30, 10, 1], quality_mode="rates")),
    ("rev-nomct", (100, 80), "RGB", dict(mct=0)),
    ("rev-gray16", (120, 90), "I;16", dict()),
    ("irr-gray16", (120, 90), "I;16", dict(irreversible=True)),
    ("rev-tiny", (1, 1), "RGB", dict()),
    ("rev-thin", (1, 37), "L", dict()),
    ("rev-wide", (53, 2), "L", dict()),
    ("rev-la", (64, 64), "LA", dict()),
]


def main():
    os.makedirs(OUT, exist_ok=True)
    for name, size, mode, opts in CASES:
        img = picture(*size, mode)
        for ext, no_jp2 in (("j2k", True), ("jp2", False)):
            if ext == "jp2" and name not in ("rev53-rgb", "rev-rgba-tiles", "rev-la", "rev-gray16"):
                continue
            buf = io.BytesIO()
            img.save(buf, "JPEG2000", no_jp2=no_jp2, **opts)
            data = buf.getvalue()
            back = Image.open(io.BytesIO(data))
            back.load()
            if back.mode == "I;16":
                arr = (np.asarray(back, dtype=np.uint16) >> 8).astype(np.uint8)[..., None]
            else:
                arr = np.asarray(back, dtype=np.uint8)
                if arr.ndim == 2:
                    arr = arr[..., None]
            h, w, n = arr.shape
            with open(os.path.join(OUT, f"{name}.{ext}"), "wb") as f:
                f.write(data)
            ref = struct.pack("<3I", w, h, n) + arr.tobytes()
            with open(os.path.join(OUT, f"{name}.{ext}.ref.gz"), "wb") as f:
                f.write(gzip.compress(ref, 9, mtime=0))
            print(name, ext, back.mode, w, h, n, len(data))


main()
