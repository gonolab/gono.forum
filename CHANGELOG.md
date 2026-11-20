# Changelog

What changed in each release of Gono Forum. A version's section is also the title and text of its GitHub release
(see "Publishing a release" in [docs/releasing.md](docs/releasing.md)), so links in a section are absolute: the
release page does not resolve relative ones.

## 0.3.0 (early alpha)

Three built-in skins, a retention period for site notifications, and close to two hundred fixes from a review of the
whole code base. Five database migrations run when it first starts; read "Upgrading from 0.2.0" before you upgrade.

### Added

- **Built-in skins Atrium, Harbor and Ledger**, which any site can choose at Appearance › Skins (`/manage/skins`).
  They are made only of what the skin layer offers (palettes, a stylesheet, the frame, behavior modules, a default
  layout, board colors and glyphs), so they also show what a skin can do:
  - `atrium`: a white page under a light grey header, the boards in side navigation, the board directory on the home
    page, flat topic rows with the board as a badge and replies and views as stat boxes. Palettes `main`, `forest`,
    `plum`, `teal` and `slate`.
  - `harbor`: side navigation with the new-thread button, the page head as a banner, topic rows with the board as a
    label in its color and a reply count column, a title band in the board's color over board and thread pages.
    Palettes `main`, `contrast` and `colored`.
  - `ledger`: flat white pages ruled into columns, topic lists as columns (title, poster, replies, views, activity),
    flat posts with a round avatar, the thread's timeline beside it. Palettes `blue`, `teal`, `indigo` and `contrast`.

  All three turn on the four behavior modules. See "Built-in skins" in
  [docs/skins.md](https://github.com/gonolab/gono.forum/blob/v0.3.0/docs/skins.md).
- **Notification retention**: two settings under Tools & maintenance › Maintenance, how long site notifications are
  kept once read (30 days by default) and while unread (180 days by default, never shorter than the read period). See
  "Changed" and "Notification retention" in
  [docs/queries.md](https://github.com/gonolab/gono.forum/blob/v0.3.0/docs/queries.md).
- Hooks for skins and scripts: the thread page's `article.thread` and the rows of the board directory (`/forums`)
  carry their board's `data-board`, so `/boards.css` colors reach them; the page data block has `now`, the server's
  time as it wrote the page; the replies section carries `data-last-position`, the thread's highest floor. See "Page
  data" in [docs/skins.md](https://github.com/gonolab/gono.forum/blob/v0.3.0/docs/skins.md).
- "Restore built-in text" on all six policy pages. Saving a policy with a blank title and body restores the built-in
  text too, and the edit form shows that text as placeholders.
- The member API sends `Retry-After` with a 429 for a posting quota (the seconds until the quota allows another post)
  and with a 503 from inside a write.
- `gono-forum-cli settle-due` prints how many campaigns it settled, how many are still paying out and how many
  failed, and exits with a failing status when any failed. The command-line tool now prints its warnings on standard
  error.

### Changed

- **Site notifications are deleted after their retention period** (see "Added"): read ones 30 days after they were
  read, unread ones 180 days after they arrived, by default. The cleanup runs every minute, 1,000 members at a time,
  and keeps the unread counts in step. The built-in privacy notice states both periods. The notification list stops
  at 100 pages, and the lists of a member's personal space at 1,000.
- **Cookie names follow the forum's path and port.** A forum under a path prefix or on a port other than the default
  one now names every cookie after them (`gonof_session_forum`, `__Host-gonof_session__port8443`), so several
  forums on one host no longer read and overwrite each other's sessions. A forum at the root on the default port keeps
  the plain names. See "Reverse proxy" in
  [docs/install.md](https://github.com/gonolab/gono.forum/blob/v0.3.0/docs/install.md).
- The action log shows administrators every entry. More than seventy kinds of entry the forum wrote but did not list
  (domains, friend links, honor groups, media, two-factor changes and others) now have their own labels.
- A site has at most 400 top-level boards, checked when a board is created or promoted and by the content import. A
  sub-site path on a host with a root site may not start with a board's address, and a new board address may not
  match such a path.
- Registration refuses an email address the mail library cannot send to (such as `first..last@example.test`),
  instead of accepting it and failing every mail to it.
- A new name for the site's points may not contain `|` or be a word the prize format reads as another prize type.
  A site that already uses such a name keeps it.
- Sanction, lift, expiry and appeal result mail carries the operator's reason, so it only goes to a verified address;
  the appeal link only to an account with a password or a verified address. The notification on the site is
  unchanged.
- Images with an ICC profile other than sRGB (Display P3, for instance) are converted to sRGB when they are
  re-encoded, thumbnails and animation frames included, instead of losing their colors with the profile. An animation
  written as a GIF keeps the source's loop count.
- A link to a whole thread (`/thread-N`) becomes a card; a link to one of its pages, a reading view or a reply stays
  the link it was written as. Plain text (excerpts, feeds, page descriptions) puts a space after each table cell.
- Likes, downvotes and favorites change their counters by one instead of counting the post's rows again, and the
  all-time leaderboards read their first places from per-member totals instead of every post in history.
- `gono-forum-cli set-password` checks the site's minimum password length instead of a fixed 12 characters.
  `import-profiles` refuses a list without `uid=1` or with usernames that differ only in case. Imported posts get a
  growth row without points; likes, highlights and accepted answers afterwards count as usual. See
  [docs/content-import.md](https://github.com/gonolab/gono.forum/blob/v0.3.0/docs/content-import.md).
- The daily upload quota is documented as a soft limit: an upload is admitted while its original size fits in what is
  left, and then counts with the larger of its original and processed size.
- `forum.install.sh` checks `/healthz` after an upgrade with wget or bash's `/dev/tcp` when curl is missing, instead
  of reporting success unchecked, and stops before it stops the service when it cannot read the listen address. It
  refuses to upgrade or roll back an instance without its unit or configuration, and `uninstall` removes nothing
  while the service still runs.
- The one-click upgrade keeps the current binaries as `*.previous` hard links before it renames the new ones in, so no
  binary is ever missing, and requires both new binaries to report the release. It refuses with 409 when a check run
  since found another release than the page showed. The admin page reports an upgrade whose command-line tool is not
  the new version, and one that a power cut stopped before the new version was in place, which the old version then
  gives up.

### Fixed

- **Accounts and sign-in**: read-only mode no longer holds back sign-in and phone verification codes; sends are
  counted one at a time, so concurrent sends cannot go past the interval or the daily limits, and a code is only
  valid once the provider took it. A third-party sign-in started in a newer tab is no longer cancelled by an older
  one. Members who cannot post yet are sent to the requirement they miss, not always to email verification.
  Registration forms record the versions of the terms they showed and ask again when the terms changed meanwhile.
- **Privacy and data requests**: the personal export carries the member's poll options, news sources and lottery
  conditions and prizes, and exports past 4 GiB no longer fail. Erasing a lottery's post erases its prizes and
  conditions too. Erasure walks a member's posts along an index instead of scanning the site, and long erasures take
  turns with other requests. Deleted authors' uids no longer show in lists.
- **Mail**: a mail draft is only sent for the event it was written for. Every retention cleanup step runs even when
  one fails, and temporary mail files left by an interrupted write are removed. Sanction mail is dated in the site's
  time zone.
- **Moderation and admin forms**: stale honor renewals, verification edits, friend link forms and layout editor
  forms are refused with 409 instead of overwriting newer changes. A moderator whose hide is in effect is recused
  from the restoration request. Viewing appeals no longer holds the database writer. Invite code filters match the
  status the list shows. Friend link applications refuse every address of the forum itself.
- **Boards**: overlapping board merges, and new child boards under a board being merged away, no longer strand
  threads in a disabled board. A child board moved to the top level keeps its parent's domain links. Moving a thread
  into an archive board returns to the move page; a refused move keeps the chosen board and reason.
- **Activities**: cards update after an in-page reply and only offer what the member can do; check-in days follow the
  site's current time zone; prize types can no longer be changed by the points name; large lotteries pay out over
  several passes; an early draw fixes its winners in the draw itself; a campaign plan resumes after the site language
  changed.
- **Posting and the editor**: drafts are saved only after a change and restored where the page chose the forum or a
  quote; Enter posts the thread; every board's posting guide shows without JavaScript; the code button picks a fence
  longer than any it holds; Escape in a menu no longer closes the docked reply box.
- **Attachments and storage**: checks that fail on the disk or run out of time are retried instead of rejecting the
  upload; the editor stops waiting on a paused check; trash and purge actions are decided by the current state; brand
  images and board icons answer 503 when storage is unavailable instead of 404; storage settings no longer add a
  profile on every save.
- **Reading pages**: the floor tracker follows the innermost nested floor; the timeline and progress modules follow
  replies posted in place and a timeline of another thread opens that thread; unread marks take their starting point
  from the server's clock; the page behind the phone drawer can no longer take focus; `lang` and `dir` follow a
  boosted navigation; a change to a behavior module alone gets a new asset address, and during a rolling release only
  the answering build's assets are cached for good.
- **Rendering and lists**: the text around images in raw HTML is kept; mentions, quotes and attachments shown as plain
  text past the nesting limit no longer notify or bind; guests see current link cards; title searches lower letters
  the way MySQL does on both backends; a feed reads its pinned and other threads in one snapshot; weekly statistics
  follow the site's local week; sitemaps list threads past id 250,000,000.
- **Private messages and notifications**: message forms accept what the server accepts and keep the recipient they
  were started for; a refused send cannot be delivered twice; reading a conversation no longer holds its lock while
  it reads; follows count toward the interaction limits; "Mark all as read" only shows where it marks something.
- **Imports and the command-line tool**: content imports run one at a time and count nested replies in one pass;
  imported replies are never dated after the import; `enable-api-keys` keeps the old tokens when writing the new ones
  fails; `import-content --members-out` checks its file first.
- **Member API**: an edit returns, and a replay returns again, the version that edit wrote; publishing a thread
  replays after its board's address or the site's time zone changed; a reused key on an ended activity is a 409.
- **Setup and health checks**: the setup wizard leaves a database that already has its administrator alone, and
  SQLite paths with `%`, `?` or `#` name the file as typed. `/healthz` answers on a host registered only with a
  sub-path, and in time while the database pool is full.
- **Performance**: the Outbox is indexed for its workers and for account deletion; reply counts, site statistics,
  the daily report quota and member lists no longer grow with the data.

### Upgrading from 0.2.0

As with every upgrade during the alpha, back up the database and the instance directory first
([docs/install.md](https://github.com/gonolab/gono.forum/blob/v0.3.0/docs/install.md), "Files to back up"). No
configuration changes are needed.

Five database migrations run when 0.3.0 first starts:

- `0006_friend_link_version` and `0007_post_request_version` add a version column to friend links and to the request
  records of posts. That takes no time on MySQL 8.4 and SQLite. MySQL 5.7 rebuilds both tables in place; the request
  records are those of the last 30 days by default (Tools & maintenance › Maintenance, "Request key retention").
- `0008_notification_retention` adds the two retention settings.
- `0009_rank_member` creates `gonof_rank_member`, fills it from the posts and indexes the growth accounts. It takes
  time in proportion to the posts: 2.6 to 3.4 seconds for 932,972 posts on MySQL 8.4 on a test machine.
- `0010_outbox_indexes` adds two indexes to `gonof_common_outbox`. On MySQL they are built in place, but the build
  waits for transactions already open on the table: do not upgrade while one is (a `mysqldump --single-transaction`
  still running, for instance). 3 million rows took 2.9 seconds on MySQL 8.4 and 4.6 on 5.7.

The forum does not answer until the migrations are done; both the one-click upgrade and `forum.install.sh upgrade`
wait for them. To go back to 0.2.0 afterwards, restore the database from the backup, and empty `mail/prepared/` in
`[identity] local_directory`: 0.2.0 refuses the mail drafts 0.3.0 writes.

Also check before you upgrade:

- **Notifications.** About a minute after the start, the cleanup begins deleting read notifications older than 30 days
  and unread ones older than 180 days. To keep them longer, set both periods under Tools & maintenance › Maintenance
  right after the upgrade; what is deleted before that is gone. A site that saved its own privacy policy keeps it as
  it is, so it may want to state the periods.
- **Sign-in.** On a forum under a path prefix or on a port other than the default one, the cookie names change: every
  member is signed out once and pages left open must be reloaded before they can post.
- **Skins with a built-in id.** An imported skin or a skin in the instance directory called `atrium`, `harbor` or
  `ledger` is now shadowed by the built-in skin of that id, and a site that chose it shows the built-in one. The test
  skins in `tests/fixtures/skins/` of 0.2.0 had two of these ids; they are now `frame` and `ruled`. Before upgrading,
  give such a skin another id in its manifest, import it again (or rescan the instance directory) and choose it.
- **Rendered posts** are cached under a new key, so posts are rendered again on their first view after the upgrade.
- **The upgrade itself is run by 0.2.0**, so this release's fixes to it take effect from the next one. If a one-click
  upgrade from 0.2.0 is cut short (power lost) before the new web binary is in place, 0.2.0 keeps running with
  `data/upgrade/pending`: remove that file before the service has started twice more, then upgrade again.

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
