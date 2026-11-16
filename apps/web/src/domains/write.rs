//! Writes to the domain tables: add, edit and delete in the admin module, and the coupling with the forum URL and
//! forums.
//! The switch itself is a settings key saved via [`crate::runtime::update`]; creating the primary domain row happens in
//! [`follow_settings`].
//!
//! Only this module writes the two tables (checked by `db/source_guards.rs`). Lock order: saving the forum URL is
//! "settings row (exclusive) → top-level forums (shared) → domain rows (exclusive)"; saving a domain is "write gate
//! (shared) → forum rows (shared) → domain rows (exclusive)"; saving a forum is "forum row (exclusive) → domain rows
//! (shared)"; deleting a forum is "forum row (exclusive) → link rows". All take forums before domains, so there is no
//! cycle.
use crate::db::Tx;
use std::collections::{BTreeMap, BTreeSet};

use super::{Canonical, Domain, DomainMap, MAX_DOMAINS, authority_of};
use crate::{
    cache::{Block, GlobalCache},
    db::{self, Database},
    identity::{self, Actor, IdentityError},
    runtime::SiteSettings,
};

/// Exclusive locking read of the domain rows: the whole table by primary key, with the same upper bound as reading the
/// table. Under REPEATABLE READ it also locks the gaps, so concurrent inserts queue up.
const LOCK_SQL: &str = "SELECT domain_id,site_url,site_name,canonical,logo_aid,favicon_aid,verification,login_methods,participation,language,status,version,dateline FROM gonof_common_site_domain ORDER BY domain_id LIMIT 51 FOR UPDATE";
/// Shared locking read used when saving a forum: only the URL and status are needed.
const SHARE_SQL: &str = "SELECT domain_id,site_url,status FROM gonof_common_site_domain ORDER BY domain_id LIMIT 51 LOCK IN SHARE MODE";
const LINKS_SQL: &str =
    "SELECT domain_id,fid FROM gonof_common_site_domain_forum ORDER BY domain_id,fid LIMIT 20000";
/// Saving and deleting a domain first take a shared lock on all forums (the number of forums is bounded), blocking
/// concurrent forum saves that change a slug or level or delete a forum.
const FORUMS_SQL: &str = "SELECT fid,slug,name,parent_fid FROM gonof_forum_forum ORDER BY fid LIMIT 10000 LOCK IN SHARE MODE";

#[derive(Debug)]
pub enum DomainError {
    /// A form problem (422); the text is shown directly to the administrator.
    Invalid(String),
    Conflict(&'static str),
    /// The path of an existing domain changed and needs a second confirmation first: `from` and `to` are the URLs
    /// before and after the change.
    NeedsConfirm {
        from: String,
        to: String,
    },
    /// After confirmation the path was changed again by someone else.
    Moved,
    Forbidden,
    NotFound,
    Internal(anyhow::Error),
}

impl std::fmt::Display for DomainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => f.write_str(message),
            Self::Conflict(message) => f.write_str(message),
            Self::NeedsConfirm { .. } => {
                f.write_str(crate::i18n::current().domain_error_needs_confirm())
            }
            Self::Moved => f.write_str(crate::i18n::current().domain_error_moved()),
            Self::Forbidden => f.write_str(crate::i18n::current().domain_error_forbidden()),
            Self::NotFound => f.write_str(crate::i18n::current().domain_error_not_found()),
            Self::Internal(error) => write!(f, "multi-domain service error: {error}"),
        }
    }
}

impl std::error::Error for DomainError {}

impl From<sqlx::Error> for DomainError {
    fn from(error: sqlx::Error) -> Self {
        Self::Internal(error.into())
    }
}

impl From<anyhow::Error> for DomainError {
    fn from(error: anyhow::Error) -> Self {
        Self::Internal(error)
    }
}

impl From<IdentityError> for DomainError {
    fn from(error: IdentityError) -> Self {
        match error {
            IdentityError::Validation(message) => Self::Invalid(message.into()),
            IdentityError::Conflict(message) => Self::Conflict(message),
            IdentityError::Unauthorized | IdentityError::Forbidden => Self::Forbidden,
            IdentityError::Suspended(_) | IdentityError::Restricted(_) => Self::Forbidden,
            IdentityError::NotFound => Self::NotFound,
            IdentityError::Internal(error) => Self::Internal(error),
        }
    }
}

impl DomainError {
    /// Deadlocks and lock wait timeouts can retry the whole operation (same as `ContentWriteError::retryable`).
    fn retryable(&self) -> bool {
        match self {
            Self::Internal(error) => error.chain().any(|cause| {
                cause
                    .downcast_ref::<sqlx::Error>()
                    .is_some_and(crate::db::is_lock_conflict)
            }),
            _ => false,
        }
    }
}

/// One row submitted by the admin form. For the primary domain row only `forums` matters (the rest follows the global
/// settings).
#[derive(Clone, Debug, Default)]
pub struct DomainInput {
    /// 0 means a new row.
    pub domain_id: i64,
    pub version: i64,
    pub site_url: String,
    pub site_name: String,
    pub canonical: String,
    /// 0 means not set (use the main site's).
    pub logo_aid: i64,
    pub favicon_aid: i64,
    pub verification: String,
    /// Login entry points and participation conditions: `None` follows the site-wide setting, `Some` holds the ticked
    /// items (joined in constant order before validation). Ignored for the primary domain row.
    pub login_methods: Option<Vec<String>>,
    pub participation: Option<Vec<String>>,
    /// The domain's interface language tag; `None` follows the site language (docs/architecture.md, "Languages").
    /// Ignored for the primary domain row.
    pub language: Option<String>,
    pub enabled: bool,
    pub forums: Vec<i64>,
    /// The pre-change path prefix carried back by the confirmation page when changing the path; `None` until confirmed.
    pub confirmed_from: Option<String>,
}

/// Save result: the domain id, and the names of top-level forums that no enabled domain shows after the save (hinting
/// "… so visitors cannot reach them").
#[derive(Debug)]
pub struct Saved {
    pub domain_id: i64,
    pub unshown: Vec<String>,
}

/// A forum read under lock.
struct ForumRow {
    fid: i64,
    slug: String,
    name: String,
    parent_fid: Option<i64>,
}

async fn lock_forums(db: &Database, tx: &mut Tx<'_>) -> Result<Vec<ForumRow>, DomainError> {
    let rows = db::query(FORUMS_SQL)
        .fetch_all(db.observe(&mut **tx))
        .await?;
    rows.iter()
        .map(|row| {
            Ok(ForumRow {
                fid: row.try_get("fid")?,
                slug: db::text(row, "slug")?,
                name: db::text(row, "name")?,
                parent_fid: row.try_get("parent_fid")?,
            })
        })
        .collect::<Result<_, anyhow::Error>>()
        .map_err(DomainError::Internal)
}

/// Exclusively locks the whole domain table and reads it together with its link rows into a [`DomainMap`]. The link
/// rows are not locked separately: everyone who writes them first takes the lock on the domain or forum rows.
async fn lock_map(db: &Database, tx: &mut Tx<'_>) -> anyhow::Result<DomainMap> {
    let rows = db::query(LOCK_SQL).fetch_all(db.observe(&mut **tx)).await?;
    let links = db::query(LINKS_SQL)
        .fetch_all(db.observe(&mut **tx))
        .await?;
    let mut forums: BTreeMap<i64, BTreeSet<i64>> = BTreeMap::new();
    for link in links {
        forums
            .entry(link.try_get("domain_id")?)
            .or_default()
            .insert(link.try_get("fid")?);
    }
    let mut domains = Vec::with_capacity(rows.len());
    for row in rows {
        let id: i64 = row.try_get("domain_id")?;
        let canonical = db::text(&row, "canonical")?;
        let language: Option<String> = row.try_get("language")?;
        let language = match language.as_deref() {
            None => None,
            Some(tag) => Some(crate::i18n::Lang::parse(tag).ok_or_else(|| {
                anyhow::anyhow!("domain table language is not a compiled-in language: {tag}")
            })?),
        };
        domains.push(Domain {
            domain_id: id,
            site_url: db::text(&row, "site_url")?,
            site_name: row.try_get("site_name")?,
            canonical: Canonical::parse(&canonical).ok_or_else(|| {
                anyhow::anyhow!(crate::i18n::current().domain_error_load_canonical(&canonical))
            })?,
            logo_aid: row.try_get("logo_aid")?,
            favicon_aid: row.try_get("favicon_aid")?,
            verification: row.try_get("verification")?,
            login_methods: row.try_get("login_methods")?,
            participation: row.try_get("participation")?,
            language,
            enabled: row.try_get::<i64, _>("status")? == 1,
            version: row.try_get("version")?,
            dateline: row.try_get("dateline")?,
            forums: forums.remove(&id).unwrap_or_default(),
        });
    }
    Ok(DomainMap::new(domains))
}

/// The location of a URL: host, port and path, ignoring the scheme (the same location cannot be registered twice).
fn place(url: &str) -> (&str, &str) {
    (authority_of(url), crate::site::prefix_of(url))
}

/// Whether one of two path prefixes contains the other (including equality); the root path does not count as
/// containing a sub-path (root plus sub-path is allowed).
fn nested(a: &str, b: &str) -> bool {
    if a.is_empty() || b.is_empty() {
        return a == b;
    }
    let inside = |outer: &str, inner: &str| {
        inner
            .strip_prefix(outer)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    };
    inside(a, b) || inside(b, a)
}

/// Structural rules of the whole table; `forums` are all forums. Returns the description of the first rule that
/// fails.
fn check_map(map: &DomainMap, forum_url: &str, forums: &[(i64, &str)]) -> Result<(), String> {
    let main = map.main(forum_url).map(|main| main.domain_id);
    let rows = map.rows();
    if rows.len() > MAX_DOMAINS {
        return Err(crate::i18n::current().domain_error_too_many(MAX_DOMAINS));
    }
    for (index, row) in rows.iter().enumerate() {
        if rows[..index]
            .iter()
            .any(|earlier| place(&earlier.site_url) == place(&row.site_url))
        {
            return Err(crate::i18n::current().domain_error_same_place(&row.site_url));
        }
    }
    let enabled: Vec<&Domain> = rows.iter().filter(|row| row.enabled).collect();
    for (index, row) in enabled.iter().enumerate() {
        for earlier in &enabled[..index] {
            if earlier.authority() != row.authority() {
                continue;
            }
            if earlier.prefix().is_empty() && row.prefix().is_empty() {
                return Err(crate::i18n::current().domain_error_two_roots(row.authority()));
            }
            if nested(earlier.prefix(), row.prefix()) {
                return Err(
                    crate::i18n::current().domain_error_nested(&earlier.site_url, &row.site_url)
                );
            }
        }
    }
    // When a host has registered the root path, the first segment of a sub-path must not be a forum URL or a system
    // path.
    for row in &enabled {
        let Some(first) = row.prefix().split('/').nth(1) else {
            continue;
        };
        let has_root = enabled
            .iter()
            .any(|other| other.authority() == row.authority() && other.prefix().is_empty());
        if !has_root {
            continue;
        }
        if crate::urls::reserved_segment(first) {
            return Err(crate::i18n::current().domain_error_reserved(&row.site_url, first));
        }
        if forums.iter().any(|(_, slug)| *slug == first) {
            return Err(crate::i18n::current().domain_error_forum_slug(&row.site_url, first));
        }
    }
    let main_forums = main.and_then(|id| map.get(id)).map(|main| &main.forums);
    for row in enabled.iter().filter(|row| Some(row.domain_id) != main) {
        if row.canonical == Canonical::Main {
            let missing: Vec<i64> = row
                .forums
                .iter()
                .filter(|fid| main_forums.is_none_or(|shown| !shown.contains(fid)))
                .copied()
                .collect();
            if !missing.is_empty() && main_forums.is_some() {
                return Err(crate::i18n::current()
                    .domain_error_canonical_main(&row.site_url, names(forums, &missing)));
            }
        }
    }
    let mut owners: BTreeMap<i64, &Domain> = BTreeMap::new();
    for row in enabled
        .iter()
        .filter(|row| Some(row.domain_id) != main && row.canonical == Canonical::Own)
    {
        for fid in &row.forums {
            if let Some(owner) = owners.insert(*fid, row) {
                return Err(crate::i18n::current()
                    .domain_error_two_owners(names(forums, &[*fid]), &owner.site_url));
            }
        }
    }
    Ok(())
}

fn names(forums: &[(i64, &str)], fids: &[i64]) -> String {
    fids.iter()
        .map(|fid| {
            forums
                .iter()
                .find(|(id, _)| id == fid)
                .map_or_else(|| fid.to_string(), |(_, slug)| (*slug).to_owned())
        })
        .collect::<Vec<_>>()
        .join(crate::i18n::current().common_list_separator())
}

/// Top-level forums (names) that no enabled domain shows.
fn unshown(map: &DomainMap, forums: &[ForumRow]) -> Vec<String> {
    forums
        .iter()
        .filter(|forum| forum.parent_fid.is_none())
        .filter(|forum| {
            !map.rows()
                .iter()
                .any(|row| row.enabled && row.shows(forum.fid))
        })
        .map(|forum| forum.name.clone())
        .collect()
}

/// Login entry points and participation conditions: `None` follows the site-wide setting and is stored as NULL; ticked
/// items are joined comma-separated in constant order and validated with the same rules as the site-wide setting.
/// Ticking none is allowed for both: password sign-in only, and no participation requirement.
fn login_set(
    chosen: Option<&[String]>,
    allowed: &[&str],
    message: &str,
) -> Result<Option<String>, DomainError> {
    let Some(chosen) = chosen else {
        return Ok(None);
    };
    let value = allowed
        .iter()
        .filter(|item| chosen.iter().any(|picked| picked == *item))
        .copied()
        .collect::<Vec<_>>()
        .join(",");
    let unknown = chosen
        .iter()
        .any(|picked| !allowed.contains(&picked.as_str()));
    if unknown || !crate::runtime::settings::set_shape(&value, allowed, true) {
        return Err(DomainError::Invalid(message.into()));
    }
    Ok(Some(value))
}

/// Webmaster verification codes: after removing blank lines and surrounding whitespace, validated with the same rules
/// as the global setting.
fn verification(value: &str) -> Result<String, DomainError> {
    let text = value
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if !crate::runtime::settings::verification_shape(&text) {
        return Err(DomainError::Invalid(
            crate::i18n::current().domain_error_verification().into(),
        ));
    }
    Ok(text)
}

/// Adds or edits a row.
pub async fn save(
    db: &Database,
    global: &GlobalCache,
    actor: Actor,
    input: &DomainInput,
    orphan_seconds: i64,
) -> Result<Saved, DomainError> {
    let mut retry = 0_u64;
    let saved = loop {
        match save_once(db, actor, input, orphan_seconds).await {
            Err(error) if retry < 2 && error.retryable() => {
                retry += 1;
                tokio::time::sleep(std::time::Duration::from_millis(20 * retry)).await;
            }
            result => break result?,
        }
    };
    global.announce(&[Block::Domains]).await?;
    Ok(saved)
}

async fn save_once(
    db: &Database,
    actor: Actor,
    input: &DomainInput,
    orphan_seconds: i64,
) -> Result<Saved, DomainError> {
    if input.domain_id < 0 || input.version < 0 || (input.domain_id > 0 && input.version == 0) {
        return Err(DomainError::Invalid(
            crate::i18n::current().domain_error_id().into(),
        ));
    }
    let mut tx = db.begin_write().await?;
    let settings = crate::runtime::locked(db, &mut tx).await?;
    crate::runtime::admin_locked(db, &mut tx, actor).await?;
    if !settings.domains.enabled {
        return Err(DomainError::Invalid(
            crate::i18n::current().domain_error_disabled().into(),
        ));
    }
    let forum_url = settings.site.url.clone();
    let forums = lock_forums(db, &mut tx).await?;
    let map = lock_map(db, &mut tx).await?;
    let main = map.main(&forum_url).map(|main| main.domain_id);
    let existing = match input.domain_id {
        0 => None,
        id => Some(map.get(id).cloned().ok_or(DomainError::NotFound)?),
    };
    if let Some(existing) = &existing
        && existing.version != input.version
    {
        return Err(DomainError::Conflict(
            crate::i18n::current().domain_error_stale(),
        ));
    }
    // Only top-level forums are allowed, and they must exist.
    let mut chosen = BTreeSet::new();
    for fid in &input.forums {
        match forums.iter().find(|forum| forum.fid == *fid) {
            Some(forum) if forum.parent_fid.is_none() => {
                chosen.insert(*fid);
            }
            _ => {
                return Err(DomainError::Invalid(
                    crate::i18n::current().domain_error_forums().into(),
                ));
            }
        }
    }
    let now = identity::now();
    let is_main = existing
        .as_ref()
        .is_some_and(|existing| Some(existing.domain_id) == main);
    let candidate = if is_main {
        Domain {
            forums: chosen,
            ..existing.clone().expect("the primary domain row exists")
        }
    } else {
        let raw = input.site_url.trim();
        if raw.chars().count() > 255 {
            return Err(DomainError::Invalid(
                crate::i18n::current().domain_error_url_length().into(),
            ));
        }
        let site_url = crate::config::normalize_forum_url(raw)
            .map_err(|_| DomainError::Invalid(crate::i18n::current().domain_error_url().into()))?;
        if site_url == forum_url {
            return Err(DomainError::Invalid(
                crate::i18n::current().domain_error_is_main().into(),
            ));
        }
        let site_name = input.site_name.trim().to_owned();
        if !(1..=80).contains(&site_name.chars().count()) {
            return Err(DomainError::Invalid(
                crate::i18n::current().domain_error_name().into(),
            ));
        }
        let canonical = Canonical::parse(&input.canonical).ok_or_else(|| {
            DomainError::Invalid(crate::i18n::current().domain_error_canonical().into())
        })?;
        if input.logo_aid < 0 || input.favicon_aid < 0 {
            return Err(DomainError::Invalid(
                crate::i18n::current().domain_error_image_id().into(),
            ));
        }
        Domain {
            domain_id: input.domain_id,
            site_url,
            site_name: Some(site_name),
            canonical,
            logo_aid: (input.logo_aid > 0).then_some(input.logo_aid),
            favicon_aid: (input.favicon_aid > 0).then_some(input.favicon_aid),
            verification: Some(verification(&input.verification)?),
            login_methods: login_set(
                input.login_methods.as_deref(),
                crate::runtime::settings::LOGIN_METHODS,
                crate::i18n::current().domain_error_login(),
            )?,
            participation: login_set(
                input.participation.as_deref(),
                crate::runtime::settings::PARTICIPATION,
                crate::i18n::current().domain_error_participation(),
            )?,
            language: match input
                .language
                .as_deref()
                .map(str::trim)
                .filter(|tag| !tag.is_empty())
            {
                None => None,
                Some(tag) => Some(crate::i18n::Lang::parse(tag).ok_or_else(|| {
                    DomainError::Invalid(crate::i18n::current().domain_error_language().into())
                })?),
            },
            enabled: input.enabled,
            version: existing.as_ref().map_or(1, |existing| existing.version + 1),
            dateline: now,
            forums: chosen,
        }
    };
    // An existing domain changed its path: it must pass the second confirmation first, and the pre-change path carried
    // back by the confirmation page must equal the current value read under lock.
    if let Some(existing) = &existing
        && !is_main
    {
        let from = crate::site::prefix_of(&existing.site_url);
        if from != candidate.prefix() {
            match &input.confirmed_from {
                None => {
                    return Err(DomainError::NeedsConfirm {
                        from: existing.site_url.clone(),
                        to: candidate.site_url.clone(),
                    });
                }
                Some(confirmed) if confirmed != from => return Err(DomainError::Moved),
                Some(_) => {}
            }
        }
    }
    let mut rows: Vec<Domain> = map
        .rows()
        .iter()
        .filter(|row| Some(row.domain_id) != existing.as_ref().map(|e| e.domain_id))
        .cloned()
        .collect();
    rows.push(Domain {
        domain_id: if candidate.domain_id == 0 {
            i64::MAX
        } else {
            candidate.domain_id
        },
        ..candidate.clone()
    });
    let next = DomainMap::new(rows);
    let slugs: Vec<(i64, &str)> = forums
        .iter()
        .map(|forum| (forum.fid, forum.slug.as_str()))
        .collect();
    check_map(&next, &forum_url, &slugs).map_err(DomainError::Invalid)?;
    // Images: a newly chosen one must be an image uploaded by this member, passed checks and not in the trash; one
    // that was replaced and is no longer used by anyone enters the retention period.
    let before = existing
        .as_ref()
        .map_or([None, None], |e| [e.logo_aid, e.favicon_aid]);
    let after = [candidate.logo_aid, candidate.favicon_aid];
    check_images(db, &mut tx, actor, &before, &after).await?;
    let domain_id = match &existing {
        None => i64::try_from(
            db::query("INSERT INTO gonof_common_site_domain(site_url,site_name,canonical,logo_aid,favicon_aid,verification,login_methods,participation,language,status,version,dateline) VALUES(?,?,?,?,?,?,?,?,?,?,1,?)")
                .bind(&candidate.site_url)
                .bind(&candidate.site_name)
                .bind(candidate.canonical.as_str())
                .bind(candidate.logo_aid)
                .bind(candidate.favicon_aid)
                .bind(&candidate.verification)
                .bind(&candidate.login_methods)
                .bind(&candidate.participation)
                .bind(candidate.language.map(crate::i18n::Lang::tag))
                .bind(i64::from(candidate.enabled))
                .bind(now)
                .execute(db.observe(&mut *tx))
                .await?
                .last_insert_id(),
        )
        .map_err(anyhow::Error::new)?,
        Some(existing) => {
            let updated = db::query("UPDATE gonof_common_site_domain SET site_url=?,site_name=?,canonical=?,logo_aid=?,favicon_aid=?,verification=?,login_methods=?,participation=?,language=?,status=?,version=version+1,dateline=? WHERE domain_id=? AND version=?")
                .bind(&candidate.site_url)
                .bind(&candidate.site_name)
                .bind(candidate.canonical.as_str())
                .bind(candidate.logo_aid)
                .bind(candidate.favicon_aid)
                .bind(&candidate.verification)
                .bind(&candidate.login_methods)
                .bind(&candidate.participation)
                .bind(candidate.language.map(crate::i18n::Lang::tag))
                .bind(i64::from(candidate.enabled))
                .bind(now)
                .bind(existing.domain_id)
                .bind(existing.version)
                .execute(db.observe(&mut *tx))
                .await?
                .rows_affected();
            if updated != 1 {
                return Err(DomainError::Conflict(crate::i18n::current().domain_error_stale()));
            }
            existing.domain_id
        }
    };
    replace_links(db, &mut tx, domain_id, &candidate.forums).await?;
    release_images(db, &mut tx, &before, &after, now, orphan_seconds).await?;
    crate::runtime::audit(
        db,
        &mut tx,
        actor,
        if existing.is_some() {
            "site.domain.save"
        } else {
            "site.domain.create"
        },
        serde_json::json!({
            "domain_id": domain_id,
            "site_url": candidate.site_url,
            "from": existing.as_ref().map(|e| e.site_url.clone()),
            "forums": candidate.forums,
            "canonical": candidate.canonical.as_str(),
            "login_methods": candidate.login_methods,
            "participation": candidate.participation,
            "language": candidate.language.map(crate::i18n::Lang::tag),
            "enabled": candidate.enabled,
        }),
    )
    .await?;
    let saved_map = {
        let mut rows: Vec<Domain> = next
            .rows()
            .iter()
            .filter(|row| row.domain_id != i64::MAX)
            .cloned()
            .collect();
        rows.push(Domain {
            domain_id,
            ..candidate
        });
        DomainMap::new(rows)
    };
    let unshown = unshown(&saved_map, &forums);
    tx.commit().await?;
    Ok(Saved { domain_id, unshown })
}

/// Deletes a row (the primary domain cannot be deleted). Link rows are deleted first; a logo or favicon no longer used
/// by anyone enters the retention period.
pub async fn delete(
    db: &Database,
    global: &GlobalCache,
    actor: Actor,
    domain_id: i64,
    version: i64,
    orphan_seconds: i64,
) -> Result<(), DomainError> {
    let mut retry = 0_u64;
    loop {
        match delete_once(db, actor, domain_id, version, orphan_seconds).await {
            Err(error) if retry < 2 && error.retryable() => {
                retry += 1;
                tokio::time::sleep(std::time::Duration::from_millis(20 * retry)).await;
            }
            result => break result?,
        }
    }
    global.announce(&[Block::Domains, Block::Settings]).await?;
    Ok(())
}

async fn delete_once(
    db: &Database,
    actor: Actor,
    domain_id: i64,
    version: i64,
    orphan_seconds: i64,
) -> Result<(), DomainError> {
    if domain_id <= 0 || version <= 0 {
        return Err(DomainError::Invalid(
            crate::i18n::current().domain_error_id().into(),
        ));
    }
    let mut tx = db.begin_write().await?;
    let settings = crate::runtime::locked(db, &mut tx).await?;
    crate::runtime::admin_locked(db, &mut tx, actor).await?;
    lock_forums(db, &mut tx).await?;
    let map = lock_map(db, &mut tx).await?;
    let row = map.get(domain_id).cloned().ok_or(DomainError::NotFound)?;
    if row.site_url == settings.site.url {
        return Err(DomainError::Invalid(
            crate::i18n::current().domain_error_delete_main().into(),
        ));
    }
    if row.version != version {
        return Err(DomainError::Conflict(
            crate::i18n::current().domain_error_stale(),
        ));
    }
    db::query("DELETE FROM gonof_common_site_domain_forum WHERE domain_id=?")
        .bind(domain_id)
        .execute(db.observe(&mut *tx))
        .await?;
    db::query("DELETE FROM gonof_common_site_domain WHERE domain_id=? AND version=?")
        .bind(domain_id)
        .bind(version)
        .execute(db.observe(&mut *tx))
        .await?;
    let now = identity::now();
    let before = [row.logo_aid, row.favicon_aid];
    for aid in before.iter().flatten() {
        db::query("SELECT aid FROM gonof_forum_attachment_0 WHERE aid=? FOR UPDATE")
            .bind(aid)
            .fetch_optional(db.observe(&mut *tx))
            .await?;
    }
    release_images(db, &mut tx, &before, &[None, None], now, orphan_seconds).await?;
    // The domain's own skin choice goes with it (docs/skins.md, "Choosing a skin").
    crate::skins::selection::delete_domain(db, &mut tx, domain_id).await?;
    crate::runtime::audit(
        db,
        &mut tx,
        actor,
        "site.domain.delete",
        serde_json::json!({"domain_id": domain_id, "site_url": row.site_url}),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Replaces a row's link rows: deletes them all, then inserts them back in ascending order.
async fn replace_links(
    db: &Database,
    tx: &mut Tx<'_>,
    domain_id: i64,
    forums: &BTreeSet<i64>,
) -> Result<(), DomainError> {
    db::query("DELETE FROM gonof_common_site_domain_forum WHERE domain_id=?")
        .bind(domain_id)
        .execute(db.observe(&mut **tx))
        .await?;
    if forums.is_empty() {
        return Ok(());
    }
    let values = vec!["(?,?)"; forums.len()].join(",");
    let sql = format!("INSERT INTO gonof_common_site_domain_forum(domain_id,fid) VALUES{values}");
    let mut query = db::query(&sql);
    for fid in forums {
        query = query.bind(domain_id).bind(*fid);
    }
    query.execute(db.observe(&mut **tx)).await?;
    Ok(())
}

/// Locks the attachment rows of the old and new images and checks the newly chosen ones (same rules as brand images:
/// uploaded by this member or already bound, passed checks, an image, not past the retention period, not in the
/// trash; a favicon must be square and at least 32×32).
async fn check_images(
    db: &Database,
    tx: &mut Tx<'_>,
    actor: Actor,
    before: &[Option<i64>; 2],
    after: &[Option<i64>; 2],
) -> Result<(), DomainError> {
    let ids: BTreeSet<i64> = before.iter().chain(after).flatten().copied().collect();
    let now = identity::now();
    for id in ids {
        let row = db::query("SELECT a.uid,d.lifecycle,d.scan_status,d.mime_type,d.width,d.height,d.purge_after,d.trashed_at FROM gonof_forum_attachment a JOIN gonof_forum_attachment_0 d ON d.aid=a.aid WHERE a.aid=? FOR UPDATE")
            .bind(id)
            .fetch_optional(db.observe(&mut **tx))
            .await?
            .ok_or_else(|| DomainError::Invalid(crate::i18n::current().domain_error_image().into()))?;
        for (index, chosen) in after.iter().enumerate() {
            if *chosen != Some(id) {
                continue;
            }
            let bound = before[index] == Some(id);
            if (!bound && row.try_get::<i64, _>("uid")? != actor.uid)
                || db::text(&row, "lifecycle")? != "ready"
                || db::text(&row, "scan_status")? != "clean"
                || !db::text(&row, "mime_type")?.starts_with("image/")
                || row
                    .try_get::<Option<i64>, _>("purge_after")?
                    .is_some_and(|t| t <= now)
            {
                return Err(DomainError::Invalid(
                    crate::i18n::current().domain_error_image().into(),
                ));
            }
            if row.try_get::<Option<i64>, _>("trashed_at")?.is_some() {
                return Err(DomainError::Invalid(
                    crate::i18n::current().domain_error_image_trashed().into(),
                ));
            }
            if index == 1 {
                let width = row.try_get::<Option<i64>, _>("width")?.unwrap_or(0);
                let height = row.try_get::<Option<i64>, _>("height")?.unwrap_or(0);
                if width != height || width < 32 {
                    return Err(DomainError::Invalid(
                        crate::i18n::current().domain_error_favicon().into(),
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Newly bound images leave the retention period; replaced images with no remaining reference enter it (same as brand
/// images and forum icons). The caller has already locked the attachment rows.
async fn release_images(
    db: &Database,
    tx: &mut Tx<'_>,
    before: &[Option<i64>; 2],
    after: &[Option<i64>; 2],
    now: i64,
    orphan_seconds: i64,
) -> Result<(), DomainError> {
    for aid in after.iter().flatten() {
        db::query("UPDATE gonof_forum_attachment_0 SET purge_after=NULL WHERE aid=?")
            .bind(aid)
            .execute(db.observe(&mut **tx))
            .await?;
    }
    let dropped: BTreeSet<i64> = before
        .iter()
        .flatten()
        .filter(|aid| !after.contains(&Some(**aid)))
        .copied()
        .collect();
    for aid in dropped {
        let mut references = 0_i64;
        for (table, column) in [
            ("gonof_forum_attachment_reference", "aid"),
            ("gonof_common_member", "avatar_aid"),
            ("gonof_common_site_asset", "aid"),
            ("gonof_forum_forum", "icon_aid"),
            ("gonof_common_site_domain", "logo_aid"),
            ("gonof_common_site_domain", "favicon_aid"),
        ] {
            references +=
                db::query_scalar::<i64>(&format!("SELECT COUNT(*) FROM {table} WHERE {column}=?"))
                    .bind(aid)
                    .fetch_one(db.observe(&mut **tx))
                    .await?;
        }
        if references == 0 {
            db::query("UPDATE gonof_forum_attachment_0 SET purge_after=GREATEST(dateline,?),updated_at=GREATEST(dateline,?) WHERE aid=? AND lifecycle NOT IN ('purging','purged')")
                .bind(now.saturating_add(orphan_seconds))
                .bind(now)
                .bind(aid)
                .execute(db.observe(&mut **tx))
                .await?;
        }
    }
    Ok(())
}

/// Coupling on settings save (when the switch or the forum URL changes), called inside the save transaction of
/// [`crate::runtime`] after the exclusive lock on the settings rows. Returns whether the domain table changed (if so,
/// the `domains` block is broadcast as well).
///
/// - The forum URL cannot be empty while the switch is on.
/// - The forum URL changed: the row whose URL equals the old forum URL is changed to the new URL along with it;
///   rejected when the new URL is already registered as another row.
/// - The switch is (or stays) on and the table has no row whose URL equals the forum URL: creates the primary domain
///   row with all current top-level forums ticked.
pub async fn follow_settings(
    db: &Database,
    tx: &mut Tx<'_>,
    before: &SiteSettings,
    after: &SiteSettings,
) -> Result<bool, IdentityError> {
    let url_changed = before.site.url != after.site.url;
    let enabling = after.domains.enabled && !before.domains.enabled;
    if after.domains.enabled && after.site.url.is_empty() {
        return Err(IdentityError::Validation(if enabling {
            crate::i18n::current().domain_error_enable_without_url()
        } else {
            crate::i18n::current().domain_error_clear_url()
        }));
    }
    if !url_changed && !enabling {
        return Ok(false);
    }
    // Lock the top-level forums first (link rows are inserted for the primary domain), then the domain rows.
    let tops: Vec<(i64, String)> = if after.domains.enabled {
        db::query(
            "SELECT fid,slug FROM gonof_forum_forum ORDER BY fid LIMIT 10000 LOCK IN SHARE MODE",
        )
        .fetch_all(db.observe(&mut **tx))
        .await?
        .iter()
        .map(|row| -> anyhow::Result<(i64, String)> {
            Ok((row.try_get("fid")?, db::text(row, "slug")?))
        })
        .collect::<anyhow::Result<_>>()?
    } else {
        Vec::new()
    };
    let parents: BTreeSet<i64> = if after.domains.enabled {
        db::query_scalar::<i64>(
            "SELECT fid FROM gonof_forum_forum WHERE parent_fid IS NULL ORDER BY fid LIMIT 10000",
        )
        .fetch_all(db.observe(&mut **tx))
        .await?
        .into_iter()
        .collect()
    } else {
        BTreeSet::new()
    };
    let map = lock_map(db, tx).await?;
    let now = identity::now();
    let mut changed = false;
    if url_changed && !before.site.url.is_empty() && !after.site.url.is_empty() {
        let old_main = map.main(&before.site.url).map(|main| main.domain_id);
        if map.rows().iter().any(|row| {
            Some(row.domain_id) != old_main && place(&row.site_url) == place(&after.site.url)
        }) {
            return Err(IdentityError::Validation(
                crate::i18n::current().domain_error_url_registered(),
            ));
        }
        if let Some(id) = old_main {
            db::query("UPDATE gonof_common_site_domain SET site_url=?,version=version+1,dateline=? WHERE domain_id=?")
                .bind(&after.site.url)
                .bind(now)
                .bind(id)
                .execute(db.observe(&mut **tx))
                .await?;
            changed = true;
        }
    }
    let after_rows: Vec<Domain> = map
        .rows()
        .iter()
        .map(|row| {
            if row.site_url == before.site.url && url_changed && !after.site.url.is_empty() {
                Domain {
                    site_url: after.site.url.clone(),
                    ..row.clone()
                }
            } else {
                row.clone()
            }
        })
        .collect();
    let mut next = DomainMap::new(after_rows);
    if after.domains.enabled && next.main(&after.site.url).is_none() {
        let id = i64::try_from(
            db::query("INSERT INTO gonof_common_site_domain(site_url,site_name,canonical,logo_aid,favicon_aid,verification,login_methods,participation,status,version,dateline) VALUES(?,NULL,'self',NULL,NULL,NULL,NULL,NULL,1,1,?)")
                .bind(&after.site.url)
                .bind(now)
                .execute(db.observe(&mut **tx))
                .await?
                .last_insert_id(),
        )
        .map_err(anyhow::Error::new)?;
        replace_links(db, tx, id, &parents)
            .await
            .map_err(into_identity)?;
        let mut rows = next.rows().to_vec();
        rows.push(Domain {
            domain_id: id,
            site_url: after.site.url.clone(),
            site_name: None,
            canonical: Canonical::Own,
            logo_aid: None,
            favicon_aid: None,
            verification: None,
            login_methods: None,
            participation: None,
            language: None,
            enabled: true,
            version: 1,
            dateline: now,
            forums: parents.clone(),
        });
        next = DomainMap::new(rows);
        changed = true;
    }
    // The new forum URL must also follow the same structural rules (unique root path per host, no nested sub-paths,
    // avoiding forum URLs).
    if after.domains.enabled {
        let slugs: Vec<(i64, &str)> = tops
            .iter()
            .map(|(fid, slug)| (*fid, slug.as_str()))
            .collect();
        if check_map(&next, &after.site.url, &slugs).is_err() {
            return Err(IdentityError::Validation(
                crate::i18n::current().domain_error_url_conflict(),
            ));
        }
    }
    Ok(changed || enabling)
}

fn into_identity(error: DomainError) -> IdentityError {
    match error {
        DomainError::Internal(error) => IdentityError::Internal(error),
        other => IdentityError::Internal(anyhow::anyhow!(other.to_string())),
    }
}

/// Before saving a forum (new or slug changed): when a host has registered the root path, the slug must not equal the
/// first segment of any sub-path of that host. Also takes a shared lock on the domain rows, blocking concurrent domain
/// saves that register a sub-path. Called inside the forum save transaction after the forum row is locked.
pub async fn check_forum_slug(db: &Database, tx: &mut Tx<'_>, slug: &str) -> anyhow::Result<bool> {
    let rows = db::query(SHARE_SQL)
        .fetch_all(db.observe(&mut **tx))
        .await?;
    let mut enabled = Vec::new();
    for row in &rows {
        if row.try_get::<i64, _>("status")? == 1 {
            enabled.push(db::text(row, "site_url")?);
        }
    }
    let taken = enabled.iter().any(|url| {
        let (authority, prefix) = place(url);
        prefix.split('/').nth(1) == Some(slug)
            && enabled.iter().any(|other| place(other) == (authority, ""))
    });
    Ok(!taken)
}

/// After saving a forum: a forum turned into a child forum has its link rows deleted; a newly created
/// top-level forum gets one added to the primary domain row. Returns whether the domain table changed. Before the call,
/// [`check_forum_slug`] has already taken a shared lock on the domain rows.
pub async fn forum_saved(
    db: &Database,
    tx: &mut Tx<'_>,
    fid: i64,
    parent_fid: Option<i64>,
    created: bool,
) -> anyhow::Result<bool> {
    if parent_fid.is_some() {
        let removed = db::query("DELETE FROM gonof_common_site_domain_forum WHERE fid=?")
            .bind(fid)
            .execute(db.observe(&mut **tx))
            .await?
            .rows_affected();
        return Ok(removed > 0);
    }
    if !created {
        return Ok(false);
    }
    let forum_url: Option<String> =
        db::query_scalar("SELECT svalue FROM gonof_common_settings WHERE skey='settings.site.url'")
            .fetch_optional(db.observe(&mut **tx))
            .await?;
    let Some(forum_url) = forum_url.filter(|url| !url.is_empty()) else {
        return Ok(false);
    };
    let main: Option<i64> =
        db::query_scalar("SELECT domain_id FROM gonof_common_site_domain WHERE site_url=?")
            .bind(&forum_url)
            .fetch_optional(db.observe(&mut **tx))
            .await?;
    let Some(main) = main else {
        return Ok(false);
    };
    db::query("INSERT INTO gonof_common_site_domain_forum(domain_id,fid) VALUES(?,?)")
        .bind(main)
        .bind(fid)
        .execute(db.observe(&mut **tx))
        .await?;
    Ok(true)
}

/// Before deleting a forum: deletes its link rows (the foreign key has no `ON DELETE`). Returns whether the domain table
/// changed.
pub async fn forum_deleted(db: &Database, tx: &mut Tx<'_>, fid: i64) -> anyhow::Result<bool> {
    let removed = db::query("DELETE FROM gonof_common_site_domain_forum WHERE fid=?")
        .bind(fid)
        .execute(db.observe(&mut **tx))
        .await?
        .rows_affected();
    Ok(removed > 0)
}

#[cfg(test)]
mod tests {
    use super::super::tests::row;
    use super::*;

    const FORUMS: &[(i64, &str)] = &[(1, "general"), (2, "feedback"), (3, "vip")];

    #[test]
    fn nested_paths_ignore_the_root() {
        assert!(nested("/bbs", "/bbs/feedback"));
        assert!(nested("/bbs", "/bbs"));
        assert!(!nested("/bbs", "/bbsx"));
        assert!(!nested("", "/bbs"));
        assert!(nested("", ""));
    }

    #[test]
    fn check_map_enforces_places_hosts_and_single_homes() {
        let main = "https://demo.com";
        let ok = DomainMap::new(vec![
            row(1, main, &[1, 2, 3]),
            row(2, "https://demo2.com", &[2, 3]),
            row(3, "https://demo.com/help", &[2]),
        ]);
        let mut fixed = ok.clone();
        fixed.rows[2].canonical = Canonical::Main;
        assert_eq!(check_map(&fixed, main, FORUMS), Ok(()));
        // Differing only in scheme is still the same location.
        let same = DomainMap::new(vec![
            row(1, main, &[1]),
            row(2, "http://demo2.com", &[]),
            row(3, "https://demo2.com", &[]),
        ]);
        assert!(check_map(&same, main, FORUMS).is_err());
        // Two root paths on the same host (same host and port).
        let roots = DomainMap::new(vec![row(1, main, &[1]), row(2, "http://demo.com", &[])]);
        assert!(check_map(&roots, main, FORUMS).is_err());
        // Nested sub-paths.
        let nested = DomainMap::new(vec![
            row(1, "https://x.com/bbs", &[1]),
            row(2, "https://x.com/bbs/feedback", &[]),
        ]);
        assert!(check_map(&nested, "https://x.com/bbs", FORUMS).is_err());
        // With a root path plus sub-paths, the first segment of a sub-path avoids forum URLs and system paths.
        for clash in [
            "https://demo.com/feedback",
            "https://demo.com/manage",
            "https://demo.com/thread-1",
        ] {
            let map = DomainMap::new(vec![row(1, main, &[1]), row(2, clash, &[])]);
            assert!(check_map(&map, main, FORUMS).is_err(), "{clash}");
        }
        // Hosts without a root path are not checked against each other.
        let free = DomainMap::new(vec![
            row(1, main, &[1]),
            row(2, "https://x.com/feedback", &[]),
        ]);
        assert_eq!(check_map(&free, main, FORUMS), Ok(()));
        // One forum with two canonical domains.
        assert!(check_map(&ok, main, FORUMS).is_err());
        // When canonical points to the primary domain, the primary domain must show the same forums.
        let mut lacking =
            DomainMap::new(vec![row(1, main, &[1]), row(2, "https://demo2.com", &[2])]);
        lacking.rows[1].canonical = Canonical::Main;
        assert!(check_map(&lacking, main, FORUMS).is_err());
        // Disabled rows take no part in the canonical and path rules, only in "same location".
        let mut stopped = ok.clone();
        stopped.rows[2].enabled = false;
        assert_eq!(check_map(&stopped, main, FORUMS), Ok(()));
    }
}
