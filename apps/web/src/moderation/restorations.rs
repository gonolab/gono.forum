//! Content restoration requests: authors ask to restore their own posts that were hidden by governance, or withdrawn
//! with the author time window already past; staff with forum permissions accept or reject them.
//!
//! Reports, sanction appeals and restoration requests are three independent record sets: reports target other people's
//! content, appeals target account sanctions, and restoration requests only target one's own posts. The rule that
//! authors restore their own withdrawals within the time window is unchanged, and withdrawals within the window are
//! not accepted as requests.
use super::*;
use crate::content::edits::ContentEditor;
use crate::db::Tx;
use std::collections::BTreeMap;

/// Restoration request rules, taken from the admin area's "Security › Anti-spam & governance". Requests bypass the
/// write gate (accepted as usual in read-only mode), so the page handler takes the settings from the page frame and
/// passes them in (docs/settings-and-caching.md, "Write gate and locks").
#[derive(Clone, Copy, Debug)]
pub struct RestorationRules {
    /// Time window (seconds) in which authors restore their own withdrawals: withdrawals within the window are not
    /// accepted as requests.
    pub window_seconds: i64,
    /// Maximum requests per member over a rolling 24 hours (`settings.limit.restoration_daily`).
    pub daily_limit: i64,
    /// Cooldown (seconds, `settings.governance.restoration_cooldown_days`) before requesting the same post again after
    /// a rejection. Computed live from the time of the last conclusion, so a change applies retroactively at once.
    pub cooldown_seconds: i64,
}

impl RestorationRules {
    pub fn from_settings(settings: &crate::runtime::SiteSettings) -> Self {
        Self {
            window_seconds: crate::content::edits::edit_window(settings),
            daily_limit: settings.limit.restoration_daily,
            cooldown_seconds: settings
                .governance
                .restoration_cooldown_days
                .saturating_mul(86_400),
        }
    }
}
/// Entries per page in the member's own list.
pub const RESTORE_PAGE_SIZE: usize = 20;

#[derive(Clone, Debug)]
pub struct Restoration {
    pub id: i64,
    pub uid: i64,
    pub pid: i64,
    pub state: String,
    pub hidden_by: Option<i64>,
    pub reason: String,
    pub status: String,
    pub handler_uid: Option<i64>,
    pub resolution: String,
    pub handled_at: i64,
    pub dateline: i64,
}

impl Restoration {
    pub fn status_label(&self) -> &'static str {
        status_label(&self.status)
    }
    pub fn state_label(&self) -> &'static str {
        let t = crate::i18n::current();
        match self.state.as_str() {
            "hidden" => t.governance_state_hidden(),
            "withdrawn" => t.governance_state_withdrawn(),
            "both" => t.governance_state_both(),
            _ => t.governance_state_unknown(),
        }
    }
    pub fn time(&self) -> String {
        display_time(self.dateline)
    }
    pub fn handled_time(&self) -> String {
        display_time(self.handled_at)
    }
    /// The expected handling deadline shown on the page: submission time plus
    /// `settings.governance.restoration_due_days` days; overdue items are only flagged, not handled automatically. The
    /// template passes in the days from the page frame settings.
    pub fn due_time(&self, due_days: &i64) -> String {
        display_time(
            self.dateline
                .saturating_add(due_days.saturating_mul(86_400)),
        )
    }
    pub fn overdue(&self, due_days: &i64) -> bool {
        self.status == "pending"
            && identity::now()
                > self
                    .dateline
                    .saturating_add(due_days.saturating_mul(86_400))
    }
}

pub fn status_label(status: &str) -> &'static str {
    let t = crate::i18n::current();
    match status {
        "pending" => t.governance_status_pending(),
        "accepted" => t.governance_status_accepted(),
        "rejected" => t.governance_status_rejected(),
        _ => t.governance_status_unknown(),
    }
}

fn decode(row: crate::db::DbRow) -> Result<Restoration> {
    Ok(Restoration {
        id: row.try_get("id")?,
        uid: row.try_get("uid")?,
        pid: row.try_get("pid")?,
        state: db::text(&row, "state")?,
        hidden_by: row.try_get("hidden_by")?,
        reason: db::text(&row, "reason")?,
        status: db::text(&row, "status")?,
        handler_uid: row.try_get("handler_uid")?,
        resolution: db::text(&row, "resolution")?,
        handled_at: row.try_get("handled_at")?,
        dateline: row.try_get("dateline")?,
    })
}

/// Current state of the requested post. Both the management view and the author can see hidden or withdrawn original
/// text and titles.
#[derive(Clone, Debug)]
pub struct RestoreTarget {
    pub pid: i64,
    pub tid: i64,
    pub fid: i64,
    pub first: bool,
    pub authorid: Option<i64>,
    pub subject: String,
    pub body: String,
    pub forum_name: String,
    pub forum_slug: String,
    /// This floor has `invisible=-1`.
    pub hidden: bool,
    /// This floor's `invisible` is a reserved value other than 0 or -1.
    pub reserved: bool,
    /// This floor is withdrawn; for a first post the thread's withdrawal time is checked as well.
    pub withdrawn: bool,
    pub erased: bool,
    /// The reply is out of sight because its first post or parent floor is hidden.
    pub inherited: bool,
    pub dateline: i64,
}

/// Whether the author can submit a restoration request for this post.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Eligibility {
    /// A request is possible; the value is the state at submission, written to the `state` column.
    Eligible(&'static str),
    /// The withdrawal is still within the author time window; restore it yourself on the thread page.
    SelfRestore,
    /// This floor is public; there is no state to restore.
    Public,
    /// Out of sight only because the thread or parent floor is hidden; this floor itself was not acted on.
    Inherited,
    Erased,
    Reserved,
}

impl Eligibility {
    pub fn message(self) -> &'static str {
        match self {
            Self::Eligible(_) => "",
            Self::SelfRestore => crate::i18n::current().governance_restore_self(),
            Self::Public => crate::i18n::current().governance_restore_public(),
            Self::Inherited => crate::i18n::current().governance_restore_inherited(),
            Self::Erased => crate::i18n::current().governance_restore_erased(),
            Self::Reserved => crate::i18n::current().governance_restore_reserved(),
        }
    }
    pub fn eligible(self) -> bool {
        matches!(self, Self::Eligible(_))
    }
}

/// Eligibility only looks at this floor's own state; the logic lives in code and the SQL only fetches raw fields.
/// Permanently erased posts never appear in the member's own list, so `erased` is passed separately.
pub fn eligibility(
    own: &crate::content::OwnState,
    erased: bool,
    window_seconds: i64,
    now: i64,
) -> Eligibility {
    if erased {
        return Eligibility::Erased;
    }
    if own.reserved {
        return Eligibility::Reserved;
    }
    match (own.hidden, own.withdrawn) {
        (true, true) => Eligibility::Eligible("both"),
        (true, false) => Eligibility::Eligible("hidden"),
        (false, true) => {
            if now >= own.dateline && now - own.dateline < window_seconds {
                Eligibility::SelfRestore
            } else {
                Eligibility::Eligible("withdrawn")
            }
        }
        (false, false) if own.inherited => Eligibility::Inherited,
        (false, false) => Eligibility::Public,
    }
}

impl RestoreTarget {
    pub fn own(&self) -> crate::content::OwnState {
        crate::content::OwnState {
            hidden: self.hidden,
            reserved: self.reserved,
            withdrawn: self.withdrawn,
            inherited: self.inherited,
            dateline: self.dateline,
        }
    }
    pub fn eligibility(&self, window_seconds: i64, now: i64) -> Eligibility {
        eligibility(&self.own(), self.erased, window_seconds, now)
    }
    pub fn state_label(&self) -> &'static str {
        let t = crate::i18n::current();
        match (self.hidden, self.withdrawn, self.erased) {
            (_, _, true) => t.governance_target_erased(),
            (true, true, _) => t.governance_target_hidden_withdrawn(),
            (true, false, _) => t.governance_target_hidden(),
            (false, true, _) => t.governance_target_withdrawn(),
            (false, false, _) if self.inherited => t.governance_target_inherited(),
            _ => t.common_status_public(),
        }
    }
    pub fn href(&self) -> String {
        format!("/posts/{}", self.pid)
    }
    pub fn manage_href(&self) -> String {
        format!("/manage/content/{}", self.pid)
    }
}

const TARGET_SQL: &str = "SELECT p.pid,p.tid,p.fid,p.first,p.authorid,p.message,p.invisible,p.deleted_at,p.erased_at,p.dateline,
        t.subject,t.deleted_at AS thread_deleted_at,f.name AS forum_name,f.slug AS forum_slug,
        firstpost.invisible AS first_invisible,parent.invisible AS parent_invisible
    FROM gonof_forum_post p
    JOIN gonof_forum_thread t ON t.tid=p.tid
    JOIN gonof_forum_forum f ON f.fid=t.fid
    JOIN gonof_forum_post firstpost ON firstpost.first_tid=t.tid
    LEFT JOIN gonof_forum_post parent ON parent.pid=p.root_pid AND parent.tid=p.tid
    WHERE p.pid=?";

fn decode_target(row: &crate::db::DbRow) -> Result<RestoreTarget> {
    let first = row.try_get::<i64, _>("first")? == 1;
    let invisible: i64 = row.try_get("invisible")?;
    let first_invisible: i64 = row.try_get("first_invisible")?;
    let parent_invisible: Option<i64> = row.try_get("parent_invisible")?;
    Ok(RestoreTarget {
        pid: row.try_get("pid")?,
        tid: row.try_get("tid")?,
        fid: row.try_get("fid")?,
        first,
        authorid: row.try_get("authorid")?,
        subject: db::text(row, "subject")?,
        body: db::text(row, "message")?,
        forum_name: db::text(row, "forum_name")?,
        forum_slug: db::text(row, "forum_slug")?,
        hidden: invisible == -1,
        reserved: ![0, -1].contains(&invisible),
        withdrawn: row.try_get::<i64, _>("deleted_at")? > 0
            || (first && row.try_get::<i64, _>("thread_deleted_at")? > 0),
        erased: row.try_get::<i64, _>("erased_at")? > 0,
        inherited: !first && (first_invisible != 0 || parent_invisible.is_some_and(|v| v != 0)),
        dateline: row.try_get("dateline")?,
    })
}

/// Reads the requested post. When `owner` is `Some`, only that member's own posts are returned; other people's posts
/// 404 just like missing ones.
pub async fn restore_target(db: &Database, pid: i64, owner: Option<i64>) -> Result<RestoreTarget> {
    if pid <= 0 {
        return Err(ContentWriteError::NotFound);
    }
    let row = crate::db::query(TARGET_SQL)
        .bind(pid)
        .fetch_optional(db.executor())
        .await?
        .ok_or(ContentWriteError::NotFound)?;
    let target = decode_target(&row)?;
    if owner.is_some() && target.authorid != owner {
        return Err(ContentWriteError::NotFound);
    }
    Ok(target)
}

/// Moderators, trainee moderators and global moderators only see requests within their authorized scope (by the
/// forum the post is currently in); administrators see everything; reviewers do not handle restoration requests.
/// Trainee moderators can only accept requests that merely unhide; those that restore a withdrawal are refused at
/// execution (`content::edits::staff_action_allowed`).
fn scope(actor: Actor) -> String {
    format!(
        "EXISTS (SELECT 1 FROM gonof_common_member m WHERE m.uid={} AND m.credentials_version={} AND m.`freeze`=0 AND m.account_state='active' AND (m.adminid=1 OR (m.adminid IN (2,3,4) AND EXISTS (SELECT 1 FROM gonof_forum_post sp WHERE sp.pid=r.pid AND (m.adminid=2 OR {})))))",
        actor.uid,
        actor.credentials_version,
        crate::organization::moderates_fid_sql("m.uid", "sp.fid")
    )
}

/// Submits a restoration request and returns its id. Replaying the same key returns the original request; when the
/// same post already has a pending request, that one is returned.
pub async fn submit_restoration(
    db: &Database,
    actor: Actor,
    pid: i64,
    text: &str,
    key: &str,
    rules: &RestorationRules,
    now: i64,
) -> Result<i64> {
    let text = reason(text, 1000)?;
    if !valid_key(key) || now <= 0 || pid <= 0 {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().governance_error_restoration_key(),
        ));
    }
    for attempt in 0..3 {
        let result = submit_inner(db, actor, pid, text, key, rules, now).await;
        if result.as_ref().is_err_and(|e| e.retryable()) && attempt < 2 {
            continue;
        }
        return result;
    }
    unreachable!()
}

async fn submit_inner(
    db: &Database,
    actor: Actor,
    pid: i64,
    text: &str,
    key: &str,
    rules: &RestorationRules,
    now: i64,
) -> Result<i64> {
    let mut tx = db.begin_write().await?;
    // The requester must be the author, so all requests for one post come from the same member; locking this member's
    // row serializes the replay, dedup, cooldown and quota checks with the insert. Later reads take their snapshot
    // after the lock is held.
    lock_accounts(db, &mut tx, actor, None).await?;
    let previous =
        crate::db::query("SELECT * FROM gonof_forum_restore_request WHERE uid=? AND request_key=?")
            .bind(actor.uid)
            .bind(key)
            .fetch_optional(db.observe(&mut *tx))
            .await?;
    if let Some(row) = previous {
        let r = decode(row)?;
        if r.pid != pid || r.reason != text {
            return Err(ContentWriteError::Conflict(
                crate::i18n::current().governance_error_restoration_changed(),
            ));
        }
        tx.commit().await?;
        return Ok(r.id);
    }
    let row = crate::db::query(TARGET_SQL)
        .bind(pid)
        .fetch_optional(db.observe(&mut *tx))
        .await?
        .ok_or(ContentWriteError::NotFound)?;
    let target = decode_target(&row)?;
    if target.authorid != Some(actor.uid) {
        return Err(ContentWriteError::NotFound);
    }
    let state = match target.eligibility(rules.window_seconds, now) {
        Eligibility::Eligible(state) => state,
        other => return Err(ContentWriteError::Validation(other.message())),
    };
    let pending: Option<i64> = crate::db::query_scalar(
        "SELECT id FROM gonof_forum_restore_request WHERE pid=? AND status='pending' ORDER BY id LIMIT 1",
    )
    .bind(pid)
    .fetch_optional(db.observe(&mut *tx))
    .await?;
    if let Some(id) = pending {
        tx.commit().await?;
        return Ok(id);
    }
    let rejected: Option<i64> = crate::db::query_scalar(
        "SELECT MAX(handled_at) FROM gonof_forum_restore_request WHERE pid=? AND status='rejected'",
    )
    .bind(pid)
    .fetch_one(db.observe(&mut *tx))
    .await?;
    if rejected.is_some_and(|at| now < at.saturating_add(rules.cooldown_seconds)) {
        return Err(ContentWriteError::Forbidden(
            crate::i18n::current().governance_error_restoration_cooldown(),
        ));
    }
    let count: i64 = crate::db::query_scalar(
        "SELECT COUNT(*) FROM gonof_forum_restore_request WHERE uid=? AND dateline>?",
    )
    .bind(actor.uid)
    .bind(now - 86_400)
    .fetch_one(db.observe(&mut *tx))
    .await?;
    if count >= rules.daily_limit {
        // Another request fits once the one at position `count - limit` from the oldest leaves the 24 hours.
        let leaving: Option<i64> = crate::db::query_scalar(
            "SELECT dateline FROM gonof_forum_restore_request WHERE uid=? AND dateline>? ORDER BY dateline LIMIT 1 OFFSET ?",
        )
        .bind(actor.uid)
        .bind(now - 86_400)
        .bind(count.saturating_sub(rules.daily_limit))
        .fetch_optional(db.observe(&mut *tx))
        .await?;
        return Err(ContentWriteError::Quota(
            crate::i18n::current().governance_error_restoration_quota(),
            leaving.map_or(86_400, |dateline| {
                dateline.saturating_add(86_400).saturating_sub(now)
            }),
        ));
    }
    let hidden_by = if target.hidden {
        hidden_by(db, &mut tx, actor.uid, pid).await?
    } else {
        None
    };
    let id = crate::db::query("INSERT INTO gonof_forum_restore_request(uid,pid,state,hidden_by,reason,dateline,request_key) VALUES(?,?,?,?,?,?,?)")
        .bind(actor.uid)
        .bind(pid)
        .bind(state)
        .bind(hidden_by)
        .bind(text)
        .bind(now)
        .bind(key)
        .execute(db.observe(&mut *tx))
        .await?
        .last_insert_id() as i64;
    audit(
        db,
        &mut tx,
        actor.uid,
        actor.uid,
        "restore.submit",
        serde_json::json!({"restore_id":id,"pid":pid,"tid":target.tid,"fid":target.fid,"state":state}),
        now,
    )
    .await?;
    tx.commit().await?;
    Ok(id)
}

/// The operator of the most recent effective governance hide of this post. Reads this post's hide log entries in
/// descending order via the log's (pid, dateline) index (the log's uid column is the post author, checked as
/// well). Whether one took effect is in data.result: hiding an already hidden post is recorded as
/// unchanged and must be skipped. Fetches one page at a time and stops at the first effective entry.
async fn hidden_by(db: &Database, tx: &mut Tx<'_>, author: i64, pid: i64) -> Result<Option<i64>> {
    const PAGE: usize = 20;
    let mut before = (i64::MAX, i64::MAX);
    loop {
        let rows = crate::db::query(
            "SELECT id,dateline,operationuid,data FROM gonof_common_log WHERE pid=? AND type='content.hide' AND uid=?
                AND (dateline<? OR (dateline=? AND id<?))
             ORDER BY dateline DESC,id DESC LIMIT ?",
        )
        .bind(pid)
        .bind(author)
        .bind(before.0)
        .bind(before.0)
        .bind(before.1)
        .bind(PAGE as i64)
        .fetch_all(db.observe(&mut **tx))
        .await?;
        for row in &rows {
            let data: serde_json::Value = serde_json::from_str(&crate::db::text(row, "data")?)
                .map_err(|error| ContentWriteError::Internal(error.into()))?;
            if data.get("result").and_then(serde_json::Value::as_str) == Some("applied") {
                return Ok(row.try_get("operationuid")?);
            }
        }
        match rows.last() {
            Some(last) if rows.len() == PAGE => {
                before = (last.try_get("dateline")?, last.try_get("id")?);
            }
            _ => return Ok(None),
        }
    }
}

/// The operator of the hide in effect on `pid` right now: `None` when the post is not hidden by governance. A
/// request records who hid the post when it was filed (`hidden_by`), but the post may have been unhidden and hidden
/// again by someone else since; this is the hide that accepting the request would undo.
async fn current_hider(db: &Database, tx: &mut Tx<'_>, pid: i64) -> Result<Option<i64>> {
    let row = crate::db::query("SELECT authorid,invisible FROM gonof_forum_post WHERE pid=?")
        .bind(pid)
        .fetch_optional(db.observe(&mut **tx))
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    match row.try_get::<Option<i64>, _>("authorid")? {
        Some(author) if row.try_get::<i64, _>("invisible")? == -1 => {
            hidden_by(db, tx, author, pid).await
        }
        _ => Ok(None),
    }
}

/// [`current_hider`] for the review page, in a read-only transaction: the recusal notice there and the check when
/// resolving use the same answer.
pub async fn hider_now(db: &Database, pid: i64) -> Result<Option<i64>> {
    let mut tx = db.begin_read_only().await?;
    let hider = current_hider(db, &mut tx, pid).await?;
    tx.commit().await?;
    Ok(hider)
}

pub struct RestorationPage {
    pub rows: Vec<Restoration>,
    pub total: usize,
}

/// The member's own requests, paged newest submission first.
pub async fn own_restorations(db: &Database, actor: Actor, page: usize) -> Result<RestorationPage> {
    active(db, actor).await?;
    if page == 0 {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().governance_error_page_number(),
        ));
    }
    let total: i64 =
        crate::db::query_scalar("SELECT COUNT(*) FROM gonof_forum_restore_request WHERE uid=?")
            .bind(actor.uid)
            .fetch_one(db.executor())
            .await?;
    let total = usize::try_from(total).map_err(anyhow::Error::from)?;
    if page > total.div_ceil(RESTORE_PAGE_SIZE).max(1) {
        return Err(ContentWriteError::NotFound);
    }
    let rows = crate::db::query(
        "SELECT * FROM gonof_forum_restore_request WHERE uid=? ORDER BY dateline DESC,id DESC LIMIT ? OFFSET ?",
    )
    .bind(actor.uid)
    .bind(RESTORE_PAGE_SIZE as i64)
    .bind(((page - 1) * RESTORE_PAGE_SIZE) as i64)
    .fetch_all(db.executor())
    .await?
    .into_iter()
    .map(decode)
    .collect::<Result<_>>()?;
    Ok(RestorationPage { rows, total })
}

/// Admin queue: pending requests in submission order (first come, first served), handled ones by id descending; fetches
/// at most 21 rows each time to tell whether there is a next page.
pub async fn restoration_queue(
    db: &Database,
    actor: Actor,
    status: &str,
    cursor: i64,
) -> Result<Vec<Restoration>> {
    active(db, actor).await?;
    if !matches!(status, "pending" | "accepted" | "rejected") || cursor < 0 {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().governance_manage_error_restoration_filter(),
        ));
    }
    let (order, compare) = if status == "pending" {
        ("ASC", ">")
    } else {
        ("DESC", "<")
    };
    let rows = crate::db::query(&format!(
        "SELECT r.* FROM gonof_forum_restore_request r WHERE r.status=? AND (?=0 OR r.id{compare}?) AND {} ORDER BY r.id {order} LIMIT 21",
        scope(actor)
    ))
    .bind(status)
    .bind(cursor)
    .bind(cursor)
    .fetch_all(db.executor())
    .await?;
    rows.into_iter().map(decode).collect()
}

pub async fn restoration_detail(
    db: &Database,
    actor: Actor,
    id: i64,
    staff: bool,
) -> Result<Restoration> {
    active(db, actor).await?;
    let guard = if staff {
        scope(actor)
    } else {
        format!("r.uid={}", actor.uid)
    };
    decode(
        crate::db::query(&format!(
            "SELECT r.* FROM gonof_forum_restore_request r WHERE r.id=? AND {guard}"
        ))
        .bind(id)
        .fetch_optional(db.executor())
        .await?
        .ok_or(ContentWriteError::NotFound)?,
    )
}

/// The latest request for each post in the member's own list. `pids` come from one list page, at most 20.
pub async fn latest_restorations(
    db: &Database,
    uid: i64,
    pids: &[i64],
) -> Result<BTreeMap<i64, Restoration>> {
    if pids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let sql = format!(
        "SELECT * FROM gonof_forum_restore_request WHERE uid=? AND pid IN ({}) ORDER BY id DESC",
        vec!["?"; pids.len()].join(",")
    );
    let mut query = crate::db::query(&sql).bind(uid);
    for pid in pids {
        query = query.bind(pid);
    }
    let mut latest = BTreeMap::new();
    for row in query.fetch_all(db.executor()).await? {
        let r = decode(row)?;
        latest.entry(r.pid).or_insert(r);
    }
    Ok(latest)
}

/// Admin content detail page: ids of this post's pending requests. The caller has already confirmed management
/// permission.
pub async fn pending_for_post(db: &Database, pid: i64) -> Result<Vec<i64>> {
    Ok(crate::db::query_scalar(
        "SELECT id FROM gonof_forum_restore_request WHERE pid=? AND status='pending' ORDER BY id LIMIT 20",
    )
    .bind(pid)
    .fetch_all(db.executor())
    .await?)
}

/// Whether the handler must recuse: the requester themselves, whoever made the hide the request was filed against, or
/// whoever made the hide in effect now (`hider`, from [`current_hider`]); administrators are the final reviewers and
/// are exempt.
pub fn recused(r: &Restoration, uid: i64, is_admin: bool, hider: Option<i64>) -> bool {
    !is_admin && (r.uid == uid || r.hidden_by == Some(uid) || hider == Some(uid))
}

/// Accept or reject. Accepting runs the existing unhide and withdrawal-restore actions with the management identity in
/// the same transaction; rejecting only records the conclusion and notifies the requester.
pub async fn resolve_restoration(
    db: &Database,
    editor: &ContentEditor,
    actor: Actor,
    id: i64,
    status: &str,
    text: &str,
    now: i64,
) -> Result<()> {
    if !matches!(status, "accepted" | "rejected") || now <= 0 {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().governance_manage_error_choose_result(),
        ));
    }
    let text = reason(text, 500)?;
    let found = restoration_detail(db, actor, id, true).await?;
    if found.status != "pending" {
        if found.status == status
            && found.resolution == text
            && found.handler_uid == Some(actor.uid)
        {
            return Ok(());
        }
        return Err(ContentWriteError::Conflict(
            crate::i18n::current().governance_manage_error_restoration_handled(),
        ));
    }
    for attempt in 0..3 {
        let result = resolve_inner(db, editor, actor, &found, status, text, now).await;
        if result.as_ref().is_err_and(|e| e.retryable()) && attempt < 2 {
            tokio::time::sleep(std::time::Duration::from_millis(20 * (attempt + 1))).await;
            continue;
        }
        return result;
    }
    unreachable!()
}

async fn resolve_inner(
    db: &Database,
    editor: &ContentEditor,
    actor: Actor,
    found: &Restoration,
    status: &str,
    text: &str,
    now: i64,
) -> Result<()> {
    let id = found.id;
    // Accepting recomputes the effective reply count and last reply, so like the admin buttons it uses the READ
    // COMMITTED content write transaction.
    let mut tx = db.begin_content_write().await?;
    let settings = crate::runtime::guard_write(db, &mut tx, true)
        .await
        .map_err(crate::organization::identity_error)?;
    // Lock order matches report handling: account → forum, thread, post → request row.
    let role = lock_accounts(db, &mut tx, actor, Some(found.uid)).await?;
    if !(1..=4).contains(&role) {
        return Err(ContentWriteError::Forbidden(
            crate::i18n::current().governance_manage_error_not_staff(),
        ));
    }
    // Scope and post locks first, so the hide judged for recusal cannot change before the decision commits.
    lock_scope(db, &mut tx, actor, role, found.pid).await?;
    let hider = current_hider(db, &mut tx, found.pid).await?;
    if recused(found, actor.uid, role == 1, hider) {
        return Err(ContentWriteError::Forbidden(
            crate::i18n::current().governance_manage_error_restoration_recuse(),
        ));
    }
    let changed = if status == "accepted" {
        editor
            .restore_for_request(
                &mut tx,
                &settings,
                actor,
                found.uid,
                found.pid,
                text,
                &format!("restore-request:{id}"),
                now,
            )
            .await?
    } else {
        false
    };
    let current = decode(
        crate::db::query("SELECT * FROM gonof_forum_restore_request WHERE id=? FOR UPDATE")
            .bind(id)
            .fetch_one(db.observe(&mut *tx))
            .await?,
    )?;
    if current.status != "pending" {
        if current.status == status
            && current.resolution == text
            && current.handler_uid == Some(actor.uid)
        {
            return Ok(());
        }
        return Err(ContentWriteError::Conflict(
            crate::i18n::current().governance_manage_error_restoration_handled(),
        ));
    }
    if now < current.dateline {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().governance_manage_error_restoration_time(),
        ));
    }
    crate::db::query(
        "UPDATE gonof_forum_restore_request SET status=?,handler_uid=?,resolution=?,handled_at=? WHERE id=?",
    )
    .bind(status)
    .bind(actor.uid)
    .bind(text)
    .bind(now)
    .bind(id)
    .execute(db.observe(&mut *tx))
    .await?;
    // When accepting changed content, the unhide / withdrawal restore itself has already sent the author a governance
    // notification, so it is not repeated. The notification keeps only the handler's conclusion;
    // "Your restoration request was accepted: …" is put together by the request's status when the list is shown
    // (`describe_notices`).
    if !(status == "accepted" && changed) {
        notify(
            db,
            &mut tx,
            Notice {
                uid: current.uid,
                authorid: actor.uid,
                kind: crate::notifications::record::Kind::Moderation,
                target: crate::notifications::record::Target::Post(current.pid),
                key: &format!("restore.{id}.result"),
                reason: text,
                now,
            },
        )
        .await?;
    }
    let fid: i64 = crate::db::query_scalar("SELECT fid FROM gonof_forum_post WHERE pid=?")
        .bind(current.pid)
        .fetch_one(db.observe(&mut *tx))
        .await?;
    audit(
        db,
        &mut tx,
        actor.uid,
        current.uid,
        "restore.resolve",
        serde_json::json!({"restore_id":id,"pid":current.pid,"fid":fid,"result":status,"reason":text,"changed":changed}),
        now,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Confirms that the handler still has permission for the forum the post is in, before either decision (accepting
/// then runs the unhide or restore, which checks again); locks forum, thread and post in that order, consistent with
/// content management, so the post's hide state stays put until the decision commits. Permanently erased posts can
/// still be rejected, which closes the request.
async fn lock_scope(
    db: &Database,
    tx: &mut Tx<'_>,
    actor: Actor,
    role: i64,
    pid: i64,
) -> Result<()> {
    let row = crate::db::query("SELECT tid,fid FROM gonof_forum_post WHERE pid=?")
        .bind(pid)
        .fetch_optional(db.observe(&mut **tx))
        .await?
        .ok_or(ContentWriteError::NotFound)?;
    let tid: i64 = row.try_get("tid")?;
    let fid: i64 = row.try_get("fid")?;
    let forum = crate::organization::lock_forum_path(db, tx, fid).await?;
    let current: i64 =
        crate::db::query_scalar("SELECT fid FROM gonof_forum_thread WHERE tid=? FOR UPDATE")
            .bind(tid)
            .fetch_one(db.observe(&mut **tx))
            .await?;
    let post_fid: i64 =
        crate::db::query_scalar("SELECT fid FROM gonof_forum_post WHERE pid=? FOR UPDATE")
            .bind(pid)
            .fetch_one(db.observe(&mut **tx))
            .await?;
    if current != fid || post_fid != fid {
        return Err(ContentWriteError::Conflict(
            crate::i18n::current().governance_manage_error_restoration_moved(),
        ));
    }
    // Moderators and trainee moderators manage their appointed forums and those forums' child forums; the parent
    // forum id comes from the locked forum path.
    if crate::identity::forum_role(role)
        && !crate::organization::moderator_locked(
            db,
            tx,
            actor.uid,
            fid,
            forum.try_get::<Option<i64>, _>("parent_fid")?,
        )
        .await?
    {
        return Err(ContentWriteError::NotFound);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(hidden: bool, withdrawn: bool) -> RestoreTarget {
        RestoreTarget {
            pid: 2,
            tid: 1,
            fid: 1,
            first: false,
            authorid: Some(5),
            subject: String::new(),
            body: String::new(),
            forum_name: String::new(),
            forum_slug: String::new(),
            hidden,
            reserved: false,
            withdrawn,
            erased: false,
            inherited: false,
            dateline: 1_000,
        }
    }

    #[test]
    fn eligibility_follows_own_state_and_keeps_the_author_window() {
        let window = 86_400;
        assert_eq!(
            target(true, false).eligibility(window, 1_100),
            Eligibility::Eligible("hidden")
        );
        assert_eq!(
            target(true, true).eligibility(window, 1_100),
            Eligibility::Eligible("both")
        );
        // Withdrawals within the window are restored by the author, so requests are not accepted; only past the window
        // can one request.
        assert_eq!(
            target(false, true).eligibility(window, 1_000 + window - 1),
            Eligibility::SelfRestore
        );
        assert_eq!(
            target(false, true).eligibility(window, 1_000 + window),
            Eligibility::Eligible("withdrawn")
        );
        assert_eq!(
            target(false, false).eligibility(window, 1_100),
            Eligibility::Public
        );
        let mut inherited = target(false, false);
        inherited.inherited = true;
        assert_eq!(inherited.eligibility(window, 1_100), Eligibility::Inherited);
        let mut erased = target(true, true);
        erased.erased = true;
        assert_eq!(erased.eligibility(window, 1_100), Eligibility::Erased);
        let mut reserved = target(false, false);
        reserved.reserved = true;
        assert_eq!(reserved.eligibility(window, 1_100), Eligibility::Reserved);
    }

    #[test]
    fn recusal_covers_requester_and_hider_but_not_administrators() {
        let r = Restoration {
            id: 1,
            uid: 5,
            pid: 2,
            state: "hidden".into(),
            hidden_by: Some(7),
            reason: "Reason".into(),
            status: "pending".into(),
            handler_uid: None,
            resolution: String::new(),
            handled_at: 0,
            dateline: 1,
        };
        assert!(recused(&r, 5, false, None));
        assert!(recused(&r, 7, false, None));
        assert!(!recused(&r, 8, false, None));
        assert!(!recused(&r, 5, true, None));
        assert!(!recused(&r, 7, true, None));
        // Hidden again by someone else after the request was filed.
        assert!(recused(&r, 8, false, Some(8)));
        assert!(recused(&r, 7, false, Some(8)));
        assert!(!recused(&r, 8, true, Some(8)));
    }
}
