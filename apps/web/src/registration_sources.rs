//! Administrators count registrations separately by the site, promotion channel, inviter and fixed prefix in the
//! registration log.
use crate::db::Tx;
use crate::{
    administration::{self, PAGE_SIZE},
    db::{self, Database},
    identity::{Actor, IdentityError},
};

#[derive(Clone, Debug, Default, serde::Serialize)]
pub(crate) struct RegistrationSource {
    site: String,
    channel: String,
}

impl RegistrationSource {
    pub(crate) fn from_request(origin: Option<&str>, channel: Option<&str>) -> Self {
        Self {
            site: crate::site::registration_address(origin),
            channel: channel
                .and_then(valid_channel)
                .unwrap_or_default()
                .to_owned(),
        }
    }
}

/// Writes the member's own registration source (`gonof_common_member_register`) in the registration transaction: the
/// admin statistics group by these columns instead of reading the registration log. Same rules as the log: the
/// prefix only records a non-empty prefix that actually let the registration through; the site address, taken from the
/// request's Origin, can be long and is truncated beyond 500 characters.
pub(crate) async fn record(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: i64,
    source: &RegistrationSource,
    inviter: Option<i64>,
    prefix: Option<&str>,
) -> Result<(), sqlx::Error> {
    let site: String = source.site.chars().take(500).collect();
    db::query("INSERT INTO gonof_common_member_register(uid,site,channel,inviter_uid,invite_prefix) VALUES(?,?,?,?,?)")
        .bind(uid)
        .bind(site)
        .bind(&source.channel)
        .bind(inviter)
        .bind(prefix.filter(|prefix| !prefix.is_empty()))
        .execute(db.observe(&mut **tx))
        .await?;
    Ok(())
}

pub(crate) fn valid_channel(value: &str) -> Option<&str> {
    let value = value.trim_matches(' ');
    ((1..=64).contains(&value.chars().count())
        && value
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-')))
    .then_some(value)
}

pub(crate) struct SourceCount {
    pub label: String,
    pub count: i64,
}

pub(crate) struct Report {
    pub total: i64,
    pub sites: Vec<SourceCount>,
    pub channels: Vec<SourceCount>,
    pub inviters: Vec<SourceCount>,
    pub prefixes: Vec<SourceCount>,
    pub more_sites: bool,
    pub more_channels: bool,
    pub more_inviters: bool,
    pub more_prefixes: bool,
}

pub(crate) async fn counts(
    db: &Database,
    actor: Actor,
    site_page: i64,
    channel_page: i64,
    inviter_page: i64,
    prefix_page: i64,
) -> Result<Report, IdentityError> {
    administration::admin(db, actor).await?;
    if [site_page, channel_page, inviter_page, prefix_page]
        .iter()
        .any(|page| !(1..=i64::MAX / PAGE_SIZE).contains(page))
    {
        return Err(IdentityError::Validation(
            crate::i18n::current().account_manage_error_sources_page(),
        ));
    }
    // The total and the four summaries see the same registrations and account deletions in one snapshot.
    let mut tx = db.begin_read_only().await?;
    let total = db::query_scalar("SELECT COUNT(*) FROM gonof_common_member_register")
        .fetch_one(db.observe(&mut *tx))
        .await?;
    let t = crate::i18n::current();
    let (sites, more_sites) = groups(
        db,
        &mut tx,
        Column::Site,
        t.account_manage_sources_unlisted(),
        site_page,
    )
    .await?;
    let (channels, more_channels) = groups(
        db,
        &mut tx,
        Column::Channel,
        t.account_manage_sources_no_channel(),
        channel_page,
    )
    .await?;
    let (inviters, more_inviters) = inviters(db, &mut tx, inviter_page).await?;
    let (prefixes, more_prefixes) = prefixes(db, &mut tx, prefix_page).await?;
    tx.commit().await?;
    Ok(Report {
        total,
        sites,
        channels,
        inviters,
        prefixes,
        more_sites,
        more_channels,
        more_inviters,
        more_prefixes,
    })
}

/// Which column to group by: there are only these two values, so the column name is written into the statement.
#[derive(Clone, Copy)]
enum Column {
    Site,
    Channel,
}

async fn groups(
    db: &Database,
    tx: &mut Tx<'_>,
    column: Column,
    empty_label: &str,
    page: i64,
) -> Result<(Vec<SourceCount>, bool), IdentityError> {
    // Rows written by `record` always hold a string (empty when the registration had no mark); NULL means the source
    // was not recorded and gets its own label.
    let sql = match column {
        Column::Site => {
            "SELECT site AS source_label, COUNT(*) AS registrations FROM gonof_common_member_register
             GROUP BY site ORDER BY registrations DESC, source_label ASC LIMIT ? OFFSET ?"
        }
        Column::Channel => {
            "SELECT channel AS source_label, COUNT(*) AS registrations FROM gonof_common_member_register
             GROUP BY channel ORDER BY registrations DESC, source_label ASC LIMIT ? OFFSET ?"
        }
    };
    let t = crate::i18n::current();
    let not_recorded = t.account_manage_sources_not_recorded();
    count_page(db, tx, db::query(sql), page, |row| {
        let label: Option<String> = row.try_get("source_label")?;
        Ok(match label.as_deref() {
            None => not_recorded.to_owned(),
            Some("") => empty_label.to_owned(),
            // A real channel or site that happens to read like one of the labels above.
            Some(name) if name == empty_label || name == not_recorded => {
                t.account_manage_sources_marked(name)
            }
            Some(name) => name.to_owned(),
        })
    })
    .await
}

async fn inviters(
    db: &Database,
    tx: &mut Tx<'_>,
    page: i64,
) -> Result<(Vec<SourceCount>, bool), IdentityError> {
    // Grouped by the inviter UID at registration time; the member table only supplies the current display name, so
    // renames and account deletions do not split the historical counts.
    let query = db::query(
        "SELECT r.inviter_uid, COUNT(*) AS registrations, m.username, m.account_state
         FROM gonof_common_member_register r JOIN gonof_common_member m ON m.uid=r.inviter_uid
         WHERE r.inviter_uid>0 GROUP BY r.inviter_uid, m.username, m.account_state
         ORDER BY registrations DESC, r.inviter_uid ASC LIMIT ? OFFSET ?",
    );
    let t = crate::i18n::current();
    count_page(db, tx, query, page, |row| {
        let uid: i64 = row.try_get("inviter_uid")?;
        let state: Option<String> = row.try_get("account_state")?;
        let username: Option<String> = row.try_get("username")?;
        Ok(match (state.as_deref(), username) {
            (Some("deleted"), _) => {
                t.account_manage_sources_inviter(t.common_deleted_user(), uid.to_string())
            }
            (_, Some(name)) => t.account_manage_sources_inviter(name, uid.to_string()),
            _ => format!("UID {uid}"),
        })
    })
    .await
}

async fn prefixes(
    db: &Database,
    tx: &mut Tx<'_>,
    page: i64,
) -> Result<(Vec<SourceCount>, bool), IdentityError> {
    // Only records a non-empty prefix that actually let the registration through at the time (both writing and
    // backfilling follow this rule).
    let query = db::query(
        "SELECT invite_prefix AS prefix, COUNT(*) AS registrations FROM gonof_common_member_register
         WHERE invite_prefix<>'' GROUP BY invite_prefix ORDER BY registrations DESC, prefix ASC LIMIT ? OFFSET ?",
    );
    count_page(db, tx, query, page, |row| Ok(row.try_get("prefix")?)).await
}

async fn count_page(
    db: &Database,
    tx: &mut Tx<'_>,
    query: db::Query<'_>,
    page: i64,
    label: impl Fn(&db::DbRow) -> Result<String, IdentityError>,
) -> Result<(Vec<SourceCount>, bool), IdentityError> {
    let rows = query
        .bind(PAGE_SIZE + 1)
        .bind((page - 1) * PAGE_SIZE)
        .fetch_all(db.observe(&mut **tx))
        .await?;
    if page > 1 && rows.is_empty() {
        return Err(IdentityError::NotFound);
    }
    let more = rows.len() > PAGE_SIZE as usize;
    let mut counts = Vec::new();
    for row in rows.iter().take(PAGE_SIZE as usize) {
        counts.push(SourceCount {
            label: label(row)?,
            count: row.try_get("registrations")?,
        });
    }
    Ok((counts, more))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_names_preserve_case_and_count_unicode_characters() {
        // Channel names may use any letters and count characters, not bytes: "\u{5fae}\u{4fe1}" is "WeChat" in
        // Chinese, "\u{8393}" ("berry") a three-byte character, "\u{7ad9}\u{70b9}" means "site".
        assert_eq!(
            valid_channel("  \u{5fae}\u{4fe1}.A_1-x  "),
            Some("\u{5fae}\u{4fe1}.A_1-x")
        );
        let longest = "\u{8393}".repeat(64);
        assert_eq!(valid_channel(&longest), Some(longest.as_str()));
        for invalid in [
            "",
            " ",
            "a b",
            "a/b",
            "https://\u{7ad9}\u{70b9}",
            "a\n",
            &"\u{8393}".repeat(65),
        ] {
            assert_eq!(valid_channel(invalid), None);
        }
    }
}
