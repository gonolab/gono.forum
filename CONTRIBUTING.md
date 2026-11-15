# Contributing to Gono Forum

Thank you for helping. Bug reports, translations, documentation and code are all welcome.

- **Bugs and ideas**: open an issue with the templates provided. For anything larger than a small fix, please open an
  issue before writing code, so we can agree on the approach first.
- **Security problems**: do not open a public issue; see [SECURITY.md](SECURITY.md).
- **Translations**: see [docs/translating.md](docs/translating.md).
- Everyone taking part follows the [code of conduct](CODE_OF_CONDUCT.md).

## Licensing of contributions

Gono Forum is licensed under MIT OR Apache-2.0. By sending a pull request you agree that your contribution is
licensed under both, the same as the rest of the project. There is no CLA and no sign-off requirement. Only send
work you have the right to license this way; do not copy code, styles, icons or text from other projects unless
their license allows it, and say where it came from.

## Getting set up

Follow "Build from source" in the [README](README.md): Rust from `rust-toolchain.toml`, Node.js 22.13 or later,
`npm ci --ignore-scripts && npm run build`, a `config/development.toml`, and `cargo run --bin gono-forum-web`. SQLite
is enough for most work; use MySQL when you touch SQL that behaves differently there. An overview of the code is in
[docs/architecture.md](docs/architecture.md).

## Conventions

**Code**

- Run `cargo fmt` and keep `cargo clippy --all-targets -- -D warnings` clean.
- Comments, log messages, panic and assertion messages, and command-line output are in English.
- Text that people see in pages, mail or API errors is never written in code or templates: add a message to
  `apps/web/locales/en/` first, then the Simplified Chinese translation (see "For developers: adding interface text"
  in [docs/translating.md](docs/translating.md)).
- Files are UTF-8 with LF line endings and a final newline.
- When a change alters behaviour that a document in `docs/` describes (the member API in [docs/api.md](docs/api.md),
  roles and permissions in [docs/roles.md](docs/roles.md), …), update the document in the same pull request.

**Database**

- Go through the `db` module's types (`db::query`, `db::Tx`, `db::DbRow`); do not use SQLx's MySQL or SQLite types
  outside it.
- Write SQL for MySQL; it must also pass the SQLite rewriter (`db/dialect.rs`). Prefer the form that works on both
  as written: do truncation, string building and the current time in Rust, and split multi-table updates into a
  query for the keys and an update by key.
- Keep SQL simple: return plain columns and handle `NULL`, formatting and per-row decisions in Rust. Filtering,
  sorting and paging stay in SQL and stay bounded.
- Do not wait on the network, the disk or password hashing inside a write transaction (SQLite has one writer).
- No triggers, stored routines, events or `DEFINER`. Invariants are kept by code and checked by tests (see "Data" in
  [docs/architecture.md](docs/architecture.md)).
- **Schema changes need an issue first.** A change is a new migration appended after the existing ones, with one file
  in `db/migrations/mysql/` and one with the same name in `db/migrations/sqlite/`; `tests/sqlite_schema.rs` checks
  that both produce the same schema. During the 0.1.x alpha, migrations are appended in the same way and folded into
  a new baseline once, at the first release candidate (alpha sites are not guaranteed to upgrade past it; see
  "Status" in the [README](README.md)). From then on, released migrations are never edited.
- Before changing thread lists, thread pages or the columns and counters they copy, read
  [docs/queries.md](docs/queries.md); for settings reads and writes, [docs/settings-and-caching.md](docs/settings-and-caching.md).

**Caching**

- Register new caches in `apps/web/src/cache/registry.rs`. Anything per member has the uid in its key; results that
  embed an authentication decision are never cached. The two cache layers and their rules are in
  [docs/settings-and-caching.md](docs/settings-and-caching.md).

**Pages and styles**

- Every page must work without JavaScript; HTMX and scripts only enhance it.
- Follow the style guide in [docs/styles.md](docs/styles.md): the color tokens from
  `frontend/styles/themes/default.json`, the existing breakpoints (900, 760, 700, 640, 480 and 420 px) and logical
  properties (`margin-inline-start`, never `margin-left`).
- No external resources at run time: no CDNs, remote fonts, scripts, images or APIs. Vendored files go in
  `apps/web/static/` with their license in `docs/licenses/`, an entry in [NOTICE](NOTICE) and one in
  [docs/assets.md](docs/assets.md).

## Tests

Run the Rust suite on SQLite, and on MySQL when you change SQL (commands in the README's "Tests"). Tests create and
drop their own databases; they need a Redis 7 and, for MySQL, an account that may create and drop databases. A
single test binary runs with `--test <name>`, for example `--test i18n_leaks_http`. A test that checks a MySQL
mechanism only starts with `gono_forum_web::mysql_only!()`, which returns early on SQLite; anything else must pass on
both backends.

Continuous integration (`.github/workflows/ci.yml`) runs on every pull request and every push to `main`, and only
checks `cargo fmt --all --check` and `cargo clippy --locked --all-targets -- -D warnings`. The Rust suite is not run
there: run it yourself, on SQLite, and on MySQL 5.7.44 and 8.4 when you change SQL. Before a release all three run on
the commit to be tagged (see [docs/releasing.md](docs/releasing.md)). The browser checks below are not part of CI
either; run them yourself when you change pages or styles.

### Browser checks

`tests/e2e/` holds Playwright checks that start their own throwaway sites: two local instances (English and
Simplified Chinese) on SQLite in a temporary directory, with Redis off and random ports, taken through the setup wizard.
The English site gets the development fixture (`seed-demo`); the Chinese site ticks the wizard's sample content in
Simplified Chinese, so its pages carry Chinese titles, names and posts. They never connect to MySQL, Redis or any
remote host, and the temporary directories are removed afterwards.

```bash
cargo build -p gono-forum-web --bins     # the checks run target/debug/gono-forum-web and gono-forum-cli
npm run build
npx playwright install chromium          # once
npm run test:checks                      # no horizontal overflow, no console errors, visible focus, no Chinese on en pages
npm run test:screens                     # full-page screenshots into .local/visual/<label>/
npm run test:e2e                         # both
```

`checks` and `screens` cover 23 pages in four viewports (1440 and 390 px wide, light and dark).

For changes to styles there are two more tools; their output stays in `.local/` and is not committed:

- `GONOF_VISUAL_LABEL=before npm run test:screens`, make the change, `GONOF_VISUAL_LABEL=after npm run test:screens`,
  then `node scripts/visual-compare.mjs before after` writes `.local/visual/compare-before-after.html`, a side-by-side
  page for judging the change by eye.
- `node scripts/ui-measure.mjs measure <label> --fast` records the computed styles of every element on about 380
  pages (anonymous and administrator, wide and narrow, light and dark) into `.local/ui-measure/<label>/`, and
  `node scripts/ui-measure.mjs compare <before> <after>` lists what changed. A full run without `--fast` also forces
  hover, focus and active states and takes much longer.

The browser checks and both tools have so far only been run on macOS. If something fails on Linux or Windows,
please open an issue.

### Install script

After changing `forum.install.sh`, run `bash -n forum.install.sh`, `shellcheck forum.install.sh` and
`tests/install/simulate.sh`. The simulation runs install, upgrade, rollback and uninstall, including the upgrade's
failure paths (a rename that fails halfway, a version that never answers, a version put back that does not start,
an interrupted or killed run, a `--version` that hangs). It also runs the start guard after a one-click upgrade whose
new version keeps failing. It runs all of this against a fake systemd in a temporary directory, as an ordinary user,
and never touches `/opt`, `/etc` or the real service manager. It needs bash, curl, python3, tar and GNU coreutils (on
macOS `brew install coreutils`), and takes about two minutes.

## Commits and pull requests

- Keep a pull request to one change, with tests that cover it.
- Commit messages are in English and start with a type: `feat: …`, `fix: …`, `docs: …`, `refactor: …`, `test: …`.
- Describe what changed and why, how you tested it, and anything the reviewer should look at in the browser. The
  pull request template has a checklist.
