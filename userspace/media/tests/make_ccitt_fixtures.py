"""Writes CCITT fax test data with libtiff (through Pillow): the same
1-bit picture as Group 4, Group 3 1D and Group 3 2D strips, and the
expected rows (1 = a white run, packed MSB first):

    python userspace/media/tests/make_ccitt_fixtures.py
"""

import io
import os

import numpy as np
from PIL import Image, ImageDraw, TiffImagePlugin

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "data", "ccitt")


def picture():
    img = Image.new("1", (613, 211), 1)
    d = ImageDraw.Draw(img)
    for i in range(0, 600, 37):
        d.line((i, 0, 600 - i, 210), fill=0, width=1 + i % 5)
    d.ellipse((100, 40, 300, 180), outline=0, width=6)
    d.rectangle((400, 20, 600, 120), fill=0)
    d.text((410, 140), "OSyx CCITT fax 0123456789", fill=0)
    return img


def strip(img, compression, t4=None):
    buf = io.BytesIO()
    kw = {"compression": compression}
    if t4 is not None:
        info = TiffImagePlugin.ImageFileDirectory_v2()
        info[292] = t4
        kw["tiffinfo"] = info
    img.save(buf, "TIFF", **kw)
    t = Image.open(io.BytesIO(buf.getvalue()))
    off = t.tag_v2[273][0] if isinstance(t.tag_v2[273], tuple) else t.tag_v2[273]
    n = t.tag_v2[279][0] if isinstance(t.tag_v2[279], tuple) else t.tag_v2[279]
    return buf.getvalue()[off:off + n]


def main():
    os.makedirs(OUT, exist_ok=True)
    img = picture()
    # Pillow saves mode "1" as BlackIsZero, and libtiff codes 0 bits as
    # white runs: the white runs of the strips are the picture's black.
    bits = np.packbits(~np.asarray(img), axis=1)
    open(os.path.join(OUT, "ref.bin"), "wb").write(bits.tobytes())
    open(os.path.join(OUT, "size"), "wb").write(np.array([img.width, img.height], "<u4").tobytes())
    open(os.path.join(OUT, "g4.bin"), "wb").write(strip(img, "group4"))
    open(os.path.join(OUT, "g3.bin"), "wb").write(strip(img, "group3"))
    open(os.path.join(OUT, "g3-2d.bin"), "wb").write(strip(img, "group3", t4=1))
    for f in sorted(os.listdir(OUT)):
        print(f, os.path.getsize(os.path.join(OUT, f)))


if __name__ == "__main__":
    main()
