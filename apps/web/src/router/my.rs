//! The personal center directory: top-level sections and which tabs belong to them.
//!
//! The page frame (title, tabs) and the top-level entries on the member card share the constants here; routes are still
//! written one by one in `routes()` and the various modules, and the paths must match `PAGES`. There is no table of
//! 301 redirects from old paths.
use axum::{extract::Path as AxumPath, routing::post};

use super::extract::{CsrfForm, Page as PageState};
use super::*;
use crate::rate_limit::Rule;

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/my", get(overview))
        .route("/my/threads", get(own_threads))
        .route("/my/posts", get(own_posts))
        .route("/my/public", get(public_profile))
        .route("/my/messages", get(messages))
        .route("/my/messages/new", get(compose_page).post(compose))
        .route("/my/messages/{id}", get(conversation))
        .route("/my/messages/{id}/replies", post(reply))
        // Explicitly claim the whole prefix: unregistered /my sub-paths must be 404 and must not fall through to the
        // dynamic routes for members' public content.
        .route("/my/", get(not_found))
        .route("/my/{*path}", get(not_found))
}

/// Frame data shared by personal center pages: the screen reader heading, the top-level section name, the second-level
/// tabs and the top-level entries on the card.
pub(super) struct Frame {
    pub heading: &'static str,
    /// The page id, so the points page can be named after the site's points ([`Frame::heading_label`]).
    pub page: &'static str,
    pub section_title: &'static str,
    pub tabs: Vec<Tab>,
    pub links: Vec<CardLink>,
    pub icons: Vec<CardLink>,
}

impl Frame {
    /// The screen reader heading; `credit` is the site's name for its points (`p.credit()`), the name of the points
    /// page.
    pub fn heading_label<'a>(&'a self, credit: &'a str) -> &'a str {
        if self.page == "credits" {
            credit
        } else {
            self.heading
        }
    }
}

/// The member's username for the `$name_html` slot of the overview greeting: not translated by browsers.
pub(super) fn username_markup(name: &str) -> crate::i18n::Markup {
    crate::i18n::Markup::trusted(format!(
        "<span translate=\"no\">{}</span>",
        crate::i18n::Markup::text(name)
    ))
}

pub(super) fn frame(page_id: &str) -> Frame {
    frame_with_filter(page_id, "")
}

pub(super) fn frame_with_filter(page_id: &str, filter: &str) -> Frame {
    let current = entry(page_id).expect("the page must be registered in the directory first");
    let (links, icons) = card_links(page_id);
    Frame {
        heading: current.title(),
        page: current.id,
        section_title: section(current.section).map_or("", |s| s.title()),
        tabs: tabs_with_filter(page_id, filter),
        links,
        icons,
    }
}

/// Fills in the two pieces of data the personal center's right column needs; on failure, returns the error page to
/// respond with directly.
///
/// The right column appears on every personal center page, and neither piece belongs to a particular page, so both are
/// fetched here: the private message dot only asks whether there is anything unread, and the thread card queries from
/// the logged-in view rather than taking the public result from the shared cache.
/// The card does not show the six numbers, so the member summary is not queried here; the overview page body needs
/// it and fetches it itself.
pub(super) async fn fill_shell(state: &AppState, p: &mut PageMeta<'_>) -> Option<Response> {
    let session = state.viewer.session.as_ref()?;
    match crate::messages::has_unread(&state.db, session.member.uid).await {
        Ok(unread) => p.has_unread_messages = unread,
        Err(error) => return Some(database_error(state, error)),
    }
    let blocked = match crate::content::blocked_authors(&state.db, session.member.uid).await {
        Ok(blocked) => blocked,
        Err(error) => return Some(database_error(state, error)),
    };
    let forums = state
        .frame
        .listable_forums(&state.frame.forum_filters.timeline_excluded());
    match crate::content::sidebar_topics_for_viewer(
        &state.db,
        crate::content::ListViewer {
            uid: Some(session.member.uid),
            forums: &forums,
            blocked_uids: &blocked,
        },
        crate::content::SidebarParams::from_settings(&state.frame.settings),
    )
    .await
    {
        Ok((hot, latest)) => {
            p.sidebar = Some(super::SidebarTopics { hot, latest });
            None
        }
        Err(error) => Some(database_error(state, error)),
    }
}

/// Only a top-level section with a single page may produce tabs from a query parameter: the first is "all", the rest
/// filter by value, and switching within the page does not change the path.
pub struct FilterTabs {
    pub page: &'static str,
    pub param: &'static str,
    pub options: &'static [&'static str],
}

pub const FILTER_TABS: &[FilterTabs] = &[FilterTabs {
    page: "notifications",
    param: "category",
    options: &["replies", "following", "system"],
}];

/// The tab name of a filter value in this request's language.
fn filter_title(value: &str) -> &'static str {
    let t = crate::i18n::current();
    match value {
        "replies" => t.my_filter_replies(),
        "following" => t.my_filter_following(),
        _ => t.my_filter_system(),
    }
}

/// The three icons in the card's top-right corner, in this order: affairs, settings, content management.
/// Top-level sections not in this list get text links; together the two cover SECTIONS exactly.
const CARD_ICONS: &[&str] = &["affairs", "settings", "moderation"];

/// A top-level entry on the personal center card. The card is the top-level navigation; the second level stays with
/// the in-page tabs.
pub struct CardLink {
    pub href: &'static str,
    pub title: &'static str,
    pub icon: &'static str,
    pub staff: bool,
    /// The path is the current page; the template writes aria-current="page".
    pub current_page: bool,
    /// Merely within this top-level section; the template writes aria-current="true" and must not pose as the current
    /// page.
    pub current_section: bool,
}

fn card_link(section: &'static Section, current: Option<&'static Entry>) -> Option<CardLink> {
    let landing = section_pages(section.id).next()?;
    // /my/moderation in the directory is only a placeholder with no registered route; the card goes straight to the
    // admin area.
    let href = if section.id == "moderation" {
        "/manage"
    } else {
        landing.path
    };
    Some(CardLink {
        href,
        title: section.title(),
        icon: section.icon,
        staff: section.staff,
        current_page: current.is_some_and(|c| c.path == href),
        current_section: current.is_some_and(|c| c.section == section.id),
    })
}

/// Returns the card's two groups (text links, icons), each keeping its own order.
pub fn card_links(page_id: &str) -> (Vec<CardLink>, Vec<CardLink>) {
    let current = entry(page_id);
    (
        SECTIONS
            .iter()
            .filter(|s| !CARD_ICONS.contains(&s.id))
            .filter_map(|s| card_link(s, current))
            .collect(),
        CARD_ICONS
            .iter()
            .filter_map(|id| section(id))
            .filter_map(|s| card_link(s, current))
            .collect(),
    )
}

/// Second-level in-page tabs. A top-level section with a single page also gets one, serving as that page's title.
pub struct Tab {
    pub href: String,
    pub title: &'static str,
    /// The page id, empty for filter tabs ([`Tab::label`]).
    pub page: &'static str,
    pub current: bool,
}

impl Tab {
    /// The tab's name; `credit` is the site's name for its points (`p.credit()`), the name of the points page.
    pub fn label<'a>(&'a self, credit: &'a str) -> &'a str {
        if self.page == "credits" {
            credit
        } else {
            self.title
        }
    }
}

pub fn tabs(current: &str) -> Vec<Tab> {
    tabs_with_filter(current, "")
}

/// `filter` only matters for single-page sections declared in FILTER_TABS, and is the currently selected query
/// parameter value.
pub fn tabs_with_filter(current: &str, filter: &str) -> Vec<Tab> {
    let Some(current) = entry(current) else {
        return Vec::new();
    };
    if let Some(filters) = FILTER_TABS.iter().find(|f| f.page == current.id) {
        let mut tabs = vec![Tab {
            href: current.path.to_owned(),
            title: crate::i18n::current().home_all(),
            page: "",
            current: filter.is_empty(),
        }];
        let param = filters.param;
        tabs.extend(filters.options.iter().map(|value| Tab {
            href: format!("{}?{param}={value}", current.path),
            title: filter_title(value),
            page: "",
            current: *value == filter,
        }));
        return tabs;
    }
    section_pages(current.section)
        .map(|p| Tab {
            href: p.path.to_owned(),
            title: p.title(),
            page: p.id,
            current: p.id == current.id,
        })
        .collect()
}

#[derive(Template)]
#[template(path = "my/overview.html")]
struct OverviewTemplate<'a> {
    p: PageMeta<'a>,
    frame: Frame,
    /// Escaped bio HTML with addresses wrapped in links (`bio::html`).
    bio: String,
    joined: String,
    threads: i64,
    replies: i64,
    favorites: i64,
    available: i64,
    frozen: i64,
    following_threads: i64,
    following_forums: i64,
    profile_href: String,
}

async fn overview(PageState(state): PageState) -> Response {
    let Some(session) = state.viewer.session.as_ref() else {
        return auth::redirect("/login?return_to=%2Fmy");
    };
    let actor = session.member.actor();
    // The six numbers appear only on this page, not on the card, so only this page fetches the summary.
    let summary = match state.member_summary(actor).await {
        Ok(summary) => summary,
        Err(error) => return database_error(&state, error),
    };
    let data = match crate::content::home::my_overview(&state.db, actor).await {
        Ok(data) => data,
        Err(error) => return database_error(&state, error),
    };
    let profile_href = crate::urls::space(session.member.uid);
    let mut p = meta(
        &state,
        crate::i18n::current().frame_my_center().into(),
        crate::i18n::current().my_description_overview().into(),
        "/my",
        "",
        "",
    );
    if let Some(error) = fill_shell(&state, &mut p).await {
        return error;
    }
    render(&OverviewTemplate {
        frame: frame("home"),
        bio: crate::bio::html(&data.bio),
        joined: crate::clock::date(data.regdate, &crate::clock::current()),
        threads: summary.threads,
        replies: summary.replies,
        favorites: summary.favorites,
        available: summary.available,
        frozen: summary.frozen,
        following_threads: data.following_threads,
        following_forums: data.following_forums,
        profile_href,
        p,
    })
}

/// A top-level section on the sidebar member card. When `staff` is true it only appears to moderators and
/// administrators.
pub struct Section {
    pub id: &'static str,
    pub icon: &'static str,
    pub staff: bool,
}

impl Section {
    /// The section's name in this request's language.
    pub fn title(&self) -> &'static str {
        let t = crate::i18n::current();
        match self.id {
            "home" => t.my_section_home(),
            "content" => t.my_section_content(),
            "notifications" => t.frame_notifications(),
            "messages" => t.home_card_messages(),
            "settings" => t.home_card_settings(),
            "affairs" => t.home_card_cases(),
            _ => t.frame_manage(),
        }
    }
}

/// A page under a top-level section; second-level tabs are rendered in this order, and the first page of a section is
/// its landing page.
pub struct Entry {
    pub id: &'static str,
    pub path: &'static str,
    pub section: &'static str,
}

impl Entry {
    /// The page's name in this request's language. The points page is named after the site's points
    /// (`settings.site.credit_name`), which only the page frame knows: [`Tab::label`] and [`Frame::heading_label`]
    /// put it in; here it is the language's default word.
    pub fn title(&self) -> &'static str {
        let t = crate::i18n::current();
        match self.id {
            "home" => t.my_page_overview(),
            "credits" => t.brand_credit_name(),
            "growth" => t.my_page_growth(),
            "prizes" => t.my_page_prizes(),
            "invites" => t.my_page_invites(),
            "public" => t.frame_public_profile(),
            "threads" => t.home_stats_threads(),
            "posts" => t.home_stats_replies(),
            "favorites" => t.home_card_favorites(),
            "following-threads" => t.my_page_following_threads(),
            "following-forums" => t.my_page_following_forums(),
            "following-members" => t.my_page_following_members(),
            "followers" => t.profile_followers(),
            "following-feed" => t.my_page_following_feed(),
            "attachments" => t.my_page_attachments(),
            "attachments-trash" => t.my_page_attachments_trash(),
            "notifications" => t.frame_notifications(),
            "messages" => t.home_card_messages(),
            "profile" => t.frame_my_profile(),
            "account" => t.frame_account_security(),
            "api" => t.my_page_api(),
            "preferences" => t.my_page_preferences(),
            "reading" => t.my_page_reading(),
            "style" => t.my_page_style(),
            "blocked-users" => t.my_page_blocked_users(),
            "forum-filters" => t.my_page_forum_filters(),
            "feed" => t.my_page_feed(),
            "reports" => t.my_page_reports(),
            "restorations" => t.governance_restorations(),
            "sanctions" => t.my_page_sanctions(),
            "data" => t.my_page_data(),
            _ => t.frame_manage(),
        }
    }
}

pub const SECTIONS: &[Section] = &[
    Section {
        id: "home",
        icon: "home",
        staff: false,
    },
    Section {
        id: "content",
        icon: "file",
        staff: false,
    },
    Section {
        id: "notifications",
        icon: "bell",
        staff: false,
    },
    Section {
        id: "messages",
        icon: "message",
        staff: false,
    },
    Section {
        id: "settings",
        icon: "settings",
        staff: false,
    },
    Section {
        id: "affairs",
        icon: "flag",
        staff: false,
    },
    Section {
        id: "moderation",
        icon: "tool",
        staff: true,
    },
];

pub const PAGES: &[Entry] = &[
    Entry {
        id: "home",
        path: "/my",
        section: "home",
    },
    Entry {
        id: "credits",
        path: "/my/credits",
        section: "home",
    },
    Entry {
        id: "growth",
        path: "/my/credits/growth",
        section: "home",
    },
    Entry {
        id: "prizes",
        path: "/my/prizes",
        section: "home",
    },
    Entry {
        id: "invites",
        path: "/my/invites",
        section: "home",
    },
    Entry {
        id: "public",
        path: "/my/public",
        section: "home",
    },
    Entry {
        id: "threads",
        path: "/my/threads",
        section: "content",
    },
    Entry {
        id: "posts",
        path: "/my/posts",
        section: "content",
    },
    Entry {
        id: "favorites",
        path: "/my/favorites",
        section: "content",
    },
    Entry {
        id: "following-threads",
        path: "/my/following/threads",
        section: "content",
    },
    Entry {
        id: "following-forums",
        path: "/my/following/forums",
        section: "content",
    },
    Entry {
        id: "following-members",
        path: "/my/following/members",
        section: "content",
    },
    Entry {
        id: "followers",
        path: "/my/followers",
        section: "content",
    },
    Entry {
        id: "following-feed",
        path: "/my/following/feed",
        section: "content",
    },
    Entry {
        id: "attachments",
        path: "/my/attachments",
        section: "content",
    },
    Entry {
        id: "attachments-trash",
        path: "/my/attachments/trash",
        section: "content",
    },
    Entry {
        id: "notifications",
        path: "/my/notifications",
        section: "notifications",
    },
    Entry {
        id: "messages",
        path: "/my/messages",
        section: "messages",
    },
    Entry {
        id: "profile",
        path: "/my/profile",
        section: "settings",
    },
    Entry {
        id: "account",
        path: "/my/account",
        section: "settings",
    },
    Entry {
        id: "api",
        path: "/my/api",
        section: "settings",
    },
    Entry {
        id: "preferences",
        path: "/my/preferences",
        section: "settings",
    },
    Entry {
        id: "reading",
        path: "/my/reading",
        section: "settings",
    },
    Entry {
        id: "style",
        path: "/my/style",
        section: "settings",
    },
    Entry {
        id: "blocked-users",
        path: "/my/blocked-users",
        section: "settings",
    },
    Entry {
        id: "forum-filters",
        path: "/my/forum-filters",
        section: "settings",
    },
    Entry {
        id: "feed",
        path: "/my/feed",
        section: "settings",
    },
    Entry {
        id: "reports",
        path: "/my/reports",
        section: "affairs",
    },
    Entry {
        id: "restorations",
        path: "/my/restorations",
        section: "affairs",
    },
    Entry {
        id: "sanctions",
        path: "/my/sanctions",
        section: "affairs",
    },
    Entry {
        id: "data",
        path: "/my/data",
        section: "affairs",
    },
    Entry {
        id: "moderation",
        path: "/my/moderation",
        section: "moderation",
    },
];

pub fn entry(id: &str) -> Option<&'static Entry> {
    PAGES.iter().find(|p| p.id == id)
}

pub fn section(id: &str) -> Option<&'static Section> {
    SECTIONS.iter().find(|s| s.id == id)
}

/// The pages under a top-level section, in tab order.
pub fn section_pages(section: &str) -> impl Iterator<Item = &'static Entry> {
    PAGES.iter().filter(move |p| p.section == section)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn catalog_is_internally_consistent() {
        let ids: HashSet<_> = SECTIONS.iter().map(|s| s.id).collect();
        assert_eq!(ids.len(), SECTIONS.len(), "section ids must be unique");
        let paths: HashSet<_> = PAGES.iter().map(|p| p.path).collect();
        assert_eq!(paths.len(), PAGES.len(), "page paths must be unique");
        for id in CARD_ICONS {
            assert!(ids.contains(id), "{id} is not a registered section");
        }
        let (links, icons) = card_links("home");
        assert_eq!(
            links.len() + icons.len(),
            SECTIONS.len(),
            "the card's text links and icons together must cover every section"
        );
        assert_eq!(icons.len(), CARD_ICONS.len());
        assert!(
            icons.iter().any(|i| i.href == "/manage"),
            "content management is not under /my, so its icon points to the admin area"
        );
        let home = links.first().expect("the home page is the first text link");
        assert!(
            home.current_page && home.current_section,
            "the overview page is current at both levels"
        );
        for page in PAGES {
            assert!(
                ids.contains(page.section),
                "the section of {} does not exist",
                page.id
            );
            assert!(
                page.path == "/my" || page.path.starts_with("/my/"),
                "{} is not under /my",
                page.path
            );
        }
        for section in SECTIONS {
            assert!(
                section_pages(section.id).next().is_some(),
                "{} has no pages, so its sidebar link would point nowhere",
                section.id
            );
        }
        // The first page of the first top-level section is the entry of the personal center.
        assert_eq!(
            section_pages(SECTIONS[0].id).next().map(|p| p.path),
            Some("/my")
        );
    }
}

#[derive(Template)]
#[template(path = "my/content.html")]
struct ContentTemplate<'a> {
    p: PageMeta<'a>,
    frame: Frame,
    rows: Vec<OwnRow>,
    total: usize,
    page: usize,
    previous: String,
    next: String,
}

/// One row of the member's own list: the content itself, plus the restoration request entry (hidden when
/// `restore_href` is empty).
struct OwnRow {
    post: crate::content::MemberPost,
    status: &'static str,
    restore_href: String,
    restore_label: &'static str,
}

/// The restoration request entry: points to a pending request if there is one; offers the form if a request can be
/// made; points back to the original post for self-restoration while a withdrawal is still within its time window;
/// during the cooldown after a rejection, points to that decision.
fn restore_link(
    own: &crate::content::OwnState,
    post: &crate::content::MemberPost,
    latest: Option<&crate::moderation::Restoration>,
    rules: &crate::moderation::RestorationRules,
    now: i64,
) -> (String, &'static str) {
    use crate::moderation::{Eligibility, eligibility};
    if let Some(r) = latest.filter(|r| r.status == "pending") {
        return (
            format!("/my/restorations/{}", r.id),
            crate::i18n::current().governance_restore_pending_link(),
        );
    }
    match eligibility(own, false, rules.window_seconds, now) {
        Eligibility::Eligible(_) => match latest {
            Some(r)
                if r.status == "rejected"
                    && now < r.handled_at.saturating_add(rules.cooldown_seconds) =>
            {
                (
                    format!("/my/restorations/{}", r.id),
                    crate::i18n::current().governance_restore_rejected_link(),
                )
            }
            _ => (
                format!("/my/restorations/new?pid={}", post.pid),
                crate::i18n::current().governance_restore_link(),
            ),
        },
        Eligibility::SelfRestore => (
            post.href.clone(),
            crate::i18n::current().governance_restore_self_link(),
        ),
        _ => match latest {
            Some(r) if r.status == "accepted" => (
                format!("/my/restorations/{}", r.id),
                crate::i18n::current().governance_restore_accepted_link(),
            ),
            Some(r) => (
                format!("/my/restorations/{}", r.id),
                crate::i18n::current().governance_restore_rejected_link(),
            ),
            None => (String::new(), ""),
        },
    }
}

#[derive(Default, Deserialize)]
struct ListQuery {
    page: Option<usize>,
}

const PAGE_SIZE: usize = 20;

async fn own_threads(state: PageState, query: Query<ListQuery>) -> Response {
    own_content(state, query, crate::content::MemberContentKind::Threads).await
}

async fn own_posts(state: PageState, query: Query<ListQuery>) -> Response {
    own_content(state, query, crate::content::MemberContentKind::Posts).await
}

/// Thread or reply lists from the member's own view: they include moderation-hidden and author-withdrawn content,
/// which public collections do not show.
async fn own_content(
    PageState(state): PageState,
    Query(query): Query<ListQuery>,
    kind: crate::content::MemberContentKind,
) -> Response {
    let (page_id, path) = match kind {
        crate::content::MemberContentKind::Threads => ("threads", "/my/threads"),
        crate::content::MemberContentKind::Posts => ("posts", "/my/posts"),
    };
    let Some(session) = state.viewer.session.as_ref() else {
        return auth::redirect(&format!(
            "/login?return_to={}",
            url::form_urlencoded::byte_serialize(path.as_bytes()).collect::<String>()
        ));
    };
    let page = query.page.unwrap_or(1);
    if page == 0 {
        return error_page(
            &state,
            StatusCode::BAD_REQUEST,
            crate::i18n::current().my_error_page_number(),
        );
    }
    let uid = session.member.uid;
    let result = match crate::content::own_content(
        &state.db,
        uid,
        kind,
        page,
        PAGE_SIZE,
        crate::urls::Paging::from_settings(&state.frame.settings),
    )
    .await
    {
        Ok(result) => result,
        Err(error) => return database_error(&state, error),
    };
    let pids: Vec<i64> = result
        .rows
        .iter()
        .filter(|row| row.own.hidden || row.own.withdrawn)
        .map(|row| row.post.pid as i64)
        .collect();
    let latest = match crate::moderation::latest_restorations(&state.db, uid, &pids).await {
        Ok(latest) => latest,
        Err(error) => return database_error(&state, anyhow::Error::from(error)),
    };
    let rules = crate::moderation::RestorationRules::from_settings(&state.frame.settings);
    let now = crate::identity::now();
    let rows = result
        .rows
        .into_iter()
        .map(|row| {
            let (restore_href, restore_label) = if row.own.hidden || row.own.withdrawn {
                restore_link(
                    &row.own,
                    &row.post,
                    latest.get(&(row.post.pid as i64)),
                    &rules,
                    now,
                )
            } else {
                (String::new(), "")
            };
            OwnRow {
                post: row.post,
                status: row.status,
                restore_href,
                restore_label,
            }
        })
        .collect();
    let entry = entry(page_id).expect("the page must be in the directory");
    let mut p = meta(
        &state,
        entry.title().into(),
        crate::i18n::current().my_description_content().into(),
        path,
        "",
        "",
    );
    if let Some(error) = fill_shell(&state, &mut p).await {
        return error;
    }
    let last = crate::content::member_list_pages(result.total, PAGE_SIZE);
    render(&ContentTemplate {
        p,
        frame: frame(page_id),
        total: result.total,
        rows,
        page,
        previous: if page > 1 {
            format!("{path}?page={}", page - 1)
        } else {
            String::new()
        },
        next: if page < last {
            format!("{path}?page={}", page + 1)
        } else {
            String::new()
        },
    })
}

/// The "Public profile" tab: the member's own home page has no separate page and redirects to the public personal
/// space.
async fn public_profile(PageState(state): PageState) -> Response {
    let Some(session) = state.viewer.session.as_ref() else {
        return auth::redirect("/login?return_to=%2Fmy%2Fpublic");
    };
    auth::redirect(&crate::urls::space(session.member.uid))
}

#[derive(Template)]
#[template(path = "my/messages.html")]
struct MessagesTemplate<'a> {
    p: PageMeta<'a>,
    frame: Frame,
    rows: Vec<crate::messages::Conversation>,
    total: i64,
    previous: String,
    next: String,
}

struct MessageView {
    mine: bool,
    body: String,
    time: String,
}

struct ThreadView {
    thread_id: i64,
    counterpart: String,
    counterpart_state: String,
    messages: Vec<MessageView>,
}

#[derive(Template)]
#[template(path = "my/conversation.html")]
struct ConversationTemplate<'a> {
    p: PageMeta<'a>,
    frame: Frame,
    thread: ThreadView,
    /// Why a reply cannot be sent to the active counterpart (`posting::message_refusal`); empty shows the form.
    refusal: &'static str,
    request_key: String,
    message: String,
    body: String,
    previous: String,
    next: String,
}

#[derive(Template)]
#[template(path = "my/message_new.html")]
struct ComposeTemplate<'a> {
    p: PageMeta<'a>,
    frame: Frame,
    request_key: String,
    message: String,
    username: String,
    body: String,
    addressed: Option<Addressed>,
}

/// The member a new message was started for (`?to=`): the form carries their uid and the name it was prefilled with,
/// so a submission that left the name alone is checked against that member (`compose`).
struct Addressed {
    uid: i64,
    name: String,
}

/// What the new message form is rendered with.
#[derive(Default)]
struct ComposeDraft {
    message: String,
    username: String,
    body: String,
    addressed: Option<Addressed>,
    /// The request key of the submission being refilled; empty generates a new one.
    request_key: String,
}

#[derive(Deserialize)]
struct ReplyForm {
    _csrf: String,
    request_key: String,
    body: String,
}
crate::router::extract::csrf_forms!(ReplyForm);

#[derive(Deserialize)]
struct ComposeForm {
    _csrf: String,
    request_key: String,
    username: String,
    body: String,
    /// The uid and name of [`Addressed`], present when the form was prefilled from `?to=`.
    #[serde(default)]
    to: Option<i64>,
    #[serde(default)]
    to_name: String,
}
crate::router::extract::csrf_forms!(ComposeForm);

fn message_time(dateline: i64) -> String {
    crate::clock::datetime(dateline, &crate::clock::current())
}

#[allow(clippy::result_large_err)]
fn signed_in(state: &AppState, path: &str) -> Result<crate::identity::Actor, Response> {
    state
        .viewer
        .session
        .as_ref()
        .map(|session| session.member.actor())
        .ok_or_else(|| {
            auth::redirect(&format!(
                "/login?return_to={}",
                url::form_urlencoded::byte_serialize(path.as_bytes()).collect::<String>()
            ))
        })
}

async fn messages(PageState(state): PageState, Query(query): Query<ListQuery>) -> Response {
    let actor = match signed_in(&state, "/my/messages") {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    let page = query.page.unwrap_or(1);
    let (rows, total) = match crate::messages::inbox(
        &state.db,
        actor.uid,
        page,
        crate::messages::PAGE_SIZE,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => return message_error(&state, error),
    };
    let mut p = meta(
        &state,
        crate::i18n::current().home_card_messages().into(),
        crate::i18n::current().my_description_messages().into(),
        "/my/messages",
        "",
        "",
    );
    if let Some(error) = fill_shell(&state, &mut p).await {
        return error;
    }
    let last = (total as usize).div_ceil(crate::messages::PAGE_SIZE).max(1);
    render(&MessagesTemplate {
        p,
        frame: frame("messages"),
        rows,
        total,
        previous: if page > 1 {
            format!("/my/messages?page={}", page - 1)
        } else {
            String::new()
        },
        next: if page < last {
            format!("/my/messages?page={}", page + 1)
        } else {
            String::new()
        },
    })
}

fn message_error(state: &AppState, error: crate::content::writes::ContentWriteError) -> Response {
    let (status, message) = crate::router::posting::write_error(error);
    error_page(state, status, &message)
}

/// The request key a refilled form carries: the submitted one while it is still free (`messages::keeps_request_key`),
/// so sending the refilled form cannot deliver a second copy of a message that did go out; otherwise a new one.
fn refill_key(
    submitted: &str,
    error: Option<&crate::content::writes::ContentWriteError>,
) -> String {
    if crate::identity::valid_request_key(submitted)
        && error.is_none_or(crate::messages::keeps_request_key)
    {
        submitted.to_owned()
    } else {
        crate::identity::random_token()
    }
}

/// `request_key` is the key of the reply being refilled ([`refill_key`]); empty generates a new one.
#[allow(clippy::too_many_arguments)]
async fn conversation_view(
    state: &AppState,
    actor: crate::identity::Actor,
    id: i64,
    page: Option<usize>,
    message: String,
    body: String,
    request_key: String,
    status: StatusCode,
) -> Response {
    let thread = match crate::messages::conversation(
        &state.db,
        actor.uid,
        id,
        page,
        crate::messages::PAGE_SIZE,
    )
    .await
    {
        Ok(thread) => thread,
        Err(error) => return message_error(state, error),
    };
    let mut p = meta(
        state,
        crate::i18n::current().home_card_messages().into(),
        crate::i18n::current().my_description_messages().into(),
        &format!("/my/messages/{id}"),
        "",
        "",
    );
    if let Some(error) = fill_shell(state, &mut p).await {
        return error;
    }
    let refusal = match (&state.viewer.session, thread.counterpart_state.as_str()) {
        (Some(session), "active") => match crate::router::posting::message_refusal(
            state,
            &session.member,
            thread.counterpart_uid,
            true,
        )
        .await
        {
            Ok(refusal) => refusal,
            Err(error) => return database_error(state, error),
        },
        _ => "",
    };
    let last = (thread.total as usize)
        .div_ceil(crate::messages::PAGE_SIZE)
        .max(1);
    let page = thread.page;
    let view = ThreadView {
        thread_id: thread.thread_id,
        counterpart: thread.counterpart,
        counterpart_state: thread.counterpart_state,
        messages: thread
            .messages
            .into_iter()
            .map(|m| MessageView {
                mine: m.mine,
                body: m.body,
                time: message_time(m.dateline),
            })
            .collect(),
    };
    let mut response = render(&ConversationTemplate {
        p,
        frame: frame("messages"),
        thread: view,
        refusal,
        request_key: if request_key.is_empty() {
            crate::identity::random_token()
        } else {
            request_key
        },
        message,
        body,
        previous: if page > 1 {
            format!("/my/messages/{id}?page={}", page - 1)
        } else {
            String::new()
        },
        next: if page < last {
            format!("/my/messages/{id}?page={}", page + 1)
        } else {
            String::new()
        },
    });
    *response.status_mut() = status;
    response
}

async fn conversation(
    PageState(state): PageState,
    AxumPath(id): AxumPath<i64>,
    Query(query): Query<ListQuery>,
) -> Response {
    let actor = match signed_in(&state, &format!("/my/messages/{id}")) {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    conversation_view(
        &state,
        actor,
        id,
        query.page,
        String::new(),
        String::new(),
        String::new(),
        StatusCode::OK,
    )
    .await
}

async fn reply(
    PageState(state): PageState,
    AxumPath(id): AxumPath<i64>,
    CsrfForm(form): CsrfForm<ReplyForm>,
) -> Response {
    let actor = match signed_in(&state, &format!("/my/messages/{id}")) {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    // Conversation ownership is decided by the member's own row state; a conversation that is not the member's is
    // treated as nonexistent.
    let target = match crate::messages::counterpart(&state.db, actor.uid, id).await {
        Ok(target) => target,
        Err(error) => return message_error(&state, error),
    };
    match message_send_allowed(&state, actor.uid, target).await {
        Ok(true) => {}
        Ok(false) => {
            return conversation_view(
                &state,
                actor,
                id,
                None,
                crate::i18n::current().my_error_message_rate().into(),
                form.body,
                refill_key(&form.request_key, None),
                StatusCode::TOO_MANY_REQUESTS,
            )
            .await;
        }
        Err(unavailable) => return unavailable,
    }
    match crate::messages::send(&state.db, actor, target, &form.body, &form.request_key).await {
        Ok(_) => auth::redirect(&format!("/my/messages/{id}")),
        Err(error) => {
            let key = refill_key(&form.request_key, Some(&error));
            let (status, message) = crate::router::posting::write_error(error);
            conversation_view(&state, actor, id, None, message, form.body, key, status).await
        }
    }
}

#[derive(Default, Deserialize)]
struct ComposeQuery {
    /// The other party's uid brought by the "Messages" entry on a member's home page. It prefills the username, and
    /// the form carries it along ([`Addressed`]) so the name cannot quietly lead to someone else on submission.
    to: Option<i64>,
}

async fn compose_page(
    PageState(state): PageState,
    query: Result<Query<ComposeQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    // A malformed parameter is not an error and is treated as absent: this is only a prefill.
    let to = query
        .ok()
        .and_then(|Query(query)| query.to)
        .filter(|uid| *uid > 0);
    // Signing in again comes back to the same recipient.
    let path = to.map_or_else(
        || "/my/messages/new".to_owned(),
        |uid| format!("/my/messages/new?to={uid}"),
    );
    let actor = match signed_in(&state, &path) {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    let addressed = match to.filter(|uid| *uid != actor.uid) {
        Some(uid) => match crate::messages::recipient_name(&state.db, uid).await {
            Ok(name) => name.map(|name| Addressed { uid, name }),
            Err(error) => return database_error(&state, error),
        },
        None => None,
    };
    compose_view(
        &state,
        ComposeDraft {
            username: addressed
                .as_ref()
                .map(|addressed| addressed.name.clone())
                .unwrap_or_default(),
            addressed,
            ..ComposeDraft::default()
        },
        StatusCode::OK,
    )
    .await
}

async fn compose_view(state: &AppState, draft: ComposeDraft, status: StatusCode) -> Response {
    let mut p = meta(
        state,
        crate::i18n::current().my_compose_title().into(),
        crate::i18n::current().my_compose_description().into(),
        "/my/messages/new",
        "",
        "",
    );
    if let Some(error) = fill_shell(state, &mut p).await {
        return error;
    }
    let mut response = render(&ComposeTemplate {
        p,
        frame: frame("messages"),
        request_key: if draft.request_key.is_empty() {
            crate::identity::random_token()
        } else {
            draft.request_key
        },
        message: draft.message,
        username: draft.username,
        body: draft.body,
        addressed: draft.addressed,
    });
    *response.status_mut() = status;
    response
}

async fn compose(PageState(state): PageState, CsrfForm(form): CsrfForm<ComposeForm>) -> Response {
    let actor = match signed_in(&state, "/my/messages/new") {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    // A refilled form keeps the member it was started for, as long as its name is still the one prefilled.
    let addressed = form.to.filter(|uid| *uid > 0).map(|uid| Addressed {
        uid,
        name: form.to_name.clone(),
    });
    let refill =
        |message: String, error: Option<&crate::content::writes::ContentWriteError>| ComposeDraft {
            message,
            username: form.username.clone(),
            body: form.body.clone(),
            addressed: addressed.as_ref().map(|addressed| Addressed {
                uid: addressed.uid,
                name: addressed.name.clone(),
            }),
            request_key: refill_key(&form.request_key, error),
        };
    let target = match crate::messages::find_member(&state.db, &form.username).await {
        Ok(uid) => uid,
        Err(error) => {
            let draft = refill(String::new(), Some(&error));
            let (status, message) = crate::router::posting::write_error(error);
            return compose_view(&state, ComposeDraft { message, ..draft }, status).await;
        }
    };
    // The name was prefilled for one member and left as it was, yet it now leads to another: that member changed
    // their name and someone else took the old one. Ask instead of delivering to the new owner; the field shows the
    // intended member's current name when they can still receive messages.
    if let Some(intended) = addressed.as_ref().filter(|addressed| {
        crate::messages::normalized_username(&form.username)
            == crate::messages::normalized_username(&addressed.name)
    }) && intended.uid != target
    {
        let current = match crate::messages::recipient_name(&state.db, intended.uid).await {
            Ok(name) => name,
            Err(error) => return database_error(&state, error),
        };
        let uid = intended.uid;
        return compose_view(
            &state,
            ComposeDraft {
                username: current.clone().unwrap_or_default(),
                addressed: current.map(|name| Addressed { uid, name }),
                ..refill(
                    crate::i18n::current().my_error_recipient_renamed().into(),
                    None,
                )
            },
            StatusCode::CONFLICT,
        )
        .await;
    }
    match message_send_allowed(&state, actor.uid, target).await {
        Ok(true) => {}
        Ok(false) => {
            return compose_view(
                &state,
                refill(crate::i18n::current().my_error_message_rate().into(), None),
                StatusCode::TOO_MANY_REQUESTS,
            )
            .await;
        }
        Err(unavailable) => return unavailable,
    }
    match crate::messages::send(&state.db, actor, target, &form.body, &form.request_key).await {
        Ok(thread_id) => auth::redirect(&format!("/my/messages/{thread_id}")),
        Err(error) => {
            let draft = refill(String::new(), Some(&error));
            let (status, message) = crate::router::posting::write_error(error);
            compose_view(&state, ComposeDraft { message, ..draft }, status).await
        }
    }
}

/// New conversations and replies share the window: the per-account per-minute count and the per-account per-recipient
/// per-minute count are the admin settings `settings.limit.message_per_minute` and `message_peer_per_minute` (window
/// of 1 minute, a constant).
/// Only the sender is counted, so the other party cannot use up my sending quota by messaging me.
///
/// This only decides and produces no response: rejected requests are refilled as-is by their own form pages, so the
/// body is not lost to rate limiting. `Err` is the 503 when Redis is enabled but unavailable.
async fn message_send_allowed(state: &AppState, uid: i64, target: i64) -> Result<bool, Response> {
    let limit = &state.frame.settings.limit;
    let count = |value: i64| u32::try_from(value).unwrap_or(0);
    state
        .limit(&[
            Rule::new("message-sender", uid, count(limit.message_per_minute), 60),
            Rule::new(
                "message-peer",
                format!("{uid}:{target}"),
                count(limit.message_peer_per_minute),
                60,
            ),
        ])
        .await
}

async fn not_found(PageState(state): PageState) -> Response {
    error_page(
        &state,
        StatusCode::NOT_FOUND,
        crate::i18n::current().common_error_page_not_found(),
    )
}
