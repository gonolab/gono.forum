"""Export the Gono Forum icon bitmaps from mark.svg.

    pip install cairosvg pillow
    python design/gono-brand/build-icons.py

Sizes match the slots the web app serves today: app-192/512 (web app
manifest), apple-touch-icon (180), favicon-16/32 and a 16/32/48 favicon.ico.
"""
import re
from io import BytesIO
from pathlib import Path

import cairosvg
from PIL import Image

HERE = Path(__file__).resolve().parent
SUPERSAMPLE = 4


def raster(svg, size):
    data = cairosvg.svg2png(bytestring=svg, output_width=size * SUPERSAMPLE,
                            output_height=size * SUPERSAMPLE)
    image = Image.open(BytesIO(data)).convert('RGBA')
    return image.resize((size, size), Image.Resampling.LANCZOS)


def main():
    mark = (HERE / 'mark.svg').read_bytes()
    # iOS rounds the corners itself and paints transparent pixels black,
    # so the touch icon is the same tile without the corner radius.
    square = re.sub(rb' rx="[^"]*"', b'', mark, count=1)

    raster(mark, 512).save(HERE / 'app-512.png', optimize=True)
    raster(mark, 192).save(HERE / 'app-192.png', optimize=True)
    raster(square, 180).convert('RGB').save(HERE / 'apple-touch-icon.png', optimize=True)

    icons = {size: raster(mark, size) for size in (16, 32, 48)}
    for size in (16, 32):
        icons[size].save(HERE / f'favicon-{size}.png', optimize=True)
    icons[48].save(HERE / 'favicon.ico', sizes=[(16, 16), (32, 32), (48, 48)],
                   append_images=[icons[16], icons[32]])


if __name__ == '__main__':
    main()
