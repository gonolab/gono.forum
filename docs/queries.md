# Queries

How thread lists (home page, forum pages, search, sidebars, the pinned block, feeds) and the thread page read the
database, and the copied columns and count tables that keep those reads simple. The general SQL rules are in
[architecture.md](architecture.md), "Data"; the cache layers mentioned here are described in
[settings-and-caching.md](settings-and-caching.md).

In the code and the schema a board is a *forum* (`gonof_forum_forum`, `fid`). Forums have two levels: top-level
forums and their child forums.

## Principles

- A list is one single-table statement on `gonof_forum_thread`, forced onto a narrow index that starts with the
  scope's forum id (nothing for the whole site), then `displayorder`, then the sort time. List, total and sidebar
  statements have no `UNION`, no derived table and no subquery; the capped count for search is the one exception.
- Everything that depends on the visitor (signed in or not, readable forums, blocked members) is worked out in Rust
  and written into the statement as equalities and `IN` lists, which the index's trailing columns (`visibility`,
  `fid`, `authorid`) evaluate without reading the table row.
- Page totals come from count tables maintained by the write paths, and lists stop at 1,000 pages.
- What a list row shows is on the thread row: the author name, the last replier and the top-level forum are copies.
  Forum names and URLs come from the process forum cache by `fid`, avatars by uid (`/avatar/<uid>`), so list
  statements join nothing.
- Counters the thread page shows live on the rows they count and change in the transaction that writes the detail
  rows.
- These rules are for read paths. Statements that recheck permissions under a lock inside write transactions keep
  the joined form (`content::PUBLIC_THREAD`, `content::readable_thread_sql`, `organization::forum_readable_sql`,
  which checks the parent forum with `EXISTS`).

## Whether a thread is listed

`displayorder` is both the pin level and the listing switch. Lists read `displayorder >= 0`.

| Value | Meaning | Written by |
| --- | --- | --- |
| 3, 2, 1 | pinned: on every forum page, in the forum family, in its own forum | pin actions in `content/edits.rs` |
| 0 | listed, not pinned | posting, unpinning, unhiding |
| −1 (`copies::HIDDEN_ORDER`) | the first post is hidden by moderation | `copies::sync_first_hidden` |
| −2 (`copies::RESERVED_ORDER`) | reserved state: not published, private, or read-restricted (`publication_state`, `visibility`, `readperm`); no write path sets these states yet | `copies::sync_first_hidden`, `copies::rebuild_listing` |

The pin actions only accept a thread whose current value is 0–3, so a hidden thread cannot be pinned. Unhiding sets
the value back to 0; the earlier pin level is not kept.

- **Thread visibility.** `visibility` is `public` or `members`; anonymous visitors get `t.visibility = 'public'`.
- **Forum readability is not on the thread row.** Readable forums come from the process forum cache (see "Process
  caches") and are written as `t.fid IN (…)`. Disabling a forum or changing its visibility rewrites no thread rows
  and no counts; it takes effect when the forum cache is cleared.
- **Withdrawn threads stay listed.** A withdrawn thread (`deleted_at`, always written in the same transaction as the
  first post's `deleted_at`) stays in lists and totals with its title replaced by a placeholder. Search, feeds, the
  sitemap and the statistics add `deleted_at = 0`.
- **Single threads.** `content::thread_visible_sql` (`t.fid IN (readable) AND t.displayorder >= 0 AND
  t.visibility …`) is the readability condition of the thread page, floor links, thread cards, member pages,
  rankings, statistics and the sitemap, so a thread with a negative `displayorder` is not found there either.
  Statements that already join the forum table (favorite, follow and notification lists) use
  `content::thread_visible_by_forum_sql`.
- **Posts.** Outside the thread page, a post is visible when `content::post_visible` holds: `invisible = 0 AND
  deleted_at = 0 AND erased_at = 0 AND root_invisible = 0`. `root_invisible` copies the parent reply's hidden flag
  onto nested replies, so the parent is not looked up.

## List indexes

| Index | Columns | Used for |
| --- | --- | --- |
| `gonof_thread_global_latest`, `_created` | `displayorder, lastpost` or `dateline`, `tid, visibility, fid, authorid` | home page, search, sidebar latest, the pin index, site feeds |
| `gonof_thread_family_latest`, `_created` | `family_fid, displayorder, …, tid, visibility, fid, authorid` | top-level forum pages (with their child forums), their feeds |
| `gonof_thread_forum_latest`, `_created` | `fid, displayorder, …, tid, visibility, authorid` | child forum pages, their feeds, forum statistics |
| `gonof_thread_category_*`, `gonof_thread_family_category_*`, `gonof_thread_forum_category_*` | as above, with `category` after the scope column | category filter |
| `gonof_thread_featured` | `digest, displayorder, lastpost, tid, visibility, family_fid, fid, authorid` | highlighted threads |
| `gonof_thread_global_active` | `lastpost, displayorder, replies, tid, visibility, family_fid, fid, authorid` | sidebar hot threads |

`*_latest` sorts by last reply (`lastpost`), `*_created` by post time (`dateline`). `ListQuery::index` in `content.rs`
picks the index from scope, category and sort, and the statement names it with `FORCE INDEX`; on SQLite the rewriter
turns that into `INDEXED BY gonof_forum_thread__<name>`, so both backends read the same index. The
`gonof_thread_forum_*` indexes start with `fid` and also serve the forum foreign key; there is no separate `fid` index.

A member's threads and replies (the personal space lists, their recent posts and the member's own list) are read from
the post table instead, forced onto `gonof_post_author_created (authorid, first, dateline)` and joined to the thread
and forum by primary key. They sort by `dateline DESC, tid DESC, position DESC`: ties go by the primary key that every
secondary index ends with, so a page with `first` fixed is read in index order. Without the hint MySQL starts from
the threads of every readable forum when there are many of them.

## Scopes and filters

`content::ListScope` is the forum scope:

- `All`: the home page, search and the member area's sidebar.
- `Family(fid)`: a top-level forum page, its own threads and its child forums' in one range, `t.family_fid = ?`.
- `Forum(fid)`: a child forum page, `t.fid = ?`.

`ListScope::of_forum` picks between the last two. `content::ListViewer` carries what depends on the visitor:

- `uid`: `None` adds `t.visibility = 'public'`.
- `forums`: forum ids this visitor may see in the scope, written as `t.fid IN (…)` (not for `Forum`). For the home
  page and search, the readable forums of this site's domain minus those the member hid from the timeline or blocked
  (`SiteFrame::listable_forums`); for a top-level forum page, the forum and its readable child forums the member has
  not blocked. When it is empty, no statement is sent.
- `blocked_uids`: members the viewer blocked, read once per page by `content::blocked_authors` (a primary-key range
  on `gonof_home_member_block`) and shared by the list, the total, the sidebar and the pinned block. Written as
  `(t.authorid IS NULL OR t.authorid NOT IN (…))`, and left out when empty.

Forum and member ids come from the database as integers and are written into the SQL text; other values are bound.
A member's page of a top-level forum:

```sql
SELECT t.tid, t.fid, t.authorid, … FROM gonof_forum_thread t FORCE INDEX (gonof_thread_family_latest)
WHERE 1=1 AND t.family_fid = ? AND t.displayorder >= 0 AND t.fid IN (3,7,8)
  AND (t.authorid IS NULL OR t.authorid NOT IN (42))
ORDER BY t.displayorder DESC, t.lastpost DESC, t.tid DESC LIMIT ? OFFSET ?
```

- **Sorts** (`urls::Sort`): `Latest` by `lastpost`, `Created` by `dateline`, `Pinned` with `displayorder > 0`,
  `Featured` with `digest = 1` on `gonof_thread_featured`. All order by `displayorder DESC` first, so pinned threads
  of every level come first.
- **Category** (0–5, the thread's activity kind) adds `t.category = ?` and combines only with `Latest` and
  `Created`; other combinations redirect.
- **Search** adds `t.deleted_at = 0 AND LOWER(t.subject) LIKE LOWER(?) ESCAPE '!'`, which needs the table row.
- **Thumbnails**: `thread_images::attach` reads the page's thumbnails in one `IN` query on
  `gonof_forum_thread_image`, skipping withdrawn and erased threads. `thread_images::rewrite` rewrites a thread's set
  when the thread is posted or its first post edited; erasing the first post deletes it.

## Page totals and the page cap

`content::list_scope_for_viewer` computes the total first; it is used only for the page count.

- **Latest, created and category lists**: `counted_threads` sums `gonof_forum_thread_count.threads` over the
  visitor's forums (the scope forum for `Forum`), only the `visibility = 'public'` rows for anonymous visitors, and
  only the category's rows when filtering. When the member blocks people, a single-table count of those members'
  listed threads on the `authorid` index is subtracted.
- **Pinned and highlighted**: `COUNT(*)` over their own index range; few threads match.
- **Search**: `SELECT COUNT(*) FROM (SELECT 1 … LIMIT ?) capped`, reading at most the cap.

`content::MAX_LIST_PAGES` is 1,000, a constant rather than a setting. The total is capped at 1,000 × threads per page;
a page past the last one returns no rows and the handler answers 404. Paging uses `OFFSET`, whose depth the cap
bounds.

## Sidebars, the pinned block and feeds

- **Hot threads** (`content::hot_topics`): `gonof_thread_global_active` takes the range `lastpost >= now − hot days`,
  filters `displayorder >= 0`, `replies > 0`, the scope and the viewer in the index, then sorts by `replies DESC,
  tid DESC`, a filesort over the threads active in the window (bounded by the display settings).
- **Latest threads** (`content::latest_topics`): the first N of the `*_created` list, without a total.
- **Pinned block** (`content/pins.rs`): on page 1 of a forum page, level 2 pins of the same family and level 3 pins
  of the whole site, shown above the list and outside its total; pins the list already contains are skipped. The pin
  index (`tid, fid, displayorder` with `displayorder IN (2,3)`, read from `gonof_thread_global_latest`) is a process
  cache, and `Scope::applicable` picks the thread ids in code; when none apply, nothing is queried. Otherwise the rows
  are read by primary key and every condition (level, forum, the viewer's forums, visibility and blocks) is checked
  again in that statement, so a stale index can only make a new pin appear late.
- **Feeds** (`content/feeds.rs`): two single-table statements on the scope's `*_created` index, joined to the first
  post for the body: `displayorder = 0` in index order, and `displayorder > 0` (a handful, sorted). The code merges
  them by post time and keeps the first N. Withdrawn threads are left out; personal feeds add the owner's blocks,
  excluded forums and, when chosen, members-only threads.
- **Statistics** (`content::statistics_in`, `content::forum_statistics_at`) are aggregate counts over public content,
  cached by time period, not count-table reads; their thread counts leave out withdrawn threads.

## Data sources

The data sources ([data.md](data.md)) read through the same paths as lists and pages, so most of their statements
are listed above; the new ones are below. Plans are from MySQL 8.4 and 5.7 on the development fixture.

- **`threads`** (`content::sources::threads`): the list statements above without the total, `LIMIT` 1 to 20 and no
  `OFFSET`: `latest`, `pinned` and a category on the scope's `*_latest` (or `*_category_latest`) index, `created` on
  `*_created`, `featured` on `gonof_thread_featured`, `hot` on `gonof_thread_global_active` as the sidebar's hot
  threads (a window of 1, 3, 7 or 30 days, the 30-day window being the largest sort, as the sidebar's
  `settings.display.hot_days` allows today; snapshots like the leaderboards' would replace it on very busy sites).
  The list thumbnails are one more `IN` statement when the site shows them. Anonymous results are cached in Redis
  (`data-threads`, keyed by the list version).
- **`boards` with `last_post`** (`content::sources::board_last_posts`): for each board, one statement per pin level,
  3 to 0, an index seek to the newest row of that level, the newest of the four kept in code:

  ```sql
  SELECT … FROM gonof_forum_thread t FORCE INDEX (gonof_thread_family_latest)   -- a child board: gonof_thread_forum_latest, t.fid = ?
  WHERE t.family_fid = ? AND t.displayorder = ? AND t.visibility = 'public' AND t.fid IN (…)
  ORDER BY t.lastpost DESC, t.tid DESC LIMIT 1
  ```

  `ref` on the index with the scope and the pin level as equalities, read backwards, `Using index` for the
  conditions (8.4: `Backward index scan; Using index`). The result is kept in the process cache per view (anonymous,
  members) until the list version changes (`board-last-posts`); a page with the boards' block reads at most four
  statements per board on a miss.
- **`thread.outline`** (`content::sources`): outside the thread's own page, `outline_head` reads the thread row by
  primary key under `content::thread_visible_sql` (`const`); `outline_floors` reads `(position, dateline)` on the post
  primary key, `FORCE INDEX (PRIMARY)` (`range` on `PRIMARY`; without the hint 8.4 takes `gonof_post_root_position`
  and sorts): every floor up to 5,000, a sample of at most 201 floors by `position IN (…)` past that. Cached in Redis
  by thread and `cache_revision` (`thread-outline`).
- **`members.honors`** (`honors::listed_holders`): `gonof_member_honor_group (honor_id, granted_at, uid)` with the group
  as an equality, read backwards (`ref`), and the member by primary key (`eq_ref`), at most the limit plus up to 20
  members the viewer blocked, who are removed in code.
- **`members.verified`** (`directory::listed`): `gonof_verification_directory (category, granted_at, uid)` with the
  category as an equality (`ref`), the member by primary key (`eq_ref`), as the verified members page reads it but
  without the viewer's blocks, which are removed in code.
- **`members.rank`** (`ranks::read_listed`): the leaderboard page's two statements without the viewer's blocks.
- **`me.card`**: the growth account by primary key, besides the member card's summary.

`members.rank`, `members.honors` and `members.verified` keep one copy per visitor class (anonymous, signed in) in
Redis (`data-members`, segmented by the list lifetime) and remove each viewer's blocks in code, so a block costs a
Redis read for every visitor. `members.recent`, `moderators`, `site.stats` and `site.links` read process caches only.

## Thread page

`topic_response` in `router.rs` makes these reads:

- **Thread**: `content::thread_for_viewer`, one statement: the thread row, the first post by its unique `first_tid`
  and the author's member row by uid, under `content::thread_visible_sql` with the page frame's readable forums. The
  counters are columns: `favorites`, `tips` and `tip_total` on the thread, `downvotes` on the first post, and
  `replies` (flat view) or `root_replies` (nested view) for the page count. The author's `freeze` and
  `account_state` decide whether tipping is offered.
- **Floors**: `content::load_replies` reads one range on the post primary key `(tid, position)` in flat view, or on
  `gonof_post_root_position (tid, root_position)` in nested view; each row joins its parent, its reply target and
  their authors by primary key. Posts are not filtered in SQL: hidden, withdrawn and erased ones keep their floor and
  render as placeholders.
- **Nested replies**: one more statement on `gonof_post_sub_position (root_pid, sub_position)`: the first page of
  every top-level reply on the page as `root_pid IN (…) AND sub_position < 1 + per page`, plus one range for the reply
  whose nested replies are being paged. Their page counts come from `sub_replies_count`.
- **Blocks**: `content::apply_blocks` reads which of the page's authors the reader blocked, from one table.
- **Moderator check**: no query. Role, frozen flag and account state come from the member row that authentication
  read; appointments come from the moderator roster (`identity::manages_forum`, see "Process caches").
- **Activity card**: `activities::card_for_thread` reads nothing for an ordinary thread (category 0); otherwise the
  activity row and the detail table of its kind. The bounty accept button reuses that row.
- **Reader state** (members only): `interaction::viewer_thread_state` reads favorite, follow, blocked by the thread
  author, downvote and tipped in one statement anchored on the thread's primary key, each join a primary- or
  unique-key lookup:

  ```sql
  SELECT fav.tid AS favorited, fol.tid AS followed, blk.uid AS blocked_by_author, dv.uid AS downvoted, tip.id AS tipped
  FROM gonof_forum_thread t
  LEFT JOIN gonof_home_favorite fav ON fav.uid=? AND fav.tid=t.tid
  LEFT JOIN gonof_home_thread_follow fol ON fol.uid=? AND fol.tid=t.tid
  LEFT JOIN gonof_home_member_block blk ON blk.uid=t.authorid AND blk.blocked_uid=?
  LEFT JOIN gonof_forum_post_downvote dv ON dv.pid=? AND dv.uid=?
  LEFT JOIN gonof_common_credit_ledger tip ON tip.event_key=?
  WHERE t.tid=?
  ```

- **Likes**: `interaction::reactions` reads `reactions` for the first post and the page's posts by primary key in
  batches; for a member the same statement joins their own like row. Highlighted replies are ranked from that result
  in code.
- **Authors**: `growth::page_authors` reads the page's authors by member primary key with their growth account,
  frozen flag and account state. Verification badges (`directory::badges_of_active`) and honor grants
  (`honors::badges_of_active`) are single-table reads for active authors only, matched against the cached honor group
  directory; expired and disabled entries are dropped in code.
- **Tips**: `activities::tip_summary` takes count and total from the thread row and reads the recent tippers only
  when `tips > 0`.

For anonymous visitors with Redis enabled, floors and rendered bodies come from Redis, keyed by the site-wide content
version and the thread's `cache_revision` (see "Cache versions"); members always read the database.

## Process caches

These process caches replace per-request queries; [settings-and-caching.md](settings-and-caching.md) describes their
lifetimes, clearing and broadcasting.

- **Readable forums** (`GlobalCache::forums`, one copy for anonymous visitors and one for members):
  `organization::forums` reads the whole forum table once and keeps a forum when it and its parent are enabled and
  visible to the visitor, judging the parent in code. The page frame narrows it to the site's domain; the thread page
  uses the full readable set and redirects threads outside the domain.
- **Moderator roster** (`GlobalCache::moderators`): read-only checks only, namely the thread page's moderation entries
  and moderators downloading attachments of hidden or withdrawn posts (`media.rs`). A revoked appointment may still
  apply there for up to one cache lifetime; moderation actions recheck the appointment row with a locking read
  (`organization::moderator_locked`). See [roles.md](roles.md), "Moderators and boards".
- **Pin index** (`GlobalCache::pins`) and **honor group directory** (`GlobalCache::honors`), described above.

## Copy columns

| Column | Holds | Written by |
| --- | --- | --- |
| `gonof_forum_thread.author` | the author's username; for a deleted account a fixed marker (`copies::DELETED_AUTHOR`, translated when shown by `copies::shown_name`) | the INSERT (`copies::ThreadCopies`); `copies::member_changed` on renames and account deletion |
| `last_reply_pid`, `last_reply_author`, `last_reply_summary`, `lastpost`, `valid_replies` | the latest visible reply | the reply statement in `content/writes.rs`; `content::refresh_effective_replies` after moderation; `copies::member_changed` for the name |
| `last_reply_uid` | the uid of the latest visible reply's author; `NULL` without a reply, for a reply without an author and once that account is deleted (indexed, foreign key to the member) | the same statements as `last_reply_author`, in the same `UPDATE`: the reply statement, `content::refresh_effective_replies`, and `copies::member_changed`, which finds a member's threads by `WHERE last_reply_uid = ?` on its index and writes `NULL` on account deletion; migration 0004 filled it from `last_reply_pid` |
| `family_fid` | the parent forum's id, or the forum's own id for a top-level forum | the INSERT (`copies::ForumCopies`); `copies::move_thread`, in the statement that changes `fid`; `copies::resync_forum` when a forum gets another parent (`copies::forum_saved`) |
| `displayorder` (negative values) | first post hidden, or a reserved state | `copies::sync_first_hidden` |
| `gonof_forum_post.root_invisible` | the parent reply's `invisible` on nested replies; 0 on top-level replies and first posts | `copies::sync_root_invisible` when a top-level reply is hidden or unhidden |

`replies`, `root_replies` and `maxposition` on the thread and `sub_replies_count` on the parent reply are incremented
by the reply statements. `copies::update_keys` is the pattern for rewriting many rows: read the keys inside the
transaction, then update by key in chunks of 500, one statement for both backends.

## Count tables

| Table | Holds | Changed by |
| --- | --- | --- |
| `gonof_forum_thread_count (fid, visibility, category, threads)` | listed threads (`displayorder >= 0`) per forum, thread visibility and category | `copies::count_thread`, an upsert by delta: posting a thread (+1); `copies::move_thread` for a listed thread (moving and merging forums use it); editing the first post's visibility moves it between rows; `copies::sync_first_hidden` when hiding or unhiding takes it out or puts it back. An empty forum's rows are deleted with the forum (`organization.rs`). |
| `gonof_common_member_count (uid, threads, replies, digests)` | the member's own threads and replies that are neither hidden nor withdrawn, and the highlighted threads among them | `copies::count_member` and `copies::post_counted_changed`: posting a thread or reply; hiding, unhiding, withdrawing and restoring (`content/edits.rs`); erasure (`privacy/worker.rs`); highlighting or unhighlighting while the first post counts. Posts without an author are not counted. |

The member count is read by the profile page (its numbers and the page totals of its threads and replies tabs) and the
member card in the home sidebar. Every visitor sees the same number, except that a visitor who blocked the member sees
0; entries the visitor cannot read are still left out of the pages, so the last pages may be short.

## Counter columns

| Column | Kept by |
| --- | --- |
| `gonof_forum_post.reactions`, `downvotes` | `counters::recount_reactions`, `counters::recount_downvotes` |
| `gonof_forum_thread.favorites` | `counters::recount_favorites` |
| `gonof_forum_thread.tips`, `tip_total` | `counters::add_tip` |

All in `interaction/counters.rs`, in the transaction that writes the detail rows; the caller holds an exclusive lock
on the thread row (`interaction::lock_target`, or the tip transaction), so writes to one thread queue up.

- **Likes, downvotes, favorites**: after inserting or deleting a detail row, the count is redone from the detail rows
  with a locking read (`LOCK IN SHARE MODE`, dropped on SQLite, where the single writer serializes) and written back.
  It does not rely on upsert affected-row counts: the connection reports found rows, so an insert and an existing row
  both return 1. A like removes the member's downvote on the same post and the reverse, each recounted.
- **Tips** only grow: after the payment and receipt ledger rows are written, `tips + 1` and `tip_total + amount`.
- **Account deletion** leaves the member's likes, downvotes and favorites in place, so these counts do not change.

## Cache versions

Every `UPDATE` of a thread row increments `cache_revision`, counter updates included (`copies::touch_thread` does it
when only a post changed); `copies::bump_site` raises the site-wide content version on structural events only. List
keys carry `content::list_revision`, the site-wide version plus `MAX(pid)` of the post table, so new threads and
replies change them without touching the version row. The keys are described in
[settings-and-caching.md](settings-and-caching.md), "The Redis cache".

## Leaderboards

The 12 boards (growth, threads, replies and likes, each for 24 hours, 30 days and all time) are snapshots, not live
queries. A background task in every process aggregates them every `settings.ranks.refresh_seconds` and replaces a
board's rows in `gonof_rank_snapshot` (one row per place) in a short transaction. `gonof_rank_board` keeps one row per
board and period with the time it was last computed, written in the same transaction, so a board that came out empty
still has its time: its page says nobody is on it yet rather than that it is being built. A board computed less than
half an interval ago, by this process or another, is skipped. Reading a board filters the snapshot by each member's
current state (frozen, deleted, private space, blocked by the viewer) and closes up the places.

## Rules enforced by the source scan

`db/source_guards.rs` (`content_copies_versions_and_identities_change_only_through_their_code`) scans the production
source and fails when:

| Statement | Fails when |
| --- | --- |
| `INSERT INTO gonof_forum_thread` | outside `content/writes.rs` and `content/seed.rs`, or without `author` and `family_fid` (which has no default) |
| `UPDATE gonof_forum_thread` | without `cache_revision`; `fid` without `family_fid`; `author` or `family_fid` outside `content/copies.rs`; `displayorder` outside `copies.rs` and `content/edits.rs`; `last_reply_author` or `last_reply_uid` outside `content/writes.rs`, `content.rs` and `copies.rs`; `favorites`, `tips`, `tip_total` outside `interaction/counters.rs`; `category` or `authorid` anywhere |
| `UPDATE gonof_forum_post` | `root_invisible` outside `copies.rs`; `reactions`, `downvotes` outside `counters.rs` |
| the two count tables | written outside `copies.rs`, apart from deleting an empty forum's rows in `organization.rs` |
| like, downvote and favorite rows | written outside `interaction.rs`, `interaction/counters.rs` and `growth.rs` |
| thread and post rows | deleted anywhere |

## Last online

Requests do not write last-online times. `Identity::touch_online` writes the session table `gonof_common_session` at
most once a minute per sign-in and process. The per-minute cleanup (`Identity::cleanup_online`) takes up to 1,000
sessions idle for more than 15 minutes, writes their latest activity into `gonof_common_member_presence` for active
accounts (with `GREATEST`, so it only moves forward), then deletes them. The profile page (`content::member_presence`)
reads the session table first and the presence table only when the member is not online, taking the later time. A data
export flushes the member's own session time first (`Identity::flush_presence`).

On MySQL the session table uses the MEMORY engine: a database restart empties it, and last-online times from up to
15 minutes before the restart are lost.

## Checking consistency

- `apps/web/tests/consistency/content.sql` holds read-only checks that must all be 0: the copies (the last replier's
  name and uid against the author of `last_reply_pid` among them), `displayorder`
  against the first post's hidden flag and the reserved states, thread and first-post withdrawal, `root_invisible`,
  both count tables, the counter columns against the detail rows and the tip ledger, reply counts and post shapes.
  `test_support::content_drift` runs it (MySQL only); `tests/thread_copies.rs` runs it after every test.
- A test that writes threads or posts with raw SQL calls `test_support::refresh_listing` (`copies::rebuild_listing`,
  also used by the development fixture import, plus `counters::rebuild`) or the narrower `resync_thread`,
  `sync_forum_copies` and `sync_member_copies`; after changing process-cached data directly it calls
  `GlobalCache::clear_local`.
- `tests/thread_list_queries.rs` compares every scope, sort, category, search and viewer page by page, and the totals,
  with a reference computed in code from the raw columns; it also asserts no `UNION` or nested query (search's count
  aside), no filesort on the thread table (hot threads aside) and the 1,000-page cap.
- `tests/thread_page_queries.rs` checks the counters against the detail rows and that, in the usual state, the thread
  page sends no `UNION`, subquery or `EXISTS`, no counting over likes, favorites, downvotes or tips, no activity or
  moderator table query for an ordinary thread, one reader-state statement for a member, and at most 12 statements
  for a member and 8 for an anonymous visitor.
