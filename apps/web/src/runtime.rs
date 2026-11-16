//! Site settings: the scalar settings and documents under the `settings.` prefix in `gonof_common_settings`.
//!
//! - **Scalar settings** are one row each (`settings.{group}.{field}`, structure in [`settings`]), decoded from the
//!   same set of rows by the page frame, the write gate and the admin area. Missing rows, bad values and out-of-range
//!   values are always errors, with no fallback to code defaults (docs/settings-and-caching.md, "Reading settings").
//! - **Navigation**: the document `settings.navigation.*`, three rows the page frame reads with the scalar settings.
//! - **Policies** (`policy.terms` …) live in `gonof_common_localized_text`, one row per language
//!   (docs/architecture.md, "Languages"); mail templates are in the same table, read and written by
//!   `mail/templates.rs`. Bodies are up to 60000 bytes and are not read with every page request or under the write
//!   gate.
//!
//! Page display reads the process cache ([`crate::cache::process`]); the first lock of a protected write transaction
//! is [`GATE_SQL`], which still reads the database with a shared lock — "after settings are committed, old requests no
//! longer write with old values" relies on the lock, which a cache cannot provide.
use crate::db::Tx;
use crate::{
    cache::{Block, GlobalCache},
    db::{self, Database},
    identity::{Actor, IdentityError},
};
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

pub mod kv;
pub mod settings;

pub use kv::SettingsError;
pub use settings::*;

pub type Result<T> = std::result::Result<T, IdentityError>;

/// Key prefix of the navigation document: `settings.navigation.{title|body|updated_at}`.
fn document_prefix(kind: &str) -> String {
    format!("settings.{kind}.")
}

/// The three fields of each document (in ascending order); reading, locking and writing all use these three rows.
const DOCUMENT_FIELDS: [&str; 3] = ["body", "title", "updated_at"];

/// Reading and locking reads of scalar settings: an equality `IN` over every scalar key, taken from
/// [`settings::scalar_keys`] (the same source as decoding and encoding) and written as literals in ascending order.
/// `extra` is the three navigation rows the page frame additionally reads.
///
/// **Reads and writes are textually identical**: the plain read, the gate and the save are all produced by this one
/// function, differing only in the trailing locking clause — this is the pivot of the deadlock-freedom argument
/// (docs/settings-and-caching.md, "Write gate and locks"): the gate and the save lock the same set of primary keys,
/// both taking locks in ascending index order, so there is no intermediate state of "each holding half and waiting
/// for the other".
///
/// **Equality reads by primary key instead of range reads**: a range scan has to read, and when necessary wait for,
/// the row after the end of each range, and that row belongs to another family (such as the navigation document);
/// under REPEATABLE READ a range locking read also takes gap locks. An equality lookup on a unique index only locks
/// the row it finds, touching neither adjacent rows nor gaps.
///
/// **`FORCE INDEX (PRIMARY)` cannot be omitted**: the scalar keys are most of the table's rows, so the optimizer
/// finds point lookups too expensive, and both 5.7.44 and 8.4.11 switch to a full table scan (`type=ALL`), where a
/// locking read locks every row, worse than the range form. With the primary key named, both versions use
/// `type=range key=PRIMARY` (estimated one row per listed key: the scalar keys for the gate, plus the three
/// navigation rows for the page frame).
///
/// Keys only contain lowercase letters, digits, dots and underscores, so writing them as literals needs no escaping;
/// each is checked during construction.
fn scalar_sql(extra: &[String], suffix: &str) -> String {
    let mut keys: Vec<String> = settings::scalar_keys()
        .into_iter()
        .map(str::to_owned)
        .chain(extra.iter().cloned())
        .collect();
    keys.sort_unstable();
    let list = keys
        .iter()
        .map(|key| {
            assert!(
                key.bytes().all(|b| b.is_ascii_lowercase()
                    || b.is_ascii_digit()
                    || b == b'.'
                    || b == b'_'),
                "settings keys may only contain lowercase letters, digits, dots and underscores: {key}"
            );
            format!("'{key}'")
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "SELECT skey,svalue FROM gonof_common_settings FORCE INDEX (PRIMARY) WHERE skey IN ({list}){suffix}"
    )
}
static READ_SQL: LazyLock<String> = LazyLock::new(|| scalar_sql(&[], ""));
/// The page frame's statement: scalar settings, the navigation document, the site's layout document and every
/// domain's override document (docs/skins.md, "Choosing a skin"), in one round trip. The domain documents are one
/// key family read by range; this read takes no locks, so the range touches nothing else.
static READ_WITH_NAVIGATION_SQL: LazyLock<String> = LazyLock::new(|| {
    let keys: Vec<String> = ["navigation", "layout"]
        .into_iter()
        .flat_map(|kind| {
            let prefix = document_prefix(kind);
            DOCUMENT_FIELDS.map(move |field| format!("{prefix}{field}"))
        })
        .collect();
    let (low, high) = kv::range(crate::skins::selection::DOMAIN_LAYOUT_PREFIX);
    scalar_sql(&keys, &format!(" OR (skey>='{low}' AND skey<'{high}')"))
});
/// The site-wide write gate. **It must stay a single SQL statement**: `cookie_auth_concurrency` asserts that an entry
/// point whose authentication fails runs only this one query; one more would mean business queries still run after
/// authentication failed.
pub static GATE_SQL: LazyLock<String> = LazyLock::new(|| scalar_sql(&[], " LOCK IN SHARE MODE"));
/// The exclusive lock taken when saving, which also serves as the before-image.
static SAVE_LOCK_SQL: LazyLock<String> = LazyLock::new(|| scalar_sql(&[], " FOR UPDATE"));
const DOCUMENT_READ_SQL: &str =
    "SELECT skey,svalue FROM gonof_common_settings WHERE skey>=? AND skey<?";
/// Saving a document locks its own three rows: an equality read by primary key that touches neither adjacent scalar
/// rows nor other documents (9.20).
/// With only three rows the optimizer would use the primary key anyway; naming the index keeps it independent of
/// statistics.
pub const DOCUMENT_LOCK_SQL: &str = "SELECT skey,svalue FROM gonof_common_settings FORCE INDEX (PRIMARY) WHERE skey IN (?,?,?) FOR UPDATE";
const UPSERT_SQL: &str = "INSERT INTO gonof_common_settings(skey,svalue) VALUES{} ON DUPLICATE KEY UPDATE svalue=VALUES(svalue)";

fn rows_of(rows: &[db::DbRow]) -> Result<kv::Rows> {
    kv::Rows::collect(rows).map_err(IdentityError::Internal)
}

/// Broken settings: the key name, the stored value and the expected value go into the error chain, and the response
/// layer recognises [`SettingsError`] and shows a dedicated error page.
fn broken(error: SettingsError) -> IdentityError {
    IdentityError::Internal(anyhow::Error::new(error))
}

fn decode_rows(rows: &[db::DbRow]) -> Result<SiteSettings> {
    settings::decode(&rows_of(rows)?).map_err(broken)
}

/// The site URL used for external links (canonical, share metadata, sitemap, robots, feeds, URLs returned by the API),
/// without a trailing slash, and with the path prefix in a sub-path deployment.
///
/// Within a request it first looks at the site resolved by the entry layer ([`crate::site`]); outside a request (mail,
/// background tasks) it takes the forum URL from the admin "Global settings › Site settings" (`settings.site.url`).
/// When the forum URL is empty, it uses the origin `request` derived from this request (scheme from the connection,
/// host from `Host`, see `router::auth::request_origin`; a reverse proxy must pass through the original `Host`). The
/// deployment config has no site URL; when none of these exist — occasions without a request such as mail and
/// background tasks, or a request without a usable `Host` — it reports an error, with no fallback to any default URL.
pub fn site_origin(
    settings: &SiteSettings,
    request: Option<&str>,
) -> std::result::Result<String, SettingsError> {
    let site = crate::site::current();
    if let Some(url) = site.url {
        return Ok(url.to_string());
    }
    // Multi-domain on, host not registered: the origin derived from this request plus the primary domain's prefix.
    if site.derived {
        return request
            .map(|origin| format!("{origin}{}", site.prefix))
            .ok_or_else(|| {
                SettingsError::new(
                    "settings.site.url",
                    Some(&settings.site.url),
                    crate::i18n::current().config_error_no_host(),
                )
            });
    }
    if !settings.site.url.is_empty() {
        return Ok(settings.site.url.clone());
    }
    request.map(str::to_owned).ok_or_else(|| {
        SettingsError::new(
            "settings.site.url",
            Some(""),
            crate::i18n::current().config_error_no_site_url(),
        )
    })
}

/// Writes the given rows in one multi-row upsert. Splitting it into per-row UPDATEs would raise this page's SQL count
/// by an order of magnitude.
async fn write_rows(
    db: &Database,
    tx: &mut Tx<'_>,
    pairs: &[(impl AsRef<str>, String)],
) -> Result<()> {
    if pairs.is_empty() {
        return Ok(());
    }
    let placeholders = ["(?,?)"].repeat(pairs.len()).join(",");
    let sql = UPSERT_SQL.replace("{}", &placeholders);
    let mut statement = db::query(&sql);
    for (key, value) in pairs {
        statement = statement.bind(key.as_ref()).bind(value.as_str());
    }
    statement.execute(db.observe(&mut **tx)).await?;
    Ok(())
}

/// Reads the database directly (bypassing the in-process cache): for admin edit pages that need current values, and
/// background tasks and mail without a cache handle.
pub async fn read(db: &Database) -> Result<SiteSettings> {
    let rows = db::query(&READ_SQL).fetch_all(db.executor()).await?;
    decode_rows(&rows)
}

/// What the page frame reads in its one statement.
pub struct FrameRows {
    pub settings: SiteSettings,
    pub navigation: Document,
    /// The site's skin choice (`settings.layout.*`); `None` when none was made or the body no longer parses.
    pub layout: Option<crate::skins::selection::Layout>,
    /// Each domain's override document, by domain id.
    pub domain_layouts: Vec<(i64, crate::skins::selection::Layout)>,
}

/// Fetches scalar settings, the navigation document and the layout documents in one SQL statement: this is the
/// source for the in-process settings block.
pub async fn read_with_navigation(db: &Database) -> Result<FrameRows> {
    let rows = db::query(&READ_WITH_NAVIGATION_SQL)
        .fetch_all(db.executor())
        .await?;
    let rows = rows_of(&rows)?;
    let settings = settings::decode(&rows).map_err(broken)?;
    let navigation = document_from(&rows, &document_prefix("navigation")).map_err(broken)?;
    // The layout document is checked like the navigation one: its rows must exist and be in range.
    document_from(&rows, &document_prefix("layout")).map_err(broken)?;
    let (layout, domain_layouts) = crate::skins::selection::from_rows(&rows);
    Ok(FrameRows {
        settings,
        navigation,
        layout,
        domain_layouts,
    })
}

/// Writes a document's three rows (body, title, save time) under `prefix`, in the caller's transaction, which has
/// locked them.
pub(crate) async fn write_document(
    db: &Database,
    tx: &mut Tx<'_>,
    prefix: &str,
    body: &str,
    title: &str,
    updated_at: i64,
) -> Result<()> {
    let pairs = [
        (format!("{prefix}body"), body.to_owned()),
        (format!("{prefix}title"), title.to_owned()),
        (format!("{prefix}updated_at"), updated_at.to_string()),
    ];
    write_rows(db, tx, &pairs).await
}

/// Write gate: the first lock of this transaction, a shared lock on the scalar settings rows, then decoding.
pub async fn locked(db: &Database, tx: &mut Tx<'_>) -> Result<SiteSettings> {
    let rows = db::query(&GATE_SQL)
        .fetch_all(db.observe(&mut **tx))
        .await?;
    decode_rows(&rows)
}
pub async fn guard_write(db: &Database, tx: &mut Tx<'_>, management: bool) -> Result<SiteSettings> {
    let settings = locked(db, tx).await?;
    if settings.site.read_only && !management {
        return Err(IdentityError::Suspended(
            settings.site.read_only_notice(crate::i18n::current()),
        ));
    }
    Ok(settings)
}
pub async fn admin_locked(db: &Database, tx: &mut Tx<'_>, actor: Actor) -> Result<()> {
    let row = db::query("SELECT adminid,freeze,credentials_version,account_state FROM gonof_common_member WHERE uid=? FOR UPDATE")
        .bind(actor.uid).fetch_optional(db.observe(&mut **tx)).await?.ok_or(IdentityError::Unauthorized)?;
    admin_allowed(&row, actor)
}

/// Same as [`admin_locked`], but reads via the connection pool without locking or opening a write transaction: only for
/// read-only admin pages (on SQLite, opening a write transaction takes the site-wide write lock). Permissions may still
/// change after the page renders; saving rechecks inside the transaction.
pub async fn admin_unlocked(db: &Database, actor: Actor) -> Result<()> {
    let row = db::query("SELECT adminid,freeze,credentials_version,account_state FROM gonof_common_member WHERE uid=?")
        .bind(actor.uid).fetch_optional(db.executor()).await?.ok_or(IdentityError::Unauthorized)?;
    admin_allowed(&row, actor)
}

fn admin_allowed(row: &db::DbRow, actor: Actor) -> Result<()> {
    if row.try_get::<i64, _>("credentials_version")? != actor.credentials_version
        || row.try_get::<i64, _>("freeze")? != 0
        || db::text(row, "account_state")? != "active"
    {
        return Err(IdentityError::Unauthorized);
    }
    if row.try_get::<i64, _>("adminid")? != 1 {
        return Err(IdentityError::Forbidden);
    }
    Ok(())
}
pub async fn audit(
    db: &Database,
    tx: &mut Tx<'_>,
    actor: Actor,
    kind: &str,
    data: serde_json::Value,
) -> Result<()> {
    db::query("INSERT INTO gonof_common_log(uid,operationuid,type,data,source,dateline) VALUES(?,?,?,?,'web',?)")
        .bind(actor.uid).bind(actor.uid).bind(kind).bind(data.to_string()).bind(crate::identity::now())
        .execute(db.observe(&mut **tx)).await?;
    Ok(())
}

/// Normalisation before saving: trim surrounding whitespace, normalise the site URL with the same rules as the config
/// file, and unify line breaks in multi-line text to LF.
/// After normalisation the whole value is validated with the read rules ([`settings::check`]).
fn normalize(value: &mut SiteSettings) -> Result<()> {
    let trim = |text: &mut String| *text = text.trim().to_owned();
    trim(&mut value.site.name);
    trim(&mut value.site.description);
    trim(&mut value.site.read_only_message);
    trim(&mut value.site.brand_text);
    trim(&mut value.site.credit_name);
    trim(&mut value.site.index_name);
    trim(&mut value.site.contact_email);
    trim(&mut value.site.url);
    if !value.site.url.is_empty() {
        // The input says maxlength=255, and the server needs the same limit; otherwise an overlong URL submitted
        // around the form would be stored in the settings and then spliced into every external link.
        if value.site.url.chars().count() > 255 {
            return Err(IdentityError::Validation(
                crate::i18n::current().config_error_url_long(),
            ));
        }
        value.site.url = crate::config::normalize_forum_url(&value.site.url).map_err(|_| {
            IdentityError::Validation(crate::i18n::current().config_error_url_shape())
        })?;
    }
    // robots: unify line breaks to LF and remove trailing blank lines.
    let robots = value.seo.robots.replace("\r\n", "\n").replace('\r', "\n");
    value.seo.robots = robots.trim_end_matches(['\n', ' ', '\t']).to_owned();
    // Webmaster verification: remove blank lines and surrounding whitespace on each line.
    value.seo.verification = value
        .seo
        .verification
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    value.growth.level_names = value
        .growth
        .level_names
        .iter()
        .map(|name| name.trim().to_owned())
        .collect();
    // Invite code prefixes: remove surrounding whitespace on each line and blank lines, keeping only the first of
    // duplicates.
    let mut prefixes: Vec<String> = Vec::new();
    for prefix in &value.invite.prefixes {
        let prefix = prefix.trim();
        if !prefix.is_empty() && !prefixes.iter().any(|seen| seen == prefix) {
            prefixes.push(prefix.to_owned());
        }
    }
    value.invite.prefixes = prefixes;
    Ok(())
}

/// Saving for each admin settings page (or a block within a page).
///
/// `edit` only changes this page's fields on **the current values read under lock**, and only rows whose values
/// changed are written back: two tabs each save their own, and a stale whole-page value from a form never overwrites
/// other tabs. Order: exclusive lock on the whole range of scalar settings (doubling as the before-image) → check the
/// administrator → normalise and validate the whole → one multi-row upsert → the audit log records which keys changed
/// → commit → clear this process's settings block and broadcast ([`GlobalCache::changed`]).
///
/// Validation failure returns `Validation` (form refill with 422); a broadcast failure after commit returns `Internal`
/// carrying `Unpublished`, and the response layer gives a 503 stating that the settings were saved.
///
/// The forum URL's path prefix cannot be changed here (returns `Validation`): a changed prefix takes effect immediately
/// on save and the old URLs instantly become 404, so it must pass a second confirmation via [`update_confirmed`].
pub async fn update(
    db: &Database,
    global: &GlobalCache,
    actor: Actor,
    audit_kind: &str,
    edit: impl FnOnce(&mut SiteSettings),
) -> Result<SiteSettings> {
    save(db, global, Some(actor), audit_kind, None, edit).await
}

/// The message to see before changing the forum URL's path prefix, in this request's language; the admin area uses it
/// to go to the second confirmation.
pub fn prefix_needs_confirm() -> &'static str {
    crate::i18n::current().config_error_prefix_needs_confirm()
}

/// The forum URL's path was changed again by someone else after confirmation, in this request's language.
pub fn prefix_moved() -> &'static str {
    crate::i18n::current().config_error_prefix_moved()
}

/// A save that passed the second confirmation (the admin "Forum URL" block): changing the path prefix is allowed, but
/// the current prefix read under lock must still be the `confirmed_from` seen on the confirmation page, otherwise it
/// returns `Conflict` — so two administrators confirming one after the other are not missed.
pub async fn update_confirmed(
    db: &Database,
    global: &GlobalCache,
    actor: Actor,
    audit_kind: &str,
    confirmed_from: &str,
    edit: impl FnOnce(&mut SiteSettings),
) -> Result<SiteSettings> {
    save(
        db,
        global,
        Some(actor),
        audit_kind,
        Some(confirmed_from),
        edit,
    )
    .await
}

/// Command-line rescue (`gono-forum-cli set-site-url`): changes the forum URL directly when the path was set wrong and
/// the admin area is unreachable.
/// Validation follows the same rules as the admin area, without checking for an administrator or a second
/// confirmation, and the audit log records one entry with `source = 'cli'`. Returns the values before and after.
pub async fn set_site_url(
    db: &Database,
    global: &GlobalCache,
    url: &str,
) -> Result<(String, SiteSettings)> {
    let mut before = String::new();
    let saved = save(db, global, None, "site.url.cli", None, |s| {
        before = s.site.url.clone();
        s.site.url = url.to_owned();
    })
    .await?;
    Ok((before, saved))
}

async fn save(
    db: &Database,
    global: &GlobalCache,
    actor: Option<Actor>,
    audit_kind: &str,
    confirmed_from: Option<&str>,
    edit: impl FnOnce(&mut SiteSettings),
) -> Result<SiteSettings> {
    let mut tx = db.begin_write().await?;
    // The exclusive lock and the before-image are fetched at once. Textually identical to the gate (the same set of
    // primary keys), and both take locks in ascending index order, so "save" and "write gate" can only queue behind
    // each other, never each hold half and form a cycle.
    let rows = db::query(&SAVE_LOCK_SQL)
        .fetch_all(db.observe(&mut *tx))
        .await?;
    let before = decode_rows(&rows)?;
    if let Some(actor) = actor {
        admin_locked(db, &mut tx, actor).await?;
    }
    let mut candidate = before.clone();
    edit(&mut candidate);
    normalize(&mut candidate)?;
    let candidate =
        settings::check(&candidate).map_err(|error| IdentityError::Validation(error.expected))?;
    // Path prefix changes are only allowed for confirmed admin saves and the command-line rescue.
    let from = crate::site::prefix_of(&before.site.url);
    if from != crate::site::prefix_of(&candidate.site.url) {
        match confirmed_from {
            Some(confirmed) if confirmed != from => {
                return Err(IdentityError::Conflict(prefix_moved()));
            }
            None if actor.is_some() => {
                return Err(IdentityError::Validation(prefix_needs_confirm()));
            }
            _ => {}
        }
    }
    // Coupling with multi-domain sites: when the switch or the forum URL changes, the primary domain row is created or
    // changed accordingly.
    let domains_changed =
        crate::domains::write::follow_settings(db, &mut tx, &before, &candidate).await?;
    let old = settings::encode(&before);
    let changed: Vec<(&'static str, String)> = settings::encode(&candidate)
        .into_iter()
        .zip(&old)
        .filter(|((_, new), (_, old))| new != old)
        .map(|(pair, _)| pair)
        .collect();
    write_rows(db, &mut tx, &changed).await?;
    // There is no optimistic locking, so two administrators saving the same page at once means the later write
    // overwrites without notice. Recording which items changed is the compensation — the before-image is in the lock
    // statement above, at no extra query cost.
    let detail = serde_json::json!({
        "changed": changed.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
    });
    match actor {
        Some(actor) => audit(db, &mut tx, actor, audit_kind, detail).await?,
        None => {
            // The command line has no operator: `uid` and `operationuid` are both left empty, and the source is
            // recorded as `cli`.
            db::query("INSERT INTO gonof_common_log(uid,operationuid,type,data,source,dateline) VALUES(NULL,NULL,?,?,'cli',?)")
                .bind(audit_kind)
                .bind(serde_json::json!({"changed": detail["changed"], "from": before.site.url, "to": candidate.site.url}).to_string())
                .bind(crate::identity::now())
                .execute(db.observe(&mut *tx))
                .await?;
        }
    }
    tx.commit().await?;
    let blocks: &[Block] = if domains_changed {
        &[Block::Settings, Block::Domains]
    } else {
        &[Block::Settings]
    };
    global.changed(blocks).await.map_err(|mut error| {
        // Saving settings already read the before-image under lock. Old settings in other processes expire with the
        // old lifetime, while the cache handle newly created for the CLI may never have read the settings.
        error.ttl_seconds = Some(before.cache_ttl());
        IdentityError::Internal(error.into())
    })?;
    Ok(candidate)
}

/// Documents kept in the settings table: only the navigation document (policies are in
/// `gonof_common_localized_text`).
pub const DOCUMENT_KINDS: &[&str] = &["navigation"];

/// Policy documents, one row per language in `gonof_common_localized_text`.
pub const POLICY_KINDS: [&str; 6] = crate::cache::POLICY_KINDS;

#[derive(Clone, Debug, Default)]
pub struct Document {
    pub title: String,
    pub body: String,
    /// Last saved time, UTC seconds. 0 means never saved, and the front end falls back to the built-in text.
    pub updated_at: i64,
}

fn document_from(rows: &kv::Rows, prefix: &str) -> std::result::Result<Document, SettingsError> {
    Ok(Document {
        title: rows.text(
            &format!("{prefix}title"),
            crate::i18n::current().config_check_document_title(),
            |v| v.chars().count() <= 120,
        )?,
        body: rows.text(
            &format!("{prefix}body"),
            crate::i18n::current().config_check_document_body(),
            |v| v.len() <= 60_000,
        )?,
        updated_at: rows.int(
            &format!("{prefix}updated_at"),
            0,
            i64::MAX,
            crate::i18n::current().config_check_document_updated_at(),
        )?,
    })
}

/// A document read directly from the database: the navigation document, or a policy in the site language (the admin
/// pages pass a language with [`document_in`]).
pub async fn document(db: &Database, kind: &str) -> Result<Document> {
    if POLICY_KINDS.contains(&kind) {
        let lang = read(db).await?.site.lang();
        return document_in(db, kind, lang).await;
    }
    if !DOCUMENT_KINDS.contains(&kind) {
        return Err(IdentityError::NotFound);
    }
    let prefix = document_prefix(kind);
    let (low, high) = kv::range(&prefix);
    let rows = db::query(DOCUMENT_READ_SQL)
        .bind(&low)
        .bind(&high)
        .fetch_all(db.executor())
        .await?;
    document_from(&rows_of(&rows)?, &prefix).map_err(broken)
}
/// One policy in one language; a language without a row gives an empty document with `updated_at` 0 (the built-in
/// text applies).
pub async fn document_in(db: &Database, kind: &str, lang: crate::i18n::Lang) -> Result<Document> {
    Ok(policy_copies(db, kind)
        .await?
        .remove(&lang)
        .unwrap_or_default())
}

/// Every saved language of one policy (one statement): the source of the in-process policy blocks.
pub async fn policy_copies(
    db: &Database,
    kind: &str,
) -> Result<std::collections::BTreeMap<crate::i18n::Lang, Document>> {
    if !POLICY_KINDS.contains(&kind) {
        return Err(IdentityError::NotFound);
    }
    let rows = db::query(
        "SELECT language,title,body,updated_at FROM gonof_common_localized_text WHERE kind=?",
    )
    .bind(kind)
    .fetch_all(db.executor())
    .await?;
    let mut copies = std::collections::BTreeMap::new();
    for row in rows {
        let tag = db::text(&row, "language")?;
        // A row of a language this binary does not carry (removed translation) is skipped, not an error.
        let Some(lang) = crate::i18n::Lang::parse(&tag) else {
            tracing::warn!(kind, language = %tag, "policy row of a language that is not compiled in");
            continue;
        };
        let document = Document {
            title: db::text(&row, "title")?,
            body: db::text(&row, "body")?,
            updated_at: row.try_get("updated_at")?,
        };
        if document.title.chars().count() > 120
            || document.body.len() > 60_000
            || document.updated_at < 0
        {
            return Err(IdentityError::Internal(anyhow::anyhow!(
                "policy {kind} ({tag}) is out of range: title up to 120 characters, body up to 60000 bytes"
            )));
        }
        copies.insert(lang, document);
    }
    Ok(copies)
}

/// One customized text of `gonof_common_localized_text`: kind (`mail.…`, `policy.…`), language and save time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalizedRow {
    pub kind: String,
    pub language: crate::i18n::Lang,
    pub updated_at: i64,
}

/// Every customized mail template and policy, by kind and language: for the admin language overview and the language
/// tabs of the edit pages. Rows of languages this binary does not carry are left out.
pub async fn localized_rows(db: &Database) -> Result<Vec<LocalizedRow>> {
    let rows = db::query(
        "SELECT kind,language,updated_at FROM gonof_common_localized_text ORDER BY kind,language",
    )
    .fetch_all(db.executor())
    .await?;
    let mut found = Vec::with_capacity(rows.len());
    for row in rows {
        if let Some(language) = crate::i18n::Lang::parse(&db::text(&row, "language")?) {
            found.push(LocalizedRow {
                kind: db::text(&row, "kind")?,
                language,
                updated_at: row.try_get("updated_at")?,
            });
        }
    }
    Ok(found)
}

/// Saves one policy in one language (an upsert of its row) and clears that policy's in-process block.
pub async fn save_document_in(
    db: &Database,
    global: &GlobalCache,
    actor: Actor,
    kind: &str,
    lang: crate::i18n::Lang,
    mut input: Document,
) -> Result<Document> {
    if !POLICY_KINDS.contains(&kind) {
        return Err(IdentityError::NotFound);
    }
    if input.title.chars().count() > 120 || input.body.len() > 60_000 {
        return Err(IdentityError::Validation(
            crate::i18n::current().policy_error_too_long(),
        ));
    }
    let mut tx = db.begin_write().await?;
    admin_locked(db, &mut tx, actor).await?;
    input.updated_at = crate::identity::now();
    db::query("INSERT INTO gonof_common_localized_text(kind,language,title,body,updated_at) VALUES(?,?,?,?,?) ON DUPLICATE KEY UPDATE title=VALUES(title),body=VALUES(body),updated_at=VALUES(updated_at)")
        .bind(kind)
        .bind(lang.tag())
        .bind(&input.title)
        .bind(&input.body)
        .bind(input.updated_at)
        .execute(db.observe(&mut *tx))
        .await?;
    audit(
        db,
        &mut tx,
        actor,
        "site.document.save",
        serde_json::json!({"document":kind,"language":lang.tag(),"updated_at":input.updated_at}),
    )
    .await?;
    tx.commit().await?;
    let block = Block::policy(kind).unwrap_or(Block::Settings);
    global
        .announce(&[block])
        .await
        .map_err(IdentityError::Internal)?;
    Ok(input)
}

/// Saves the navigation document, or a policy in the site language (the admin pages pass a language with
/// [`save_document_in`]).
pub async fn save_document(
    db: &Database,
    global: &GlobalCache,
    actor: Actor,
    kind: &str,
    mut input: Document,
) -> Result<Document> {
    if POLICY_KINDS.contains(&kind) {
        let lang = read(db).await?.site.lang();
        return save_document_in(db, global, actor, kind, lang, input).await;
    }
    if !DOCUMENT_KINDS.contains(&kind) {
        return Err(IdentityError::NotFound);
    }
    if input.title.chars().count() > 120 || input.body.len() > 60_000 {
        return Err(IdentityError::Validation(
            crate::i18n::current().policy_error_too_long(),
        ));
    }
    if kind == "navigation" {
        parse_navigation(&input.body)?;
    }
    let mut tx = db.begin_write().await?;
    let prefix = document_prefix(kind);
    // Each document locks its own three rows instead of borrowing the site settings rows as a mutex: the documents are
    // unrelated, so there is no reason for them to block each other.
    // Equality locks by primary key, never touching the row after the end of a range (scalar settings rows sit on
    // both sides of the navigation rows).
    let mut lock = db::query(DOCUMENT_LOCK_SQL);
    for field in DOCUMENT_FIELDS {
        lock = lock.bind(format!("{prefix}{field}"));
    }
    lock.fetch_all(db.observe(&mut *tx)).await?;
    admin_locked(db, &mut tx, actor).await?;
    if kind == "navigation" {
        for item in parse_navigation(&input.body)? {
            if let Some(fid) = item
                .target
                .strip_prefix("forum:")
                .and_then(|s| s.parse::<i64>().ok())
            {
                let row =
                    db::query("SELECT fid FROM gonof_forum_forum WHERE fid=? LOCK IN SHARE MODE")
                        .bind(fid)
                        .fetch_optional(db.observe(&mut *tx))
                        .await?;
                if row.is_none() {
                    return Err(IdentityError::Validation(
                        crate::i18n::current().config_error_navigation_forum(),
                    ));
                }
            }
        }
    }
    input.updated_at = crate::identity::now();
    let pairs = [
        (format!("{prefix}body"), input.body.clone()),
        (format!("{prefix}title"), input.title.clone()),
        (format!("{prefix}updated_at"), input.updated_at.to_string()),
    ];
    write_rows(db, &mut tx, &pairs).await?;
    audit(
        db,
        &mut tx,
        actor,
        "site.document.save",
        serde_json::json!({"document":kind,"updated_at":input.updated_at}),
    )
    .await?;
    tx.commit().await?;
    // Navigation and scalar settings are in the same block (fetched by the page frame in one SQL statement).
    global
        .announce(&[Block::Settings])
        .await
        .map_err(IdentityError::Internal)?;
    Ok(input)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NavigationItem {
    pub label: String,
    pub target: String,
}
pub fn parse_navigation(body: &str) -> Result<Vec<NavigationItem>> {
    let mut items = Vec::new();
    for line in body.lines().filter(|s| !s.trim().is_empty()) {
        let Some((label, target)) = line.split_once('|') else {
            return Err(IdentityError::Validation(
                crate::i18n::current().config_error_navigation_line(),
            ));
        };
        let (label, target) = (label.trim(), target.trim());
        let forum = target
            .strip_prefix("forum:")
            .and_then(|n| n.parse::<i64>().ok())
            .is_some_and(|n| n > 0);
        let page = matches!(
            target,
            "/" | "/search" | "/terms" | "/privacy" | "/rules" | "/contact"
        );
        if label.is_empty() || label.chars().count() > 30 || (!forum && !page) || items.len() >= 12
        {
            return Err(IdentityError::Validation(
                crate::i18n::current().config_error_navigation_items(),
            ));
        }
        items.push(NavigationItem {
            label: label.into(),
            target: target.into(),
        });
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn navigation_rejects_external_and_privileged_targets() {
        for body in [
            "Test | https://example.com",
            "Admin | /manage",
            "Test | //example.com",
            "Test | forum:0",
        ] {
            assert!(parse_navigation(body).is_err());
        }
        assert_eq!(
            parse_navigation("General | forum:1\nRules | /rules")
                .unwrap()
                .len(),
            2
        );
    }

    /// The keys in the `IN` list: the three statements other than the page frame's list exactly the scalar keys,
    /// and the page frame adds the three navigation rows; mail templates, policies and keys outside `settings.` are not
    /// touched; the three statements differ only in the trailing locking clause.
    #[test]
    fn scalar_statements_list_exactly_the_scalar_keys() {
        fn listed(sql: &str) -> Vec<String> {
            let (_, rest) = sql.split_once(" IN (").expect("reads by primary key IN");
            let (list, _) = rest.split_once(')').unwrap();
            list.split(',')
                .map(|key| {
                    key.strip_prefix('\'')
                        .and_then(|key| key.strip_suffix('\''))
                        .unwrap()
                        .to_owned()
                })
                .collect()
        }
        let scalars: Vec<String> = settings::encode(&settings::tests::seeded())
            .into_iter()
            .map(|(key, _)| key.to_owned())
            .collect();
        assert_eq!(scalars.len(), 134);
        assert_eq!(
            listed(&READ_SQL),
            scalars,
            "the IN list is in ascending key order, the same keys as the encoding"
        );
        let mut with_navigation = scalars.clone();
        for document in ["navigation", "layout"] {
            with_navigation.extend(
                ["body", "title", "updated_at"].map(|field| format!("settings.{document}.{field}")),
            );
        }
        with_navigation.sort();
        // The domain layout documents are one key family read by a half-open range after the IN list.
        assert!(READ_WITH_NAVIGATION_SQL.ends_with(
            ") OR (skey>='settings.domain_layout.' AND skey<'settings.domain_layout/')"
        ));
        assert_eq!(listed(&READ_WITH_NAVIGATION_SQL), with_navigation);
        for key in listed(&READ_WITH_NAVIGATION_SQL) {
            assert!(key.starts_with("settings."), "{key}");
            for family in ["settings.mail.", "settings.policy."] {
                assert!(
                    !key.starts_with(family),
                    "{key} must not be read for the page frame"
                );
            }
        }
        assert!(
            !listed(&READ_SQL)
                .iter()
                .any(|key| key.starts_with("settings.navigation."))
        );
        for sql in [
            &*READ_SQL,
            &*READ_WITH_NAVIGATION_SQL,
            &*GATE_SQL,
            &*SAVE_LOCK_SQL,
        ] {
            assert!(
                sql.contains(" FROM gonof_common_settings FORCE INDEX (PRIMARY) WHERE skey IN (")
            );
        }
        assert_eq!(
            GATE_SQL.strip_suffix(" LOCK IN SHARE MODE"),
            Some(READ_SQL.as_str())
        );
        assert_eq!(
            SAVE_LOCK_SQL.strip_suffix(" FOR UPDATE"),
            Some(READ_SQL.as_str())
        );
        assert_eq!(
            DOCUMENT_LOCK_SQL.matches('?').count(),
            DOCUMENT_FIELDS.len()
        );
    }

    #[test]
    fn normalization_trims_and_unifies_line_breaks() {
        let mut value = settings::tests::seeded();
        value.site.name = "  Coins  ".into();
        value.site.url = " https://example.com/ ".into();
        value.seo.robots = "User-agent: *\r\nDisallow: /\r\n\r\n".into();
        value.seo.verification = "\n  a=b  \n\nc=d\n".into();
        normalize(&mut value).unwrap();
        assert_eq!(value.site.name, "Coins");
        assert_eq!(value.site.url, "https://example.com");
        assert_eq!(value.seo.robots, "User-agent: *\nDisallow: /");
        assert_eq!(value.seo.verification, "a=b\nc=d");
        assert!(settings::check(&value).is_ok());
        value.site.url = "ftp://x".into();
        assert!(normalize(&mut value).is_err());
    }
}
