# Gono Forum brand assets

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="wordmark-dark.svg">
  <img src="wordmark.svg" alt="Gono Forum" height="64">
</picture>
&nbsp;&nbsp;
<img src="mark.svg" alt="" width="64" height="64">

The default identity of Gono Forum. The wordmark spells **Gono**, and its capital
G doubles as a speech bubble: the ring carries a tail at the lower left. The mark
is that G on its own, white on an indigo tile, so the favicon, the app icon and
the header all point at the same shape.

## Files

| File | What it is |
| --- | --- |
| `wordmark.svg` | Wordmark for light backgrounds, indigo `#4f46e5`. Intrinsic size 107 × 32. |
| `wordmark-dark.svg` | Same wordmark for dark backgrounds, `#818cf8`. |
| `mark.svg` | Square mark, 512 × 512: indigo tile with a 112 corner radius and a white G. |
| `app-512.png`, `app-192.png` | Web app manifest icons, rounded tile with transparent corners. |
| `apple-touch-icon.png` | 180 × 180, square tile without rounded corners (see below). |
| `favicon-16.png`, `favicon-32.png` | Browser tab icons. |
| `favicon.ico` | 16, 32 and 48 px in one file. |
| `draw.py` | Builds the three SVGs from the letter geometry. |
| `build-icons.py` | Exports the PNG and ICO files from `mark.svg`. |

The bitmap names and sizes match the slots the web app already serves, so they
can replace the current files one for one.

## Design

- **Construction.** Monoline geometric letters drawn on a centerline from
  circles, arcs and straight strokes, with round terminals. Units: x-height 100,
  cap height 140, stroke 20 (one fifth of the x-height); round letters overshoot
  the x-height and baseline by 2.
- **G.** A ring that opens at the upper right (−38°, measured clockwise from
  3 o'clock) and a bar that runs in from the right edge to 18% of the radius from
  the centre. The tail leaves the ring between 118° and 158° and points down and
  left, ending about 5 units below the baseline. Its tip is softened by a
  2-unit radius so it matches the round terminals.
- **o** is a full ring, 104 wide. **n** is a half ring on two stems, 92 wide.
- **Spacing.** G to o 14, o to n 20, n to o 20.
- **Mark.** The same G drawn at stroke 24 instead of 20, a small weight boost so
  it holds up at 16 px. It fills 62% of the tile, centred on its bounds; the tile
  is 512 with a 112 corner radius (about 22%).
- Every SVG is plain filled paths: no fonts, no `<text>`, no strokes, no
  transforms. The wordmark is a single path; the mark is the tile plus one path.

## Colour

| Role | Value | In the default theme |
| --- | --- | --- |
| Indigo | `#4f46e5` | light `--accent`, `--mark`, `--brand-bg`; `--choice-accent` |
| Indigo for dark mode | `#818cf8` | dark `--accent`, `--mark` |
| White | `#ffffff` | the G on the tile |

Contrast: `#4f46e5` on `#ffffff` 6.3 : 1, on `#f7f8fb` 5.9 : 1; `#818cf8` on
`#0a0f1d` 6.4 : 1, on `#121827` 5.9 : 1; white on the indigo tile 6.3 : 1.

## Usage

- **Light backgrounds** (`#ffffff`, `#f7f8fb`): `wordmark.svg`.
- **Dark backgrounds** (`#0a0f1d`, `#121827`): `wordmark-dark.svg`. Do not put
  the indigo wordmark on a dark background; it drops to about 3 : 1.
- **On the indigo itself, or on a photo:** use the wordmark in one colour, white.
  Each wordmark file is one path, so this only means changing its `fill`.
- **The mark** works on both light and dark backgrounds as it is. The tile keeps
  6.3 : 1 against white and 3.0 : 1 against `#0a0f1d`; on `#121827` panels the
  tile edge is softer (2.8 : 1) but the white G still reads.
- **Clear space.** Keep at least half the x-height free around the wordmark; at
  32 px tall that is about 11 px. Keep an eighth of the tile's size around the
  mark.
- **Minimum size.** Wordmark 20 px tall on screen (the header uses 28–32 px);
  below that, use the mark. The mark goes down to 16 px; at 16 and 32 px use the
  exported PNGs rather than letting the browser scale the SVG.
- **Do not** colour the tail or single letters differently (they are one shape),
  stretch, outline, rotate or add shadows and gradients, move the tail to another
  corner, or set "Gono" in a typeface as a stand-in for the wordmark.

`apple-touch-icon.png` is a square without rounded corners because iOS applies
its own mask and fills transparent pixels with black. The manifest icons keep
the rounded tile.

## Rebuilding

```bash
pip install skia-pathops fonttools cairosvg pillow
python design/gono-brand/draw.py
python design/gono-brand/build-icons.py
```

`draw.py` writes the three SVGs; change the constants at its top (stroke, tail
angles, opening, spacing) rather than editing path data by hand.
`build-icons.py` renders `mark.svg` at four times the target size with CairoSVG
and scales it down with Lanczos.

## Licence

These files are original artwork made for this project. Every shape is
constructed in `draw.py` from circles, arcs and straight lines; no third-party
fonts, icon sets or other artwork were used, traced or adapted.

They are brand assets and are **not** covered by the MIT / Apache-2.0 licence
that applies to the Gono Forum source code. Copyright © 2026 gonolab. All
rights reserved.
