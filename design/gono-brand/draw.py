"""Draw the Gono Forum wordmark and mark as outlined SVG paths.

    pip install skia-pathops fonttools
    python design/gono-brand/draw.py

Every letter is built on a centerline from circles, arcs and straight strokes,
then stroked, merged and written out as one filled path per file: no fonts,
no <text>. Units: x-height 100 (y 0..100, baseline at 100), cap height 140,
stroke 20, round shapes overshoot by 2.
"""
import math
from pathlib import Path

import pathops
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.svgLib.path import parse_path

HERE = Path(__file__).resolve().parent

INDIGO = '#4f46e5'       # default theme: light --accent and --choice-accent
INDIGO_DARK = '#818cf8'  # default theme: dark --accent
WHITE = '#ffffff'

STROKE = 20
MARK_STROKE = 24         # the G is drawn a little heavier for icon sizes
OVERSHOOT = 2
TAIL = (118, 158)        # degrees (clockwise from 3 o'clock) where the tail leaves the G
OPENING = -38            # where the G's upper terminal ends


# --- geometry ---------------------------------------------------------------

def parse(d, matrix=(1, 0, 0, 1, 0, 0)):
    path = pathops.Path()
    parse_path(d, TransformPen(path.getPen(), matrix))
    return path


def stroke(d, width):
    path = parse(d)
    path.stroke(width, pathops.LineCap.ROUND_CAP, pathops.LineJoin.ROUND_JOIN, 4)
    path.convertConicsToQuads()
    return path


def union(paths):
    out = pathops.Path()
    for path in paths:
        out = pathops.op(out, path, pathops.PathOp.UNION, fix_winding=True)
    return out


def point(cx, cy, r, deg):
    a = math.radians(deg)
    return f'{cx + r * math.cos(a):.3f} {cy + r * math.sin(a):.3f}'


def circle(cx, cy, r):
    return f'M{cx + r} {cy}A{r} {r} 0 1 1 {cx - r} {cy}A{r} {r} 0 1 1 {cx + r} {cy}Z'


def arc(cx, cy, r, start, end):
    large = 1 if abs(end - start) > 180 else 0
    sweep = 1 if end > start else 0
    return f'M{point(cx, cy, r, start)}A{r} {r} 0 {large} {sweep} {point(cx, cy, r, end)}'


# --- letters ----------------------------------------------------------------

def letter_G(x, w=STROKE, cap=140):
    """Capital G whose outline is a speech bubble: tail at the lower left."""
    outer = cap / 2 + OVERSHOOT
    r = outer - w / 2
    cx, cy = x + outer, 100 - cap / 2
    bowl = stroke(arc(cx, cy, r, OPENING, -360), w)
    bar = stroke(f'M{cx + r} {cy}H{cx + r * 0.18}', w)
    a, b = TAIL
    tip = f'{cx - outer * 0.98:.3f} {cy + outer * 1.02:.3f}'
    wedge = (f'M{point(cx, cy, r, a)}L{point(cx, cy, outer - 1, a)}L{tip}'
             f'L{point(cx, cy, outer - 1, b)}L{point(cx, cy, r, b)}Z')
    tail = union([parse(wedge), stroke(wedge, 4)])  # the thin stroke softens the tip
    return [bowl, bar, tail], 2 * outer


def letter_o(x, w=STROKE):
    outer = 50 + OVERSHOOT
    return [stroke(circle(x + outer, 50, outer - w / 2), w)], 2 * outer


def letter_n(x, w=STROKE, width=92):
    r = (width - w) / 2
    cy = r + w / 2 - OVERSHOOT
    left, right, foot = x + w / 2, x + width - w / 2, 100 - w / 2
    return [stroke(f'M{left} {foot}V{cy}A{r} {r} 0 0 1 {right} {cy}V{foot}', w)], width


def set_word(letters):
    """letters: [(draw, space before)]"""
    x, parts = 0, []
    for draw, space in letters:
        x += space
        shapes, advance = draw(x)
        parts += shapes
        x += advance
    return union(parts)


# --- output -----------------------------------------------------------------

class CubicPen(SVGPathPen):
    """Write quadratic segments as the equivalent cubics; some rasterizers draw SVG Q curves badly."""

    def _qCurveToOne(self, pt1, pt2):
        x0, y0 = self._getCurrentPoint()
        c1 = (x0 + 2 / 3 * (pt1[0] - x0), y0 + 2 / 3 * (pt1[1] - y0))
        c2 = (pt2[0] + 2 / 3 * (pt1[0] - pt2[0]), pt2[1] + 2 / 3 * (pt1[1] - pt2[1]))
        self._curveToOne(c1, c2, pt2)


def path_data(path):
    def number(v):
        text = f'{v:.2f}'.rstrip('0').rstrip('.')
        return '0' if text in ('-0', '') else text
    pen = CubicPen(None, ntos=number)
    path.draw(pen)
    return pen.getCommands()


def fit(path, matrix):
    return path.transform(*matrix)


def write(name, width, height, view, body):
    svg = (f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" '
           f'viewBox="{view}" role="img" aria-label="Gono Forum">\n'
           f'  <title>Gono Forum</title>\n{body}</svg>\n')
    (HERE / name).write_text(svg, encoding='utf-8')


def main():
    word = set_word([(letter_G, 0), (letter_o, 14), (letter_n, 20), (letter_o, 20)])
    x0, y0, x1, y1 = word.bounds
    x0, y0, x1, y1 = math.floor(x0), math.floor(y0), math.ceil(x1), math.ceil(y1)
    word = fit(word, (1, 0, 0, 1, -x0, -y0))
    w, h = x1 - x0, y1 - y0
    height = 32
    width = round(height * w / h, 2)
    for name, color in (('wordmark.svg', INDIGO), ('wordmark-dark.svg', INDIGO_DARK)):
        write(name, width, height, f'0 0 {w} {h}', f'  <path fill="{color}" d="{path_data(word)}"/>\n')

    size, radius, share = 512, 112, 0.62
    shapes, _ = letter_G(0, w=MARK_STROKE)
    glyph = union(shapes)
    gx0, gy0, gx1, gy1 = glyph.bounds
    scale = share * size / max(gx1 - gx0, gy1 - gy0)
    glyph = fit(glyph, (scale, 0, 0, scale,
                        size / 2 - scale * (gx0 + gx1) / 2, size / 2 - scale * (gy0 + gy1) / 2))
    write('mark.svg', size, size, f'0 0 {size} {size}',
          f'  <rect width="{size}" height="{size}" rx="{radius}" fill="{INDIGO}"/>\n'
          f'  <path fill="{WHITE}" d="{path_data(glyph)}"/>\n')


if __name__ == '__main__':
    main()
