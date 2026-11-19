# Installing and running Gono Forum

This guide is for people who run a Gono Forum site. It covers the release packages and `forum.install.sh`; to work on
the code, see the README's "Build from source".

## What you need

- A Linux server on x86_64 or arm64 with systemd, and root access. The binaries are statically linked, so the
  distribution does not matter.
- A reverse proxy that terminates HTTPS (the examples use nginx). Gono Forum speaks plain HTTP and listens on
  `127.0.0.1` by default.
- A database: SQLite needs nothing installed; MySQL needs 5.7.44 or later and an empty database.
- Optional: Redis 7.0 or later (only for several processes), an S3-compatible bucket for image attachments, an SMTP
  account for mail.

## Install

```bash
curl -fsSL https://gono.dev/forum.install.sh | sudo bash -s -- install
```

`https://gono.dev/forum.install.sh` redirects to `forum.install.sh` of the latest release on GitHub
(`https://github.com/gonolab/gono.forum/releases/latest/download/forum.install.sh`), which works the same.

Options for `install`:

| Option | Meaning |
| --- | --- |
| `--instance NAME` | Instance name (lowercase letters, digits, `-`; default `main`). Each instance has its own directory, account and service. |
| `--port PORT` | Listen on `127.0.0.1:PORT` (default 58833). |
| `--listen ADDR` | Full listen address instead of `--port`. Keep it on loopback unless you know why not (see "Reverse proxy"). |
| `--version VERSION` | A specific release instead of the latest. |
| `--package FILE` | Install a downloaded package instead of downloading. Its `checksums.txt` must be next to it, or given with `--checksums FILE`. |
| `--releases-url URL` | Another release source with the same layout (a mirror, or `file:///absolute/path`). Also used by the admin area's update check. |

Every package and `checksums.txt` is checked with SHA-256; a missing or wrong checksum stops the script before it
changes anything. It also stops if the port is in use or the instance is already installed. An instance counts as
installed once its systemd unit exists, which the script writes last: an install that stopped before that (a full
disk after the binaries were written, say) is finished by running the same `install` again, and `upgrade` refuses
such an instance. An instance removed with `uninstall` but without `--purge` is installed again with its kept
configuration and data (see "Uninstall").

The script creates:

```text
/opt/gono-forum-<instance>/          owned by the service account, 0750
  gono-forum-web  gono-forum-cli     the binaries (writable by the service account for one-click upgrades)
  config/gono-forum.toml             0600; the setup wizard adds the database, Redis and three keys
  data/                              local data: SQLite database (data/db/), upload staging (data/media/),
                                     mail written to files, exports, the setup token, upgrade state
/usr/local/lib/gono-forum/forum.install.sh
                                     owned by root and shared by all instances: run it for upgrade, rollback,
                                     status and uninstall; the service runs it as a start guard
```

Everything in the instance directory belongs to the service account, so the script never runs anything from there as
root, and what it does there (writing files, checking a binary's version) it does as that account. Run the copy in
`/usr/local/lib/gono-forum/`, never a script inside an instance directory, with `sudo`.

- System account `gono-forum-<instance>` (no login shell, no home directory).
- systemd service `gono-forum-<instance>.service`: `Type=notify`, restarts on failure, `GONOF_HOME` set to the
  instance directory, and sandboxing (`ProtectSystem=strict` with only the instance directory writable,
  `NoNewPrivileges`, no capabilities, private `/tmp`, restricted address families and system calls). Put local
  changes in a drop-in with `systemctl edit gono-forum-<instance>`; upgrades rewrite the unit file but keep drop-ins.

At the end the script prints a one-time setup token, also saved in `data/setup-token`. Until setup is finished the
service also prints it to the journal at every start (`journalctl -u gono-forum-<instance>`), and writes a new one if
the file is missing.

## Setup wizard

Until setup is finished, every address shows the wizard and `/healthz` returns 503. Open the site through your
reverse proxy (or an SSH tunnel to the listen port) and enter the token. Then:

1. **Database.** SQLite: the file path, by default `data/db/gonof.db` in the instance directory; it is created if
   missing. A relative path is in the instance directory, and the file name is used as typed (`%`, `?` and `#`
   included). MySQL: a connection string such as `mysql://gonof:password@127.0.0.1:3306/gonof` (URL-encode special
   characters in the password). Create the database first, empty, with `utf8mb4` and `utf8mb4_bin`, and give the
   account all privileges on that database only:

   ```sql
   CREATE DATABASE gonof CHARACTER SET utf8mb4 COLLATE utf8mb4_bin;
   CREATE USER 'gonof'@'127.0.0.1' IDENTIFIED BY '…';
   GRANT ALL PRIVILEGES ON gonof.* TO 'gonof'@'127.0.0.1';
   ```

   No global privileges such as SUPER are needed. A database that already holds other tables, or another Gono Forum
   site, is refused before anything in it changes.

   The connection uses TLS whenever the server offers it. The client's TLS library (rustls) only accepts ECDHE key
   exchange, which some MySQL 5.7 builds do not offer; the official `mysql:5.7` Docker image is one of them. With
   such a server the connection fails with `HandshakeFailure`. Either give the server a TLS setup that offers ECDHE
   (a build against a current OpenSSL does), or, when the database is on the same machine or a private network, add
   `?ssl-mode=disabled` to the connection string.
2. **Redis** (optional): the URL and a key prefix. Leave it off for a single process (see "Redis").
3. **Forum address** (optional): the full public address, including a path if the forum lives under one
   (`https://example.com/forum`). Left empty, links follow the host of each request. It can be changed later in the
   admin area.
4. **Site time zone**, used for daily limits, check-ins and weekly statistics.
5. **Administrator**: username, optional email (counted as verified) and password.
6. **Sample content** (optional, off by default): 10 members, 5 boards, 100 threads and 1,000 replies, dated over the
   two months before the install, in the language you pick below the box: English or Simplified Chinese. Both have
   the same threads, authors and nesting; only the words differ. The choice only affects the sample: the interface
   language is set afterwards in the admin area (`/manage/languages`). The sample shows what lists, threads and nested
   replies look like before real members arrive. The sample members have no password or email, so nobody can sign in
   as them. It needs a database without boards, threads or posts (the wizard says so otherwise); leave it off for a
   site that goes straight into use. `gono-forum-cli seed-sample` adds the same content later (see "Command-line tool");
   to bring content of your own instead, see [content-import.md](content-import.md).

The wizard writes `config/gono-forum.toml` (keeping anything already in it), creates the tables, creates the
administrator, adds the sample content if you ticked it, deletes the token and switches to the forum on the same port
without a restart. If adding the sample content fails after the administrator exists, setup still finishes without it
and the journal says why.

After that, sign in and go through the admin area at `/manage`: the site name, registration mode, boards
(`/manage/forums`; a new site has none unless you added the sample content), mail (sending is off until you turn it
on), storage for attachments, sign-in providers and the language.

## Reverse proxy

The application trusts the client address in `X-Real-IP` and the scheme in `X-Forwarded-Proto` only when it listens
on loopback and the request comes from loopback. So run the proxy on the same machine and set both headers:

```nginx
server {
    listen 443 ssl;
    http2 on;
    server_name forum.example.com;
    # ssl_certificate …; ssl_certificate_key …;

    client_max_body_size 14m;   # the image upload limit ([media] image_max_bytes, 10 MiB by default) plus 4 MiB

    location / {
        proxy_pass http://127.0.0.1:58833;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-Proto $scheme;
    }
}
```

- `client_max_body_size` matches what the forum itself accepts for an upload: the image limit plus 4 MiB for the rest
  of the form (the editor without JavaScript sends its whole form with the file). Raise both together.
- Without `X-Real-IP`, every visitor counts as `127.0.0.1` and per-address rate limits (sign-in, registration,
  password reset, API) are shared by the whole site; the log warns about it once an hour.
- Without `X-Forwarded-Proto: https`, the application assumes plain HTTP and its session cookies lose `Secure`.
- Under a sub-path, forward the path unchanged (`location /forum/ { proxy_pass http://127.0.0.1:58833; … }`, no
  trailing slash on `proxy_pass`) and set the forum address with the path. The application strips the prefix
  itself; `/forum/healthz` is the health check (on the listen address itself `/healthz` answers too). If the admin area becomes unreachable after a wrong address, fix it
  with `gono-forum-cli set-site-url` (see "Command-line tool").
- Several forums can share one host under different paths or on different ports. Browsers keep cookies per host,
  whatever the port, so each forum names its cookies after its path prefix and its port when that is not the default
  one (`gonof_session_forum`, `__Host-gonof_session__port8443`), and signing in to one does not sign out of another.
  A forum at the root on the default port keeps the plain names; moving a forum to another path or port signs
  everyone out of it once.
- Under a sub-path, crawlers still read only `/robots.txt` at the domain root, so the forum's own
  `/forum/robots.txt` has no effect. The SEO section of Search & performance (`/manage/settings/search`) shows the
  text to merge into the domain root's `robots.txt` (rules with the path prefix and the sitemap line); merge it again
  after changing the rules or the indexing switch.

Static files are served by the application; the proxy does not need to serve them. An address with the running build's
asset version is cached for a year and every other one is revalidated, so while an upgrade rolls across several
machines, one still on the old build cannot leave its old files cached under the new build's addresses.

## Redis

- **One process** (the usual install): leave Redis off. Caches and rate limits live in the process.
- **Several processes or machines on one database**: turn Redis on in every process with the same URL and key
  prefix. Shared caches, rate limits and cache invalidation then go through Redis, and Redis becomes a hard
  dependency: a process refuses to start without it, and requests return 503 while it is down. Use a separate key
  prefix for each site sharing one Redis.

To change it after setup, edit `[redis]` in `config/gono-forum.toml` and restart the service.

## Several machines

Background work is not tied to the machine that started it: any process may process an upload another one received,
send its mail or build a data export, and a member may download that export through any machine. So every machine
must see the same files in two directories of `config/gono-forum.toml`:

| Setting | What is in it |
| --- | --- |
| `[media] local_directory` | uploads waiting to be checked and processed, and the storage credentials |
| `[identity] local_directory` | data exports, mail and text messages written to files, the setup token |

Put both on storage every machine mounts at the same path (a network file system), and give the service access to
it with a drop-in (`systemctl edit gono-forum-<instance>`, then `[Service]` and `ReadWritePaths=/srv/gono-forum`
for a mount at `/srv/gono-forum`). Point the settings at directories on that mount, such as
`/srv/gono-forum/identity` and `/srv/gono-forum/media`, not at the instance's `data/` itself: `data/upgrade/`
belongs to the binaries of each machine and must not be shared. The database must be MySQL, and Redis must be on.

## Files to back up

| What | Why |
| --- | --- |
| The database | MySQL: `mysqldump --single-transaction`. SQLite: `sqlite3 data/db/gonof.db ".backup '/backup/gonof.db'"` (safe while running), or copy the file with the service stopped. |
| `config/gono-forum.toml` | Holds the three keys. Without `[identity.two_factor] active_key`, members who use an authenticator can only sign in with recovery codes, and lottery redemption codes cannot be read; without `[common] encryption_key`, the stored SMTP and SMS secrets must be entered again; a new cookie key signs everyone out and voids SMS codes not used yet. |
| `data/` | The attachment storage keys (`data/media/secrets/`, readable only by the service account and not encrypted: without them attachments cannot be read until the keys are entered again in the admin area), exports waiting to be downloaded, mail written to files (deleted with its event after the notification and mail event retention period, 30 days by default; a temporary `.tmp` file left by a write that stopped halfway is deleted after an hour), text messages written to files (deleted with their code after two days, or when the account they were sent to is deleted), the upgrade state. |
| `skins/` | Skins in the instance directory ([skins.md](skins.md), "Sources"), if you use any. Every machine of a site has its own; imported skins are in the database. |
| The S3 bucket | The attachments themselves. |

When you restore the database from a backup, empty `mail/prepared/` in `[identity] local_directory` as well. It holds
the finished text of mail still being retried, under the id of its event, and the restored database hands those ids
to new events. A draft that does not match its event is discarded and written again; emptying the folder also
covers the drafts of versions up to 0.2.0, which record no event.

Upgrades do not take backups (next section).

## Upgrades

**From the admin area**: Tools & maintenance › Version updates checks the release source and, on a Linux release package
started by systemd, upgrades with one click: it downloads the package, checks its SHA-256, makes sure both new
binaries run on this machine and report the new version, swaps them in (keeping the old ones as `*.previous`) and lets
systemd start the new version. If the new version fails to start twice, the start guard puts the previous binaries
back, all of them or none. If the page says the last upgrade is incomplete (the command-line tool is not the new
version), run `forum.install.sh upgrade --force` to put both in place. The
page explains why the button is missing when it cannot upgrade this way (source build, unsupported platform, not
started by systemd, program directory not writable, or an unfinished upgrade or rollback from the shell). A one-click upgrade replaces only the two binaries; `forum.install.sh upgrade` also
updates `forum.install.sh` and the unit file.

While systemd restarts the service the forum is unreachable for several seconds (the unit waits `RestartSec=3` before
it starts the new version), and a reverse proxy answers 502 in that time. The update page waits through it and
reloads once the forum answers again. Without JavaScript it simply reloads every 3 seconds and may stop on the
proxy's error page; reload it by hand.

**From the shell**:

```bash
sudo bash /usr/local/lib/gono-forum/forum.install.sh upgrade              # latest release
sudo bash /usr/local/lib/gono-forum/forum.install.sh upgrade --version 0.2.0
sudo bash /usr/local/lib/gono-forum/forum.install.sh upgrade --package gono-forum_0.2.0_linux_amd64.tar.gz   # checksums.txt next to it
sudo bash /usr/local/lib/gono-forum/forum.install.sh rollback             # swap back to *.previous (run again to undo)
sudo bash /usr/local/lib/gono-forum/forum.install.sh status               # versions, service state, last upgrade
```

`upgrade` replaces the binaries as a whole or not at all. It puts the new binaries next to the old ones and checks
that they run, then saves the current binaries and their `*.previous` in `switch-backup/` in the instance directory
(hard links, so this takes no extra space) before it stops the service and moves the new ones in. After the restart
it checks `/healthz` on the listen address, the top-level `listen` of `config/gono-forum.toml` (`127.0.0.1:58833` when
it is not set), with curl, otherwise wget, otherwise bash's own `/dev/tcp`. The script reads that value in double or
single quotes, with or without a comment after it; when it cannot read it (escapes, a multi-line string), the upgrade
stops before the service is stopped. Each request may take 5 seconds, and the check gives up after 30. If any
step fails, or the new version does not start or answer, the script puts every saved binary back (keeping the new
ones as `*.failed`), starts the old version and checks it again. If the old version does not answer either, the
script says so and lists what to do by hand; it does not report the old version as running. An upgrade interrupted
by Ctrl-C or a dropped connection is undone the same way. A run that is killed outright leaves `switch-backup/`
behind: `status` reports it, the one-click upgrade waits for it to go, and the next `upgrade` or `rollback` puts it
back before doing anything else.

`upgrade` refuses an instance whose unit or `config/gono-forum.toml` is missing (see "Install" for an install that
stopped halfway). It does nothing when both binaries already report the version, unless you add `--force`. From GitHub
Releases, pre-releases are only installed when named with `--version`. `rollback` uses the same switch. It refuses to
run when a `*.previous` is missing or the two are of different versions. Once switched it stays switched, even if
the previous version does not start: run it again to switch forward.

The admin area reads the release source from `[update] releases_url` in `config/gono-forum.toml` (GitHub Releases
when it is not set; `install --releases-url` writes it). `forum.install.sh upgrade` does not read that file, which the
service account can change: give it the same `--releases-url` again. It stops with a message when the configuration
names another source and the option is missing. How packages are built and how the upgrade and the start guard work in
detail is in [releasing.md](releasing.md).

Rolling back to 0.2.0 or earlier: empty `mail/prepared/` first. Those versions refuse the drafts later ones write,
so the mail they belong to would fail every retry; without a draft it is written again.

**Migrations run automatically when a new version starts, and nothing is backed up first.** If a new version has
migrated the database and is then rolled back, the old version may refuse to start on the newer schema: restore the
database from your backup. Back up before every upgrade.

**Upgrades are not guaranteed during the alpha.** At the first release candidate the database migrations will be
folded into a new baseline, and a site installed from a 0.1.x or 0.2.x release may not upgrade past that point and
would have to be set up again (README, "Status").

Several processes or machines: upgrade each one; a one-click upgrade only replaces the instance it runs in.

## Several instances on one machine

Give each instance its own name, port, database (or SQLite file) and, with Redis, key prefix:

```bash
sudo bash /usr/local/lib/gono-forum/forum.install.sh install --instance community --port 58834
```

Every `forum.install.sh` command takes `--instance`.

## Uninstall

```bash
sudo bash /usr/local/lib/gono-forum/forum.install.sh uninstall           # keeps config/, data/ and the account
sudo bash /usr/local/lib/gono-forum/forum.install.sh uninstall --purge   # deletes them too (asks for the instance name)
```

Both first stop and disable the service. If that fails, or the service is still running afterwards, the command stops
with an error and removes nothing. With `--purge`, an account that `userdel` cannot delete (a process still running
as it, say) is reported, and the command exits with an error after removing everything else; delete the account by
hand then. Neither touches the MySQL database, Redis, the bucket or the reverse proxy. Purging the last instance on
the machine also removes `/usr/local/lib/gono-forum/`.

`install` with the same instance name after an `uninstall` without `--purge` is a reinstall: it puts the binaries,
the unit and the service back and keeps `config/gono-forum.toml` as it is, with the database address, the three keys,
the listen address and the release source. The setup wizard does not run again (unless setup was never finished, in
which case the kept token is printed). `--port` and `--listen` are refused then unless they name the kept listen
address; change the listen address in the kept configuration instead.

## Command-line tool

`gono-forum-cli` reads the same configuration. Run it in the instance directory as the service account, for
example:

```bash
cd /opt/gono-forum-main && sudo -u gono-forum-main ./gono-forum-cli set-site-url https://forum.example.com
```

Without arguments it lists its commands. The ones a site operator is most likely to need:

| Command | Use |
| --- | --- |
| `set-site-url <url>` | Change the forum address when the admin area is unreachable (`""` clears it). |
| `set-password <uid>` | Set a member's password from standard input, with the length rules of the web pages (the site's minimum length at Security › Account security, up to 128 characters). |
| `reset-two-factor <uid> --reason=<text>` | Turn off a member's two-step verification after checking their identity; signs them out everywhere and notifies them. |
| `rekey-two-factor` | Re-encrypt authenticator secrets and redemption codes after rotating the two-factor key. |
| `migrate` | Run migrations (also creates a new SQLite file). The service does this by itself at start. |
| `seed-sample [--language=<tag>]` | Add the setup wizard's optional sample content (10 members, 5 boards, 100 threads, 1,000 replies) to a site that has no boards, threads or posts yet; in English unless `--language` names another language with a sample (`--language=zh-CN`). |
| `export-sample <dir> [--language=<tag>]`, `check-content <dir>`, `import-content <dir>` | Write the sample as a content dataset to start from, check a dataset, and import one into a site without boards, threads or posts: see [content-import.md](content-import.md). |
| `skin check <dir\|file> [--builtin]`, `skin pack <dir> [--output=<file>]` | Check a skin directory or pack the way an import does, and pack a skin directory into one file to import at Appearance › Skins: see [skins.md](skins.md). Neither needs a database. |

## Two-factor key rotation

To replace `[identity.two_factor] active_key` in `config/gono-forum.toml`:

1. Generate a new key (`openssl rand -hex 32`), put it in `active_key` and move the old one into `previous_keys`
   (at most three; they are only used for decryption).
2. Restart the service. A member's secret still encrypted with an old key is re-encrypted the next time they pass
   two-step verification.
3. Run `gono-forum-cli rekey-two-factor`. It reports, for authenticator secrets and for lottery redemption codes,
   how many items it moved to the current key, how many were already on it and how many it could not decrypt.
4. Remove the old key only after both lines report 0 items it could not decrypt and a second run moves 0 items.

The cookie key `[identity.cookie]` rotates the same way through `previous_keys`; sessions signed with a removed key
end at once. It also keys the stored digests of SMS verification codes, so codes sent under a removed key stop
working too (they last at most 30 minutes, and members can ask for a new one). `[common] encryption_key` cannot be
rotated: after changing it, enter the SMTP and SMS secrets again in the admin area. The three keys must all be
different.

## Health and logs

- `GET /healthz` returns 200 when the forum is serving, 503 during setup or when the database (or Redis, if
  enabled) is unavailable or does not answer within 2 seconds, which holds even when every database connection is
  busy. Under a sub-path it is `/forum/healthz`, and `/healthz` at the root answers too (on every host of a site with
  several domains, also one registered only with a sub-path), so a check on the listen address works without the
  forum address.
- Logs go to the journal: `journalctl -u gono-forum-main`.
