# Architecture

An overview for people reading or changing the code. It describes how things are, not a plan. The reference documents
in `docs/` go into more detail:

- [install.md](install.md): installing, configuring, upgrading and backing up a site from the release packages.
- [content-import.md](content-import.md): the content dataset format and the commands that check and import it.
- [translating.md](translating.md): adding or updating a language, and adding interface text in code.
- [settings-and-caching.md](settings-and-caching.md): how site settings are stored and read, and the process and
  Redis caches.
- [queries.md](queries.md): how thread lists and thread pages are queried, with the copy columns and count tables
  that keep them cheap.
- [api.md](api.md): the member REST API under `/api/v1/`.
- [data.md](data.md): the data sources that blocks, the page data block and the read-only JSON under `/data/v1/`
  share.
- [roles.md](roles.md): staff roles, mutes, level gates and honor groups.
- [styles.md](styles.md): the style guide for stylesheets, class names and templates, and how to check changes.
- [skins.md](skins.md): skins (format, sources, checks, cascade layers, choosing and previewing one) and the markup
  hooks stylesheets and scripts can rely on: list row classes and states, split counts, `<time>`, optional elements,
  the page data block, script events and the declared header height.
- [assets.md](assets.md): where bundled fonts, icons, images and scripts come from, and their licences.
- [releasing.md](releasing.md): building and publishing release packages, and how sites upgrade themselves.

## Shape of the code

Gono Forum is a modular monolith in one Rust package, `apps/web` (`gono-forum-web`):

```text
apps/web/
  src/lib.rs              the library: everything the two binaries and the tests share
  src/main.rs             gono-forum-web: the web server and its background workers
  src/bin/gono-forum-cli.rs  migrations and maintenance commands
  src/router.rs, router/  routes and request handlers, grouped by area
  src/<area>.rs, <area>/  features: identity, content, interaction, notifications, messages, media,
                          moderation, organization, administration, privacy, activities, growth, …
  src/db/                 database access for both backends, migrations, SQL rewriting for SQLite
  src/cache/              process and Redis caches, with a registry of every cache
  src/i18n.rs, i18n/      translations, number and date formatting
  templates/              Askama templates (compiled into the binary)
  frontend/styles/        stylesheet sources, built into static/css/ by npm run build
  skins/                  built-in skins (compiled into the binary)
  static/                 scripts, fonts, images and the built stylesheets
  locales/<language>/     Fluent translation files and the language's sample content
  tests/                  integration tests
db/migrations/{mysql,sqlite}/  one baseline plus appended migrations, one file per backend
config/                   default.toml (compiled in) and examples for local configuration
tests/e2e/                browser checks against throwaway local sites (Playwright)
tests/install/            simulated runs of forum.install.sh against a fake systemd
scripts/                  build, packaging and measuring tools
```

Modules are split by feature and by size, not by layer: a feature keeps its queries, transactions and rules together
and grows into a directory when it needs to. Shared plumbing (configuration, database, cache, mail, Outbox, rate
limits) has its own modules.

## Requests

1. **Entry layer.** Before routing, the request's site is resolved from its `Host` and path (`site.rs`): which
   domain it is on, the boards that domain shows, its language, and the path prefix when the forum is installed
   under a sub-path, which is stripped here. Addresses outside a domain's boards are redirected to the canonical
   domain.
2. **Authentication.** The session cookie is encrypted and authenticated with the cookie key. Each request looks
   the member up by uid only and compares the credentials version and the frozen flag in code; a mismatch drops the
   session. Authentication finishes before any personal data is read or any protected action runs.
3. **Handlers** in `router/` read through the feature modules and render a template. Pages share `PageMeta` (`p`
   in templates), which carries the request's language as `p.t`.
4. **Writes** check permissions again inside the transaction, use conditional updates and version checks for
   concurrency, and are protected by CSRF tokens and rate limits.

Every page is complete server-rendered HTML. HTMX swaps fragments for replies, reactions, previews and paging, and
keeps the address bar in sync; without JavaScript the same forms post normally and redirect.

## Addresses

| Page | First page | Page N | Notes |
| --- | --- | --- | --- |
| Home | `/` | `/page-N` | `/page-1` redirects to `/` |
| Board | `/<slug>` | `/<slug>-N` | `/<slug>-1` redirects to `/<slug>` |
| Thread | `/thread-<id>` | `/thread-<id>-N` | a reply is `#floor-<position>` on its page |
| Member | `/space/<uid>` | | addressed by uid, so renaming keeps the address |
| Member's threads and replies | `/space/<uid>/threads`, `/space/<uid>/posts` | `…-N` | |

Other entry points have no page number: `/new` and `/new/<slug>` (new thread), `/edit/thread-<id>`, `/login`,
`/register`, `/logout`, `/my/…` (the member's own area: notifications, messages, settings, data), `/search`,
`/forums`, `/terms`, `/privacy`, `/rules`, `/contact`, `/feed.xml` and `/atom.xml` (also per board, and private
per-member feeds), `/sitemap.xml`, `/robots.txt`, `/manage/…` (admin area) and `/healthz`.

Endpoints used by HTMX return HTML fragments, for example:

| Action | Request | Returns |
| --- | --- | --- |
| Reply to the thread | `POST /api/thread-<id>/comments` | the new top-level reply; the reply count in the thread header and the activity card come along out of band, so a red packet claim or lottery entry made by the reply shows at once |
| Reply to a reply | `POST /api/posts/<root pid>/sub-comments` | the new nested reply |
| Like a reply | `POST /api/comments/<pid>/vote` | the updated reaction control |
| Favorite a thread | `POST /api/thread-<id>/star` | the updated favorite control |
| Preview | `POST /api/preview` | the rendered Markdown |

A reply comes back as a fragment only when the thread page's reply box sent it (its form carries the header
`X-Composer-Context: thread`) and it lands on the page the reader is on; otherwise, as from the editor page a failed
reply leaves in place of the thread, the answer is an `HX-Redirect` to the reply, with `HX-Refresh` when the browser
is already at that page's address (`HX-Current-URL`), so that it reloads the page instead of only moving to the reply.

The member API under `/api/v1/` (threads, replies, likes, check-in and so on) is JSON with personal bearer tokens;
see [api.md](api.md). The data sources ([data.md](data.md)) are read-only views of forum data shared by a layout's
blocks, the page data block and `GET /data/v1/<source>`, which answers the session's own view as JSON to the site's
own pages.

## Data

- **Two backends, one SQL.** All access goes through the `db` module's own types (`db::query`, `db::Tx`,
  `db::DbRow`), which dispatch on the connection URL. SQL is written for MySQL; on SQLite it passes through a
  rewriter (`db/dialect.rs`) with a fixed set of rules (locking reads and index hints dropped, `GREATEST` → `max`,
  upserts → `ON CONFLICT`, …) that refuses anything it does not know. A unit test prepares every static statement on
  SQLite, so a statement that only works on MySQL fails the build's tests. SQLite has a read pool and a single
  writer; write transactions start with `BEGIN IMMEDIATE` and retry on lock conflicts, so they must not wait on the
  network, the disk or password hashing.
- **Schema.** The schema starts from one baseline, `0001_baseline.sql`. Later changes are appended as `0002_…`,
  `0003_…`, each a pair of files with the same name in `db/migrations/mysql/` and `db/migrations/sqlite/`, registered
  in `db/migrations.rs`; a unit test checks the two directories against the registry, and `tests/sqlite_schema.rs`
  compares the resulting schemas table by table. SQLx checksums every migration, so a released one is never edited;
  the one exception is planned for the first release candidate, which folds the alpha migrations (0.1.x and 0.2.x)
  into a new baseline (sites installed from an alpha are not guaranteed to upgrade past it).
  Tables are prefixed `gonof_`. Members, boards, threads and posts keep short, stable column names (`uid`, `fid`,
  `tid`, `pid`, `first`, `position`). The opening post is the thread's first post; a reply's `root_pid` points at the
  top-level reply it belongs to and `reply_to_pid` at the post it answers, with `root_position` and `sub_position`
  numbering the two levels.
- **No triggers, routines or events.** The schema has no triggers, stored routines, events or `DEFINER` clauses, so
  migrations need no privileges beyond the forum's own database. Unit tests in `db/migrations.rs` reject any
  migration that contains them (`DROP TRIGGER` included), and `tests/schema_triggers.rs` checks that a fully
  migrated database has no triggers. Rules a trigger could have enforced are kept by code. Values are checked at the
  write entry points (`identity/rules.rs`, `messages/rules.rs`, `activities/invariants.rs`, `media::check_new_upload`,
  `notifications/record.rs`). Rows that must not change, only grow or never be deleted (the points ledger, logs,
  posts, sanctions and appeals, used tokens) are protected by conditional updates such as `WHERE consumed_at IS NULL`
  and by `db/source_guards.rs`, a unit test that scans the source for statements writing those tables or columns
  outside the module that owns them. On MySQL, the SQL checks in `apps/web/tests/consistency/` look for rows that
  break the rules after tests have written through the real code paths.
- **List copies.** Thread rows carry what lists need (author and last poster names, top-level board), and counters
  (replies, reactions, favorites, tips) are kept on the rows they count, all updated in the same transaction as the
  data they copy. The view count is the exception: counted in memory and added once a minute, approximate. Lists
  filter, sort and page in SQL against narrow indexes and stay bounded (at most 1,000 pages); see
  [queries.md](queries.md).
- **Simple SQL.** Queries return plain columns; turning `NULL` into a default, formatting and per-row decisions
  happen in Rust. Filtering, sorting and paging stay in SQL.

## Side effects and background work

Mail, notifications, media processing and data requests are written as Outbox events in the same transaction as the
change that causes them, and consumed by workers inside the web process. Workers claim events with a lease, retry
transient failures with backoff and give up after 20 attempts (an administrator can schedule another try). Posting
and other writes carry request keys, so a retried request returns the original result instead of writing twice.
Thread views are not events: each process buffers the views it counts and writes them once a minute and when it shuts
down ([queries.md](queries.md), "Views").

Uploads land in a private staging directory first, are checked and re-encoded in a time-limited child process
outside any transaction, and are stored in a private S3-compatible bucket. Images are served through the forum
(`/media/<aid>`, `/thumb/<tid>/<aid>`), which checks permissions on every request.

Re-encoding drops metadata, the ICC profile included, and browsers read an image without a profile as sRGB. An image
with an RGB profile other than sRGB (Display P3 and the like) is therefore converted to sRGB first, with moxcms; the
list thumbnail is scaled from the converted image, and every frame of an animation is converted the same way. A
profile that does not parse, or one moxcms cannot convert from, leaves the pixels as they are. Animated WebP and APNG
become GIFs; the output keeps the source's loop count (a GIF without a loop extension still plays once).

## Caches

There are two layers, registered in `cache/registry.rs`:

- **Process cache**: site-wide data with a fixed number of entries (settings, policies, branding, boards, domains,
  moderators, pins, links, skins). Every process keeps a copy for a configurable time. Site statistics also start here and,
  with Redis on, are shared through Redis.
- **Redis cache**: anonymous page content (thread lists, sidebars, rendered posts, feeds) and per-member data, only
  when Redis is enabled; with Redis off these reads go to the database.

A save that changes site-wide data clears the affected entries in its own process after commit and, with Redis on,
publishes the invalidation to the other processes. When Redis is enabled it is a hard dependency: a failed Redis
call turns the request into a 503 rather than serving stale or uncached data, and rate limits are shared through it.
Caches only hold data that can be rebuilt; anything per member has the uid in its key, and results that embed an
authentication decision are never cached. Settings and caches are described in
[settings-and-caching.md](settings-and-caching.md).

## Languages

Interface text is in Fluent files, `apps/web/locales/<tag>/*.ftl`, compiled into the binary; nothing is read from disk
at run time. English (`en`) is the base language and the only one that must be complete. How to add text or a
language is in [translating.md](translating.md).

- **Generated methods.** `build.rs` parses the English files and generates one method on `i18n::Lang` per message
  and attribute: `frame-more` becomes `frame_more()`, `frame-search.placeholder` becomes
  `frame_search_placeholder()`. Variables become arguments in the order they first appear in the English text. A
  message without variables returns `&'static str` (formatted once per language and kept for the life of the
  process, so labels and error payloads keep their `&'static str` types), a message with variables returns `String`,
  and a message with a `*_html` variable takes `Markup` for it and returns `Markup`. A misspelt key or a wrong number
  of arguments does not compile; an English file that does not parse, a repeated key or two keys that make the same
  method name fail the build.
- **Lookup.** Each language is a Fluent bundle, loaded once per process while the forum starts; a translation file
  that does not parse, or a key repeated across a language's files, stops it. A message is looked up in the
  request's language and then in English, so there is always text. Fluent's bidi isolation marks are off; the page's
  `dir` handles direction.
- **Markup.** Translations contain neither `<` nor private-use characters. For a `*_html` variable the formatter
  inserts a private-use placeholder, HTML-escapes the whole sentence, then puts the caller's `Markup` in its place.
  `Markup` is built only by code (`PageMeta::link`, `PageMeta::link_with`, `Markup::tag`), and templates print the
  result with `|safe`.
- **Which language.** The site setting `settings.site.language`, or for another domain the `language` column of its
  row in `gonof_common_site_domain` (`NULL` follows the site; the main domain always does). The entry layer resolves
  it with the rest of the site, `i18n::current()` returns it, and templates have it as `p.t`. There is no
  `Accept-Language`, no choice per member and no language prefix in addresses. Outside a request `current()` is
  English; background work passes the site language explicitly (`SiteInfo::lang()`). The admin page
  `/manage/languages` sets the site language and shows each compiled-in language's completion, the share of English
  keys it translates.
- **Numbers and dates.** ICU4X with its full built-in data formats numbers (including numbers in messages, through
  the bundle's formatter) and dates, so a new language needs no data files. `meta-number-grouping` in each language's
  `meta.ftl` is `auto` (CLDR grouping, English `1,234`) or `never` (Simplified Chinese `1234`). The date style
  `locale`, the default on a new site, writes dates the language's way; the fixed styles `ymd-dash`, `ymd-cn` and
  `mdy-slash` are the same in every language. Relative times ("3 hours ago", "yesterday") are messages in `time.ftl`.
  Time zone labels (`UTC+8`) and `datetime-local` fields do not change with the language.
- **Page attributes.** `<html lang>` is the language tag, `dir="rtl"` is added only for right-to-left languages
  (ICU4X directionality), and `og:locale` is the tag with its likely region (`en_US`, `zh_CN`).
- **Browser scripts.** Keys starting with `js-` live in `js.ftl`. At start the server builds one script per language
  from them (missing keys taken from English) and serves it at `/static/i18n/<tag>.js?v=<asset version>`, loaded
  before `site.js`. It defines `window.gonofT(key, args)`, which fills `{ $var }` and picks plural variants with
  `Intl.PluralRules`. So that no Fluent runtime is needed in the browser, `js.ftl` messages are limited to plain
  text, `{ $var }`, or one select on a variable over such variants.
- **Brand words.** The site name, header brand text, the name of points and the name of the activity index
  (`settings.site.name`, `brand_text`, `credit_name`, `index_name`) may be left empty. An empty site name or brand
  text shows "Gono Forum" in every language; an empty points name, index name, site description, read-only notice or
  level name shows the language's default from `brand.ftl` (English "Points", "Activity Index"). The database keeps
  the empty string: the page frame fills its own copy of the settings (`SiteSettings::fill_defaults`), other code
  uses `SiteInfo::display_name`, `credit_word`, `index_word` and the like, and admin forms show the empty field with
  the default as placeholder. Messages take these words as ordinary variables (`{ $site }`, `{ $credit }`,
  `{ $index }`) that the caller passes (`p.credit()`, `p.index_word()`). The name of points also stands for the credit
  type in the lottery prize format (`name | winners | type | points each | note`), so a new name cannot contain `|` or
  be a word used there for another prize type (`code`, `item`, or a language's words for them); the prize editor
  writes the identifiers `credit`, `code` and `item` whatever the site calls its points.
- **Sample content.** The setup wizard's optional sample content and `gono-forum-cli seed-sample` /
  `export-sample` read `locales/<tag>/sample.json`, compiled in by `build.rs` like the translations. The English
  file is complete; another language's file is a translation of it, matched by member name, board slug and position,
  so every language gets the same threads, authors, times and nesting (`content::sample`). A language without the
  file offers no sample. The sample's language is chosen on its own and does not set the site language.
- **Mail and policies.** Built-in mail templates and policy pages are messages (`mail.ftl`, `policy.ftl`). An
  administrator's own version is a row per kind and language in `gonof_common_localized_text` (primary key `kind`,
  `language`); a row means customized. Restoring the built-in text deletes it, and so does saving a policy with a
  blank title and body; a row with both blank reads as the built-in text, which the policy edit forms show as
  placeholders. The admin edit pages take `?lang=`, and their forms always submit it, so a form opened in one
  language saves into that language even if the site language changed meanwhile. Mail a member starts with a link (email verification, password reset, email change, appeal and data
  access) is written in the language of the domain the request came from; all other mail is in the site language.
  Policy pages follow the request's language; the process cache holds one entry per policy kind with all its
  languages.
- **Caches.** `clock::TimeFormat` includes the language, and caches whose values hold formatted times or interface
  text (thread lists, floors, rendered posts, feeds) put it in their keys, so each language gets its own copies. A
  site only uses its own language and those of its domains.
- **Header navigation.** Which board links fit in the header is decided on the server (`router/nav_fit.rs`) by
  estimating text widths from character classes (full-width, emoji, narrow, wide and other ASCII), never from the
  language. The width budgets were measured on one fixed header (`Header::MEASURED`); a page whose brand text,
  sign-in buttons or "More" label differ moves its budget by the estimated difference.
- **Errors.** Text a person can see is a message: pages, form errors, banners, HTMX fragments, the setup wizard, the
  admin area and the `error` field of the member API (its `code` stays a fixed identifier). Text nobody sees is plain
  English in the code: logs, internal errors behind the generic error page, invariant failures, database and I/O
  context, command-line output and delivery failure reasons stored in `last_error`.
- **Stored system text.** Text the system writes to the database for later display is stored as a kind or a code
  and worded when shown. Points history rows written by the system are shown by their `kind` (an administrator's
  adjustment keeps the reason they wrote); governance notifications and sanction mail store only the reason a person
  wrote, and their opening words come from the event when shown or delivered; attachment checks store a fixed code
  in `scan_detail`; the name stored for a deleted author is shown as "Deleted user" in the page's language.
- **Checks.** Unit tests in `i18n` check every translation file (see translating.md, "Checking a translation").
  `tests/i18n_leaks_http.rs` runs an English site with English-only content and fails on any Han character in the
  pages it visits: public and member pages, the admin area for every staff role, the debug panel and built-in mail.

## Styles and scripts

Stylesheets are plain CSS in `frontend/styles/`, combined by `npm run build` (esbuild) into `forum.css` in cascade
layers: `guards` (rules nothing may override), `core`, then `skin` and `member` for the skin's and a member's
stylesheets. Colors come from one set of design tokens (`frontend/styles/themes/default.json`) with light, dark and
system themes; the server writes them, merged with the skin's palettes (and derived from two seed colors when the
administrator set them), into the skin's stylesheet. Layout uses
logical properties (`margin-inline-start`, not `margin-left`) so that right-to-left languages mirror correctly.
Scripts are small, framework-free and optional: htmx plus a few enhancements and the editor, and the behavior
modules a skin may turn on (`static/js/behaviors/`, [skins.md](skins.md), "Behavior modules"), which also only
enhance the page.

**Skins** ([skins.md](skins.md)) change the look without a build: a manifest, palettes over the core scheme and a
checked stylesheet. They come from the binary (`apps/web/skins/`), from the database (imported at `/manage/skins`)
and from the instance directory (`$GONOF_HOME/skins/`), in that priority; the process cache holds them checked and
compiled (`skins.rs`, `skins/`). Every skin is checked by the stylesheet checker members' personal styles also use
(`css_check.rs`): safety rules and the "Built on Gono Forum" guard block every skin, the rest of the style guide
blocks built-in skins and warns for the others. The site's choice and each domain's are layout documents in the
settings table, read with the settings; a choice that cannot be used falls back to the built-in default.

The templates also write hooks for stylesheets and scripts that are not part of the default look, listed in
[skins.md](skins.md):

- **Page data block.** `layout/base.html` writes `<script type="application/json" id="gonof-page">` into every page's
  head (`router/page_data.rs`): the kind of page, its ids and page number, on thread pages the floors with their
  positions, pids and times and the floor the page counts as read up to, the site's name, language, direction, path
  prefix, points and index names and date style, the records of the page's data-source blocks, and on a board
  directory home its boards' latest threads. It carries nothing personal, and its JSON is escaped like the JSON-LD
  blocks.
- **Places and blocks.** The asides, page heads and footer of the home, board and thread pages are places that show
  blocks: data sources with a view, or core cards (`layout.rs`, `router/blocks.rs`, `templates/blocks/`). The domain's
  layout document, the site's, the skin's manifest and the core default layout decide them place by place; the core
  default renders the pages as they were before layouts.
- **Frame.** A skin's manifest also chooses the page frame ([skins.md](skins.md), "Frame"): side navigation from the
  `side.nav` place on every front page, the page head as a banner, the board directory home page (which layout
  documents may choose too) and built-in behavior modules; `<html>` carries the choices as `data-*` attributes.
- **Script events.** `site.js` dispatches `gonof:ready` on `document` when the page has loaded and `gonof:swap` after
  every HTMX swap has settled.
- **Unread.** `site.js` keeps what the reader read in the browser's `localStorage`, per account and never on the
  server, and marks unread topic rows and directory boards `is-unread` from the rows' `data-maxposition` and last
  activity ([skins.md](skins.md), "Unread").
- **Declared header height.** `--sticky-top` is the height the sticky header covers, declared in CSS; scroll margins,
  sticky asides, the thread page's reading-position tracker (`site.js`) and list scroll restoration (`navigation.js`)
  all use it rather than measuring the header.

## Tests

- Unit tests sit next to the code. Integration tests in `apps/web/tests/` start the application in-process against a
  fresh database per test (`TestFixture`), on MySQL by default or on SQLite with `GONOF_TEST_BACKEND=sqlite`, and
  drop it afterwards. The test configuration (`Config::load_test`) reads only `config/test.toml`, which is optional,
  and the `GONOF_TEST_*` variables; on SQLite it needs no database URL, and empty cookie and two-factor keys get random
  ones for the run. Cases that check a MySQL mechanism only begin with `mysql_only!()` and return early on SQLite.
- `tests/e2e/` starts throwaway local sites and checks pages in a browser (see `CONTRIBUTING.md`).
- `tests/install/simulate.sh` runs `forum.install.sh` and its upgrade failure paths in a temporary directory, with
  fakes for systemd and the system commands (see `CONTRIBUTING.md`).
- `.github/workflows/ci.yml` runs formatting and Clippy for pull requests and pushes to `main`; the release workflow
  calls it before building packages. The Rust suite and the browser checks run on a developer machine, not in CI
  (see `CONTRIBUTING.md`, "Tests").

Installation and operation are covered in [install.md](install.md).
