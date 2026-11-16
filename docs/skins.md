# Skin hooks

The templates are Askama, compiled into the binary, so the HTML structure of a page cannot change at run time. What
a stylesheet or a script can rely on instead is a set of public hooks the core templates write on purpose: fixed
class names, `data-*` attributes for states, raw values next to the formatted ones, optional elements that are output
but hidden, a page data block, two script events and a declared sticky header height. This document lists them.

There is no skin loader yet. Today the hooks are what a member's personal stylesheet (`custom_css.rs`, see
[styles.md](styles.md), "Where styles live") and page scripts can build on; a skin layer is meant to build on the same
hooks. The default look does not use the optional elements, so outputting them changes nothing a visitor sees.

The hooks are a contract: renaming or removing one needs a reason and an update of this document, and new hooks are
added here when they are added to the templates. `apps/web/tests/skin_hooks_http.rs` renders the home page, a board
page, a child board, a thread page, a member's space, search and the forum directory, and fails when a hook listed
here is missing. The general rules for class names and state attributes are in [styles.md](styles.md),
"Class names".

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
