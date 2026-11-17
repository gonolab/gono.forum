//! Scoped management reads. Public content queries retain their own visibility rules.
use crate::db::DbRow;
use crate::{
    db::{self, Database},
    i18n::Lang,
    identity::{Actor, IdentityError},
};
use serde::Deserialize;

type Result<T> = std::result::Result<T, IdentityError>;
pub const PAGE_SIZE: i64 = 20;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ContentQuery {
    pub kind: String,
    #[serde(deserialize_with = "optional_id")]
    pub fid: Option<i64>,
    pub state: String,
    pub q: String,
    pub page: Option<i64>,
}

impl ContentQuery {
    pub fn page(&self) -> i64 {
        self.page.unwrap_or(1)
    }
    pub fn selected_forum(&self, fid: &i64) -> bool {
        self.fid == Some(*fid)
    }
    fn validate(&self) -> Result<()> {
        if !matches!(self.kind.as_str(), "" | "thread" | "reply")
            || !matches!(self.state.as_str(), "" | "visible" | "hidden" | "deleted")
            || self.fid.is_some_and(|fid| fid <= 0)
            || self.page() < 1
            || self.page() > i64::MAX / PAGE_SIZE
            || self.q.chars().count() > 200
        {
            return Err(IdentityError::Validation(
                crate::i18n::current().forum_content_error_filter(),
            ));
        }
        Ok(())
    }
}

fn optional_id<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<i64>, D::Error> {
    let value = Option::<String>::deserialize(deserializer)?;
    value
        .filter(|value| !value.is_empty())
        .map(|value| value.parse().map_err(serde::de::Error::custom))
        .transpose()
}

#[derive(Debug)]
pub struct ManagedPost {
    pub pid: i64,
    pub tid: i64,
    pub fid: i64,
    pub first: bool,
    pub position: i64,
    pub username: String,
    pub forum: String,
    pub subject: String,
    pub message: String,
    pub state: String,
    pub post_version: i64,
    pub thread_version: i64,
    pub deleted: bool,
    pub erased: bool,
    pub hidden: bool,
    pub closed: bool,
    pub pinned: bool,
    pub featured: bool,
}

#[derive(Debug)]
pub struct Revision {
    pub version: i64,
    pub editor: String,
    pub subject: String,
    pub message: String,
    pub dateline: i64,
}

impl Revision {
    pub fn time(&self) -> String {
        display_time(self.dateline)
    }
}

#[derive(Clone, Debug)]
pub struct ManagedForum {
    pub fid: i64,
    pub name: String,
    pub parent_fid: Option<i64>,
    /// A child forum after grouping by parent and child (indented in drop-downs).
    pub child: bool,
}

impl ManagedForum {
    /// The name shown in drop-downs: child forums carry an indent prefix.
    pub fn label(&self) -> String {
        if self.child {
            format!("{}{}", crate::organization::CHILD_PREFIX, self.name)
        } else {
            self.name.clone()
        }
    }
}

pub struct ContentPage {
    pub rows: Vec<ManagedPost>,
    pub total: i64,
}

pub struct RevisionPage {
    pub rows: Vec<Revision>,
    pub total: i64,
}

pub async fn staff(db: &Database, actor: Actor) -> Result<i64> {
    let member = crate::db::query(
        "SELECT adminid,credentials_version,`freeze`,account_state FROM gonof_common_member WHERE uid = ?",
    )
    .bind(actor.uid)
    .fetch_optional(db.executor())
    .await?
    .ok_or(IdentityError::Unauthorized)?;
    if member.try_get::<i64, _>("credentials_version")? != actor.credentials_version
        || member.try_get::<i64, _>("freeze")? != 0
        || db::text(&member, "account_state")? != "active"
    {
        return Err(IdentityError::Unauthorized);
    }
    match member.try_get::<i64, _>("adminid")? {
        role @ 1..=5 => Ok(role),
        _ => Err(IdentityError::Forbidden),
    }
}

fn active(actor: Actor) -> String {
    // Both values are server-issued integers. Recheck the credential generation in the data query.
    format!(
        "EXISTS (SELECT 1 FROM gonof_common_member active_actor WHERE active_actor.uid = {} AND active_actor.credentials_version = {} AND active_actor.`freeze` = 0 AND active_actor.account_state = 'active' AND active_actor.adminid IN (1, 2, 3, 4, 5))",
        actor.uid, actor.credentials_version
    )
}

pub async fn forums(db: &Database, actor: Actor) -> Result<Vec<ManagedForum>> {
    let role = staff(db, actor).await?;
    // Moderators manage the forums they are appointed to and their child forums.
    let rows = crate::db::query(&format!("SELECT fid, name, parent_fid FROM gonof_forum_forum f WHERE {} AND (? IN (1, 2) OR {}) ORDER BY displayorder, fid", active(actor), crate::organization::moderates_sql("?", "f")))
        .bind(role).bind(actor.uid).fetch_all(db.executor()).await?;
    let forums = rows
        .iter()
        .map(|row| {
            Ok(ManagedForum {
                fid: row.try_get("fid")?,
                name: db::text(row, "name")?,
                parent_fid: row.try_get("parent_fid")?,
                child: false,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    // Ordered by parent/child groups: both the content filter and the appointment page drop-downs use this list.
    Ok(
        crate::organization::grouped_by_parent(&forums, |f| (f.fid, f.parent_fid))
            .into_iter()
            .map(|(forum, child)| ManagedForum {
                child,
                ..forum.clone()
            })
            .collect(),
    )
}

/// The tables a content statement joins to the post `p`. Every post has its thread (foreign key on `(tid, fid)`), the
/// thread's forum (foreign key) and exactly one first post (checked by `tests/consistency/content.sql`), so the inner
/// joins never drop a row: the list joins everything its columns need, and the count joins only the tables its
/// conditions name, which with no filter is none.
#[derive(Clone, Copy, Default)]
struct Joins {
    thread: bool,
    forum: bool,
    /// The parent forum and the first post, for the hidden state ([`HIDDEN`]).
    hidden_state: bool,
    author: bool,
}

impl Joins {
    const ALL: Joins = Joins {
        thread: true,
        forum: true,
        hidden_state: true,
        author: true,
    };

    fn sql(self) -> String {
        let mut from = String::from("FROM gonof_forum_post p");
        if self.thread || self.hidden_state {
            from.push_str(" JOIN gonof_forum_thread t ON t.tid = p.tid");
        }
        if self.forum || self.hidden_state {
            from.push_str(" JOIN gonof_forum_forum f ON f.fid = p.fid");
        }
        if self.hidden_state {
            from.push_str(" LEFT JOIN gonof_forum_forum pf ON pf.fid = f.parent_fid JOIN gonof_forum_post firstpost ON firstpost.first_tid = t.tid");
        }
        if self.author {
            from.push_str(" LEFT JOIN gonof_common_member author ON author.uid = p.authorid");
        }
        from
    }
}

// Withdrawal affects the author's own text; descendants remain public unless governance hides them.
// Whether a forum is enabled and visible depends on two rows, the forum and its parent (the thread row carries no
// forum copies): looking only at the forum's own row would count posts in a child forum whose parent is disabled or
// members-only as "public". A nested reply's parent floor is hidden when its `root_invisible` copy is set, as on read
// paths (`content::post_visible`).
const DELETED: &str = "(p.deleted_at > 0 OR (p.first = 1 AND t.deleted_at > 0))";
const HIDDEN: &str = "(p.invisible <> 0 OR firstpost.invisible <> 0 OR t.displayorder < 0 OR f.status <> 1 OR f.visibility <> 'public' OR COALESCE(pf.status, 1) <> 1 OR COALESCE(pf.visibility, 'public') <> 'public' OR t.visibility <> 'public' OR t.readperm <> 0 OR t.publication_state <> 'published' OR p.root_invisible <> 0)";
const COLUMNS: &str = "p.pid, p.tid, p.fid, p.first, p.position, author.username, f.name AS forum_name, t.subject, p.message, p.version AS post_version, t.version AS thread_version, p.deleted_at, p.erased_at, author.account_state AS author_state, t.deleted_at AS thread_deleted_at, p.invisible, t.closed, t.displayorder, t.digest, firstpost.invisible AS first_invisible, f.status AS forum_status, f.visibility AS forum_visibility, pf.status AS parent_status, pf.visibility AS parent_visibility, t.visibility AS thread_visibility, t.readperm, t.publication_state, p.root_invisible AS parent_invisible";

fn decode(row: &DbRow) -> Result<ManagedPost> {
    let t = crate::i18n::current();
    let first = row.try_get::<i64, _>("first")? == 1;
    let erased = row.try_get::<i64, _>("erased_at")? > 0;
    let deleted = row.try_get::<i64, _>("deleted_at")? > 0
        || (first && row.try_get::<i64, _>("thread_deleted_at")? > 0);
    let hidden = row.try_get::<i64, _>("invisible")? != 0
        || row.try_get::<i64, _>("first_invisible")? != 0
        || row.try_get::<i64, _>("displayorder")? < 0
        || {
            let scope = crate::content::copies::forum_scope(row)?;
            !scope.enabled || scope.visibility != crate::content::copies::ForumVisibility::Public
        }
        || db::text(row, "thread_visibility")? != "public"
        || row.try_get::<i64, _>("readperm")? != 0
        || db::text(row, "publication_state")? != "published"
        || row
            .try_get::<Option<i64>, _>("parent_invisible")?
            .unwrap_or(0)
            != 0;
    Ok(ManagedPost {
        pid: row.try_get("pid")?,
        tid: row.try_get("tid")?,
        fid: row.try_get("fid")?,
        first,
        position: row.try_get("position")?,
        username: if row.try_get::<Option<String>, _>("author_state")?.as_deref() == Some("deleted")
        {
            t.common_deleted_user().into()
        } else {
            row.try_get::<Option<String>, _>("username")?
                .unwrap_or_else(|| t.common_deleted_user().into())
        },
        forum: db::text(row, "forum_name")?,
        subject: db::text(row, "subject")?,
        message: db::text(row, "message")?,
        state: if erased {
            t.forum_content_status_erased()
        } else if deleted {
            t.forum_content_status_withdrawn()
        } else if hidden {
            t.forum_content_status_hidden()
        } else {
            t.forum_content_status_public()
        }
        .into(),
        post_version: row.try_get("post_version")?,
        thread_version: row.try_get("thread_version")?,
        deleted,
        erased,
        // Action state is local to this post; `state` above describes effective public visibility.
        hidden: row.try_get::<i64, _>("invisible")? != 0,
        closed: row.try_get::<i64, _>("closed")? > 0,
        pinned: row.try_get::<i64, _>("displayorder")? > 0,
        featured: row.try_get::<i64, _>("digest")? > 0,
    })
}

pub async fn content(db: &Database, actor: Actor, filter: &ContentQuery) -> Result<ContentPage> {
    filter.validate()?;
    let role = staff(db, actor).await?;
    // Only the filters in use go into the statement, worked out here rather than as `? = 0 OR …` in SQL: MySQL cannot
    // drop a `LIKE '%%'` and would join every post to count them, and the count joins only the tables named here
    // ([`Joins`]). UIDs and FIDs are integers, written directly into the statement as in `active()`.
    let mut conditions = vec![active(actor)];
    let mut joins = Joins::default();
    // Administrators and global moderators see everything; moderators and trainee moderators go by their appointed
    // forums; reviewers also see reported content (docs/roles.md, "Reviewers").
    if !matches!(role, 1 | 2) {
        let moderates = crate::organization::moderates_sql(&actor.uid.to_string(), "f");
        conditions.push(if role == 5 {
            format!("({moderates} OR {})", crate::moderation::reported_sql("p"))
        } else {
            moderates
        });
        joins.forum = true;
    }
    if let Some(fid) = filter.fid {
        conditions.push(format!("(p.fid = {fid} OR f.parent_fid = {fid})"));
        joins.forum = true;
    }
    match filter.kind.as_str() {
        "thread" => conditions.push("p.first = 1".into()),
        "reply" => conditions.push("p.first = 0".into()),
        _ => {}
    }
    match filter.state.as_str() {
        "deleted" => {
            conditions.push(DELETED.into());
            joins.thread = true;
        }
        "hidden" => {
            conditions.push(format!("NOT {DELETED} AND {HIDDEN}"));
            joins.hidden_state = true;
        }
        "visible" => {
            conditions.push(format!("NOT {DELETED} AND NOT {HIDDEN}"));
            joins.hidden_state = true;
        }
        _ => {}
    }
    let search = (!filter.q.trim().is_empty()).then(|| like(&filter.q));
    if search.is_some() {
        conditions.push("(LOWER(t.subject) LIKE ? ESCAPE '!' OR LOWER(p.message) LIKE ? ESCAPE '!' OR LOWER(COALESCE(author.username, '')) LIKE ? ESCAPE '!')".into());
        joins.thread = true;
        joins.author = true;
    }
    let conditions = format!("WHERE {}", conditions.join(" AND "));
    let count_sql = format!("SELECT COUNT(*) {} {conditions}", joins.sql());
    let mut count = crate::db::query_scalar::<i64>(&count_sql);
    if let Some(search) = &search {
        count = count.bind(search).bind(search).bind(search);
    }
    let total = count.fetch_one(db.executor()).await?;
    if filter.page() > ((total + PAGE_SIZE - 1) / PAGE_SIZE).max(1) {
        return Err(IdentityError::NotFound);
    }
    let list_sql = format!(
        "SELECT {COLUMNS} {} {conditions} ORDER BY p.pid DESC LIMIT ? OFFSET ?",
        Joins::ALL.sql()
    );
    let mut list = crate::db::query(&list_sql);
    if let Some(search) = &search {
        list = list.bind(search).bind(search).bind(search);
    }
    let rows = list
        .bind(PAGE_SIZE)
        .bind((filter.page() - 1) * PAGE_SIZE)
        .fetch_all(db.executor())
        .await?;
    Ok(ContentPage {
        rows: rows.iter().map(decode).collect::<Result<_>>()?,
        total,
    })
}

pub async fn post(
    db: &Database,
    actor: Actor,
    pid: i64,
    revisions_page: i64,
) -> Result<(ManagedPost, RevisionPage)> {
    let role = staff(db, actor).await?;
    if !(1..=i64::MAX / PAGE_SIZE).contains(&revisions_page) {
        return Err(IdentityError::Validation(
            crate::i18n::current().forum_content_error_revisions_page(),
        ));
    }
    let guard = active(actor);
    let row = crate::db::query(&format!(
        "SELECT {COLUMNS} {} WHERE {guard} AND p.pid = ? AND (? IN (1, 2) OR {} OR (? = 5 AND {}))",
        Joins::ALL.sql(),
        crate::organization::moderates_sql("?", "f"),
        crate::moderation::reported_sql("p"),
    ))
    .bind(pid)
    .bind(role)
    .bind(actor.uid)
    .bind(role)
    .fetch_optional(db.executor())
    .await?
    .ok_or(IdentityError::NotFound)?;
    let post = decode(&row)?;
    let total: i64 = crate::db::query_scalar(&format!(
        "SELECT COUNT(*) FROM gonof_forum_post_revision r WHERE {guard} AND r.pid = ?"
    ))
    .bind(pid)
    .fetch_one(db.executor())
    .await?;
    if revisions_page > ((total + PAGE_SIZE - 1) / PAGE_SIZE).max(1) {
        return Err(IdentityError::NotFound);
    }
    let rows = crate::db::query(&format!("SELECT r.version, m.username AS editor, r.subject, r.message, r.dateline FROM gonof_forum_post_revision r LEFT JOIN gonof_common_member m ON m.uid = r.editor_uid WHERE {guard} AND r.pid = ? ORDER BY r.version DESC LIMIT ? OFFSET ?"))
        .bind(pid).bind(PAGE_SIZE).bind((revisions_page-1)*PAGE_SIZE).fetch_all(db.executor()).await?;
    let revisions = rows
        .iter()
        .map(|row| {
            Ok(Revision {
                version: row.try_get("version")?,
                editor: row
                    .try_get::<Option<String>, _>("editor")?
                    .unwrap_or_else(|| crate::i18n::current().common_deleted_user().into()),
                subject: row
                    .try_get::<Option<String>, _>("subject")?
                    .unwrap_or_default(),
                message: db::text(row, "message")?,
                dateline: row.try_get("dateline")?,
            })
        })
        .collect::<Result<_>>()?;
    Ok((
        post,
        RevisionPage {
            rows: revisions,
            total,
        },
    ))
}

pub struct LogEntry {
    pub id: i64,
    pub operator: String,
    pub operation: String,
    pub reason: String,
    pub result: String,
    pub target: String,
    pub dateline: i64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LogScope {
    /// Bound to a specific post or thread, so the forum's moderators can see it too.
    Content,
    /// Only site administrators can see it.
    Admin,
}

/// Content logs (`content.*`, `restore.*`) write the three columns `pid`, `fid` and `to_fid` in the same statement
/// (fields that are queried are split into columns): moderators filtering logs by forum and restore requests looking
/// up who hid something read these columns instead of the JSON in data. Only integers in data are recognized; for
/// other events the three columns stay empty.
pub(crate) fn log_targets(kind: &str, data: &serde_json::Value) -> [Option<i64>; 3] {
    if !(kind.starts_with("content.") || kind.starts_with("restore.")) {
        return [None; 3];
    }
    ["pid", "fid", "to"].map(|key| data.get(key).and_then(serde_json::Value::as_i64))
}

/// The label of an audit event in a language: a generated method of [`Lang`].
type EventLabel = fn(Lang) -> &'static str;

/// Audit event registry: event name, visibility scope, label (a translation key, maintenance.ftl).
///
/// The log page's two IN lists and the labels are all generated from this table, so only one place needs
/// maintenance, and names without labels, or labels that find nothing, cannot occur. A listed name that nothing
/// writes raises no error, its entries simply never appear; publishing, for example, writes
/// `content.thread.publish`/`content.reply.publish` (see the two call sites of `finish` in `content/writes.rs`), not
/// `content.thread`/`content.reply`.
const EVENTS: &[(&str, LogScope, EventLabel)] = &[
    (
        "content.thread.publish",
        LogScope::Content,
        Lang::maintenance_event_content_thread_publish,
    ),
    (
        "content.reply.publish",
        LogScope::Content,
        Lang::maintenance_event_content_reply_publish,
    ),
    (
        "content.edit",
        LogScope::Content,
        Lang::maintenance_event_content_edit,
    ),
    (
        "content.withdraw",
        LogScope::Content,
        Lang::maintenance_event_content_withdraw,
    ),
    (
        "content.restore",
        LogScope::Content,
        Lang::maintenance_event_content_restore,
    ),
    (
        "content.hide",
        LogScope::Content,
        Lang::maintenance_event_content_hide,
    ),
    (
        "content.unhide",
        LogScope::Content,
        Lang::maintenance_event_content_unhide,
    ),
    (
        "content.lock",
        LogScope::Content,
        Lang::maintenance_event_content_lock,
    ),
    (
        "content.unlock",
        LogScope::Content,
        Lang::maintenance_event_content_unlock,
    ),
    (
        "content.pin",
        LogScope::Content,
        Lang::maintenance_event_content_pin,
    ),
    (
        "content.unpin",
        LogScope::Content,
        Lang::maintenance_event_content_unpin,
    ),
    (
        "content.feature",
        LogScope::Content,
        Lang::maintenance_event_content_feature,
    ),
    (
        "content.unfeature",
        LogScope::Content,
        Lang::maintenance_event_content_unfeature,
    ),
    (
        "content.thread.move",
        LogScope::Content,
        Lang::maintenance_event_content_thread_move,
    ),
    // No code path writes this event; it stays listed so existing records of it keep their label and visibility
    // scope.
    (
        "content.thread.tags",
        LogScope::Content,
        Lang::maintenance_event_content_thread_tags,
    ),
    (
        "identity.role.assign",
        LogScope::Admin,
        Lang::maintenance_event_identity_role_assign,
    ),
    (
        "report.submit",
        LogScope::Admin,
        Lang::maintenance_event_report_submit,
    ),
    (
        "report.resolve",
        LogScope::Admin,
        Lang::maintenance_event_report_resolve,
    ),
    (
        "sanction.issue",
        LogScope::Admin,
        Lang::maintenance_event_sanction_issue,
    ),
    (
        "sanction.revoke",
        LogScope::Admin,
        Lang::maintenance_event_sanction_revoke,
    ),
    (
        "sanction.expire",
        LogScope::Admin,
        Lang::maintenance_event_sanction_expire,
    ),
    (
        "appeal.submit",
        LogScope::Admin,
        Lang::maintenance_event_appeal_submit,
    ),
    (
        "appeal.resolve",
        LogScope::Admin,
        Lang::maintenance_event_appeal_resolve,
    ),
    (
        "restore.submit",
        LogScope::Content,
        Lang::maintenance_event_restore_submit,
    ),
    (
        "restore.resolve",
        LogScope::Content,
        Lang::maintenance_event_restore_resolve,
    ),
    (
        "organization.forum.save",
        LogScope::Admin,
        Lang::maintenance_event_organization_forum_save,
    ),
    (
        "organization.tag.save",
        LogScope::Admin,
        Lang::maintenance_event_organization_tag_save,
    ),
    (
        "organization.tag.merge",
        LogScope::Admin,
        Lang::maintenance_event_organization_tag_merge,
    ),
    (
        "organization.tag.alias.remove",
        LogScope::Admin,
        Lang::maintenance_event_organization_tag_alias_remove,
    ),
    (
        "invite.batch.create",
        LogScope::Admin,
        Lang::maintenance_event_invite_batch_create,
    ),
    (
        "invite.status.change",
        LogScope::Admin,
        Lang::maintenance_event_invite_status_change,
    ),
    (
        "site.settings.save",
        LogScope::Admin,
        Lang::maintenance_event_site_settings_save,
    ),
    (
        "site.document.save",
        LogScope::Admin,
        Lang::maintenance_event_site_document_save,
    ),
    (
        "site.branding.save",
        LogScope::Admin,
        Lang::maintenance_event_site_branding_save,
    ),
    (
        "skin.import",
        LogScope::Admin,
        Lang::maintenance_event_skin_import,
    ),
    (
        "skin.select",
        LogScope::Admin,
        Lang::maintenance_event_skin_select,
    ),
    (
        "skin.delete",
        LogScope::Admin,
        Lang::maintenance_event_skin_delete,
    ),
    (
        "skin.colors",
        LogScope::Admin,
        Lang::maintenance_event_skin_colors,
    ),
    (
        "layout.save",
        LogScope::Admin,
        Lang::maintenance_event_layout_save,
    ),
    (
        "mail.settings.save",
        LogScope::Admin,
        Lang::maintenance_event_mail_settings_save,
    ),
    (
        "mail.template.save",
        LogScope::Admin,
        Lang::maintenance_event_mail_template_save,
    ),
    (
        "mail.test.request",
        LogScope::Admin,
        Lang::maintenance_event_mail_test_request,
    ),
    (
        "mail.test.result",
        LogScope::Admin,
        Lang::maintenance_event_mail_test_result,
    ),
    (
        "task.retry",
        LogScope::Admin,
        Lang::maintenance_event_task_retry,
    ),
    (
        "privacy.request",
        LogScope::Admin,
        Lang::maintenance_event_privacy_request,
    ),
    (
        "privacy.cancel",
        LogScope::Admin,
        Lang::maintenance_event_privacy_cancel,
    ),
    (
        "privacy.retry",
        LogScope::Admin,
        Lang::maintenance_event_privacy_retry,
    ),
    (
        "privacy.complete",
        LogScope::Admin,
        Lang::maintenance_event_privacy_complete,
    ),
    (
        "identity.profile.update",
        LogScope::Admin,
        Lang::maintenance_event_identity_profile_update,
    ),
    (
        "identity.privacy.update",
        LogScope::Admin,
        Lang::maintenance_event_identity_privacy_update,
    ),
    (
        "identity.email.change_request",
        LogScope::Admin,
        Lang::maintenance_event_identity_email_change_request,
    ),
    (
        "identity.email.change",
        LogScope::Admin,
        Lang::maintenance_event_identity_email_change,
    ),
    (
        "api.key.create",
        LogScope::Admin,
        Lang::maintenance_event_api_key_create,
    ),
    (
        "api.key.rotate",
        LogScope::Admin,
        Lang::maintenance_event_api_key_rotate,
    ),
    (
        "api.key.revoke",
        LogScope::Admin,
        Lang::maintenance_event_api_key_revoke,
    ),
    (
        "identity.password.change",
        LogScope::Admin,
        Lang::maintenance_event_identity_password_change,
    ),
    (
        "identity.password.set",
        LogScope::Admin,
        Lang::maintenance_event_identity_password_set,
    ),
    (
        "identity.password.request",
        LogScope::Admin,
        Lang::maintenance_event_identity_password_request,
    ),
    (
        "identity.password.reset",
        LogScope::Admin,
        Lang::maintenance_event_identity_password_reset,
    ),
    (
        "identity.session.logout_all",
        LogScope::Admin,
        Lang::maintenance_event_identity_session_logout_all,
    ),
];

/// Event names contain only letters, digits and dots and are spliced directly into the SQL IN list; a unit test guards
/// this assumption.
fn event_list(scope: LogScope) -> String {
    EVENTS
        .iter()
        .filter(|(_, event_scope, _)| *event_scope == scope)
        .map(|(name, _, _)| format!("'{name}'"))
        .collect::<Vec<_>>()
        .join(",")
}
static CONTENT_EVENTS: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| event_list(LogScope::Content));
static ADMIN_EVENTS: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| event_list(LogScope::Admin));

impl LogEntry {
    pub fn label(&self) -> &str {
        EVENTS
            .iter()
            .find(|(name, _, _)| *name == self.operation)
            .map_or(
                crate::i18n::current().maintenance_event_other(),
                |(_, _, label)| label(crate::i18n::current()),
            )
    }
    pub fn result_label(&self) -> &str {
        let t = crate::i18n::current();
        match self.result.as_str() {
            "unchanged" | "noop" => t.maintenance_logs_result_unchanged(),
            // Outcomes of reports and appeals: both are written in `result`, with the reason in `reason`.
            "resolved" => t.maintenance_logs_result_resolved(),
            "accepted" => t.maintenance_logs_result_accepted(),
            "rejected" | "dismissed" => t.maintenance_logs_result_rejected(),
            _ => t.maintenance_logs_result_done(),
        }
    }
    pub fn time(&self) -> String {
        crate::administration::local_time(self.dateline)
    }
}

fn display_time(timestamp: i64) -> String {
    crate::clock::datetime(timestamp, &crate::clock::current())
}
#[derive(Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct LogFilter {
    pub kind: String,
    pub operator: String,
    pub target: String,
    pub from: String,
    pub until: String,
    pub page: Option<i64>,
}
impl LogFilter {
    pub fn location(&self, page: i64) -> String {
        let mut q = url::form_urlencoded::Serializer::new(String::new());
        for (name, value) in [
            ("kind", self.kind.as_str()),
            ("operator", &self.operator),
            ("target", &self.target),
            ("from", &self.from),
            ("until", &self.until),
        ] {
            if !value.is_empty() {
                q.append_pair(name, value);
            }
        }
        q.append_pair("page", &page.to_string());
        format!("/manage/logs?{}", q.finish())
    }
}
pub async fn logs(db: &Database, actor: Actor, page: i64) -> Result<(Vec<LogEntry>, i64)> {
    logs_filtered(
        db,
        actor,
        &LogFilter {
            page: Some(page),
            ..Default::default()
        },
    )
    .await
}
pub async fn logs_filtered(
    db: &Database,
    actor: Actor,
    filter: &LogFilter,
) -> Result<(Vec<LogEntry>, i64)> {
    let role = staff(db, actor).await?;
    let page = filter.page.unwrap_or(1);
    if !(1..=i64::MAX / PAGE_SIZE).contains(&page) {
        return Err(IdentityError::Validation(
            crate::i18n::current().maintenance_logs_error_page(),
        ));
    }
    // The forum of a content log is in the `fid` column. Moving a thread involves two forums: `fid` records the source
    // forum and `to_fid` the target forum, and moderators of both must see it. Other events have `to_fid` NULL, which
    // does not affect matching. Moderators manage the forums they are appointed to and their child forums, by the
    // forums' current hierarchy.
    let (fid, moved_to) = ("l.fid", "l.to_fid");
    let guard = active(actor);
    let admin_events = &*ADMIN_EVENTS;
    let content_events = &*CONTENT_EVENTS;
    let mut clause = format!(
        "FROM gonof_common_log l LEFT JOIN gonof_common_member m ON m.uid=l.operationuid WHERE {guard} AND ((l.type IN ({content_events}) AND (?=1 OR ?=2 OR {moderates_from} OR {moderates_to})) OR ({role}=1 AND l.type IN ({admin_events})))",
        // UIDs are server-issued integers, written directly into the statement as in `active()`.
        moderates_from = crate::organization::moderates_fid_sql(&actor.uid.to_string(), fid),
        moderates_to = crate::organization::moderates_fid_sql(&actor.uid.to_string(), moved_to),
    );
    if !filter.kind.is_empty() {
        if !matches!(
            filter.kind.as_str(),
            "content"
                | "identity"
                | "report"
                | "sanction"
                | "appeal"
                | "restore"
                | "organization"
                | "invite"
                | "site"
                | "mail"
                | "task"
                | "privacy"
        ) {
            return Err(IdentityError::Validation(
                crate::i18n::current().maintenance_logs_error_kind(),
            ));
        }
        clause.push_str(&format!(" AND l.type LIKE '{}.%'", filter.kind));
    }
    // The target member is filtered by the l.uid column: the member acted on is bound to that column, which is indexed;
    // target_uid in data is written by only four kinds of events, so filtering by it would miss password changes,
    // password recovery and signing out of all devices triggered by members themselves.
    // Site settings and mail settings bind the administrator themself to uid, so filtering by their ID as the target
    // member matches those too.
    for (value, column) in [
        (&filter.operator, "l.operationuid"),
        (&filter.target, "l.uid"),
    ] {
        if !value.is_empty() {
            let number =
                value
                    .parse::<i64>()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or(IdentityError::Validation(
                        crate::i18n::current().maintenance_logs_error_member(),
                    ))?;
            clause.push_str(&format!(" AND {column}={number}"));
        }
    }
    for (value, operator, next) in [(&filter.from, ">=", false), (&filter.until, "<", true)] {
        if !value.is_empty() {
            let date = chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").map_err(|_| {
                IdentityError::Validation(crate::i18n::current().maintenance_logs_error_date())
            })?;
            let date = if next {
                date.succ_opt().ok_or(IdentityError::Validation(
                    crate::i18n::current().maintenance_logs_error_date(),
                ))?
            } else {
                date
            };
            // Boundaries use midnight in the display time zone: the filtered range must match the times shown on the
            // page.
            let stamp = crate::clock::parse_input(
                &format!("{}T00:00", date.format("%Y-%m-%d")),
                &crate::clock::current(),
            )
            .ok_or(IdentityError::Validation(
                crate::i18n::current().maintenance_logs_error_date(),
            ))?;
            clause.push_str(&format!(" AND l.dateline{operator}{stamp}"));
        }
    }
    let total: i64 = crate::db::query_scalar(&format!("SELECT COUNT(*) {clause}"))
        .bind(role)
        .bind(role)
        .fetch_one(db.executor())
        .await?;
    if page > ((total + PAGE_SIZE - 1) / PAGE_SIZE).max(1) {
        return Err(IdentityError::NotFound);
    }
    let rows=crate::db::query(&format!("SELECT l.id, l.type, l.data, l.dateline, m.username AS operator {clause} ORDER BY l.id DESC LIMIT ? OFFSET ?"))
        .bind(role).bind(role).bind(PAGE_SIZE).bind((page-1)*PAGE_SIZE).fetch_all(db.executor()).await?;
    let rows = rows
        .iter()
        .map(|row| {
            let data: serde_json::Value = serde_json::from_str(&db::text(row, "data")?)
                .map_err(|error| IdentityError::Internal(error.into()))?;
            let t = crate::i18n::current();
            let target = if let Some(pid) = data.get("pid").and_then(|v| v.as_i64()) {
                format!("PID {pid}")
            } else if let Some(uid) = data.get("target_uid").and_then(|v| v.as_i64()) {
                format!("UID {uid}")
            } else if let Some(id) = data.get("appeal_id").and_then(|v| v.as_i64()) {
                t.maintenance_logs_target_appeal(id.to_string())
            } else if let Some(id) = data.get("report_id").and_then(|v| v.as_i64()) {
                t.maintenance_logs_target_report(id.to_string())
            } else if let Some(id) = data.get("sanction_id").and_then(|v| v.as_i64()) {
                t.maintenance_logs_target_sanction(id.to_string())
            } else if let Some(id) = data.get("fid").and_then(|v| v.as_i64()) {
                format!("FID {id}")
            } else if let Some(id) = data.get("tid").and_then(|v| v.as_i64()) {
                format!("TID {id}")
            } else if let Some(id) = data.get("eventid").and_then(|v| v.as_i64()) {
                t.maintenance_logs_target_task(id.to_string())
            } else if let Some(id) = data.get("request_id").and_then(|v| v.as_i64()) {
                t.maintenance_logs_target_data_request(id.to_string())
            } else {
                String::new()
            };
            Ok(LogEntry {
                id: row.try_get("id")?,
                operator: row
                    .try_get::<Option<String>, _>("operator")?
                    .unwrap_or_else(|| t.maintenance_logs_system().into()),
                operation: db::text(row, "type")?,
                reason: data
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .into(),
                result: data
                    .get("result")
                    .and_then(|v| v.as_str())
                    .unwrap_or("success")
                    .into(),
                target,
                dateline: row.try_get("dateline")?,
            })
        })
        .collect::<Result<_>>()?;
    Ok((rows, total))
}

pub struct RoleMember {
    pub uid: i64,
    pub username: String,
    pub adminid: i64,
    pub version: i64,
    pub forum_ids: Vec<i64>,
}
pub async fn members(
    db: &Database,
    actor: Actor,
    q: &str,
    page: i64,
) -> Result<(Vec<RoleMember>, i64)> {
    if staff(db, actor).await? != 1 {
        return Err(IdentityError::Forbidden);
    }
    if !(1..=i64::MAX / PAGE_SIZE).contains(&page) || q.chars().count() > 200 {
        return Err(IdentityError::Validation(
            crate::i18n::current().forum_moderators_error_query(),
        ));
    }
    let search = like(q);
    let guard = active(actor);
    let total:i64=crate::db::query_scalar(&format!("SELECT COUNT(*) FROM gonof_common_member WHERE {guard} AND password IS NOT NULL AND LOWER(username) LIKE ? ESCAPE '!'"))
        .bind(&search).fetch_one(db.executor()).await?;
    if page > ((total + PAGE_SIZE - 1) / PAGE_SIZE).max(1) {
        return Err(IdentityError::NotFound);
    }
    let rows=crate::db::query(&format!("SELECT uid, username, adminid, credentials_version FROM gonof_common_member WHERE {guard} AND password IS NOT NULL AND LOWER(username) LIKE ? ESCAPE '!' ORDER BY uid LIMIT ? OFFSET ?"))
        .bind(search).bind(PAGE_SIZE).bind((page-1)*PAGE_SIZE).fetch_all(db.executor()).await?;
    Ok((
        rows.iter()
            .map(|row| {
                Ok(RoleMember {
                    uid: row.try_get("uid")?,
                    username: db::text(row, "username")?,
                    adminid: row.try_get("adminid")?,
                    version: row.try_get("credentials_version")?,
                    forum_ids: vec![],
                })
            })
            .collect::<Result<_>>()?,
        total,
    ))
}
pub async fn member(db: &Database, actor: Actor, uid: i64) -> Result<RoleMember> {
    if staff(db, actor).await? != 1 {
        return Err(IdentityError::Forbidden);
    }
    let guard = active(actor);
    let row=crate::db::query(&format!("SELECT uid, username, adminid, credentials_version FROM gonof_common_member WHERE {guard} AND uid = ? AND password IS NOT NULL"))
        .bind(uid).fetch_optional(db.executor()).await?.ok_or(IdentityError::NotFound)?;
    let forum_ids = crate::db::query_scalar(&format!(
        "SELECT fid FROM gonof_forum_moderator WHERE {guard} AND uid = ? ORDER BY fid"
    ))
    .bind(uid)
    .fetch_all(db.executor())
    .await?;
    Ok(RoleMember {
        uid: row.try_get("uid")?,
        username: db::text(&row, "username")?,
        adminid: row.try_get("adminid")?,
        version: row.try_get("credentials_version")?,
        forum_ids,
    })
}
fn like(value: &str) -> String {
    format!(
        "%{}%",
        value
            .trim()
            .to_lowercase()
            .replace('!', "!!")
            .replace('%', "!%")
            .replace('_', "!_")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_event_registry_is_unique_quotable_and_covers_content_publication() {
        let mut seen = std::collections::BTreeSet::new();
        for (name, _, label) in EVENTS {
            assert!(seen.insert(*name), "repeated event name: {name}");
            assert!(
                !label(crate::i18n::current()).is_empty(),
                "{name} has no label"
            );
            // Names are spliced directly into the IN list, so they must contain no quotes or backslashes.
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_'),
                "the event name cannot be spliced into SQL: {name}"
            );
        }
        // What is actually written to gonof_common_log are the names with .publish; see the finish call sites in
        // content/writes.rs.
        let content = event_list(LogScope::Content);
        assert!(content.contains("'content.thread.publish'"));
        assert!(content.contains("'content.reply.publish'"));
        // Names that nothing writes must not be listed.
        for dead in [
            "'content.thread'",
            "'content.reply'",
            "'thread.move'",
            "'thread.tags'",
        ] {
            assert!(!content.contains(dead), "a dead event name is back: {dead}");
        }
        assert!(event_list(LogScope::Admin).contains("'identity.password.set'"));
        // The lists are generated from the same table, so there cannot be a combination that "can be filtered but has
        // no label".
        let mut entry = LogEntry {
            id: 0,
            operator: String::new(),
            operation: String::new(),
            reason: String::new(),
            result: String::new(),
            target: String::new(),
            dateline: 0,
        };
        for (name, _, label) in EVENTS {
            entry.operation = (*name).to_owned();
            assert_eq!(entry.label(), label(crate::i18n::current()));
        }
        entry.operation = "content.thread".into();
        assert_eq!(
            entry.label(),
            "Management action",
            "a dead name has no label of its own"
        );
    }
}
