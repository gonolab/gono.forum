//! Reports, account sanctions and in-site governance notifications share primary-database transactions; permissions
//! and notification recipients are decided by the server.
use crate::db::Tx;
use crate::{
    content::writes::ContentWriteError,
    db::{self, Database},
    identity::{self, Actor},
};
use std::collections::BTreeSet;

mod appeals;
mod reports;
mod restorations;
mod sanctions;
pub use appeals::*;
pub use reports::*;
pub use restorations::*;
pub use sanctions::*;
type Result<T> = std::result::Result<T, ContentWriteError>;

pub(crate) fn valid_key(key: &str) -> bool {
    crate::identity::valid_request_key(key)
}
fn reason(value: &str, max: usize) -> Result<&str> {
    let value = value.trim();
    if value.is_empty()
        || value.chars().count() > max
        || value
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\r' && c != '\t')
    {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().governance_error_reason(),
        ));
    }
    Ok(value)
}
pub fn display_time(value: i64) -> String {
    crate::clock::datetime(value, &crate::clock::current())
}

async fn lock_accounts(
    db: &Database,
    tx: &mut Tx<'_>,
    actor: Actor,
    other: Option<i64>,
) -> Result<i64> {
    let mut ids = BTreeSet::from([actor.uid]);
    ids.extend(other);
    let mut role = 0;
    for uid in ids {
        let row=crate::db::query("SELECT adminid,emailstatus,mobile,`freeze`,credentials_version,account_state FROM gonof_common_member WHERE uid=? FOR UPDATE")
            .bind(uid).fetch_optional(db.observe(&mut **tx)).await?.ok_or(ContentWriteError::NotFound)?;
        if uid == actor.uid {
            if row.try_get::<i64, _>("credentials_version")? != actor.credentials_version
                || row.try_get::<i64, _>("freeze")? != 0
                || db::text(&row, "account_state")? != "active"
            {
                return Err(ContentWriteError::Unauthorized);
            }
            role = row.try_get("adminid")?;
            let participation = crate::login::Participation::current();
            if role != 1
                && !crate::login::permits(
                    db.observe(&mut **tx),
                    uid,
                    row.try_get::<i64, _>("emailstatus")? == 1,
                    row.try_get::<Option<String>, _>("mobile")?.is_some(),
                )
                .await?
            {
                return Err(ContentWriteError::Forbidden(
                    participation.message(crate::i18n::current().governance_error_verify_email()),
                ));
            }
        }
    }
    Ok(role)
}
async fn active(db: &Database, actor: Actor) -> Result<()> {
    if !identity::valid_actor(db, actor).await? {
        return Err(ContentWriteError::Unauthorized);
    }
    Ok(())
}
async fn administrator(db: &Database, actor: Actor) -> Result<()> {
    let member = crate::db::query(
        "SELECT credentials_version,`freeze`,adminid,account_state FROM gonof_common_member WHERE uid=?",
    )
    .bind(actor.uid)
    .fetch_optional(db.executor())
    .await?;
    let authorized = if let Some(member) = member {
        member.try_get::<i64, _>("credentials_version")? == actor.credentials_version
            && member.try_get::<i64, _>("freeze")? == 0
            && member.try_get::<i64, _>("adminid")? == 1
            && db::text(&member, "account_state")? == "active"
    } else {
        false
    };
    if !authorized {
        return Err(ContentWriteError::Forbidden(
            crate::i18n::current().governance_manage_error_sanction_admin(),
        ));
    }
    Ok(())
}
/// Viewers of the sanctions page: administrators and global moderators (global moderators can only mute and lift
/// mutes; the write entry points check that separately).
async fn sanction_staff(db: &Database, actor: Actor) -> Result<()> {
    let member = crate::db::query(
        "SELECT credentials_version,`freeze`,adminid,account_state FROM gonof_common_member WHERE uid=?",
    )
    .bind(actor.uid)
    .fetch_optional(db.executor())
    .await?;
    let authorized = if let Some(member) = member {
        member.try_get::<i64, _>("credentials_version")? == actor.credentials_version
            && member.try_get::<i64, _>("freeze")? == 0
            && matches!(member.try_get::<i64, _>("adminid")?, 1 | 2)
            && db::text(&member, "account_state")? == "active"
    } else {
        false
    };
    if !authorized {
        return Err(ContentWriteError::Forbidden(
            crate::i18n::current().governance_manage_error_sanction_staff(),
        ));
    }
    Ok(())
}

/// The wording of governance notifications on one page of the notification list, in this request's language (the
/// database keeps only the reason a person wrote). What happened is put in front here, by
/// event key and the rows it points to:
///
/// - sanctions, appeals and their expiry: [`sanctions::describe_sanctions`];
/// - `report.{id}.result`: "Your report was handled" or "Your report was not upheld" by the report's status;
/// - `restore.{id}.result`: "Your restoration request was accepted" or "… was not accepted" by the request's status;
/// - `moderation.{actor}.{key}.{code}`: the moderator's action by the code at the end of the key
///   (`content::edits::moderation_label`), without a query.
///
/// At most one bounded query per kind present on the page (reports and restoration requests by their ids). Rows whose
/// moderation key ends without a code (the stored text is a whole sentence) and rows whose target is missing keep the
/// stored text.
pub(crate) async fn describe_notices(
    db: &Database,
    uid: i64,
    items: &mut [crate::notifications::Notification],
) -> anyhow::Result<()> {
    sanctions::describe_sanctions(db, uid, items).await?;
    let t = crate::i18n::current();
    let reports: BTreeSet<i64> = items
        .iter()
        .filter(|n| n.kind == "report_result" && n.event_key.starts_with("report."))
        .filter_map(|n| n.report_id)
        .collect();
    let report_status = statuses(db, "gonof_common_report", uid, &reports).await?;
    let restorations: BTreeSet<i64> = items
        .iter()
        .filter(|n| n.kind == "moderation")
        .filter_map(|n| restoration_id(&n.event_key))
        .collect();
    let restoration_status =
        statuses(db, "gonof_forum_restore_request", uid, &restorations).await?;
    for notice in items.iter_mut() {
        let reason = notice.reason.as_str();
        let text = if notice.kind == "report_result" {
            match notice
                .report_id
                .and_then(|id| report_status.get(&id))
                .map(String::as_str)
            {
                Some("resolved") => t.governance_notice_report_resolved(reason),
                Some(_) => t.governance_notice_report_dismissed(reason),
                None => continue,
            }
        } else if notice.kind == "moderation" {
            if let Some(id) = restoration_id(&notice.event_key) {
                // Written only when the request was handled: accepted, or rejected otherwise.
                match restoration_status.get(&id).map(String::as_str) {
                    Some("accepted") => t.governance_notice_restoration_accepted(reason),
                    Some(_) => t.governance_notice_restoration_rejected(reason),
                    None => continue,
                }
            } else {
                let Some(label) = moderation_code(&notice.event_key)
                    .and_then(crate::content::edits::moderation_label)
                else {
                    continue;
                };
                t.editor_moderation_notice(label, reason)
            }
        } else {
            continue;
        };
        notice.reason = text;
    }
    Ok(())
}

/// The id in `restore.{id}.result`.
fn restoration_id(key: &str) -> Option<i64> {
    key.strip_prefix("restore.")?
        .strip_suffix(".result")?
        .parse()
        .ok()
}

/// The action code at the end of `moderation.{actor}.{key}.{code}`; keys written before the code was added have three
/// parts and give `None`.
fn moderation_code(key: &str) -> Option<&str> {
    let mut parts = key.strip_prefix("moderation.")?.splitn(3, '.');
    let (_actor, _key, code) = (parts.next()?, parts.next()?, parts.next()?);
    Some(code)
}

/// The `status` of the member's own rows among `ids` in `table` (reports or restoration requests, both owned through
/// `uid`): one bounded query, none when `ids` is empty.
async fn statuses(
    db: &Database,
    table: &str,
    uid: i64,
    ids: &BTreeSet<i64>,
) -> anyhow::Result<std::collections::HashMap<i64, String>> {
    let mut found = std::collections::HashMap::new();
    if ids.is_empty() {
        return Ok(found);
    }
    let sql = format!(
        "SELECT id,status FROM {table} WHERE uid=? AND id IN ({})",
        vec!["?"; ids.len()].join(",")
    );
    let mut query = crate::db::query(&sql).bind(uid);
    for id in ids {
        query = query.bind(*id);
    }
    for row in query.fetch_all(db.executor()).await? {
        found.insert(row.try_get::<i64, _>("id")?, db::text(&row, "status")?);
    }
    Ok(found)
}

pub(crate) struct Notice<'a> {
    pub uid: i64,
    pub authorid: i64,
    pub kind: crate::notifications::record::Kind,
    pub target: crate::notifications::record::Target,
    pub key: &'a str,
    pub reason: &'a str,
    pub now: i64,
}
/// Governance results and notifications commit in the same database transaction; replays are constrained jointly by
/// the business request and the event unique key.
///
/// The pairing of type and target object, a non-empty reason, recipient ownership and unread counts are all
/// guaranteed by `notifications::record::insert`; `active_member` locks the recipient member row first.
pub(crate) async fn notify(db: &Database, tx: &mut Tx<'_>, n: Notice<'_>) -> Result<()> {
    if !crate::privacy::active_member(db, tx, n.uid).await? {
        return Ok(());
    }
    crate::notifications::record::insert(
        db,
        tx,
        &crate::notifications::record::NewNotice {
            uid: n.uid,
            authorid: n.authorid,
            kind: n.kind,
            target: n.target,
            event_key: n.key,
            reason: n.reason,
            dateline: n.now,
        },
    )
    .await?;
    Ok(())
}
async fn audit(
    db: &Database,
    tx: &mut Tx<'_>,
    actor: i64,
    uid: i64,
    kind: &str,
    data: serde_json::Value,
    now: i64,
) -> Result<()> {
    let [pid, fid, to_fid] = crate::manage::log_targets(kind, &data);
    crate::db::query("INSERT INTO gonof_common_log(uid,operationuid,type,data,source,dateline,pid,fid,to_fid) VALUES(?,?,?,?, 'web', ?,?,?,?)")
        .bind(uid).bind(actor).bind(kind).bind(data.to_string()).bind(now)
        .bind(pid).bind(fid).bind(to_fid)
        .execute(db.observe(&mut **tx)).await?;
    Ok(())
}
