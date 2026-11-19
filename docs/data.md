# Data sources

Data sources are named, read-only views of forum data. Each one returns the same records to three outputs:

- **Blocks.** A layout's blocks render a source with one of its views, on the server, so a page shows them without
  JavaScript ([skins.md](skins.md), "Places and blocks").
- **The page data block.** Every page lists the records of its data-source blocks in the `blocks` field of
  `#gonof-page` ([skins.md](skins.md), "Page data").
- **`GET /data/v1/<source>`.** The same records as JSON, for scripts on the site's own pages.

The personal sources `me.card`, `me.counts` and `me.checkin` exist only in `/data/v1`: they have no views, so no block
and no page data block carries them.

## Rules

1. **Closed parameters.** Every parameter is a value from a fixed list or a bounded number, and every combination is
   read along an index listed in [queries.md](queries.md), "Data sources". Lists are capped (`limit` 1 to 20) and are
   not paged. A new combination needs code and an index review; nothing in the admin area can compose one.
2. **The visitor's view.** Every source reads as the visitor: this site's boards they may read (the domain's boards on
   a site with several domains), minus the boards they hid from the timeline or blocked, minus the members they
   blocked, and for anonymous visitors only public threads (`content::ListViewer`). A board, thread or honor group the
   visitor cannot read is "not found". Results without anything personal are cached; permission checks never are.
3. **Only what pages show.** Records hold only fields some public page shows anyway.
4. **Data, not wording.** Records carry raw values; the only text is what pages show (names, titles, the times'
   `text` and `title`), and caches that hold formatted text are keyed by the language.
5. **A budget per page.** A page renders at most 10 data-source blocks; each is answered from a cache or by bounded
   statements (listed in [queries.md](queries.md)), and the debug panel lists every block with its time.
6. **View types.** Records are types written for this purpose (`data/records.rs`), never internal structures: no
   link notes or applicants, no thread withdrawal, version or erasure markers. A test checks every source's fields
   against the tables below.
7. **Fields only grow.** Every field is always present, `null` where it does not apply. Removing or changing a field
   means a new `v`. Every source is registered in `cache/registry.rs` (`SOURCES`) with the caches it reads.

## Common shapes

The envelope:

```json
{"v": 1, "source": "threads", "params": {"scope": "site", "sort": "hot", "days": 7, "category": null, "limit": 10},
 "items": [], "generated_at": "2026-11-16T05:27:55Z"}
```

`params` are the parameters as the source understood them: every key, defaults filled in, `current` replaced by the
board or thread it meant on the page. `items` is always a list; sources that describe one thing return one item.
`members.rank` adds `computed_at`, a time or `null` while the board has not been computed yet.

| Shape | Fields |
| --- | --- |
| Time | `{"at": "2026-11-16T05:27:55Z", "text": "3 hours ago", "title": "Nov 16, 2026, 13:27"}`: the instant in UTC to the second, the text a list shows (relative when the site shows relative times) and the full date. |
| Member | `{"uid", "name", "href", "avatar": {"src", "initials", "tone"}}`: `src` is the avatar by uid (transparent when the member has none, so a tile with `initials` in color `tone`, 0 to 5, shows through). For a deleted or unknown author `uid`, `href` and `src` are `null` and `name` is "Deleted user" in the page's language. |
| Board | `{"fid", "name", "href", "parent_fid"}`. |

Addresses (`href`, `src`) are relative to the site and carry its path prefix. Counts are integers. A thread's `kind`
is `bounty`, `redpacket`, `lottery`, `poll`, `news` or `null`.

## Sources

### `threads`

| Parameter | Values |
| --- | --- |
| `scope` | `site`, `board:<fid>` (a top-level board with its child boards), `current` (blocks only: the page's board, the site on the home page). Default: `current` in a block, `site` in `/data/v1`. |
| `sort` | `latest` (last reply), `created`, `hot` (most replies among threads with a reply in the window), `featured`, `pinned`. Default `latest`. |
| `days` | Only with `hot`: `site` (`settings.display.hot_days`), 1, 3, 7 or 30. Default `site`. |
| `category` | Only with `latest` and `created`: a `kind`. |
| `limit` | 1 to 20, or `site`: `settings.display.hot_count` for `hot`, `latest_count` for the others. Default 10. |

Pinned threads of every level come first in `latest`, `created`, `featured` and `pinned`, as in lists.

| Field | Value |
| --- | --- |
| `tid` | The thread id. |
| `title` | The title; withdrawn and erased threads show their placeholder, as in lists. |
| `href` | The thread's first page in the visitor's reading view (`?view=flat` for a member who reads flat), as list titles link it. |
| `board` | The thread's own board (a Board), or `null` when the visitor cannot see it. |
| `kind` | The thread's kind or `null`. |
| `pin` | The pin level, 0 to 3. |
| `featured` | Highlighted. |
| `closed` | Locked. |
| `members_only` | The thread or its board is for signed-in members. |
| `author` | The author (a Member). |
| `created` | When it was posted (a Time). |
| `last_activity` | The last reply, or the post when there is none (a Time). |
| `replies` | Live replies. |
| `favorites` | How many members favorited it. |
| `tips` | How many tips it received. |
| `last_reply` | `{"pid", "member", "href", "summary"}` for the last live reply (`member` from the thread row's `last_reply_uid` and `last_reply_author`, `href` its permanent address `/posts/<pid>`, `summary` its first 120 characters as plain text), or `null`. |
| `pages` | Pages of the thread in the visitor's view (flat or nested). |
| `max_position` | The highest floor number, for comparing with the floor a reader saw last. |
| `views` | Views, approximate: each member or guest counts once per thread in 30 minutes, and new views are written once a minute ([queries.md](queries.md), "Views"). Anonymous results are cached for the list lifetime, so the number can lag by that much as well. |
| `thumbs` | `[{"src", "width", "height"}]`: the list thumbnails, as many as `settings.display.list_thumbnails`. |

### `boards`

| Parameter | Values |
| --- | --- |
| `parent` | `top` (top-level boards) or a top-level board's `fid` (its child boards). Default `top`. |
| `counts` | `true` adds thread counts. Default `false`. |
| `last_post` | `true` adds each board's latest thread. Default `false`. |

Boards come from the forum cache in their display order; boards the member blocked are left out. A home page that
opens with the board directory reads this source with `counts` and `last_post` ([skins.md](skins.md), "Board
directory home").

| Field | Value |
| --- | --- |
| `fid` | The board id. |
| `slug` | The board's address segment. |
| `name` | The name. |
| `href` | The board page. |
| `description` | The description, plain text. |
| `parent_fid` | The parent board, or `null`. |
| `icon` | `{"src"}` for an uploaded board icon, or `null`. |
| `color` | `#rrggbb` or `null` ([skins.md](skins.md), "Board colors and glyphs"). |
| `glyph` | A built-in glyph name or `null`. |
| `access` | `public` or `members`. |
| `counts` | With `counts`: `{"threads", "today"}` (threads, and posts today, of the board with its child boards), for boards anonymous visitors can read; `null` otherwise, since board statistics count public content only. |
| `last_post` | With `last_post`: `{"tid", "title", "href", "at", "member", "summary", "max_position"}` for the board's newest listed thread by last activity, its child boards' included (`href` the last reply's permanent address `/posts/<pid>`, or for a thread without replies its first page in the visitor's reading view; `member` its last poster, `summary` the last reply's excerpt or `null` for a thread without replies, `max_position` its highest floor number), or `null`; also `null` when the visitor blocked that poster or the child board the thread is in. |
| `children` | The child boards the visitor sees (Boards). |

### `members.recent`

Parameter `limit`. The newest members, newest first.

| Field | Value |
| --- | --- |
| `uid`, `name`, `href`, `avatar` | The member. |
| `joined` | When they signed up (a Time). |

### `members.rank`

Parameters `board` (`growth`, `threads`, `replies`, `likes`; default `growth`), `period` (`24h`, `30d`, `all`;
default `30d`) and `limit`. The leaderboard snapshot ([queries.md](queries.md), "Leaderboards"), with the members
the leaderboard page leaves out left out here too and places closed up; the envelope's `computed_at` says when the
board was computed.

| Field | Value |
| --- | --- |
| `position` | The place, from 1. |
| `uid`, `name`, `href`, `avatar` | The member. |
| `score` | The board's score. |

### `members.honors`

Parameters `honor` (an honor group id; without it, every enabled group, at most 10) and `limit` (members per group).

| Field | Value |
| --- | --- |
| `honor` | `{"id", "name", "description"}`. |
| `members` | The group's newest holders whose grant has not expired (Members). |

### `members.verified`

Parameters `kind` (`merchant`, `agent`, `blogger`, `webmaster`, `channel`; without it, every kind) and `limit`
(members per kind). Grouped by kind, as on the verified members page.

| Field | Value |
| --- | --- |
| `kind` | The kind's code. |
| `name` | The kind's name. |
| `members` | `[{"uid", "name", "href", "avatar", "site"}]` in order of verification; `site` is `{"href", "host"}` of the verified website, or `null`. |

### `moderators`

Parameter `board`: a board's `fid`, or `current` in a block (on a board or thread page). The board's own moderators,
then those of its parent.

| Field | Value |
| --- | --- |
| `uid`, `name`, `href`, `avatar` | The moderator. |
| `role` | `moderator` or `trainee`. |
| `inherited` | Appointed to the parent board. |

### `site.stats`

Parameter `scope`: `site`, `board:<fid>` or `current` (as for `threads`). One item.

| Field | Value |
| --- | --- |
| `threads` | Threads. |
| `replies` | Replies, or `null` for a board. |
| `contributors` | The activity index (`settings.site.index_name`), or `null` for a board. |
| `today` | Posts today. |

The site's numbers are the sidebar statistics, the board's the board statistics; both count public content and are
recomputed by time segment ([settings-and-caching.md](settings-and-caching.md), "The process cache").

### `site.links`

Parameter `place`: `home` (the links shown on the home page, at most `settings.links.home_count`; none on a domain
other than the main one) or `directory` (every listed link, by category). Only approved links.

| Field | Value |
| --- | --- |
| `name` | The site's name. |
| `url` | Its address. |
| `domain` | Its domain, or `null` for `home`. |
| `description` | Its description, or `null` for `home`. |
| `category` | The category code, or `null` for `home`. |

### `thread.outline`

Parameter `tid`: a thread id, or `current` in a block on a thread page. One item: where a long thread's pages and days
begin, for a timeline or a reading position. Away from the thread's own page, a thread in a board the visitor cannot
list (another domain's board, or one they hid or blocked) is not found, as in rule 2. Positions are floor numbers
(`#floor-N`), counted in the flat view (floor 1 the opening post, then the replies in order); hidden floors keep their
numbers.

| Field | Value |
| --- | --- |
| `tid` | The thread. |
| `pages` | Pages in the flat view. |
| `per_page` | Replies per page (`settings.display.replies_per_page`). |
| `last_position` | The highest floor number. |
| `first`, `last` | `{"position", "at"}` of the first and the last floor. |
| `page_marks` | `[{"page", "from", "to", "first_at"}]`: each page's floors and the time of its first floor. |
| `date_marks` | `[{"position", "page", "at"}]`: the first floor of each day, or of each month when the thread spans more than 60 days. |

`page_marks` and `date_marks` together hold at most 200 marks, evenly sampled when there are more. Threads with more
than 5,000 floors are read at sampled floors only, so their marks are approximate.

### `me.card`, `me.counts`, `me.checkin`

No parameters. Only for a signed-in member, about that member.

| Source | Fields |
| --- | --- |
| `me.card` | `uid`, `name`, `href`, `avatar` (the member), `level` (`{"name"}`), `role` (`admin`, `global_moderator`, `moderator`, `trainee`, `reviewer` or `member`), `credits` (`{"word", "amount"}`: the name of the points and the available amount), `verification` (`{"email", "mobile"}`: a verified email, a linked phone number). |
| `me.counts` | `notifications_unread`, `messages_unread` (conversations with unread messages). |
| `me.checkin` | `today` (checked in today), `streak` (days in a row). |

## `/data/v1`

`GET /data/v1/<source>?<parameters>`, for example `/data/v1/threads?sort=hot&days=7&limit=5`. On a site installed
under a sub-path the prefix comes first. Parameters are the source's, written as text (`limit=5`, `counts=true`);
`current` is not available (there is no page), so `moderators` and `thread.outline` need their id.

- **Read only.** Only `GET` (and `HEAD`); other methods get `405 method_not_allowed`. Nothing changes state: checking
  in and the like stay with their forms and the member API ([api.md](api.md)).
- **The visitor's session.** The session cookie decides whose view it is; nothing else does. A personal source without
  a session gets `401 unauthorized`, never a redirect.
- **Same site only.** No CORS headers are ever sent, so pages of other sites cannot read the answers, and a request
  whose `Sec-Fetch-Site` is `cross-site` gets `403 cross_site`. There is no JSONP: an unknown parameter such as
  `callback` is an error.
- **Headers.** `Content-Type: application/json; charset=utf-8`, `Cache-Control: private, no-store` and
  `X-Content-Type-Options: nosniff`. The endpoint sets no cookie of its own.

Errors are answered like the member API's ([api.md](api.md), "Responses"), `{"code", "error"}`:

| Status | `code` | When |
| --- | --- | --- |
| 400 | `invalid_param` | An unknown parameter, a repeated one or a value outside the source's list; `error` names the parameter. |
| 401 | `unauthorized` | A personal source without a session. |
| 403 | `cross_site` | `Sec-Fetch-Site: cross-site`. |
| 404 | `not_found` | No such source, or a board, thread or honor group the visitor cannot read. |
| 405 | `method_not_allowed` | Not `GET`. |
| 503 | `unavailable` | The site's Redis is unavailable (`Retry-After: 3`). |
| 500 | `internal` | The source failed; the error is logged. |
