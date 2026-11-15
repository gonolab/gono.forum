# Changelog

What changed in each release of Gono Forum. A version's section is also the title and text of its GitHub release
(see "Publishing a release" in [docs/releasing.md](docs/releasing.md)), so links in a section are absolute: the
release page does not resolve relative ones.

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
