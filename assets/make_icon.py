"""Draws hydra's icon: three heads (environment colours) rising from one neck on a dark tile.

Run from the repo root:  python assets/make_icon.py
Writes assets/hydra.ico (16-256 px), assets/hydra.png (512 px) and site/favicon.png (64 px).
"""
from PIL import Image, ImageDraw

TILE = (24, 34, 44)          # dark slate, the guide's terminal background
NECK_BASE = (219, 227, 234)  # light, the guide's terminal text colour
HEADS = [                    # the guide's work / personal / client colours (bright variants)
    ((0.25, 0.33), (60, 194, 174)),
    ((0.50, 0.21), (231, 170, 69)),
    ((0.75, 0.33), (164, 139, 236)),
]
BASE = (0.50, 0.84)


def bezier(p0, p1, p2, steps=64):
    return [
        (
            (1 - t) ** 2 * p0[0] + 2 * (1 - t) * t * p1[0] + t ** 2 * p2[0],
            (1 - t) ** 2 * p0[1] + 2 * (1 - t) * t * p1[1] + t ** 2 * p2[1],
        )
        for t in (i / steps for i in range(steps + 1))
    ]


def draw(size: int) -> Image.Image:
    ss = 8                                  # supersample, then downscale for clean edges
    n = size * ss
    img = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    d.rounded_rectangle([0, 0, n - 1, n - 1], radius=int(n * 0.22), fill=TILE)

    # Small icons need relatively thicker shapes to stay readable.
    neck_w = max(0.085, 1.5 / size)
    head_r = max(0.105, 2.2 / size)
    px = lambda p: (p[0] * n, p[1] * n)

    for (hx, hy), colour in HEADS:
        ctrl = (0.5 + (hx - 0.5) * 0.15, 0.58)
        # A smooth thick stroke: stamp overlapping discs along the curve.
        r = neck_w * n / 2
        for x, y in (px(p) for p in bezier(BASE, ctrl, (hx, hy), steps=400)):
            d.ellipse([x - r, y - r, x + r, y + r], fill=colour)

    for (hx, hy), colour in HEADS:
        x, y = px((hx, hy))
        r = head_r * n
        d.ellipse([x - r, y - r, x + r, y + r], fill=colour)

    bx, by = px(BASE)
    br = max(0.095, 2.0 / size) * n
    d.ellipse([bx - br, by - br, bx + br, by + br], fill=NECK_BASE)
    return img.resize((size, size), Image.LANCZOS)


if __name__ == "__main__":
    sizes = [16, 24, 32, 48, 64, 128, 256]
    frames = [draw(s) for s in sizes]
    frames[-1].save("assets/hydra.ico", sizes=[(s, s) for s in sizes], append_images=frames[:-1])
    draw(512).save("assets/hydra.png")
    draw(64).save("site/favicon.png")
