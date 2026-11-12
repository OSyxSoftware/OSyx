"""Writes the TIFF test files in tests/data/image and their reference
decodings, in the format of make_image_fixtures.py (gzip of width, height,
frame count and the RGBA rows):

    python userspace/media/tests/make_tiff_fixtures.py

Pillow writes the fax-compressed, palette and CMYK files and decodes every
file it can read; tifffile (with imagecodecs) writes the layouts Pillow
cannot (big-endian, tiles, planar samples, predictors, 16 bits). Where
Pillow's own conversion to RGBA differs from what a viewer shows (16-bit
samples, associated alpha, orientation), the reference is computed from the
pixels that were written; the script checks the two agree wherever both
exist.
"""

import gzip
import os

import numpy as np
import tifffile
from PIL import Image

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "data", "image")
W, H = 37, 23


def base():
    y, x = np.mgrid[0:H, 0:W]
    r = (x * 255 // (W - 1)).astype(np.uint8)
    g = (y * 255 // (H - 1)).astype(np.uint8)
    b = ((x + y) * 7 % 256).astype(np.uint8)
    a = np.where((x - W // 2) ** 2 + (y - H // 2) ** 2 < (H // 2) ** 2, 255, (x * 9) % 256).astype(np.uint8)
    return np.stack([r, g, b, a], -1)


def write_ref(name, rgba, pages=1):
    rgba = np.ascontiguousarray(rgba, np.uint8)
    with open(os.path.join(OUT, name + ".ref.gz"), "wb") as f:
        head = np.array([rgba.shape[1], rgba.shape[0], pages], "<u4").tobytes()
        f.write(gzip.compress(head + rgba.tobytes(), mtime=0))
    print(f"{name:28s} {os.path.getsize(os.path.join(OUT, name)):6d} bytes {rgba.shape}")


def pillow_rgba(name):
    return np.asarray(Image.open(os.path.join(OUT, name)).convert("RGBA"))


def emit(name, expected=None, check=True):
    """Reference for `name`: `expected` if given (and equal to Pillow's
    decoding when `check`), else Pillow's decoding."""
    if expected is None:
        expected = pillow_rgba(name)
    elif check:
        got = pillow_rgba(name)
        assert got.shape == expected.shape and np.array_equal(got, expected), f"{name}: Pillow disagrees"
    write_ref(name, expected)


def opaque(rgb):
    return np.dstack([rgb, np.full(rgb.shape[:2], 255, np.uint8)])


def grey_rgba(g8):
    return opaque(np.dstack([g8, g8, g8]))


def main():
    os.makedirs(OUT, exist_ok=True)
    rgba = base()
    rgb = np.ascontiguousarray(rgba[..., :3])
    grey = rgba[..., 0].copy()
    p = lambda n: os.path.join(OUT, n)

    # Pillow: the common little-endian strips.
    Image.fromarray(rgb, "RGB").save(p("rgb8.tif"))
    emit("rgb8.tif", opaque(rgb))
    Image.fromarray(rgb, "RGB").save(p("rgb8-lzw.tif"), compression="tiff_lzw")
    emit("rgb8-lzw.tif", opaque(rgb))
    Image.fromarray(rgb, "RGB").save(p("rgb8-packbits.tif"), compression="packbits")
    emit("rgb8-packbits.tif", opaque(rgb))
    Image.fromarray(rgba, "RGBA").save(p("rgba8-deflate.tif"), compression="tiff_adobe_deflate")
    emit("rgba8-deflate.tif", rgba)
    Image.fromarray(grey, "L").save(p("grey8.tif"))
    emit("grey8.tif", grey_rgba(grey))
    pal = Image.fromarray(rgb, "RGB").quantize(16)
    pal.save(p("pal4-lzw.tif"), compression="tiff_lzw")
    emit("pal4-lzw.tif")
    cmyk = Image.fromarray(rgb, "RGB").convert("CMYK")
    cmyk.save(p("cmyk8-lzw.tif"), compression="tiff_lzw")
    emit("cmyk8-lzw.tif")

    # Bilevel: a pattern with long and short runs, in both fax codings and
    # uncompressed in both polarities.
    y, x = np.mgrid[0:64, 0:101]
    bits = ((x // 7 + y // 5) % 2 == 0) | ((x * y) % 13 == 0)
    mono = Image.fromarray((bits * 255).astype(np.uint8), "L").convert("1", dither=Image.Dither.NONE)
    mono.save(p("mono-g4.tif"), compression="group4")
    emit("mono-g4.tif")
    mono.save(p("mono-g3.tif"), compression="group3")
    emit("mono-g3.tif")
    mono.save(p("mono.tif"))
    emit("mono.tif")
    mono_rgba = np.asarray(mono.convert("RGBA"))
    tifffile.imwrite(p("mono-miniswhite.tif"), ~bits, photometric="miniswhite", compression=None)
    emit("mono-miniswhite.tif", mono_rgba)

    # tifffile: what Pillow does not write.
    tifffile.imwrite(p("rgb8-be-tiles.tif"), rgb, byteorder=">", tile=(16, 16), photometric="rgb", compression="zlib")
    emit("rgb8-be-tiles.tif", opaque(rgb))
    tifffile.imwrite(p("rgb8-planar-pred.tif"), np.moveaxis(rgb, -1, 0), planarconfig="separate", photometric="rgb", compression="lzw", predictor=True, rowsperstrip=5)
    emit("rgb8-planar-pred.tif", opaque(rgb))
    tifffile.imwrite(p("rgb8-pred-strips.tif"), rgb, photometric="rgb", compression="zlib", predictor=True, rowsperstrip=4)
    emit("rgb8-pred-strips.tif", opaque(rgb))
    rgba_un = rgba
    tifffile.imwrite(p("rgba8-tiles-lzw.tif"), rgba_un, tile=(16, 32), photometric="rgb", extrasamples=["unassalpha"], compression="lzw", predictor=True)
    emit("rgba8-tiles-lzw.tif", rgba_un)
    # 16 bits, both byte orders, with the predictor: the high byte shows.
    rgb16 = rgb.astype(np.uint16) * 257 + 3
    for order, name in ((">", "rgb16-be-pred.tif"), ("<", "rgb16-le-pred.tif")):
        tifffile.imwrite(p(name), rgb16, byteorder=order, photometric="rgb", compression="zlib", predictor=True)
        emit(name, opaque((rgb16 >> 8).astype(np.uint8)), check=False)
    grey16 = grey.astype(np.uint16) * 257
    tifffile.imwrite(p("grey16-be.tif"), grey16, byteorder=">", photometric="minisblack")
    emit("grey16-be.tif", grey_rgba((grey16 >> 8).astype(np.uint8)), check=False)
    # 4-bit grey, packed two pixels to a byte.
    g4 = (grey >> 4).astype(np.uint8)
    tifffile.imwrite(p("grey4.tif"), g4, bitspersample=4, photometric="minisblack")
    emit("grey4.tif", grey_rgba(g4 * 17), check=False)
    # Associated alpha: stored premultiplied, shown straight.
    pre = rgba.copy()
    pre[..., :3] = (rgba[..., :3].astype(np.uint16) * rgba[..., 3:4] + 127) // 255
    tifffile.imwrite(p("rgba8-assoc.tif"), pre, photometric="rgb", extrasamples=["assocalpha"])
    a = pre[..., 3:4].astype(np.uint32)
    straight = np.where(a == 0, pre[..., :3], np.minimum(255, (pre[..., :3].astype(np.uint32) * 255 + a // 2) // np.maximum(a, 1)))
    straight = np.where(a == 255, pre[..., :3], straight)
    emit("rgba8-assoc.tif", np.dstack([straight.astype(np.uint8), pre[..., 3]]), check=False)
    # Orientation 6: the stored rows are the picture's columns, right to left.
    tifffile.imwrite(p("rgb8-rotated.tif"), rgb, photometric="rgb", extratags=[(274, "H", 1, 6, False)])
    emit("rgb8-rotated.tif", opaque(np.rot90(rgb, -1)), check=False)
    # Fill order 2: the bits of every byte reversed.
    tifffile.imwrite(p("mono-lsb.tif"), bits, photometric="minisblack", compression=None)
    raw = bytearray(open(p("mono-lsb.tif"), "rb").read())
    with tifffile.TiffFile(p("mono-lsb.tif")) as t:
        page = t.pages[0]
        off, n = page.dataoffsets[0], page.databytecounts[0]
        assert len(page.dataoffsets) == 1 and page.tags.get(266) is None
    raw[off:off + n] = bytes(int(f"{b:08b}"[::-1], 2) for b in raw[off:off + n])
    with open(p("mono-lsb.tif"), "wb") as f:
        f.write(raw)
    patch_fill_order(p("mono-lsb.tif"))
    emit("mono-lsb.tif", mono_rgba, check=False)
    # Two pages: the first is shown.
    with tifffile.TiffWriter(p("pages.tif")) as t:
        t.write(rgb, photometric="rgb")
        t.write(grey, photometric="minisblack")
    write_ref("pages.tif", opaque(rgb), pages=1)


def patch_fill_order(path):
    """Turns the file's first unused-looking tag into FillOrder = 2. The
    Software tag (305) tifffile writes is not needed by any reader."""
    import struct

    b = bytearray(open(path, "rb").read())
    assert b[:4] == b"II*\0"
    ifd = struct.unpack_from("<I", b, 4)[0]
    n = struct.unpack_from("<H", b, ifd)[0]
    entries = [bytes(b[ifd + 2 + 12 * i: ifd + 14 + 12 * i]) for i in range(n)]
    tags = [struct.unpack_from("<H", e)[0] for e in entries]
    assert 305 in tags, tags
    entries[tags.index(305)] = struct.pack("<HHII", 266, 3, 1, 2)
    entries.sort(key=lambda e: struct.unpack_from("<H", e)[0])
    b[ifd + 2: ifd + 2 + 12 * n] = b"".join(entries)
    open(path, "wb").write(b)


if __name__ == "__main__":
    main()
