# Assets

A register of the files the interface ships that are not ordinary source code: fonts, brand images, icons, vendored
scripts and the color scheme, with where each comes from and its licence. Gono Forum's own code is licensed under
MIT OR Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE`). `NOTICE` summarizes the exceptions and the third-party files, and
their licence texts are in `docs/licenses/`. When you add or replace an asset, update this register, `NOTICE` and
`docs/licenses/` in the same change. How styles and icons are written is in [styles.md](styles.md).

## Fonts

| File | What it is | Licence | SHA-256 |
| --- | --- | --- | --- |
| `apps/web/static/fonts/space-mono-latin-400-normal.woff2` | Space Mono Regular, latin subset | OFL-1.1 | `fb4a81a2d0a893e5c38c394a7e716a1cef0b24610a0af49c96f6d529bd66bf2b` |
| `apps/web/static/fonts/space-mono-latin-700-normal.woff2` | Space Mono Bold, latin subset | OFL-1.1 | `2d46bd159b53f55c41167a4f1540a074649464194fd1e416f5b4694a6c0f282c` |
| `apps/web/static/fonts/space-mono-latin-ext-400-normal.woff2` | Space Mono Regular, latin-ext subset | OFL-1.1 | `44ad50c760fa8f5b62da3d489edb40adebabaf97988b9b0adbbc3f0be859050a` |
| `apps/web/static/fonts/space-mono-latin-ext-700-normal.woff2` | Space Mono Bold, latin-ext subset | OFL-1.1 | `52edd9a8959ec3e68d618e2a4757182c0e61bfeea61679850cad8744a68cadf3` |
| `apps/web/static/css/fonts.css` | The `@font-face` rules for the four files, with their `unicode-range` | MIT OR Apache-2.0 (project code) | |

Space Mono is by The Space Mono Project Authors ([googlefonts/spacemono](https://github.com/googlefonts/spacemono)).
The files report version 1.003 in their name table and carry the copyright notice of The Space Mono Project Authors
with a link to the Open Font License. Nothing in the repository builds the subsets; they are shipped as they
are. Licence text: [licenses/Space-Mono-OFL-1.1.txt](licenses/Space-Mono-OFL-1.1.txt), which declares no Reserved
Font Name. Only monospaced text uses the font (`--font-mono`: code, digits); everything else uses the reader's system
fonts.

## Brand images

| File | What it is | SHA-256 |
| --- | --- | --- |
| `apps/web/static/img/favicon-32.png` | Browser tab icon, linked from every page | `5f5b0a9706206cadd0a5ddf03f73d89f849218b7e1dccb5b0815a078bc782a39` |
| `apps/web/static/img/apple-touch-icon.png` | Touch icon, 180 × 180 | `25be2113f301db16fad34a8764a0e066b1df40b955ca006463e94a0801e1e5c5` |
| `apps/web/static/img/app-192.png` | Web app manifest icon | `112c30be48de3aa86765684e0c1472a3aaae5f2b4bf10e608cfa2fcc9c2fae84` |
| `apps/web/static/img/app-512.png` | Web app manifest icon | `b96691ee0fecb064af0cb3df610b80b492db999b73515976a4f94ca5548c43ff` |
| `apps/web/static/img/favicon-16.png` | Browser tab icon, 16px | `02574a5ff4da2bfc6fdfbbef524c0b7194cabdd7ec505d3e42df489aa3f7b862` |
| `apps/web/static/img/favicon.ico` | 16, 32 and 48px in one file | `8c6b308eb3fc9c10330eaa666c410ed82fa6863b0c1a1edfff7418b4e8976d63` |

The six files are byte-for-byte copies of the exports in `design/gono-brand/`. That directory holds the Gono mark and
wordmark (`mark.svg`, `wordmark.svg`, `wordmark-dark.svg`), the script that constructs them from circles, arcs and
straight strokes (`draw.py`), and the script that exports the bitmaps from `mark.svg` (`build-icons.py`); its
[README](../design/gono-brand/README.md) describes the construction and usage. `favicon-16.png` and `favicon.ico` are
served under `/static/img/` but no page links them. The interface does not use the wordmarks: the header shows the
site name as text. A site can replace the tab, touch and app icons with its own uploads in the admin area; these files
are the defaults.

**Licence:** original artwork made for this project, and **not** covered by MIT OR Apache-2.0. `NOTICE` excludes
`design/gono-brand/` and the icons built from it in `apps/web/static/img/`, and the brand README reserves all rights.

## Icons

All interface icons are inline SVG drawn for this project. They are part of the source code and are licensed with it
(MIT OR Apache-2.0).

| Where | Icons |
| --- | --- |
| `apps/web/templates/partials/header.html` | Menu, chevrons, search, and the three theme switch icons (system, light, dark) |
| `apps/web/templates/home/index.html`, `partials/cross_pins.html`, `topic/detail.html`, `partials/user_icon.html` | List metadata: child board arrow, member, replies |
| `apps/web/templates/partials/favorite_icon.html`, `reaction_icon.html`, `downvote_icon.html`, `tip_icon.html` | Favorite, like, downvote, tip |
| `apps/web/templates/partials/user_card.html`, `partials/my_card.html` | Member card shortcuts and menu |
| `apps/web/templates/layout/base.html` | Lightning bolt before the footer performance line |
| `apps/web/templates/partials/manage_icons.html` | Ten admin module icons, as `<symbol>` elements |
| `apps/web/frontend/styles/composer.css` | Editor toolbar: 14 SVG masks as `data:` URIs for 13 tools (full screen has two states) |

## Scripts

| File | What it is | Licence | SHA-256 |
| --- | --- | --- | --- |
| `apps/web/static/js/vendor/htmx.min.js` | htmx 2.0.10, `dist/htmx.min.js` from the npm package `htmx.org` | 0BSD, text in [licenses/htmx-0BSD.txt](licenses/htmx-0BSD.txt) | `4b2fd977b3ae23ae06329f0168486647c60413405200c0a10fb5e6f8d3700998` |
| `apps/web/static/js/vendor/head-support.js` | The htmx head-support extension, `head-support.js` from the npm package `htmx-ext-head-support` 2.0.4 | 0BSD, as declared in the package's `package.json`, copied to [licenses/htmx-ext-head-support-package.json](licenses/htmx-ext-head-support-package.json) | `207f449ba70ad0d384b1734288ddae8493d26737bd74d8510829c0be5b737568` |
| `apps/web/static/js/composer.js` (build output, git-ignored) | The editor: `apps/web/frontend/composer*.js`, `lottery-prizes.js` and `red-packet.js` bundled by `npm run build:editor` with `@lezer/common` 1.5.2, `@lezer/highlight` 1.2.3 and `@lezer/markdown` 1.7.2, as locked in `package-lock.json` | Project code: MIT OR Apache-2.0; the `@lezer` packages: MIT, texts in [licenses/composer-bundle-LICENSES.txt](licenses/composer-bundle-LICENSES.txt) | changes with each build |
| `apps/web/static/js/site.js`, `navigation.js`, `debug.js` | The site's own scripts | MIT OR Apache-2.0 | |

`/static/i18n/<language>.js` is generated from the translation files, not read from disk, and is project code too.
The build and test tools in `package.json` (esbuild, Playwright) are not shipped.

## Color scheme

`apps/web/frontend/styles/themes/default.json` is the only color scheme and is project code (MIT OR Apache-2.0). Its
base colors are the color variables of web-standard-kit's `style.css`, a style kit by the same author; only the values
are used, no files, and they are licensed with the project. The thread tag colors were tuned for this project following the approach of
[Radix Colors](https://www.radix-ui.com/colors/docs/palette-composition/understanding-the-scale) (soft grounds with
high-contrast text) and
[Carbon's categorical palettes](https://v10.carbondesignsystem.com/data-visualization/color-palettes/) (a separate hue
per category); no values, code or files from either are used.

## Images made in code

A 1 × 1 transparent PNG, written as bytes in `apps/web/src/router/auth/profile.rs` (`BLANK_AVATAR`), is served for
members without an avatar so the initials tile behind it shows through. Initials tiles themselves are CSS. Project
code, MIT OR Apache-2.0.

## Licence files

- `NOTICE`: the project's licence, the brand exclusion, and each bundled third-party file with its licence.
- `docs/licenses/`: `Space-Mono-OFL-1.1.txt`, `htmx-0BSD.txt`, `htmx-ext-head-support-package.json` and
  `composer-bundle-LICENSES.txt`.
- Release packages (`scripts/package-release.sh`) carry `LICENSE-MIT`, `LICENSE-APACHE`, `NOTICE`, `docs/licenses/`
  and `THIRD-PARTY-LICENSES.txt`, which cargo-about generates from `scripts/licenses/` with the licences of the Rust
  crates linked into the binary.
