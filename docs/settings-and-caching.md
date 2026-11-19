# Settings and caching

How site settings are stored, read, validated and saved, and how the process cache and the Redis cache work. The
overview is in [architecture.md](architecture.md), "Caches"; when to turn Redis on is in [install.md](install.md),
"Redis".

## Where settings live

Everything an administrator can change lives in the database and is edited under `/manage`. The deployment
configuration (`config/gono-forum.toml`) only holds deployment facts: the listen address, the database and Redis
connections, keys, local directories, attachment and image limits, and the release source for updates. Nothing falls
back from one to the other, and code has no defaults for settings: the only defaults are the rows the baseline
migration seeds.

| Table and keys | What it holds | Code |
| --- | --- | --- |
| `gonof_common_settings`, `settings.{group}.{field}` | 134 scalar settings, one row each | `apps/web/src/runtime.rs`, `runtime/settings.rs`, `runtime/kv.rs` |
| `gonof_common_settings`, `settings.navigation.{body,title,updated_at}` | the header navigation document | `runtime.rs` |
| `gonof_common_settings`, `settings.layout.{body,title,updated_at}` and `settings.domain_layout.<domain id>.{body,title,updated_at}` | the site's skin choice, seed colors and places, and each domain's override ([skins.md](skins.md), "Choosing a skin" and "Places and blocks"; [styles.md](styles.md), "Seed colors") | `skins/selection.rs` |
| `gonof_common_settings`, `settings.layout_draft.site.{body,title,updated_at}` and `settings.layout_draft.<domain id>.…` | the layout editor's unpublished drafts ([skins.md](skins.md), "Editing a layout"); read only by a previewing administrator's page frame | `skins/selection.rs` |
| `gonof_common_skin` | imported skins, one row per id, with a `version` column for optimistic locking ([skins.md](skins.md)) | `skins/store.rs` |
| `gonof_common_settings`, `mail.transport`, `media.storage`, `login.providers` | one JSON value each (SMTP, attachment storage, SMS and sign-in providers). The SMTP, SMS and sign-in provider secrets inside are encrypted with `[common] encryption_key`; the attachment storage keys are not in the database at all: each storage configuration names a file under `[media] local_directory` (`secrets/<id>`, mode 0600, plain JSON). Configurations are kept for the attachments stored with them, each distinct one once: saving the storage settings unchanged writes nothing, and returning to an earlier configuration makes it active again | `mail/settings.rs`, `media.rs`, `sms.rs` |
| `gonof_common_settings`, `identity.bootstrap_admin`, `identity.username_namespace` | lock rows, not settings (see "Write gate and locks") | `identity.rs`, `identity/profile.rs` |
| `gonof_common_localized_text`, key `(kind, language)` | policies (`policy.terms`, `.privacy`, `.rules`, `.contact`, `.verified`, `.links`) and mail templates (`mail.{kind}`), one row per customized language | `runtime.rs`, `mail/templates.rs` |
| `gonof_common_credit_rule` | credit reward rules (`reply`, `signin`, `thread`), with a `version` column for optimistic locking | `activities/ledger.rs` |

`gonof_common_settings` has two columns, `skey varchar(190)` (primary key, `utf8mb4_bin`) and `svalue text`. The
migrations seed 142 rows: the 134 scalar settings, the three navigation rows, the three rows of the layout document and
the two lock rows; the JSON rows are inserted on their first save, a domain's layout document when its own skin is
first chosen, its colors first set or its places first published (it is deleted with the domain, or when the domain
follows the site again in all three), and a layout draft by the first change in the layout editor (deleted when it is
published or discarded, and with its domain). A policy or mail template with no row for a language uses that language's built-in text
(a blank title and body, or subject and body, counts as not customized: saving a blank policy and "Restore built-in
text" delete the row, and the policy edit forms show the built-in text as placeholders).
Per-member choices are in their own tables, where `NULL` means "follow the site default" in `settings.reading.*`.

## Keys and groups

Every scalar key is `settings.{group}.{field}`. `SiteSettings` has one sub-structure per group, and each field is
named after the last segment of its key. `mail`, `navigation`, `layout`, `domain_layout`, `layout_draft` and `policy` are
not scalar groups; a unit test keeps scalar keys out of those prefixes.

| Group | Admin page |
| --- | --- |
| `site` (name, contact, read-only mode, forum address, brand words) and `time` | Global settings › Site settings |
| `site.description`, `seo`, `feed`, `cache` | Global settings › Search & performance |
| `site.language` | Languages |
| `signup`, `invite`, `username`, `api` | Global settings › Registration & permissions |
| `login`, `sms` | Global settings › Login methods |
| `growth`, `ranks`, `tip` | Global settings › Points (the tab is named after the site's points) |
| `display`, `reading` | Appearance › Display |
| `links` | Appearance › Friend links (application rules) |
| `forum` | Forums › Forums (site-wide block at the top) |
| `domains` | Multiple domains |
| `posting`, `limit`, `governance` | Security › Anti-spam & governance |
| `account` | Security › Account security |
| `email` | Security › Mail settings (the sending switch) |
| `maintenance`, `debug` | Tools & maintenance › Maintenance |

## Encoding

`svalue` is plain text (`runtime/kv.rs`, `runtime/settings.rs`):

- Booleans are exactly `true` or `false`; integers are canonical decimal (no plus sign, padding or spaces) inside a
  closed range.
- Strings are stored as typed. The empty string is a valid value: an empty contact address or forum address means
  "not set".
- Choices must be in an allowlist: an unknown `settings.signup.mode` would make every equality check fail, which
  amounts to open registration.
- Number lists are comma-separated (`settings.tip.amounts`, `settings.growth.levels`); sets of choices are
  comma-separated in allowlist order (`settings.login.methods`); multi-line text has one item per line, separated by
  LF (`settings.seo.robots`, `settings.growth.level_names`, `settings.invite.prefixes`).

Rows read by prefix (the navigation document) use a half-open key range `['prefix.', 'prefix/')` from `kv::range`,
never `LIKE 'prefix%'`, because `_` is a `LIKE` wildcard.

## Reading settings

Decoding never falls back. A missing row, a value that does not parse, a value outside its hard bounds and a broken
cross-field rule (such as `settings.posting.daily_threads` below `first_day_threads`) all raise `SettingsError` with
the key, what the database holds and what the value should be. Every page reads the settings, so one bad row turns the
whole site, the admin area included, into a plain error page naming the key, and it has to be fixed in the database.
The hard bounds in code are validation ranges, not defaults.

Each caller uses one of three copies:

| Copy | Read by | Function |
| --- | --- | --- |
| Process cache (the settings block) | the page frame and everything that only displays: robots.txt, the sitemap, feeds, the web manifest, the member API, the debug panel, session lifetimes and the registration switch, mail delivery, most background workers | `GlobalCache::settings` |
| Write gate | every protected write transaction, for everything it checks | `runtime::locked`, `runtime::guard_write` |
| Direct read | admin edit pages (they show current values), a few workers without a cache handle, the CLI | `runtime::read` |

On a miss the settings block loads the scalar settings, the navigation document and the layout documents in one
statement (`runtime::read_with_navigation`): the scalar keys and the two documents by primary key `IN`, and the domain
layout documents by the half-open range of their family after it (`OR (skey>='settings.domain_layout.' AND
skey<'settings.domain_layout/')`); this read takes no locks. The page frame then adjusts its own copy per request: another domain's site name
and verification codes, and, for values left empty (site name, description, read-only message, brand words, points and
index names, level names), the product name or the words of the request's language. Admin forms keep the empty
strings. A navigation body that no longer parses does not fail pages: the header falls back to the default board
navigation and a warning is logged. Likewise a layout document that no longer parses, or a skin that can no longer be
used, falls back to the built-in default skin with an error in the log ([skins.md](skins.md), "Sources").

## Write gate and locks

The first lock of every protected write transaction is `GATE_SQL`, a shared-lock read of all scalar settings
(`runtime::locked`); `guard_write` also refuses the write while `settings.site.read_only` is on, unless it is a
management action. Everything the transaction checks (quotas, counts in the database, the time zone offset, the site
address, tip tiers) uses the copy the gate returned, never the cache. A settings save waits for transactions holding
the gate, and every transaction that takes the gate after the save commits sees the new values. Writes that stay open
in read-only mode (reports, restoration requests) skip the gate and get their settings from the page frame.
`GATE_SQL` must stay one statement: `tests/cookie_auth_concurrency.rs` asserts that an entry point whose authentication
fails runs only this query.

The plain read, the gate and the save lock come from one function (`scalar_sql` in `runtime.rs`) and one key list,
and differ only in the trailing clause (none, `LOCK IN SHARE MODE`, `FOR UPDATE`):

```sql
SELECT skey,svalue FROM gonof_common_settings FORCE INDEX (PRIMARY)
 WHERE skey IN ('settings.account.email_change_ttl_hours', …)   -- every scalar key, ascending, as literals
```

- **Primary key equality.** An equality lookup locks only the rows it finds. A range scan would also read, and may
  wait for, the row after each range, which belongs to another family, and under REPEATABLE READ take gap locks. So
  the gate, settings saves and document saves lock disjoint rows and never wait for each other.
- **`FORCE INDEX (PRIMARY)` is required.** The scalar keys are most of the table; without the hint MySQL scans the
  whole table and locks every row. `tests/site_settings.rs` pins the plan and the absence of lock waits.
- **No deadlock.** The gate and the save lock the same keys in ascending order, so they can only queue.
- **One statement per read.** `settings.site.read_only` and `read_only_message` are separate rows; a plain read gets
  both in one statement, and the gate holds its shared locks until commit, so nobody sees half of a save.
- **Order.** The gate is the first lock of its transaction, and settings rows are locked before member rows. Key
  literals are checked to contain only lowercase letters, digits, dots and underscores.
- **Lock rows.** `identity.username_namespace` is locked, before any member lock, by everything that claims a
  username (registration, renames, account deletion). `identity.bootstrap_admin` reads `pending` until the setup
  wizard has created the first administrator, and is locked to serialize the first save of each JSON row.

On SQLite the rewriter drops the locking clauses and the primary key hint; write transactions start with
`BEGIN IMMEDIATE`, and the single writer gives the same guarantees.

## Saving settings

Every settings form saves through `runtime::update` (`router/manage_settings.rs` and the pages in "Keys and groups"):

1. Open a write transaction and lock all scalar rows `FOR UPDATE`; the rows read are also the before-image.
2. Check that the actor is an active administrator.
3. Apply only this page's fields to the values read under the lock. A number, choice or text field that was not
   submitted keeps its stored value, and an unticked checkbox is off, so hidden sub-blocks (the member API block while
   the API is off) are kept.
4. Normalize (trim, normalize the forum address, LF line breaks) and validate by encoding the whole set and decoding
   it with the read rules, so a form error and the error page say the same thing. A failure returns 422 and redisplays
   the form with the submitted values.
5. Write only the rows whose text changed, in one multi-row `INSERT … ON DUPLICATE KEY UPDATE svalue=VALUES(svalue)`.
   Only seeded keys are upserted; `db/source_guards.rs` rejects code that changes `skey` or upserts another column.
6. Record an audit log entry listing the changed keys (`site.settings.save`, or a kind such as `mail.switch`).
7. Commit, then invalidate the settings block (see "Invalidation").

There is no optimistic locking. Two tabs never overwrite each other's fields, but when two administrators save the
same page at once the later save wins without a warning; the audit log shows who changed what.

A new path prefix in `settings.site.url` breaks every old address, so `update` refuses it; the admin area asks for a
confirmation that saves through `update_confirmed`, which fails if the prefix changed again meanwhile.
`gono-forum-cli set-site-url` changes the address when the admin area is unreachable, with the same validation and an
audit entry with `source = 'cli'`. Turning on `settings.domains.enabled` or changing the forum address also creates or
updates the primary domain row in the same transaction.

Documents save on their own rows. The navigation document locks its three rows (`DOCUMENT_LOCK_SQL`, primary key
equality) and must point at existing boards; a policy or mail template upserts its `(kind, language)` row after the
administrator check. Titles are at most 120 characters and bodies at most 60,000 bytes (mail template bodies 16,000),
counted after the form's CRLF line breaks become LF. The edit pages check the byte limit in the browser as well
(`maxlength` counts characters), and their routes accept request bodies up to 256 KiB rather than the 128 KiB of
other admin forms, since URL encoding turns each byte of non-ASCII text into three. Mail templates are not cached:
a mail is rendered with the current template at its first delivery attempt and retries reuse the prepared file
(`mail/prepared/{eventid}.eml` in `[identity] local_directory`), so an invalid template holds back pending mail of its
kind until it is fixed or restored. The prepared file records the event it was written for (event key, recipient
account and queue time); one that does not match the event now holding that id is discarded and the mail rendered
again. Event ids repeat after a database restore, so empty `mail/prepared/` when you restore the database (see "Files
to back up" in [install.md](install.md)).

## Adding a setting

1. Add the field to its group in `runtime/settings.rs`, decode it in `decode` (bounds and the requirement message) and
   encode it in `encode`. `scalar_keys()` comes from `encode`, so reads and locks include the new key.
2. Seed the row in a new migration, MySQL and SQLite versions.
3. Add the form field to the page's field table in `router/manage_settings.rs` (or its own page), with the English
   translation keys first.

Unit tests fail on any key missing from either list, and an integration test checks that the migrations seed exactly
`scalar_keys()`.

## When a change takes effect

- Pages: at once in the process that saved; elsewhere when the broadcast arrives (Redis on), or after the cache
  lifetime if it is lost. Writes check against the gate, so they follow the new value from the next transaction.
- Windows computed from a start time and the current setting change for existing items immediately, in both
  directions: the edit window, the new-member period, the appeal window, the friend-link application rules.
- Periods written as an absolute time when something is issued (invite validity, the old-username protection period)
  only affect what is issued afterwards.
- Display parameters (items per page, card counts) are part of the cache keys of the content they change, so a change
  needs no invalidation. Old links with a page number may land on different content.

## The process cache

`apps/web/src/cache/` has two layers. The process cache (`cache/process.rs`) holds site-wide data with a fixed number
of entries and a known set of write points, one copy per process, with Redis on or off. The handle `GlobalCache` lives
on `AppState` and is passed explicitly to every function that saves cached data.

| Block | Holds | Cleared by |
| --- | --- | --- |
| `settings` | scalar settings, the navigation document and its parsed entries, the layout documents (the site's and every domain's skin choice and seed colors) | settings saves, navigation saves, skin choices, color saves, domain deletion |
| `policy.*` | one slot per policy kind, with every saved language | saving any language of that policy |
| `branding` | brand image bindings | replacing a brand image |
| `credit-rules` | credit reward rules, for display (reward transactions read the rules with locks) | saving a rule |
| `links` | friend links for the home page and the directory page | link writes that change what is shown |
| `pins` | index of site-wide level-2 and level-3 pins | pinning, unpinning, hiding, accepted restorations, moving threads, board saves, merges |
| `moderators` | moderator roster, for display and read-only checks (see [roles.md](roles.md)) | appointments, renames, board deletion, account deletion |
| `honors` | honor group directory | honor group saves |
| `forums` | readable boards, one slot for anonymous visitors and one for members, and the board stylesheets compiled from the members' copy for the skins pages use ([skins.md](skins.md), "Board colors and glyphs") | board saves, ordering, deletion, merges |
| `domains` | the domain table, read only while `settings.domains.enabled` is on | domain saves, the switch, the forum address, board saves and deletion |
| `skins` | every skin of the three sources, checked and compiled ([skins.md](skins.md), "Sources"); built-in skins are checked once per process, imported ones on every load, the instance directory is read again only after the block is cleared; also the colored variants pages use (a skin with seed colors, compiled on first use, at most 32) | skin imports and deletes, "Rescan" at `/manage/skins` |

`board-last-posts` holds the latest thread of each board for the `boards` data source, one slot for the anonymous view
and one for members, each kept while the list version and the boards are the same.

Statistics live here too but are not cleared by writes. Site statistics, scoped statistics (per domain scope, at most
128 slots), the newest members for anonymous visitors and board statistics are keyed by a time segment of their
configured length, plus the site time zone offset (the three kinds of statistics), the site day (board statistics) and
the content revision (newest members). A new segment recomputes; a process miss reads Redis when it is on, and only then computes.

Loading follows the same rules for every block: one request per process loads it while the others wait for the
result; the generation number is compared before storing, so a block cleared during the load does not get a stale
value written back; a failed load is not cached.

Image bytes (`cache/images.rs`), thumbnails and object storage clients are also kept per process, but bound to a
database row and evicted by capacity: callers check the current binding or permission before using an entry. A storage
client is bound to its credentials file: saving new keys for a bucket gives every earlier configuration of that bucket
the new keys and removes the old file, and a client whose file is gone is built again.

## Invalidation

After the write transaction commits, the save function calls `GlobalCache::changed` (or `announce`):

1. Clear the affected blocks in this process (generation plus one, value dropped), so the next request here sees the
   new data. With Redis off that is all.
2. With Redis on, `PUBLISH {namespace}:global-cache <names>` (block names joined by commas). Every process holds a
   subscription and clears those blocks on receipt. The publish counts toward the request's Redis count; the
   subscription connection does not.
3. If publishing fails, the data is saved and this process is cleared, and the request returns 503 saying so, with the
   cache lifetime after which other processes catch up (for a settings save, the one read before the save) and the
   worst case, 86,400 seconds, the maximum of `settings.cache.ttl_seconds`.

Some routes clear after the service returns; the board merge worker clears as it moves threads and only logs a failed
publish. Other rules:

- A block name a process does not recognize (a newer version during a rolling deploy) clears everything.
- **Refresh caches** (Global settings › Search & performance, `GlobalCache::refresh_all`) clears every block and
  statistics slot here and publishes `*`. Admin saves never need it; use it after editing the database by hand, or
  after the CLI changed settings while Redis is off. Tests call `GlobalCache::clear_local`.
- **Subscription.** With Redis on, a process that cannot subscribe at startup refuses to start. A lost subscription
  reconnects in the background (1, 2, 4 … 30 seconds apart), and a successful reconnect clears every block, because
  messages during the outage are lost.
- **Heartbeat.** Every 30 seconds the subscription publishes to its own heartbeat channel. A heartbeat that is not back
  by the next beat, reaches no subscriber or cannot be sent counts as a disconnect, so a half-broken connection is
  replaced within about a minute.

The board stylesheet (`/boards.css?k=<skin>&v=<hash>`) is served from the `forums` block like a skin stylesheet, one per
skin or colored variant (at most 32 kept): `public` for a year with the current hash when a page of the site uses that
skin, `private` for a previewed one, `no-cache` with any other hash. Skin stylesheets (`/skin/<id>.css?v=<hash>`, and
`?c=<seeds>&v=<hash>` for a skin with seed colors) are not cached by the server beyond the `skins` block, which holds them compiled; browsers keep the one with the current hash for a
year (`public` when a page of the site uses the skin, or the skin with those colors, `private` otherwise, so a
previewed skin or colors stay out of shared caches). Colors that no page uses are compiled only for the administrator
previewing them; any other request for them gets the skin's own stylesheet with `no-cache` ([skins.md](skins.md),
"The stylesheet of a skin"). An administrator's preview is a sealed cookie,
not a cache entry, and the pages it changes are those of a signed-in member, which are never cached for others
([skins.md](skins.md), "Preview").

## The Redis cache

The Redis cache (`cache/redis.rs`) holds what is per member, or per page and parameters with no fixed number of
entries. It exists only when `redis.enabled` is true; with Redis off every read misses, every write is dropped and
callers read the database.

| Entry | Holds | Invalidated by | Lifetime |
| --- | --- | --- | --- |
| `thread-list` | anonymous thread lists | list version (content revision and new-post watermark) | list |
| `sidebar-topics` | the hot and latest cards of pages without places (the member area, leaderboards, links, verified members, member search) | list version and a list-lifetime segment | list |
| `forum-pins` | anonymous pin block on a board's first page | list version | list |
| `feed-items` | feed entries, by scope, boards and language | list version | list |
| `floors` | anonymous reply pages | content revision and thread revision | page |
| `rendered-bodies` | rendered post HTML for anonymous visitors | content revision and post versions | page |
| `member-frame` | a member's own board filters and reading choices | deleted on write | member |
| `data-threads` | anonymous thread lists of the data sources ([data.md](data.md)) | list version (and a list-lifetime segment for hot threads) | list |
| `data-members` | the member lists of the data sources (`members.rank`, `members.honors`, `members.verified`), one copy per visitor class without anyone's blocks | list-lifetime segment | list |
| `thread-outline` | the floors behind `thread.outline` | thread revision | page |

- **Keys.** `CacheKey::new(entry, revision, variants)` builds `{namespace}:cache:v2:{tag}:{digest}`: the registry tag
  (with its structure version, such as `home-v9`) and a SHA-256 digest of the tag, revision and variants, so no uid
  appears in a key. Settings and request details that change the content (page sizes, card counts, the time style,
  the path prefix, the language) are part of the variants.
- **Versions.** `gonof_common_content_revision.revision` is raised by `content::copies::bump_site` on structural events
  (title changes, hiding, withdrawal, moves, board changes, renames, account deletion); every update of a thread row
  raises `gonof_forum_thread.cache_revision`, except the view count's ("View counting"); list keys also carry the
  highest post id, so new threads and replies change them without touching the revision row. A key only uses versions
  this request read from the database, never one read from Redis. See [queries.md](queries.md).
- **Anonymous only.** Lists, sidebars, pins, floors and rendered bodies are cached for anonymous visitors only;
  members' views depend on their own blocks and permissions.
- **Member frame.** One key per uid holds the raw choices (board hides and blocks, reading choices, the custom style
  version); site defaults and the board tree are merged in per request. Its lifetime is `settings.cache.ttl_seconds`
  capped at 300 seconds (0: not cached). Saving reading preferences, the custom style or board filters deletes the key
  after commit, and the account deletion worker deletes it before finishing; a failed delete returns a 503 that says
  the change was saved.
- **Times.** Times shown from cached floors and sidebar cards are formatted again for each request, as list rows are,
  so relative times ("5 minutes ago") do not lag by a cache lifetime.
- **Bad values and stampedes.** A value that no longer decodes (an old format after a deploy) is a miss and is
  unlinked. Only one request per key per process fills a missing entry; a failed fill writes nothing.

## The cache registry

Every cache has an entry in `cache/registry.rs`: name, layer (`Process`, `ProcessThenRedis`, `Redis`), invalidation
(`OnWrite`, `Versioned`, `Segmented`, `Binding`), lifetime source, whether it is per member, its process slot names
and its Redis tag. Redis keys can only be built from an entry with a tag, debug builds panic when the process cache
reads an unregistered slot, and the debug panel takes display names from it. Unit tests enforce the rules: per-member
entries live only in Redis and use the member lifetime; Redis entries never use the process or capacity lifetime;
segmented entries have a segment length; only entries checked against a binding are evicted by capacity.

Register a new cache first. Bump the tag's version whenever the cached structure or meaning changes (for
`rendered-bodies`, whenever the rendered HTML changes); old keys then simply expire.

## Cache lifetimes

| Setting | Seed | Range | Used for |
| --- | --- | --- | --- |
| `settings.cache.ttl_seconds` | 3600 | 0–86400 | every process block; the member frame (capped at 300) |
| `settings.cache.page_ttl_seconds` | 60 | 1–3600 | floors and rendered bodies (Redis only) |
| `settings.cache.list_ttl_seconds` | 60 | 1–3600 | lists, sidebar cards, pins, feeds (Redis only) |
| `settings.cache.statistics_seconds` | 600 | 30–86400 | segment length of site and scoped statistics and the newest members |
| `settings.cache.forum_statistics_seconds` | 600 | 30–86400 | segment length of board statistics |

They are edited under Global settings › Search & performance. `settings.cache.ttl_seconds = 0` turns the process cache
off: every request reads the database, statistics included. The lifetime is itself in the settings block, so a process
that missed the broadcast of a lifetime change keeps its old lifetime once more. Each request takes its Redis lifetimes
from its own copy of the settings (`cache::Lifetimes`).

## Redis as a dependency

`[redis]` has four fields: `enabled`, `url` (`redis://` or `rediss://`, overridden by `GONOF_REDIS_URL`),
`namespace` (letters, digits, `:`, `_`, `-`, up to 120 bytes, overridden by `GONOF_REDIS_NAMESPACE`) and
`timeout_milliseconds`, which bounds every connection and command. `enabled` describes the deployment: off for one
process, on for several processes sharing one database. With Redis on it is a hard dependency:

- At startup the process checks the connection, requires Redis 7.0 or later and subscribes to the invalidation
  channel; any failure stops startup.
- A failed command carries the `cache::Unavailable` marker and the request returns 503 (`Retry-After: 3`). Nothing
  falls back to the database or to in-process state, and requests are not probed in advance.
- Background workers check Redis before each round and pause until it is back; `/healthz` returns 503 meanwhile.

## Rate limits

Rate limits (`apps/web/src/rate_limit.rs`) follow the same switch. The limits are settings (`settings.account.*`,
`settings.limit.*`, `settings.links.*`, `settings.api.*`); the windows are constants, named in the keys
(`login_ip_per_5min`). A limit of 0 always rejects.

- **Redis off:** in-process counters, with a fixed window that starts at the first count.
- **Redis on:** a sliding log per key in a sorted set, `{namespace}:ratelimit:{scope}:{key}`, scored with Redis `TIME`
  so machine clocks need not agree; one check is one Lua script and one round trip. Rejections are not recorded,
  several keys are judged in order and the first full one rejects, and a key expires after its window. Limits are
  checked before the database write; a Redis failure returns 503 instead of letting the request through.

Posting intervals and the hourly posting limit are not rate limits: they are counted from the posts table inside the
posting transaction, so they are exact across processes without Redis.

## View counting

Thread views ([queries.md](queries.md), "Views") count once per visitor and thread in 30 minutes. Like rate limits,
the deduplication follows the Redis switch; it is not a cache and is not in the registry (`content/views.rs`).

- **Visitor.** A signed-in member is their uid. Anyone else is the first 16 bytes of SHA-256 over a daily key, the
  client address (as rate limits see it, through a trusted proxy's `X-Real-IP`) and the `User-Agent`. The daily key is
  32 random bytes for the current UTC day. It is never written to the database or the logs: each process keeps the
  current day's key in memory and drops it when the day changes, and with Redis on the first process to need it
  stores it as `{namespace}:view-key:<day>` (`SET … NX GET`), which expires five minutes after its day, so that every
  process derives the same digest. Once a key is gone, the digests made with it cannot be traced back to an address;
  a guest who opens a thread on both sides of midnight UTC counts twice.
- **Redis on:** `SET {namespace}:view:<tid>:<visitor> 1 NX EX 1800`, where the visitor part is `u<uid>` or `g` and
  the digest in hex. A key that already exists means the view does not count. One command per counted request (plus
  one a day for the key), shared by every process. A failed command answers 503, as every Redis failure does.
- **Redis off:** each process remembers the pairs it has seen in a table of at most 100,000 entries, each kept for 30
  minutes; when it is full, the oldest pairs are forgotten first. A site that runs several processes without Redis
  counts a visitor once in each process that serves them.
- **Writing.** Counted views wait in the process's buffer and are written every minute and at a normal shutdown
  ([queries.md](queries.md), "Views"). The writing task only needs the database, so unlike the polling workers it
  does not pause while Redis is unreachable. It takes no write gate, and read-only mode does not stop it: a view is
  bookkeeping, not a change a member makes.

## What may be cached

- Only data that can be rebuilt from the database.
- Member-private data only in Redis, with the uid in the key; the registry enforces this.
- Never the result of a query that folds an authentication check into its SQL: that would cache the outcome, and
  frozen members or rotated credentials would keep passing.
- Never the session lookup, the write gate, locking reads and permission rechecks inside transactions, or the current
  values shown by admin edit pages.

## Profiling

- **Debug panel** (Tools & maintenance › Maintenance, `settings.debug.panel`): administrators see every SQL statement
  and Redis command of the page with timings and `EXPLAIN`; its "In-process cache" section shows which blocks were
  hits, loaded or bypassed. The list starts with the request: reading the switch itself and resolving the site (the
  settings and the domain table) are on it when they go to the database.
- **Footer line** (Appearance › Display, `settings.display.footer_metrics`):
  a lightning icon, then `19.1 ms · MySQL 11 · Redis 23` (`SQLite` on SQLite sites): the server time and the queries
  the request itself sent up to the footer, counted from the start of the request, site resolution included. Without
  Redis the Redis count is left out, as is the panel's Redis section.
- To see every query, set `settings.cache.ttl_seconds` to 0; to count without Redis, also set `[redis] enabled =
  false`. Debug builds give accurate counts but inflated times; measure times with `--release`.
