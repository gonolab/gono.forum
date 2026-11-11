//! Per-channel content preferences; account and governance messages remain mandatory.
use super::*;
use crate::db::Tx;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Reply,
    Quote,
    Mention,
    Thread,
    Forum,
}
impl Kind {
    pub const ALL: [Self; 5] = [
        Self::Reply,
        Self::Quote,
        Self::Mention,
        Self::Thread,
        Self::Forum,
    ];
    pub fn value(self) -> &'static str {
        match self {
            Self::Reply => "reply",
            Self::Quote => "quote",
            Self::Mention => "mention",
            Self::Thread => "follow_thread",
            Self::Forum => "follow_forum",
        }
    }
    pub fn field(self) -> &'static str {
        match self {
            Self::Thread => "thread",
            Self::Forum => "forum",
            other => other.value(),
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Reply => crate::i18n::current().social_preference_reply(),
            Self::Quote => crate::i18n::current().social_preference_quote(),
            Self::Mention => crate::i18n::current().social_preference_mention(),
            Self::Thread => crate::i18n::current().social_preference_thread(),
            Self::Forum => crate::i18n::current().social_preference_forum(),
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.value() == value)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Preferences {
    pub site: [bool; 5],
    pub email: [bool; 5],
    /// Private message mail alerts, off by default. Mail channel only: on-site there is the
    /// conversation unread count.
    pub email_message: bool,
    /// New-device sign-in alerts, on by default.
    pub email_login: bool,
    pub version: i64,
}
impl Default for Preferences {
    /// The switches match the column defaults of `gonof_home_notification_preference`: members without a preference
    /// row take these values.
    fn default() -> Self {
        Self {
            site: [true; 5],
            email: [false; 5],
            email_message: false,
            email_login: true,
            version: 0,
        }
    }
}
impl Preferences {
    pub fn highest(&self, kinds: &BTreeSet<Kind>, email: bool) -> Option<Kind> {
        let enabled = if email { &self.email } else { &self.site };
        Kind::ALL
            .into_iter()
            .enumerate()
            .find_map(|(index, kind)| (enabled[index] && kinds.contains(&kind)).then_some(kind))
    }
    pub fn enabled(&self, kind: Kind, email: bool) -> bool {
        self.highest(&BTreeSet::from([kind]), email).is_some()
    }
}
fn decode(row: Option<db::DbRow>) -> anyhow::Result<Preferences> {
    let Some(row) = row else {
        return Ok(Preferences::default());
    };
    let mut value = Preferences {
        version: row.try_get("version")?,
        ..Default::default()
    };
    for (index, kind) in Kind::ALL.into_iter().enumerate() {
        value.site[index] = row.try_get::<i64, _>(format!("site_{}", kind.field()).as_str())? == 1;
        value.email[index] =
            row.try_get::<i64, _>(format!("email_{}", kind.field()).as_str())? == 1;
    }
    value.email_message = row.try_get::<i64, _>("email_message")? == 1;
    value.email_login = row.try_get::<i64, _>("email_login")? == 1;
    Ok(value)
}
const SELECT: &str = "SELECT site_reply,site_quote,site_mention,site_thread,site_forum,email_reply,email_quote,email_mention,email_thread,email_forum,email_message,email_login,version FROM gonof_home_notification_preference WHERE uid=?";
/// Caller holds the recipient account row, as all preference writes do.
pub(crate) async fn locked(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: i64,
) -> anyhow::Result<Preferences> {
    decode(
        crate::db::query(SELECT)
            .bind(uid)
            .fetch_optional(db.observe(&mut **tx))
            .await?,
    )
}
pub async fn get(db: &Database, actor: Actor) -> Result<Preferences> {
    if !identity::valid_actor(db, actor).await? {
        return Err(ContentWriteError::Unauthorized);
    }
    Ok(decode(
        crate::db::query(SELECT)
            .bind(actor.uid)
            .fetch_optional(db.executor())
            .await?,
    )?)
}
pub async fn save(db: &Database, actor: Actor, input: &Preferences) -> Result<()> {
    if input.version < 0 {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().social_error_preferences_version(),
        ));
    }
    let mut tx = db.begin_write().await?;
    interaction::lock_actor(db, &mut tx, actor, false).await?;
    let current = locked(db, &mut tx, actor.uid).await?;
    if current.version != input.version {
        return Err(ContentWriteError::Conflict(
            crate::i18n::current().social_error_preferences_changed(),
        ));
    }
    if current.site == input.site
        && current.email == input.email
        && current.email_message == input.email_message
        && current.email_login == input.email_login
    {
        tx.commit().await?;
        return Ok(());
    }
    let version = input
        .version
        .checked_add(1)
        .ok_or(ContentWriteError::Validation(
            crate::i18n::current().social_error_preferences_range(),
        ))?;
    // The value rules are guaranteed here, not by the schema: switches only come from bool, and the version comes from
    // checked_add and is at least 1; preference rows are only written here (pinned by db::source_guards).
    let mut query=crate::db::query("INSERT INTO gonof_home_notification_preference(uid,site_reply,site_quote,site_mention,site_thread,site_forum,email_reply,email_quote,email_mention,email_thread,email_forum,email_message,email_login,version,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?) ON DUPLICATE KEY UPDATE site_reply=VALUES(site_reply),site_quote=VALUES(site_quote),site_mention=VALUES(site_mention),site_thread=VALUES(site_thread),site_forum=VALUES(site_forum),email_reply=VALUES(email_reply),email_quote=VALUES(email_quote),email_mention=VALUES(email_mention),email_thread=VALUES(email_thread),email_forum=VALUES(email_forum),email_message=VALUES(email_message),email_login=VALUES(email_login),version=VALUES(version),updated_at=VALUES(updated_at)").bind(actor.uid);
    for value in input
        .site
        .into_iter()
        .chain(input.email)
        .chain([input.email_message, input.email_login])
    {
        query = query.bind(i64::from(value));
    }
    query
        .bind(version)
        .bind(identity::now())
        .execute(db.observe(&mut *tx))
        .await?;
    crate::db::query("INSERT INTO gonof_common_log(uid,operationuid,type,data,source,dateline) VALUES(?,?,'identity.notification_preferences',?,'web',?)")
        .bind(actor.uid).bind(actor.uid).bind(serde_json::json!({"version":version}).to_string()).bind(identity::now()).execute(db.observe(&mut *tx)).await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn channel_priority_is_chosen_after_disabled_kinds_are_removed() {
        let mut settings = Preferences::default();
        let kinds = BTreeSet::from(Kind::ALL);
        assert_eq!(settings.highest(&kinds, false), Some(Kind::Reply));
        assert_eq!(settings.highest(&kinds, true), None);
        settings.site[0] = false;
        settings.email[2] = true;
        assert_eq!(settings.highest(&kinds, false), Some(Kind::Quote));
        assert_eq!(settings.highest(&kinds, true), Some(Kind::Mention));
        settings.site = [false; 5];
        assert_eq!(settings.highest(&kinds, false), None);
    }
}
