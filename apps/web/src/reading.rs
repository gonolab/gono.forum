//! Reading and list preferences (`gonof_home_reading_preference`).
//!
//! Preferences are stored on the server. For logged-in members the page frame and the forum filter share one cache
//! key isolated by uid; redirect entry points read the database separately when needed. Anonymous visitors use the
//! site defaults.
//!
//! - The default sort is chosen only between "Latest replies / Newest threads", and applies to the bare home and forum
//!   URLs.
//! - The default reply view only decides whether thread links **generated** on the site carry `?view=flat`; the bare
//!   URL `/thread-N` is two-level for everyone, so page numbers and floors in a URL mean the same
//!   whoever views it.
//! - Title jumps to the latest reply and opening in a new tab only apply to the thread lists of the home page,
//!   forums and search.
//!
//! Preferences a member has not set are NULL in the database and follow the site defaults `settings.reading.*`.
use crate::{
    content::writes::ContentWriteError,
    db::{self, Database},
    identity::Actor,
    interaction,
    runtime::ReadingDefaults,
    urls::Sort,
};

type Result<T> = std::result::Result<T, ContentWriteError>;

/// Values of the five preferences.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Values {
    /// Sort of the home page and forums when there is no `sort` parameter: only ever `Latest` or `Created`.
    pub list_sort: Sort,
    /// Use the flat view when generating thread links on the site.
    pub flat: bool,
    /// List titles link to the latest reply.
    pub title_latest: bool,
    /// List titles open in a new tab.
    pub title_new_tab: bool,
    /// Avatars are round; otherwise rounded squares.
    pub avatar_circle: bool,
}

impl Values {
    /// Site defaults `settings.reading.*`. The values were already validated by settings decoding against the same
    /// allowlist (a unit test pins both sides to agree).
    pub fn site(defaults: &ReadingDefaults) -> Self {
        Self {
            list_sort: Reading::parse_sort(&defaults.list_sort).expect("decoder checked the sort"),
            flat: Reading::parse_view(&defaults.thread_view).expect("decoder checked the view"),
            title_latest: defaults.title_latest,
            title_new_tab: defaults.title_new_tab,
            avatar_circle: Reading::parse_avatar_shape(&defaults.avatar_shape)
                .expect("decoder checked the avatar shape"),
        }
    }
}

/// What the member chose. `None` follows the site default (NULL in the database).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Choices {
    pub list_sort: Option<Sort>,
    pub flat: Option<bool>,
    pub title_latest: Option<bool>,
    pub title_new_tab: Option<bool>,
    pub avatar_circle: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reading {
    /// The effective values: the member's choice, with the site default for anything not chosen.
    pub list_sort: Sort,
    pub flat: bool,
    pub title_latest: bool,
    pub title_new_tab: bool,
    pub avatar_circle: bool,
    pub version: i64,
    /// Version of the personal style for its stylesheet address; 0 when there is none (never saved, or cleared).
    /// The page frame only takes the version; the style source is read only by `/my/style` and the stylesheet.
    pub css_version: i64,
    /// The member's choices; all `None` for anonymous visitors.
    pub chosen: Choices,
    /// Site defaults, used by the form's "Follow the site (now …)".
    pub site: Values,
}

impl Reading {
    /// Merges the member's choices with the site defaults.
    pub fn resolve(chosen: Choices, site: Values, version: i64, css_version: i64) -> Self {
        Self {
            list_sort: chosen.list_sort.unwrap_or(site.list_sort),
            flat: chosen.flat.unwrap_or(site.flat),
            title_latest: chosen.title_latest.unwrap_or(site.title_latest),
            title_new_tab: chosen.title_new_tab.unwrap_or(site.title_new_tab),
            avatar_circle: chosen.avatar_circle.unwrap_or(site.avatar_circle),
            version,
            css_version,
            chosen,
            site,
        }
    }

    /// Anonymous visitors: everything follows the site defaults.
    pub fn site_default(defaults: &ReadingDefaults) -> Self {
        Self::resolve(Choices::default(), Values::site(defaults), 0, 0)
    }

    /// Placeholder before the page frame has read the settings, not a default; see
    /// `router::frame::SiteFrame::loaded`.
    pub fn blank() -> Self {
        let site = Values {
            list_sort: Sort::Latest,
            flat: false,
            title_latest: false,
            title_new_tab: false,
            avatar_circle: false,
        };
        Self::resolve(Choices::default(), site, 0, 0)
    }

    pub fn sort_value(&self) -> &'static str {
        self.list_sort.value()
    }

    pub fn view_value(&self) -> &'static str {
        view_text(self.flat)
    }

    /// Link of a thread list title: with "Jump to the latest reply" on and a valid reply, links to that reply's
    /// permanent URL (`/posts/{pid}` redirects to the actual floor by current permissions and view preference);
    /// otherwise to the first page.
    pub fn title_href(&self, id: u64, last_reply_pid: Option<i64>) -> String {
        match last_reply_pid.filter(|pid| self.title_latest && *pid > 0) {
            Some(pid) => format!("/posts/{pid}"),
            None => crate::urls::thread(id, 1, self.flat),
        }
    }

    /// The two values used by the form and the database; anything else is rejected.
    pub fn parse_sort(value: &str) -> Option<Sort> {
        match value {
            "latest" => Some(Sort::Latest),
            "created" => Some(Sort::Created),
            _ => None,
        }
    }

    pub fn parse_view(value: &str) -> Option<bool> {
        match value {
            "nested" => Some(false),
            "flat" => Some(true),
            _ => None,
        }
    }

    pub fn parse_avatar_shape(value: &str) -> Option<bool> {
        match value {
            "rounded" => Some(false),
            "circle" => Some(true),
            _ => None,
        }
    }

    pub fn avatar_shape(&self) -> &'static str {
        shape_text(self.avatar_circle)
    }
}

fn view_text(flat: bool) -> &'static str {
    if flat { "flat" } else { "nested" }
}

fn shape_text(circle: bool) -> &'static str {
    if circle { "circle" } else { "rounded" }
}

/// Does not select `custom_css`: the style source can be up to 16 KiB, and only `/my/style` and the stylesheet need
/// it. Whether there is one is enough for the page frame: a cleared style keeps its version (the stylesheet address
/// must not repeat) but pages no longer link it.
const SELECT: &str = "SELECT list_sort,thread_view,title_latest,title_new_tab,version,avatar_shape,custom_css_version,custom_css IS NOT NULL AS has_custom_css FROM gonof_home_reading_preference WHERE uid=?";

/// The raw choices of the member row. NULL follows the site; a bad value in the database (hand-edited) is also
/// treated as following the site, so one dirty row cannot turn a list page into a 500 — this is a member preference,
/// not a site setting, and the "no fallback" rule only covers site settings (docs/settings-and-caching.md, "Reading
/// settings").
fn decode(row: Option<db::DbRow>) -> anyhow::Result<(Choices, i64, i64)> {
    let Some(row) = row else {
        return Ok((Choices::default(), 0, 0));
    };
    let text = |column: &str| -> anyhow::Result<Option<String>> {
        Ok(row.try_get::<Option<String>, _>(column)?)
    };
    let flag = |column: &str| -> anyhow::Result<Option<bool>> {
        Ok(row
            .try_get::<Option<i64>, _>(column)?
            .and_then(|value| match value {
                0 => Some(false),
                1 => Some(true),
                _ => None,
            }))
    };
    let chosen = Choices {
        list_sort: text("list_sort")?.and_then(|v| Reading::parse_sort(&v)),
        flat: text("thread_view")?.and_then(|v| Reading::parse_view(&v)),
        title_latest: flag("title_latest")?,
        title_new_tab: flag("title_new_tab")?,
        avatar_circle: text("avatar_shape")?.and_then(|v| Reading::parse_avatar_shape(&v)),
    };
    let css_version = if row.try_get::<i64, _>("has_custom_css")? != 0 {
        row.try_get("custom_css_version")?
    } else {
        0
    };
    Ok((chosen, row.try_get("version")?, css_version))
}

pub(crate) async fn load_choices(db: &Database, uid: i64) -> anyhow::Result<(Choices, i64, i64)> {
    decode(
        crate::db::query(SELECT)
            .bind(uid)
            .fetch_optional(db.executor())
            .await?,
    )
}

/// Preferences for a page. Anonymous visitors do not hit the database and get all site defaults. The session was
/// already verified in middleware, so this only reads one row by primary key.
///
/// Handlers that render pages should not call it directly: the page frame has already read it; use
/// `state.frame.reading`.
pub async fn for_viewer(
    db: &Database,
    uid: Option<i64>,
    defaults: &ReadingDefaults,
) -> anyhow::Result<Reading> {
    let site = Values::site(defaults);
    let Some(uid) = uid else {
        return Ok(Reading::resolve(Choices::default(), site, 0, 0));
    };
    let (chosen, version, css_version) = load_choices(db, uid).await?;
    Ok(Reading::resolve(chosen, site, version, css_version))
}

/// Saves. Rechecks credentials and compares the version under the member row lock, written the same way as
/// notification preferences; the operation log is written in the same transaction.
/// All five are saved together: `None` writes NULL, i.e. follows the site default.
pub async fn save(
    db: &Database,
    cache: &crate::cache::Cache,
    actor: Actor,
    chosen: &Choices,
    version: i64,
) -> Result<()> {
    if version < 0 {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().settings_error_reading_version(),
        ));
    }
    if chosen
        .list_sort
        .is_some_and(|sort| !matches!(sort, Sort::Latest | Sort::Created))
    {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().settings_error_reading_sort(),
        ));
    }
    let mut tx = db.begin_write().await?;
    interaction::lock_actor(db, &mut tx, actor, false).await?;
    let (current, current_version, _) = decode(
        crate::db::query(SELECT)
            .bind(actor.uid)
            .fetch_optional(db.observe(&mut *tx))
            .await?,
    )?;
    if current_version != version {
        return Err(ContentWriteError::Conflict(
            crate::i18n::current().settings_error_reading_conflict(),
        ));
    }
    if current == *chosen {
        tx.commit().await?;
        crate::cache::member::invalidate(cache, actor.uid).await?;
        return Ok(());
    }
    let version = version.checked_add(1).ok_or(ContentWriteError::Validation(
        crate::i18n::current().settings_error_reading_version_range(),
    ))?;
    let now = crate::identity::now();
    crate::db::query("INSERT INTO gonof_home_reading_preference(uid,list_sort,thread_view,title_latest,title_new_tab,avatar_shape,version,updated_at) VALUES(?,?,?,?,?,?,?,?) ON DUPLICATE KEY UPDATE list_sort=VALUES(list_sort),thread_view=VALUES(thread_view),title_latest=VALUES(title_latest),title_new_tab=VALUES(title_new_tab),avatar_shape=VALUES(avatar_shape),version=VALUES(version),updated_at=VALUES(updated_at)")
        .bind(actor.uid)
        .bind(chosen.list_sort.map(Sort::value))
        .bind(chosen.flat.map(view_text))
        .bind(chosen.title_latest.map(i64::from))
        .bind(chosen.title_new_tab.map(i64::from))
        .bind(chosen.avatar_circle.map(shape_text))
        .bind(version)
        .bind(now)
        .execute(db.observe(&mut *tx))
        .await?;
    crate::db::query("INSERT INTO gonof_common_log(uid,operationuid,type,data,source,dateline) VALUES(?,?,'identity.reading_preferences',?,'web',?)")
        .bind(actor.uid)
        .bind(actor.uid)
        .bind(serde_json::json!({"version": version}).to_string())
        .bind(now)
        .execute(db.observe(&mut *tx))
        .await?;
    tx.commit().await?;
    crate::cache::member::invalidate(cache, actor.uid).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_two_sorts_and_two_views_parse() {
        assert_eq!(Reading::parse_sort("latest"), Some(Sort::Latest));
        assert_eq!(Reading::parse_sort("created"), Some(Sort::Created));
        for value in ["pinned", "featured", "LATEST", "", "latest "] {
            assert_eq!(Reading::parse_sort(value), None, "{value}");
        }
        assert_eq!(Reading::parse_view("flat"), Some(true));
        assert_eq!(Reading::parse_view("nested"), Some(false));
        assert_eq!(Reading::parse_view(""), None);
        assert_eq!(Reading::parse_avatar_shape("circle"), Some(true));
        assert_eq!(Reading::parse_avatar_shape("rounded"), Some(false));
        assert_eq!(Reading::parse_avatar_shape("square"), None);
    }

    /// The site default allowlist and the parsing here are the same: any value settings decoding lets through can be
    /// parsed here.
    #[test]
    fn site_defaults_parse_every_allowed_value() {
        for value in crate::runtime::LIST_SORTS {
            assert!(Reading::parse_sort(value).is_some(), "{value}");
        }
        for value in crate::runtime::THREAD_VIEWS {
            assert!(Reading::parse_view(value).is_some(), "{value}");
        }
        for value in crate::runtime::AVATAR_SHAPES {
            assert!(Reading::parse_avatar_shape(value).is_some(), "{value}");
        }
    }

    #[test]
    fn unset_choices_follow_the_site() {
        let site = Values {
            list_sort: Sort::Created,
            flat: true,
            title_latest: true,
            title_new_tab: false,
            avatar_circle: true,
        };
        let anonymous = Reading::resolve(Choices::default(), site, 0, 0);
        assert_eq!(anonymous.list_sort, Sort::Created);
        assert!(anonymous.flat && anonymous.title_latest && anonymous.avatar_circle);
        let member = Reading::resolve(
            Choices {
                flat: Some(false),
                ..Choices::default()
            },
            site,
            3,
            0,
        );
        assert!(!member.flat, "a chosen item uses the member's own choice");
        assert_eq!(
            member.list_sort,
            Sort::Created,
            "an item not chosen follows the site"
        );
        assert!(member.avatar_circle);
    }
}
