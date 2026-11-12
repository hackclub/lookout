#!/usr/bin/env python3
"""Regenerate assets/app-icon-macos.png from the full-bleed icon-full-bleed.icns.

Puts the artwork on Apple's macOS icon grid (824px rounded square inset 100px
in 1024, radius 185.4, template shadow 0 10px 10px 30% black). macOS before 26
draws .icns files as-is, so full-bleed art shows as a square there.

Usage: python3 make-icon.py clients/native/packaging/macos/icon-full-bleed.icns clients/native/assets/app-icon-macos.png
(needs Pillow)
"""
import sys
from PIL import Image, ImageDraw, ImageFilter
src, out = sys.argv[1], sys.argv[2]
f = Image.open(src)
art = f.icns.getimage((512, 512, 2)).convert('RGBA')  # 1024px full-bleed artwork
N, inset, body, radius = 1024, 100, 824, 185.4
SS = 4
mask = Image.new('L', (N * SS, N * SS), 0)
ImageDraw.Draw(mask).rounded_rectangle(
    [inset * SS, inset * SS, (inset + body) * SS - 1, (inset + body) * SS - 1],
    radius=radius * SS, fill=255)
mask = mask.resize((N, N), Image.LANCZOS)
artwork = art.resize((body, body), Image.LANCZOS)
layer = Image.new('RGBA', (N, N), (0, 0, 0, 0))
layer.paste(artwork, (inset, inset))
layer.putalpha(mask)
# Apple's Big Sur template shadow: 0 10px 10px rgba(0,0,0,0.3)
sh = Image.new('RGBA', (N, N), (0, 0, 0, 0))
sh.putalpha(mask.point(lambda a: int(a * 0.3)))
sh = sh.transform(sh.size, Image.AFFINE, (1, 0, 0, 0, 1, -10)).filter(ImageFilter.GaussianBlur(5))
canvas = Image.alpha_composite(sh, layer)
canvas.save(out)
