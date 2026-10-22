"""Generate the binary boot assets embedded in the OSyx bootloader and kernel.

Outputs (committed to the repository, regenerate only when sources change):
  boot/assets/logo.osxi   - OSyx logo, straight-alpha RGBA8
  boot/assets/mono16.osxf - Source Code Pro Regular 16 px coverage atlas
  boot/assets/mono22.osxf - Source Code Pro Light 22 px coverage atlas

Formats (all integers little-endian u32):
  OSXI: b"OSXI", width, height, then width*height*4 bytes (R, G, B, A)
  OSXF: b"OSXF", first_char, glyph_count, cell_w, cell_h, baseline,
        then glyph_count * cell_w * cell_h coverage bytes (0..255)

Usage: python -I scripts/gen-assets.py [--logo OSyx.png] [--fonts DIR]
"""

import argparse
import struct
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "boot" / "assets"
DEFAULT_FONTS = Path(r"C:\msys64\ucrt64\share\fonts\adobe-source-code-pro")
LOGO_SIZE = 320
FIRST_CHAR = 32
LAST_CHAR = 126


def color_to_alpha(rgb, bg):
    """Recover (fg, alpha) such that alpha*fg + (1-alpha)*bg == rgb."""
    alpha = 0.0
    for c, b in zip(rgb, bg):
        if c > b:
            alpha = max(alpha, (c - b) / (255 - b))
        elif c < b:
            alpha = max(alpha, (b - c) / b)
    if alpha <= 0.0:
        return (0, 0, 0, 0)
    fg = tuple(max(0, min(255, round((c - b) / alpha + b))) for c, b in zip(rgb, bg))
    return fg + (round(alpha * 255),)


def gen_logo(src: Path):
    im = Image.open(src).convert("RGB")
    # Average the four corners: the artwork sits on a flat dark background.
    w, h = im.size
    corners = [im.getpixel(p) for p in [(2, 2), (w - 3, 2), (2, h - 3), (w - 3, h - 3)]]
    bg = tuple(round(sum(c[i] for c in corners) / 4) for i in range(3))
    small = im.resize((LOGO_SIZE, LOGO_SIZE), Image.LANCZOS)
    out = bytearray(b"OSXI" + struct.pack("<II", LOGO_SIZE, LOGO_SIZE))
    for y in range(LOGO_SIZE):
        for x in range(LOGO_SIZE):
            out += bytes(color_to_alpha(small.getpixel((x, y)), bg))
    (OUT / "logo.osxi").write_bytes(out)
    print(f"logo.osxi: {LOGO_SIZE}x{LOGO_SIZE}, background {bg}, {len(out)} bytes")


def gen_font(path: Path, size: int, name: str):
    font = ImageFont.truetype(str(path), size)
    ascent, descent = font.getmetrics()
    cell_w = round(font.getlength("M"))
    cell_h = ascent + descent
    count = LAST_CHAR - FIRST_CHAR + 1
    out = bytearray(b"OSXF" + struct.pack("<IIIII", FIRST_CHAR, count, cell_w, cell_h, ascent))
    for code in range(FIRST_CHAR, LAST_CHAR + 1):
        cell = Image.new("L", (cell_w, cell_h), 0)
        ImageDraw.Draw(cell).text((0, 0), chr(code), font=font, fill=255)
        out += cell.tobytes()
    (OUT / name).write_bytes(out)
    print(f"{name}: cell {cell_w}x{cell_h}, baseline {ascent}, {len(out)} bytes")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--logo", type=Path, default=ROOT / "OSyx.png")
    ap.add_argument("--fonts", type=Path, default=DEFAULT_FONTS)
    args = ap.parse_args()
    OUT.mkdir(parents=True, exist_ok=True)
    gen_logo(args.logo)
    gen_font(args.fonts / "SourceCodePro-Regular.otf", 16, "mono16.osxf")
    gen_font(args.fonts / "SourceCodePro-Light.otf", 22, "mono22.osxf")
    lic = Path(r"C:\msys64\ucrt64\share\licenses\adobe-source-code-pro-fonts\LICENSE.md")
    if lic.exists():
        (OUT / "LICENSE-SourceCodePro.md").write_text(lic.read_text(encoding="utf-8"), encoding="utf-8")


if __name__ == "__main__":
    main()
