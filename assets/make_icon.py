"""Draws hydra's icon: three heads (environment colours) rising from one neck on a dark tile.

Run from the repo root:  python assets/make_icon.py   (needs Pillow)
Writes:
  assets/hydra.ico         16-256 px, embedded in hydra.exe
  assets/hydra.png         512 px, the README's logo
  assets/hydra-64.png      hydraui's title bar
  site/favicon.png         64 px
  assets/hydra.icon/       the macOS 26+ Liquid Glass icon (Icon Composer format: layered SVGs
                           plus icon.json); scripts/macos/make-app.sh compiles it with actool
  assets/hydra-macos.png   1024 px on Apple's macOS icon grid (824 px tile, 100 px margin, drop
                           shadow): the .icns for older macOS and the Dock icon of a bare binary
"""
import json
import os

from PIL import Image, ImageChops, ImageDraw, ImageFilter

TILE = (24, 34, 44)          # dark slate, the guide's terminal background
TILE_TOP = (40, 54, 68)      # a lighter slate for the tile's top, as Liquid Glass lights it
NECK_BASE = (219, 227, 234)  # light, the guide's terminal text colour
HEADS = [                    # the guide's work / personal / client colours (bright variants)
    ((0.25, 0.33), (60, 194, 174)),
    ((0.50, 0.21), (231, 170, 69)),
    ((0.75, 0.33), (164, 139, 236)),
]
BASE = (0.50, 0.84)

# Proportions of the tile (0-1), used at sizes where the minimum-pixel rules don't kick in.
NECK_W = 0.085
HEAD_R = 0.105
BASE_R = 0.095

# Apple's macOS grid: on a 1024 canvas the tile is 824 px, inset 100 px, corner radius ~185.
MAC_CANVAS = 1024
MAC_TILE = 824
MAC_RADIUS = 0.2237      # of the tile

# Liquid Glass refracts and lights each layer's edges, so the glyph gets a little more room
# inside the masked shape than on the flat tile.
GLASS_SCALE = 0.9


def bezier(p0, p1, p2, steps=64):
    return [
        (
            (1 - t) ** 2 * p0[0] + 2 * (1 - t) * t * p1[0] + t ** 2 * p2[0],
            (1 - t) ** 2 * p0[1] + 2 * (1 - t) * t * p1[1] + t ** 2 * p2[1],
        )
        for t in (i / steps for i in range(steps + 1))
    ]


def neck_ctrl(hx):
    return (0.5 + (hx - 0.5) * 0.15, 0.58)


def draw_glyph(d, px, n, neck_w=NECK_W, head_r=HEAD_R, base_r=BASE_R):
    """Draws the necks, heads and base; `px` maps tile coordinates (0-1) to pixels, `n` is the
    tile's size in pixels."""
    for (hx, hy), colour in HEADS:
        # A smooth thick stroke: stamp overlapping discs along the curve.
        r = neck_w * n / 2
        for x, y in (px(p) for p in bezier(BASE, neck_ctrl(hx), (hx, hy), steps=400)):
            d.ellipse([x - r, y - r, x + r, y + r], fill=colour)
    for (hx, hy), colour in HEADS:
        x, y = px((hx, hy))
        r = head_r * n
        d.ellipse([x - r, y - r, x + r, y + r], fill=colour)
    bx, by = px(BASE)
    r = base_r * n
    d.ellipse([bx - r, by - r, bx + r, by + r], fill=NECK_BASE)


def draw(size: int) -> Image.Image:
    """The flat, full-bleed tile: Windows, the README, the title bar and the site."""
    ss = 8                                  # supersample, then downscale for clean edges
    n = size * ss
    img = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    d.rounded_rectangle([0, 0, n - 1, n - 1], radius=int(n * 0.22), fill=TILE)
    # Small icons need relatively thicker shapes to stay readable.
    draw_glyph(
        d,
        lambda p: (p[0] * n, p[1] * n),
        n,
        neck_w=max(NECK_W, 1.5 / size),
        head_r=max(HEAD_R, 2.2 / size),
        base_r=max(BASE_R, 2.0 / size),
    )
    return img.resize((size, size), Image.LANCZOS)


def glass_point(p):
    """Tile coordinates (0-1) pulled toward the centre by GLASS_SCALE."""
    return (0.5 + (p[0] - 0.5) * GLASS_SCALE, 0.5 + (p[1] - 0.5) * GLASS_SCALE)


def vertical_gradient(w, h, top, bottom):
    grad = Image.new("RGBA", (1, h))
    for y in range(h):
        t = y / max(1, h - 1)
        grad.putpixel((0, y), tuple(round(a + (b - a) * t) for a, b in zip(top, bottom)) + (255,))
    return grad.resize((w, h))


def draw_macos() -> Image.Image:
    """The icon on Apple's macOS grid, lit the way Liquid Glass lights it: a softly graded tile,
    a bright rim along its top edge, a sheen across the glyph and a drop shadow under it all."""
    ss = 4
    n = MAC_CANVAS * ss
    tile = MAC_TILE * ss
    off = (n - tile) // 2
    radius = int(tile * MAC_RADIUS)
    box = [off, off, off + tile - 1, off + tile - 1]

    mask = Image.new("L", (n, n), 0)
    ImageDraw.Draw(mask).rounded_rectangle(box, radius=radius, fill=255)

    # Shadow: the tile's shape, blurred and dropped a little.
    shadow = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    shadow_alpha = mask.point(lambda a: a * 0.45).filter(ImageFilter.GaussianBlur(14 * ss))
    shadow.putalpha(ImageChops.offset(shadow_alpha, 0, 10 * ss))
    img = shadow

    # The tile, graded from a lighter top to the guide's slate.
    body = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    body.paste(vertical_gradient(tile, tile, TILE_TOP, TILE), (off, off))
    img.paste(body, (0, 0), mask)

    # The glyph, then a sheen over its top half.
    glyph = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    gp = lambda p: (off + glass_point(p)[0] * tile, off + glass_point(p)[1] * tile)
    draw_glyph(ImageDraw.Draw(glyph), gp, tile * GLASS_SCALE)
    sheen = vertical_gradient(n, n, (255, 255, 255), (255, 255, 255))
    sheen_alpha = Image.linear_gradient("L").rotate(180).point(lambda a: max(0, a - 128) * 0.55)
    sheen.putalpha(ImageChops.multiply(sheen_alpha.resize((n, n)), glyph.getchannel("A")))
    img.alpha_composite(glyph)
    img.alpha_composite(sheen)

    # Rim light: a thin bright line along the tile's top edge, fading out down the sides.
    rim = Image.new("L", (n, n), 0)
    ImageDraw.Draw(rim).rounded_rectangle(box, radius=radius, outline=255, width=3 * ss)
    fade = Image.linear_gradient("L").rotate(180).resize((n, n)).point(lambda a: max(0, a - 150) * 1.4)
    rim = ImageChops.multiply(rim, fade)
    img.alpha_composite(Image.merge("RGBA", (*[Image.new("L", (n, n), 255)] * 3, rim.point(lambda a: a * 0.5))))

    return img.resize((MAC_CANVAS, MAC_CANVAS), Image.LANCZOS)


def hex_colour(c):
    return "#%02x%02x%02x" % c


def svg(body: str) -> str:
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{MAC_CANVAS}" height="{MAC_CANVAS}" '
        f'viewBox="0 0 {MAC_CANVAS} {MAC_CANVAS}">\n{body}</svg>\n'
    )


def write_icon_bundle(path: str):
    """The Icon Composer bundle. Each layer is a full 1024 canvas; the system masks it to the
    icon's shape, renders each group as Liquid Glass and derives the dark, tinted and clear
    appearances itself. Groups are listed front to back."""
    s = MAC_CANVAS
    pt = lambda p: (glass_point(p)[0] * s, glass_point(p)[1] * s)
    f = lambda v: f"{v:.1f}"

    necks = ""
    for (hx, hy), colour in HEADS:
        (bx, by), (cx, cy), (ex, ey) = pt(BASE), pt(neck_ctrl(hx)), pt((hx, hy))
        necks += (
            f'  <path d="M{f(bx)} {f(by)} Q{f(cx)} {f(cy)} {f(ex)} {f(ey)}" fill="none" '
            f'stroke="{hex_colour(colour)}" stroke-width="{f(NECK_W * s * GLASS_SCALE)}" '
            f'stroke-linecap="round"/>\n'
        )
    heads = ""
    for p, colour in HEADS:
        x, y = pt(p)
        heads += f'  <circle cx="{f(x)}" cy="{f(y)}" r="{f(HEAD_R * s * GLASS_SCALE)}" fill="{hex_colour(colour)}"/>\n'
    bx, by = pt(BASE)
    base = f'  <circle cx="{f(bx)}" cy="{f(by)}" r="{f(BASE_R * s * GLASS_SCALE)}" fill="{hex_colour(NECK_BASE)}"/>\n'

    assets = os.path.join(path, "Assets")
    os.makedirs(assets, exist_ok=True)
    for name, body in (("necks.svg", necks), ("heads.svg", heads), ("base.svg", base)):
        with open(os.path.join(assets, name), "w") as out:
            out.write(svg(body))

    srgb = lambda c: "srgb:" + ",".join(f"{v / 255:.5f}" for v in c) + ",1.00000"
    group = lambda layers, translucency: {
        "layers": [{"glass": True, "image-name": f"{name}.svg", "name": name} for name in layers],
        "shadow": {"kind": "layer-color", "opacity": 0.5},
        "translucency": {"enabled": True, "value": translucency},
    }
    manifest = {
        "fill": {"automatic-gradient": srgb(TILE)},
        # Front to back: the base knot and the heads sit above the necks they grow from.
        "groups": [group(["base", "heads"], 0.2), group(["necks"], 0.4)],
        "supported-platforms": {"squares": "shared"},
    }
    with open(os.path.join(path, "icon.json"), "w") as out:
        json.dump(manifest, out, indent=2)
        out.write("\n")


if __name__ == "__main__":
    sizes = [16, 24, 32, 48, 64, 128, 256]
    frames = [draw(s) for s in sizes]
    frames[-1].save("assets/hydra.ico", sizes=[(s, s) for s in sizes], append_images=frames[:-1])
    draw(512).save("assets/hydra.png")
    draw(64).save("assets/hydra-64.png")
    draw(64).save("site/favicon.png")
    draw_macos().save("assets/hydra-macos.png")
    write_icon_bundle("assets/hydra.icon")
