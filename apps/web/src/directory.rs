//! Verified members: categories, granting and expiry, the directory, and the small badges on floors and profile
//! pages.
//!
//! A member has at most one verification (`gonof_common_member_verification`), written only by administrators.
//! Verifications that are frozen, expired, or whose account deletion is in progress are not shown anywhere: the
//! directory filters them out in SQL (list filtering and paging stay in SQL), while floors and profile pages check in
//! code after fetching by primary key. Verification status lives in its own table rather than on the member row, so
//! granting and expiring it does not contend for the member row lock that posting transactions hold.
use crate::db::Tx;
use std::collections::{BTreeSet, HashMap};

use crate::{
    db::{self, Database, DbRow},
    identity::{Actor, IdentityError},
};

type Result<T> = std::result::Result<T, IdentityError>;

/// Maximum number of people shown per category section in the directory; a 51st person shows the "More verified …"
/// link.
pub const SECTION_SIZE: usize = 50;
/// Rows per page in the admin list.
pub const ADMIN_PAGE_SIZE: usize = 20;
/// The range of "expiring soon" in the admin area.
pub const EXPIRING_WINDOW: i64 = 30 * 86_400;

/// Verification categories. Display order is the order of [`Category::ALL`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    Merchant,
    Agent,
    Blogger,
    Webmaster,
    Channel,
}

impl Category {
    pub const ALL: [Self; 5] = [
        Self::Merchant,
        Self::Agent,
        Self::Blogger,
        Self::Webmaster,
        Self::Channel,
    ];

    pub fn slug(self) -> &'static str {
        match self {
            Self::Merchant => "merchant",
            Self::Agent => "agent",
            Self::Blogger => "blogger",
            Self::Webmaster => "webmaster",
            Self::Channel => "channel",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Merchant => crate::i18n::current().site_verified_merchant(),
            Self::Agent => crate::i18n::current().site_verified_agent(),
            Self::Blogger => crate::i18n::current().site_verified_blogger(),
            Self::Webmaster => crate::i18n::current().site_verified_webmaster(),
            Self::Channel => crate::i18n::current().site_verified_channel(),
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|item| item.slug() == value)
    }
}

/// The verification shown on floors and personal profile pages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Badge {
    pub category: Category,
    pub title: String,
    pub site_url: Option<String>,
}

impl Badge {
    pub fn label(&self) -> &'static str {
        self.category.label()
    }

    /// The linked website is shown as its host name; empty string when not filled in.
    pub fn site_host(&self) -> String {
        self.site_url
            .as_deref()
            .map(display_host)
            .unwrap_or_default()
    }

    pub fn site_href(&self) -> &str {
        self.site_url.as_deref().unwrap_or("")
    }
}

/// A URL is shown as its host name (with a leading `www.` removed); shown as-is when it cannot be parsed.
pub fn display_host(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .map(|host| host.strip_prefix("www.").unwrap_or(&host).to_owned())
        .unwrap_or_else(|| url.to_owned())
}

/// Whether a single verification should be shown right now: not expired, member not frozen and account normal (not
/// shown while account deletion is in progress).
fn shown(expires_at: Option<i64>, freeze: i64, account_state: &str, now: i64) -> bool {
    expires_at.is_none_or(|at| at > now) && freeze == 0 && account_state == "active"
}

fn category_of(row: &DbRow) -> Result<Category> {
    let value = db::text(row, "category").map_err(IdentityError::Internal)?;
    Category::parse(&value).ok_or_else(|| {
        IdentityError::Internal(anyhow::anyhow!("unknown verification category {value}"))
    })
}

/// Verifications shown right now for a set of members, fetched in bulk by primary key (the authors of one page of
/// floors), never entering the shared cache.
pub async fn badges(db: &Database, uids: &[i64], now: i64) -> Result<HashMap<i64, Badge>> {
    let uids: BTreeSet<i64> = uids.iter().copied().filter(|uid| *uid > 0).collect();
    if uids.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = vec!["?"; uids.len()].join(",");
    let sql = format!(
        "SELECT v.uid,v.category,v.title,v.site_url,v.expires_at,m.freeze,m.account_state \
         FROM gonof_common_member_verification v JOIN gonof_common_member m ON m.uid=v.uid \
         WHERE v.uid IN ({placeholders})"
    );
    let mut query = db::query(&sql);
    for uid in &uids {
        query = query.bind(*uid);
    }
    let mut badges = HashMap::new();
    for row in query.fetch_all(db.executor()).await? {
        let account_state = db::text(&row, "account_state")?;
        if !shown(
            row.try_get("expires_at")?,
            row.try_get("freeze")?,
            &account_state,
            now,
        ) {
            continue;
        }
        badges.insert(
            row.try_get("uid")?,
            Badge {
                category: category_of(&row)?,
                title: db::text(&row, "title")?,
                site_url: row.try_get("site_url")?,
            },
        );
    }
    Ok(badges)
}

/// Same as [`badges`], with the authors' freeze and account status already read (the thread page's
/// [`crate::growth::page_authors`]): only reads the members in `active`, from a single table by primary key, without
/// joining the member table again.
pub async fn badges_of_active(
    db: &Database,
    active: &BTreeSet<i64>,
    now: i64,
) -> Result<HashMap<i64, Badge>> {
    if active.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = vec!["?"; active.len()].join(",");
    let sql = format!(
        "SELECT uid,category,title,site_url,expires_at FROM gonof_common_member_verification WHERE uid IN ({placeholders})"
    );
    let mut query = db::query(&sql);
    for uid in active {
        query = query.bind(*uid);
    }
    let mut badges = HashMap::new();
    for row in query.fetch_all(db.executor()).await? {
        if !shown(row.try_get("expires_at")?, 0, "active", now) {
            continue;
        }
        badges.insert(
            row.try_get("uid")?,
            Badge {
                category: category_of(&row)?,
                title: db::text(&row, "title")?,
                site_url: row.try_get("site_url")?,
            },
        );
    }
    Ok(badges)
}

/// The verification shown right now for one member.
pub async fn badge(db: &Database, uid: i64, now: i64) -> Result<Option<Badge>> {
    Ok(badges(db, &[uid], now).await?.remove(&uid))
}

/// One row in the directory.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Listed {
    pub uid: i64,
    pub username: String,
    pub avatar: i64,
    pub avatar_aid: Option<i64>,
    pub title: String,
    pub site_url: Option<String>,
    pub granted_at: i64,
}

impl Listed {
    pub fn site_host(&self) -> String {
        self.site_url
            .as_deref()
            .map(display_host)
            .unwrap_or_default()
    }

    pub fn site_href(&self) -> &str {
        self.site_url.as_deref().unwrap_or("")
    }
}

/// One directory section: page `page` of a category in verification order ([`SECTION_SIZE`] per page), and whether
/// there is more after it.
///
/// Same scope as member search and the leaderboard: members whose space is "Only me" are not listed,
/// those with "Logged-in members" are listed only to logged-in visitors, and people the visitor blocked are not listed;
/// frozen, expired and deletion-in-progress ones are not listed either. Uses the category equality and index order of
/// `gonof_verification_directory`, with `eq_ref` on the member table by primary key.
pub async fn section(
    db: &Database,
    category: Category,
    viewer_uid: Option<i64>,
    now: i64,
    page: usize,
) -> Result<(Vec<Listed>, bool)> {
    let page = page.max(1);
    let sql = format!(
        "SELECT v.uid,v.title,v.site_url,v.granted_at,m.username,m.avatar,m.avatar_aid \
         FROM gonof_common_member_verification v JOIN gonof_common_member m ON m.uid=v.uid \
         WHERE v.category=? AND (v.expires_at IS NULL OR v.expires_at>?) \
         AND m.freeze=0 AND m.account_state='active' AND {} \
         ORDER BY v.granted_at,v.uid LIMIT ? OFFSET ?",
        crate::content::members::listed_scope_sql(viewer_uid)
    );
    let rows = db::query(&sql)
        .bind(category.slug())
        .bind(now)
        .bind((SECTION_SIZE + 1) as i64)
        .bind(((page - 1) * SECTION_SIZE) as i64)
        .fetch_all(db.executor())
        .await?;
    let more = rows.len() > SECTION_SIZE;
    let listed = rows
        .iter()
        .take(SECTION_SIZE)
        .map(|row| {
            Ok(Listed {
                uid: row.try_get("uid")?,
                username: db::text(row, "username")?,
                avatar: row.try_get("avatar")?,
                avatar_aid: row.try_get("avatar_aid")?,
                title: db::text(row, "title")?,
                site_url: row.try_get("site_url")?,
                granted_at: row.try_get("granted_at")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((listed, more))
}

/// The first members of a verification category for the public (the `members.verified` data source,
/// docs/data.md): the same rows as [`section`], without the viewer's blocks (the caller removes them), so one result
/// serves every visitor of a class; `signed_in` adds spaces shown to "Logged-in members". At most `limit`, on
/// `gonof_verification_directory` in index order.
pub async fn listed(
    db: &Database,
    category: Category,
    signed_in: bool,
    now: i64,
    limit: i64,
) -> Result<Vec<Listed>> {
    let space = if signed_in {
        "m.space_visibility IN ('public','members')"
    } else {
        "m.space_visibility='public'"
    };
    let sql = format!(
        "SELECT v.uid,v.title,v.site_url,v.granted_at,m.username,m.avatar,m.avatar_aid \
         FROM gonof_common_member_verification v JOIN gonof_common_member m ON m.uid=v.uid \
         WHERE v.category=? AND (v.expires_at IS NULL OR v.expires_at>?) \
         AND m.freeze=0 AND m.account_state='active' AND {space} \
         ORDER BY v.granted_at,v.uid LIMIT ?"
    );
    db::query(&sql)
        .bind(category.slug())
        .bind(now)
        .bind(limit)
        .fetch_all(db.executor())
        .await?
        .iter()
        .map(|row| {
            Ok(Listed {
                uid: row.try_get("uid")?,
                username: db::text(row, "username")?,
                avatar: row.try_get("avatar")?,
                avatar_aid: row.try_get("avatar_aid")?,
                title: db::text(row, "title")?,
                site_url: row.try_get("site_url")?,
                granted_at: row.try_get("granted_at")?,
            })
        })
        .collect()
}

/// All fields of one verification (admin area).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verification {
    pub uid: i64,
    pub category: Category,
    pub title: String,
    pub site_url: Option<String>,
    pub granted_by: i64,
    pub granted_at: i64,
    pub reviewed_at: i64,
    pub expires_at: Option<i64>,
}

fn verification_of(row: &DbRow) -> Result<Verification> {
    Ok(Verification {
        uid: row.try_get("uid")?,
        category: category_of(row)?,
        title: db::text(row, "title")?,
        site_url: row.try_get("site_url")?,
        granted_by: row.try_get("granted_by")?,
        granted_at: row.try_get("granted_at")?,
        reviewed_at: row.try_get("reviewed_at")?,
        expires_at: row.try_get("expires_at")?,
    })
}

const COLUMNS: &str =
    "v.uid,v.category,v.title,v.site_url,v.granted_by,v.granted_at,v.reviewed_at,v.expires_at";

/// Filters of the admin list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    All,
    Category(Category),
    /// Expires within 30 days (not yet expired).
    Expiring,
    Expired,
}

impl Filter {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "" | "all" => Some(Self::All),
            "expiring" => Some(Self::Expiring),
            "expired" => Some(Self::Expired),
            other => Category::parse(other).map(Self::Category),
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Expiring => "expiring",
            Self::Expired => "expired",
            Self::Category(category) => category.slug(),
        }
    }
}

/// One row of the admin list: the verification itself plus the member's current status.
#[derive(Clone, Debug)]
pub struct AdminRow {
    pub verification: Verification,
    pub username: String,
    pub freeze: bool,
    pub account_state: String,
}

impl AdminRow {
    /// Whether it is shown on the front end right now, and the reason when it is not.
    pub fn state_label(&self, now: i64) -> &'static str {
        let t = crate::i18n::current();
        if self.account_state != "active" {
            t.site_manage_hidden_deleting()
        } else if self.freeze {
            t.site_manage_hidden_frozen()
        } else if self.verification.expires_at.is_some_and(|at| at <= now) {
            t.site_manage_hidden_expired()
        } else {
            t.site_manage_shown()
        }
    }
}

/// Admin list, [`ADMIN_PAGE_SIZE`] rows per page; returns this page and whether there is a next page.
pub async fn admin_list(
    db: &Database,
    filter: Filter,
    page: usize,
    now: i64,
) -> Result<(Vec<AdminRow>, bool)> {
    let page = page.max(1);
    let (condition, order) = match filter {
        Filter::All => ("1=1", "v.uid DESC"),
        Filter::Category(_) => ("v.category=?", "v.granted_at,v.uid"),
        Filter::Expiring => ("v.expires_at>? AND v.expires_at<=?", "v.expires_at,v.uid"),
        Filter::Expired => ("v.expires_at<=?", "v.expires_at,v.uid"),
    };
    let sql = format!(
        "SELECT {COLUMNS},m.username,m.freeze,m.account_state \
         FROM gonof_common_member_verification v JOIN gonof_common_member m ON m.uid=v.uid \
         WHERE {condition} ORDER BY {order} LIMIT ? OFFSET ?"
    );
    let mut query = db::query(&sql);
    query = match filter {
        Filter::All => query,
        Filter::Category(category) => query.bind(category.slug()),
        Filter::Expiring => query.bind(now).bind(now + EXPIRING_WINDOW),
        Filter::Expired => query.bind(now),
    };
    let rows = query
        .bind((ADMIN_PAGE_SIZE + 1) as i64)
        .bind(((page - 1) * ADMIN_PAGE_SIZE) as i64)
        .fetch_all(db.executor())
        .await?;
    let more = rows.len() > ADMIN_PAGE_SIZE;
    let list = rows
        .iter()
        .take(ADMIN_PAGE_SIZE)
        .map(|row| {
            Ok(AdminRow {
                verification: verification_of(row)?,
                username: db::text(row, "username")?,
                freeze: row.try_get::<i64, _>("freeze")? != 0,
                account_state: db::text(row, "account_state")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((list, more))
}

/// What the admin edit page needs: the member's current status and the existing verification (possibly none). `None`
/// when the member does not exist or has been deleted.
pub struct Subject {
    pub uid: i64,
    pub username: String,
    pub freeze: bool,
    pub account_state: String,
    pub verification: Option<Verification>,
}

pub async fn subject(db: &Database, uid: i64) -> Result<Option<Subject>> {
    let Some(member) =
        db::query("SELECT uid,username,freeze,account_state FROM gonof_common_member WHERE uid=?")
            .bind(uid)
            .fetch_optional(db.executor())
            .await?
    else {
        return Ok(None);
    };
    let account_state = db::text(&member, "account_state")?;
    if account_state == "deleted" {
        return Ok(None);
    }
    let verification = db::query(&format!(
        "SELECT {COLUMNS} FROM gonof_common_member_verification v WHERE v.uid=?"
    ))
    .bind(uid)
    .fetch_optional(db.executor())
    .await?
    .map(|row| verification_of(&row))
    .transpose()?;
    Ok(Some(Subject {
        uid,
        username: db::text(&member, "username")?,
        freeze: member.try_get::<i64, _>("freeze")? != 0,
        account_state,
        verification,
    }))
}

/// Linked website: only http/https, with a host name, without user name or password, ≤ 500 characters (the same check
/// as news sources).
/// The empty string means not filled in.
pub fn site_url(value: &str) -> Result<Option<String>> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let url = url::Url::parse(value).map_err(|_| {
        IdentityError::Validation(crate::i18n::current().site_manage_error_site_scheme())
    })?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || value.chars().count() > 500
    {
        return Err(IdentityError::Validation(
            crate::i18n::current().site_manage_error_site(),
        ));
    }
    Ok(Some(value.to_owned()))
}

/// One line of plain text: leading and trailing whitespace trimmed, no control characters accepted.
fn plain_line(value: &str, max: usize) -> Option<String> {
    let value = value.trim();
    (value.chars().count() <= max && !value.chars().any(char::is_control)).then(|| value.to_owned())
}

/// Verification content submitted by the admin form.
#[derive(Clone, Debug)]
pub struct Grant {
    pub category: String,
    pub title: String,
    pub site_url: String,
    /// `None` means permanent.
    pub expires_at: Option<i64>,
}

/// Result of granting, modifying or renewing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Saved {
    Created,
    Updated,
    Unchanged,
}

/// Gate (admin moderation is allowed during read-only periods) → administrator re-check → lock the target member row
/// → lock the verification row.
async fn lock_subject(db: &Database, tx: &mut Tx<'_>, actor: Actor, uid: i64) -> Result<String> {
    crate::runtime::guard_write(db, tx, true).await?;
    crate::runtime::admin_locked(db, tx, actor).await?;
    let state: String =
        db::query_scalar("SELECT account_state FROM gonof_common_member WHERE uid=? FOR UPDATE")
            .bind(uid)
            .fetch_optional(db.observe(&mut **tx))
            .await?
            .ok_or(IdentityError::NotFound)?;
    Ok(state)
}

async fn log(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: i64,
    operator: i64,
    kind: &str,
    data: serde_json::Value,
    now: i64,
) -> Result<()> {
    db::query("INSERT INTO gonof_common_log(uid,operationuid,type,data,source,dateline) VALUES(?,?,?,?,'web',?)")
        .bind(uid)
        .bind(operator)
        .bind(kind)
        .bind(data.to_string())
        .bind(now)
        .execute(db.observe(&mut **tx))
        .await?;
    Ok(())
}

/// Grant, modify or renew (administrators only). The form is compared field by field with the existing record, and
/// nothing is written when nothing changed, so replays and duplicate submissions add no extra log entries. Frozen
/// members can be granted one (it is not shown while frozen); members whose deletion is in progress cannot. Log type
/// `identity.verification.*`, deleted together with the member's own `identity.%` logs when the deletion executes.
pub async fn save(db: &Database, actor: Actor, uid: i64, input: Grant, now: i64) -> Result<Saved> {
    let category = Category::parse(input.category.trim()).ok_or(IdentityError::Validation(
        crate::i18n::current().site_manage_error_category(),
    ))?;
    let title = plain_line(&input.title, 60)
        .filter(|title| !title.is_empty())
        .ok_or(IdentityError::Validation(
            crate::i18n::current().site_manage_error_title(),
        ))?;
    let site_url = site_url(&input.site_url)?;
    let mut tx = db.begin_write().await?;
    if lock_subject(db, &mut tx, actor, uid).await? != "active" {
        return Err(IdentityError::Validation(
            crate::i18n::current().site_manage_error_deleting(),
        ));
    }
    let existing = db::query(&format!(
        "SELECT {COLUMNS} FROM gonof_common_member_verification v WHERE v.uid=? FOR UPDATE"
    ))
    .bind(uid)
    .fetch_optional(db.observe(&mut *tx))
    .await?
    .map(|row| verification_of(&row))
    .transpose()?;
    // A newly entered expiry date must be in the future; bringing back an already expired date unchanged (only editing
    // the description) is not blocked.
    if input.expires_at != existing.as_ref().and_then(|v| v.expires_at)
        && input.expires_at.is_some_and(|at| at <= now)
    {
        return Err(IdentityError::Validation(
            crate::i18n::current().site_manage_error_past(),
        ));
    }
    let saved = match &existing {
        None => {
            db::query("INSERT INTO gonof_common_member_verification(uid,category,title,site_url,granted_by,granted_at,reviewed_at,expires_at) VALUES(?,?,?,?,?,?,?,?)")
                .bind(uid)
                .bind(category.slug())
                .bind(&title)
                .bind(&site_url)
                .bind(actor.uid)
                .bind(now)
                .bind(now)
                .bind(input.expires_at)
                .execute(db.observe(&mut *tx))
                .await?;
            log(
                db,
                &mut tx,
                uid,
                actor.uid,
                "identity.verification.grant",
                serde_json::json!({"category": category.slug(), "expires_at": input.expires_at}),
                now,
            )
            .await?;
            Saved::Created
        }
        Some(old) => {
            let mut changed = Vec::new();
            if old.category != category {
                changed.push("category");
            }
            if old.title != title {
                changed.push("title");
            }
            if old.site_url != site_url {
                changed.push("site_url");
            }
            if old.expires_at != input.expires_at {
                changed.push("expires_at");
            }
            if changed.is_empty() {
                Saved::Unchanged
            } else {
                db::query("UPDATE gonof_common_member_verification SET category=?,title=?,site_url=?,expires_at=?,reviewed_at=? WHERE uid=?")
                    .bind(category.slug())
                    .bind(&title)
                    .bind(&site_url)
                    .bind(input.expires_at)
                    .bind(now)
                    .bind(uid)
                    .execute(db.observe(&mut *tx))
                    .await?;
                let kind = if changed == ["expires_at"] {
                    "identity.verification.renew"
                } else {
                    "identity.verification.update"
                };
                log(
                    db,
                    &mut tx,
                    uid,
                    actor.uid,
                    kind,
                    serde_json::json!({
                        "category": category.slug(),
                        "expires_at": input.expires_at,
                        "changed": changed,
                    }),
                    now,
                )
                .await?;
                Saved::Updated
            }
        }
    };
    tx.commit().await?;
    Ok(saved)
}

/// Revoke a verification (administrators only, reason required). Writes nothing and returns `false` when there is no
/// verification.
pub async fn revoke(db: &Database, actor: Actor, uid: i64, reason: &str, now: i64) -> Result<bool> {
    let reason = crate::administration::reason(reason)?.to_owned();
    let mut tx = db.begin_write().await?;
    lock_subject(db, &mut tx, actor, uid).await?;
    let category: Option<String> = db::query_scalar(
        "SELECT category FROM gonof_common_member_verification WHERE uid=? FOR UPDATE",
    )
    .bind(uid)
    .fetch_optional(db.observe(&mut *tx))
    .await?;
    let Some(category) = category else {
        return Ok(false);
    };
    db::query("DELETE FROM gonof_common_member_verification WHERE uid=?")
        .bind(uid)
        .execute(db.observe(&mut *tx))
        .await?;
    log(
        db,
        &mut tx,
        uid,
        actor.uid,
        "identity.verification.revoke",
        serde_json::json!({"category": category, "reason": reason}),
        now,
    )
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// When account deletion executes (the caller already holds the member row lock): deletes the verification, changes
/// pending friend link applications to withdrawn, and keeps links already on display.
pub async fn forget_member(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: i64,
    now: i64,
) -> anyhow::Result<()> {
    db::query("DELETE FROM gonof_common_member_verification WHERE uid=?")
        .bind(uid)
        .execute(db.observe(&mut **tx))
        .await?;
    db::query("UPDATE gonof_common_friend_link SET status='withdrawn',live_domain=NULL,handler_uid=NULL,handled_at=?,updated_at=? WHERE owner_uid=? AND status='pending'")
        .bind(now)
        .bind(now)
        .bind(uid)
        .execute(db.observe(&mut **tx))
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories_round_trip_in_display_order() {
        let labels: Vec<_> = Category::ALL.iter().map(|c| c.label()).collect();
        assert_eq!(
            labels,
            ["Merchant", "Agent", "Blogger", "Webmaster", "Channel owner"]
        );
        for category in Category::ALL {
            assert_eq!(Category::parse(category.slug()), Some(category));
        }
        assert_eq!(Category::parse("intern"), None);
    }

    #[test]
    fn site_url_accepts_only_plain_http_links() {
        assert_eq!(site_url("  ").unwrap(), None);
        assert_eq!(
            site_url(" https://example.com/a ").unwrap().as_deref(),
            Some("https://example.com/a")
        );
        for bad in [
            "javascript:alert(1)",
            "ftp://example.com",
            "https://user:pw@example.com",
            "example.com",
            "data:text/html,1",
        ] {
            assert!(site_url(bad).is_err(), "{bad}");
        }
        assert!(site_url(&format!("https://example.com/{}", "a".repeat(500))).is_err());
    }

    #[test]
    fn badges_hide_when_expired_frozen_or_closing() {
        assert!(shown(None, 0, "active", 100));
        assert!(shown(Some(101), 0, "active", 100));
        assert!(!shown(Some(100), 0, "active", 100));
        assert!(!shown(None, 1, "active", 100));
        assert!(!shown(None, 0, "deleting", 100));
    }

    #[test]
    fn hosts_drop_the_www_prefix() {
        assert_eq!(display_host("https://www.example.com/x"), "example.com");
        assert_eq!(display_host("http://blog.example.com"), "blog.example.com");
    }

    #[test]
    fn filters_parse_categories_and_expiry_views() {
        assert_eq!(Filter::parse(""), Some(Filter::All));
        assert_eq!(Filter::parse("expiring"), Some(Filter::Expiring));
        assert_eq!(
            Filter::parse("blogger"),
            Some(Filter::Category(Category::Blogger))
        );
        assert_eq!(Filter::parse("nope"), None);
    }
}
