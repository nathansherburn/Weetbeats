#!/usr/bin/env python3
"""Draws the Weetbeats app icon and writes every size Tauri's bundler asks for.

Run from the repo root:  python3 tools/icon/make_icon.py

The mark is the app in miniature: a step grid with four beats lit. Drawn here rather
than pulled in as a binary blob so it can be changed by editing numbers.
"""

import math
import os
import struct
import zlib

OUT = os.path.join(os.path.dirname(__file__), "..", "..", "src-tauri", "icons")
SIZE = 1024

# Matches the app's palette.
BG_TOP = (0x2A, 0x21, 0x38)
BG_BOTTOM = (0x14, 0x11, 0x1B)
LIT = (0xFF, 0x4D, 0x87)
GLOW = (0xFF, 0xD7, 0x5E)
DIM = (0x3A, 0x32, 0x4A)

# Which of the sixteen steps are lit. Four on the floor, with a couple of offbeats.
PATTERN = [
    1, 0, 0, 0,
    1, 0, 2, 0,
    1, 0, 0, 0,
    1, 0, 2, 0,
]


def rounded_rect_distance(px, py, x, y, w, h, r):
    """Signed distance from a point to a rounded rectangle. Negative means inside."""
    cx = abs(px - (x + w / 2)) - (w / 2 - r)
    cy = abs(py - (y + h / 2)) - (h / 2 - r)
    outside = math.hypot(max(cx, 0.0), max(cy, 0.0))
    return outside + min(max(cx, cy), 0.0) - r


def blend(dst, src, alpha):
    return tuple(round(d + (s - d) * alpha) for d, s in zip(dst, src))


def draw():
    """Render the icon as a list of RGBA rows."""
    # macOS icons leave a margin: the artwork is about 80% of the canvas.
    pad = SIZE * 0.09
    body = SIZE - pad * 2
    radius = body * 0.235

    cells = 4
    grid_pad = body * 0.16
    grid = body - grid_pad * 2
    gap = grid * 0.055
    cell = (grid - gap * (cells - 1)) / cells
    cell_radius = cell * 0.26

    rows = []
    for py in range(SIZE):
        row = bytearray()
        for px in range(SIZE):
            x = px + 0.5
            y = py + 0.5

            # The rounded body, with a top to bottom gradient.
            d = rounded_rect_distance(x, y, pad, pad, body, body, radius)
            body_alpha = min(max(0.5 - d, 0.0), 1.0)
            if body_alpha <= 0.0:
                row += b"\x00\x00\x00\x00"
                continue

            t = (y - pad) / body
            colour = blend(BG_TOP, BG_BOTTOM, min(max(t, 0.0), 1.0))

            # The step boxes.
            gx = pad + grid_pad
            gy = pad + grid_pad
            col = int((x - gx) // (cell + gap)) if x >= gx else -1
            rowi = int((y - gy) // (cell + gap)) if y >= gy else -1
            if 0 <= col < cells and 0 <= rowi < cells:
                bx = gx + col * (cell + gap)
                by = gy + rowi * (cell + gap)
                cd = rounded_rect_distance(x, y, bx, by, cell, cell, cell_radius)
                ca = min(max(0.5 - cd, 0.0), 1.0)
                if ca > 0.0:
                    state = PATTERN[rowi * cells + col]
                    step = {0: DIM, 1: LIT, 2: GLOW}[state]
                    colour = blend(colour, step, ca)

            row += bytes(colour) + bytes([round(body_alpha * 255)])
        rows.append(bytes(row))
    return rows


def downsample(rows, size):
    """Box filter from the full size image down to `size`."""
    src = len(rows)
    scale = src / size
    out = []
    for y in range(size):
        y0, y1 = int(y * scale), max(int(y * scale) + 1, int((y + 1) * scale))
        row = bytearray()
        for x in range(size):
            x0, x1 = int(x * scale), max(int(x * scale) + 1, int((x + 1) * scale))
            totals = [0, 0, 0, 0]
            count = 0
            for sy in range(y0, y1):
                line = rows[sy]
                for sx in range(x0, x1):
                    i = sx * 4
                    a = line[i + 3]
                    # Weight colour by alpha so transparent edges do not darken.
                    totals[0] += line[i] * a
                    totals[1] += line[i + 1] * a
                    totals[2] += line[i + 2] * a
                    totals[3] += a
                    count += 1
            alpha = totals[3] / count
            if totals[3] == 0:
                row += b"\x00\x00\x00\x00"
            else:
                row += bytes(
                    [
                        round(totals[0] / totals[3]),
                        round(totals[1] / totals[3]),
                        round(totals[2] / totals[3]),
                        round(alpha),
                    ]
                )
        out.append(bytes(row))
    return out


def png(rows):
    """Encode RGBA rows as a PNG."""
    size = len(rows)
    raw = b"".join(b"\x00" + row for row in rows)

    def chunk(tag, data):
        body = tag + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def icns(images):
    """Pack PNGs into an icns. Each entry is a four byte type, a length, then the PNG."""
    body = b""
    for tag, data in images:
        body += tag + struct.pack(">I", len(data) + 8) + data
    return b"icns" + struct.pack(">I", len(body) + 8) + body


def main():
    os.makedirs(OUT, exist_ok=True)
    print("drawing...")
    full = draw()

    sizes = {}
    for size in (1024, 512, 256, 128, 64, 32):
        sizes[size] = full if size == SIZE else downsample(full, size)
        print(f"  {size}x{size}")

    encoded = {size: png(rows) for size, rows in sizes.items()}

    files = {
        "32x32.png": 32,
        "128x128.png": 128,
        "128x128@2x.png": 256,
        "icon.png": 512,
    }
    for name, size in files.items():
        path = os.path.join(OUT, name)
        with open(path, "wb") as f:
            f.write(encoded[size])
        print(f"wrote {name}")

    # The icns types macOS looks for, from 16@2x up to 512@2x.
    layers = [
        (b"ic11", 32),
        (b"ic12", 64),
        (b"ic07", 128),
        (b"ic13", 256),
        (b"ic08", 256),
        (b"ic14", 512),
        (b"ic09", 512),
        (b"ic10", 1024),
    ]
    with open(os.path.join(OUT, "icon.icns"), "wb") as f:
        f.write(icns([(tag, encoded[size]) for tag, size in layers]))
    print("wrote icon.icns")


if __name__ == "__main__":
    main()
