# Translating Gono Forum

Gono Forum's interface text lives in [Fluent](https://projectfluent.org/) files that are compiled into the binary.
English (`en`) is the base language: every message exists there, and the code is generated from it. Every other
language, Simplified Chinese (`zh-CN`) included, is a translation of it, and any message a translation does not have
falls back to English. A partial translation is therefore useful as soon as it is merged, and can be completed in
later pull requests.

Posts, usernames, profiles and other member content are always shown as written; only the interface is translated.
One line of the interface is not translated on purpose: the footer's "Built on Gono Forum" is English in every
language. It is written in `layout/base.html` rather than in a translation file, carries `translate="no"` so that
browser translation leaves it alone, and no skin can hide it ([skins.md](skins.md), "Built on Gono Forum"). The
performance figures after it keep `translate="yes"`.
A site's language is chosen by its administrator (one language per site, and optionally one per extra domain);
members do not pick a language, and addresses carry no language prefix.

## Files

```text
apps/web/locales/
  en/                 the base: complete, and the source of the generated code
    meta.ftl          the language's own name and number style
    frame.ftl         header, footer, navigation, menus
    time.ftl          relative times ("3 hours ago")
    js.ftl            text used by the browser scripts
    mail.ftl          built-in mail templates
    policy.ftl        built-in terms, privacy and rules pages
    manage.ftl …      one file per area of the site (32 files in all)
    sample.json       the setup wizard's sample content (optional; see "Sample content")
  zh-CN/              Simplified Chinese, same file names
```

`skin.ftl` also holds the names and descriptions of the built-in skins (`skin-builtin-<id>-name` and `-description`),
which are translated like any other message. Skins imported in the admin area or placed in the instance directory
bring their own names in each language in their manifest instead ([skins.md](skins.md), "The manifest").

A language is a directory named with its [BCP 47](https://www.rfc-editor.org/info/bcp47) tag: `de`, `fr`, `pt-BR`,
`zh-TW`, `ar`. The tag is matched exactly, so `pt-BR` and `pt` are different languages. Use the same file names as
`en/`; which file a message is in does not matter at run time, but keeping the same layout makes review and later
updates much easier. A file may be left out entirely, and its messages fall back to English.

Dates, number formatting, plural rules and text direction come from the Unicode CLDR data compiled into the binary,
so they work for any tag without extra files. Right-to-left languages get `dir="rtl"` on every page and the
stylesheets use logical properties, but no right-to-left language has been reviewed in the browser yet; please
report what looks wrong.

## Adding a language

1. Create `apps/web/locales/<tag>/` and copy `meta.ftl` from `en/`. Translate `language-name` into the language's
   own name (`Deutsch`, `Français`, `العربية`); it is what administrators see in the language list. Set
   `meta-number-grouping` (see below).
2. Copy the other files you want to translate from `en/` and translate the messages. Delete messages you have not
   translated rather than leaving English copies, so that the completion figure stays honest and later changes to
   the English text still reach your language.
3. Build and run the checks (below), then try the language in a local site.
4. Open a pull request with the checklist at the end of this page.

To update a language later, compare its files with `en/`: new keys in English are missing from the translation
(they show in English until translated), and keys removed from English must be removed from the translation too,
or the checks fail.

## Rules for messages

An English message from `time.ftl`, and its translation into Indonesian, which has no plural forms:

```fluent
time-hours-ago = { $n ->
    [one] { $n } hour ago
   *[other] { $n } hours ago
}
```

```fluent
time-hours-ago = { $n } jam yang lalu
```

- **Keys** are stable IDs such as `time-hours-ago`: lowercase letters, digits and hyphens. Never rename or
  invent keys in a translation; every key (and every attribute such as `.placeholder`) must exist in `en/`.
- **Comments** in `en/` (`#` above a message, `##` above a group) explain where messages appear and what each
  variable holds. You do not need to copy them.
- **Variables** (`{ $count }`, `{ $site }`) must be the same set as in English: use each one the English message uses,
  and no others. Their order can change freely. Site-specific words such as the site name (`$site`), the name of
  points (`$credit`) and of the activity index (`$index`) are variables, because each site sets its own.
- **Plurals**: select on a number variable with the plural categories of your language (`zero`, `one`, `two`,
  `few`, `many`, `other`), with a default variant marked `*`. Languages without plural forms can drop the selector
  and write one sentence.
- **Counts split in two.** Some counts are two messages, so that the page can put the number and the word in
  elements of their own ([skins.md](skins.md), "Counts"): a `…-label` message with only the word, and a message that
  places `{ $value_html }` (the number) and `{ $label_html }` (the word). Put the two slots in your language's order,
  with the spacing it uses (`site-forums-threads` is "3 threads" in English and word-first in Simplified Chinese).
  A label that selects its plural form keeps `$count` even in a language without plurals, written as a selector with
  only the default variant: `{ $count -> *[other] word }`, since a message must use the same variables as English.
- **No markup.** A translation must not contain `<`. When a sentence contains a link or code, the English message has
  a variable whose name ends in `_html` (`{ $link_html }`); place it where it belongs in your sentence and the code
  inserts the element. Characters from the Unicode private use area are not allowed either (they are used
  internally for that insertion).
- **Literal braces** are written as string literals: `{"{"}` for `{`. Mail template hints use this to show
  `{"{{"}link{"}}"}`, which the reader sees as `{{link}}`.
- **Browser scripts** (`js.ftl`): only plain text, `{ $variable }`, or a whole message that is one selector on a
  number variable whose variants are plain text and variables. Keys there start with `js-`, and `js-` keys live only
  in `js.ftl`.
- **Tone**: plain and direct; address the reader as "you" in the form usual for interfaces in your language. Keep
  punctuation natural for your language rather than copying English punctuation.

## meta.ftl

| Key | Meaning |
| --- | --- |
| `language-name` | The language's name in itself. Required for every language. |
| `meta-number-grouping` | How numbers inside sentences are written: `auto` groups digits the way CLDR says the language does (`1,234`, `1.234`, `1 234`); `never` writes them without separators (`1234`). It is a setting, not text: keep one of the two words. |

## Sample content

The setup wizard can fill a new site with sample content (see "Setup wizard" in [install.md](install.md)), and
`gono-forum-cli seed-sample --language=<tag>` and `export-sample` do the same from the command line. Its text is
`sample.json` in the language's directory, next to the translation files. Translating it is optional: a language
without the file offers no sample, and the wizard and the commands list only the languages that have one. The
sample's language is chosen on its own; it does not change the site language.

`en/sample.json` is complete: the members, the boards with their threads, and the pools that replies are drawn from.
A translation keeps that structure and holds only text:

```json
{
  "members": {
    "nora": {"username": "…", "bio": "…"}
  },
  "forums": {
    "general": {
      "name": "…", "description": "…", "guidance": "…",
      "threads": [{"title": "…", "body": "…"}],
      "replies": ["…"]
    }
  },
  "replies": ["…"],
  "follow_ups": ["…"]
}
```

- Members are keyed by their English username and boards by slug: every one must be there, and no others. Threads
  and replies are matched by position, so each list has exactly as many entries as in English, in the same order.
  Pins, features, authors, times and who answers whom come from English and are the same in every language.
- Usernames may be translated. Write them the way they are stored: lowercase, without spaces, `@`, `/` or `\`, at
  most 50 characters, all different. Board slugs stay as they are.
- Translate freely rather than word for word, so the sample reads like people writing in your language, but keep
  each title and body about the same subject and length. Avoid dates and years. Bodies use the forum's Markdown
  (see "Writing" in [content-import.md](content-import.md)).
- No text twice in one pool: a board's `replies` together with the shared `replies`, and the `follow_ups`. That keeps
  a thread from showing the same reply twice.
- Board names have at most 50 characters and differ from each other; bios have at most 500 characters.

`cargo test -p gono-forum-web --lib content::sample` checks all of this. Then export the sample into the repository's
examples, which a unit test keeps identical to it (remove an old copy first, since the command only writes into an
empty directory):

```bash
cargo run --bin gono-forum-cli -- export-sample examples/content/<tag> --language=<tag>
```

## Checking a translation

You need Rust and Node.js as described in the README's "Build from source".

The translation checks are unit tests and need no database:

```bash
cargo test -p gono-forum-web --lib i18n
```

They fail when a file does not parse, a key is not in English or is repeated across files, a key name or variable
name is malformed, a message uses different variables from English, a message contains `<` or private-use
characters, a `js.ftl` message leaves the allowed subset, or `meta.ftl` has no `language-name`. `build.rs` only parses
the English files; the other languages are embedded as they are and parsed when the program starts, and a file that
does not parse stops the start, so run the checks before opening a site.

To see the language in a page, run a local site (README, "Build from source"), sign in as the administrator, open
**/manage/languages** and pick your language as the site language. The same page lists every compiled-in language
with its completion, the share of English keys the language has. Untranslated text shows in English, which makes it
easy to spot.

`apps/web/tests/i18n_leaks_http.rs` crawls the English site and fails on any Chinese character that leaked out of
the translation system. It matters when you add or change interface text in the code rather than translations. Like
the other integration tests it needs a Redis 7 (README, "Tests"); on SQLite nothing else:

```bash
GONOF_TEST_BACKEND=sqlite GONOF_TEST_REDIS_URL='redis://127.0.0.1:6379/0' \
  cargo test -p gono-forum-web --test i18n_leaks_http
```

## For developers: adding interface text

Text shown to people is never written into templates or Rust code directly. How the generated methods, fallback,
formatting and caches work is described in [architecture.md, "Languages"](architecture.md#languages); this section is
what to do.

1. **Add the English message** to the file of the area it belongs to in `apps/web/locales/en/` (`frame.ftl` for the
   header and footer, `account.ftl` for sign-in and registration, `manage.ftl`, `config.ftl` and the other admin
   files for `/manage`, …). Keys start with the file's name (`account-…`), and error messages usually continue with
   `error` (`account-error-…`). Write a `#` comment when the context or a variable is not obvious: translators read
   it, and it becomes the documentation of the generated method.
2. **Use the generated method.** `build.rs` turns `frame-more` into `frame_more()`, and a message with variables
   takes them as arguments in the order they first appear in the English text. In templates write
   `{{ p.t.frame_more() }}` (fragment templates without `p` have their own `t: Lang` field); in request code
   `crate::i18n::current().frame_more()`; in background work, which has no request, an explicit language, usually
   the site's (`SiteInfo::lang()`). Static tables of labels hold the method itself
   (`fn(Lang) -> &'static str`, as in `mail::templates::KINDS`) and call it with the request's language.
3. **Pass site words as variables.** The site name, the name of points and the name of the activity index are set by
   each site, so a message uses `{ $site }`, `{ $credit }` or `{ $index }` and the caller passes the value
   (`p.credit()`, `p.index_word()` in templates, `SiteInfo::credit_word(lang)` and the like elsewhere).
4. **Put markup in a slot.** When a sentence contains a link or an element, give the English message a variable
   ending in `_html` and pass `Markup`: `p.link("/my/credits", text)` or `p.link_with(…)` for on-site links (they add
   the path prefix), `crate::i18n::Markup::tag("code", value)` for one element. Print the result with `|safe`.
5. **Script text** goes into `js.ftl` with a `js-` key, within the subset described in "Rules for messages", and is
   read in the browser with `window.gonofT('js-key', { n: 3 })`.
6. **Add the Simplified Chinese translation** to the same file in `apps/web/locales/zh-CN/` (other languages catch
   up on their own).

What needs a message follows "Errors" and "Stored system text" in architecture.md: anything a person can see
anywhere, including errors, is a message, and text nobody sees is plain English in the code. In practice:

- When unsure whether an error reaches a page, make it a message. One without variables is a `&'static str` and fits
  payloads such as `IdentityError::Validation(&'static str)` directly.
- Never tell errors apart by comparing their text with a fixed string; the text depends on the language.
- When the system writes text to the database for display later, store a kind or a code and translate it when shown.
  Only what a person wrote, such as a moderator's reason, is stored as text.

Testing:

- `cargo test -p gono-forum-web --lib i18n` checks the translation files (see "Checking a translation").
- `tests/i18n_leaks_http.rs` fails on Chinese text in English pages. A new page, form or mail should be visited
  there.
- The integration fixture `TestFixture::migrated` sets up each test database as an English site with sample brand
  words (`test_support::restore_sample_site`), and English is the language outside a request, so tests assert English
  text. Apart from the files in `apps/web/locales/` (translations and sample content) and the examples exported from
  them (`examples/content/<tag>/`), the repository contains no Chinese: a test about another language sets
  `settings.site.language` itself and takes the expected text from the generated `Lang` methods
  (`Lang::parse("zh-CN")`) or from the sample files, not from string literals.

## Pull request checklist

- [ ] The directory name is the language's BCP 47 tag, and the file names match `en/`.
- [ ] `meta.ftl` has `language-name` in the language itself and `meta-number-grouping` set to `auto` or `never`.
- [ ] No keys that are missing from `en/`; untranslated messages are left out, not copied in English.
- [ ] Every message uses the same variables as English, and plurals use your language's categories.
- [ ] No `<` in any message; links and code use the `_html` variables.
- [ ] `cargo test -p gono-forum-web --lib i18n` passes.
- [ ] If you translated the sample content: `examples/content/<tag>/` is exported from it and
  `cargo test -p gono-forum-web --lib content::` passes.
- [ ] You looked at the main pages (home, a board, a thread, sign-in, registration, the admin area) with the language
  selected, and long words do not break the layout.
- [ ] The pull request says which files are complete and who can review the language, if you know someone.

By sending a pull request you agree that your translation is licensed under the project's licenses (MIT OR
Apache-2.0), like any other contribution.
