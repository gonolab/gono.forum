# Changelog

What changed in each release of Gono Forum. A version's section is also the title and text of its GitHub release
(see "Publishing a release" in [docs/releasing.md](docs/releasing.md)), so links in a section are absolute: the
release page does not resolve relative ones.

## 0.2.0 (early alpha)

A new minor version for two reasons: skins and layouts, which change a site's look and arrangement without building
anything, and an incompatible change to the error answers of the member API.

### Incompatible changes

- The member API (`/api/v1`) answers errors as
  `{"code": "invalid_token", "error": "The token is invalid, turned off or expired"}` instead of
  `{"error": {"code": "invalid_token", "message": "…"}}`. Status codes, the `code` values and the `WWW-Authenticate`
  and `Retry-After` headers are unchanged. Programs that check `error.code` must check `code`, and programs that show
  `error.message` must show `error`; a program that talks to sites on both versions can tell the two apart by whether
  `error` is a string or an object. The new `/data/v1` answers errors the same way. See "Responses" in
  [docs/api.md](https://github.com/gonolab/gono.forum/blob/v0.2.0/docs/api.md).

### Added

- **Skins.** A skin is a manifest, palettes over the core color scheme and an optional stylesheet, checked before it
  is used: nothing in it may make a request or hide the "Built on Gono Forum" line, and its colors are checked for
  contrast. Skins come from three sources: built into the binary (today only `default`, the standard look), imported
  as a pack at Appearance › Skins (`/manage/skins`) and kept in the database, or placed in the instance directory as
  `skins/<id>/`. That page chooses the site's skin and palette, lists every skin with its problems and warnings, and
  imports, deletes and rescans skins; a domain can choose its own skin on its page under Multiple domains.
  **Preview** shows a skin only to the administrator who started it, on every page, until they publish or exit.
  `gono-forum-cli skin check` and `gono-forum-cli skin pack` check a skin and pack it for import, without a database.
  See [docs/skins.md](https://github.com/gonolab/gono.forum/blob/v0.2.0/docs/skins.md); the two test skins in
  [tests/fixtures/skins/](https://github.com/gonolab/gono.forum/tree/v0.2.0/tests/fixtures/skins) show the format.
- **Seed colors.** Appearance › Colors (`/manage/appearance/colors`) takes a primary and a secondary color and derives
  the skin's colors from them in light and dark mode, raising text to 4.5:1 and field borders and the focus ring to
  3:1; the page lists the colors it changes and the contrast of every pair. Only the two seeds are stored, so they
  apply over any skin and are derived again when the skin changes. A domain follows the site's seeds, uses its skin's
  own colors or has seeds of its own. Both can be previewed before saving, a domain's on that domain's address. See
  "Seed colors" in [docs/styles.md](https://github.com/gonolab/gono.forum/blob/v0.2.0/docs/styles.md).
- **Layouts.** Appearance › Layout (`/manage/layout`) chooses the blocks shown in each place of a page: the asides of
  the home, board and thread pages, a page head on each of them, the footer and the side navigation. A block is
  either a data source in one of its views (hot or latest threads, boards, new members, ranks, honor groups,
  verified members, moderators, site statistics, friend links, a thread's outline), with a title and who sees it, or
  a core block such as the member card or a Markdown text. Changes go to a draft that the administrator can preview
  on every page before publishing it. A domain's layout (`/manage/layout?domain=<id>`) sets only the places it
  changes and follows the site in the others. The same page chooses whether the home page opens with the board
  directory. A skin can bring its own default layout; without one, pages show the blocks they showed in 0.1.3. See
  "Places and blocks" and "Editing a layout" in
  [docs/skins.md](https://github.com/gonolab/gono.forum/blob/v0.2.0/docs/skins.md).
- **Data sources and `/data/v1`.** The records behind those blocks are also in each page's data block
  (`#gonof-page`), and as JSON at `GET /data/v1/<source>`, for scripts on the site's own pages: read only, read as
  the visitor with their session, and same site only (no CORS headers; cross-site requests are refused). Three
  personal sources, `me.card`, `me.counts` and `me.checkin`, are only there. See
  [docs/data.md](https://github.com/gonolab/gono.forum/blob/v0.2.0/docs/data.md).
- **Board colors and glyphs.** A board's form (Forums › Forums) can give it a color and one of 32 built-in glyphs.
  The default look does not use them. Skins do, through `--board-color`, `--board-on-color` and `--board-glyph`, with
  the color adjusted to 3:1 against the grounds of the skin and seed colors in use; the board directory and the
  `grid` view of boards show the glyph. See "Board colors and glyphs" in
  [docs/skins.md](https://github.com/gonolab/gono.forum/blob/v0.2.0/docs/skins.md).
- **Page structure a skin can choose** (`[frame]` in its manifest): side navigation, a column at the start of the page
  that holds the `side.nav` blocks and folds into the header's drawer below 1100px; the page head as a full-width
  banner; and a home page that opens with the board directory above the thread list, listing each top-level board
  with its description, child boards, thread count, posts today and latest thread. The administrator can also choose
  the directory on the layout page. See "Frame" in
  [docs/skins.md](https://github.com/gonolab/gono.forum/blob/v0.2.0/docs/skins.md).
- **Behavior modules**, four built-in scripts a skin can turn on: `timeline` (a slider over the thread's floors, also
  usable by keyboard), `progress` (a reading progress bar and the floor being read), `autohide-header` (the header
  slides away while the reader scrolls down) and `docked-composer` (the reply box docked at the bottom of the
  window). They only enhance the page, and the admin area, sign-in and account security pages load none. See "Behavior
  modules" in [docs/skins.md](https://github.com/gonolab/gono.forum/blob/v0.2.0/docs/skins.md).
- **Thread views** are counted: once per visitor and thread every 30 minutes, leaving out `HEAD` requests,
  prefetches, requests without a `User-Agent` and crawlers. Members count by account; anyone else by a digest of the
  address, the `User-Agent` and a key that changes every day and is never written to the database or the logs.
  Counts are kept in memory and written once a minute, so they are approximate, and they add to the numbers sample
  and imported content came with. The default look shows them in thread lists, cross-forum pins and the thread head.
  See "Views" in [docs/queries.md](https://github.com/gonolab/gono.forum/blob/v0.2.0/docs/queries.md) and
  "View counting" in
  [docs/settings-and-caching.md](https://github.com/gonolab/gono.forum/blob/v0.2.0/docs/settings-and-caching.md).
- **Unread marks.** Thread lists mark threads with posts the reader has not seen: the default look draws a dot before
  the title, and screen readers hear "3 new replies", or "Unread" when the number is not known. The board directory
  marks boards whose latest thread is unread. What was read is kept only in the browser's local storage, per
  account, for up to 1,000 threads, and is never sent to the server: it works for visitors too, and devices do not
  share it. Everything before the first list or thread page a browser opens counts as read. The reading preferences
  (`/my/reading`) offer **Clear reading progress on this device**. Without JavaScript nothing is marked. See "Unread"
  in [docs/skins.md](https://github.com/gonolab/gono.forum/blob/v0.2.0/docs/skins.md).
- The built-in privacy policy, shown until a site saves its own at Global settings › Privacy policy, says that
  recently viewed threads and reading progress stay in the browser, separately for each account, are not sent to the
  site and are not cleared by signing out, and where to clear them.
- Public hooks for skins, personal styles and scripts: fixed class names and `data-*` attributes on list rows,
  `<time datetime>` on every shown time, counts split into a number and a word, optional elements that are hidden in
  the default look, the page data block, the script events `gonof:ready` and `gonof:swap`, and the header height as
  `--sticky-top`. They are a documented contract; see "The hooks" in
  [docs/skins.md](https://github.com/gonolab/gono.forum/blob/v0.2.0/docs/skins.md).
- The action log records skin imports, choices and deletes, color changes and published layouts.

### Changed

- Form field borders stand out more: `--input-border` is `#87909e` in light mode and `rgb(148 163 184 / 56%)` in dark
  mode, no longer `--line-strong`, so they reach 3:1 against their grounds. No other color of the default scheme
  changes.
- The category tag on a thread page has the same color as in the thread lists.
- The header's search button shows the focus ring of the other buttons.
- Personal styles are served in the `member` cascade layer, after the core and the skin: their rules now win over the
  core's whatever the selectors, and they can no longer change `[hidden]`, `.sr-only` or the "Built on Gono Forum"
  line, not even with `!important`. See "Cascade layers" in
  [docs/skins.md](https://github.com/gonolab/gono.forum/blob/v0.2.0/docs/skins.md).
- For translators: some counts are now two messages, a `…-label` message with the word and one that places the
  number and the word in the language's order; the names of built-in skins are in `skin.ftl`. See "Rules for
  messages" in [docs/translating.md](https://github.com/gonolab/gono.forum/blob/v0.2.0/docs/translating.md).

### Upgrading from 0.1.3

As with every upgrade during the alpha, back up the database and the instance directory first
([docs/install.md](https://github.com/gonolab/gono.forum/blob/v0.2.0/docs/install.md), "Files to back up"). Once you
put skins in the instance directory, back up its `skins/` too; imported skins are in the database. Member API
clients need the change under "Incompatible changes". No configuration changes are needed. A site that saved its own
privacy policy keeps it as it is, so it may want to add what the browser now keeps of reading (see "Unread marks").

Three database migrations run when 0.2.0 first starts:

- `0003_skins` creates `gonof_common_skin` for imported skins and adds the site's empty layout document to the
  settings (`settings.layout.*`). It takes no time.
- `0004_thread_last_reply_uid` adds `last_reply_uid`, with an index and a foreign key, to `gonof_forum_thread`, and
  fills it on every thread with the author of its last reply (left empty when that account is deleted). It takes
  time on large sites; see below.
- `0005_board_color_glyph` adds `color` and `glyph` to `gonof_forum_forum`, empty for every board. It takes no time.

The forum does not answer until the migrations are done. systemd's start timeout is extended meanwhile, and both the
one-click upgrade and `forum.install.sh upgrade` wait for the new version to start. A site with tens of thousands of
threads waits a few seconds. On a test machine, `0004` took about a minute per million threads: on MySQL 8.4 with its
default 128 MB buffer pool, about 25 seconds to rebuild the thread table (adding a foreign key copies the table) and
28 to fill the column; on SQLite, about 47 seconds. On a large site:

- Plan for that downtime, and keep free disk space of at least the size of the thread table: MySQL builds a copy of
  it, and SQLite writes every changed page to its WAL file in one transaction.
- Let the migration finish: do not stop the service or interrupt `forum.install.sh upgrade` (over SSH, run it in
  `tmux` or `screen`). On MySQL a migration cut short is left marked as failed, and neither version starts until the
  database is restored from the backup. On SQLite it is rolled back and runs again at the next start.

To go back to 0.1.3 after the migrations, restore the database from the backup as well: 0.1.3 refuses to start on a
database with migrations it does not know, so `forum.install.sh rollback` or the start guard alone is not enough.

`skin` is now a reserved address. A board whose address is `skin` keeps its page, but its public feeds
(`/skin/feed.xml`, `/skin/atom.xml`) are no longer served and its form will not save until the address is changed.

## 0.1.3 (early alpha)

The update page waits through the restart of a one-click upgrade, and the footer performance line is shorter.

### Fixed

- During a one-click upgrade the update page no longer stops on the reverse proxy's 502 page while the service
  restarts: it waits until the new version answers, then reloads. The waiting page is served by the version being
  replaced, so this takes effect for upgrades from 0.1.3 on; when upgrading from an earlier version, reload the page
  by hand if it shows a 502. Without JavaScript the page still reloads every 3 seconds.
- Reloading the update page after an upgrade no longer repeats "The upgrade has started".

### Changed

- The optional footer performance line (Appearance › Display) is shorter: a lightning icon, then
  "19.1 ms · MySQL 11 · Redis 23", with a tooltip saying what the figures are. Without Redis the Redis count is left
  out.
- The footer says "Built on Gono Forum" instead of "Powered by Gono Forum".

### Upgrading from 0.1.2

No database migrations. As with every upgrade during the alpha, back up the database and the instance directory
first ([docs/install.md](https://github.com/gonolab/gono.forum/blob/v0.1.3/docs/install.md), "Files to back up").

## 0.1.2 (early alpha)

Participation requirements can be left empty.

### Changed

- The participation requirements (Global settings › Login methods, and a domain's customized requirements) no longer
  need at least one item ticked. With none ticked, nothing is required: every member whose account is active and not
  frozen may start threads, reply, send private messages, like, downvote and tip, join activities, upload, create
  invite codes and turn on the API. The default is still a verified email, and existing settings are kept. See
  [docs/roles.md](https://github.com/gonolab/gono.forum/blob/v0.1.2/docs/roles.md).

### Upgrading from 0.1.1

No database migrations. As with every upgrade during the alpha, back up the database and the instance directory
first ([docs/install.md](https://github.com/gonolab/gono.forum/blob/v0.1.2/docs/install.md), "Files to back up").

## 0.1.1 (early alpha)

Sample content in Simplified Chinese.

### Added

- The setup wizard's optional sample content (10 members, 5 boards, 100 threads and 1,000 replies) is now offered in
  Simplified Chinese as well as English: choose the language under the sample checkbox. Every language has the same
  threads, authors and nesting; only the words differ. The choice does not set the interface language, which is set
  afterwards in the admin area.
- `gono-forum-cli seed-sample` and `gono-forum-cli export-sample` take `--language=<tag>` (English by default), and
  an exported dataset's `manifest.json` names its language.
- Translators can add sample content for their language in `apps/web/locales/<tag>/sample.json`; see "Sample
  content" in [docs/translating.md](https://github.com/gonolab/gono.forum/blob/v0.1.1/docs/translating.md).

### Changed

- `examples/content/` now holds one directory per language: `examples/content/en/` and `examples/content/zh-CN/`.

### Upgrading from 0.1.0

No database migrations. As with every upgrade during the alpha, back up the database and the instance directory
first ([docs/install.md](https://github.com/gonolab/gono.forum/blob/v0.1.1/docs/install.md), "Files to back up").

## 0.1.0 (early alpha)

The first public release of Gono Forum, a self-hosted discussion forum that runs as a single binary on MySQL or
SQLite. The [README](https://github.com/gonolab/gono.forum/blob/v0.1.0/README.md) lists the features, and
[docs/install.md](https://github.com/gonolab/gono.forum/blob/v0.1.0/docs/install.md) covers installing a site.

This is an early alpha: try it out, but expect configuration and behavior to change between releases.

**Upgrades are not guaranteed.** At the first release candidate the database migrations will be folded into a new
baseline, and a site installed from a 0.1.x release may not upgrade past that point and would have to be set up
again. Upgrades run database migrations automatically and take no backup: back up the database and the instance
directory before every upgrade.

### Tested

- The Rust test suite on SQLite, MySQL 5.7.44 and MySQL 8.4.
- `forum.install.sh` install, upgrade, rollback and start guard, including their failure paths, against a simulated
  systemd.
- The browser checks (layout, console errors, keyboard focus) in Chromium, on macOS only.

### Known limitations

- Packages are built only for Linux on x86_64 and arm64 with systemd; elsewhere, build from source.
- The arm64 package is cross-compiled and checked to be statically linked, but has not yet been run on real arm64
  hardware. A report of whether it works is welcome.
- Packages are not signed. `checksums.txt` comes from the same place as the packages, so the SHA-256 check catches
  broken downloads, not a compromised release source.

### Feedback

Please report bugs and ideas in [GitHub Issues](https://github.com/gonolab/gono.forum/issues/new/choose), and
security problems privately as described in
[SECURITY.md](https://github.com/gonolab/gono.forum/blob/v0.1.0/SECURITY.md).
