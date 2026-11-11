# Styles

How the interface's CSS, colors, class names and icons are organized, for anyone who changes templates or styles.
The fonts, images and vendored files the interface ships are listed in [assets.md](assets.md).

## Where styles live

- **Sources** are plain CSS in `apps/web/frontend/styles/`. `index.css` imports the other files in a fixed order, and
  `themes/default.json` holds every color.
- **Build.** `npm run build` (or only `npm run build:styles` after a style change) runs `scripts/build-styles.mjs`. It
  turns the color scheme into custom-property rules, bundles them with `index.css` through esbuild into
  `apps/web/static/css/forum.css`, and bundles `debug.css` on its own into `apps/web/static/css/debug.css`. The output
  is not minified. Both files are git-ignored build output: do not edit or commit them. `cargo build` does not build
  CSS, so run `npm run build` after a style change; `scripts/package-release.sh` runs it itself and compiles the
  built files into the release binary.
- **`apps/web/static/css/fonts.css`** is a source file, not build output: the `@font-face` rules for Space Mono.
- **Loading.** `layout/base.html` links `fonts.css`, then `forum.css`, then `debug.css` only while the debug panel is
  on. Every static URL carries `?v=` with the asset version, so a new build is fetched at once.
- **Pages outside the frame.** `errors/plain.html` links only `forum.css`. The setup wizard (`setup/wizard.html`) has
  its own inline styles and colors, because it runs before the forum's address is known and serves no assets.
- **Personal styles.** A member may write a personal stylesheet; `custom_css.rs` validates it, and the member's own
  pages link it after `forum.css`. Class names are therefore visible to members, so rename them only for a reason.

## Style files

In `index.css` order; later files win at equal specificity.

| File | What it holds |
| --- | --- |
| `foundation.css` | Size, font, easing and safe-area variables |
| `elements.css` | Element defaults: box sizing, body text, links, headings, code, media, the font of form controls, focus |
| `controls.css` | Rules no component owns: controls wrapped in their `label`, buttons without a class, `heading`, two-step verification, recovery codes, OAuth callback addresses |
| `views.css` | Smaller page parts: the text column of policy pages, data tables, conversations, notification titles, attachment quotas, member search |
| `prose.css` | Rendered post bodies (also on policy pages, previews and the admin content view): `prose`, code blocks, spoilers, in-site thread cards, video placeholders, emoji zoom; the thread title block |
| `components.css` | The shared components (see "Shared components"), buttons, panels, tags, pagination, avatars, and the spacing between blocks in the main column |
| `frame.css` | The page frame: skip link, header, "More", drawer, notices, footer, page width, and the header navigation tiers |
| `thread.css` | Thread page: head, floors, interaction buttons, tips, highlighted replies, replies, reply box, moderator tools |
| `home.css` | Listing pages (home, board, search): `listing`, `board-strip`, `crumbs`, `board-head`, `pane` |
| `profile.css` | Member profile, follow and block, policy pages (`reader-layout`, `policy-nav`) |
| `settings.css` | Account security, profile settings, avatar check |
| `editor.css` | The editor's template parts: `editor-page`, `editor`, uploads, poll, deadline and prize setup |
| `site.css` | `[hidden]`, `sr-only`, avatar initials, reduced motion, print, and page rules for invites, directories, check-in and the page jump button |
| `personalization.css` | Avatar shape preference and the personal style page |
| `manage.css` | Admin area: module menu and admin shell |
| `composer.css` | What `composer.js` builds: toolbar and its icons, full screen, emoji and mention menus, attachment rows |
| `lottery-prizes.css` | Lottery prize rows in the posting form |
| `debug.css` | The debug panel (`gonof-debug`); built and linked separately |

`components.css`, `frame.css`, `thread.css`, `home.css`, `profile.css`, `settings.css` and `editor.css` open with a
comment listing their blocks. Put a new block in the file whose area it belongs to and add it to that comment.

## Color scheme

`themes/default.json` is the only color scheme; light, dark and "follow the system" all use it. It has three objects:

- `shared`: tokens common to both modes, often references to other roles (`"--selected-bg": "var(--primary-bg)"`), so
  one role follows another until it is given a value of its own.
- `light`: the light values. A token here overrides the same token in `shared`.
- `dark`: only what differs in dark mode; everything else comes from `shared` and `light`.

The build writes light values on `:root[data-palette="default"]` (and on `:root` without `data-palette`), dark values
under `[data-theme="dark"]`, and dark values again under `@media (prefers-color-scheme: dark)` when `data-theme` is
neither `light` nor `dark`. `layout/base.html` sets `data-palette="default"`; `site.js` sets `data-theme` from the
theme switch (remembered in `localStorage` as `gonof-theme`) and removes it for "system". Without JavaScript a page
follows the system setting.

The build stops on an invalid token name or value (braces, semicolons, `@import`, or a `url()` other than an inline
`data:image/svg+xml`), a `var()` reference to an undefined token, a reference cycle, or light and dark modes with
different token sets. The interface has no palette picker; another `themes/<name>.json` with the same tokens would be
built as a palette selected by `data-palette="<name>"`.

Stylesheets contain no literal colors: every color is a `var(--…)` from the scheme. The black in the SVG masks in
`composer.css` only gives the mask its shape. Reuse a role where its meaning fits. When a component needs a color of
its own, as the tags and the favorite button do, add a token named after its use to `light`, and to `dark` where it
differs.

## Color roles

| Roles | Use |
| --- | --- |
| `--bg`, `--panel`, `--panel-alt`, `--panel-hover` | Page ground, panels, tinted and hovered surfaces |
| `--line`, `--line-soft`, `--line-strong` | Borders and rules |
| `--text-strong`, `--text`, `--text-muted`, `--text-subtle`, `--text-disabled` | Text, from strongest to weakest |
| `--accent`, `--accent-hover`, `--accent-soft`, `--accent-border` | Interactive emphasis, current items |
| `--link`, `--link-hover` | Links in running text |
| `--focus-ring` | Keyboard focus outline of links, buttons and other controls that are not form fields |
| `--brand-bg`, `--brand-text`, `--brand-hover-bg` (from `--mark`, `--mark-deep`, `--mark-text`) | The text brand block in the header |
| `--primary-bg`, `--primary-text`, `--primary-border`, `--primary-hover` | Main actions |
| `--secondary-bg`, `--secondary-text`, `--secondary-border`, `--secondary-hover` | Secondary actions |
| `--selected-bg`, `--selected-text`, `--selected-border` | Selected controls |
| `--disabled-bg`, `--disabled-text`, `--disabled-border` | Disabled controls |
| `--input-bg`, `--input-text`, `--input-border`, `--input-placeholder`, `--input-focus`, `--choice-accent` | Form controls; `--input-focus` is the border of a focused field and the focus line of checkboxes and radios, `--choice-accent` the `accent-color` of checkboxes and radios |
| `--editor-bg`, `--editor-text`, `--toolbar-selected-bg`, `--toolbar-selected-text` | The post editor |
| `--success*`, `--warning*`, `--danger*`, `--info*` | Status messages, banners and tags: the base color, a `-soft` ground with `-soft-text`, and `-border` or `-fill` where needed |
| `--favorite-border`, `--favorite-text`, `--reaction-active` | Favorite and like buttons when pressed |
| `--code-inline-*`, `--code-block-*`, `--quote-*` | Post bodies: code and quotes; previews use the same set |
| `--tag-<kind>-bg`, `--tag-<kind>-text` | Thread tags (see "Tag colors") |
| `--avatar-bg`, `--avatar-text`, `--avatar-green-*`, `--avatar-amber-*` | Initials tiles for members without an avatar |
| `--inverse`, `--inverse-text`, `--backdrop`, `--selection` | The skip link, the drawer backdrop, text selection |
| `--shadow-base`, `--shadow-medium` | Panels and cards; menus, the drawer and other raised surfaces |
| `--sticky-top` | Not a color: the height the sticky header takes, which sticky asides and floor anchors leave free |

## Tag colors

Tags (`tag`) mark pinned and featured threads and the thread categories. Each kind has its own hue, the label always
carries text so color is not the only cue, and every pair keeps at least 4.5:1 contrast in both modes. The thread
categories map to classes in `content.rs` (`badge_class`).

| Tag | Class | Light text / ground | Dark text / ground |
| --- | --- | --- | --- |
| Pinned | `tag--pinned` | `#c2410c` / `#fff0e5` | `#fdba74` / `#3b2518` |
| Featured | `tag--featured` | `#856300` / `#fff8cc` | `#f2d675` / `#332e16` |
| Bounty | `tag--bounty` | `#287a39` / `#eaf6e9` | `#9bd58c` / `#20331e` |
| Red packet | `tag--redpacket` | `#b4232c` / `#fdebec` | `#fda4ab` / `#3b1d23` |
| Lottery | `tag--lottery` | `#a02d87` / `#fbeaf7` | `#e9a0d6` / `#361d32` |
| Poll | `tag--poll` | `#1266a8` / `#e7f2fd` | `#8bc5fa` / `#192d43` |
| News | `tag` (no modifier) | `#586174` / `#eff1f5` | `#bac2d0` / `#272d39` |

The plain `tag` is the neutral style with a thin border. `tag--level` uses the accent, `tag--verified` the success
roles, `tag--honor` the warning roles and `tag--state` (frozen or deleting accounts) the danger roles.

## Design variables

| Variable | Value | Use |
| --- | --- | --- |
| `--r-xs`, `--r-sm`, `--r-input`, `--r`, `--r-pill` | 4, 6, 8, 12, 9999 px | Corner radii: tags; navigation items; inputs and full-width buttons; panels; buttons, chips and the search box |
| `--container` | 1200px | Maximum page width |
| `--sidebar-w` | 260px | Aside column width |
| `--gap` | 20px | Gap between the main column and the aside, and between aside cards |
| `--post-image-max` | 720px | Widest image in a post body |
| `--font-sans` | system UI fonts, with CJK fallbacks | Interface and post text |
| `--font-mono` | Space Mono, then system monospace | Code, digits and other monospaced text |
| `--ease-out` | `cubic-bezier(0.23, 1, 0.32, 1)` | Transitions |
| `--safe-top`, `--safe-right`, `--safe-bottom`, `--safe-left` | `env(safe-area-inset-*)`, 0 where there is none | Room for notches and gesture bars |

Body text is 15px with a line height of 1.75; form controls are 14px (16px from 420px down, so phones do not zoom on
focus). Emphasis uses font weights 500 and 600 only. Borders are 1px; accent lines (the underline of current tabs
and boards, the start edge of quotes, nested lists and error messages) are 2px. `site.css` switches transitions and
animations off when the reader prefers reduced motion.

## Breakpoints

Styles are written for wide screens and narrowed with `@media (max-width: …)` at six widths only:

| Width | What changes |
| --- | --- |
| 900px | The header board navigation gives way to the drawer button; the aside narrows to 220px and gaps shrink to 16px; policy pages move their navigation above the text; the admin module menu becomes a scrolling row |
| 760px | One column; in `layout--stack` the aside keeps only cards marked `panel--keep`; listing pages show the board strip; the header tightens and sign-up moves into the drawer; only the first pinned row stays in a list |
| 700px | The footer becomes one centered column |
| 640px | Two-column admin forms become one; row forms move under the row text; floors put the avatar beside the head; policy links become a row of pills |
| 480px | Six-figure tallies become three columns; list thumbnails shrink |
| 420px | Board chips in list rows are hidden; form controls are 16px; the header drops the notifications link and the account caret |

Do not add other widths. The only exception is the header navigation, which `frame.css` switches at 1199, 1099 and
999px (see "Header navigation widths"). Queries for `prefers-reduced-motion` and `print` are fine; dark mode comes from
the color scheme build, not from `prefers-color-scheme` rules in the stylesheets.

## Header navigation widths

The server decides which boards stay in the header and which go into "More" (`apps/web/src/router/nav_fit.rs`): it
estimates each label's width from its characters and fits the items into a budget per width tier (`TIERS`: from 901,
1000, 1100 and 1200px). An item that does not fit at every width records the lowest tier that keeps it inline in
`data-fit`, and "More" records in `data-until` the tier from which it is no longer needed; `frame.css` shows
exactly one copy of each board at any width. From 900px down the header navigation is hidden and the drawer lists
every board.

The estimate is only right while the CSS matches the constants in `nav_fit.rs`:

- `.masthead__board`: 14px text, `padding-inline: 9px`, `min-inline-size: 3.9em`; items 2px apart.
- "More" (`.forum-more__toggle`, also a `.masthead__board`): its label, a 12px chevron and the same padding.
- `.masthead__action` (the visitor's log-in and sign-up buttons): `padding-inline: 10px` and a 1px border.
- `.masthead__brand`: 16px, weight 600.
- `.masthead__search`: a fixed 150px box wherever the header navigation shows.

Change one of these and you must change the constants in `nav_fit.rs` as well, and measure the budgets in `TIERS`
again on the visitor's header.

## Class names

- **Blocks, elements, modifiers:** `block`, `block__element`, `block--modifier`, and `block__element--modifier`
  (`tally__value--alert`). Lowercase words joined by hyphens, named after this project's own components.
- **State goes in attributes** where one fits: `aria-current` for the current item (tabs, board navigation, pagination,
  menus), `aria-pressed` for toggles, `[open]` on `details`, `disabled` or `aria-disabled="true"`, `hidden`, and
  `data-state` for a record's state. Otherwise use an `is-*` class (`is-unread`, `is-today`, `is-drop-target`); a
  class that switches a whole layout is `has-*` (`has-write-fullscreen`).
- **Script hooks** are `data-*` attributes (`data-theme-toggle`, `data-thread-id`, `data-media-load`), not style
  classes. Some Rust tests match markup literally, so run them after renaming a class.
- Helpers: `container` (page width), `sr-only` (visually hidden).

The blocks of each area are listed in the opening comment of its file (see "Style files").

## Shared components

Six components carry most pages. Use them rather than writing page-specific markup for the same job.

- **Form fields.** A form is `fields` (`fields--two` for two columns in the admin area, `fields--inline` for filter
  bars). Each control sits in a `div.field` with `label.field__label` (`for` the control's `id`), the control, and an
  optional `p.field__hint` that the control names in `aria-describedby`; `field--wide`, `field--required` (a visual
  mark only; the control keeps `required`) and `field--code` adjust it. A checkbox is
  `div.checkline > label.checkline__label > input + span`; radio and checkbox groups are `fieldset.option-group` with
  `option-group__legend`, `__hint` and `__options`. Messages are `form-feedback` (an error with `role="alert"`, or
  `--success`, `--info`, `--warning`) and `form-note`; buttons go in `form-actions` (`--sticky` on long admin forms).
  `control` styles a single control outside a field.
- **Narrow pages.** Sign-in, registration, password reset and one-time links use `div.narrow-page` (400px, centered)
  around `section.panel.panel--auth`, with `nav.tabs.tabs--auth` across the top and one `button button--block`
  action. `narrow-page--embedded` is a 640px column inside the member area.
- **Tabs.** `nav.tabs` with `a.tabs__item`, the current one marked `aria-current="page"`; `tabs--switch` is a
  segmented two- or three-way switch, and `section-tabs` are the scrolling sub-sections of the admin area.
- **List rows.** `ul.rows` (`--topics`, `--posts`, `--ranks`, `--children`) of `li.rows__item` (`--pinned`):
  `row-avatar`, then `row-body` with `row-headline` (tags, `row-title`, `row-pages`) and `row-meta`, then optional
  `row-end` or `row-actions`. An empty list is `li.empty`.
- **Aside cards.** `section.panel.panel--aside` with `h2.panel__title` and the card's content, for example `mini-list`
  (`__title`, `__count`, `__empty`). They sit in `layout__aside`; add `panel--keep` to a card that should stay on
  phones.
- **Member card.** `person` (`__avatar`, `__ident`, `__name`, `__role`, `__shortcuts`; `--large` on profiles) inside
  `person-card` (`__actions`, `__nav`, `__link`, `__dot`), with figures in a `dl.tally` (`__item`, `__label`,
  `__value`; `tally--six` on profiles).

Buttons are `button` (the primary pill), `button--secondary`, `button--danger`, `button--small` and `button--block`.
`button--primary` may be written to state intent; it has no rules of its own. A `button` without any class also gets
the primary look. Icon-only buttons are `icon-button`.

## Logical properties and right-to-left

Write flow-relative properties so right-to-left languages mirror the layout without extra rules: `margin-inline-start`,
`padding-inline`, `padding-block`, `inset-inline-end`, `inset-block-start`, `border-inline-start`,
`border-start-start-radius`, `inline-size`, `block-size`, `min-inline-size`, `text-align: start` or `end`. Do not write
`left`, `right`, `margin-left`, `width` and the like; use the `block` forms for the vertical direction too. Media
queries are the exception and use `max-width`.

`<html>` gets `dir="rtl"` for right-to-left languages (from CLDR, `p.t.is_rtl()`); left-to-right pages carry no
`dir`. What logical properties cannot express is written under `:dir(rtl)`: the drawn caret of selects, the fade at
the end of scrolling filter rows, and glyphs that point along the reading direction (`meta-glyph--mirror`, the
breadcrumb separator), which flip with `scale: -1 1`.

`site.css`, `manage.css`, `personalization.css`, `lottery-prizes.css` and `debug.css` still contain some physical
properties; convert a rule to logical properties when you change it.

## Icons

Icons are inline SVG drawn for the project, with no icon font or external sprite. They follow one pattern:

```html
<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"
  stroke-linejoin="round" aria-hidden="true" focusable="false">…</svg>
```

A 24 × 24 grid, round line caps and joins, and the color of the surrounding text. The SVG is hidden from assistive
technology; the text beside it names the control, or an icon-only control gets an `aria-label` (usually with
`title`).

- An icon used in one place is written inline in its template (`partials/header.html`, `home/index.html`,
  `partials/user_card.html`, `partials/my_card.html`). An icon used in several places is a partial:
  `partials/favorite_icon.html`, `reaction_icon.html`, `downvote_icon.html`, `tip_icon.html`, `user_icon.html`.
- List metadata icons get the class `meta-glyph` (14px). Add `meta-glyph--mirror` to an icon that points along the
  reading direction.
- Admin module icons are `<symbol>` elements in `partials/manage_icons.html` (stroke width 1.6), used as
  `<svg><use href="#manage-icon-<name>"></use></svg>`. A new module adds its symbol there.
- Editor toolbar icons are SVG masks in `composer.css` (`.tool-*::after` with a `data:image/svg+xml` URI), painted
  with `background-color: currentColor`.
- Icons are static; nothing animates.

Draw new icons yourself, or take them from a set whose licence allows it and record it in [assets.md](assets.md).

## HTML conventions

- Front pages extend `layout/base.html`, the member area `layout/my.html` and the admin area `layout/manage.html`.
- No visible text is written in templates: every string is a translation key (`{{ p.t.some_key() }}`); see "For
  developers: adding interface text" in [translating.md](translating.md).
- No `style` attributes, `<style>` blocks or inline scripts (a JSON-LD block is data, not a script). The page's
  Content-Security-Policy allows styles and scripts only from the site.
- Member names, the brand and other names carry `translate="no"`.
- Visually hidden text uses `sr-only`. `hidden` always hides: `site.css` gives `[hidden]` precedence over any
  `display` rule.
- Every focusable element shows keyboard focus. Links, buttons and other controls get a `:focus-visible` ring,
  normally a 2px outline in `--focus-ring`. Form fields get no ring: text fields, selects and textareas darken their
  own border to `--input-focus` (the header search darkens its box), and checkboxes and radios, which keep the native
  look, get a 1px `--input-focus` line around their own shape (a `drop-shadow` filter, since browsers draw outlines
  of native radios square). File inputs look like buttons and keep the ring. A field built
  by a script takes the `control` class so that it has a border to darken. `test:checks` fails on a Tab stop that
  shows neither.
- Toggles (favorite, like, downvote) use `aria-pressed`; a favorite placeholder that cannot be pressed uses
  `data-active` instead of ARIA.

## Pages without JavaScript

Every page works without JavaScript; scripts only add to it. Forms post and redirect; with scripts, HTMX
(`hx-boost` on `<body>`) turns the same links and forms into partial page updates. The drawer, "More" and the account
menu are `<details>` elements and open without scripts. Controls that need a script start out `hidden` and the script
reveals them: the theme switch, the drawer's close button and the video "load player" button. Rules that only make
sense with a script are scoped to `html[data-js="true"]`, which `site.js` sets, such as the full-height drawer with
its backdrop.

## No external resources

A page loads stylesheets, scripts, fonts, images and API responses only from its own site: no CDNs, remote fonts,
remote images or third-party scripts. The Content-Security-Policy enforces this (`default-src 'self'`,
`img-src 'self' data:`). The one other host it allows is `www.youtube-nocookie.com`, for the frame a reader loads by
clicking a video placeholder. A theme value may contain `url()` only as an inline `data:image/svg+xml`.

A third-party file goes under `apps/web/static/`, with its licence text in `docs/licenses/`, a line in `NOTICE` and an
entry in [assets.md](assets.md).

## Checking changes

The browser checks run the debug binaries with the built assets, so build both first:

```bash
cargo build -p gono-forum-web --bins
npm run build
npx playwright install chromium   # once
npm run test:checks               # tests/e2e/checks.spec.js
npm run test:screens              # tests/e2e/screens.spec.js
npm run test:e2e                  # both
```

They start throwaway local sites (`tests/e2e/support/instance.js`): SQLite in a temporary directory, Redis off,
random keys and ports, the demo content and a signed-in administrator, removed afterwards. Nothing connects to MySQL,
Redis or a remote host. The pages are listed in `tests/e2e/support/pages.js`, and each is opened at 1440 × 900 and
390 × 844, light and dark.

- `test:checks` runs on the English and the Simplified Chinese site. It fails on sideways scrolling, console errors,
  a Tab stop without a visible focus ring (or, on a form field, an edge that darkens), or Chinese characters on an
  English page (apart from a language's own name and the sample Chinese date format).
- `test:screens` takes full-page screenshots on the English and Chinese sites into
  `.local/visual/<label>/<language>/<viewport>/`, where the label is `GONOF_VISUAL_LABEL` (default `latest`).
- `node scripts/visual-compare.mjs before after` builds `.local/visual/compare-before-after.html` from two labelled
  screenshot sets: each page side by side, filterable by language, viewport and page. Take `before` on the old code
  and `after` on the new, and judge the change by eye; there are no pixel thresholds.
- `node scripts/ui-measure.mjs measure <label>` crawls the pages a visitor and the administrator can reach on a
  throwaway English site and records the computed styles of every element at 1440px light (with forced hover, focus
  and active states unless `--fast`), 390px light and 1440px dark; `compare <before> <after>` lists what changed per
  element. `widths` measures in-between widths, `sweep` finds the widths where a layout switches, and `report`
  summarizes one run. Results go to `.local/ui-measure/`.

Everything under `.local/` is git-ignored and is not committed.
