#!/usr/bin/env python3
"""Render a tmux `capture-pane -p -e` ANSI dump to a PNG screenshot.

Parses 24-bit SGR colour + a handful of control sequences into a grid of
(cell, fg, bg, bold) and rasterises it with SF Mono via Pillow.  Faithful
enough to reproduce the Tokyo Night palette and bold emphasis of the hermon
TUI.

Usage: ansi2png.py <input.ansi> <output.png> [--size 22] [--pad 0]
"""
from __future__ import annotations

import argparse
import sys

from PIL import Image, ImageDraw, ImageFont

# Tokyo Night Storm palette (hermon src/render).  Used only as the default
# fg/bg for cells the TUI left with no explicit colour.
DEFAULT_FG = (0xC0, 0xCA, 0xF5)
DEFAULT_BG = (0x1A, 0x1B, 0x26)

FONT = "/System/Library/Fonts/SFNSMono.ttf"

# Standard 8/16-colour fallback (rarely hit; the TUI emits truecolor).
BASIC_FG = {
    30: (0, 0, 0), 31: (0xCD, 0x31, 0x31), 32: (0x31, 0xA3, 0x54),
    33: (0xE0, 0xAF, 0x68), 34: (0x7A, 0xA2, 0xF7), 35: (0xBB, 0x9A, 0xF7),
    36: (0x7D, 0xCF, 0xFF), 37: (0xC0, 0xCA, 0xF5),
}
BRIGHT_FG = {
    90: (0x56, 0x5F, 0x89), 91: (0xF7, 0x76, 0x8E), 92: (0x9E, 0xCE, 0x6A),
    93: (0xE0, 0xAF, 0x68), 94: (0x7A, 0xA2, 0xF7), 95: (0xBB, 0x9A, 0xF7),
    96: (0x7D, 0xCF, 0xFF), 97: (0xA9, 0xB1, 0xD6),
}


def parse_sgr(params: list[str], fg, bg, bold) -> tuple:
    i = 0
    while i < len(params):
        p = params[i]
        if p == "" or p == "0":
            fg, bg, bold = None, None, False
        elif p == "1":
            bold = True
        elif p == "22":
            bold = False
        elif p == "39":
            fg = None
        elif p == "49":
            bg = None
        elif p == "38" or p == "48":
            if i + 1 < len(params):
                mode = params[i + 1]
                if mode == "2" and i + 4 < len(params):
                    try:
                        rgb = (int(params[i + 2]), int(params[i + 3]), int(params[i + 4]))
                    except ValueError:
                        rgb = None
                    if rgb is not None:
                        if p == "38":
                            fg = rgb
                        else:
                            bg = rgb
                    i += 4
                elif mode == "5" and i + 2 < len(params):
                    idx = int(params[i + 2]) if params[i + 2].isdigit() else -1
                    if p == "38":
                        fg = xterm_256(idx)
                    else:
                        bg = xterm_256(idx)
                    i += 2
        elif p.isdigit():
            n = int(p)
            if n in BASIC_FG:
                fg = BASIC_FG[n]
            elif n in BRIGHT_FG:
                fg = BRIGHT_FG[n]
            elif 40 <= n <= 47:
                bg = BASIC_FG[n - 10]
            elif 100 <= n <= 107:
                bg = BRIGHT_FG[n - 10]
        i += 1
    return fg, bg, bold


def xterm_256(idx: int):
    if idx < 0:
        return None
    if idx < 16:
        return BASIC_FG.get(idx, BASIC_FG.get(30, DEFAULT_FG))
    if idx < 232:
        idx -= 16
        return (CUBE[idx // 36] * 51, CUBE[(idx // 6) % 6] * 51, CUBE[idx % 6] * 51)
    v = 8 + (idx - 232) * 10
    return (v, v, v)


CUBE = (0, 95, 135, 175, 215, 255)


def parse_ansi(text: str) -> tuple[list[list[tuple]], int, int]:
    """Return (grid, rows, cols).  grid[r][c] = (char, fg, bg, bold)."""
    rows: list[list[tuple]] = [[]]
    fg = None
    bg = None
    bold = False

    i = 0
    n = len(text)
    while i < n:
        ch = text[i]
        if ch == "\x1b" and i + 1 < n and text[i + 1] == "[":
            # CSI sequence: consume to the final byte 0x40-0x7E
            j = i + 2
            while j < n and not (0x40 <= ord(text[j]) <= 0x7E):
                j += 1
            if j >= n:
                break
            final = text[j]
            body = text[i + 2:j]
            if final == "m":
                fg, bg, bold = parse_sgr(body.split(";"), fg, bg, bold)
            # other CSI (cursor move, erase, etc.) ignored for a static capture
            i = j + 1
            continue
        if ch == "\n":
            rows.append([])
            i += 1
            continue
        if ch == "\r":
            # carriage return: tmux uses \n only; treat \r\n as newline already handled
            i += 1
            continue
        if ch == "\t":
            row = rows[-1]
            col = len(row)
            for _ in range(8 - (col % 8)):
                row.append((" ", fg, bg, bold))
            i += 1
            continue
        rows[-1].append((ch, fg, bg, bold))
        i += 1

    # drop trailing blank lines
    while rows and all(c[0] == " " for c in rows[-1]):
        rows.pop()

    cols = max((len(r) for r in rows), default=0)
    for r in rows:
        while len(r) < cols:
            r.append((" ", None, None, False))
    return rows, len(rows), cols


def render(grid, rows, cols, size: int, pad: int) -> Image.Image:
    font = ImageFont.truetype(FONT, size)
    # advance width of a space = the monospace cell width
    adv = font.getlength(" ")
    line_h = int(size * 1.4)
    W = int(cols * adv) + 2 * pad
    H = rows * line_h + 2 * pad
    img = Image.new("RGB", (W, H), DEFAULT_BG)
    d = ImageDraw.Draw(img)

    # background pass
    for r in range(rows):
        for c in range(cols):
            _, _f, b, _b = grid[r][c]
            if b is not None and b != DEFAULT_BG:
                x = pad + int(c * adv)
                y = pad + r * line_h
                d.rectangle([x, y, x + int(adv) + 1, y + line_h], fill=b)

    # text pass
    for r in range(rows):
        for c in range(cols):
            ch, f, b, bold = grid[r][c]
            if ch == " " or ch == "":
                continue
            color = f if f is not None else DEFAULT_FG
            x = pad + int(c * adv)
            y = pad + r * line_h + int((line_h - size) / 2)
            d.text((x, y), ch, font=font, fill=color)
            if bold:
                d.text((x + max(1, size // 22), y), ch, font=font, fill=color)

    return img


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("input")
    ap.add_argument("output")
    ap.add_argument("--size", type=int, default=22)
    ap.add_argument("--pad", type=int, default=0)
    args = ap.parse_args()

    with open(args.input, encoding="utf-8", errors="replace") as f:
        text = f.read()

    grid, rows, cols = parse_ansi(text)
    if rows == 0 or cols == 0:
        print("empty capture", file=sys.stderr)
        return 1
    img = render(grid, rows, cols, args.size, args.pad)
    img.save(args.output)
    print(f"{args.output}: {img.width}x{img.height} ({rows}x{cols} cells)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
