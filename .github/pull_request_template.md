## What and why

<!-- What this pull request changes and why. Link the issue it addresses. -->

## How it was tested

<!-- Commands you ran and what you checked in the browser. -->

## Checklist

- [ ] `cargo fmt` and `cargo clippy --all-targets -- -D warnings` are clean.
- [ ] Tests pass on SQLite, and on MySQL if SQL changed (README, "Tests").
- [ ] New interface text is a message in `apps/web/locales/en/` (and `zh-CN/`), not written in code or templates.
- [ ] Pages still work without JavaScript.
- [ ] Schema changes were agreed in an issue, and come as a new migration in both `db/migrations/mysql/` and `db/migrations/sqlite/`.
- [ ] Styles use the color tokens, the existing breakpoints and logical properties; `npm run test:checks` passes.
- [ ] Documentation is updated where behavior changed.

### For translations

- [ ] The directory name is the language's BCP 47 tag and the file names match `apps/web/locales/en/`.
- [ ] `meta.ftl` has `language-name` and `meta-number-grouping`.
- [ ] Messages use the same variables as English, contain no `<`, and untranslated messages are left out.
- [ ] `cargo test -p gono-forum-web --lib i18n` passes.
- [ ] I looked at the main pages with the language selected (docs/translating.md, "Checking a translation").

By submitting this pull request I agree that my contribution is licensed under MIT OR Apache-2.0.
