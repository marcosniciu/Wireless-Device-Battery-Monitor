"""Draws the BatteryHub icon (res/batteryhub.ico) with Pillow.

A rounded blue tile with a white battery outline, a green charge level and a small lightning
bolt. Every size is drawn at 4x and downscaled, and the smallest sizes get thicker strokes so
they stay readable on the taskbar. Run: python res/make_icon.py
"""
from pathlib import Path

from PIL import Image, ImageDraw

SIZES = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256]
SS = 4  # supersampling factor


def lerp(a, b, t):
    return tuple(round(x + (y - x) * t) for x, y in zip(a, b))


def draw(size: int) -> Image.Image:
    S = size * SS
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))

    # Tile: vertical gradient, rounded corners.
    top, bottom = (38, 132, 255), (18, 72, 196)
    grad = Image.new("RGBA", (S, S))
    gd = ImageDraw.Draw(grad)
    for y in range(S):
        gd.line([(0, y), (S, y)], fill=lerp(top, bottom, y / max(1, S - 1)) + (255,))
    mask = Image.new("L", (S, S), 0)
    margin = round(S * 0.04)
    ImageDraw.Draw(mask).rounded_rectangle([margin, margin, S - margin - 1, S - margin - 1], radius=round(S * 0.22), fill=255)
    img.paste(grad, (0, 0), mask)

    d = ImageDraw.Draw(img)
    small = size <= 24
    stroke = round(S * (0.075 if small else 0.055))
    # Battery body (horizontal) and terminal nub.
    bx0, by0 = round(S * 0.16), round(S * 0.30)
    bx1, by1 = round(S * 0.78), round(S * 0.70)
    d.rounded_rectangle([bx0, by0, bx1, by1], radius=round(S * 0.08), outline=(255, 255, 255, 255), width=stroke)
    nub_h = round((by1 - by0) * 0.42)
    ny0 = (by0 + by1) // 2 - nub_h // 2
    d.rounded_rectangle([bx1 + round(S * 0.01), ny0, bx1 + round(S * 0.075), ny0 + nub_h], radius=round(S * 0.025), fill=(255, 255, 255, 255))

    # Charge level (about 70%).
    pad = stroke + round(S * (0.035 if small else 0.03))
    fx0, fy0, fx1, fy1 = bx0 + pad, by0 + pad, bx1 - pad, by1 - pad
    level_x = fx0 + round((fx1 - fx0) * 0.70)
    d.rounded_rectangle([fx0, fy0, level_x, fy1], radius=round(S * 0.03), fill=(76, 217, 100, 255))

    # Lightning bolt over the battery (skipped at the tiniest sizes, where it would be noise).
    if size >= 32:
        cx, cy = (bx0 + bx1) / 2, (by0 + by1) / 2
        h = (by1 - by0) * 0.80
        w = h * 0.72
        pts = [
            (cx + w * 0.10, cy - h / 2),
            (cx - w * 0.45, cy + h * 0.06),
            (cx - w * 0.02, cy + h * 0.06),
            (cx - w * 0.12, cy + h / 2),
            (cx + w * 0.45, cy - h * 0.08),
            (cx + w * 0.02, cy - h * 0.08),
        ]
        outline = [(x, y) for x, y in pts]
        d.polygon(outline, fill=(255, 255, 255, 255), outline=(18, 72, 196, 255))

    return img.resize((size, size), Image.LANCZOS)


def main() -> None:
    out = Path(__file__).with_name("batteryhub.ico")
    images = [draw(s) for s in SIZES]
    images[-1].save(out, format="ICO", sizes=[(s, s) for s in SIZES], append_images=images[:-1])
    images[-1].save(out.with_suffix(".png"))
    print(f"wrote {out} ({out.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
