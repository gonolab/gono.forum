use super::*;
use crate::{content, interaction};
use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ReportTarget {
    pub target_type: String,
    pub target_id: i64,
}
impl ReportTarget {
    pub fn label(&self) -> &str {
        let t = crate::i18n::current();
        match self.target_type.as_str() {
            "thread" => t.governance_target_thread(),
            "reply" => t.governance_target_reply(),
            "profile" => t.governance_target_profile(),
            "attachment" => t.governance_target_attachment(),
            _ => t.governance_target_content(),
        }
    }
    /// The heading of the report form.
    pub fn heading(&self) -> &str {
        let t = crate::i18n::current();
        match self.target_type.as_str() {
            "thread" => t.governance_report_thread(),
            "reply" => t.governance_report_reply(),
            "profile" => t.governance_report_profile(),
            "attachment" => t.governance_report_attachment(),
            _ => t.governance_report_content(),
        }
    }
    fn validate(&self) -> Result<()> {
        if self.target_id <= 0
            || !matches!(
                self.target_type.as_str(),
                "thread" | "reply" | "profile" | "attachment"
            )
        {
            return Err(ContentWriteError::Validation(
                crate::i18n::current().governance_error_report_target(),
            ));
        }
        Ok(())
    }
}
#[derive(Debug)]
pub struct ReportAttachment {
    pub aid: i64,
    pub filename: String,
}
#[derive(Debug)]
pub struct ReportTargetView {
    pub target: ReportTarget,
    pub owner: Option<i64>,
    pub title: String,
    pub body: String,
    pub href: String,
    pub manage_href: String,
    pub attachments: Vec<ReportAttachment>,
}

/// Floors the reporter can read: the same conditions as reading, judging forum and thread scope by the reporter's
/// signed-in identity, so forums and threads visible to members can be reported; private forums and floors hidden by
/// governance, withdrawn or permanently erased still cannot be found, so the report entry never exposes content the
/// reporter could not otherwise see. `reader_uid` may only come from an authenticated member.
fn readable_post(reader_uid: i64) -> String {
    format!(
        "{} AND {}",
        content::readable_thread_sql(Some(reader_uid)),
        content::public_post("p")
    )
}

// Only published references the reporter can read make an attachment reportable; drafts and private previews do not.
fn attachment_readable(reader_uid: i64) -> String {
    format!(
        "EXISTS (SELECT 1 FROM gonof_forum_attachment_reference ar JOIN gonof_forum_post p ON p.pid=ar.pid JOIN gonof_forum_thread t ON t.tid=p.tid JOIN gonof_forum_forum f ON f.fid=t.fid JOIN gonof_forum_post firstpost ON firstpost.first_tid=t.tid WHERE ar.aid=a.aid AND {}) OR EXISTS (SELECT 1 FROM gonof_common_member avatar_owner WHERE avatar_owner.avatar_aid=a.aid AND avatar_owner.account_state='active')",
        readable_post(reader_uid)
    )
}

/// `staff` means an authorized management view, without readability filtering; otherwise it queries the scope the
/// member `reader_uid` can read.
pub async fn report_target(
    db: &Database,
    target: &ReportTarget,
    staff: bool,
    reader_uid: i64,
) -> Result<ReportTargetView> {
    target.validate()?;
    let mut result = ReportTargetView {
        target: target.clone(),
        owner: None,
        title: String::new(),
        body: String::new(),
        href: String::new(),
        manage_href: String::new(),
        attachments: vec![],
    };
    match target.target_type.as_str() {
        "thread" | "reply" => {
            let condition = if target.target_type == "thread" {
                "p.first_tid=?"
            } else {
                "p.pid=? AND p.first=0"
            };
            let visibility = if staff {
                "1=1".into()
            } else {
                readable_post(reader_uid)
            };
            let row=crate::db::query(&format!("SELECT p.pid,p.tid,p.authorid,t.subject,t.deleted_at AS thread_deleted,firstpost.deleted_at AS first_deleted,p.message FROM gonof_forum_post p JOIN gonof_forum_thread t ON t.tid=p.tid JOIN gonof_forum_forum f ON f.fid=t.fid JOIN gonof_forum_post firstpost ON firstpost.first_tid=t.tid WHERE {condition} AND {visibility}"))
                .bind(target.target_id).fetch_optional(db.executor()).await?.ok_or(ContentWriteError::NotFound)?;
            let pid: i64 = row.try_get("pid")?;
            result.owner = row.try_get("authorid")?;
            result.title = if !staff
                && (row.try_get::<i64, _>("thread_deleted")? > 0
                    || row.try_get::<i64, _>("first_deleted")? > 0)
            {
                crate::i18n::current().common_withdrawn_title().into()
            } else {
                db::text(&row, "subject")?
            };
            result.body = db::text(&row, "message")?;
            result.href = format!("/posts/{pid}");
            result.manage_href = format!("/manage/content/{pid}");
            let rows=crate::db::query("SELECT a.aid,d.filename FROM gonof_forum_attachment_reference a JOIN gonof_forum_attachment_0 d ON d.aid=a.aid WHERE a.pid=? AND d.lifecycle='ready' AND d.scan_status='clean' ORDER BY a.aid LIMIT 20")
                .bind(pid).fetch_all(db.executor()).await?;
            for row in rows {
                result.attachments.push(ReportAttachment {
                    aid: row.try_get("aid")?,
                    filename: db::text(&row, "filename")?,
                });
            }
        }
        "profile" => {
            let row = crate::db::query(
                "SELECT uid,username,bio,avatar_aid,space_visibility,account_state FROM gonof_common_member WHERE uid=?",
            )
            .bind(target.target_id)
            .fetch_optional(db.executor())
            .await?
            .ok_or(ContentWriteError::NotFound)?;
            if db::text(&row, "account_state")? != "active" {
                return Err(ContentWriteError::NotFound);
            }
            result.owner = Some(target.target_id);
            result.title = db::text(&row, "username")?;
            let visibility =
                crate::identity::SpaceVisibility::parse(&db::text(&row, "space_visibility")?)
                    .map_err(anyhow::Error::from)?;
            if staff || visibility != crate::identity::SpaceVisibility::Private {
                result.body = db::text(&row, "bio")?;
            }
            result.href = crate::urls::space(target.target_id);
            if let Some(aid) = row.try_get::<Option<i64>, _>("avatar_aid")? {
                result.attachments.push(ReportAttachment {
                    aid,
                    filename: crate::i18n::current()
                        .governance_report_current_avatar()
                        .into(),
                });
            }
        }
        "attachment" => {
            let visibility = if staff {
                "1=1".into()
            } else {
                format!(
                    "d.lifecycle='ready' AND d.scan_status='clean' AND ({})",
                    attachment_readable(reader_uid)
                )
            };
            let row=crate::db::query(&format!("SELECT a.uid,d.filename FROM gonof_forum_attachment a JOIN gonof_forum_attachment_0 d ON d.aid=a.aid WHERE a.aid=? AND {visibility}"))
                .bind(target.target_id).fetch_optional(db.executor()).await?.ok_or(ContentWriteError::NotFound)?;
            result.owner = Some(row.try_get("uid")?);
            result.title = db::text(&row, "filename")?;
            // Use a reference this reader can open or the current avatar route; never expose an S3 key.
            let pid:Option<i64>=crate::db::query_scalar(&format!("SELECT p.pid FROM gonof_forum_attachment_reference ar JOIN gonof_forum_post p ON p.pid=ar.pid JOIN gonof_forum_thread t ON t.tid=p.tid JOIN gonof_forum_forum f ON f.fid=t.fid JOIN gonof_forum_post firstpost ON firstpost.first_tid=t.tid WHERE ar.aid=? AND {} ORDER BY p.pid LIMIT 1",readable_post(reader_uid)))
                .bind(target.target_id).fetch_optional(db.executor()).await?;
            if let Some(pid) = pid {
                result.href = format!("/media/{}?pid={pid}", target.target_id);
            }
        }
        _ => unreachable!(),
    }
    Ok(result)
}

#[derive(Clone, Debug)]
pub struct Report {
    pub id: i64,
    pub uid: i64,
    pub target: ReportTarget,
    pub reason: String,
    pub status: String,
    pub handler_uid: Option<i64>,
    pub resolution: String,
    pub dateline: i64,
    pub handled_at: i64,
}
impl Report {
    pub fn status_label(&self) -> &str {
        let t = crate::i18n::current();
        match self.status.as_str() {
            "resolved" => t.governance_report_resolved(),
            "dismissed" => t.governance_report_dismissed(),
            _ => t.governance_report_pending(),
        }
    }
    pub fn time(&self) -> String {
        display_time(self.dateline)
    }
    pub fn handled_time(&self) -> String {
        display_time(self.handled_at)
    }
}
fn decode(row: crate::db::DbRow) -> Result<Report> {
    Ok(Report {
        id: row.try_get("id")?,
        uid: row.try_get("uid")?,
        target: ReportTarget {
            target_type: db::text(&row, "target_type")?,
            target_id: row.try_get("target_id")?,
        },
        reason: db::text(&row, "reason")?,
        status: db::text(&row, "status")?,
        handler_uid: row.try_get("handler_uid")?,
        resolution: db::text(&row, "resolution")?,
        dateline: row.try_get("dateline")?,
        handled_at: row.try_get("handled_at")?,
    })
}
/// Posts reviewers can manage (docs/roles.md, "Reviewers"): replies that have been reported, or first posts
/// of threads that have been reported. Whether or not the report has been handled, a mistaken hide can still be
/// undone. Each of the two parts uses the `gonof_report_target` index.
pub(crate) fn reported_sql(post: &str) -> String {
    format!(
        "(({post}.first = 0 AND EXISTS (SELECT 1 FROM gonof_common_report rr WHERE rr.target_type = 'reply' AND rr.target_id = {post}.pid)) OR ({post}.first = 1 AND EXISTS (SELECT 1 FROM gonof_common_report rr WHERE rr.target_type = 'thread' AND rr.target_id = {post}.tid)))"
    )
}
/// Scope of the report queue: administrators and reviewers see everything; global moderators see all thread and reply
/// reports; moderators and trainee moderators see those in their appointed forums (including child forums).
fn scope(actor: Actor) -> String {
    format!(
        "EXISTS (SELECT 1 FROM gonof_common_member m WHERE m.uid={} AND m.credentials_version={} AND m.`freeze`=0 AND m.account_state='active' AND (m.adminid IN (1,5) OR (m.adminid IN (2,3,4) AND EXISTS (SELECT 1 FROM gonof_forum_post sp WHERE ((r.target_type='thread' AND sp.first_tid=r.target_id) OR (r.target_type='reply' AND sp.pid=r.target_id AND sp.first=0)) AND (m.adminid=2 OR {})))))",
        actor.uid,
        actor.credentials_version,
        crate::organization::moderates_fid_sql("m.uid", "sp.fid")
    )
}

/// `daily_limit` is the per-person cap on reports over a rolling 24 hours (`settings.limit.report_daily`). Reports
/// bypass the write gate (accepted as usual in read-only mode), so the page handler takes the settings from the page
/// frame and passes them in (docs/settings-and-caching.md, "Write gate and locks").
pub async fn submit_report(
    db: &Database,
    actor: Actor,
    target: &ReportTarget,
    text: &str,
    key: &str,
    now: i64,
    daily_limit: i64,
) -> Result<i64> {
    target.validate()?;
    let text = reason(text, 1000)?;
    if !valid_key(key) || now <= 0 {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().governance_error_report_key(),
        ));
    }
    for attempt in 0..3 {
        let result = submit_inner(db, actor, target, text, key, (now, daily_limit)).await;
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
    target: &ReportTarget,
    text: &str,
    key: &str,
    (now, daily_limit): (i64, i64),
) -> Result<i64> {
    let mut tx = db.begin_write().await?;
    lock_accounts(
        db,
        &mut tx,
        actor,
        (target.target_type == "profile").then_some(target.target_id),
    )
    .await?;
    // Report rows use a plain read without locking: submissions and handling for the same reporter both lock this
    // reporter's member row first (the lock_accounts above), so they are already serialized; the snapshot is taken
    // after the member lock is held and sees results the other side already committed.
    // Neither this read nor the duplicate check below is FOR UPDATE: the duplicate check often uses the
    // (target_type,target_id) index, so a locking read would lock other people's pending reports on the same target and
    // then wait for the content lock, while handling a report locks the content before the report rows, and the two
    // could wait on each other into a deadlock. Both paths go account → content → report rows.
    let previous =
        crate::db::query("SELECT * FROM gonof_common_report WHERE uid=? AND request_key=?")
            .bind(actor.uid)
            .bind(key)
            .fetch_optional(db.observe(&mut *tx))
            .await?;
    if let Some(row) = previous {
        let r = decode(row)?;
        if r.target.target_type != target.target_type
            || r.target.target_id != target.target_id
            || r.reason != text
        {
            return Err(ContentWriteError::Conflict(
                crate::i18n::current().governance_error_report_changed(),
            ));
        }
        tx.commit().await?;
        return Ok(r.id);
    }
    let duplicate:Option<i64>=crate::db::query_scalar("SELECT id FROM gonof_common_report WHERE uid=? AND target_type=? AND target_id=? AND status='pending' ORDER BY id LIMIT 1")
        .bind(actor.uid).bind(&target.target_type).bind(target.target_id).fetch_optional(db.observe(&mut *tx)).await?;
    if let Some(id) = duplicate {
        tx.commit().await?;
        return Ok(id);
    }
    let count: i64 = crate::db::query_scalar(
        "SELECT COUNT(*) FROM gonof_common_report WHERE uid=? AND dateline>?",
    )
    .bind(actor.uid)
    .bind(now - 86400)
    .fetch_one(db.observe(&mut *tx))
    .await?;
    if count >= daily_limit {
        return Err(ContentWriteError::Forbidden(
            crate::i18n::current().governance_error_report_quota(),
        ));
    }
    let owner = match target.target_type.as_str() {
        "thread" | "reply" => {
            let pid = if target.target_type == "thread" {
                crate::db::query_scalar("SELECT pid FROM gonof_forum_post WHERE first_tid=?")
                    .bind(target.target_id)
                    .fetch_optional(db.observe(&mut *tx))
                    .await?
                    .ok_or(ContentWriteError::NotFound)?
            } else {
                target.target_id
            };
            let t = interaction::lock_target_readable_by(db, &mut tx, pid, actor.uid).await?;
            if !t.visible || t.first != (target.target_type == "thread") {
                return Err(ContentWriteError::NotFound);
            }
            t.authorid
        }
        "profile" => {
            let member = crate::db::query(
                "SELECT uid,account_state FROM gonof_common_member WHERE uid=? FOR UPDATE",
            )
            .bind(target.target_id)
            .fetch_optional(db.observe(&mut *tx))
            .await?
            .ok_or(ContentWriteError::NotFound)?;
            if db::text(&member, "account_state")? != "active" {
                return Err(ContentWriteError::NotFound);
            }
            Some(member.try_get("uid")?)
        }
        "attachment" => {
            let row=crate::db::query(&format!("SELECT a.uid FROM gonof_forum_attachment a JOIN gonof_forum_attachment_0 d ON d.aid=a.aid WHERE a.aid=? AND d.lifecycle='ready' AND d.scan_status='clean' AND ({}) FOR UPDATE",attachment_readable(actor.uid)))
                .bind(target.target_id).fetch_optional(db.observe(&mut *tx)).await?.ok_or(ContentWriteError::NotFound)?;
            Some(row.try_get("uid")?)
        }
        _ => unreachable!(),
    };
    if owner == Some(actor.uid) {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().governance_error_report_own(),
        ));
    }
    let id=crate::db::query("INSERT INTO gonof_common_report(uid,target_type,target_id,reason,dateline,request_key) VALUES(?,?,?,?,?,?)").bind(actor.uid).bind(&target.target_type).bind(target.target_id).bind(text).bind(now).bind(key).execute(db.observe(&mut *tx)).await?.last_insert_id() as i64;
    audit(db,&mut tx,actor.uid,actor.uid,"report.submit",serde_json::json!({"report_id":id,"target_type":target.target_type,"target_id":target.target_id}),now).await?;
    tx.commit().await?;
    Ok(id)
}

pub async fn report_list(
    db: &Database,
    actor: Actor,
    staff: bool,
    status: &str,
    before: i64,
) -> Result<Vec<Report>> {
    active(db, actor).await?;
    if !matches!(status, "" | "pending" | "resolved" | "dismissed") || before < 0 {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().governance_error_report_filter(),
        ));
    }
    let guard = if staff {
        scope(actor)
    } else {
        format!("r.uid={}", actor.uid)
    };
    let rows=crate::db::query(&format!("SELECT r.* FROM gonof_common_report r WHERE {guard} AND (?='' OR r.status=?) AND (?=0 OR r.id<?) ORDER BY r.id DESC LIMIT 21"))
        .bind(status).bind(status).bind(before).bind(before).fetch_all(db.executor()).await?;
    rows.into_iter().map(decode).collect()
}
pub async fn report_detail(db: &Database, actor: Actor, id: i64, staff: bool) -> Result<Report> {
    active(db, actor).await?;
    let guard = if staff {
        scope(actor)
    } else {
        format!("r.uid={}", actor.uid)
    };
    decode(
        crate::db::query(&format!(
            "SELECT r.* FROM gonof_common_report r WHERE r.id=? AND {guard}"
        ))
        .bind(id)
        .fetch_optional(db.executor())
        .await?
        .ok_or(ContentWriteError::NotFound)?,
    )
}
pub async fn resolve_report(
    db: &Database,
    actor: Actor,
    id: i64,
    status: &str,
    text: &str,
    now: i64,
) -> Result<()> {
    if !matches!(status, "resolved" | "dismissed") || now <= 0 {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().governance_manage_error_choose_result(),
        ));
    }
    let text = reason(text, 1000)?;
    let found = report_detail(db, actor, id, true).await?;
    // Owner of the profile or attachment (reviewers recuse themselves from reports targeting themselves, just as
    // moderators do for thread and reply reports).
    let owner = if matches!(found.target.target_type.as_str(), "profile" | "attachment") {
        match report_target(db, &found.target, true, actor.uid).await {
            Ok(view) => view.owner,
            Err(ContentWriteError::NotFound) => None,
            Err(error) => return Err(error),
        }
    } else {
        None
    };
    let mut tx = db.begin_write().await?;
    let role = lock_accounts(db, &mut tx, actor, Some(found.uid)).await?;
    if !(1..=5).contains(&role) {
        return Err(ContentWriteError::Forbidden(
            crate::i18n::current().governance_manage_error_not_staff(),
        ));
    }
    if matches!(found.target.target_type.as_str(), "thread" | "reply") {
        let condition = if found.target.target_type == "thread" {
            "p.first_tid=?"
        } else {
            "p.pid=? AND p.first=0"
        };
        let row = crate::db::query(&format!("SELECT p.pid,p.tid,t.fid FROM gonof_forum_post p JOIN gonof_forum_thread t ON t.tid=p.tid WHERE {condition}"))
            .bind(found.target.target_id).fetch_optional(db.observe(&mut *tx)).await?.ok_or(ContentWriteError::NotFound)?;
        let pid: i64 = row.try_get("pid")?;
        let tid: i64 = row.try_get("tid")?;
        let fid: i64 = row.try_get("fid")?;
        // Lock the forum row and take its parent forum id at the same time: a moderator manages the appointed forum
        // and its child forums.
        let parent: Option<i64> = crate::db::query_scalar(
            "SELECT parent_fid FROM gonof_forum_forum WHERE fid=? FOR UPDATE",
        )
        .bind(fid)
        .fetch_one(db.observe(&mut *tx))
        .await?;
        let current_fid: i64 =
            crate::db::query_scalar("SELECT fid FROM gonof_forum_thread WHERE tid=? FOR UPDATE")
                .bind(tid)
                .fetch_one(db.observe(&mut *tx))
                .await?;
        if current_fid != fid {
            return Err(ContentWriteError::Conflict(
                crate::i18n::current().governance_manage_error_report_moved(),
            ));
        }
        let post = crate::db::query(
            "SELECT tid,fid,authorid FROM gonof_forum_post WHERE pid=? FOR UPDATE",
        )
        .bind(pid)
        .fetch_one(db.observe(&mut *tx))
        .await?;
        if post.try_get::<i64, _>("tid")? != tid || post.try_get::<i64, _>("fid")? != fid {
            return Err(ContentWriteError::Conflict(
                crate::i18n::current().governance_manage_error_report_relocated(),
            ));
        }
        // Moderators and global moderators recuse themselves from reports on their own content; administrators are the
        // final reviewers and are exempt.
        if role != 1 && post.try_get::<Option<i64>, _>("authorid")? == Some(actor.uid) {
            return Err(ContentWriteError::Forbidden(
                crate::i18n::current().governance_manage_error_report_own(),
            ));
        }
        if crate::identity::forum_role(role)
            && !crate::organization::moderator_locked(db, &mut tx, actor.uid, fid, parent).await?
        {
            return Err(ContentWriteError::Forbidden(
                crate::i18n::current().governance_manage_error_report_forum(),
            ));
        }
    } else if !matches!(role, 1 | 5) {
        return Err(ContentWriteError::Forbidden(
            crate::i18n::current().governance_manage_error_report_profile(),
        ));
    } else if role != 1 && owner == Some(actor.uid) {
        return Err(ContentWriteError::Forbidden(
            crate::i18n::current().governance_manage_error_report_own_profile(),
        ));
    }
    let r = decode(
        crate::db::query(&format!(
            "SELECT r.* FROM gonof_common_report r WHERE r.id=? AND {} FOR UPDATE",
            scope(actor)
        ))
        .bind(id)
        .fetch_optional(db.observe(&mut *tx))
        .await?
        .ok_or(ContentWriteError::NotFound)?,
    )?;
    if r.status != "pending" {
        if r.status == status && r.resolution == text && r.handler_uid == Some(actor.uid) {
            tx.commit().await?;
            return Ok(());
        }
        return Err(ContentWriteError::Conflict(
            crate::i18n::current().governance_manage_error_report_handled(),
        ));
    }
    if now < r.dateline {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().governance_manage_error_report_time(),
        ));
    }
    // Handled only once (guaranteed here, not by the schema): pending was already checked under the lock, and the
    // conditional update blocks once more; the report content (reporter, target, reason, time, request key) appears
    // in no UPDATE (pinned by a source scan).
    let handled = crate::db::query(
        "UPDATE gonof_common_report SET status=?,handler_uid=?,resolution=?,handled_at=? WHERE id=? AND status='pending'",
    )
    .bind(status)
    .bind(actor.uid)
    .bind(text)
    .bind(now)
    .bind(id)
    .execute(db.observe(&mut *tx))
    .await?
    .rows_affected();
    if handled != 1 {
        return Err(ContentWriteError::Conflict(
            crate::i18n::current().governance_manage_error_report_handled(),
        ));
    }
    // The notification keeps only the reviewer's conclusion; "Your report was handled: …" is put together and
    // translated when the list is shown (`describe_notices`).
    notify(
        db,
        &mut tx,
        Notice {
            uid: r.uid,
            authorid: actor.uid,
            kind: crate::notifications::record::Kind::ReportResult,
            target: crate::notifications::record::Target::Report(id),
            key: &format!("report.{id}.result"),
            reason: text,
            now,
        },
    )
    .await?;
    audit(
        db,
        &mut tx,
        actor.uid,
        r.uid,
        "report.resolve",
        serde_json::json!({"report_id":id,"result":status,"reason":text}),
        now,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
