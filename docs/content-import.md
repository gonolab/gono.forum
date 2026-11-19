# Importing content

A content dataset is a directory of files with boards, members, threads and the replies written for each thread,
prepared outside the forum and imported once into a new site: to fill a demo site, or to give a load test realistic
pages. The setup wizard's optional sample content (see [install.md](install.md), "Setup wizard") is the small, built-in
case; this is the way to bring your own.

## Commands

```bash
gono-forum-cli export-sample content          # the wizard's sample content as a dataset, to start from
gono-forum-cli export-sample content --language=zh-CN   # the same in Simplified Chinese
gono-forum-cli check-content content          # check a dataset; needs no database or configuration
gono-forum-cli import-content content         # check, then import into the configured site
gono-forum-cli import-content content --members-out=members.json
```

- `export-sample` writes into a new or empty directory, in English unless `--language` names another language that
  has a sample (see [translating.md](translating.md), "Sample content"); the manifest's `language` is that tag. It
  creates every file new and stops at one that already exists, so of two exports into the same directory only one
  succeeds. The
  same files are in the repository at `examples/content/<tag>/` (`en`, `zh-CN`); a unit test keeps them identical to
  `apps/web/locales/<tag>/sample.json`. The sample's replies are drawn from shared pools, so they show the format, not
  the writing standard below.
- `check-content` reads every file and lists every problem with its file, line, thread key and reply id (the first
  200 errors and 200 warnings, then a count). It exits with an error when there is any error.
- `import-content` runs the same check and imports nothing if it fails. The site must have no boards, threads or posts
  yet (members are fine: finish the setup wizard first, without the sample content). Boards and members go in one
  transaction, threads 200 at a time, and the listing copies and counts are recomputed once at the end. If it stops
  halfway, the site keeps what was written and a second run refuses to stack on it: restore the database or start
  from a new one. Restart the forum afterwards so its process caches pick up the new boards.
- `--members-out` writes the new members' uids and names as a profile list for `enable-api-keys` and
  `enable-interactions`, for load tests that need signed-in members. It refuses to overwrite a file. The file is
  created before anything is imported, so a path that cannot be written stops the command while the site is still
  empty, and it is removed again if the import fails.

## Files

```text
content/
  manifest.json          format and version
  boards.json            all boards, one JSON array
  users.jsonl            one member per line
  threads/
    threads-001.jsonl    one thread per line, its replies inside it
    threads-002.jsonl    any number of *.jsonl files, read in name order; up to 1,000 threads per file is advised
```

A symbolic link in `threads/` is read like the file it points to; an entry that cannot be listed, or a `.jsonl` entry
whose file cannot be read, is an error rather than a file left out. Files are UTF-8 without a byte order mark, with LF
line endings. In `.jsonl` files each line is one complete JSON
object: no empty lines, comments or commas between lines. Line breaks inside a body are written `\n`. Fields not listed
below are errors, so a misspelt name is caught.

### manifest.json

```json
{ "format": "gono-forum-content", "version": 1, "language": "en" }
```

`format` and `version` must be exactly these; `language` is informational.

### boards.json

| Field | | Rules |
| --- | --- | --- |
| `slug` | required | The address (`/help`). Lowercase letters, digits and `-`, at most 64 characters, unique; not ending in `-` and digits (`/tips-2` is a page address); not one of the site's own top-level addresses (`urls::RESERVED_SEGMENTS`, or starting with `page-` or `thread-`). |
| `name` | required | At most 50 characters, unique ignoring case. |
| `description` | required | Not empty. |
| `guidance` | optional | Shown when posting in the board. |
| `parent` | optional | The slug of a top-level board listed earlier. At most two levels and 10 children per parent. |

The list order is the display order. A parent board can hold threads of its own and also lists its children's. A site
has at most 400 top-level boards, the same limit the admin area keeps when a board is created or a child board is
moved to the top level, so a dataset may list at most 400 boards without a `parent`.

### users.jsonl

| Field | | Rules |
| --- | --- | --- |
| `username` | required | 1–50 characters, no whitespace, `@`, `/` or `\`, and not the stored marker of a deleted account (the Simplified Chinese for "deleted user", `\u{5df2}\u{6ce8}\u{9500}\u{7528}\u{6237}`). Stored in lowercase after NFKC, so `Nora` and `nora` are the same name and may not both appear. Staff-like names (`admin`, `moderator`, …) are allowed but pointed out. |
| `bio` | optional | At most 500 characters. |

Imported members are synthetic authors: no password, email or role, so nobody can sign in as them until
`enable-interactions` or `set-password` is used. A name already taken on the site (by the administrator, say) gets the
lowest free number appended; the dataset's own references still resolve.

### threads/*.jsonl

| Field | | Rules |
| --- | --- | --- |
| `key` | required | 1–64 letters, digits or `-`, unique across all files. Only used to name the thread in problems. |
| `board` | required | A board slug. |
| `author` | required | A username from `users.jsonl` (compared in stored form). |
| `title` | required | 1–255 characters after trimming, no control characters. |
| `body` | required | Markdown, not empty, at most 60,000 bytes, no NUL. |
| `pinned`, `featured` | optional | `true` pins the thread in its board (level 1) or features it. |
| `replies` | required | The replies in the order they were written; `[]` for none. |

Each reply has `id` (a positive integer, unique in the thread), `author`, `body` (the same rules as the thread body)
and optionally `reply_to`, the `id` of an earlier reply in the same thread. Without `reply_to` the reply answers the
thread and opens a floor; with it, the reply joins the floor of the reply it answers (`root_pid` is that floor's first
reply, `reply_to_pid` the reply itself). A reply that answers its own author is pointed out, not refused.

### Writing

Bodies use the forum's Markdown: paragraphs, headings, emphasis, strikethrough, lists, quotes, code, tables, links and
`<spoiler>…</spoiler>`. Task lists are not enabled. Not allowed, because they need uploads or real ids: images, raw HTML
other than `<spoiler>`, and links to `attachment:`, `member:` or `post:`. Mentions are plain `@name` text; the import
sends no notifications. Avoid dates and years in the text, since the import chooses the times.

## What the import generates

- Times: threads start at random during the 365 days before the import and are numbered in that order; replies follow
  their thread in list order within 30 days, most of them early, at least a second apart and never after the import
  (a thread starts at least an hour plus one second per reply before the import, so a long thread has room); each
  member joins between one and 60 days before
  their first post (members who never post join at a random time in the period). The same dataset always gives the
  same result apart from the shift with the import time.
- Ids: members take the next free uids in the order they joined; threads and posts are numbered from 1, each thread's
  first post followed by its replies. Keys and reply ids are not stored.
- View counts from the reply counts; text-tile avatars; reply counts, last replies, board and member counts and the
  leaderboard totals recomputed by the same code as the development fixture. Likes, favorites and tips are not imported.
- Growth points: the import earns none. Each imported post gets a growth record without base points, so the likes,
  highlights and accepted answers it receives afterwards count as usual (see [roles.md](roles.md), "Growth levels").
  A thread imported as `featured` earns no highlight points for that highlight, not even if it is taken off and given
  again.

The code is in `apps/web/src/content/dataset.rs` (reading, checking and planning) and `seed::import_dataset`
(writing).
