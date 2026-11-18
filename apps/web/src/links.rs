//! Friend links: category directory, home page sidebar, member applications and admin review.
//!
//! Applications and links share one table `gonof_common_friend_link`: one row is one record of a website on this
//! site, and an application is simply a link pending review. States only allow pending → active / rejected /
//! withdrawn and active → removed; rejected, withdrawn and removed are all terminal. `live_domain` equals `domain`
//! while pending or active and is NULL otherwise; the unique key guarantees that the same website has at most one
//! record under review or listed at a time.

use crate::db::Tx;
use crate::{
    db::{self, Database, DbRow},
    identity::{self, Actor, IdentityError},
};

type Result<T> = std::result::Result<T, IdentityError>;

/// Hard limit on the number of home page sidebar entries: the upper bound of `settings.links.home_count` (1–50). The
/// process cache fetches this many and the page then truncates by the setting, so changing the setting needs no new
/// cache key.
pub const HOME_MAX: usize = 50;
/// Maximum number of entries on the directory page at once; when this many are active, the admin area stops
/// approving new ones.
pub const DIRECTORY_LIMIT: usize = 500;
/// Entries per page in the admin list.
pub const ADMIN_PAGE_SIZE: usize = 20;
/// Maximum number of finished applications (withdrawn, rejected, removed) listed under "My applications". Those still
/// pending or active are always listed: a member has at most 20 (the upper bound of `settings.links.live_per_member`).
pub const MINE_LIMIT: usize = 20;

/// Friend link categories; the display order is [`LinkCategory::ALL`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LinkCategory {
    Forum,
    Blog,
    Directory,
    Tools,
    Other,
}

impl LinkCategory {
    pub const ALL: [Self; 5] = [
        Self::Forum,
        Self::Blog,
        Self::Directory,
        Self::Tools,
        Self::Other,
    ];

    pub fn slug(self) -> &'static str {
        match self {
            Self::Forum => "forum",
            Self::Blog => "blog",
            Self::Directory => "directory",
            Self::Tools => "tools",
            Self::Other => "other",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Forum => crate::i18n::current().site_links_kind_forum(),
            Self::Blog => crate::i18n::current().site_links_kind_blog(),
            Self::Directory => crate::i18n::current().site_links_kind_directory(),
            Self::Tools => crate::i18n::current().site_links_kind_tools(),
            Self::Other => crate::i18n::current().site_links_kind_other(),
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|item| item.slug() == value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Pending,
    Active,
    Rejected,
    Withdrawn,
    Removed,
}

impl Status {
    pub const ALL: [Self; 5] = [
        Self::Pending,
        Self::Active,
        Self::Rejected,
        Self::Withdrawn,
        Self::Removed,
    ];

    pub fn slug(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Rejected => "rejected",
            Self::Withdrawn => "withdrawn",
            Self::Removed => "removed",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => crate::i18n::current().site_links_status_pending(),
            Self::Active => crate::i18n::current().site_links_status_active(),
            Self::Rejected => crate::i18n::current().site_links_status_rejected(),
            Self::Withdrawn => crate::i18n::current().site_links_status_withdrawn(),
            Self::Removed => crate::i18n::current().site_links_status_removed(),
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|item| item.slug() == value)
    }
}

/// All fields of one friend link.
#[derive(Clone, Debug)]
pub struct Link {
    pub id: i64,
    pub status: Status,
    pub category: LinkCategory,
    pub name: String,
    pub url: String,
    pub domain: String,
    pub description: String,
    pub backlink_url: Option<String>,
    pub note: String,
    pub home: bool,
    pub displayorder: i64,
    pub owner_uid: Option<i64>,
    /// The applicant's current username; empty for links added by an administrator, "Deleted user" for deleted
    /// accounts.
    pub owner_name: String,
    pub resolution: String,
    pub dateline: i64,
    pub handled_at: Option<i64>,
    /// Raised by every write to the row; admin forms carry it so that a form read before another change is refused.
    pub version: i64,
}

impl Link {
    pub fn status_label(&self) -> &'static str {
        self.status.label()
    }

    pub fn category_label(&self) -> &'static str {
        self.category.label()
    }

    pub fn is_pending(&self) -> bool {
        self.status == Status::Pending
    }

    pub fn is_active(&self) -> bool {
        self.status == Status::Active
    }

    pub fn backlink(&self) -> &str {
        self.backlink_url.as_deref().unwrap_or("")
    }
}

const COLUMNS: &str = "l.id,l.status,l.category,l.name,l.url,l.domain,l.description,l.backlink_url,l.note,l.home,l.displayorder,l.owner_uid,l.resolution,l.dateline,l.handled_at,l.version,m.username AS owner_username,m.account_state AS owner_state";
const FROM: &str =
    "FROM gonof_common_friend_link l LEFT JOIN gonof_common_member m ON m.uid=l.owner_uid";

fn link_of(row: &DbRow) -> Result<Link> {
    let status = db::text(row, "status")?;
    let category = db::text(row, "category")?;
    let owner_name = match row.try_get::<Option<String>, _>("owner_state")?.as_deref() {
        None => String::new(),
        Some("deleted") => crate::i18n::current().common_deleted_user().to_owned(),
        Some(_) => row
            .try_get::<Option<String>, _>("owner_username")?
            .unwrap_or_default(),
    };
    Ok(Link {
        id: row.try_get("id")?,
        status: Status::parse(&status).ok_or_else(|| {
            IdentityError::Internal(anyhow::anyhow!("unknown link status {status}"))
        })?,
        category: LinkCategory::parse(&category).ok_or_else(|| {
            IdentityError::Internal(anyhow::anyhow!("unknown link category {category}"))
        })?,
        name: db::text(row, "name")?,
        url: db::text(row, "url")?,
        domain: db::text(row, "domain")?,
        description: db::text(row, "description")?,
        backlink_url: row.try_get("backlink_url")?,
        note: db::text(row, "note")?,
        home: row.try_get::<i64, _>("home")? != 0,
        displayorder: row.try_get("displayorder")?,
        owner_uid: row.try_get("owner_uid")?,
        owner_name,
        resolution: db::text(row, "resolution")?,
        dateline: row.try_get("dateline")?,
        handled_at: row.try_get("handled_at")?,
        version: row.try_get("version")?,
    })
}

/// One entry in the home page sidebar.
#[derive(Clone, Debug)]
pub struct HomeLink {
    pub name: String,
    pub url: String,
}

/// Home page sidebar: active links with "Show on home page" checked, the first [`HOME_MAX`] by sort value; the page
/// truncates by `settings.links.home_count`.
/// `ref gonof_link_home` (equality on the two columns status and home), read in index order without sorting; not
/// stored in Redis.
pub async fn home(db: &Database) -> Result<Vec<HomeLink>> {
    db::query("SELECT name,url FROM gonof_common_friend_link WHERE status='active' AND home=1 ORDER BY displayorder,id LIMIT ?")
        .bind(HOME_MAX as i64)
        .fetch_all(db.executor())
        .await?
        .iter()
        .map(|row| {
            Ok(HomeLink {
                name: db::text(row, "name")?,
                url: db::text(row, "url")?,
            })
        })
        .collect()
}

/// One entry on the directory page.
#[derive(Clone, Debug)]
pub struct Listed {
    pub name: String,
    pub url: String,
    pub domain: String,
    pub description: String,
}

/// Directory page: all active links grouped by category (groups in the order of [`LinkCategory::ALL`]), sorted by
/// sort value within each group. One `ref gonof_link_list` read in index order, at most [`DIRECTORY_LIMIT`] entries,
/// grouped in code (no pagination, only layout).
pub async fn directory(db: &Database) -> Result<Vec<(LinkCategory, Vec<Listed>)>> {
    let rows = db::query("SELECT category,name,url,domain,description FROM gonof_common_friend_link WHERE status='active' ORDER BY category,displayorder,id LIMIT ?")
        .bind(DIRECTORY_LIMIT as i64)
        .fetch_all(db.executor())
        .await?;
    let mut groups: Vec<(LinkCategory, Vec<Listed>)> = LinkCategory::ALL
        .into_iter()
        .map(|category| (category, Vec::new()))
        .collect();
    for row in &rows {
        let category = db::text(row, "category")?;
        let Some(category) = LinkCategory::parse(&category) else {
            continue;
        };
        if let Some((_, group)) = groups.iter_mut().find(|(c, _)| *c == category) {
            group.push(Listed {
                name: db::text(row, "name")?,
                url: db::text(row, "url")?,
                domain: db::text(row, "domain")?,
                description: db::text(row, "description")?,
            });
        }
    }
    groups.retain(|(_, group)| !group.is_empty());
    Ok(groups)
}

/// The member's own applications: those still pending or active first, all of them, since only here can the member
/// withdraw or take one down; then the newest [`MINE_LIMIT`] finished ones. A list of the newest 20 alone let a run
/// of withdrawals push out an old active link that still took up one of the member's places. Uses the owner_uid
/// prefix of `gonof_link_request`.
pub async fn mine(db: &Database, uid: i64) -> Result<Vec<Link>> {
    let mut links = Vec::new();
    for statuses in ["IN ('pending','active')", "NOT IN ('pending','active')"] {
        let rows = db::query(&format!(
            "SELECT {COLUMNS} {FROM} WHERE l.owner_uid=? AND l.status {statuses} ORDER BY l.id DESC LIMIT ?"
        ))
        .bind(uid)
        .bind(MINE_LIMIT as i64)
        .fetch_all(db.executor())
        .await?;
        for row in &rows {
            links.push(link_of(row)?);
        }
    }
    Ok(links)
}

/// Admin list. When filtering by status, `FORCE INDEX (gonof_link_queue)` is spelled out: the optimizers of 5.7.44
/// and 8.4.11 both switch to `gonof_link_list`, which also starts with status, and then filesort the whole range (in
/// measured plans, page 2 of pending read 464 rows, 104 rows when forced). Pending links are ordered by submission
/// (a queue), the others newest first.
pub async fn admin_list(
    db: &Database,
    status: Option<Status>,
    page: usize,
) -> Result<(Vec<Link>, bool)> {
    let page = page.max(1);
    let limit = (ADMIN_PAGE_SIZE + 1) as i64;
    let offset = ((page - 1) * ADMIN_PAGE_SIZE) as i64;
    let rows = match status {
        None => {
            db::query(&format!(
                "SELECT {COLUMNS} {FROM} ORDER BY l.id DESC LIMIT ? OFFSET ?"
            ))
            .bind(limit)
            .bind(offset)
            .fetch_all(db.executor())
            .await?
        }
        Some(status) => {
            let order = if status == Status::Pending {
                "l.id"
            } else {
                "l.id DESC"
            };
            db::query(&format!(
                "SELECT {COLUMNS} FROM gonof_common_friend_link l FORCE INDEX (gonof_link_queue) \
                 LEFT JOIN gonof_common_member m ON m.uid=l.owner_uid \
                 WHERE l.status=? ORDER BY {order} LIMIT ? OFFSET ?"
            ))
            .bind(status.slug())
            .bind(limit)
            .bind(offset)
            .fetch_all(db.executor())
            .await?
        }
    };
    let more = rows.len() > ADMIN_PAGE_SIZE;
    let links = rows
        .iter()
        .take(ADMIN_PAGE_SIZE)
        .map(link_of)
        .collect::<Result<Vec<_>>>()?;
    Ok((links, more))
}

pub async fn get(db: &Database, id: i64) -> Result<Option<Link>> {
    db::query(&format!("SELECT {COLUMNS} {FROM} WHERE l.id=?"))
        .bind(id)
        .fetch_optional(db.executor())
        .await?
        .map(|row| link_of(&row))
        .transpose()
}

/// Website URL normalization: only http/https, no username or password, a host name that is a domain with a dot (no
/// IPs or single-label hosts), ≤ 500 characters. Returns the URL as given (trimmed) and the normalized host name:
/// lowercase, IDN converted to ASCII (already done by `url` parsing), trailing dot and leading `www.` removed,
/// ≤ 190 characters.
pub fn site(value: &str) -> Result<(String, String)> {
    let value = value.trim();
    let invalid = IdentityError::Validation(crate::i18n::current().site_links_error_url());
    if value.is_empty() || value.chars().count() > 500 {
        return Err(invalid);
    }
    let url = url::Url::parse(value)
        .map_err(|_| IdentityError::Validation(crate::i18n::current().site_links_error_url()))?;
    let domain = match url.host() {
        Some(url::Host::Domain(host)) => host.trim_end_matches('.').to_ascii_lowercase(),
        _ => return Err(invalid),
    };
    let domain = domain
        .strip_prefix("www.")
        .map(str::to_owned)
        .unwrap_or(domain);
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || !domain.contains('.')
        || domain.starts_with('.')
        || domain.len() > 190
    {
        return Err(invalid);
    }
    Ok((value.to_owned(), domain))
}

/// The page holding the backlink: an empty string means the other site's home page; if filled in, it must be on the
/// same site as the URL (the same host, or one a subdomain of the other).
fn backlink(value: &str, domain: &str) -> Result<Option<String>> {
    if value.trim().is_empty() {
        return Ok(None);
    }
    let (url, host) = site(value)?;
    let related = host == domain
        || host.ends_with(&format!(".{domain}"))
        || domain.ends_with(&format!(".{host}"));
    if !related {
        return Err(IdentityError::Validation(
            crate::i18n::current().site_links_error_backlink(),
        ));
    }
    Ok(Some(url))
}

/// One line of plain text: trimmed, no control characters accepted.
fn plain(value: &str, max: usize, message: &'static str) -> Result<String> {
    let value = value.trim();
    if value.chars().count() > max || value.chars().any(char::is_control) {
        return Err(IdentityError::Validation(message));
    }
    Ok(value.to_owned())
}

fn name(value: &str) -> Result<String> {
    let name = plain(value, 30, crate::i18n::current().site_links_error_name())?;
    if name.is_empty() {
        return Err(IdentityError::Validation(
            crate::i18n::current().site_links_error_name(),
        ));
    }
    Ok(name)
}

fn category(value: &str) -> Result<LinkCategory> {
    LinkCategory::parse(value.trim()).ok_or(IdentityError::Validation(
        crate::i18n::current().site_links_error_category(),
    ))
}

fn resolution(value: &str) -> Result<String> {
    let message = crate::i18n::current().site_manage_link_error_reason();
    let value = plain(value, 200, message)?;
    if value.is_empty() {
        return Err(IdentityError::Validation(message));
    }
    Ok(value)
}

/// The message for a unique key violation on `live_domain`; other unique conflicts (in theory only the request key
/// remains) are left to the caller.
fn live_conflict(error: sqlx::Error) -> IdentityError {
    if db::unique_violation_on(
        &error,
        "gonof_link_live_domain",
        &["gonof_common_friend_link.live_domain"],
    ) {
        IdentityError::Conflict(crate::i18n::current().site_links_error_duplicate())
    } else {
        error.into()
    }
}

async fn log(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: Option<i64>,
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

/// An application submitted by a member.
#[derive(Clone, Debug, Default)]
pub struct Application {
    pub name: String,
    pub category: String,
    pub url: String,
    pub backlink_url: String,
    pub note: String,
    pub request_key: String,
}

/// Application result: the newly created row or the row hit by a replay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Applied {
    pub id: i64,
    pub replayed: bool,
}

/// A member applies for a friend link.
///
/// One transaction: shared range lock on site settings (rejected in read-only mode or when applications are closed)
/// → lock the member's own row and re-check (credentials, not frozen, account active, verified e-mail, registered
/// for at least `settings.links.min_account_days` days; administrators are exempt from e-mail and account age) → the
/// same request key returns the original row directly → at most 2 of the member's own links pending or active → the
/// same website is not accepted within 7 days after a rejection → insert (a key violation on `live_domain` means the
/// same website is already listed or under review) → write the log. Concurrent submissions by the same member queue
/// on the member row lock; different members competing for the same website are settled by the unique key.
/// `own_domains` are this forum's own domains ([`own_domains`]); none of them can be applied for.
pub async fn apply(
    db: &Database,
    actor: Actor,
    input: Application,
    own_domains: &[String],
    now: i64,
) -> Result<Applied> {
    if !identity::valid_request_key(&input.request_key) {
        return Err(IdentityError::Validation(
            crate::i18n::current().site_links_error_expired(),
        ));
    }
    let name = name(&input.name)?;
    let category = category(&input.category)?;
    let (url, domain) = site(&input.url)?;
    if own_domains.iter().any(|own| {
        *own == domain
            || own.ends_with(&format!(".{domain}"))
            || domain.ends_with(&format!(".{own}"))
    }) {
        return Err(IdentityError::Validation(
            crate::i18n::current().site_links_error_self(),
        ));
    }
    let backlink_url = backlink(&input.backlink_url, &domain)?;
    let note = plain(
        &input.note,
        200,
        crate::i18n::current().site_links_error_note(),
    )?;
    let mut tx = db.begin_write().await?;
    let settings = crate::runtime::locked(db, &mut tx).await?;
    if settings.site.read_only {
        return Err(IdentityError::Suspended(
            settings.site.read_only_notice(crate::i18n::current()),
        ));
    }
    if !settings.links.applications {
        return Err(IdentityError::Validation(
            crate::i18n::current().site_links_error_closed(),
        ));
    }
    let member = db::query("SELECT credentials_version,`freeze`,emailstatus,mobile,adminid,account_state,regdate FROM gonof_common_member WHERE uid=? FOR UPDATE")
        .bind(actor.uid)
        .fetch_optional(db.observe(&mut *tx))
        .await?
        .ok_or(IdentityError::Unauthorized)?;
    if member.try_get::<i64, _>("credentials_version")? != actor.credentials_version
        || db::text(&member, "account_state")? != "active"
        || !identity::api_key_locked(db, &mut tx, actor).await?
    {
        return Err(IdentityError::Unauthorized);
    }
    if member.try_get::<i64, _>("freeze")? != 0 {
        return Err(IdentityError::Validation(
            crate::i18n::current().site_links_error_frozen(),
        ));
    }
    let admin = member.try_get::<i64, _>("adminid")? == 1;
    let participation = crate::login::Participation::current();
    if !admin
        && !crate::login::permits(
            db.observe(&mut *tx),
            actor.uid,
            member.try_get::<i64, _>("emailstatus")? == 1,
            member.try_get::<Option<String>, _>("mobile")?.is_some(),
        )
        .await?
    {
        return Err(IdentityError::Validation(
            participation.message(crate::i18n::current().site_links_verify_first()),
        ));
    }
    // The application rules are the admin settings `settings.links.*` (the application rules block of
    // "Appearance › Friend links"), taking the copy read by the write gate; computed live from the registration and
    // rejection times, so changes apply retroactively at once (docs/settings-and-caching.md, "When a change takes
    // effect").
    let rules = &settings.links;
    if !admin
        && member.try_get::<i64, _>("regdate")?
            > now - rules.min_account_days.saturating_mul(86_400)
    {
        return Err(IdentityError::Validation(
            crate::i18n::current().site_links_error_age(),
        ));
    }
    if let Some(id) = db::query_scalar::<i64>(
        "SELECT id FROM gonof_common_friend_link WHERE owner_uid=? AND request_key=?",
    )
    .bind(actor.uid)
    .bind(&input.request_key)
    .fetch_optional(db.observe(&mut *tx))
    .await?
    {
        return Ok(Applied { id, replayed: true });
    }
    let live: i64 = db::query_scalar(
        "SELECT COUNT(*) FROM gonof_common_friend_link WHERE owner_uid=? AND status IN ('pending','active')",
    )
    .bind(actor.uid)
    .fetch_one(db.observe(&mut *tx))
    .await?;
    if live >= rules.live_per_member {
        return Err(IdentityError::Validation(
            crate::i18n::current().site_links_error_limit(),
        ));
    }
    let rejected: Option<Option<i64>> = db::query_scalar(
        "SELECT handled_at FROM gonof_common_friend_link WHERE domain=? AND status='rejected' ORDER BY id DESC LIMIT 1",
    )
    .bind(&domain)
    .fetch_optional(db.observe(&mut *tx))
    .await?;
    if rejected
        .flatten()
        .is_some_and(|at| at + rules.reject_cooldown_days.saturating_mul(86_400) > now)
    {
        return Err(IdentityError::Validation(
            crate::i18n::current().site_links_error_cooldown(),
        ));
    }
    let id = db::query("INSERT INTO gonof_common_friend_link(status,category,name,url,domain,live_domain,backlink_url,note,owner_uid,request_key,dateline,updated_at) VALUES('pending',?,?,?,?,?,?,?,?,?,?,?)")
        .bind(category.slug())
        .bind(&name)
        .bind(&url)
        .bind(&domain)
        .bind(&domain)
        .bind(&backlink_url)
        .bind(&note)
        .bind(actor.uid)
        .bind(&input.request_key)
        .bind(now)
        .bind(now)
        .execute(db.observe(&mut *tx))
        .await
        .map_err(live_conflict)?
        .last_insert_id() as i64;
    log(
        db,
        &mut tx,
        Some(actor.uid),
        actor.uid,
        "site.links.apply",
        serde_json::json!({"id": id, "domain": domain}),
        now,
    )
    .await?;
    tx.commit().await?;
    Ok(Applied {
        id,
        replayed: false,
    })
}

/// The applicant withdraws a pending application, or removes their own active link (removal is not allowed in
/// read-only mode: it changes public pages). Already handled ones return a conflict, and a repeated submission does
/// not write the log again.
///
/// After commit, clears this process's friend link blocks (home page and directory page) and broadcasts.
pub async fn withdraw(
    db: &Database,
    global: &crate::cache::GlobalCache,
    actor: Actor,
    id: i64,
    now: i64,
) -> Result<Status> {
    let mut tx = db.begin_write().await?;
    let settings = crate::runtime::locked(db, &mut tx).await?;
    let member = db::query(
        "SELECT credentials_version,account_state FROM gonof_common_member WHERE uid=? FOR UPDATE",
    )
    .bind(actor.uid)
    .fetch_optional(db.observe(&mut *tx))
    .await?
    .ok_or(IdentityError::Unauthorized)?;
    if member.try_get::<i64, _>("credentials_version")? != actor.credentials_version
        || db::text(&member, "account_state")? != "active"
        || !identity::api_key_locked(db, &mut tx, actor).await?
    {
        return Err(IdentityError::Unauthorized);
    }
    let row = db::query(
        "SELECT status,owner_uid,domain FROM gonof_common_friend_link WHERE id=? FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(db.observe(&mut *tx))
    .await?
    .ok_or(IdentityError::NotFound)?;
    if row.try_get::<Option<i64>, _>("owner_uid")? != Some(actor.uid) {
        return Err(IdentityError::NotFound);
    }
    let status = Status::parse(&db::text(&row, "status")?);
    let next = match status {
        Some(Status::Pending) => Status::Withdrawn,
        Some(Status::Active) => {
            if settings.site.read_only {
                return Err(IdentityError::Suspended(
                    settings.site.read_only_notice(crate::i18n::current()),
                ));
            }
            Status::Removed
        }
        _ => {
            return Err(IdentityError::Conflict(
                crate::i18n::current().site_links_already_handled(),
            ));
        }
    };
    db::query("UPDATE gonof_common_friend_link SET status=?,live_domain=NULL,handler_uid=NULL,handled_at=?,updated_at=?,version=version+1 WHERE id=? AND status=?")
        .bind(next.slug())
        .bind(now)
        .bind(now)
        .bind(id)
        .bind(status.map(Status::slug).unwrap_or_default())
        .execute(db.observe(&mut *tx))
        .await?;
    log(
        db,
        &mut tx,
        Some(actor.uid),
        actor.uid,
        "site.links.withdraw",
        serde_json::json!({"id": id, "domain": db::text(&row, "domain")?, "to": next.slug()}),
        now,
    )
    .await?;
    tx.commit().await?;
    links_changed(global).await?;
    Ok(next)
}

/// After a friend link write commits: clears this process's two slots, home page and directory page, and broadcasts.
/// Pending applications are not in these two slots, so an application itself does not clear them
/// (docs/settings-and-caching.md, "The process cache"); the account deletion worker only turns pending applications
/// into withdrawn ones (`directory::forget_member`), which likewise does not affect what is shown, so it does not
/// clear them either.
async fn links_changed(global: &crate::cache::GlobalCache) -> Result<()> {
    global
        .announce(&[crate::cache::Block::Links])
        .await
        .map_err(IdentityError::Internal)
}

/// Link content filled in in the admin area (shared by adding, editing before approval and editing while active).
#[derive(Clone, Debug, Default)]
pub struct LinkInput {
    pub category: String,
    pub name: String,
    pub url: String,
    pub description: String,
    pub home: bool,
    pub displayorder: i64,
}

struct Checked {
    category: LinkCategory,
    name: String,
    url: String,
    domain: String,
    description: String,
    home: bool,
    displayorder: i64,
}

fn check(input: &LinkInput) -> Result<Checked> {
    let (url, domain) = site(&input.url)?;
    if !(-9999..=9999).contains(&input.displayorder) {
        return Err(IdentityError::Validation(
            crate::i18n::current().site_manage_link_error_order_range(),
        ));
    }
    Ok(Checked {
        category: category(&input.category)?,
        name: name(&input.name)?,
        url,
        domain,
        description: plain(
            &input.description,
            60,
            crate::i18n::current().site_manage_link_error_description(),
        )?,
        home: input.home,
        displayorder: input.displayorder,
    })
}

/// Gate (admin governance is allowed in read-only mode) → administrator re-check.
async fn admin_tx(db: &Database, actor: Actor) -> Result<Tx<'static>> {
    let mut tx = db.begin_write().await?;
    crate::runtime::guard_write(db, &mut tx, true).await?;
    crate::runtime::admin_locked(db, &mut tx, actor).await?;
    Ok(tx)
}

/// Whether another link may become active. The last places go one at a time: the administrators' lock row first (as
/// for other one-at-a-time admin saves), then a count that reads the latest committed rows. A plain count reads the
/// snapshot of the transaction, so on MySQL two administrators approving or adding at 499 each counted 499 and both
/// went in, past what the directory page shows.
async fn ensure_room(db: &Database, tx: &mut Tx<'_>) -> Result<()> {
    db::query(
        "SELECT skey FROM gonof_common_settings WHERE skey='identity.bootstrap_admin' FOR UPDATE",
    )
    .fetch_one(db.observe(&mut **tx))
    .await?;
    let active: i64 = db::query_scalar(
        "SELECT COUNT(*) FROM gonof_common_friend_link WHERE status='active' LOCK IN SHARE MODE",
    )
    .fetch_one(db.observe(&mut **tx))
    .await?;
    if active >= DIRECTORY_LIMIT as i64 {
        return Err(IdentityError::Validation(
            crate::i18n::current().site_manage_link_error_full(),
        ));
    }
    Ok(())
}

/// Added directly by an administrator (active, no applicant).
pub async fn create(
    db: &Database,
    global: &crate::cache::GlobalCache,
    actor: Actor,
    input: LinkInput,
    now: i64,
) -> Result<i64> {
    let input = check(&input)?;
    let mut tx = admin_tx(db, actor).await?;
    ensure_room(db, &mut tx).await?;
    let id = db::query("INSERT INTO gonof_common_friend_link(status,category,name,url,domain,live_domain,description,home,displayorder,handler_uid,dateline,handled_at,updated_at) VALUES('active',?,?,?,?,?,?,?,?,?,?,?,?)")
        .bind(input.category.slug())
        .bind(&input.name)
        .bind(&input.url)
        .bind(&input.domain)
        .bind(&input.domain)
        .bind(&input.description)
        .bind(i64::from(input.home))
        .bind(input.displayorder)
        .bind(actor.uid)
        .bind(now)
        .bind(now)
        .bind(now)
        .execute(db.observe(&mut *tx))
        .await
        .map_err(live_conflict)?
        .last_insert_id() as i64;
    log(
        db,
        &mut tx,
        None,
        actor.uid,
        "site.links.add",
        serde_json::json!({"id": id, "domain": input.domain}),
        now,
    )
    .await?;
    tx.commit().await?;
    links_changed(global).await?;
    Ok(id)
}

/// An admin action on one link.
#[derive(Clone, Debug)]
pub enum Decision {
    /// Approve a pending application; name, category, home page flag and order can be changed at the same time.
    Approve(LinkInput),
    /// Reject a pending application; a reason is required.
    Reject(String),
    /// Edit an active link.
    Update(LinkInput),
    /// Remove an active link; a reason is required.
    Remove(String),
}

/// Result of handling: `Unchanged` means the state had already been handled and nothing was written this time (a
/// repeated submission or two administrators handling it at once).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handled {
    Done,
    Unchanged,
}

/// Admin approve, reject, edit, remove (administrators only). First lock this row and read its current state, judge
/// it by the state machine, then do an update conditioned on the state; if the state has already changed, return
/// [`Handled::Unchanged`] without writing the log again.
///
/// `version` is the row version the admin form was read at. When given and the row has been written since (another
/// administrator edited it), the action is refused with a conflict, so an older form cannot put back the fields it
/// still shows; every write raises the version (`version=version+1 … AND version=?`).
pub async fn decide(
    db: &Database,
    global: &crate::cache::GlobalCache,
    actor: Actor,
    id: i64,
    version: Option<i64>,
    decision: Decision,
    now: i64,
) -> Result<Handled> {
    let (expected, kind) = match &decision {
        Decision::Approve(_) => (Status::Pending, "site.links.approve"),
        Decision::Reject(_) => (Status::Pending, "site.links.reject"),
        Decision::Update(_) => (Status::Active, "site.links.update"),
        Decision::Remove(_) => (Status::Active, "site.links.remove"),
    };
    let (checked, reason) = match &decision {
        Decision::Approve(input) | Decision::Update(input) => (Some(check(input)?), String::new()),
        Decision::Reject(reason) | Decision::Remove(reason) => (None, resolution(reason)?),
    };
    let mut tx = admin_tx(db, actor).await?;
    let row = db::query(
        "SELECT status,owner_uid,domain,version FROM gonof_common_friend_link WHERE id=? FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(db.observe(&mut *tx))
    .await?
    .ok_or(IdentityError::NotFound)?;
    if Status::parse(&db::text(&row, "status")?) != Some(expected) {
        return Ok(Handled::Unchanged);
    }
    let current: i64 = row.try_get("version")?;
    if version.is_some_and(|version| version != current) {
        return Err(IdentityError::Conflict(
            crate::i18n::current().site_manage_link_error_changed(),
        ));
    }
    let owner: Option<i64> = row.try_get("owner_uid")?;
    let old_domain = db::text(&row, "domain")?;
    let domain = match checked {
        // Both approving and editing result in active; if the URL changed, live_domain changes with it, and a key
        // violation means another record already holds it.
        Some(input) => {
            if expected == Status::Pending {
                ensure_room(db, &mut tx).await?;
            }
            db::query("UPDATE gonof_common_friend_link SET status='active',category=?,name=?,url=?,domain=?,live_domain=?,description=?,home=?,displayorder=?,handler_uid=?,handled_at=?,updated_at=?,version=version+1 WHERE id=? AND status=? AND version=?")
                .bind(input.category.slug())
                .bind(&input.name)
                .bind(&input.url)
                .bind(&input.domain)
                .bind(&input.domain)
                .bind(&input.description)
                .bind(i64::from(input.home))
                .bind(input.displayorder)
                .bind(actor.uid)
                .bind(now)
                .bind(now)
                .bind(id)
                .bind(expected.slug())
                .bind(current)
                .execute(db.observe(&mut *tx))
                .await
                .map_err(live_conflict)?;
            input.domain
        }
        None => {
            let next = if expected == Status::Pending {
                Status::Rejected
            } else {
                Status::Removed
            };
            db::query("UPDATE gonof_common_friend_link SET status=?,live_domain=NULL,resolution=?,handler_uid=?,handled_at=?,updated_at=?,version=version+1 WHERE id=? AND status=? AND version=?")
                .bind(next.slug())
                .bind(&reason)
                .bind(actor.uid)
                .bind(now)
                .bind(now)
                .bind(id)
                .bind(expected.slug())
                .bind(current)
                .execute(db.observe(&mut *tx))
                .await?;
            old_domain.clone()
        }
    };
    log(
        db,
        &mut tx,
        owner,
        actor.uid,
        kind,
        serde_json::json!({"id": id, "domain": domain, "from_domain": old_domain, "reason": reason}),
        now,
    )
    .await?;
    tx.commit().await?;
    links_changed(global).await?;
    Ok(Handled::Done)
}

/// This site's own normalized domain, used to reject applications for this site. Nothing is blocked when it cannot
/// be determined.
pub fn own_domain(site_url: &str) -> Option<String> {
    site(site_url).ok().map(|(_, domain)| domain)
}

/// Every normalized domain this forum is reached on, used to reject applications for the forum itself: the forum URL
/// and each address of the domain table (`gonof_common_site_domain`, enabled or not, at most
/// [`crate::domains::MAX_DOMAINS`] rows, read by primary key). Addresses that cannot be normalized (an IP, a
/// single-label host) block nothing.
pub async fn own_domains(db: &Database, site_url: &str) -> Result<Vec<String>> {
    let urls: Vec<String> = db::query_scalar(
        "SELECT site_url FROM gonof_common_site_domain ORDER BY domain_id LIMIT ?",
    )
    .bind(crate::domains::MAX_DOMAINS as i64)
    .fetch_all(db.executor())
    .await?;
    let mut domains: Vec<String> = std::iter::once(site_url)
        .chain(urls.iter().map(String::as_str))
        .filter_map(own_domain)
        .collect();
    domains.sort();
    domains.dedup();
    Ok(domains)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sites_normalize_to_a_comparable_domain() {
        assert_eq!(
            site(" https://WWW.Example.COM./blog ").unwrap(),
            (
                "https://WWW.Example.COM./blog".to_owned(),
                "example.com".to_owned()
            )
        );
        // An internationalized domain: "\u{4f8b}\u{5b50}.\u{6d4b}\u{8bd5}" is the Chinese for "example.test".
        assert_eq!(
            site("http://\u{4f8b}\u{5b50}.\u{6d4b}\u{8bd5}/").unwrap().1,
            "xn--fsqu00a.xn--0zwm56d"
        );
        assert_eq!(
            site("https://blog.example.com").unwrap().1,
            "blog.example.com"
        );
        for bad in [
            "",
            "example.com",
            "ftp://example.com",
            "https://user:pw@example.com",
            "https://127.0.0.1/",
            "https://[::1]/",
            "https://localhost/",
            "javascript:alert(1)",
        ] {
            assert!(site(bad).is_err(), "{bad}");
        }
        assert!(site(&format!("https://example.com/{}", "a".repeat(500))).is_err());
    }

    #[test]
    fn backlinks_must_live_on_the_applied_site() {
        assert_eq!(backlink(" ", "example.com").unwrap(), None);
        assert!(backlink("https://example.com/links", "example.com").is_ok());
        assert!(backlink("https://blog.example.com/links", "example.com").is_ok());
        assert!(backlink("https://example.com/links", "blog.example.com").is_ok());
        assert!(backlink("https://other.test/links", "example.com").is_err());
        assert!(backlink("https://notexample.com/", "example.com").is_err());
    }

    #[test]
    fn categories_and_statuses_round_trip() {
        let labels: Vec<_> = LinkCategory::ALL.iter().map(|c| c.label()).collect();
        assert_eq!(labels, ["Forums", "Blogs", "Directories", "Tools", "Other"]);
        for category in LinkCategory::ALL {
            assert_eq!(LinkCategory::parse(category.slug()), Some(category));
        }
        for status in Status::ALL {
            assert_eq!(Status::parse(status.slug()), Some(status));
        }
    }

    #[test]
    fn own_domain_ignores_unusable_addresses() {
        assert_eq!(
            own_domain("https://example.com").as_deref(),
            Some("example.com")
        );
        assert_eq!(own_domain("http://127.0.0.1:58833"), None);
    }
}
