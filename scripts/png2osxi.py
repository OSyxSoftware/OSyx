#!/usr/bin/env python3
"""Converts a PNG to the OSyx OSXI image format (straight-alpha RGBA).

    png2osxi.py <in.png> <out.osxi>

OSXI: b"OSXI", u32 width, u32 height (little-endian), then width*height
RGBA bytes. Standard library only: decodes 8-bit grey, grey+alpha, RGB,
RGBA and palette PNGs without interlacing (what image editors export).
"""
import struct
import sys
import zlib


def paeth(a, b, c):
    p = a + b - c
    pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
    if pa <= pb and pa <= pc:
        return a
    return b if pb <= pc else c


def decode(data):
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError("not a PNG file")
    pos, idat, palette, trns = 8, bytearray(), None, None
    width = height = depth = ctype = interlace = None
    while pos < len(data):
        length, kind = struct.unpack(">I4s", data[pos:pos + 8])
        body = data[pos + 8:pos + 8 + length]
        pos += 12 + length
        if kind == b"IHDR":
            width, height, depth, ctype, _, _, interlace = struct.unpack(">IIBBBBB", body)
        elif kind == b"PLTE":
            palette = [body[i:i + 3] for i in range(0, len(body), 3)]
        elif kind == b"tRNS":
            trns = body
        elif kind == b"IDAT":
            idat += body
        elif kind == b"IEND":
            break
    if depth != 8 or interlace != 0:
        raise ValueError(f"unsupported PNG: bit depth {depth}, interlace {interlace}")
    channels = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}[ctype]
    raw = zlib.decompress(bytes(idat))
    stride = width * channels
    out = bytearray(width * height * 4)
    prev = bytearray(stride)
    i = 0
    for y in range(height):
        f = raw[i]
        line = bytearray(raw[i + 1:i + 1 + stride])
        i += 1 + stride
        bpp = channels
        if f == 1:
            for x in range(bpp, stride):
                line[x] = (line[x] + line[x - bpp]) & 0xFF
        elif f == 2:
            for x in range(stride):
                line[x] = (line[x] + prev[x]) & 0xFF
        elif f == 3:
            for x in range(stride):
                left = line[x - bpp] if x >= bpp else 0
                line[x] = (line[x] + ((left + prev[x]) >> 1)) & 0xFF
        elif f == 4:
            for x in range(stride):
                left = line[x - bpp] if x >= bpp else 0
                up_left = prev[x - bpp] if x >= bpp else 0
                line[x] = (line[x] + paeth(left, prev[x], up_left)) & 0xFF
        elif f != 0:
            raise ValueError(f"bad filter {f} in row {y}")
        o = y * width * 4
        if ctype == 2:
            out[o:o + width * 4:4] = line[0::3]
            out[o + 1:o + width * 4:4] = line[1::3]
            out[o + 2:o + width * 4:4] = line[2::3]
            out[o + 3:o + width * 4:4] = b"\xff" * width
        elif ctype == 6:
            out[o:o + width * 4] = line
        else:
            for x in range(width):
                if ctype == 0:
                    g = line[x]
                    px = (g, g, g, 255)
                elif ctype == 4:
                    g = line[2 * x]
                    px = (g, g, g, line[2 * x + 1])
                else:
                    k = line[x]
                    a = trns[k] if trns and k < len(trns) else 255
                    px = (*palette[k], a)
                out[o + 4 * x:o + 4 * x + 4] = bytes(px)
        prev = line
    return width, height, bytes(out)


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    with open(sys.argv[1], "rb") as f:
        w, h, rgba = decode(f.read())
    with open(sys.argv[2], "wb") as f:
        f.write(b"OSXI" + struct.pack("<II", w, h) + rgba)
    print(f"{sys.argv[2]}: {w}x{h} OSXI, {len(rgba) + 12} bytes")


if __name__ == "__main__":
    main()
