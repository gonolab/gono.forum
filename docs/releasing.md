# Releasing Gono Forum

For maintainers: how release packages are built and published, and how an installed site upgrades itself. Running a
site from a release is covered in [install.md](install.md).

## Release files

A release is four files:

```text
gono-forum_<version>_linux_amd64.tar.gz
gono-forum_<version>_linux_arm64.tar.gz
forum.install.sh
checksums.txt          sha256sum lines for the two packages and forum.install.sh
```

Each package unpacks into a directory named like the package, holding `gono-forum-web` and `gono-forum-cli`,
`forum.install.sh`, and the licence files: `LICENSE-MIT`, `LICENSE-APACHE`, `NOTICE`, `docs/licenses/` and
`THIRD-PARTY-LICENSES.txt`. To keep builds repeatable, archive entries are sorted, owned by 0:0 and dated with the time
of the last commit. `forum.install.sh` is also published on its own, so that
`curl …/releases/latest/download/forum.install.sh | sudo bash -s -- install` works. The address in the install
instructions, `https://gono.dev/forum.install.sh`, redirects to that file of the latest release.

## How the binaries are built

- **Targets.** `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`, statically linked, cross-compiled with
  `cargo zigbuild` (zig is the C compiler for the musl targets). On musl the global allocator is mimalloc, because
  musl's own allocator is slow under multi-threaded load; other builds keep the system allocator.
- **Profile.** `--profile dist`: the release profile plus thin LTO and stripped symbols.
- **The `bundled` feature.** `build.rs` embeds every file under `apps/web/static/` with `include_bytes!`, and fails if
  the `npm run build` output (`css/forum.css`, `css/debug.css`, `js/composer.js`) is missing. The binary serves these
  files from memory with a strong ETag and `304 Not Modified`; addresses with `?v=` are cached for a year, others are
  revalidated. Without the feature, source builds serve `apps/web/static/` from disk, so style changes need no
  rebuild. Templates, translations and `config/default.toml` are compiled into every build, bundled or not.
- **Instance root and configuration.** `GONOF_HOME` sets the instance directory. Without it a bundled binary uses
  the directory it is in, and a source build uses the source tree. Relative paths in the configuration
  (`local_directory`, a relative SQLite file) resolve against the instance directory. The private configuration is
  the file named by `GONOF_CONFIG`, otherwise `config/gono-forum.toml` in the instance directory (bundled) or
  `config/development.toml` (source build), read on top of the compiled-in defaults.
- **Version.** The crate version in `apps/web/Cargo.toml`, or the value of `GONOF_BUILD_VERSION` at build time (for
  test packages such as `0.1.1-dev.1`). `gono-forum-web --version` prints `gono-forum-web <version>`; the update check
  and `forum.install.sh` compare that.
- **Third-party licences.** `cargo about generate`, configured by `scripts/licenses/about.toml` with the template
  `scripts/licenses/about.hbs`, writes `THIRD-PARTY-LICENSES.txt`: the licence texts of the Rust crates linked into
  the release binaries (build and dev dependencies left out), one file for both targets. A crate under a licence
  that `about.toml` does not accept fails the build until it is reviewed.

## Building packages

On Linux:

```bash
scripts/package-release.sh [--version <semver>] [--arch amd64|arm64]... [--out <dir>]
```

It needs rustup (the toolchain comes from `rust-toolchain.toml`; the script adds the musl targets), zig,
cargo-zigbuild, cargo-about (`cargo install --locked cargo-about --features cli`), Node.js with npm, tar and
sha256sum. Without `--arch` it builds both architectures; without `--out` it writes to `dist/<version>/`.

The script runs `npm ci --ignore-scripts && npm run build`, generates the licence file, then for each architecture
runs `cargo zigbuild --locked --profile dist --features bundled --target <triple> -p gono-forum-web --bins`. It checks
with `file` that both binaries are statically linked and, when the build machine has the same architecture as the
package, that `gono-forum-web --version` reports the expected version. Last it writes `checksums.txt`.

## Publishing a release

1. Set `version` in `apps/web/Cargo.toml` and let Cargo update `Cargo.lock`; packages are built with `--locked`.
2. Write the version's section in `CHANGELOG.md`: a heading `## <version>`, optionally followed by a label such as
   `(early alpha)`, then the text. The release is titled "Gono Forum " followed by the heading, and its text is the
   section. Links in it must be absolute, because the release page does not resolve relative ones.
   `scripts/release-notes.sh title|notes <version>` prints both.
3. Commit, then run the Rust suite on that commit on SQLite, MySQL 5.7.44 and MySQL 8.4 (commands in the README's
   "Tests"; 5.7 with `--test-threads=2`). CI does not run it, so this is the only test run a release gets.
4. Tag `v<version>` and push the tag.

`.github/workflows/release.yml` runs on every pushed `v*` tag. It first runs the checks in `.github/workflows/ci.yml`
on the tagged commit (formatting and Clippy); if either fails, nothing is built or published. Then it installs
Node.js 22, the two musl targets, zig, cargo-zigbuild and cargo-about 0.9.3, fails unless the tag without its `v`
equals the crate version, and fails unless `CHANGELOG.md` has exactly one section for the version and that section
has text. It then runs `scripts/package-release.sh --version <version>` and creates the GitHub release with the title
and text from the changelog and the four files. A version with a `-` (`0.2.0-rc.1`) is published as a pre-release.

GitHub's "latest" release skips pre-releases, so the update check and `forum.install.sh` without `--version` never offer
one; `forum.install.sh install|upgrade --version 0.2.0-rc.1` installs it explicitly.

## The release source

The update check and `forum.install.sh` read two kinds of address under a release source:

```text
<releases_url>/latest/download/checksums.txt
<releases_url>/download/v<version>/<package>
```

`releases_url` defaults to `https://github.com/gonolab/gono.forum/releases`. Any mirror with the same layout works,
including a local directory (`file:///absolute/path`); `forum.install.sh install --releases-url` writes it into the
instance configuration as `[update] releases_url`, where the update check reads it. `forum.install.sh` itself does not:
it runs as root and installs what it downloads, its own next version among it, while that file belongs to the service
account. It takes a source other than the default only from `--releases-url`, and refuses to start when the
configuration names another source and the option is missing. Only `https://` and `file:///` sources are accepted; over HTTPS at most five
redirects are followed (GitHub sends release assets through two).

The check reads `checksums.txt`, keeps the lines that are a 64-digit digest followed by a package name for this
platform (`linux_amd64` or `linux_arm64`), and takes the highest version by SemVer 2.0 ordering. A mirror can
therefore list several versions in one `checksums.txt`. The result is kept in the process that checked.

## In-app update check and one-click upgrade

Tools & maintenance › Version updates (`/manage/updates`, administrators only) checks the release source and, when a
newer version exists and nothing blocks it, offers a one-click upgrade. The result of a check is kept in the process,
so the upgrade form sends back the version and package digest the page showed; when a check run since (from another
tab, or by another administrator) found another release, the upgrade is refused with 409 and the page has to be looked
at again. The upgrade runs in the background and the page shows its progress:

1. Download the package (at most 256 MiB) and compare its SHA-256 with `checksums.txt`; a mismatch stops here.
2. Take the two binaries out of the archive: only regular files named `gono-forum-web` and `gono-forum-cli`, at the
   top level or one directory down. Links, other entries and paths with `..` are ignored; an entry over 256 MiB is
   refused.
3. Write them next to the running binaries as `*.new` and run each with `--version`. Each must print exactly
   `<binary> <new version>` (`gono-forum-web 0.2.0`, `gono-forum-cli 0.2.0`) within 15 seconds, which proves it runs on
   this machine and belongs to the release; otherwise both `*.new` files are removed and nothing else changes.
4. Write the marker `data/upgrade/pending` (from, to, time).
5. For the web binary and then the command-line tool: hard-link the current binary as `*.previous` (a copy where the
   file system has no hard links), then rename the new one over it. Each step is a single rename, so no binary is ever
   missing. If a step fails, the steps already done are undone, last first, so both binaries are the running version
   again; the marker and the `*.new` files are removed once that worked (and kept for the start guard and the new
   version if it did not).
6. Shut down gracefully. systemd (`Restart=always`) starts the new binary.

The new version runs migrations, sends `READY=1` and then confirms the upgrade if `pending` names its own version. An
upgrade cut short between the two binaries (power lost, the process killed) leaves the command-line tool's `*.new`;
the new version first moves it in the same way. It then runs `gono-forum-cli --version` and writes
`data/upgrade/last`: `result=upgraded` when the tool reports the new version, otherwise `result=incomplete`, for which
the admin page says to run `forum.install.sh upgrade --force`. Either way it removes the marker and the start count. When
`last` cannot be written, it keeps the marker, removes only the start count and records the outcome at the next start.
While migrations run, the process keeps extending systemd's start timeout (`EXTEND_TIMEOUT_USEC`).

While the upgrade runs, the page reloads every 3 seconds to show the current step. Between step 6 and the new
version's first answer there is a gap of at least `RestartSec=3` in which a reverse proxy answers 502, so `site.js`
reloads the page only once a request for it succeeds; without JavaScript a `<noscript>` meta refresh reloads it regardless. The
"upgrade has started" notice is shown only while an upgrade runs, so reloading its address afterwards shows the
outcome alone. `tests/e2e/upgrade-wait.spec.js` checks the wait with simulated answers.

The page gives the reasons instead of a button when the process cannot replace itself: a source build, a platform
other than Linux on x86_64 or arm64, not started by systemd (no `INVOCATION_ID`), a program directory the service
account cannot write to, or a `switch-backup/` left in it by an upgrade or rollback with `forum.install.sh` that did
not finish (see "Upgrades and rollbacks from the shell"). Only the directory needs to be writable: the running
binaries are never opened for writing (Linux refuses that with `ETXTBSY`), they are renamed. `forum.install.sh
upgrade` works in all of these cases.

A one-click upgrade replaces only the two binaries of the instance it runs in; `forum.install.sh upgrade` also replaces
the copy of `forum.install.sh` in `/usr/local/lib/gono-forum/` and rewrites the unit file.

## Who runs what

The instance directory belongs to the service account, which has to replace the binaries for a one-click upgrade, so it
could also replace or link any other entry there. Nothing that root runs lives in it: `forum.install.sh install` and
`upgrade` keep the script in `/usr/local/lib/gono-forum/forum.install.sh` (root-owned, shared by all instances on the
machine), and the unit's start guard reads it from there. Whatever the script does inside the instance directory
(reading a binary's version, writing the configuration, the setup token or `data/upgrade/last`, putting binaries in
place and swapping them) runs as the service account, through `setpriv` in a session of its own without a terminal.
Root only creates the directory and removes entries from it.

## The start guard

The unit that `forum.install.sh` writes runs `/usr/local/lib/gono-forum/forum.install.sh guard <instance directory>` as
`ExecStartPre`, as the service account, before every start. The guard is a shell script because a new binary may crash before it can do
anything. Without `data/upgrade/pending` it does nothing. With it, it counts starts in `data/upgrade/attempts`: the new
version gets two starts, and before the third the guard switches back. It switches all binaries or none, the same way
as `forum.install.sh upgrade` (next section). It saves the binaries in `switch-backup/`, together with a copy of the
marker, and then, binary by binary, keeps the new one as `*.failed` and renames its `*.previous` over it. It then
writes `data/upgrade/last` with `result=rolled_back`, removes `switch-backup/` and the marker, and lets the previous
version start.

The guard never starts a mix of both versions. In these cases it gives up instead: it removes the marker, leaves the
new version in place for systemd to keep starting, and says why in the journal:

- a rename fails during the switch: the guard first puts back the new binaries from `switch-backup/`;
- a `*.previous` is missing;
- `switch-backup/` already exists without a copy of this marker, that is, an upgrade or rollback with
  `forum.install.sh` was cut short. The one-click upgrade refuses to start in that state, and the guard does not
  touch it.

A guard that is itself cut short (killed, power lost) leaves `switch-backup/` with its copy of the marker. The next
start puts those binaries back and switches again. Running the binaries' `--version` is limited to 15 seconds,
both by the one-click upgrade and by `forum.install.sh`, so a binary that hangs cannot hold up either.

`forum.install.sh rollback` swaps `*.previous` and the current binaries by hand (running it again swaps forward) and
records `result=switched`. The admin page shows the last of these outcomes.

## Upgrades and rollbacks from the shell

`forum.install.sh upgrade` replaces the binaries in a single switch, which either finishes or is undone. It refuses an
instance without its unit (written last by `install`, so an install that stopped halfway has none) or without
`config/gono-forum.toml`, and it reads the listen address for step 7 first: the top-level `listen` key, in double or
single quotes with an optional comment after it, `127.0.0.1:58833` when the key is missing. A value it cannot read
stops it before anything changes, because the switch could not check the new version.

1. Download the package and check it against `checksums.txt`, into a temporary directory that belongs to root.
2. Stop here if both installed binaries already report the package's version (unless `--force` is given). Both are
   compared because an earlier run may have replaced only one of them.
3. Install each binary next to the running one as `*.new` and check that `<binary>.new --version` reports the
   package's version. Nothing has changed yet; on failure the `*.new` files are removed.
4. Install the new `forum.install.sh` and rewrite the unit. These are kept even when the upgrade is undone: they work
   with older binaries, and an older unit may point at a start guard in the instance directory that has since been
   removed.
5. Remove any leftover `data/upgrade/pending`, so that the start guard does not count these starts as a one-click
   upgrade's. Then save every binary and its `*.previous` in `switch-backup/`: hard links (copies where the file
   system has none) are collected in `switch-backup.tmp/`, which is renamed to `switch-backup/` once complete.
6. Stop the service. For each binary, hard-link it as `*.previous` and rename `*.new` over it. Every step is a single
   rename, so no binary is ever missing.
7. Start the service and check `/healthz` on the listen address once a second until it answers 200, or 503 from the
   setup wizard. The request is made with curl, otherwise with wget, otherwise with bash's own `/dev/tcp`. Each
   request has 2 seconds to connect and 5 in all, and the check gives up after 30 seconds. Once
   the service answers, renaming `switch-backup/` away finishes the upgrade: the replaced binaries stay as
   `*.previous`, the ones from before them are dropped, and `data/upgrade/last` records `result=upgraded`.

If any step from 5 on fails, the script puts back what `switch-backup/` holds: every binary and `*.previous` as it
was, and any name that was not there before is removed again. The new binaries are kept as `*.failed` when they had
started. The script then starts the service, checks it again and, if the new version had started, records
`result=rolled_back`. If the old version does not answer either, the script stops with an error and a list of what
to do by hand instead of reporting it as running. The exit handler does the same when the script is interrupted
(SIGINT, SIGTERM, SIGHUP) during the switch. A run killed outright (SIGKILL, power loss) leaves `switch-backup/` behind.
`status` reports it, the one-click upgrade refuses to start while it exists, and the next `upgrade` or `rollback` puts
it back and restarts the service before doing anything else; `rollback` stops after that.

`forum.install.sh rollback` uses the same `switch-backup/` and the same checks of the unit, the configuration and the
listen address. It first checks that every binary has a `*.previous` and
that all of them report the same version. Then it swaps each binary with its `*.previous` and finishes the switch
before it starts the service, so the rollback stands even if the previous version does not start.

`tests/install/simulate.sh` runs install, upgrade, rollback, the start guard and these failure paths against a fake
systemd in a temporary directory (see "Install script" in [CONTRIBUTING.md](../CONTRIBUTING.md)).

Migrations get no special treatment. A new version that migrated the database and then failed leaves a schema the
previous version may refuse to start on (it finds a migration it does not know); the database then has to be restored
from a backup. The admin page and [install.md](install.md) say so.

## Known limitations

- Packages are not signed. `checksums.txt` comes from the same release source as the packages, so the SHA-256 check
  catches corrupted or truncated downloads, not a compromised source.
- The arm64 package is cross-compiled and checked to be statically linked, but its `--version` check only runs on an
  arm64 build machine, and it has not yet been run on real arm64 hardware.
- Only Linux on x86_64 and arm64 with systemd is packaged.
