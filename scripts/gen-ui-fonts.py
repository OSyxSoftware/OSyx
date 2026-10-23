"""Generate the font atlases of the OSyx desktop (PRISM, the UI toolkit and
the terminal). Outputs are committed under graphics/assets/; regenerate only
when the sources or sizes change.

  sans14.osxp   Cantarell Regular 14 px  (body text)
  sans14b.osxp  Cantarell Bold 14 px     (titles, buttons)
  sans20.osxp  Cantarell Regular 20 px   (headings)
  sans32l.osxp  Cantarell Light 32 px    (large clock / hero text)
  mono15.osxf   Source Code Pro Regular 15 px, U+0020..U+017F (terminal)

OSXP (proportional, all integers little-endian):
  b"OSXP", u32 version (1), u32 line_height, u32 ascent, u32 glyph_count,
  then glyph_count records of 16 bytes sorted by code point:
    u32 code point, i16 left, i16 top (from the line top), u16 width,
    u16 height, u16 advance, u16 reserved
  then u32 offsets of each glyph's coverage bytes (relative to the start
  of the coverage block), then the coverage block (width * height bytes
  per glyph, 0..255).
OSXF (monospace) is the format of scripts/gen-assets.py.

Only glyphs the font really has are included: a code point whose rendering
equals the font's .notdef glyph is left out (the renderer falls back to '?').

Usage: python -I scripts/gen-ui-fonts.py [--fonts DIR]
"""

import argparse
import struct
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "graphics" / "assets"
FONTS = Path(r"C:\msys64\ucrt64\share\fonts")
LICENSES = Path(r"C:\msys64\ucrt64\share\licenses")

# Latin-1 and Latin Extended-A (all Polish letters), plus common punctuation
# and arrows used by the desktop.
CODEPOINTS = (
    list(range(0x20, 0x7F))
    + list(range(0xA0, 0x180))
    + [0x2013, 0x2014, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2026, 0x20AC, 0x2122,
       0x2190, 0x2191, 0x2192, 0x2193, 0x2212, 0x2713, 0x00D7]
)


def load(path: Path, size: int, variation: str | None):
    font = ImageFont.truetype(str(path), size)
    if variation:
        font.set_variation_by_name(variation)
    return font


def render(font, ch: str, ascent: int, line: int, pad: int):
    w = line * 3
    img = Image.new("L", (w, line + 2 * pad), 0)
    ImageDraw.Draw(img).text((pad, pad + ascent), ch, font=font, fill=255, anchor="ls")
    return img


def gen_prop(path: Path, size: int, variation: str | None, name: str):
    font = load(path, size, variation)
    ascent, descent = font.getmetrics()
    line = ascent + descent
    pad = size
    missing = render(font, "\U000F0000", ascent, line, pad).tobytes()
    records = []
    blobs = []
    for cp in sorted(set(CODEPOINTS)):
        ch = chr(cp)
        img = render(font, ch, ascent, line, pad)
        if cp != 0x20 and cp != 0xA0 and img.tobytes() == missing:
            continue
        advance = round(font.getlength(ch))
        box = img.getbbox()
        if box is None:
            left = top = width = height = 0
            data = b""
        else:
            x0, y0, x1, y1 = box
            crop = img.crop(box)
            left, top, width, height = x0 - pad, y0 - pad, x1 - x0, y1 - y0
            data = crop.tobytes()
        records.append((cp, left, top, width, height, advance))
        blobs.append(data)
    out = bytearray(b"OSXP" + struct.pack("<IIII", 1, line, ascent, len(records)))
    for cp, left, top, width, height, advance in records:
        out += struct.pack("<IhhHHHH", cp, left, top, width, height, advance, 0)
    offset = 0
    for b in blobs:
        out += struct.pack("<I", offset)
        offset += len(b)
    for b in blobs:
        out += b
    (OUT / name).write_bytes(out)
    print(f"{name}: {len(records)} glyphs, line {line}, ascent {ascent}, {len(out)} bytes")


def gen_mono(path: Path, size: int, name: str, first: int, last: int):
    font = ImageFont.truetype(str(path), size)
    ascent, descent = font.getmetrics()
    cell_w = round(font.getlength("M"))
    cell_h = ascent + descent
    missing = Image.new("L", (cell_w, cell_h), 0)
    ImageDraw.Draw(missing).text((0, 0), "\U000F0000", font=font, fill=255)
    missing = missing.tobytes()
    out = bytearray(b"OSXF" + struct.pack("<IIIII", first, last - first + 1, cell_w, cell_h, ascent))
    present = 0
    for code in range(first, last + 1):
        cell = Image.new("L", (cell_w, cell_h), 0)
        if not (0x7F <= code < 0xA0):
            ImageDraw.Draw(cell).text((0, 0), chr(code), font=font, fill=255)
        data = cell.tobytes()
        if data == missing:
            data = bytes(len(data))
        else:
            present += 1
        out += data
    (OUT / name).write_bytes(out)
    print(f"{name}: cell {cell_w}x{cell_h}, baseline {ascent}, {present} glyphs drawn, {len(out)} bytes")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--fonts", type=Path, default=FONTS)
    args = ap.parse_args()
    OUT.mkdir(parents=True, exist_ok=True)
    cantarell = args.fonts / "cantarell" / "Cantarell-VF.otf"
    code = args.fonts / "adobe-source-code-pro" / "SourceCodePro-Regular.otf"
    gen_prop(cantarell, 14, b"Regular", "sans14.osxp")
    gen_prop(cantarell, 14, b"Bold", "sans14b.osxp")
    gen_prop(cantarell, 20, b"Regular", "sans20.osxp")
    gen_prop(cantarell, 32, b"Light", "sans32l.osxp")
    gen_mono(code, 15, "mono15.osxf", 0x20, 0x17F)
    for src, dst in [
        (LICENSES / "cantarell-fonts" / "COPYING", "LICENSE-Cantarell.txt"),
        (LICENSES / "adobe-source-code-pro-fonts" / "LICENSE.md", "LICENSE-SourceCodePro.md"),
    ]:
        if src.exists():
            (OUT / dst).write_text(src.read_text(encoding="utf-8"), encoding="utf-8")


if __name__ == "__main__":
    main()
