#!/usr/bin/env python3
"""Generate the hermon logo (a "deck of live session panes" mark) as both a
512px PNG and a matching SVG, in the Tokyo Night Storm palette.

Usage: make_logo.py [out_dir]   (default: repo assets/)
"""
from __future__ import annotations

import os
import sys

from PIL import Image, ImageDraw

# Tokyo Night Storm palette (src/render)
BG = (0x1A, 0x1B, 0x26)        # #1a1b26
CHROME = (0x16, 0x16, 0x1E)    # #16161e
BORDER = (0x3B, 0x42, 0x61)    # #3b4261
SELECTION = (0x28, 0x34, 0x57)  # #283457
FG = (0xC0, 0xCA, 0xF5)        # #c0caf5
DIM = (0x56, 0x5F, 0x89)       # #565f89
CYAN = (0x7D, 0xCF, 0xFF)      # #7dcfff
BLUE = (0x7A, 0xA2, 0xF7)      # #7aa2f7
GREEN = (0x9E, 0xCE, 0x6A)     # #9ece6a
AMBER = (0xE0, 0xAF, 0x68)     # #e0af68

DOT_COLORS = [CYAN, BLUE, GREEN, AMBER]

SIZE = 512
GRID_INSET = 40
GAP = 30


def _hex(c) -> str:
    return "#{:02x}{:02x}{:02x}".format(*c)


def pane_rects():
    inner = SIZE - 2 * GRID_INSET
    cell = (inner - GAP) / 2
    rects = []
    for r in range(2):
        for c in range(2):
            x = GRID_INSET + c * (cell + GAP)
            y = GRID_INSET + r * (cell + GAP)
            rects.append((x, y, x + cell, y + cell))
    return rects


def draw_pane(d, box, dot_color):
    x0, y0, x1, y1 = box
    w = x1 - x0
    # pane body
    d.rounded_rectangle([x0, y0, x1, y1], radius=20, fill=CHROME, outline=BORDER, width=3)
    # status dot
    cx = x0 + 30
    cy = y0 + 30
    d.ellipse([cx - 12, cy - 12, cx + 12, cy + 12], fill=dot_color)
    # title bar (dim)
    d.rounded_rectangle([x0 + 54, y0 + 20, x0 + 54 + w * 0.45, y0 + 40], radius=9, fill=SELECTION)
    # content lines
    line_x = x0 + 30
    tops = [y0 + 66, y0 + 96, y0 + 126]
    widths = [w - 60, w - 96, w - 120]
    for i, (top, lw) in enumerate(zip(tops, widths)):
        color = DIM
        # one accent "tool" line per pane, like `▶ tool …`
        if i == 1:
            color = dot_color
        d.rounded_rectangle([line_x, top, line_x + lw, top + 14], radius=7, fill=color)


def build_png(path: str) -> None:
    # Transparent canvas; the card itself is a rounded rect so the corners
    # stay transparent and the logo sits cleanly on any background.
    img = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))

    # gradient card, clipped to the rounded-corner silhouette
    grad = Image.new("RGB", (SIZE, SIZE), BG)
    gd = ImageDraw.Draw(grad)
    for y in range(SIZE):
        t = y / SIZE
        row = tuple(int(BG[i] + (CHROME[i] - BG[i]) * t) for i in range(3))
        gd.line([(0, y), (SIZE, y)], fill=row)
    mask = Image.new("L", (SIZE, SIZE), 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, SIZE - 1, SIZE - 1], radius=96, fill=255)
    img.paste(grad, (0, 0), mask)

    d = ImageDraw.Draw(img)
    d.rounded_rectangle([2, 2, SIZE - 3, SIZE - 3], radius=94, outline=BORDER, width=4)
    for box, color in zip(pane_rects(), DOT_COLORS):
        draw_pane(d, box, color)
    img.save(path)
    print(f"{path}: {img.width}x{img.height}")


def build_svg(path: str) -> None:
    rects = pane_rects()
    panes = ""
    for (x0, y0, x1, y1), color in zip(rects, DOT_COLORS):
        w = x1 - x0
        cx, cy = x0 + 30, y0 + 30
        panes += f'''  <g>
    <rect x="{x0}" y="{y0}" width="{w:.1f}" height="{w:.1f}" rx="20" fill="{_hex(CHROME)}" stroke="{_hex(BORDER)}" stroke-width="3"/>
    <circle cx="{cx}" cy="{cy}" r="12" fill="{_hex(color)}"/>
    <rect x="{x0+54:.1f}" y="{y0+20:.1f}" width="{w*0.45:.1f}" height="20" rx="9" fill="{_hex(SELECTION)}"/>
    <rect x="{x0+30:.1f}" y="{y0+66:.1f}" width="{w-60:.1f}" height="14" rx="7" fill="{_hex(DIM)}"/>
    <rect x="{x0+30:.1f}" y="{y0+96:.1f}" width="{w-96:.1f}" height="14" rx="7" fill="{_hex(color)}"/>
    <rect x="{x0+30:.1f}" y="{y0+126:.1f}" width="{w-120:.1f}" height="14" rx="7" fill="{_hex(DIM)}"/>
  </g>
'''
    svg = f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {SIZE} {SIZE}" width="{SIZE}" height="{SIZE}">
  <rect width="{SIZE}" height="{SIZE}" rx="96" fill="{_hex(BG)}"/>
  <rect x="2" y="2" width="{SIZE-4}" height="{SIZE-4}" rx="96" fill="none" stroke="{_hex(BORDER)}" stroke-width="4"/>
{panes}</svg>
'''
    with open(path, "w") as f:
        f.write(svg)
    print(f"{path} written")


def main() -> int:
    out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(
        os.path.dirname(os.path.abspath(__file__)), "..", "..", "assets")
    os.makedirs(out, exist_ok=True)
    build_png(os.path.join(out, "logo.png"))
    build_svg(os.path.join(out, "logo.svg"))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
