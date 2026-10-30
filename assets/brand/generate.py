#!/usr/bin/env python3
"""Regenerate every brand asset in this folder from the mark's geometry.

The mark is defined once, below, on a 100-unit grid. SVGs are written by hand
from that geometry; PNG/ICO/ICNS are rasterised with Pillow (4x supersampled),
so nothing here needs a browser or a vector renderer.

    pip install pillow fonttools
    python3 assets/brand/generate.py --font path/to/InterTight-ExtraBold.ttf

The font is only needed for the lockups: their wordmark is converted to
outlines, so the committed SVGs never depend on Inter Tight being installed.
Without --font, lockups are left as they are.
"""
import argparse
import io
from pathlib import Path

from PIL import Image, ImageDraw

HERE = Path(__file__).resolve().parent

RED, INK, BONE, WHITE = "#E8321F", "#141317", "#EFECE6", "#F2F0F5"

# ── Geometry (100u grid) ─────────────────────────────────────────────────────
# full: stem 15u, 3u cut, bowl = 20u straight + r26 half circle, play hole.
# small: for <=24px. No cut, heavier stem, bigger play hole.
MARKS = {
    "full": dict(stem=(18, 12, 15, 76), stem_rx=1.5, bowl_x=36, bowl_top=12,
                 straight=20, r=26, play=((50, 25.5), (50, 50.5), (71, 38))),
    "small": dict(stem=(14, 10, 22, 80), stem_rx=0, bowl_x=36, bowl_top=10,
                  straight=22, r=28, play=((48, 22), (48, 54), (74, 38))),
}


def n(v):
    return f"{v:g}"


def mark_svg_body(kind, stem_fill, bowl_fill):
    m = MARKS[kind]
    x, y, w, h = m["stem"]
    bx, top, s, r = m["bowl_x"], m["bowl_top"], m["straight"], m["r"]
    (ax, ay), (bx2, by), (cx, cy) = m["play"]
    rx = f' rx="{n(m["stem_rx"])}"' if m["stem_rx"] else ""
    bowl = (f"M{n(bx)} {n(top)}H{n(bx + s)}A{n(r)} {n(r)} 0 0 1 {n(bx + s)} {n(top + 2 * r)}"
            f"H{n(bx)}Z M{n(ax)} {n(ay)}L{n(bx2)} {n(by)}L{n(cx)} {n(cy)}Z")
    return (f'<rect x="{n(x)}" y="{n(y)}" width="{n(w)}" height="{n(h)}"{rx} fill="{stem_fill}"/>'
            f'<path fill-rule="evenodd" fill="{bowl_fill}" d="{bowl}"/>')


def svg(view, body, title):
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="{view}">'
            f"<title>{title}</title>{body}</svg>\n")


# ── Wordmark outlines ────────────────────────────────────────────────────────
def wordmark(font_path, text, cap_u, tracking=-0.035):
    """Return (svg path d, width) with baseline at y=0, cap height = cap_u."""
    from fontTools.pens.svgPathPen import SVGPathPen
    from fontTools.pens.transformPen import TransformPen
    from fontTools.ttLib import TTFont

    f = TTFont(font_path)
    gs, cmap, hmtx = f.getGlyphSet(), f.getBestCmap(), f["hmtx"]
    k = cap_u / f["OS/2"].sCapHeight
    upem = f["head"].unitsPerEm
    pen, x = SVGPathPen(gs), 0.0
    for i, ch in enumerate(text):
        g = cmap[ord(ch)]
        gs[g].draw(TransformPen(pen, (k, 0, 0, -k, x, 0)))
        x += hmtx[g][0] * k
        if i < len(text) - 1:
            x += tracking * upem * k
    return pen.getCommands(), x


def lockups(font_path):
    # The wordmark's cap height equals the bowl (52u) and shares its top and
    # bottom edges, so the text sits beside the bowl; the stem hangs below it.
    cap, gap, baseline = 52, 22, 64
    for name, text in (("horizontal", "Pain By Choice"), ("compact", "PBC")):
        d, w = wordmark(font_path, text, cap)
        tx = 82 + gap
        for suffix, ink in (("", INK), ("-on-dark", WHITE)):
            body = (mark_svg_body("full", ink, RED)
                    + f'<path fill="{ink}" transform="translate({n(tx)} {baseline})" d="{d}"/>')
            write(f"svg/lockup-{name}{suffix}.svg",
                  svg(f"18 12 {n(round(tx + w - 18, 2))} 76", body, f"Pain By Choice — {name} lockup"))
    # Stacked: mark centred over the wordmark.
    d, w = wordmark(font_path, "Pain By Choice", 30)
    W = max(w, 64)
    for suffix, ink in (("", INK), ("-on-dark", WHITE)):
        body = (f'<g transform="translate({n(round(W / 2 - 50, 2))} 0)">{mark_svg_body("full", ink, RED)}</g>'
                f'<path fill="{ink}" transform="translate({n(round((W - w) / 2, 2))} 128)" d="{d}"/>')
        write(f"svg/lockup-stacked{suffix}.svg",
              svg(f"0 12 {n(round(W, 2))} 126", body, "Pain By Choice — stacked lockup"))


# ── App icon tiles ───────────────────────────────────────────────────────────
TILES = {"dark": (INK, WHITE, RED), "red": (RED, INK, WHITE), "light": (BONE, INK, RED)}
TILE_RADIUS = 0.225  # of tile size
MARK_SCALE = {"full": 0.62, "small": 0.74}


def tile_svg(style, kind="full"):
    bg, stem, bowl = TILES[style]
    s = MARK_SCALE[kind]
    off = 50 - 50 * s
    body = (f'<rect width="100" height="100" rx="{n(TILE_RADIUS * 100)}" fill="{bg}"/>'
            f'<g transform="translate({n(off)} {n(off)}) scale({n(s)})">'
            f"{mark_svg_body(kind, stem, bowl)}</g>")
    return svg("0 0 100 100", body, f"Pain By Choice app icon ({style})")


def raster(size, style=None, kind=None, colors=None):
    """Draw the mark (optionally on a tile) at `size` px, 4x supersampled."""
    ss = 4
    S = size * ss
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    if kind is None:
        kind = "small" if size <= 32 else "full"
    if style:
        bg, stem, bowl = TILES[style]
        radius = TILE_RADIUS * S if size > 16 else 0.19 * S
        d.rounded_rectangle((0, 0, S - 1, S - 1), radius=radius, fill=bg)
        scale = MARK_SCALE[kind]
    else:
        bg, (stem, bowl) = None, colors
        scale = 1.0
    u = S / 100 * scale
    o = S / 2 - 50 * u
    P = lambda x, y: (o + x * u, o + y * u)  # noqa: E731

    m = MARKS[kind]
    x, y, w, h = m["stem"]
    d.rounded_rectangle((*P(x, y), *P(x + w, y + h)), radius=m["stem_rx"] * u, fill=stem)
    bx, top, s, r = m["bowl_x"], m["bowl_top"], m["straight"], m["r"]
    d.rectangle((*P(bx, top), *P(bx + s, top + 2 * r)), fill=bowl)
    d.pieslice((*P(bx + s - r, top), *P(bx + s + r, top + 2 * r)), 270, 90, fill=bowl)
    tri = [P(*p) for p in m["play"]]
    if bg:
        d.polygon(tri, fill=bg)
    else:  # punch a real hole on transparent output
        hole = Image.new("L", (S, S), 0)
        ImageDraw.Draw(hole).polygon(tri, fill=255)
        img.putalpha(Image.composite(Image.new("L", (S, S), 0), img.getchannel("A"), hole))
    return img.resize((size, size), Image.LANCZOS)


def write(rel, data):
    p = HERE / rel
    p.parent.mkdir(parents=True, exist_ok=True)
    (p.write_bytes if isinstance(data, bytes) else p.write_text)(data)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--font", help="Inter Tight ExtraBold .ttf (for lockups)")
    args = ap.parse_args()

    marks = {
        "mark": ("full", INK, RED),
        "mark-on-dark": ("full", WHITE, RED),
        "mark-reverse": ("full", INK, WHITE),  # for use on Cut Red
        "mark-mono-black": ("full", INK, INK),
        "mark-mono-white": ("full", WHITE, WHITE),
        "mark-small": ("small", INK, RED),
        "mark-small-on-dark": ("small", WHITE, RED),
    }
    for name, (kind, stem, bowl) in marks.items():
        write(f"svg/{name}.svg", svg("0 0 100 100", mark_svg_body(kind, stem, bowl), "Pain By Choice"))
    for style in TILES:
        write(f"svg/app-icon-{style}.svg", tile_svg(style))
    write("svg/app-icon-dark-small.svg", tile_svg("dark", "small"))
    if args.font:
        lockups(args.font)

    (HERE / "png").mkdir(exist_ok=True)
    for size in (16, 32, 48, 64, 128, 256, 512, 1024):
        raster(size, "dark").save(HERE / f"png/app-icon-{size}.png")
    for style in ("red", "light"):
        raster(1024, style).save(HERE / f"png/app-icon-{style}-1024.png")
    raster(1024, colors=(INK, RED)).save(HERE / "png/mark-1024.png")
    raster(1024, colors=(WHITE, RED)).save(HERE / "png/mark-on-dark-1024.png")

    ico = io.BytesIO()
    raster(256, "dark").save(ico, format="ICO", sizes=[(16, 16), (32, 32), (48, 48), (256, 256)],
                             append_images=[raster(s, "dark") for s in (16, 32, 48)])
    write("favicon.ico", ico.getvalue())
    raster(1024, "dark").save(HERE / "app-icon.icns")


if __name__ == "__main__":
    main()
