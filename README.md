# Gono Forum

Gono Forum is a self-hosted discussion forum written in Rust. Pages are rendered on the server with
[Axum](https://github.com/tokio-rs/axum) and [Askama](https://github.com/askama-rs/askama), and
[HTMX](https://htmx.org/) only enhances them: reading, paging, signing in, posting and replying all work with
JavaScript turned off. It runs as a single binary on MySQL or SQLite, with Redis as an option for sites that run
more than one process.

Website: <https://www.gono.forum>

## Features

- **Boards and threads**: boards up to two levels deep, pinned and featured threads, categories, title search, and
  replies in two levels (a reply and the replies under it) or as a flat list.
- **Accounts**: open, invite-only or closed registration; email verification and password reset; optional
  authenticator (TOTP) two-step verification with recovery codes; sign-in with Google, GitHub or an SMS code.
- **Members**: profiles, following and blocking, private messages, in-site and email notifications with per-kind
  preferences, favorites, reading preferences, personal stylesheets, data export and account deletion.
- **Content**: Markdown with sanitizing, image attachments in S3-compatible storage with thumbnails, mentions and
  quotes, polls, bounties, red packets and lotteries, tips, points, growth levels, leaderboards and daily check-in.
- **Moderation**: reports, a review queue, mutes and account sanctions with appeals, content restoration requests,
  anti-spam rules, and the roles administrator, global moderator, moderator, trainee moderator and reviewer.
- **Site**: RSS and Atom feeds (site-wide, per board and personal), sitemap, robots.txt and structured data; several
  domains in one site, each with its own boards and language; installs under a sub-path such as `/forum`.
- **Member API**: personal API tokens for posting, replying and reacting from scripts
  ([docs/api.md](docs/api.md)).
- **Administration**: everything after installation is set in the admin area at `/manage`, including one-click
  upgrades.
- **Translations**: English is the base language and Simplified Chinese is the first translation; adding a
  language is a pull request of translation files ([docs/translating.md](docs/translating.md)).

## Status

Gono Forum is at 0.1.x, an early release: configuration and behavior can still change between releases. Upgrades
run database migrations automatically and do not take a backup first: back up the database and the instance
directory before every upgrade.

## Install

Release packages are static binaries for Linux on x86_64 and arm64 with systemd. On the server:

```bash
curl -fsSL https://gono.dev/forum.install.sh | sudo bash -s -- install
```

The script creates the instance `/opt/gono-forum-main/`, a system account and a hardened systemd service listening
on `127.0.0.1:58833`, and prints a one-time setup token. Put a reverse proxy with HTTPS in front of that port, open
the site and finish the setup wizard: pick SQLite or MySQL, optionally Redis, create the administrator and, if you
like, fill the forum with sample content to look around first.

[docs/install.md](docs/install.md) covers the reverse proxy, the files to back up, upgrades, rollback, several
instances on one machine and uninstalling.

## Requirements

- **Database**: MySQL 5.7.44 or later (InnoDB, `utf8mb4`), or SQLite. SQLite is built in (no system library
  needed) and suits a site run by a single process.
- **Redis** 7.0 or later, only when the site runs more than one process (see below).
- **Object storage**: image attachments need an S3-compatible private bucket, configured in the admin area. Without
  it the forum works but uploads are turned off.
- **Mail**: an SMTP account for verification and notification mail. Sending is off on a new site and is turned on
  in the admin area; without SMTP, mail is written to files in the instance's data directory.

## Build from source

You need Rust (the version pinned in `rust-toolchain.toml`; rustup installs it) and Node.js 22.13 or later with
npm.

```bash
npm ci --ignore-scripts
npm run build                      # stylesheets and the editor script
cp config/development.toml.example config/development.toml
cargo run --bin gono-forum-web
```

With an empty `database.url` the first start shows the setup wizard on <http://127.0.0.1:58833>; the setup token is
printed in the terminal and written to `.local/setup-token`. After setup the same port serves the forum. Source
builds serve static files from disk, so style changes only need `npm run build`; templates are compiled in and need
a rebuild. `cargo run --bin gono-forum-cli` lists the maintenance commands; `gono-forum-cli seed-demo` fills an
empty database with six demo boards and sample threads.

Release packages are built with `scripts/package-release.sh` on Linux (it needs the two musl targets, zig,
`cargo-zigbuild` and `cargo-about`), with the static files compiled into the binary; see
[docs/releasing.md](docs/releasing.md).

## Configuration

There are two layers:

- **The deployment file** holds only what the process needs before it can read the database: listen address,
  database and Redis connections, the local data directory, three secret keys and media limits. Release packages
  read `config/gono-forum.toml` in the instance directory, written by the setup wizard; source builds read
  `config/development.toml`. `GONOF_HOME` sets the instance directory and `GONOF_CONFIG` another file;
  `GONOF_DATABASE_URL` and `GONOF_REDIS_URL` override the connections. Unknown keys stop the start. The defaults
  with comments are in [config/default.toml](config/default.toml).
- **Everything else** (site name, forum URL, registration, mail, storage, sign-in providers, limits, caches,
  language) is set in the admin area and stored in the database.

The three keys (`[identity.cookie]`, `[identity.two_factor]` and `[common] encryption_key`) must be kept with the
database backup. Losing the two-factor key locks members who use an authenticator out of it, and losing the common
key means re-entering the SMTP and SMS secrets.

## MySQL or SQLite

The database is chosen by the scheme of `database.url`: `mysql://…` or `sqlite:///absolute/path.db`. Both run the
same code and the same test suite; queries are written for MySQL and rewritten for SQLite by a small set of fixed
rules. SQLite allows one writer at a time, so it fits small and medium sites served by a single process. The
setup wizard creates the SQLite file; outside the wizard only `gono-forum-cli migrate` creates it.

## Redis

`[redis] enabled` follows how the site is deployed:

- **One process**: turn it off. Caches and rate limits are kept in process memory.
- **Several processes or machines**: turn it on. Shared caches, rate limits and cache invalidation then go through
  Redis, and Redis becomes a hard dependency: the process refuses to start without it, and requests return 503
  while it is unreachable.

Redis only holds data that can be rebuilt.

## Tests

The Rust tests create a throwaway database per test and drop it afterwards, and need a Redis 7 to talk to. The
environment variables in the commands below are all they need:

```bash
# SQLite: no MySQL needed; each test creates a database file in the system temporary directory
GONOF_TEST_BACKEND=sqlite GONOF_TEST_REDIS_URL='redis://127.0.0.1:6379/0' \
  cargo test -p gono-forum-web --no-fail-fast -- --test-threads=4

# MySQL: an account that may create and drop databases
GONOF_TEST_MYSQL_URL='mysql://root@127.0.0.1:3306/mysql' GONOF_TEST_REDIS_URL='redis://127.0.0.1:6379/0' \
  cargo test -p gono-forum-web --no-fail-fast -- --test-threads=4
```

Instead of the variables you can copy `config/test.toml.example` to `config/test.toml` (ignored by Git) and fill it
in; a variable that is set takes precedence. The cookie and two-factor keys may be left out: each test run then uses
random ones. On SQLite, tests that check a MySQL mechanism only (execution plans, `information_schema`, MySQL error
codes) return early and pass; run with `-- --nocapture` to see their "skipped on SQLite" line.

Run `npm ci --ignore-scripts && npm run build` first; a few tests load the built stylesheet. The browser checks
start their own temporary sites and are described in [CONTRIBUTING.md](CONTRIBUTING.md).

## Translations

Translations are [Fluent](https://projectfluent.org/) files under `apps/web/locales/<language>/`, compiled into the
binary. Missing messages fall back to English, so a partial translation can be merged and completed later. See
[docs/translating.md](docs/translating.md).

## Documentation

- [docs/install.md](docs/install.md): installing and running a site.
- [docs/architecture.md](docs/architecture.md): an overview of the code, with links to the reference documents on
  settings and caching, queries, the member API, roles, styles and assets.
- [docs/translating.md](docs/translating.md): translating the interface and adding interface text.
- [docs/api.md](docs/api.md): the member API.
- [docs/releasing.md](docs/releasing.md): building and publishing releases.

## Contributing and security

See [CONTRIBUTING.md](CONTRIBUTING.md) for how to set up, test and send changes, and
[docs/architecture.md](docs/architecture.md) for an overview of the code. Please report security problems privately
as described in [SECURITY.md](SECURITY.md), not in public issues. Everyone taking part is expected to follow the
[code of conduct](CODE_OF_CONDUCT.md).

## License

Gono Forum is dual-licensed under the [MIT License](LICENSE-MIT) and the [Apache License 2.0](LICENSE-APACHE); you
may use it under either. Bundled third-party files keep their own licenses, listed in [NOTICE](NOTICE). The Gono
logo and icons are not covered by these licenses.
