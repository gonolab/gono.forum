# Skins

The templates are Askama, compiled into the binary, so the HTML structure of a page cannot change at run time. A skin
changes the look on top of that fixed structure, without building anything: palettes over the core color scheme, a
manifest and an optional stylesheet that the forum checks before it uses it. What a skin's stylesheet (and a member's
personal style, and page scripts) can rely on is a set of public hooks the core templates write on purpose: fixed
class names, `data-*` attributes for states, raw values next to the formatted ones, optional elements that are output
but hidden, a page data block, two script events and a declared sticky header height. The first part of this document
describes skins, the second part ("List rows" onwards) the hooks.

## A skin

A skin is a directory named after its id:

```text
<id>/
  skin.toml                the manifest
  palettes/<name>.json     overrides of the core color scheme, one file per palette
  skin.css                 the skin's stylesheet, optional
```

Other files are ignored with a warning. The built-in skins live in `apps/web/skins/` (today only `default`, which is
the standard look: the core color scheme unchanged and no stylesheet).

### The manifest

```toml
contract = 1                         # the skin contract this core reads; must be 1
id = "ledger"                        # 2 to 32 lowercase letters, digits and hyphens, starting with a letter
version = "1.2.0"
name = { en = "Ledger", fr = "Registre" }    # by language tag; English is required
description = { en = "Flat surfaces separated by rules" }   # optional; English required when present
author = "…"                         # optional
license = "MIT"                      # optional
min_forum = "0.2.0"                  # optional: the oldest Gono Forum the skin works with

[palettes]
default = "main"                     # palettes/main.json
variants = []                        # other palettes the administrator may choose

[frame]
nav = "top"                          # "top", or "side" for side navigation (see "Side navigation")
header_boards = true                 # false leaves the header's boards out; only with nav = "side"
masthead_height = "52px"             # optional: the declared header height (--masthead-h), 40 to 120 px
avatar_shape = "member"              # circle, rounded or square fixes every avatar; member leaves it to members
page_head = "column"                 # "column", or "banner" for the page head as a band (see "Page head banner")
home = "threads"                     # "threads", or "boards" to open the home page with the board directory

[behaviors]
enabled = ["timeline", "progress"]   # built-in modules: timeline, progress, autohide-header, docked-composer
```

Unknown keys are errors, and so are behavior modules this core does not have or names listed twice. `[layout]` is
the skin's default layout: places with their blocks, written like a layout document's `places` ("Places and blocks")
and checked as strictly as a saved document, for example:

```toml
[layout]
"home.aside" = [
  { id = "me", source = "member-card" },
  { id = "pinned", source = "threads", view = "mini-list", params = { sort = "pinned" }, title = { key = "block-title-pinned" } },
]
```

A skin brings no interface text: names and descriptions are written per language, everything else on the page
is the core's. Built-in skins are the exception: their `skin.toml` writes `name` and `description` only in English,
and the names and descriptions shown in every language are the messages `skin-builtin-<id>-name` and
`skin-builtin-<id>-description` in `locales/<language>/skin.ftl` (listed in `skins::builtin_text`; a unit test keeps
the English message equal to `skin.toml`). External skins carry no translation files, so they write each language in
`skin.toml`. The manifest is stored as normalized JSON (`gonof_common_skin.manifest` and the pack).

### Palettes

`frontend/styles/themes/default.json` is the core color scheme (see [styles.md](styles.md), "Color scheme"). A skin
palette has the same three objects and lists only what it changes:

- `shared` changes both modes, `light` only light mode, `dark` only dark mode. (In the core scheme, `dark` lists only
  what differs from light; in a skin palette each object overrides its own mode, so a light-mode override never
  leaks into dark mode.)
- Every token keeps the core's position; tokens the core does not have are ignored with a warning, since no core
  stylesheet reads them.
- Values are single CSS values that the stylesheet checker accepts on their own (colors, `transparent`, `var()` of
  other tokens, shadows, `color-mix()` and the like), without braces, semicolons, `!`, `<`, `>` or `url()`.
  References to tokens that do not exist and reference cycles are errors. Names under `--gonof-` are reserved for the
  core.

The server merges each palette onto the core scheme and writes the same three rules the build used to write: light
values on `:root[data-palette="<name>"]` (and on `:root` without `data-palette` for the skin's default palette), dark
values under `[data-theme="dark"]`, and dark values again inside `@media (prefers-color-scheme: dark)` when
`data-theme` is neither `light` nor `dark` (`skins/palette.rs`).

### Packs

To move a skin into the database it is packed into one file: `gono-forum-cli skin pack <dir>` checks a skin directory
and writes `<id>-<version>.skin.json` (or `--output=<file>`), a JSON object with `format` (`gono-forum-skin`), the
normalized `manifest`, the `palettes` by name and the stylesheet text as `css`. The same skin always packs to the same
bytes; the pack's SHA-256 is its content hash. `gono-forum-cli skin check <dir|file>` runs exactly the checks of an
import and lists every problem and warning; `--builtin` applies the rules of built-in skins. Neither needs a database.

### Limits

The same for every source, so any skin can be packed and imported again: the stylesheet at most 256 KiB as written,
comments included; all palettes together at most 128 KiB as stored; the manifest at most 16 KiB; a pack file at most
1 MiB; at most 10,000 rules in the stylesheet.

## Sources

| Source | Where | Read | When it fails its checks |
| --- | --- | --- | --- |
| Built in | `apps/web/skins/<id>/`, compiled in by `build.rs` like the translations | Once per process | A unit test fails (`skins::tests`); at run time it is unusable and logged |
| Imported | `gonof_common_skin`, one row per id, imported at `/manage/skins` | With the process cache, checked again on every load | The import is refused with every problem listed; a row that no longer passes (edited by hand, rules changed) is unusable and logged |
| Instance directory | `$GONOF_HOME/skins/<id>/` (in a source build, `skins/` in the source tree) | At start and after "Rescan", an import or a delete (a broadcast reaches every process) | Unusable, listed with its problems and logged |

- **Priority**: built in, then imported, then the instance directory. Of skins with the same id, only the one from the
  highest source that has the id is usable, even when that one fails its checks; `/manage/skins` says which source
  shadows the others. To work on a skin in the instance directory, give it another id or delete the imported copy.
- **Several machines**: imported skins are shared through the database; each machine reads its own instance directory,
  and a skin there is only usable on the machines that have it. Back up `skins/` with the instance (see
  [install.md](install.md), "Files to back up").
- **Broken at run time**: when the chosen skin cannot be used (deleted, shadowed, failing its checks after an upgrade,
  or the catalog cannot be read), pages use the built-in `default` and an error is logged, at most once a minute per
  skin. A skin never turns a page into an error.

## Two kinds of rules

Every skin is checked when it is read: the manifest, every palette and the stylesheet (`css_check.rs`, the same
checker as members' personal styles, and `skins/palette.rs`). Built-in skins ship with the core and must follow every
rule of [styles.md](styles.md); imported and instance skins are the administrator's choice, so only the rules that
keep the site safe and the "Built on Gono Forum" line block them, and the rest are listed as warnings.

| Rule | Built in | Imported, instance |
| --- | --- | --- |
| Nothing that issues a request: `url()`, `image-set()`, `@import`, `@font-face`, at-rules other than `@media`; no backslash escapes or control characters; functions only from a list | Blocks | Blocks |
| No `position: fixed`; `content` only `""`, `none` or `normal`; `z-index` only `auto` or -1 to 20 | Blocks | Blocks |
| No `!important`; no `--gonof-*` variables | Blocks | Blocks |
| "Built on Gono Forum": no selector naming `colophon`; none of the guarded properties on `html`, `body`, `footer`, `:root` or `*`; the footer colors legible (see below) | Blocks | Blocks |
| Open blocks, comments or strings at the end of the stylesheet | Blocks | Blocks |
| Header sizes that `router/nav_fit.rs` measured: font size, letter and word spacing, padding, margin, border widths, widths and gaps on `masthead*`, `forum-more*`, `theme-switch*` and `.container`; `frame.masthead_height` | Blocks | Warns |
| Visible focus: no `outline: none` (or `0`) on `:focus` rules | Blocks | Warns |
| The post actions stay visible: no `opacity` or `visibility` on `actions-bar` | Blocks | Warns |
| Media queries only `max-width` at 900, 760, 700, 640, 480 or 420 px, `prefers-reduced-motion` and media types | Blocks | Warns |
| Logical properties: no `left`, `width`, `margin-left` and the like | Blocks | Warns |
| The fixed values of the style guide: weights 400, 500 and 600; radii 0, 4, 6, 8, 12, 9999 px and 50%; borders 0, 1 and 2 px; body text 15px with a line height of 1.75; the design variables (`--r*`, `--container`, `--sidebar-w`, `--side-nav-w`, `--gap`, `--post-image-max`, `--masthead-h`, `--sticky-top`, `--safe-*`) | Blocks | Warns |
| Selectors that depend on `aria-label`, `title` or child positions (`:nth-child()`, `:first-child`, …) rather than the classes listed below | Blocks | Warns |
| Contrast (see below) | Blocks | Warns |
| A built-in skin's `skin.toml` writing a name or description in a language other than English, or no `skin-builtin-<id>-*` messages for its id | Blocks | — |
| A palette token the core does not have; a palette file the manifest does not name; a file that is not part of a skin | Warns | Warns |

Problems are stored and listed as codes with their values (`gonof_common_skin.warnings` holds the warnings the
administrator confirmed on import) and worded in the reader's language when shown.

### Contrast

Each merged palette is checked in light and dark mode. Colors written as hex, `rgb()`, `hsl()`, `oklab()`, `oklch()`,
named colors or `transparent` are evaluated, translucent ones laid over the ground they sit on; other forms
(`color-mix()`, `light-dark()`) cannot be checked, which counts like a failed pair.

- Text 4.5:1: `--text-strong`, `--text`, `--text-muted` and `--text-subtle` on `--bg`, `--panel`, `--panel-alt`,
  `--panel-hover`, `--input-bg` and `--secondary-bg`; `--primary-text` on `--primary-bg` and `--primary-hover`;
  `--secondary-text` on `--secondary-bg`; `--input-text` and `--input-placeholder` on `--input-bg`; `--link` on `--bg`
  and `--panel`; `--hero-text` on `--hero-bg`.
- Controls 3:1: `--input-border` on `--input-bg`, `--panel` and `--bg`; `--focus-ring` on `--bg` and `--panel`.

### Built on Gono Forum

The footer line "Built on Gono Forum" (`.colophon__legal` with the link `.colophon__brand`) is written in English on
purpose and is the same in every language ([translating.md](translating.md)). No skin can hide, move, shrink or cover
it: the checker refuses selectors that name `colophon` and the guarded properties (display, visibility, opacity,
filters, transforms, clipping, masks, overflow, maximum height, content visibility, zoom) on `html`, `body`, `footer`,
`:root` and `*`; the `guards` layer fixes those properties on the line and on every box around it, puts the line at
`z-index: 21` (skins stop at 20) and removes pseudo-elements on it; and each skin's stylesheet fixes the line's text,
link and ground colors to its palette's `--text-muted`, `--text` and `--panel`, which must be plain colors with 4.5:1.
This is a rule of the skin layer, not a licence term: changing the source and building it yourself is another matter.

## Cascade layers

`forum.css` begins with `@layer guards, core, skin, member;` and every stylesheet of a page goes into one of them:

| Layer | Holds |
| --- | --- |
| `guards` | `frontend/styles/guards.css`: `[hidden]`, `.sr-only` and the "Built on Gono Forum" guard. Under built-in skins (`<html data-skin-source="builtin">`) also copies of the header sizes and focus rings, which `scripts/build-styles.mjs` generates from the core stylesheets. The footer colors, from each skin's stylesheet. |
| `core` | Everything `index.css` imports. |
| `skin` | The skin's palettes, its declared header height and its stylesheet (`/skin/<id>.css`). |
| `member` | A member's personal style (`/my/style.css`), on their own pages only. |

Every declaration in `guards` is `!important`. Among important declarations the earliest layer wins, so nothing in a
later layer changes a guard, important or not; skins may not use `!important` at all, and a personal style that does
still cannot reach the guards. Among normal declarations later layers win regardless of selector specificity: a skin
overrides the core, and a personal style overrides both. The header and focus guards hold under built-in skins only,
because for imported and instance skins those rules are warnings: such a skin may change them, and pays for it if the
header navigation no longer fits.

## The stylesheet of a skin

Every page on `layout/base.html` links the skin after `forum.css` and before a personal style:
`/skin/<id>.css?v=<hash>`, where the hash is the first 12 hex digits of the stylesheet's SHA-256, so a new version of
Gono Forum that generates other rules for the same skin also gets a new address. Where the site or the domain has seed
colors ([styles.md](styles.md), "Seed colors"), the page links the skin's colored variant instead,
`/skin/<id>.css?c=<primary><secondary>&v=<hash>` (`c` is the two seeds as twelve hex digits): the same stylesheet
with every palette derived from the seeds, and its own hash, so the address changes with the colors. It holds, in this
order:

1. `@layer guards { … }` with the footer colors of each palette;
2. `@layer skin { … }` with each palette's three rules, `--masthead-h` when the manifest declares a header height,
   and the skin's stylesheet as written.

The address is served without a session or cookies. With the current hash it is cached for a year: `public` for skins
a page of the site uses (the site's or a domain's choice, and `default`) and for the colored variants a page uses (the
skin and seed colors of the site or of a domain), `private` for others, so a skin being previewed stays out of shared
caches. Another hash gets the current stylesheet with `no-cache`; a `c` that is not twelve hex digits is not found.

A colored variant is only compiled for seed colors the site or a domain uses, and the process keeps those with the
skins (at most 32, the oldest dropped first), or for the administrator previewing those colors on that skin: the
request carries their sealed preview cookie, only then is their session looked up, and the variant is compiled off the
request workers, served `private` and not kept. Any other `c` compiles nothing and gets the skin's own stylesheet with
`no-cache`, so made-up addresses cost no work, and a page cached from before a color change still has a stylesheet.
`errors/plain.html` links the built-in default skin without seed colors, which needs neither settings nor the
database.

## Choosing a skin

The site's choice is the layout document `settings.layout.{body,title,updated_at}`, three rows of the settings table
read by the page frame in the same statement as the settings (see [settings-and-caching.md](settings-and-caching.md)).
Its body is JSON:

```json
{"v": 1, "skin": "ledger", "palette": "main", "based_on": {"skin": "ledger", "version": "1.2.0"}, "places": null,
 "colors": {"primary": "#0f766e", "secondary": "#f97316"}, "home": "boards"}
```

An empty body means the built-in default with its own colors. `based_on` records the skin and version the choice was
made from; `places` holds the blocks of the places the document changes ("Places and blocks"), `null` while it
changes none. `home` is what the home page opens with ("Board directory home"), left out to follow the next
layout. `colors` holds the
seed colors ([styles.md](styles.md), "Seed colors") and is left out while the skin's own colors are used; only the two
seeds are stored, and the colors are derived from them for whichever skin is in use. Each domain may have an override
document, `settings.domain_layout.<domain id>.{body,title,updated_at}`, with the same structure; keys it leaves out
come from the site's, and its `colors` may also be `"skin"`, the skin's own colors even where the site has seeds. A
request uses the domain's skin, then the site's, then `default`; a palette the skin does not have falls back to its
default palette; it uses the domain's colors unless the domain leaves them out, then the site's. Deleting a domain
deletes its document, and a domain document left with neither a skin nor colors of its own is deleted too.

Choosing is done at `/manage/skins` (the site) and on a domain's page under Multiple domains, and seed colors at
Appearance › Colors and on a domain's page; all of these lock the document's rows, check the administrator and, for an
imported skin, that its row still exists, then write the document, record `skin.select` (a skin) or `skin.colors`
(colors, with the seeds before and after) and clear the settings block. Changing skins keeps the colors. An imported
skin that a page uses cannot be deleted.

## Board colors and glyphs

An administrator can give each board a color and one of 32 built-in glyphs in the board's form (Forums › Forums):
"Give this forum a color" with a color picker, and a choice of icon. They are stored on the board row (`color`, empty
or `#rrggbb` in lower case; `glyph`, empty or a glyph name, `organization/glyphs.rs`) and checked on every save. They
do not change the default look: the core stylesheets do not use them; a skin does.

- **Variables.** While at least one board has a color or a glyph, every page links the board stylesheet
  `/boards.css?k=<skin>&v=<hash>` after `forum.css` (`<link … data-board-style>`), compiled for the page's skin
  (`k` is the hash of the skin's stylesheet, so a colored variant has its own). It sets, on the `data-board="<fid>"`
  hook of list rows, board blocks and the board head, inside the `core` cascade layer:

  | Variable | Value |
  | --- | --- |
  | `--board-color` | The board's color, moved in lightness only as far as needed for 3:1 against the `--panel` and `--bg` of the skin's palette (with the seed colors derived into it, when the page has them), separately in light and dark mode (dark values under `[data-theme="dark"]` and in `@media (prefers-color-scheme: dark)` for the system theme). A skin with several palettes gets the values of each under `:root[data-palette="<name>"]`. |
  | `--board-on-color` | Text for `--board-color` as a fill: white or the light mode's ink, with 4.5:1. |
  | `--board-glyph` | The board's glyph as an SVG image with black strokes, for `mask-image: var(--board-glyph)`. |

  A board without a color has no `--board-color`, so a skin writes a fallback: `var(--board-color, var(--accent))`.
  The stylesheet is served without a session and kept for a year with the current hash: `public` for a skin (or
  colored variant) a page of the site uses, `private` for one only a preview shows. Any other `k` gets the built-in
  default skin's board colors with `no-cache`. The process keeps the stylesheets of up to 32 skins and compiles them
  again when the boards change.
- **Symbols.** Pages that draw glyphs include `partials/board_glyphs.html` once, which defines
  `#board-glyph-<name>` symbols drawn like every other icon (docs/styles.md, "Icons"); markup uses them as
  `<svg><use href="#board-glyph-<name>"></use></svg>`.

The glyph names are `chat`, `megaphone`, `question`, `lightbulb`, `bug`, `wrench`, `code`, `book`, `graduation`,
`newspaper`, `calendar`, `people`, `coffee`, `star`, `image`, `music`, `film`, `gamepad`, `palette`, `briefcase`,
`tag`, `globe`, `pin`, `archive`, `heart`, `home`, `flag`, `shield`, `leaf`, `rocket`, `trophy` and `lock`. A board's
uploaded icon image (`icon_aid`) is separate and unchanged.

## Places and blocks

A page has fixed places, and each place shows a list of blocks. A block is a data source ([data.md](data.md)) with
its parameters and a view, or one of the core blocks below. A page takes the blocks of each place, place by place,
from the first of: the domain's layout document, the site's, the skin's manifest (`[layout]`), and the core default
layout, which shows what the templates showed before layouts (`layout.rs`, `core_default`).

| Place | On | Where | Blocks | What it takes | Core default |
| --- | --- | --- | --- | --- | --- |
| `home.aside` | the home page, its later pages and search | the aside | 8 | every data source but `thread.outline`, every core block but `board-head` | `member-card`, hot threads, latest threads, `recent-local`, `site-overview`, `friend-links` |
| `board.aside` | board pages | the aside | 8 | as `home.aside`; `current` means the board | `member-card`, the board's hot and latest threads |
| `thread.aside` | thread pages | the aside | 6 | as `home.aside`, and `thread.outline` | `member-card` |
| `page.head.home`, `page.head.board`, `page.head.thread` | those pages | the top of the main column (after the breadcrumb on thread pages), in `div.page-head`; a band opening the main region under a skin with `frame.page_head = "banner"` ("Page head banner") | 2 | `markdown`, `site.stats` in its `compact` view, and on board pages `board-head` | `board-head` on board pages, empty elsewhere |
| `footer` | the pages with places | inside the footer, before the policy links, in `div.colophon__blocks` | 4 | `markdown`, `site.links`, `site.stats` | empty |
| `side.nav` | every page but the admin area, under a skin with side navigation | the side navigation ("Side navigation") | 4 | `nav-boards`, `nav-links`, `markdown` | empty |

A page shows at most 10 blocks across its places (a layout is refused when the places of one page, `side.nav`
included, hold more); the policy links and the "Built on Gono Forum" line are always the
core's, and blocks only sit beside them. A board page whose head has no `board-head` block, and the home page, write
their heading for screen readers only. Search results keep their own head and use `home.aside`.

### Blocks

```json
{"id": "hot-week", "source": "threads", "params": {"scope": "current", "sort": "hot", "days": 7, "limit": 10},
 "view": "mini-list", "title": {"key": "block-title-hot"}, "show": "all", "keep_on_phone": false}
```

| Field | Value |
| --- | --- |
| `id` | 1 to 32 lowercase letters, digits and hyphens, starting with a letter; unique in the document. |
| `source` | A data source with views, or a core block. |
| `params` | The source's parameters ([data.md](data.md)); `current` is the page's board or thread. Core blocks take none. |
| `view` | One of the source's views; core blocks have one and may leave it out. |
| `title` | `{"key": "block-title-…"}` (`block-title-hot`, `-latest`, `-active`, `-featured`, `-pinned`, `-boards`, `-new-members`, `-ranks`, `-honors`, `-verified`, `-moderators`, `-stats`, `-links`, `-outline`, `-notice`, `-navigation`), or `{"text": {"en": "…", …}}` by language tag, English required, at most 40 characters each. Data-source blocks and `markdown` take one, and need one in an aside; the core cards carry their own headings and take none. |
| `show` | `all` (default), `guests` or `members`. |
| `keep_on_phone` | `true` keeps the block when the aside folds away on phones (`panel--keep`). |
| `body` | `markdown` only: the text by language tag, English required, at most 4 KB each, rendered and cleaned like posts. |

Documents and skins are checked strictly when they are saved or imported: an unknown key, place, source, view or value,
too many blocks for a place or a page, a repeated id, or a block in a place that does not take it refuses the whole
layout, naming the field (`places.home.aside[2].view`). Pages read them leniently: a block that no longer passes (a
view an upgrade removed, a board that was deleted) is skipped and logged as an error, never an error page, and the
debug panel lists every block of the page with its time and the skipped ones with the reason.

### Core blocks

| Block | View | What it shows |
| --- | --- | --- |
| `member-card` | `card` | The signed-in member's card, or the sign-in card. |
| `recent-local` | `list` | The visitor's recently viewed threads, which `site.js` fills from local storage. |
| `site-overview` | `card` | The site statistics and the newest members. |
| `friend-links` | `list` | The friend links and the link to apply; only on page 1 of the main domain's home page. |
| `board-head` | `head` | A board page's head: name, icon, follow and filter controls, description, moderators, posting guide; it carries the board's `data-board`. |
| `nav-boards` | `tree` | The board tree with the new-thread button; the board of the page, or of the thread, is `aria-current="page"`. |
| `nav-links` | `list` | The header navigation's entries. |
| `markdown` | `prose` | Its `body` in the page's language. |

### Views

Each view is an Askama template, `templates/blocks/<source>/<view>.html`; the member sources share
`templates/blocks/members/<view>.html`, which their own templates include.

| Source | Views |
| --- | --- |
| `threads` | `mini-list` (titles; hot threads with their reply counts), `list` (with board, time and replies), `cards` (with the first thumbnail) |
| `boards` | `tree`, `grid`, `directory` (with description, counts and the latest thread) |
| `members.recent`, `members.honors`, `members.verified`, `moderators` | `avatars`, `list` |
| `members.rank` | `avatars`, `list`, `table` |
| `site.stats` | `figures`, `compact` |
| `site.links` | `links` |
| `thread.outline` | `timeline` (the pages, each with the date it begins; the `timeline` module draws a scrubber over them), `progress` (the pages with their floors; the `progress` module adds the floor being read) |

Every block's outer element carries `block block--<source> block--<view>` among its classes (the dot of a source name
written as a hyphen, `block--site-stats`) and `data-block="<id>"`. Rows and tiles of threads and boards carry
`data-board`, so the board variables ("Board colors and glyphs") reach them, and so does the board head. The core
default layout renders the same markup the templates wrote before layouts plus these hooks (and the board head's
`data-board`) and the `div.page-head` around a board head;
`tests/default_layout_http.rs` compares it with HTML recorded from the old templates.

## Frame

The skin's manifest and the request's choice are written on `<html>`:

| Attribute | Value |
| --- | --- |
| `data-skin` | The skin's id. |
| `data-skin-source` | `builtin`, `imported` or `instance`. |
| `data-palette` | The palette in use. |
| `data-nav` | `side` when the page shows the side navigation ("Side navigation"), else `top`. |
| `data-header-boards` | `true` when the header shows its board navigation: always beside the top navigation, and beside the side navigation unless the skin sets `frame.header_boards = false`. |
| `data-avatar-shape` | `circle` or `square` when the skin fixes that shape; when it leaves the shape to members, `circle` for a member who chose round avatars. Absent for the default rounded corners. |
| `data-behaviors` | The behavior modules the page loads, space-separated ("Behavior modules"); absent when it loads none. |

### Side navigation

A skin with `frame.nav = "side"` moves the navigation into a column at the start of the page, beside the main region,
under the full-width header. The column shows the blocks of the `side.nav` place ("Places and blocks"), so the skin's
`[layout]` must give it at least one; `nav-boards`, `nav-links` and `markdown` take no data beyond the page frame, so
the column appears on every page built on `layout/base.html` except the admin area (which keeps its module menu):

```html
<div class="site-frame container">
  <div class="side-nav" data-side-nav>…the side.nav blocks…</div>
<main class="site-main site-frame__main" id="main" tabindex="-1">…</main>
</div>
```

- **Header.** The header stays at the top across the whole width. With `frame.header_boards = false` it leaves out its
  board navigation (`.masthead__boards`); otherwise it keeps it, and `router/nav_fit.rs` leaves room for the drawer
  toggle in the width tiers below 1100px.
- **Narrow screens.** From 1099px down the column folds away and the header's drawer button shows, as it does from
  900px down under the top navigation; the drawer (a `<details>` element, so it opens without JavaScript) lists the
  boards as before. `site.js` treats the drawer as the navigation up to that width.
- **Sticking.** The column sticks under the header (`inset-block-start: var(--sticky-top)`) and scrolls on its own when
  it is taller than the window; `navigation.js` keeps its scroll position across boosted navigation.
- **Falling back.** A page whose `side.nav` has no block for this visitor (an administrator's layout that empties the
  place, or only blocks with `show: "members"` for an anonymous visitor) shows the top navigation with the header's
  boards, as does the admin area. `data-nav` says which the page shows.
- **Width.** `--side-nav-w` (200px) is the column's width; a built-in skin keeps it.

### Page head banner

With `frame.page_head = "banner"` the blocks of a page's `page.head.*` place leave the main column and open the main
region as a band in the hero colors, `--hero-bg` and `--hero-text` (by default the core scheme's `--panel-alt` and
`--text-strong`; checked like text, 4.5:1). Under the top navigation the band spans the whole page width and its
content keeps the page width; beside side navigation it spans the main column:

```html
<main class="site-main site-main--hero" id="main" tabindex="-1">
<div class="page-hero"><div class="page-hero__inner container"><div class="page-head page-head--banner">…</div></div></div>
<div class="container">…the page…</div>
</main>
```

Inside the band the text roles (`--text-strong`, `--text`, `--text-muted`, `--link`) take the hero's text color and
the blocks lose their card look. A page without head blocks (by default every page but board pages) has no band, and
its markup is the same as under `column`. The band stays inside `<main>`, so the skip link and the page's heading
work as before.

### Board directory home

Page 1 of the home page's bare address (`/`, without another sort or a category) can open with the board directory
above the thread list: each top-level board the visitor sees, with its uploaded icon or its glyph, its name and
description, its child boards, its thread count and posts today, and its newest thread by last activity with the last
poster's avatar and the time. The thread list and its paging follow as before, so `/page-N` is still the thread list.
The records are the `boards` data source's with `counts` and `last_post` ([data.md](data.md)), read as the visitor
like every list; a board the anonymous statistics do not count shows no counts, and a latest thread whose poster the
visitor blocked is left out.

What the home page opens with comes from the first that chooses it: the administrator's previewed draft, the domain's
layout document, the site's (`"home": "threads"` or `"boards"`), then the skin's `frame.home`; `threads` when none
does. The administrator chooses it on the layout page ("Editing a layout").

```html
<section class="panel board-dir" aria-labelledby="board-dir-title" data-board-directory>
  <h2 class="board-dir__title" id="board-dir-title">Forums</h2>
  <ul class="board-dir__list">
    <li class="board-dir__item" data-board="3">
      <div class="board-dir__board"><span class="board-dir__icon">…</span><div class="board-dir__text">
        <h3 class="board-dir__name"><a href="/daily">Daily</a></h3><p class="board-dir__desc">…</p>
        <ul class="board-dir__children">…</ul></div></div>
      <p class="board-dir__counts"><span class="row-meta__threads">…</span><span class="row-meta__today">…</span></p>
      <div class="board-dir__last">…<a class="board-dir__last-title" href="/posts/77">…</a>…<time …>…</time></div>
    </li>
  </ul>
</section>
```

The rows carry `data-board`, so the board variables ("Board colors and glyphs") reach them: the glyph and the rule
before the latest thread take `--board-color`. The counts are split into number and word ("Counts").

When the skin fixes the avatar shape, a member's reading settings say so; their own choice is kept and applies again
under a skin that leaves the shape to them.

A boosted navigation (`hx-boost`) replaces the body and merges the head but keeps `<html>`; `site.js` copies these
attributes from the new page in the same step as the new skin stylesheet arrives, so moving between pages with
different skins (`/manage/skins` always uses the default) never pairs one skin's stylesheet with another's palette.

## Behavior modules

Behavior modules are scripts compiled into the binary (`static/js/behaviors/<name>.js`) that a skin turns on in its
manifest, `[behaviors] enabled`; skins bring no scripts of their own, and the administrator chooses modules only by
choosing the skin. Each one only enhances the page: without JavaScript, or under a skin that does not enable it, the
page is complete and works as before.

- **Loading.** A page loads the skin's modules as deferred scripts after `site.js`, with `data-behavior="<name>"`, and
  lists them in `<html data-behaviors>`. The admin area, sign-in, sign-up, two-step verification, password and email
  pages, account security, and the other pages a personal style stays off load none (`custom_style::applies`).
- **Data.** Modules read the page data block ("Page data") and, where they need more, `/data/v1`; they start at
  `gonof:ready` and again after a boosted navigation (`gonof:swap` with the body as its target), and read the header
  height from `--sticky-top` (`window.gonofStickyTop()`).
- **Access.** Every control a module adds is reachable by keyboard and named for screen readers, and scrolling and
  movement follow `prefers-reduced-motion` (`site.css` switches transitions and animations off).
- **Markup.** What a module adds is listed below; `behaviors.css` styles it, and none of it is in the server's HTML.
  A block a module has enhanced carries `data-enhanced="<name>"`.

| Module | What it does |
| --- | --- |
| `timeline` | Turns each `thread.outline` block in its `timeline` view into a scrubber: a vertical slider (`div.timeline`, the handle `.timeline__handle` with `role="slider"`) over every floor of the thread, between links to the first and last floor with their dates. It follows the floor being read, and dragging it or moving it with the keyboard (arrows by one floor, Page Up and Page Down by a page, Home and End) previews a floor with its date; letting go or Enter goes there, on this page by scrolling, else on its page of the flat view; Escape goes back. The block's page list stays in the markup, hidden on screen. The block sticks under the header beside the floors. Data: the block's outline record and the page's floors. |
| `autohide-header` | The sticky header slides up while the reader scrolls down past twice its height and comes back as soon as they scroll up. While it is away `<html data-header-hidden>` is set and `--sticky-top` drops to the safe area, so sticky asides, the side navigation, anchor targets and the scripts that read the header height follow ("Sticky header"). It does not hide while focus or an open menu (`details`) is in the header, and focus moving into it brings it back. |
| `docked-composer` | On thread pages where the reader can reply, the reply box (`#reply-editor`) docks at the bottom of the window behind a bar (`div.composer-dock`) with a button (`aria-expanded`, `aria-controls="reply-editor"`) that opens and closes it; `<html data-composer-dock>` is `open` or `closed`. The box keeps its place in the markup, so the editor and its forms work as before. Opening focuses the text field; Escape in the box or on the button closes it and returns focus to the button. Links to `#reply-editor` on the page open it, a page opened at `#reply-editor` or with a reply or quote under way starts with it open, and a published reply (`composer:published`) closes it. Locked threads and visitors who cannot reply keep the inline notice. |
| `progress` | On thread pages, a thin bar under the header (`div.reading-progress`, hidden from screen readers) that fills as the reader moves through the thread; in each `thread.outline` block in its `progress` view, the floor being read out of all of them (`p.outline-progress` with a `<progress>` element) and the page that holds it marked `aria-current="page"` in the list. Data: the page's floors, and an outline record of the page for the thread's last floor (else the reply count). |

## The admin page

Appearance › Skins (`/manage/skins`, administrators only, see [roles.md](roles.md)) works without JavaScript:

- **Skin of the site**: choose a skin and palette for the whole site, or preview it.
- **All skins**: every skin of every source with its version, whether it is usable and why not, its problems and
  warnings, a preview button for usable skins and a delete button for imported ones.
- **Import a skin**: upload a pack. A pack that breaks a blocking rule is refused with every problem listed. A pack
  with warnings, or one that replaces an imported skin of the same id, is shown with its warnings for confirmation;
  the confirmation carries the pack back and is refused if the warnings or the row it replaces changed meanwhile.
  Imports record `skin.import` (with the number of confirmed warnings), deletes `skin.delete`.
- **Instance directory**: "Rescan" reads `$GONOF_HOME/skins/` again here and, through the cache broadcast, in every
  other process.

`/manage/skins` itself always uses the built-in default skin without seed colors, so a skin that breaks pages can
always be switched back there. The seed colors have their own page, Appearance › Colors ([styles.md](styles.md),
"Seed colors"); a domain's page under Multiple domains chooses its skin and its colors.

## Editing a layout

Appearance › Layout (`/manage/layout`, administrators only) edits the places of the site's layout document, or with
`?domain=<id>` (linked from the domain's page) the places of one domain. It works without JavaScript: every action is a
form button.

- **Places.** Each place lists the blocks it shows and where they come from. A place the document does not set shows
  the next layout's blocks; **Change this place** takes them into the document to change them here, and **Follow the
  site** (a domain) or **Use the skin's or the default blocks** (the site) leaves the place to the next layout again.
- **Blocks.** **Move up**, **Move down** and **Remove** change a place's list. **Add a block** chooses the kind of block
  the place takes, then a form generated from that source's parameter lists ([data.md](data.md)): the view, the
  parameters, a title from the list of title keys or written in English and Simplified Chinese, who sees it, and
  whether it stays on phones. New blocks get ids from their source (`threads-1`).
- **Home page.** What the home page opens with ("Board directory home"): what the next layout chooses (the site's
  for a domain, the skin's for the site), the thread list, or the board directory above it; saved to the draft like
  the places.
- **Draft.** Every change is saved to a draft, `settings.layout_draft.site.{body,title,updated_at}` or
  `settings.layout_draft.<domain id>.…`, after the strict checks of a saved layout; visitors never see it. **Preview the
  draft** shows it to this administrator on every page (on the domain's own address for a domain's draft), with the
  preview bar; **Publish the draft** writes the places and the home page into the document under the same locks and checks as a
  skin choice, records `layout.save` with the places and the home page before and after, deletes the draft and ends the preview; **Discard the
  draft** deletes it. Deleting a domain deletes its draft too.

**Changing skins.** When the document whose skin changes (the site's at `/manage/skins`, a domain's on its page) sets
places of its own, choosing a skin first asks: **Keep my layout**, or **Use the new skin's layout**, which clears the
document's places and home page so the new skin's `[layout]` and `frame.home` (or the defaults) show (recording `layout.save` after `skin.select`).
The question lists the places the document sets (and `home` when it chooses the home page) and the blocks the new
skin would not show (side navigation blocks
under a skin without side navigation). A document that sets no places follows the new skin directly. `based_on`
records the skin and version the choice was made from.

## Preview

"Preview" shows a skin, or unsaved seed colors, to the administrator who started it, on every page, without changing
what anyone else sees. The choice lives in a cookie sealed with the cookie key (`gonof_skin_preview`, eight hours) and
is applied only while the signed-in member is that administrator; publishing, exiting and signing out clear it. A
preview of a skin shows it with the colors the page has anyway (the site's or the domain's seeds, derived again for
that skin); a preview from Appearance › Colors shows the entered seeds over the site's skin and replaces any preview of
a skin; **Preview on this domain** on a domain's page (Multiple domains) shows that domain's chosen colors (its own
seeds, the skin's own colors, or the site's when it follows the site) over the domain's skin, on that domain's address
only, and publishing it saves them as the domain's; a preview of a layout draft ("Editing a layout") shows the draft's places over the skin and colors the page has
anyway. A bar at the top of every page offers **Publish** (make the skin the site's, recording `skin.select`, save the
colors as the site's, recording `skin.colors`, or publish the layout draft, recording `layout.save`) and **Exit
preview**. Pages of signed-in members are never stored
by shared caches (`private, no-store`) and the anonymous caches are not used for them; the stylesheet of a skin no
page uses is served `private`, and that of previewed colors only to the administrator previewing them, `private` too
(see "The stylesheet of a skin").

## The hooks

The hooks are a contract: renaming or removing one needs a reason and an update of this document, and new hooks are
added here when they are added to the templates. `apps/web/tests/skin_hooks_http.rs` renders the home page, a board
page, a child board, a thread page, a member's space, search and the forum directory, and fails when a hook listed
here is missing. The general rules for class names and state attributes are in [styles.md](styles.md),
"Class names". The default look does not use the optional elements, so outputting them changes nothing a visitor
sees.

## List rows

Thread lists (the home page, board pages and search, `templates/home/index.html`) and the cross-forum pins above
page 1 of a board (`partials/cross_pins.html`) use `ul.rows.rows--topics`. Each `li.rows__item` carries:

| Attribute | Value |
| --- | --- |
| `data-thread-id` | The thread id. Scripts use it too (scroll restoration, the recent threads card). |
| `data-board` | The `fid` of the thread's own board (a child board, not its parent). |
| `data-kind` | The thread's category: `bounty`, `redpacket`, `lottery`, `poll` or `news`. Ordinary threads have no `data-kind`. |
| `data-state` | Space-separated, in this order: `pinned` (at any pin level), `featured`, `closed` (locked). Absent when none applies. Select one state with `[data-state~="closed"]`. |

`data-state` says what the thread is, whatever the list shows: in the "Pinned" view every row is `pinned`, although
the list leaves out the "Pinned" tag there. `rows__item--pinned` keeps marking the rows the list shows as pinned.

Inside the row:

| Class | What it holds |
| --- | --- |
| `row-labels` | The tags of the headline (pinned, featured, category) and the optional pin and lock icons, before `row-title`. It is `display: contents`, so the tags stay items of the headline's flex row; it is left out when the row has none of them. |
| `row-meta__author` | The author: an icon and `.row-author`. |
| `row-meta__board` | The board, where the row names it: a child board's thread in its parent board's list (`[data-child-forum]`), and every cross-forum pin. |
| `row-meta__replies` | The reply count (see "Counts"). In the main list it is an icon and the number, named for screen readers by its `aria-label`; in the pins block the number and the word. |
| `row-meta__views` | The view count, after the reply count and built the same way: an icon and the number in the main list, the number and the word in the pins block, named by its `aria-label` in both. Approximate ([queries.md](queries.md), "Views"). |
| `row-meta__last` | The last replier, when the thread has replies. |
| `row-meta__time` | The time of the last activity, a `<time>` (see "Times"); in the main list its `title` is the full date. |

Other lists use the same names for the same items where they have them: a member's space (`user/profile.html`:
`row-meta__board`, `row-meta__time`, `row-meta__replies` on threads, and `row-labels` around the category tag) and the
following feed (`social/feed.html`: `row-meta__author`, `row-meta__time`, `row-meta__board`, `row-meta__replies`).
They carry no `data-board`, `data-kind` or `data-state`. The forum directory (`site/forums.html`) has
`row-meta__threads` and `row-meta__today` for its two counts.

## Counts

A count shown with a word is split into the number and the word, so that a stylesheet can, for example, put the
number above the word:

```html
<span class="row-meta__replies"><span class="stat"><span class="stat__value">6</span> <span class="stat__label">replies</span></span></span>
```

- `stat` holds the count; `stat__value` is the number, written the way the language writes numbers (as `{ $count }`
  in a translation would be), and `stat__label` the word.
- The order and the space between them come from the translation: English writes "6 replies", and Simplified
  Chinese puts the word first in some of these counts. Each such count has two messages, the word (`…-label`, which picks its plural form from
  `$count`) and the order (`{ $value_html } { $label_html }`); see [translating.md](translating.md), "Rules for
  messages". The text reads exactly as one message did.
- `stat` is an element of its own inside the meta item, so the space between number and word survives the meta
  item's flex layout.
- Counts shown without a word keep `stat` and `stat__value` only: the reply and view counts of the main list, whose
  words are in the `aria-label`.

Counts split into number and word: the reply and view counts of cross-forum pins, the reply count of threads in a
member's space and in the following feed, and the forum directory's thread count and posts today. The thread head
shows its reply and view counts as one phrase each, `span.thread-stats__count` and `span.thread-stats__views`.

## Times

Every time shown in lists (home, boards, search, cross-forum pins, a member's space, the following feed), on the
thread page (the opening post, each floor and nested reply, highlighted replies) and the sign-up date on profiles and
in member search is a `<time>` element whose `datetime` is the instant in UTC to the second:

```html
<time class="row-meta__time" datetime="2026-09-08T01:58:00Z" title="Sep 8, 2026, 01:58">Sep 8, 01:58</time>
```

The shown text and the `title` follow the site's time settings as before. A script that writes times itself reads
`datetime`. The sign-up date sits inside a translated sentence ("Joined …") and passes through a markup slot. The
profile's "Last seen" is not a `<time>`: it is only shown to the day, and an exact instant would publish more than
the page says.

## Optional elements

These elements are in the page but hidden in the default look: they carry `data-optional`, and `site.css` gives
`[data-optional]` `display: none`. A later rule that sets `display` shows them (unlike `hidden`, which always hides).
Hidden with `display: none`, they are not read by screen readers either, so each one notes what it stands in for: a
stylesheet that shows one should hide what it replaces, so that nothing is read twice.

| Element | Where | Stands in for |
| --- | --- | --- |
| `span.row-end[data-optional] > a.board-chip` | The end of every topic list row and cross-forum pin: the thread's board as a link. | `.row-meta__board`, where the row has one. On the home page and in search rows have no other board label. |
| `span.row-icon.row-icon--pinned[data-optional]` | In `.row-labels` of rows that show the "Pinned" tag: a pin icon with `sr-only` text ("Pinned"). | `.tag--pinned` of the same row. |
| `span.row-icon.row-icon--locked[data-optional]` | In `.row-labels` of locked threads: a lock icon with `sr-only` text ("Locked"). | Nothing: no other element of a row says the thread is locked. |
| `svg.floor__action-icon[data-optional]` | First in each floor's Reply and Quote action (`a.floor__action--reply`, `a.floor__action--quote`), whose text is in `span.floor__action-label`. The icon is a `<use>` of a symbol the thread page defines once (`#action-icon-reply`, `#action-icon-quote`), 14px, drawn in the text color; the Reply arrow mirrors in right-to-left pages. | Nothing. A stylesheet that shows only the icon must keep the label readable, hidden visually the way `sr-only` does it, not with `display: none`, or the link loses its name. |
| `a.thread-head__reply[data-optional]` | First in the thread head's tools: a "Reply" button linking to `#reply-editor`, the reply box, which every thread page has (for locked threads and visitors who cannot reply it holds the notice instead of the editor). | Nothing: it adds a shortcut. |

Two more hooks change the default markup without hiding anything:

- **Breadcrumb separators** are empty `span.crumbs__sep` elements with `aria-hidden="true"`; `home.css` draws the
  "›" with `::before` and mirrors it in right-to-left pages. A stylesheet can draw something else or nothing.
- **The thread page's category tag** has the same class as in lists (`tag--bounty`, `tag--redpacket`,
  `tag--lottery`, `tag--poll`; `news-badge` for news), so it has the category's color there too.

## Page data

Every page built on `layout/base.html` has one data block in its `<head>`:

```html
<script type="application/json" id="gonof-page">{"v":1,"type":"thread","tid":103,…}</script>
```

Read it with `JSON.parse(document.getElementById('gonof-page').textContent)`. Like the JSON-LD blocks, it is data and
not a script, so the Content-Security-Policy allows it without a nonce; `<`, `>` and `&` in values are written as
JSON escapes (`<`), so no value can end the element. It holds only what the page shows, and nothing personal: no
`me.*` data source and nothing a page shows only about its viewer. The records of the page's data-source blocks are
those this visitor sees on the page (filtered by the boards they may read and the members they blocked, like every
list), so a member's block can differ from an anonymous visitor's there.

Version 1 has these fields, always all of them, `null` where a field does not apply. It only grows: a new field is
added to every page, and removing or changing one means a new `v`.

| Field | Value |
| --- | --- |
| `v` | `1`. |
| `type` | `home` (`/`, `/page-N`), `board` (`/<slug>`), `thread`, `member` (`/space/<uid>` and its tabs), `search` (`/search`), or `other` for every other page. |
| `tid` | The thread of a thread page. |
| `fid` | The board of a board page; the board of the thread on a thread page. |
| `uid` | The member whose space this is. |
| `page` | The page number of a list, a thread or a member's tab. |
| `thread` | On thread pages, `{"replies", "pages", "floors"}`: the reply count the head shows, the number of pages in the view being read (flat or nested), and the floors of this page in page order, `{"position", "pid", "at"}` each. The opening post comes first on every page; in the nested view each reply is followed by its nested replies. `position` is the `N` of `#floor-N`, `at` the post time in UTC as in `<time datetime>`. Hidden and withdrawn floors are listed too, as the page shows their placeholders. |
| `site` | `{"name", "description", "language", "dir", "base", "credit_name", "index_name", "date_format"}`: this site's name (another domain's own name where it has one), its description, the interface language tag, `ltr` or `rtl`, the path prefix of a forum installed under a sub-path (empty at the root), the names of points and of the activity index, and the date style (`locale`, `ymd-dash`, `ymd-cn` or `mdy-slash`). Empty names and the description are filled with the language's defaults, as on the page. |
| `blocks` | The page's data-source blocks by block id, each the envelope `/data/v1` would answer with these parameters ([data.md](data.md)), `current` resolved: on the home, board and thread pages; `{}` elsewhere. Core blocks are not data sources and are not listed. |

The block describes the page as the server sent it. A boosted navigation (`hx-boost`) brings the new page's block
with its head; a fragment swapped into the page, such as a reply posted with HTMX, does not change it.

## Script events

`static/js/site.js` dispatches two events on `document`, after its own handlers:

| Event | When | `detail` |
| --- | --- | --- |
| `gonof:ready` | Once, at `DOMContentLoaded`. | None. |
| `gonof:swap` | After every HTMX swap has settled (`htmx:afterSettle`), boosted navigations included. | `{ target }`: the swapped element, `document.body` after a boosted navigation. |

```js
document.addEventListener('gonof:ready', start);
document.addEventListener('gonof:swap', event => { if (event.detail.target === document.body) start(); });
```

## Sticky header

The header (`.masthead`) sticks to the top of the viewport. How much room it takes is declared in CSS rather than
measured:

- `--masthead-h` is the header's height with its 1px bottom border and without the safe area: 61px, 57px from 900px
  down and 53px from 760px down. `frame.css` sets it beside the header's other sizes and builds the header row's
  height from it.
- `--sticky-top` is `--masthead-h` plus the safe area above it (`--safe-top`): the height the header covers. It is
  registered with `@property` as a length, so `getComputedStyle(document.documentElement).getPropertyValue('--sticky-top')`
  returns the resolved value, such as `61px`.
- Anchor targets (floors, the reply box, panels) take it as their `scroll-margin-block-start`, plus 11px, sticky
  asides as their top offset, plus 15px, and the side navigation as its top offset.
- `site.js` (the reading-position tracker of thread pages) and `navigation.js` (scroll restoration of lists) read
  `--sticky-top` instead of measuring where the header ends. A browser without `@property` returns the declared
  `calc()`; the scripts then let the browser resolve it on a hidden probe element.

A layout whose sticky area is not the top header declares its own `--masthead-h` (or `--sticky-top`), and the
scripts and scroll margins follow. Under the `autohide-header` behavior module ("Behavior modules") `--sticky-top`
drops to the safe area while the header is away.
