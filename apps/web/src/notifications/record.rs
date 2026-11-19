//! The only write entry point for on-site notification rows: types, column combinations, recipient ownership and the
//! unread count (docs/architecture.md, "Data").
//!
//! The schema has no triggers, so these rules are guaranteed only here: notification rows are written only through
//! [`insert`] and deleted only through [`delete_all`], and the unread count changes only through [`adjust_unread`]
//! (pinned by `db::source_guards`). Marking as read is in `notifications.rs`, which calls [`adjust_unread`] after
//! changing the rows.
//!
//! Adding a new type only changes [`Kind`]: every `match` is exhaustive, so the compiler points out what to fill in.

use crate::db::Tx;
use anyhow::{Context, Result, ensure};

use crate::db::{self, Database};

/// All on-site notification types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Reply,
    Mention,
    Quote,
    FollowThread,
    FollowForum,
    Moderation,
    ReportResult,
    Sanction,
}

/// What the notification points to, one of three: a post, a report or a sanction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Post(i64),
    Report(i64),
    Sanction(i64),
}

impl Kind {
    pub const ALL: [Self; 8] = [
        Self::Reply,
        Self::Mention,
        Self::Quote,
        Self::FollowThread,
        Self::FollowForum,
        Self::Moderation,
        Self::ReportResult,
        Self::Sanction,
    ];

    /// The value stored in `gonof_home_notification.type`.
    pub fn value(self) -> &'static str {
        match self {
            Self::Reply => "reply",
            Self::Mention => "mention",
            Self::Quote => "quote",
            Self::FollowThread => "follow_thread",
            Self::FollowForum => "follow_forum",
            Self::Moderation => "moderation",
            Self::ReportResult => "report_result",
            Self::Sanction => "sanction",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.value() == value)
    }

    /// Moderation kinds: must state a reason, and in the list they are not affected by blocking or forum filters.
    pub fn governance(self) -> bool {
        matches!(self, Self::Moderation | Self::ReportResult | Self::Sanction)
    }

    /// The notification list tab.
    pub fn category(self) -> &'static str {
        match self {
            Self::Reply | Self::Mention | Self::Quote => "replies",
            Self::FollowThread | Self::FollowForum => "following",
            Self::Moderation | Self::ReportResult | Self::Sanction => "system",
        }
    }

    /// The action wording in the notification list.
    pub fn action(self) -> &'static str {
        match self {
            Self::Reply => crate::i18n::current().my_notice_reply(),
            Self::Mention => crate::i18n::current().my_notice_mention(),
            Self::Quote => crate::i18n::current().my_notice_quote(),
            Self::FollowThread => crate::i18n::current().my_notice_follow_thread(),
            Self::FollowForum => crate::i18n::current().my_notice_follow_forum(),
            Self::Moderation => crate::i18n::current().my_notice_moderation(),
            Self::ReportResult => crate::i18n::current().my_notice_report_result(),
            Self::Sanction => crate::i18n::current().my_notice_sanction(),
        }
    }

    /// Combination of type and target: report results point only to reports, sanctions only to sanctions, and all
    /// others to posts.
    fn accepts(self, target: Target) -> bool {
        match self {
            Self::ReportResult => matches!(target, Target::Report(_)),
            Self::Sanction => matches!(target, Target::Sanction(_)),
            Self::Reply
            | Self::Mention
            | Self::Quote
            | Self::FollowThread
            | Self::FollowForum
            | Self::Moderation => matches!(target, Target::Post(_)),
        }
    }
}

impl From<super::preferences::Kind> for Kind {
    fn from(kind: super::preferences::Kind) -> Self {
        match kind {
            super::preferences::Kind::Reply => Self::Reply,
            super::preferences::Kind::Quote => Self::Quote,
            super::preferences::Kind::Mention => Self::Mention,
            super::preferences::Kind::Thread => Self::FollowThread,
            super::preferences::Kind::Forum => Self::FollowForum,
        }
    }
}

/// A notification to be written. `read_at` is always 0 (unread) when written.
#[derive(Clone, Copy, Debug)]
pub struct NewNotice<'a> {
    pub uid: i64,
    pub authorid: i64,
    pub kind: Kind,
    pub target: Target,
    pub event_key: &'a str,
    pub reason: &'a str,
    pub dateline: i64,
}

impl NewNotice<'_> {
    /// Rules that look only at this row's values without querying the database; recipient ownership is checked by
    /// primary key in [`insert`].
    pub fn validate(&self) -> std::result::Result<(), &'static str> {
        if self.uid <= 0 || self.authorid <= 0 {
            return Err("notification recipient or author is invalid");
        }
        if !self.kind.accepts(self.target) {
            return Err("notification type does not match its target");
        }
        let (Target::Post(id) | Target::Report(id) | Target::Sanction(id)) = self.target;
        if id <= 0 {
            return Err("notification target is invalid");
        }
        if self.kind.governance() && self.reason.trim().is_empty() {
            return Err("governance notifications need a reason");
        }
        let length = self.event_key.chars().count();
        if !(1..=100).contains(&length) || self.event_key.chars().all(char::is_whitespace) {
            return Err("notification event keys must be 1–100 characters and not all whitespace");
        }
        if self.dateline < 0 {
            return Err("notification time is invalid");
        }
        Ok(())
    }
}

/// Writes one unread notification, increments the recipient's unread count by one, and returns the notification ID.
///
/// The caller has already locked the recipient's member row with `FOR UPDATE` in this transaction, so the member row
/// is always locked before the notification row is written. A duplicate write of the same `(uid, event_key)` fails on
/// the unique key and the whole transaction rolls back, so the count is never over-counted.
pub(crate) async fn insert(db: &Database, tx: &mut Tx<'_>, notice: &NewNotice<'_>) -> Result<i64> {
    notice.validate().map_err(anyhow::Error::msg)?;
    let (pid, report_id, sanction_id) = match notice.target {
        Target::Post(id) => (Some(id), None, None),
        Target::Report(id) => (None, Some(id), None),
        Target::Sanction(id) => (None, None, Some(id)),
    };
    // Report results can only go to the reporter, and sanction notifications only to the sanctioned member.
    let owner_sql = match notice.target {
        Target::Post(_) => None,
        Target::Report(_) => Some("SELECT uid FROM gonof_common_report WHERE id=?"),
        Target::Sanction(_) => Some("SELECT uid FROM gonof_common_member_sanction WHERE id=?"),
    };
    if let Some(sql) = owner_sql {
        let owner: i64 = db::query_scalar(sql)
            .bind(report_id.or(sanction_id))
            .fetch_optional(db.observe(&mut **tx))
            .await?
            .context("the report or sanction of the notification does not exist")?;
        ensure!(
            owner == notice.uid,
            "{}",
            crate::i18n::current().my_error_notification_owner()
        );
    }
    let result = db::query("INSERT INTO gonof_home_notification(uid,authorid,pid,report_id,sanction_id,type,event_key,reason,dateline) VALUES(?,?,?,?,?,?,?,?,?)")
        .bind(notice.uid)
        .bind(notice.authorid)
        .bind(pid)
        .bind(report_id)
        .bind(sanction_id)
        .bind(notice.kind.value())
        .bind(notice.event_key)
        .bind(notice.reason)
        .bind(notice.dateline)
        .execute(db.observe(&mut **tx))
        .await?;
    adjust_unread(db, tx, notice.uid, 1).await?;
    Ok(i64::try_from(result.last_insert_id())?)
}

/// Deletes all of this member's notifications, resetting the unread count to zero (used for account deletion). The
/// caller has already locked the member row.
pub(crate) async fn delete_all(db: &Database, tx: &mut Tx<'_>, uid: i64) -> Result<()> {
    db::query("DELETE FROM gonof_home_notification WHERE uid=?")
        .bind(uid)
        .execute(db.observe(&mut **tx))
        .await?;
    db::query("UPDATE gonof_common_member SET unread_notifications=0 WHERE uid=?")
        .bind(uid)
        .execute(db.observe(&mut **tx))
        .await?;
    Ok(())
}

/// Changes the member's unread notification count by the number of rows that actually became unread or read. The
/// caller has already locked the member row in this transaction.
///
/// Decreases use a conditional update: a count too small to subtract from means the count and the details are already
/// inconsistent, so it fails and rolls back, never writing a negative number.
/// Retention cleanup (`maintenance.rs`): deletes the read notifications among `ids` that were read before `cutoff`,
/// checking that once more; returns how many went.
pub(crate) async fn delete_expired_read(db: &Database, ids: &[i64], cutoff: i64) -> Result<u64> {
    if ids.is_empty() {
        return Ok(0);
    }
    let sql = format!(
        "DELETE FROM gonof_home_notification WHERE id IN ({}) AND read_at>0 AND read_at<?",
        vec!["?"; ids.len()].join(",")
    );
    let mut delete = db::query(&sql);
    for id in ids {
        delete = delete.bind(*id);
    }
    Ok(delete
        .bind(cutoff)
        .execute(db.executor())
        .await?
        .rows_affected())
}

/// Retention cleanup: deletes member `uid`'s notifications among `ids` that are still unread and arrived before
/// `cutoff`, and takes them off the member's unread count, under the member row lock that marking read and new
/// notifications take. Returns how many went.
pub(crate) async fn delete_expired_unread(
    db: &Database,
    uid: i64,
    ids: &[i64],
    cutoff: i64,
) -> Result<u64> {
    if ids.is_empty() {
        return Ok(0);
    }
    let mut tx = db.begin_write().await?;
    db::query("SELECT uid FROM gonof_common_member WHERE uid=? FOR UPDATE")
        .bind(uid)
        .fetch_one(db.observe(&mut *tx))
        .await?;
    let sql = format!(
        "DELETE FROM gonof_home_notification WHERE id IN ({}) AND uid=? AND read_at=0 AND dateline<?",
        vec!["?"; ids.len()].join(",")
    );
    let mut delete = db::query(&sql);
    for id in ids {
        delete = delete.bind(*id);
    }
    let removed = delete
        .bind(uid)
        .bind(cutoff)
        .execute(db.observe(&mut *tx))
        .await?
        .rows_affected();
    adjust_unread(db, &mut tx, uid, -i64::try_from(removed)?).await?;
    tx.commit().await?;
    Ok(removed)
}

pub(crate) async fn adjust_unread(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: i64,
    delta: i64,
) -> Result<()> {
    if delta == 0 {
        return Ok(());
    }
    let result = if delta > 0 {
        db::query(
            "UPDATE gonof_common_member SET unread_notifications=unread_notifications+? WHERE uid=?",
        )
        .bind(delta)
        .bind(uid)
        .execute(db.observe(&mut **tx))
        .await?
    } else {
        let amount = delta
            .checked_neg()
            .context("unread notification change out of range")?;
        db::query("UPDATE gonof_common_member SET unread_notifications=unread_notifications-? WHERE uid=? AND unread_notifications>=?")
            .bind(amount)
            .bind(uid)
            .bind(amount)
            .execute(db.observe(&mut **tx))
            .await?
    };
    ensure!(
        result.rows_affected() == 1,
        "member {uid}'s unread notification count does not match the notifications"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notice(kind: Kind, target: Target) -> NewNotice<'static> {
        NewNotice {
            uid: 2,
            authorid: 1,
            kind,
            target,
            event_key: "event:1",
            reason: "Reason",
            dateline: 1,
        }
    }

    #[test]
    fn every_kind_round_trips_and_has_one_category() {
        for kind in Kind::ALL {
            assert_eq!(Kind::parse(kind.value()), Some(kind));
            assert!(["replies", "following", "system"].contains(&kind.category()));
            assert_eq!(kind.governance(), kind.category() == "system");
        }
        assert_eq!(Kind::parse("invalid"), None);
        assert_eq!(
            Kind::parse("reply "),
            None,
            "values compare exactly, trailing spaces included"
        );
        for kind in super::super::preferences::Kind::ALL {
            assert_eq!(Kind::from(kind).value(), kind.value());
        }
    }

    #[test]
    fn kinds_only_accept_their_own_target() {
        for kind in Kind::ALL {
            let expected = match kind {
                Kind::ReportResult => Target::Report(1),
                Kind::Sanction => Target::Sanction(1),
                _ => Target::Post(1),
            };
            for target in [Target::Post(1), Target::Report(1), Target::Sanction(1)] {
                assert_eq!(
                    notice(kind, target).validate().is_ok(),
                    target == expected,
                    "{kind:?} → {target:?}"
                );
            }
        }
    }

    #[test]
    fn rows_keep_the_value_rules_the_triggers_had() {
        let ok = notice(Kind::Reply, Target::Post(1));
        assert_eq!(ok.validate(), Ok(()));
        let content_without_reason = NewNotice { reason: "", ..ok };
        assert_eq!(content_without_reason.validate(), Ok(()));
        for broken in [
            NewNotice { uid: 0, ..ok },
            NewNotice { authorid: 0, ..ok },
            NewNotice {
                target: Target::Post(0),
                ..ok
            },
            NewNotice {
                kind: Kind::Moderation,
                reason: " \t",
                ..ok
            },
            NewNotice {
                kind: Kind::Sanction,
                target: Target::Sanction(1),
                reason: "",
                ..ok
            },
            NewNotice {
                event_key: "",
                ..ok
            },
            NewNotice {
                event_key: " \t\n\r\u{b}\u{c}",
                ..ok
            },
            NewNotice {
                event_key: &"k".repeat(101),
                ..ok
            },
            NewNotice { dateline: -1, ..ok },
        ] {
            assert!(broken.validate().is_err(), "must be rejected: {broken:?}");
        }
        // "\u{952e}" (the Chinese for "key") takes three bytes in UTF-8: the length counts characters.
        assert_eq!(
            NewNotice {
                event_key: &"\u{952e}".repeat(100),
                ..ok
            }
            .validate(),
            Ok(()),
            "the length counts characters"
        );
    }
}
