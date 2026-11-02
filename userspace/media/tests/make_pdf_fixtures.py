"""Writes the PDF test files in tests/data/pdf and reference renderings.

Needs reportlab, matplotlib, pikepdf, pypdf and pypdfium2:

    python userspace/media/tests/make_pdf_fixtures.py

Every `<name>.pdf` gets `<name>.p<N>.ref.gz`: page N rendered by PDFium at
1 pixel per point as gzip-compressed RGB rows. The tests compare the OSyx
renderer against them.
"""

import gzip
import io
import os

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt  # noqa: E402
import numpy as np  # noqa: E402
import pikepdf  # noqa: E402
import pypdf  # noqa: E402
import pypdfium2 as pdfium  # noqa: E402
from reportlab.lib.colors import Color, CMYKColor  # noqa: E402
from reportlab.pdfbase import pdfmetrics  # noqa: E402
from reportlab.pdfbase.ttfonts import TTFont  # noqa: E402
from reportlab.pdfgen import canvas  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "data", "pdf")
FONTS = os.path.join(HERE, "..", "..", "..", "graphics", "assets", "fonts", "windows")


def shapes(path):
    """Paths, strokes, dashes, colours, a gradient, clipping, transparency
    and an image, with an embedded TrueType font for the text."""
    pdfmetrics.registerFont(TTFont("Lato", os.path.join(FONTS, "Lato-Regular.ttf")))
    c = canvas.Canvas(path, pagesize=(300, 220))
    c.setFillColor(Color(0.95, 0.9, 0.8))
    c.rect(0, 0, 300, 220, stroke=0, fill=1)
    c.setFillColor(Color(0.1, 0.3, 0.8))
    c.setStrokeColor(Color(0.8, 0.1, 0.1))
    c.setLineWidth(4)
    c.rect(20, 120, 80, 70, stroke=1, fill=1)
    c.setFillColor(CMYKColor(0, 0.6, 1, 0))
    c.circle(160, 155, 35, stroke=0, fill=1)
    c.setDash(6, 3)
    c.setStrokeColor(Color(0, 0.5, 0))
    c.setLineWidth(2)
    c.line(20, 105, 280, 105)
    c.setDash()
    p = c.beginPath()
    p.moveTo(210, 120)
    p.curveTo(230, 210, 270, 90, 290, 190)
    c.setLineWidth(3)
    c.setLineCap(1)
    c.drawPath(p, stroke=1, fill=0)
    c.linearGradient(20, 20, 140, 20, (Color(1, 0, 0), Color(0, 0, 1)), extend=False)
    c.saveState()
    clip = c.beginPath()
    clip.circle(220, 50, 35)
    c.clipPath(clip, stroke=0, fill=0)
    c.setFillColor(Color(0.2, 0.7, 0.2))
    c.rect(180, 10, 80, 80, stroke=0, fill=1)
    c.restoreState()
    c.setFillColor(Color(0, 0, 0, alpha=0.5))
    c.rect(150, 30, 60, 40, stroke=0, fill=1)
    img = np.zeros((16, 24, 3), np.uint8)
    img[..., 0] = np.linspace(0, 255, 24)[None, :]
    img[..., 1] = np.linspace(0, 255, 16)[:, None]
    from reportlab.lib.utils import ImageReader
    from PIL import Image

    c.drawImage(ImageReader(Image.fromarray(img)), 110, 60, width=36, height=24)
    c.setFillColor(Color(0, 0, 0))
    c.setFont("Lato", 16)
    c.drawString(20, 200, "Zażółć gęślą jaźń 123")
    c.save()


def standard_fonts(path):
    """The standard 14 fonts, not embedded, with JPEG and rotated text."""
    c = canvas.Canvas(path, pagesize=(260, 160))
    for i, f in enumerate(["Helvetica", "Times-Bold", "Courier"]):
        c.setFont(f, 14)
        c.drawString(10, 130 - 22 * i, f"{f}: The quick brown fox")
    c.saveState()
    c.translate(200, 20)
    c.rotate(30)
    c.setFont("Helvetica-Oblique", 12)
    c.drawString(0, 0, "rotated")
    c.restoreState()
    from PIL import Image

    rgb = np.zeros((40, 60, 3), np.uint8)
    rgb[..., 2] = 200
    rgb[10:30, 15:45, 0] = 255
    buf = io.BytesIO()
    Image.fromarray(rgb).save(buf, "JPEG", quality=90)
    buf.seek(0)
    from reportlab.lib.utils import ImageReader

    c.drawImage(ImageReader(buf), 10, 10, width=60, height=40)
    c.save()


def chart(path):
    """matplotlib: Type 3 fonts, many paths, markers and fills."""
    fig, ax = plt.subplots(figsize=(4, 3), dpi=72)
    x = np.linspace(0, 6, 60)
    ax.plot(x, np.sin(x), "o-", markersize=3, label="sin")
    ax.fill_between(x, np.cos(x), alpha=0.3)
    ax.set_title("Chart")
    ax.legend()
    fig.savefig(path, format="pdf")
    plt.close(fig)


def truetype42(path):
    """matplotlib with embedded TrueType (Type 42) fonts."""
    with matplotlib.rc_context({"pdf.fonttype": 42}):
        fig = plt.figure(figsize=(3, 1), dpi=72)
        fig.text(0.05, 0.4, "TrueType 42 text", fontsize=18)
        fig.savefig(path, format="pdf")
        plt.close(fig)


def object_streams(src, path):
    """The same file rewritten with object streams and an xref stream."""
    with pikepdf.open(src) as pdf:
        pdf.save(path, object_stream_mode=pikepdf.ObjectStreamMode.generate, compress_streams=True)


def encrypted(src, path, algorithm):
    """Encrypted with an owner password only (opens without a password)."""
    r = pypdf.PdfReader(src)
    w = pypdf.PdfWriter(clone_from=r)
    w.encrypt(user_password="", owner_password="owner", algorithm=algorithm)
    with open(path, "wb") as f:
        w.write(f)


def reference(path):
    pdf = pdfium.PdfDocument(path)
    stem = os.path.splitext(path)[0]
    for i in range(len(pdf)):
        page = pdf[i]
        bmp = page.render(scale=1.0).to_numpy()
        rgb = bmp[..., :3][..., ::-1] if bmp.shape[2] >= 3 else np.repeat(bmp, 3, axis=2)
        h, w = rgb.shape[:2]
        header = np.array([w, h], "<u4").tobytes()
        with open(f"{stem}.p{i}.ref.gz", "wb") as f:
            f.write(gzip.compress(header + np.ascontiguousarray(rgb).tobytes(), mtime=0))
        print(f"{os.path.basename(path)} page {i}: {w}x{h}")


def main():
    os.makedirs(OUT, exist_ok=True)
    p = lambda n: os.path.join(OUT, n)  # noqa: E731
    shapes(p("shapes.pdf"))
    standard_fonts(p("standard-fonts.pdf"))
    chart(p("chart.pdf"))
    truetype42(p("truetype42.pdf"))
    object_streams(p("shapes.pdf"), p("objstm.pdf"))
    encrypted(p("standard-fonts.pdf"), p("rc4.pdf"), "RC4-128")
    encrypted(p("shapes.pdf"), p("aes256.pdf"), "AES-256")
    for f in sorted(os.listdir(OUT)):
        if f.endswith(".pdf"):
            reference(p(f))


if __name__ == "__main__":
    main()
