# The member API

Gono Forum has a JSON API for members who want to post and react from scripts, bots and other programs. Each member
uses a personal token, and everything a request does happens as that member, under the same rules as on the website:
posting limits, the participation requirements, mutes, read-only mode and the rest. This page is for people who write
such programs; the last section is for site administrators.

The API covers writing (starting threads of every kind, replying, editing your own posts, favorites, likes, taking
part in bounties, polls and lotteries, the daily check-in) and the few reads a program needs for that: your account,
the list of boards, activity cards and your points. It does not list threads or return post bodies, and it cannot
upload attachments.

## Overview

- **Base URL**: the forum address followed by `/api/v1`, for example `https://example.com/api/v1`. When the forum is
  installed under a sub-path, the path is part of it: `https://example.com/forum/api/v1`. The forum address is the
  Forum URL setting (Global settings › Site settings); when it is empty, the address the request came to is used. The
  API page in your settings (`/my/api`) shows the exact address.
- **Several domains**: on a site with several domains, the API answers on each of them. Links in responses use the
  domain you called, the board list shows that domain's boards, and you can post to any board you can read.
- **Identifiers** are integers: `uid` for members, `fid` for boards, `tid` for threads, `pid` for posts. Field names say
  `forum` for a board. Times are Unix seconds (UTC).
- **No CORS**: the API sends no cross-origin headers, so pages on other sites cannot call it from a browser. It is meant
  for scripts and servers.

## Getting a token

Members manage their token on the API page of their personal settings (`/my/api`):

- **Turn on the API** asks for your current password and creates a token. The page shows the token once, together with
  an example `curl` command. Copy it right away: it does not appear again after a refresh, and the site keeps only its
  SHA-256 digest, so nobody, administrators included, can look it up again.
- **Generate a new token** asks for your current password again. The old token stops working at once.
- **Turn off the API** revokes the token at once. What you posted with it stays.

To create a token:

- the site must have the API turned on;
- your account must meet the site's participation requirements (by default a verified email; a site can also accept a
  linked phone number or a linked Google or GitHub account). Administrators are exempt;
- your account needs a password. If you signed up with a text message code or another account, use "Set a password" on
  the account security page first;
- at most 5 attempts per account, and 30 per IP address, are allowed within 5 minutes.

A member has at most one token at a time. It is valid for the number of days the site sets (365 by default); after
that it stops working and the page says so, and you generate a new one. The page also shows when the token was created,
when it expires, and when and from which IP address it was last used (recorded at most once a minute).

A token is `gonof_` followed by 64 lowercase hexadecimal digits, 70 characters in all. Apart from that one display,
pages and administrators only see its first 14 characters, such as `gonof_3fa94c1e…`; `GET /me` returns them as
`key.hint`.

## Authentication

Send the token in the `Authorization` header on every request:

```bash
curl https://example.com/api/v1/me -H "Authorization: Bearer $TOKEN"
```

- The scheme name is not case-sensitive (`Bearer`, `bearer`), and spaces around the token are ignored.
- The API reads no cookies and needs no CSRF token: a browser that is signed in to the forum cannot call it without the
  token, and the API never sets cookies.
- The token acts as you. Keep it out of URLs, shared logs and repositories; if it leaks, generate a new one or turn the
  API off.

### When a token stops working

Requests with a token get `401 invalid_token` as soon as:

- you generate a new token or turn the API off;
- the token expires;
- you reset your password through password recovery, or use "Log out everywhere" on the account security page;
- an administrator revokes it, or resets your two-step verification;
- your account deletion is completed.

Changing your password or email while signed in, role changes and freezes leave the token in place. While an account
is frozen or waiting to be deleted, every request gets `403 account_restricted`; once the freeze ends, the token works
again.

Revoking a token also stops writes that are already under way: a write that has not been saved when the token is
revoked fails with `401 unauthorized` (`422 activity_rejected` for the activity endpoints and the check-in) and
changes nothing.

## Requests and responses

### Request bodies

- Endpoints that take a body expect JSON with `Content-Type: application/json`; anything else gets `415`. The body may
  be at most the site's limit, 128 KiB by default (`413` above it).
- Unknown fields and values of the wrong type are refused with `400 bad_request`, including unknown fields inside
  nested objects.
- Endpoints without a body (the `GET` endpoints, favorites, likes and the check-in) need no `Content-Type`.
- Identifiers in paths must be positive integers; anything else gets `404 not_found`.

### Responses

Every response is JSON with `Content-Type: application/json; charset=utf-8` and `Cache-Control: no-store`. A
successful response is the object described for each endpoint. An error looks like this:

```json
{"error": {"code": "invalid_token", "message": "The token is invalid, turned off or expired"}}
```

`code` is stable and is what programs should check. `message` is for people: it is written in the site's language (or
the domain's) and may change. `401` responses carry `WWW-Authenticate: Bearer realm="gonof"`; `429` and `503`
responses carry `Retry-After` in seconds.

### Status and error codes

| Status | `code` | When |
| --- | --- | --- |
| 400 | `bad_request` | The body is not valid JSON, has an unknown field or a value of the wrong type, or `Idempotency-Key` is malformed. |
| 401 | `invalid_token` | The token is missing, malformed, unknown, revoked or expired. |
| 401 | `unauthorized` | The token was revoked, or the account changed, while the request was being processed; nothing was saved. |
| 403 | `account_restricted` | The account is frozen, waiting to be deleted or deleted. |
| 403 | `api_disabled` | The site has turned the API off. |
| 403 | `read_only` | The site is in read-only mode; only `GET` requests work. |
| 403 | `forbidden` | The action is not allowed: participation requirements not met, muted, level too low, thread closed, liking your own post, edit window passed, and so on. |
| 404 | `not_found` | No such endpoint, or the board, thread or post does not exist or you cannot see it. |
| 405 | `method_not_allowed` | The path exists but not with this method. |
| 409 | `conflict` | The `Idempotency-Key` was already used for a different request, or an edit's version is out of date. |
| 413 | `payload_too_large` | The body is larger than the site's limit. |
| 415 | `unsupported_media_type` | The body is not `application/json`. |
| 422 | `validation` | A field value is invalid: title, body, attachments, activity fields, duplicate content and so on. |
| 422 | `activity_rejected` | An activity refused the action: not the author, closed or past the deadline, invalid choices, already voted, check-in turned off, and so on. |
| 429 | `quota` | A posting limit: daily or new-member allowance, minimum interval, hourly limit. |
| 429 | `rate_limited` | An API rate limit (next section). |
| 500 | `internal` | The server failed; the error is logged. |
| 503 | `unavailable` | The site's Redis is unavailable (only on sites that use Redis). A 503 from inside a write can carry the code `internal` instead; treat every 503 as temporary. |

### Rate limits

- **Per token**: a number of requests per minute (120 by default) and, of those, a number of write requests, meaning
  anything but `GET` (20 by default). Over either limit the request gets `429 rate_limited` with `Retry-After: 60`.
- **Failed authentication per IP address**: a well-formed token that is unknown, revoked or expired counts as a
  failure, and an address may fail 30 times in 5 minutes by default. Past that, every request with a token from that
  address, valid or not, gets `429 rate_limited` with `Retry-After: 300` until the window passes. A missing or
  malformed header does not count as a failure, but is also refused with `429` while the address is blocked.
- **Posting limits** are the site's own rules, the same as on the website, and answer with `429 quota`.

### Idempotency keys

Send an `Idempotency-Key` header with write requests so that a retry after a timeout or a dropped connection cannot
post twice:

```text
Idempotency-Key: weekly-digest-12
```

- The key is 1 to 200 visible ASCII characters (no spaces); anything else gets `400`.
- Starting threads, replying, editing, favorites, likes, accepting a bounty answer and voting use it. The check-in and
  entering a lottery ignore it: they already happen at most once per day, and once per thread.
- Repeating a request with the same key and the same content returns the original result without doing anything
  again: the same `tid` and `pid`, with `200` instead of `201` for new threads and replies. This holds even when the
  thread's activity has ended since, as long as the key is remembered. If two such requests arrive
  at the same time, only one is written; one gets `201`, the other `200`.
- Reusing a key for a different request gets `409 conflict` (`422 activity_rejected` for accepting and voting). Use a
  new key for every new action.
- Keys belong to the member, not to the token, and are remembered for the site's request key retention period (30 days
  by default); after that, the same key counts as a new request.
- Without the header, every request is new.

### Account and site rules

The API applies the website's rules, and write requests check them again inside the same database transaction that
saves the change:

- **Participation requirements**: members who do not meet them can still read, but their writes are refused:
  `403 forbidden` for threads, replies, favorites and likes, `404` for edits, `422 activity_rejected` for the activity
  endpoints and the check-in. `GET /me` reports this as `can_post: false`.
- **Mutes**: a muted member cannot start threads, reply, edit, like, vote or enter lotteries (`403 forbidden`, or
  `422 activity_rejected` for votes and lottery entries); favorites, the check-in and accepting bounty answers still
  work. See [roles](roles.md) for mutes and level gates.
- **Level gates**: starting red packets and lotteries, and adding links to other sites (in a body, a title or a news
  thread's `source_url`) while still a new member or below the required level, can be refused with `403 forbidden`.
- **Read-only mode**: every request other than `GET` gets `403 read_only`, including favorites and the check-in, which
  the website still allows during read-only mode.
- **Posting limits, the body limit and the author edit window** are the site's settings (Security › Anti-spam &
  governance). Administrators are not held to the posting allowances, and the people who manage a board are exempt
  from its anti-spam rules.

## Endpoints

All paths are relative to the base URL.

| Method and path | Does | Success |
| --- | --- | --- |
| `GET /me` | Your account and token | `200` |
| `GET /forums` | The boards you can read | `200` |
| `GET /credits` | Your points | `200` |
| `POST /check-in` | Check in for today | `200` |
| `POST /threads` | Start a thread of any kind | `201` |
| `PATCH /threads/{tid}` | Edit your thread's title, first post or visibility | `200` |
| `POST /threads/{tid}/replies` | Reply to a thread | `201` |
| `POST /posts/{pid}/replies` | Reply to a reply (nested reply) | `201` |
| `PATCH /posts/{pid}` | Edit your reply | `200` |
| `PUT /threads/{tid}/favorite`, `DELETE …` | Add or remove a favorite | `200` |
| `PUT /posts/{pid}/like`, `DELETE …` | Like or unlike a post | `200` |
| `GET /threads/{tid}/activity` | The activity card of a bounty, red packet, lottery, poll or news thread | `200` |
| `POST /threads/{tid}/accept` | Accept an answer to your bounty | `200` |
| `POST /threads/{tid}/vote` | Vote in a poll | `200` |
| `POST /threads/{tid}/enter` | Enter a one-click lottery | `200` |

Any other path under `/api/v1` gets `404 not_found`, and a known path with another method gets `405`.

### Your account

**`GET /me`**

```json
{
  "uid": 42,
  "username": "alice",
  "email_verified": true,
  "can_post": true,
  "profile_url": "https://example.com/space/42",
  "key": {"hint": "gonof_3fa94c1e", "created_at": 1767225600, "expires_at": 1798761600, "last_used_at": 1767312000}
}
```

`can_post` says whether writes are allowed: the account is active, not frozen, and meets the participation
requirements (or is an administrator). `key.last_used_at` is the last recorded use, `0` if none.

**`GET /credits`** returns your points: `{"available": 480, "frozen": 60, "checked_in": true}`. `frozen` is the amount
held for your open bounties, red packets and lotteries; `checked_in` says whether you have checked in today, counted in
the site's time zone.

**`POST /check-in`** takes no body and returns `{"credited": true}`. You get the day's check-in points once per
calendar day in the site's time zone; further calls that day return `{"credited": false}`. If the site has the check-in
turned off, the answer is `422 activity_rejected`.

### Boards

**`GET /forums`**

```json
{"forums": [
  {"fid": 3, "slug": "daily", "name": "Daily", "description": "Everyday talk", "parent_fid": null},
  {"fid": 7, "slug": "daily-photos", "name": "Photos", "description": "", "parent_fid": 3}
]}
```

The open boards that signed-in members can read, in the site's order. `parent_fid` is the parent board, `null` for a
top-level board. On a site with several domains, the list holds the boards of the domain you called.

### Starting threads

**`POST /threads`** starts a thread. A plain discussion needs three fields:

```bash
curl -X POST https://example.com/api/v1/threads \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -H "Idempotency-Key: weekly-digest-12" \
  -d '{"forum": "daily", "title": "Weekly digest", "body": "This week **in short**: …"}'
```

```json
{"tid": 1203, "pid": 5810, "position": 1, "url": "https://example.com/thread-1203"}
```

`pid` is the first post and `url` the thread page.

| Field | Type | Meaning |
| --- | --- | --- |
| `forum` | string or integer | Required. The board's `slug` or its `fid`; `404` if you cannot read it. |
| `title` | string | Required. 1 to 255 characters after trimming, no control characters. |
| `body` | string | Required. Markdown, not blank, up to the site's body limit (at most 60000 bytes). |
| `category` | integer | `0` discussion (default), `1` bounty, `2` red packet, `3` lottery, `4` poll, `5` news. |
| `visibility` | string | `public` (default) or `members`, exactly as written. A members-only thread is hidden from visitors who are not signed in; a members-only board makes all its threads members-only. |

The body uses the same Markdown as the website editor. The API cannot upload: to show an image, upload it on the
website first and reference it by its attachment ID the way the editor does, `![description](attachment:123)`. A post
can use up to 20 of your own uploads that have finished processing.

The activity categories take more fields; fields a category does not use are ignored. The points for bounties, red
packets and lottery points prizes are taken from your available points when the thread is posted and held until the
activity is settled; a budget larger than your balance is a `422 validation`.

| Field | Categories | Meaning |
| --- | --- | --- |
| `deadline` | 1–4 | Required, in the future. Unix seconds, or `"YYYY-MM-DDTHH:MM"` in the site's time zone. |
| `budget` | 1, 2, 3 | Points, 1 to 1,000,000,000. For a lottery, only when there is no `prizes`. |
| `shares` | 2, 3 | Red packet shares, or lottery winners when there is no `prizes`: 1 to 10000, no more than `budget`. |
| `distribution` | 2 | `equal` (default; `budget` must divide evenly by `shares`) or `random`. |
| `options` | 4 | 2 to 20 different options, each 1 to 255 characters. |
| `max_choices` | 4 | How many options a voter may pick, from 1 (default) to the number of options. |
| `source_url`, `source_name`, `source_author` | 5 | Required. An `http` or `https` link of up to 2048 characters; the source's name and the author, 1 to 255 characters each. A `source_url` on another site is held to the level gates like a link in the body. |
| `entry_mode` | 3 | `reply` (default: replying to the thread enters the lottery) or `click` (members enter with `POST /threads/{tid}/enter`). |
| `requirements` | 3 | Only with `click`: up to 10 conditions, each 1 to 200 characters, that entrants must confirm. |
| `review` | 3 | `true` lets you remove entrants before the draw (on the website). |
| `early_draw` | 3 | `true` lets you draw before the deadline (on the website). |
| `prizes` | 3 | Up to 10 prizes, described below. Without it, `budget` and `shares` make one points prize: `shares` winners get `budget / shares` each, which must divide evenly. |

A prize is an object with these fields:

| Field | Meaning |
| --- | --- |
| `name` | 1 to 60 characters. |
| `winners` | 1 to 10000. |
| `kind` | `credit` (points), `code` (a redemption code for each winner) or `item` (something you hand out yourself). |
| `amount` | Points per winner for `credit`, 1 to 1,000,000,000; leave it out for the other kinds. |
| `codes` | For `code`: exactly `winners` different codes, each up to 255 visible ASCII characters without quotes or backslashes. |
| `note` | Optional, up to 200 characters, for example how an `item` prize is claimed. |

All prizes together have at most 10000 winners, and at most 200 of them for `code` and `item` prizes. Redemption codes
never appear in API responses; winners see theirs on the website.

Example, a red packet of 100 points in 10 equal shares:

```json
{"forum": "daily", "title": "Thanks, everyone", "body": "Reply to claim a share.", "category": 2,
 "budget": 100, "shares": 10, "deadline": 1798761600}
```

### Replies

**`POST /threads/{tid}/replies`** replies to a thread, and **`POST /posts/{pid}/replies`** replies to a reply. Both take
`{"body": "…"}` (the same body rules as threads) and return `201`:

```json
{"tid": 1203, "pid": 5815, "position": 4, "url": "https://example.com/thread-1203#floor-4"}
```

A nested reply also returns `root_pid`, the top-level reply it sits under. `{pid}` can be a top-level reply or a reply
inside one; replying to a nested reply keeps the new reply under the same top-level reply. The first post of a thread
cannot take nested replies (`422`; reply to the thread instead). `position` is the post's floor number in the thread
(the first post is 1), and `url` links to it on the website.

Replies follow the website's rules: closed threads refuse them (`403`), and notifications and points work as usual.
Replying to a red packet claims a share automatically (once per member; not for the author), and replying to a lottery
with `reply` entry enters it.

### Editing

**`PATCH /threads/{tid}`** edits your thread: `{"title"?, "body"?, "visibility"?, "expected_version"?}`, with at least
one of the first three. **`PATCH /posts/{pid}`** edits your reply: `{"body", "expected_version"?}`; the first post is
edited through the thread endpoint (`422` otherwise). Both return:

```json
{"tid": 1203, "pid": 5810, "position": 1, "version": 3, "url": "https://example.com/thread-1203"}
```

- Fields you leave out keep their current values.
- `version` is the post's version: a new post has version 1, and each change to its title or body adds 1. Changing
  only `visibility` does not change it. Pass the version you last saw as `expected_version`; if the post has changed
  since, the edit gets `409 conflict`. Without it, the edit applies to the current version.
- Only your own posts can be edited through the API. Someone else's post gets `404`, or `422` for a moderator who
  manages it: moderation edits need a reason and are made on the website. The author edit window applies (24 hours
  after posting by default, then `403`), and you cannot edit a post that moderators have hidden.
- The previous text is kept as an internal revision.

### Favorites and likes

These set a state rather than toggle it, so repeating a call is harmless. None of them takes a body.

| Request | Response |
| --- | --- |
| `PUT /threads/{tid}/favorite` | `{"tid": 1203, "favorited": true}` |
| `DELETE /threads/{tid}/favorite` | `{"tid": 1203, "favorited": false}` |
| `PUT /posts/{pid}/like` | `{"pid": 5815, "liked": true, "count": 7}` |
| `DELETE /posts/{pid}/like` | `{"pid": 5815, "liked": false, "count": 6}` |

`count` is the post's like count afterwards. Adding a favorite or a like to a thread or post you cannot see gets
`404`, and liking your own post gets `403`. Liking a post takes back your downvote on it, as on the website.

### Activities

**`GET /threads/{tid}/activity`** returns the activity card of a bounty, red packet, lottery, poll or news thread,
including your own part in it; a discussion thread gets `404`. Fields that do not apply to the thread's kind are `0`,
`false`, empty or `[]`.

| Field | Meaning |
| --- | --- |
| `tid`, `kind`, `label` | The thread, its category (1 to 5) and the category's name in the site's language. |
| `owner_uid` | The thread's author. |
| `state`, `state_label` | `open`, `drawing`, `settled`, `expired` or `cancelled`, and a label for people. Past the deadline nobody can take part, even while `state` is still `open`. |
| `deadline`, `deadline_label` | Unix seconds, and the same time written in the site's time zone. |
| `budget`, `remaining` | Points put in, and points not paid out yet. |
| `shares`, `remaining_shares`, `distribution` | Red packet shares, shares left and `equal` or `random`; for a lottery, `shares` is the number of winners. |
| `entries`, `my_entry`, `my_amount` | Red packet claims or lottery entries, whether you are among them, and the points you received. |
| `accepted_pid`, `can_accept` | The accepted bounty answer (`0` if none), and whether you can accept one now. |
| `options`, `max_choices`, `voters`, `has_voted`, `can_vote` | Poll options (`number`, `label`, `votes`), the choice limit, the number of voters and your status. |
| `source_url`, `source_name`, `source_author` | The news source. |
| `entry_mode`, `requirements`, `review`, `early_draw` | The lottery's settings; `requirements` are objects with `number` and `label`. |
| `prizes` | Lottery prizes: `number`, `name`, `winners`, `kind`, `amount`, `note`. |
| `winners` | Lottery winners: `uid`, `username`, `amount`, `prize_no`, `prize`. |
| `my_prize`, `my_prize_kind` | The prize you won, empty if none. |
| `excluded`, `my_excluded` | How many entrants the author removed, and the reason if you were one of them. |
| `planned_deadline`, `planned_deadline_label` | The original deadline when the lottery was drawn early, `0` otherwise. |
| `can_enter`, `can_draw`, `can_manage`, `has_awards` | Whether you can enter, whether you can draw early, whether you are the lottery's author, and whether it has code or item prizes. |

**`POST /threads/{tid}/accept`** with `{"pid": 5815}` accepts a reply as the answer to your bounty and pays the whole
budget to its author: `{"tid": 1203, "accepted_pid": 5815}`. Only the bounty's author can do this, while it is open, for
someone else's reply.

**`POST /threads/{tid}/vote`** with `{"choices": [1, 3]}` votes in a poll, using the option `number`s from the activity
card: `{"tid": 1204, "choices": [1, 3]}`. Each member votes once and cannot change the vote.

**`POST /threads/{tid}/enter`** enters a lottery that uses `click` entry. `confirm` lists the numbers of all its
`requirements`, for example `{"confirm": [1, 2]}`; send `{}` when it has none. It returns
`{"tid": 1205, "entered": true}`, or `"entered": false` if you had already entered. The author cannot enter their own
lottery.

When an activity refuses one of these actions, the answer is `422 activity_rejected` with the reason in `message`.
Reviewing entrants, drawing early and managing redemption codes are done on the website.

## For administrators

**Turning the API on and off.** Global settings › Registration & permissions › "Enable the member API" (on by default).
While it is off, every API request gets `403 api_disabled` and members cannot create tokens; existing tokens are kept
and work again when it is turned back on, and members can still turn theirs off. The setting applies at once.

**Limits.** While the API is on, the same page has a Member API section:

| Setting | Default | Range |
| --- | --- | --- |
| Token lifetime (days) | 365 | 1–365; fixed into each token when it is created, so a change only affects new tokens |
| Requests per token per minute | 120 | 1–10000 |
| Write requests per token per minute | 20 | 1–10000, no more than the requests per minute |
| Failed authentications per IP per 5 minutes | 30 | 5–100 |
| Request body limit (KiB) | 128 | 1–1024 |

Who can create a token and write through the API follows the participation requirements (Global settings › Login
methods) and the posting rules (Security › Anti-spam & governance). Read-only mode (Global settings › Site settings)
blocks every API write. How long idempotency keys are remembered is "Request key retention (days)" under Tools &
maintenance › Maintenance.

**One member's token.** Users › Users, then open the member: the API token panel shows whether the member has one, its
first 14 characters, when it was created, when it expires, and when and from which IP address it was last used.
"Revoke API token" requires a reason (1 to 500 characters) and takes effect at once, including for writes already
under way. Only administrators see this panel. Creating, regenerating and revoking tokens appear in Tools & maintenance
› Action log. Tokens are stored only as SHA-256 digests in the `gonof_common_member_api_key` table, so a lost token
cannot be recovered, only replaced.

Tokens are also revoked when a member resets their password through password recovery, uses "Log out everywhere",
or has their account deletion completed, and when `gono-forum-cli reset-two-factor` turns off a member's two-step
verification.

**Proxy and Redis.** The per-address failure limit needs the real client address: forward `X-Real-IP` as described in
the reverse proxy section of [installing](install.md); otherwise every client counts as the proxy's address and shares
one limit. The `Authorization` header passes through nginx unchanged; nothing else is needed. With Redis on, the rate
limits are shared by all processes, and requests get `503` while Redis is unavailable; see the Redis section of
[installing](install.md) and [settings and caching](settings-and-caching.md).
