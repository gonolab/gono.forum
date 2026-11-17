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
nav = "top"                          # only "top" for now
header_boards = true                 # only true for now
masthead_height = "52px"             # optional: the declared header height (--masthead-h), 40 to 120 px
avatar_shape = "member"              # circle, rounded or square fixes every avatar; member leaves it to members
```

Unknown keys are errors. `[behaviors]` and `[layout]` are part of the contract but not supported yet: they must be
empty. A skin brings no interface text: names and descriptions are written per language, everything else on the page
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
| The fixed values of the style guide: weights 400, 500 and 600; radii 0, 4, 6, 8, 12, 9999 px and 50%; borders 0, 1 and 2 px; body text 15px with a line height of 1.75; the design variables (`--r*`, `--container`, `--sidebar-w`, `--gap`, `--post-image-max`, `--masthead-h`, `--sticky-top`, `--safe-*`) | Blocks | Warns |
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
  and `--panel`.
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
 "colors": {"primary": "#0f766e", "secondary": "#f97316"}}
```

An empty body means the built-in default with its own colors. `based_on` records the skin and version the choice was
made from; `places` (the blocks of each place on a page) stays `null` until layouts can be edited. `colors` holds the
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

## Frame

The skin's manifest and the request's choice are written on `<html>`:

| Attribute | Value |
| --- | --- |
| `data-skin` | The skin's id. |
| `data-skin-source` | `builtin`, `imported` or `instance`. |
| `data-palette` | The palette in use. |
| `data-nav` | `frame.nav`: `top`. |
| `data-header-boards` | `frame.header_boards`: `true`. |
| `data-avatar-shape` | `circle` or `square` when the skin fixes that shape; when it leaves the shape to members, `circle` for a member who chose round avatars. Absent for the default rounded corners. |

When the skin fixes the avatar shape, a member's reading settings say so; their own choice is kept and applies again
under a skin that leaves the shape to them.

A boosted navigation (`hx-boost`) replaces the body and merges the head but keeps `<html>`; `site.js` copies these
attributes from the new page in the same step as the new skin stylesheet arrives, so moving between pages with
different skins (`/manage/skins` always uses the default) never pairs one skin's stylesheet with another's palette.

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

## Preview

"Preview" shows a skin, or unsaved seed colors, to the administrator who started it, on every page, without changing
what anyone else sees. The choice lives in a cookie sealed with the cookie key (`gonof_skin_preview`, eight hours) and
is applied only while the signed-in member is that administrator; publishing, exiting and signing out clear it. A
preview of a skin shows it with the colors the page has anyway (the site's or the domain's seeds, derived again for
that skin); a preview from Appearance › Colors shows the entered seeds over the site's skin and replaces any preview of
a skin. A bar at the top of every page offers **Publish** (make the skin the site's, recording `skin.select`, or save
the colors as the site's, recording `skin.colors`) and **Exit preview**. Pages of signed-in members are never stored
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
- Counts shown without a word keep `stat` and `stat__value` only: the reply count of the main list, whose word is in
  the `aria-label`.

Counts split into number and word: the reply count of cross-forum pins, of threads in a member's space and in the
following feed, and the forum directory's thread count and posts today.

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
JSON escapes (`<`), so no value can end the element. It holds only what the page shows every visitor who may open
it, never anything about the viewer: a signed-in member gets the same block as an anonymous visitor.

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
- Anchor targets (floors, the reply box, panels) take it as their `scroll-margin-block-start`, plus 11px, and sticky
  asides as their top offset, plus 15px.
- `site.js` (the reading-position tracker of thread pages) and `navigation.js` (scroll restoration of lists) read
  `--sticky-top` instead of measuring where the header ends. A browser without `@property` returns the declared
  `calc()`; the scripts then let the browser resolve it on a hidden probe element.

A layout whose sticky area is not the top header declares its own `--masthead-h` (or `--sticky-top`), and the
scripts and scroll margins follow.
