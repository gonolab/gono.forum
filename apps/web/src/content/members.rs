//! Member directory: member search and the home page's newest members. Visibility in both follows the personal space
//! gate: anonymous visitors only see "Everyone", logged-in members additionally see "Logged-in members", and "Only me"
//! and deleted accounts never appear; logged-in members also exclude people they have blocked (search filters out
//! blocked members).
use super::*;

/// Page size of member search.
pub const SEARCH_PAGE_SIZE: usize = 20;
/// Page limit of member search. The shorter the prefix, the more hits, and the deeper the OFFSET, the more index
/// entries are read; 50 pages is at most 1000 rows, so a single query reads at most 1001 hits.
pub const SEARCH_MAX_PAGE: usize = 50;
// The number of newest members on the home page is the admin setting `settings.display.recent_members` (1–20,
// docs/settings-and-caching.md, "The Redis cache"); the home page handler passes it in and it is encoded into the
// cache key.

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ListedMember {
    pub uid: i64,
    pub username: String,
    pub avatar: i64,
    pub avatar_aid: Option<i64>,
    pub regdate: i64,
    /// Only member search fetches the bio; the home page's newest members do not show it and leave it empty.
    #[serde(default)]
    pub bio: String,
}

pub struct MemberSearchPage {
    pub members: Vec<ListedMember>,
    pub has_next: bool,
}

/// The two filters for personal space visibility and blocking; `m` is the member table alias. Account status is added
/// by the caller: member search, like the personal space gate, only excludes deleted accounts, while the home page's
/// newest members also exclude frozen accounts and those being deleted (`deleting`).
pub(crate) fn listed_scope_sql(viewer_uid: Option<i64>) -> String {
    let scope = if viewer_uid.is_some() {
        "m.space_visibility IN ('public','members')"
    } else {
        "m.space_visibility='public'"
    };
    format!("{scope} AND {}", unblocked_sql(viewer_uid, "m.uid"))
}

/// LIKE pattern for prefix matching: `!` is the escape character, and `%`, `_` and `!` itself match literally.
pub fn prefix_pattern(prefix: &str) -> String {
    let mut pattern = String::with_capacity(prefix.len() + 1);
    for ch in prefix.chars() {
        if matches!(ch, '!' | '%' | '_') {
            pattern.push('!');
        }
        pattern.push(ch);
    }
    pattern.push('%');
    pattern
}

pub(crate) fn decode_listed(row: &DbRow, with_bio: bool) -> Result<ListedMember> {
    Ok(ListedMember {
        uid: row.try_get("uid")?,
        username: decode_text(row, "username")?,
        avatar: row.try_get("avatar")?,
        avatar_aid: row.try_get("avatar_aid")?,
        regdate: row.try_get("regdate")?,
        bio: if with_bio {
            decode_text(row, "bio")?
        } else {
            String::new()
        },
    })
}

/// Searches members by username prefix, paginated in username order.
///
/// The search term goes through the same normalisation as stored usernames (trim surrounding whitespace, NFKC,
/// lowercase), so the `username` column is compared directly without wrapping it in `LOWER()`, which uses a range
/// scan of the unique username index and gets index order for sorting. The database is not queried when the
/// normalised term is empty or longer than the 50-character username limit. One extra row is fetched to tell whether
/// there is a next page, and the total is not counted: counting would read the whole prefix range, and a one-character
/// prefix may cover a large share of all members.
pub async fn search(
    db: &Database,
    query: &str,
    page: usize,
    viewer_uid: Option<i64>,
) -> Result<MemberSearchPage> {
    ensure!((1..=SEARCH_MAX_PAGE).contains(&page), "invalid search page");
    let prefix = crate::identity::normalized_username_query(query);
    if prefix.is_empty() || prefix.chars().count() > 50 {
        return Ok(MemberSearchPage {
            members: Vec::new(),
            has_next: false,
        });
    }
    let sql = format!(
        "SELECT m.uid,m.username,m.avatar,m.avatar_aid,m.regdate,m.bio FROM gonof_common_member m
         WHERE m.username LIKE ? ESCAPE '!' AND m.account_state<>'deleted' AND {}
         ORDER BY m.username LIMIT ? OFFSET ?",
        listed_scope_sql(viewer_uid)
    );
    let rows = crate::db::query(&sql)
        .bind(prefix_pattern(&prefix))
        .bind(i64::try_from(SEARCH_PAGE_SIZE + 1)?)
        .bind(i64::try_from((page - 1) * SEARCH_PAGE_SIZE)?)
        .fetch_all(db.executor())
        .await?;
    let has_next = rows.len() > SEARCH_PAGE_SIZE && page < SEARCH_MAX_PAGE;
    let members = rows
        .iter()
        .take(SEARCH_PAGE_SIZE)
        .map(|row| decode_listed(row, true))
        .collect::<Result<_>>()?;
    Ok(MemberSearchPage { members, has_next })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_pattern_escapes_like_wildcards_and_the_escape_character() {
        assert_eq!(prefix_pattern("ab"), "ab%");
        assert_eq!(prefix_pattern("a%b_c!d"), "a!%b!_c!!d%");
        // Non-ASCII text passes through: "\u{91d1}\u{5e01}" is the Chinese for "gold coins".
        assert_eq!(prefix_pattern("\u{91d1}\u{5e01}"), "\u{91d1}\u{5e01}%");
    }

    #[test]
    fn anonymous_scope_lists_only_public_spaces() {
        assert_eq!(
            listed_scope_sql(None),
            "m.space_visibility='public' AND 1=1"
        );
        let member = listed_scope_sql(Some(7));
        assert!(member.starts_with("m.space_visibility IN ('public','members') AND NOT EXISTS"));
        assert!(member.contains("blocked.uid=7"));
    }
}
