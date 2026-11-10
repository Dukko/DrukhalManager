"""Frames a screenshot like a GNOME window floating over a blurred,
procedurally generated nebula: rounded corners, a hairline border and a
soft drop shadow.

    python3 docs/screenshots/frame.py shot.png framed.png [seed] [pad]
"""
import random
import sys

from PIL import Image, ImageChops, ImageDraw, ImageFilter

src, out = sys.argv[1], sys.argv[2]
seed = int(sys.argv[3]) if len(sys.argv) > 3 else 7
pad = int(sys.argv[4]) if len(sys.argv) > 4 else 96
rng = random.Random(seed)

shot = Image.open(src).convert("RGBA")
w, h = shot.size
W, H = w + 2 * pad, h + 2 * pad

# The nebula: deep space, a few big coloured clouds, then stars.
bg = Image.new("RGB", (W, H), (5, 6, 16))
# Broad, dim clouds, then smaller bright cores in a few colours, so the
# nebula has structure instead of one even wash.
palette = [(110, 40, 190), (30, 80, 200), (10, 150, 170), (190, 50, 120), (230, 120, 60)]
for count, size, strength, blur in [(7, (3, 2), (0.30, 0.55), 6), (16, (9, 5), (0.35, 0.8), 14)]:
    clouds = Image.new("RGB", (W, H), (0, 0, 0))
    draw = ImageDraw.Draw(clouds)
    for _ in range(count):
        r = rng.randint(min(W, H) // size[0], min(W, H) // size[1])
        # Clouds gather along a diagonal band, like a galactic arm.
        t = rng.random()
        x = int(t * W + rng.gauss(0, W / 7))
        y = int((1 - t) * H * 0.9 + rng.gauss(0, H / 6))
        c = rng.choice(palette)
        k = rng.uniform(*strength)
        draw.ellipse((x - r, y - r, x + r, y + r), fill=tuple(int(v * k) for v in c))
    bg = ImageChops.add(bg, clouds.filter(ImageFilter.GaussianBlur(min(W, H) // blur)))

stars = Image.new("RGB", (W, H), (0, 0, 0))
sd = ImageDraw.Draw(stars)
for _ in range(W * H // 2600):
    x, y = rng.randrange(W), rng.randrange(H)
    b = rng.randint(90, 255)
    s = rng.choice([0, 0, 0, 1, 1, 2])
    tint = rng.choice([(1, 1, 1), (0.8, 0.9, 1), (1, 0.9, 0.8)])
    sd.ellipse((x - s, y - s, x + s, y + s), fill=tuple(int(b * t) for t in tint))
# Blurred, as if out of focus behind the window, then the brightest again
# a little sharper so it still reads as stars.
bg = ImageChops.add(bg, stars.filter(ImageFilter.GaussianBlur(2.2)))
bg = ImageChops.add(bg, stars.filter(ImageFilter.GaussianBlur(1)).point(lambda v: v // 3))
bg = bg.filter(ImageFilter.GaussianBlur(1.5))
# Darker towards the edges.
vignette = Image.new("L", (W, H), 0)
ImageDraw.Draw(vignette).ellipse((-W * 0.25, -H * 0.25, W * 1.25, H * 1.25), fill=255)
vignette = vignette.filter(ImageFilter.GaussianBlur(min(W, H) // 5))
bg = Image.composite(bg, Image.new("RGB", (W, H), (0, 0, 0)), vignette.point(lambda v: 90 + v * 165 // 255))

# The window: rounded corners and a hairline light border.
radius = 14
mask = Image.new("L", (w, h), 0)
ImageDraw.Draw(mask).rounded_rectangle((0, 0, w - 1, h - 1), radius, fill=255)
window = Image.new("RGBA", (w, h), (0, 0, 0, 0))
window.paste(shot, (0, 0), mask)
border = Image.new("RGBA", (w, h), (0, 0, 0, 0))
ImageDraw.Draw(border).rounded_rectangle((0, 0, w - 1, h - 1), radius, outline=(255, 255, 255, 34), width=1)
window = Image.alpha_composite(window, border)

# A soft shadow, a little below the window.
shadow = Image.new("L", (W, H), 0)
ImageDraw.Draw(shadow).rounded_rectangle((pad, pad + 18, pad + w, pad + h + 18), radius, fill=170)
shadow = shadow.filter(ImageFilter.GaussianBlur(28))
canvas = bg.convert("RGBA")
canvas = Image.composite(Image.new("RGBA", (W, H), (0, 0, 0, 255)), canvas, shadow)
canvas.alpha_composite(window, (pad, pad))

# Rounded outer corners too, so it sits nicely on GitHub's page.
outer = Image.new("L", (W, H), 0)
ImageDraw.Draw(outer).rounded_rectangle((0, 0, W - 1, H - 1), 24, fill=255)
canvas.putalpha(outer)
canvas.save(out, optimize=True)
print(out, canvas.size)
