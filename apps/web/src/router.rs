mod activities;
mod administration;
mod api;
mod appeals;
mod attachments;
mod auth;
mod blocks;
mod colors;
mod custom_style;
mod data_sources;
mod directory;
mod domains;
mod editing;
mod extract;
mod feed_settings;
mod forum_filters;
mod frame;
mod highlights;
mod honors;
mod interaction;
mod languages;
mod leaderboard;
mod login_methods;
mod mail;
mod manage;
mod manage_settings;
mod media_routes;
mod member_search;
mod moderation;
pub mod my;
mod nav_fit;
mod organization;
mod page_data;
mod posting;
mod privacy;
mod reading;
mod restorations;
mod site;
mod skins;
mod social;
mod syndication;
mod updates;

use std::{path::Path, sync::Arc};

use askama::Template;
use axum::{
    Router,
    extract::{Extension, OriginalUri, Query, State},
    http::{HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
    routing::get,
};
use serde::Deserialize;
use tower_http::{set_header::SetResponseHeaderLayer, trace::TraceLayer};

use crate::{
    config::Config,
    content::{self, Post, Thread},
    db::Database,
    identity::Identity,
    urls::{self, PageRoute, Sort},
};

/// Cloning happens in every page request (`auth::page_state`), so every field except `viewer` is either a shared
/// handle itself or sits behind an `Arc`: `config` no longer changes after startup, and `frame` is replaced as a
/// whole on every request.
#[derive(Clone)]
pub(crate) struct AppState {
    config: Arc<Config>,
    pub(crate) db: Database,
    cache: crate::cache::Cache,
    /// The process-wide global cache (site settings, policy documents, brand images, forums and the other blocks of
    /// `cache::Block`). Save functions receive it explicitly, clear this process after commit and broadcast
    /// (docs/settings-and-caching.md, "The process cache").
    pub(crate) global: crate::cache::GlobalCache,
    identity: Identity,
    media: crate::media::Media,
    mail: crate::mail::Mailer,
    viewer: auth::Viewer,
    /// Rate limiting: in-process counters when Redis is off, the Redis sliding log when it is on
    /// ([`crate::rate_limit`]).
    limits: Arc<crate::rate_limit::Limits>,
    frame: Arc<frame::SiteFrame>,
    asset_version: Arc<str>,
    /// Update check and one-click upgrade state of this process.
    updater: crate::updates::Updater,
    /// The instance directory: binaries, `config/`, `data/` (one-click upgrade replaces the binaries here).
    instance: crate::updates::Paths,
    /// `skins/` under the instance root: the instance directory of skins (docs/skins.md, "Sources").
    skins_dir: Arc<Path>,
}

impl AppState {
    /// This visitor's readable scope: the member id comes from the authenticated session, the forum ids from the
    /// forum cache in the page frame. Only used in handlers that loaded the page frame (taken via `Page`); entry
    /// points that skip the page frame read the process forum cache themselves.
    pub(crate) fn readable(&self) -> content::Readable<'_> {
        debug_assert!(
            self.frame.loaded,
            "the page frame must be loaded before the readable scope"
        );
        content::Readable {
            uid: self.viewer.session.as_ref().map(|s| s.member.uid),
            forums: &self.frame.readable_fids,
        }
    }

    /// The site address for outward links: canonical, sitemap, robots, absolute addresses returned by the API.
    ///
    /// It has nothing to do with transport security: the cookie's Secure flag and the CSRF same-origin check are
    /// derived from this connection and do not read it. A wrong address only makes links point to the wrong
    /// place; it never stops forms from submitting (see `secure_transport` and `csrf_ok` in `router/auth.rs`).
    ///
    /// When the administrator filled in the forum URL in the admin area, it is used; when left empty it is derived
    /// from this request's `Host` (computed when the page frame loads).
    ///
    /// It takes the value from this request's page frame, so it only works for pages that go through `Page`;
    /// `robots`, `sitemap`, feeds and the member API skip the page frame and call
    /// [`crate::runtime::site_origin`] themselves.
    fn site_url(&self) -> &str {
        &self.frame.site_url
    }

    /// This request's Redis cache lifetime, taken from the site settings in the page frame.
    fn lifetimes(&self) -> crate::cache::Lifetimes {
        crate::cache::Lifetimes::from_settings(&self.frame.settings)
    }

    /// A Redis cache handle carrying this request's lifetime.
    fn timed_cache(&self) -> crate::cache::TimedCache {
        self.cache.timed(self.lifetimes())
    }

    /// Forum statistics: process → Redis → computed live, shared by the admin forum list and `/forums`.
    async fn forum_statistics(&self) -> anyhow::Result<content::ForumStatistics> {
        content::layered_forum_statistics(
            &self.db,
            &self.global,
            &self.timed_cache(),
            crate::identity::now(),
            self.frame.settings.time.offset_minutes,
        )
        .await
    }

    /// Member card: the check-in rules come from the process-wide global cache; the rest is read from the database
    /// as usual.
    async fn member_summary(
        &self,
        actor: crate::identity::Actor,
    ) -> anyhow::Result<content::home::MemberSummary> {
        let rules = self.global.credit_rules(&self.db, self.cache_ttl()).await?;
        content::home::member_summary(
            &self.db,
            actor,
            &rules,
            self.frame.settings.time.offset_minutes,
        )
        .await
    }

    /// This request's process cache lifetime (seconds); 0 means no caching. Display data outside the page frame
    /// (policy documents, check-in rules, friend links) uses it to decide whether to read the process cache.
    fn cache_ttl(&self) -> u64 {
        self.frame.settings.cache_ttl()
    }

    /// Checks and counts the rate limit; `Ok(true)` lets the request through. Several buckets short-circuit in
    /// order ([`crate::rate_limit`]).
    /// `Err` is a ready error response: 503 when Redis is enabled but unavailable; the request is not let through
    /// and there is no fallback to in-process counting.
    async fn limit(&self, rules: &[crate::rate_limit::Rule]) -> Result<bool, Response> {
        self.limits
            .allow(rules)
            .await
            .map_err(|error| database_error(self, error))
    }

    /// Only looks, without counting: `Ok(true)` when any bucket is full. `Err` as in [`AppState::limit`].
    async fn limit_blocked(&self, rules: &[crate::rate_limit::Rule]) -> Result<bool, Response> {
        self.limits
            .blocked(rules)
            .await
            .map_err(|error| database_error(self, error))
    }
}

/// The cache version of the static assets, computed from the file contents once at startup.
///
/// Templates write it as `?v=` on every asset address, the largest files (composer.js, fonts.css and the two vendor
/// scripts) included, so a changed file is never served from an old cache entry. When a file cannot be read it falls
/// back to the process start time: better to invalidate the cache once more than to serve old assets.
fn asset_version(files: &crate::static_files::StaticFiles) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    let mut readable = false;
    for name in [
        "css/forum.css",
        "css/fonts.css",
        "css/debug.css",
        "js/site.js",
        "js/navigation.js",
        "js/composer.js",
        "js/debug.js",
        "js/vendor/htmx.min.js",
        "js/vendor/head-support.js",
    ] {
        match files.read(name) {
            Some(bytes) => {
                readable = true;
                hasher.update(name.as_bytes());
                hasher.update(bytes);
            }
            None => tracing::warn!(%name, "asset missing; version uses start time"),
        }
    }
    // The script catalogs of the translations (`/static/i18n/<language>.js`) are generated, not files.
    for lang in crate::i18n::Lang::all() {
        hasher.update(lang.tag().as_bytes());
        hasher.update(crate::i18n::script(lang).as_bytes());
    }
    if !readable {
        return std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs().to_string())
            .unwrap_or_else(|_| "0".into());
    }
    hasher
        .finalize()
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn build(config: Config, root: &Path, db: Database) -> Router {
    let cache = crate::cache::Cache::new(&config.redis).expect("Redis configuration must be valid");
    build_with_cache(config, root, db, cache)
}
/// A single application without a subscription: saving only clears this process, and publishes as usual when Redis
/// is enabled. Used by the router tests.
pub fn build_with_cache(
    config: Config,
    root: &Path,
    db: Database,
    cache: crate::cache::Cache,
) -> Router {
    let global = crate::cache::GlobalCache::new(cache.clone());
    build_with_global(config, root, db, cache, global)
}
/// For the real startup: `global` is built by [`crate::cache::GlobalCache::start`] (already subscribed when Redis
/// is enabled). When tests simulate several processes, they build one handle per application sharing the same
/// Redis namespace.
pub fn build_with_global(
    config: Config,
    root: &Path,
    db: Database,
    cache: crate::cache::Cache,
    global: crate::cache::GlobalCache,
) -> Router {
    build_with_updater(
        config,
        root,
        db,
        cache,
        global,
        crate::updates::Updater::default(),
    )
}
/// For tests: [`build_with_global`] with an update state the test holds, so it can show the update page as it looks
/// while an upgrade runs ([`crate::updates::Updater::set_running`]).
#[doc(hidden)]
pub fn build_with_updater(
    config: Config,
    root: &Path,
    db: Database,
    cache: crate::cache::Cache,
    global: crate::cache::GlobalCache,
    updater: crate::updates::Updater,
) -> Router {
    // The debug layer is always attached; each request decides by `settings.debug.panel` whether to collect
    // (docs/settings-and-caching.md, "Profiling").
    let debug_state = crate::debug::DebugState {
        database: db.clone(),
        redis_enabled: config.redis.enabled,
        global: global.clone(),
    };
    let static_files = crate::static_files::StaticFiles::for_root(root);
    let upload_limit = upload_body_limit(&config.media);
    let state = Arc::new(AppState {
        asset_version: asset_version(&static_files).into(),
        identity: Identity::new(
            db.clone(),
            config.identity.clone(),
            root.to_owned(),
            global.clone(),
        )
        .with_common(config.common.clone())
        .with_oauth(config.oauth.clone()),
        media: crate::media::Media::new(db.clone(), config.media.clone(), root.to_owned())
            .with_global(global.clone()),
        mail: crate::mail::Mailer::new(
            db.clone(),
            config.common.clone(),
            root.join(&config.identity.local_directory).join("mail"),
            global.clone(),
        ),
        config: Arc::new(config),
        db,
        cache: cache.clone(),
        global,
        viewer: auth::Viewer::default(),
        limits: Arc::new(crate::rate_limit::Limits::new(cache.clone())),
        frame: Arc::new(frame::SiteFrame::default()),
        updater,
        instance: crate::updates::Paths::for_process(root),
        skins_dir: root.join("skins").into(),
    });
    let router = static_files
        .mount(auth::routes(upload_limit))
        .merge(api::routes())
        .merge(posting::routes())
        .merge(activities::routes())
        .merge(media_routes::routes(upload_limit))
        .merge(editing::routes())
        .merge(manage::routes())
        .merge(moderation::routes())
        .merge(restorations::routes())
        .merge(appeals::routes())
        .merge(mail::routes())
        .merge(interaction::routes())
        .merge(administration::routes())
        .merge(manage_settings::routes())
        .merge(organization::routes())
        .merge(domains::routes())
        .merge(languages::routes())
        .merge(login_methods::routes())
        .merge(privacy::routes())
        .merge(social::routes())
        .merge(reading::routes())
        .merge(custom_style::routes())
        .merge(skins::routes())
        .merge(colors::routes())
        .merge(forum_filters::routes())
        .merge(feed_settings::routes())
        .merge(site::routes(upload_limit))
        .merge(syndication::routes())
        .merge(my::routes())
        .merge(attachments::routes())
        .merge(member_search::routes())
        .merge(leaderboard::routes())
        .merge(directory::routes())
        .merge(honors::routes())
        .merge(updates::routes())
        .route("/", get(browse))
        .route("/search", get(browse))
        .route("/{segment}", get(browse))
        .route("/space/{uid}", get(posting::space_page))
        .route("/space/{uid}/{collection}", get(posting::space_page))
        .route("/healthz", get(healthz))
        .route(
            "/data/v1/{source}",
            axum::routing::any(data_sources::endpoint),
        )
        .fallback(not_found)
        .layer(TraceLayer::new_for_http().make_span_with(|request: &axum::extract::Request| {
            // Token-bearing URLs must not enter ordinary request logs, even at debug level.
            let uri = if crate::urls::token_page(request.uri().path()) {
                request.uri().path().to_owned()
            } else if request.uri().path().starts_with(crate::urls::PRIVATE_FEED_PREFIX) {
                crate::urls::loggable_path(request.uri().path()).into_owned()
            } else {
                request.uri().to_string()
            };
            tracing::debug_span!("request", method = %request.method(), uri = %uri, version = ?request.version())
        }))
        .layer(axum::middleware::from_fn_with_state(state.clone(), auth::context))
        .with_state(state.clone());
    let router = crate::cache::protect(router, cache)
        .layer(axum::middleware::from_fn(crate::metrics::middleware));
    let router = router.layer(axum::middleware::from_fn_with_state(
        debug_state,
        crate::debug::middleware,
    ));
    // Site resolution and prefix stripping must happen before routing: wrap an empty router that has only a
    // fallback service, attach the entry layer to that fallback, and hand the rewritten address to the real router
    // for matching. Outwardly it is still a `Router`.
    Router::new()
        .fallback_service(router)
        .layer(axum::middleware::from_fn_with_state(
            state,
            crate::site::entry,
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(auth::FALLBACK_CSP),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
}

/// When site resolution fails there is no page frame yet. Pages and the member API each use their existing error
/// format.
pub(crate) fn site_error(
    state: &AppState,
    path: &str,
    error: crate::identity::IdentityError,
) -> Response {
    let mut response = if path
        .strip_prefix(api::PREFIX)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    {
        api::ApiError::from(error).into_response()
    } else {
        auth::identity_error(state, error)
    };
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    // The site is not resolved yet and the original address may carry a verification or feed token; the error
    // page does not send it when loading its styles.
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    auth::token_page_response(response, urls::token_page(path))
}

#[derive(Default, Deserialize)]
struct BrowseQuery {
    /// When absent, the home page and forums use the visitor's default sort (reading preferences); search always
    /// uses latest reply.
    #[serde(default)]
    sort: Option<Sort>,
    #[serde(default)]
    q: String,
    #[serde(default)]
    view: String,
    category: Option<i64>,
    sub_root: Option<u64>,
    sub_page: Option<usize>,
    page: Option<usize>,
    reply_to: Option<u64>,
    quote: Option<i64>,
}

/// The limit of a thread search term (UTF-8 bytes, counted after trimming leading and trailing whitespace); beyond
/// it the response is 400 "The search term is too long".
const SEARCH_MAX_BYTES: usize = 300;

struct PageMeta<'a> {
    /// The source of the footer figures. Evaluated at the end of the template, so the time includes rendering the
    /// whole body.
    metrics: Option<std::sync::Arc<crate::metrics::RequestMetrics>>,
    /// The backend name before the footer's SQL count: MySQL or SQLite.
    database: &'static str,
    /// Without Redis (`[redis] enabled = false`) the footer leaves out the Redis count, as the debug panel leaves out
    /// its Redis section: a site that never talks to Redis has nothing to count there.
    redis_enabled: bool,
    frame: &'a frame::SiteFrame,
    /// This request's interface language: templates write `p.t.frame_more()` (docs/architecture.md, "Languages").
    t: crate::i18n::Lang,
    /// The header brand text: the setting `settings.site.brand_text` (empty shows the product name,
    /// docs/architecture.md, "Languages") for the main domain and unregistered Hosts, the site's own name for other
    /// domains.
    brand_name: String,
    title: String,
    description: String,
    canonical: String,
    active_slug: String,
    query: String,
    authenticated: bool,
    member_summary: Option<content::home::MemberSummary>,
    sidebar: Option<SidebarTopics>,
    /// The private message dot on the personal center card. That card shows no count, so only whether there are
    /// any is stored here.
    has_unread_messages: bool,
    username: String,
    avatar: AvatarView,
    csrf: String,
    /// Signed in, account in good standing, not an administrator, but not meeting the participation requirement in
    /// effect for this request: the header notice bar, the reply box and the account
    /// menu use it to prompt verifying the email or binding a phone number.
    needs_verification: bool,
    /// Has an email that is not verified (used by the resend form on the verify-email page; unrelated to the
    /// participation requirement).
    email_unverified: bool,
    /// The participation requirement in effect for this request: whether the notice bar says "verify email" or
    /// "bind phone number".
    participation: crate::login::Participation,
    needs_composer: bool,
    asset_version: Arc<str>,
    debug_enabled: bool,
    uid: i64,
    is_admin: bool,
    is_staff: bool,
    /// The role text on the member card (administrator, global moderator, moderator, trainee moderator, reviewer,
    /// member).
    role_label: &'static str,
    nonce: String,
    published: String,
    unread_notifications: i64,
    navigation: Vec<NavigationLink>,
    /// "All forums" at the end of the header navigation; it is the current item on `/forums`.
    directory: NavigationLink,
    indexable: bool,
    /// Site verification meta (`settings.seo.verification`), output only on page 1 of the home page `/`,
    /// regardless of the indexing switch; empty on other pages.
    verification: Vec<(String, String)>,
    logo: String,
    favicon: String,
    app_icon: String,
    share_image: String,
    /// Width and height of the original share image; when empty, `og:image:width/height` is not output.
    share_width: Option<i64>,
    share_height: Option<i64>,
    /// `og:type`: `article` for thread details, `profile` for indexable profile pages, `website` for other pages.
    og_type: &'static str,
    /// `profile:username` when `og:type=profile`; empty on other pages.
    profile_username: String,
    /// Structured data, one `<script type="application/ld+json">` block per item, already escaped for a data block;
    /// nothing is output when empty. Thread pages have `DiscussionForumPosting` then `BreadcrumbList`, forum pages
    /// `BreadcrumbList`, indexable profile pages `ProfilePage`.
    json_ld: Vec<String>,
    /// The page data block (docs/skins.md, "Page data"): the kind of page and its ids; handlers of the home, board,
    /// search, thread and member pages fill it, every other page is `other`.
    page_data: page_data::PageData,
    /// Feeds for `<link rel="alternate">` autodiscovery, given only on the home page and forum lists.
    feeds: Vec<syndication::FeedLink>,
    read_only_message: String,
    /// The notice muted members see on every page (with the end time); empty when not muted (docs/roles.md,
    /// "Mutes").
    mute_message: String,
    /// Two-level admin navigation; None for non-moderators or outside /manage, and `layout/manage.html` falls back
    /// to a single column.
    manage: Option<manage::catalog::ManageFrame>,
    /// `<html data-avatar-shape>`: the skin's fixed shape, or the member's choice of round avatars when the skin leaves
    /// it to them (which only changes what they see themselves); `None` for the default rounded corners.
    avatar_shape: Option<&'static str>,
    /// The skin and palette of this page (docs/skins.md, "Choosing a skin"). `/manage/skins` always uses the built-in
    /// default, so a skin that breaks the page can still be switched back there.
    skin: Arc<crate::skins::Skin>,
    palette: String,
    /// While the administrator previews a skin or colors: the text of the bar at the top of every page.
    skin_preview: Option<String>,
    /// The address of the member's personal style sheet; empty when there is none, it is not the member's own, or
    /// this page does not apply it.
    custom_style: String,
    /// The rendered blocks of the page's places (docs/skins.md, "Places and blocks"); empty on pages without places.
    places: blocks::Placed,
}

/// The friend links card in the home page sidebar: up to 12 name texts plus the "All links" and "Apply for a link"
/// entries.
struct FriendLinksCard {
    links: Vec<crate::links::HomeLink>,
    applications_open: bool,
}

/// The popular and latest cards in the right column. The home page, forum pages and the personal center pages share
/// one sidebar template, so the data hangs on the page metadata instead of being declared again in the template
/// structs of seventeen pages.
struct SidebarTopics {
    hot: Vec<Thread>,
    latest: Vec<Thread>,
}

#[derive(Clone, Debug)]
struct NavigationLink {
    label: String,
    href: String,
    active: bool,
    /// From which viewport width tier the item stays in the header; 0 means only in "More"; see `nav_fit`.
    fit: u16,
}

/// One tab of the child forum tabs.
struct ForumTab {
    label: String,
    href: String,
    active: bool,
}

struct Tab {
    label: &'static str,
    href: String,
    active: bool,
}
struct PageLink {
    number: usize,
    href: String,
    active: bool,
}

/// The pagination bar lists only the first page, the last page and two pages on either side of the current page,
/// in ascending order; the template adds ellipses between non-adjacent page numbers.
/// Forums with thousands of pages do not output thousands of links.
fn page_window(current: usize, last: usize) -> impl Iterator<Item = usize> {
    let last = last.max(1);
    let current = current.clamp(1, last);
    let mut pages: Vec<usize> = Vec::new();
    for number in (current.saturating_sub(2).max(1)..=current.saturating_add(2).min(last))
        .chain([1, last])
        .collect::<std::collections::BTreeSet<_>>()
    {
        // When only one page is skipped, list that page directly: the ellipsis takes as much room as it does, yet
        // needs an extra click to see it.
        if let Some(&previous) = pages.last()
            && number == previous + 2
        {
            pages.push(previous + 1);
        }
        pages.push(number);
    }
    pages.into_iter()
}
struct Row<'a> {
    thread: &'a Thread,
    /// The title link; reading preferences decide whether it goes to the first page or the latest reply, and
    /// two-level or flat.
    href: String,
    avatar: AvatarView,
    /// The time shown in the list (the last reply moment), following the relative-time setting.
    time: String,
    /// The hover tooltip of the time: the absolute time with year.
    time_title: String,
    topic_pages: Vec<PageLink>,
    thumbs: Vec<ThumbView>,
    /// A thread from a child forum in a parent forum's roll-up list: the row names that child forum and links to
    /// its page.
    child_forum: bool,
    /// Name and address of the thread's forum, taken by `fid` from the page frame's forum cache (the thread row has
    /// no copy of them).
    forum_name: String,
    forum_slug: String,
}

/// One row of a list: shared by the home page, forum pages and the cross-forum pin block.
/// The third argument is this request's time style and the absolute style for hovering (see the caller).
fn list_row<'t>(
    state: &AppState,
    reading: &crate::reading::Reading,
    (format, absolute): (&crate::clock::TimeFormat, &crate::clock::TimeFormat),
    thread: &'t content::Thread,
) -> Row<'t> {
    Row {
        time: crate::clock::short_datetime(thread.updated as i64, format),
        time_title: crate::clock::datetime(thread.updated as i64, absolute),
        href: reading.title_href(thread.id, thread.last_reply_pid),
        topic_pages: topic_page_links(
            thread,
            reading.flat,
            urls::Paging::from_settings(&state.frame.settings),
        ),
        thumbs: list_thumbs(state, thread),
        child_forum: false,
        forum_name: content::forum_name(&state.frame.all_forums, thread.fid),
        forum_slug: content::forum_slug(&state.frame.all_forums, thread.fid),
        thread,
        // List avatars get their address by uid: the thread row has no avatar copy.
        avatar: list_avatar(
            thread.authorid.filter(|_| !thread.author_deleted),
            &thread.author,
        ),
    }
}

struct ThumbView {
    url: String,
    width: u32,
    height: u32,
}

/// Displayed height (pixels) and maximum width of list thumbnails; the thumbnails themselves are generated at twice
/// the size (`media/thumbnail.rs`).
const THUMB_DISPLAY_HEIGHT: i64 = 90;
const THUMB_DISPLAY_MAX_WIDTH: i64 = 210;

/// The thumbnails of a thread row: the static images in the first post in order of appearance; how many are shown is
/// set by the display setting `settings.display.list_thumbnails`, 0 shows none. 90 px high, width proportional and
/// capped; the address `/thumb/{tid}/{aid}` re-checks permission on the server against the current thread state.
/// Thumbnails are not clickable; to see the large image, open the thread.
fn list_thumbs(state: &AppState, thread: &content::Thread) -> Vec<ThumbView> {
    let shown = usize::try_from(state.frame.settings.display.list_thumbnails).unwrap_or(0);
    thread
        .thumbnails
        .iter()
        .take(shown)
        .map(|thumb| {
            let proportional = (thumb.width.max(1) * THUMB_DISPLAY_HEIGHT) / thumb.height.max(1);
            ThumbView {
                url: format!("/thumb/{}/{}", thread.id, thumb.aid),
                width: u32::try_from(proportional.clamp(1, THUMB_DISPLAY_MAX_WIDTH)).unwrap_or(1),
                height: u32::try_from(THUMB_DISPLAY_HEIGHT).unwrap_or(90),
            }
        })
        .collect()
}

struct AvatarView {
    url: String,
    label: String,
    variant: i64,
    profile_href: String,
    /// Image by uid (for lists): a text tile underneath with `/avatar/{uid}` on top; when there is no avatar that
    /// image is transparent and the text tile shows through, without JS.
    stacked: bool,
}

/// The number of background tiers of the text tile (`.avatar-0` to `.avatar-5`). The color is computed from the uid
/// and is the same everywhere (lists get avatars by uid and do not read the `avatar` column of the member table).
const AVATAR_VARIANTS: i64 = 6;

fn avatar_variant(uid: Option<i64>, fallback: i64) -> i64 {
    uid.filter(|uid| *uid > 0)
        .map_or(fallback, |uid| uid.rem_euclid(AVATAR_VARIANTS))
}

/// The avatar when the current avatar attachment is known: `/avatar/{uid}/{aid}` when there is one, otherwise the
/// text tile. `fallback` only decides the color when there is no member id (guests, deleted accounts).
fn avatar_view(uid: Option<i64>, aid: i64, fallback: i64, username: &str) -> AvatarView {
    AvatarView {
        url: uid
            .filter(|uid| *uid > 0 && aid > 0)
            .map(|uid| format!("/avatar/{uid}/{aid}"))
            .unwrap_or_default(),
        label: username.chars().take(2).collect(),
        variant: avatar_variant(uid, fallback),
        profile_href: uid.map(crate::urls::space).unwrap_or_default(),
        stacked: false,
    }
}

/// The avatar by uid: used by thread lists, the home page's newest users, thread floors and highlighted replies.
/// Without knowing whether the author has an avatar, a `/avatar/{uid}` image is laid over the text tile; the same
/// author has the same address everywhere, so the browser cache is shared. The data of these places goes into
/// caches, and changing an avatar does not bump the site-wide version, so no address with an aid is produced. When
/// the caller passes `None` (deleted author) there is only the text tile; note that thread list rows cannot
/// recognize deleted authors (`author_deleted` is only read by queries that fetch the first post body), so they
/// still produce `/avatar/{uid}`, which returns the transparent image.
fn list_avatar(uid: Option<i64>, username: &str) -> AvatarView {
    let uid = uid.filter(|uid| *uid > 0);
    AvatarView {
        url: uid.map(|uid| format!("/avatar/{uid}")).unwrap_or_default(),
        label: username.chars().take(2).collect(),
        variant: avatar_variant(uid, 0),
        profile_href: uid.map(crate::urls::space).unwrap_or_default(),
        stacked: uid.is_some(),
    }
}

/// The thread page numbers in a list row. With a flat-view preference, pagination counts all replies and carries
/// `?view=flat`, matching the title link; the floors per page are a setting ([`urls::Paging`]).
fn topic_page_links(thread: &Thread, flat: bool, paging: urls::Paging) -> Vec<PageLink> {
    let last = if flat {
        thread.reply_count
    } else {
        thread.root_replies
    }
    .div_ceil(paging.replies);
    (2..=last.min(4))
        .chain(last.saturating_sub(2).max(5)..=last)
        .map(|number| PageLink {
            number,
            href: urls::thread(thread.id, number, flat),
            active: false,
        })
        .collect()
}

#[derive(Template)]
#[template(path = "home/index.html")]
struct HomeTemplate<'a> {
    p: PageMeta<'a>,
    pinned_view: bool,
    /// Open list titles in a new tab (reading preference).
    title_new_tab: bool,
    /// The board's name on a board page, the search heading on search; empty on the home page. The page writes it as
    /// a heading for screen readers when its page head has no board-head block.
    heading: String,
    /// The search results' head (the board-head view without block hooks); search pages have no places in the
    /// page head.
    search_head: Option<String>,
    /// Child forum tabs: a parent forum page and its child forum pages share one row, and the first tab "All" is
    /// the parent forum; empty when there are no child forums.
    forum_tabs: Vec<ForumTab>,
    /// The breadcrumb "Home › parent forum" on child forum pages; empty for top-level forums,
    /// the home page and search.
    breadcrumb: Vec<(String, String)>,
    tabs: Vec<Tab>,
    categories: Vec<Tab>,
    /// Pins from elsewhere merged above the list on page 1 of a forum page; not counted in the total and not taking
    /// per-page slots.
    cross_pins: Vec<Row<'a>>,
    rows: Vec<Row<'a>>,
    pages: Vec<PageLink>,
    /// Only set on the thread search page: the "Threads / Members" scope tabs at the top.
    search_scope: Option<member_search::SearchScope>,
}

/// The current item of the header navigation (the same one also feeds the mobile drawer and the mobile forum bar):
/// the item whose address equals this page's forum `/{slug}`; when there is no such item and
/// this page's forum is a child forum, its parent forum is highlighted: child forum pages and thread pages inside
/// a child forum both count as the parent forum. When the navigation document names the child forum itself, a
/// match on it takes precedence and the parent forum is no longer highlighted as well.
fn highlighted(state: &AppState, slug: &str) -> Vec<NavigationLink> {
    let mut navigation = state.frame.navigation.clone();
    if slug.is_empty() {
        return navigation;
    }
    let own = format!("/{slug}");
    let target = if navigation.iter().any(|link| link.href == own) {
        Some(own)
    } else {
        state
            .frame
            .forum(slug)
            .and_then(|forum| state.frame.parent_of(forum))
            .map(|parent| format!("/{}", parent.slug))
    };
    if let Some(target) = target {
        for link in &mut navigation {
            link.active = link.href == target;
        }
    }
    navigation
}

fn meta<'a>(
    state: &'a AppState,
    title: String,
    description: String,
    path: &str,
    slug: &str,
    query: &str,
) -> PageMeta<'a> {
    let navigation = highlighted(state, slug);
    let session = state.viewer.session.as_ref();
    let is_admin = session.is_some_and(|s| s.member.is_admin);
    let is_staff = session.is_some_and(|s| s.member.is_staff());
    let skin = if path == "/manage/skins" || path.starts_with("/manage/skins/") {
        crate::skins::DEFAULT_SKIN.clone()
    } else {
        state.frame.skin.clone()
    };
    PageMeta {
        metrics: crate::metrics::current(),
        database: state.db.backend().label(),
        redis_enabled: state.cache.enabled(),
        navigation,
        directory: NavigationLink {
            active: path == "/forums",
            ..state.frame.directory.clone()
        },
        indexable: state.frame.settings.seo.indexing
            && state.viewer.session.is_none()
            && indexable_path(state, path),
        verification: if path == "/" {
            verification_metas(&state.frame.settings.seo.verification)
        } else {
            Vec::new()
        },
        logo: brand_path(state, "logo", "source", ""),
        favicon: brand_path(state, "favicon", "32", "/static/img/favicon-32.png"),
        app_icon: brand_path(state, "app_icon", "180", "/static/img/apple-touch-icon.png"),
        share_image: brand_path(state, "share", "source", ""),
        share_width: brand_size(state, "share").map(|(width, _)| width),
        share_height: brand_size(state, "share").map(|(_, height)| height),
        og_type: "website",
        profile_username: String::new(),
        json_ld: Vec::new(),
        page_data: page_data::PageData::default(),
        feeds: Vec::new(),
        read_only_message: if state.frame.settings.site.read_only {
            state
                .frame
                .settings
                .site
                .read_only_notice(crate::i18n::current())
        } else {
            String::new()
        },
        mute_message: match &state.viewer.session {
            Some(session) if session.member.muted() => {
                crate::moderation::muted_notice(crate::i18n::current(), session.member.mute_until)
            }
            _ => String::new(),
        },
        frame: &state.frame,
        t: crate::i18n::current(),
        brand_name: crate::site::other(|row| row.site_name.clone())
            .flatten()
            .unwrap_or_else(|| state.frame.settings.site.header_brand().to_owned()),
        title: format!("{title} - {}", state.frame.settings.site.display_name()),
        description,
        canonical: canonical_url(state, None, path),
        active_slug: slug.into(),
        query: query.into(),
        authenticated: state.viewer.session.is_some(),
        member_summary: None,
        sidebar: None,
        has_unread_messages: false,
        uid: state
            .viewer
            .session
            .as_ref()
            .map(|s| s.member.uid)
            .unwrap_or(0),
        is_admin,
        is_staff,
        role_label: match session {
            Some(s) if is_staff => s.member.role_label(),
            _ => crate::i18n::current().common_role_member(),
        },
        // The session snapshot is enough: Identity::session already re-reads the member row on every request and
        // checks the freeze and credential version, as fresh as a gate that queries the database again, so no
        // extra SQL here.
        manage: manage::catalog::frame(
            path,
            match &state.viewer.session {
                Some(session) if is_staff => session.member.adminid,
                _ => 0,
            },
        ),
        nonce: state.viewer.csp_nonce.clone(),
        published: state.viewer.published.clone(),
        unread_notifications: state.viewer.unread_notifications,
        username: state
            .viewer
            .session
            .as_ref()
            .map(|session| session.member.username.clone())
            .unwrap_or_default(),
        avatar: state
            .viewer
            .session
            .as_ref()
            .map(|s| {
                avatar_view(
                    Some(s.member.uid),
                    s.member.avatar_aid.unwrap_or(0),
                    s.member.avatar,
                    &s.member.username,
                )
            })
            .unwrap_or_else(|| {
                avatar_view(None, 0, 0, crate::i18n::current().common_guest_initial())
            }),
        csrf: state.viewer.csrf.clone(),
        needs_verification: state.viewer.session.as_ref().is_some_and(|session| {
            let member = &session.member;
            member.active() && !member.frozen && !member.can_contribute()
        }),
        email_unverified: state.viewer.session.as_ref().is_some_and(|session| {
            let member = &session.member;
            member.email.is_some() && !member.email_verified && !member.is_admin
        }),
        participation: crate::login::Participation::current(),
        // The editor bundle is far larger than the other scripts and is not loaded by default; pages that render
        // editor/form.html turn it on explicitly.
        needs_composer: false,
        asset_version: state.asset_version.clone(),
        debug_enabled: state.frame.settings.debug.panel
            && !state.viewer.sensitive
            && state
                .viewer
                .session
                .as_ref()
                .is_some_and(|session| session.member.is_admin),
        avatar_shape: match skin.manifest.frame.avatar_shape {
            crate::skins::AvatarShape::Member => {
                state.frame.reading.avatar_circle.then_some("circle")
            }
            fixed => fixed.attribute(),
        },
        palette: if Arc::ptr_eq(&skin, &state.frame.skin) {
            state.frame.palette.clone()
        } else {
            skin.palettes[0].clone()
        },
        skin,
        skin_preview: state.frame.preview.then(|| {
            let t = crate::i18n::current();
            let name = state.frame.skin.name(t);
            if state.frame.preview_colors {
                t.skin_preview_colors_bar(name)
            } else {
                t.skin_preview_bar(name)
            }
        }),
        custom_style: custom_style::href(state, path),
        places: blocks::Placed::default(),
    }
}

impl PageMeta<'_> {
    /// The site's name for its points in this request's language (`settings.site.credit_name`, filled with the
    /// language's default by the page frame, docs/architecture.md, "Languages"): the `{ $credit }` of translations.
    fn credit(&self) -> &str {
        &self.frame.settings.site.credit_name
    }
    /// The site's name for its weekly activity figure (`settings.site.index_name`), the `{ $index }` of translations.
    fn index_word(&self) -> &str {
        &self.frame.settings.site.index_name
    }
    /// This site's name (other domains use their own), as the authenticator app shows it for two-step verification.
    fn site_name(&self) -> String {
        crate::site::site_name(&self.frame.settings)
    }

    /// An on-site link for a `*_html` slot of a translation (docs/architecture.md, "Languages"): `path` is a site
    /// path such as `/my/credits` (the deployment prefix is added), `text` is escaped like any template output.
    fn link(&self, path: &str, text: &str) -> crate::i18n::Markup {
        use crate::i18n::Markup;
        Markup::trusted(format!(
            "<a href=\"{}{}\">{}</a>",
            crate::site::base(),
            Markup::text(path),
            Markup::text(text)
        ))
    }

    /// Like [`Self::link`], with a query string appended to the path (already in URL form, escaped like template
    /// output) and optional extra attributes written verbatim after `href` (fixed strings in templates, such as
    /// ` target="_blank" rel="noopener"`).
    fn link_with(
        &self,
        path: &str,
        query: &str,
        attributes: &str,
        text: &str,
    ) -> crate::i18n::Markup {
        use crate::i18n::Markup;
        Markup::trusted(format!(
            "<a href=\"{}{}{}\"{attributes}>{}</a>",
            crate::site::base(),
            Markup::text(path),
            Markup::text(query),
            Markup::text(text)
        ))
    }

    /// The page data block's JSON, escaped for `<script type="application/json">` (docs/skins.md, "Page data").
    /// The member card at the top of the aside on pages without places, drawn by the member-card block's view
    /// without block hooks.
    fn member_card(&self) -> String {
        blocks::render(self, &blocks::BlockView::bare(blocks::Body::MemberCard)).unwrap_or_else(
            |error| {
                tracing::error!(%error, "the member card failed to render");
                String::new()
            },
        )
    }

    fn page_data_json(&self) -> String {
        self.page_data.json(self)
    }

    /// A shown time inside a translated sentence, for its `*_html` slot: `text` in a `<time>` element whose
    /// `datetime` is the instant in UTC (docs/skins.md, "Times").
    fn time_tag(
        &self,
        timestamp: impl std::borrow::Borrow<i64>,
        text: impl std::fmt::Display,
    ) -> crate::i18n::Markup {
        crate::i18n::Markup::trusted(format!(
            "<time datetime=\"{}\">{}</time>",
            crate::clock::iso(timestamp),
            crate::i18n::Markup::text(&text.to_string())
        ))
    }

    /// The number slot of a count (docs/skins.md, "Counts"), written the way this language writes numbers, as
    /// `{ $count }` in a translation would write it.
    fn stat_value(&self, value: impl crate::i18n::Count) -> crate::i18n::Markup {
        crate::i18n::Markup::span("stat__value", self.t.number(value.count()))
    }
    /// The word slot of a count: the label translation for this count.
    fn stat_label(&self, label: impl std::fmt::Display) -> crate::i18n::Markup {
        crate::i18n::Markup::span("stat__label", label)
    }

    /// The address of the new-thread entry: forum pages and thread pages preselect this page's forum
    /// (`/new/{slug}`); forums the member has blocked, forums this visitor cannot read on this site and other
    /// pages use `/new`, and the forum is chosen on the posting page.
    fn post_href(&self) -> String {
        self.frame
            .forum(&self.active_slug)
            .filter(|forum| !self.frame.forum_filters.is_blocked(forum.fid))
            .map_or_else(|| "/new".to_owned(), |forum| format!("/new/{}", forum.slug))
    }

    /// The site time zone written out ("UTC+8"), used where templates say "in the site time zone". The whole site
    /// has only this one time zone (`settings.time.offset_minutes`); display, forms and daily settlement all follow
    /// it.
    fn site_zone(&self) -> String {
        crate::clock::zone_label(
            crate::clock::TimeFormat::from(&*self.frame.settings).offset_minutes,
        )
    }
    /// The site time zone's offset from UTC in minutes, used by the front end to turn the wall-clock time of
    /// `datetime-local` into an instant.
    fn site_offset_minutes(&self) -> i64 {
        self.frame.settings.time.offset_minutes
    }
    /// Whether to offer the "Sign up" entry: when the signup mode (`settings.signup.mode`) is closed, the "Sign up"
    /// links in the header, mobile menu, sidebar, reply box and the sign-in, recovery and reset-password pages are
    /// all left out; `/register` itself stays reachable and shows "Sign-ups are closed for now."
    fn registration_open(&self) -> bool {
        self.frame.settings.signup.mode != "closed"
    }
    /// Mail sending is turned off in the admin area (`settings.email.enabled`): forms that send mail show a note
    /// that mail is paused (`partials/mail_paused.html`).
    fn mail_paused(&self) -> bool {
        !self.frame.settings.email.enabled
    }
    /// The `maxlength` of the header search box. The browser counts UTF-16 units and the server UTF-8 bytes; one
    /// unit is at most 3 bytes (a four-byte character takes two units), so taking a third of the byte limit means no
    /// input can exceed the server's limit.
    fn search_max_length(&self) -> usize {
        SEARCH_MAX_BYTES / 3
    }
    /// The muted banner: the notice, then the link to the records and appeals (a markup slot of the translation,
    /// docs/architecture.md, "Languages").
    fn mute_banner(&self) -> crate::i18n::Markup {
        use crate::i18n::Markup;
        let records = Markup::trusted(format!(
            "<a href=\"{}/my/sanctions\">{}</a>",
            Markup::text(&crate::site::base().to_string()),
            Markup::text(self.t.frame_muted_records()),
        ));
        self.t
            .frame_muted_banner(self.mute_message.as_str(), records)
    }

    /// Whether the header navigation needs "More" at the end: some item does not fit at every desktop width.
    fn nav_more(&self) -> bool {
        self.navigation.iter().any(|link| !link.always_inline())
    }
    /// From which width tier "More" is no longer needed; 0 means it is always needed.
    fn nav_more_until(&self) -> u16 {
        nav_fit::more_until(&self.navigation)
    }
    /// Milliseconds with one decimal ("19.1"); shows 0.0 when there is no counting context.
    fn processed_milliseconds(&self) -> String {
        let milliseconds = self
            .metrics
            .as_ref()
            .map_or(0.0, |metrics| metrics.elapsed().as_secs_f64() * 1000.0);
        format!("{milliseconds:.1}")
    }

    fn sql_queries(&self) -> usize {
        self.metrics.as_ref().map_or(0, |metrics| metrics.sql())
    }

    fn redis_queries(&self) -> usize {
        self.metrics.as_ref().map_or(0, |metrics| metrics.redis())
    }

    fn with_composer(mut self, needed: bool) -> Self {
        self.needs_composer = needed;
        self
    }
}

/// The indexable list for the page's `meta robots`. Only looks at the path; the caller checks separately whether
/// the visitor is signed out and the indexing switch. A forum's first page and its pages `/{slug}-N` are treated
/// alike: the forums in the page frame are exactly those this visitor (anonymous
/// when signed out) can read, and forum addresses never end in `-digits`, so the two can be told apart.
fn indexable_path(state: &AppState, path: &str) -> bool {
    let forum = |segment: &str| state.frame.forums.iter().any(|f| f.slug == segment);
    path == "/"
        || path.starts_with("/page-")
        || path.starts_with("/thread-")
        || matches!(
            path,
            "/terms"
                | "/privacy"
                | "/rules"
                | "/contact"
                | "/feeds"
                | "/forums"
                | "/verified"
                | "/links"
        )
        || path.strip_prefix('/').is_some_and(|segment| {
            forum(segment)
                || segment.rsplit_once('-').is_some_and(|(slug, page)| {
                    !page.is_empty() && page.bytes().all(|b| b.is_ascii_digit()) && forum(slug)
                })
        })
}

/// Site verification: one `name=value` per line in the setting; the character set and forbidden names are already
/// checked on save and decode.
/// A forum address (including its pages) outside this site's scope: 301 to the same address on the canonical
/// domain, or 404 when it is shown nowhere; `None` when it is not an out-of-scope forum address. `uri` is the
/// address with the prefix stripped.
fn forum_elsewhere(state: &AppState, uri: &axum::http::Uri) -> Option<Response> {
    let segment = uri.path().strip_prefix('/')?;
    let slug = state
        .frame
        .any_forum(segment)
        .map(|forum| forum.slug.as_str())
        .or_else(|| {
            let (slug, page) = segment.rsplit_once('-')?;
            (!page.is_empty() && page.bytes().all(|b| b.is_ascii_digit()))
                .then_some(slug)
                .filter(|slug| state.frame.any_forum(slug).is_some())
        })?;
    let forum = state.frame.any_forum(slug)?;
    Some(elsewhere_response(
        state,
        state.frame.elsewhere(forum)?,
        uri,
    ))
}

/// An out-of-scope address: 301 to the same path (query string kept) on the canonical domain, or 404.
fn elsewhere_response(
    state: &AppState,
    target: crate::site::Elsewhere,
    uri: &axum::http::Uri,
) -> Response {
    match target {
        crate::site::Elsewhere::Moved(base) => {
            let location = match uri.query() {
                Some(query) => format!("{base}{}?{query}", uri.path()),
                None => format!("{base}{}", uri.path()),
            };
            // A cross-site absolute address that already carries the target site's prefix; it no longer goes
            // through `site::href`.
            (
                StatusCode::MOVED_PERMANENTLY,
                [(header::LOCATION, location)],
            )
                .into_response()
        }
        crate::site::Elsewhere::Gone => error_page(
            state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().common_error_page_not_found(),
        ),
    }
}

/// The absolute canonical address: `top_fid` is the top-level forum of forum pages
/// and thread pages; other pages pass `None`. With multi-domain off it is just this site's address plus the path.
fn canonical_url(state: &AppState, top_fid: Option<i64>, path: &str) -> String {
    let base = crate::site::canonical_base(top_fid).unwrap_or_else(|| state.site_url().to_owned());
    format!("{}{path}", base.trim_end_matches('/'))
}

fn verification_metas(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
}

/// The name in the page title: from page 2 on, "Page N" is appended. `meta` then adds the site name suffix.
fn paged_title(name: impl Into<String>, page: usize) -> String {
    let name = name.into();
    if page > 1 {
        crate::i18n::current().common_paged_title(name, page)
    } else {
        name
    }
}

/// The page description: the first 160 characters of the description (plain text, whitespace collapsed to one
/// space); when the description is empty it falls back to the site description.
fn description_or_site(state: &AppState, text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        state.frame.settings.site.description.clone()
    } else {
        text.chars().take(160).collect()
    }
}

fn brand_size(state: &AppState, slot: &str) -> Option<(i64, i64)> {
    let asset = state
        .frame
        .brands
        .iter()
        .find(|asset| asset.slot == slot && asset.aid.is_some())?;
    Some((asset.width?, asset.height?))
}

fn brand_path(state: &AppState, slot: &str, variant: &str, fallback: &str) -> String {
    state
        .frame
        .brands
        .iter()
        .find(|asset| asset.slot == slot && asset.aid.is_some())
        .map(|asset| asset.path(variant))
        .unwrap_or_else(|| fallback.to_owned())
}

fn render(template: &impl Template) -> Response {
    match template.render() {
        Ok(html) => (
            [
                (header::VARY, "HX-Request"),
                (header::CACHE_CONTROL, "private, no-store"),
                (header::REFERRER_POLICY, "same-origin"),
            ],
            Html(html),
        )
            .into_response(),
        Err(error) => {
            tracing::error!(%error, "page rendering failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                crate::i18n::current().common_error_render(),
            )
                .into_response()
        }
    }
}

async fn browse(
    State(state): State<Arc<AppState>>,
    Extension(viewer): Extension<auth::Viewer>,
    OriginalUri(uri): OriginalUri,
    query: Result<Query<BrowseQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    // The page frame (site settings, navigation, brand images, readable forums) comes from the process-wide global
    // cache; after a save this process clears it at once and other processes clear it on the broadcast
    // (docs/settings-and-caching.md, "The process cache"); lists and content are read on every request.
    let state = match auth::page_state(&state, viewer).await {
        Ok(state) => state,
        Err(response) => return response,
    };
    let Ok(Query(query)) = query else {
        return error_page(
            &state,
            StatusCode::BAD_REQUEST,
            crate::i18n::current().home_error_filter(),
        );
    };
    let viewer_uid = state.viewer.session.as_ref().map(|s| s.member.uid);
    let search = uri.path() == "/search";
    let slugs: Vec<_> = state.frame.forums.iter().map(|f| f.slug.as_str()).collect();
    let route = if search {
        let page = query.page.unwrap_or(1);
        if page == 0 {
            return error_page(
                &state,
                StatusCode::BAD_REQUEST,
                crate::i18n::current().home_error_search_page(),
            );
        }
        Some(PageRoute::Home { page })
    } else {
        PageRoute::parse(uri.path().trim_start_matches('/'), &slugs)
    };
    let Some(route) = route else {
        // Multi-domain sites: an out-of-scope forum address redirects to its canonical domain.
        if let Some(response) = forum_elsewhere(&state, &uri) {
            return response;
        }
        return error_page(
            &state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().common_error_page_not_found(),
        );
    };
    if let PageRoute::Thread { id, page } = route {
        return topic(&state, &uri, &query, id, page).await;
    }
    let slug = match &route {
        PageRoute::Forum { slug, .. } => slug.as_str(),
        _ => "",
    };
    // The page frame already fetched the forums by visibility; the current forum, its child forums and the follow
    // entry share that copy and are not queried again.
    let forum = state.frame.forum(slug);
    // Blocked forums (including those blocked through their parent forum) list no threads, only a notice and an
    // unblock entry.
    if let Some(f) = forum.filter(|f| state.frame.forum_filters.is_blocked(f.fid)) {
        return forum_filters::forum_blocked_page(&state, f, uri.path());
    }
    let timeline_excluded = state.frame.forum_filters.timeline_excluded();
    // A forum page lists the forum's threads; a top-level forum also rolls up its child forums (those in the page
    // frame this visitor can read), leaving out child forums the member has blocked. The forum
    // itself is not blocked at this point, so it needs no exclusion; hidden forums (including child forums) are
    // listed as usual when visited directly.
    let family: Vec<&crate::organization::ForumRecord> = match forum {
        Some(current) => std::iter::once(current)
            .chain(state.frame.forums.iter().filter(|child| {
                current.parent_fid.is_none()
                    && child.parent_fid == Some(current.fid)
                    && !state.frame.forum_filters.is_blocked(child.fid)
            }))
            .collect(),
        None => Vec::new(),
    };
    let family_fids: Vec<i64> = family.iter().map(|f| f.fid).collect();
    // List scope: a top-level forum together with its child forums (read as one range by the thread row's
    // top-level forum id), a child forum only itself; the home page and search are site-wide. Which forums' threads
    // can be listed is computed in code from the page frame's forum cache: for a forum page, the forums of the family
    // that are readable and not blocked by the member; for the home page and search, the readable forums within this
    // site's scope, minus those the member hid or blocked. The popular and latest cards in the right column use the
    // same scope.
    let list_scope = match forum {
        Some(current) => content::ListScope::of_forum(current.fid, current.parent_fid),
        None => content::ListScope::All,
    };
    let list_forums: Vec<i64> = if forum.is_some() {
        family_fids.clone()
    } else {
        state.frame.listable_forums(&timeline_excluded)
    };
    // Members the viewer has blocked: shared by the list, the counts, the right column and the pin block.
    let blocked = match viewer_uid {
        Some(uid) => match content::blocked_authors(&state.db, uid).await {
            Ok(blocked) => blocked,
            Err(error) => return database_error(&state, error),
        },
        None => Vec::new(),
    };
    let list_viewer = content::ListViewer {
        uid: viewer_uid,
        forums: &list_forums,
        blocked_uids: &blocked,
    };
    // The pin block takes threads across forums: visible are the forums readable site-wide that the member has not
    // hidden or blocked.
    let pin_forums = state.frame.listable_forums(&timeline_excluded);
    let q = if search { query.q.trim() } else { "" };
    if q.len() > SEARCH_MAX_BYTES {
        return error_page(
            &state,
            StatusCode::BAD_REQUEST,
            crate::i18n::current().home_error_search_too_long(),
        );
    }
    // Reading preferences: the page frame already read them for signed-in members; anonymous visitors get the site
    // default.
    let reading = state.frame.reading.clone();
    // The bare addresses of the home page and forums follow the default sort; search does not.
    let default_sort = if search {
        Sort::Latest
    } else {
        reading.list_sort
    };
    let sort = query.sort.unwrap_or(default_sort);
    // Internal links: the visitor's default sort omits the parameter, the other one spells it out; search keeps
    // its original form.
    let link = |route: &PageRoute, sort: Sort| {
        if search {
            urls::search(q, sort, route.page())
        } else {
            urls::listing_for(route, sort, default_sort)
        }
    };
    // The canonical does not depend on the visitor: latest reply carries no parameter.
    let path = if search {
        urls::search(q, sort, route.page())
    } else {
        urls::listing(&route, sort)
    };
    let path = category_url(path, query.category);
    // The threads per page is the admin setting `settings.display.threads_per_page` (10–100); the list cache key
    // already includes the per-page count.
    let page_size = usize::try_from(state.frame.settings.display.threads_per_page).unwrap_or(0);
    if query.category.is_some_and(|c| !(0..=5).contains(&c)) {
        return error_page(
            &state,
            StatusCode::BAD_REQUEST,
            crate::i18n::current().home_error_category(),
        );
    }
    // 301 rather than 302: categories combining only with latest reply and latest thread is an established product
    // rule, so a permanent redirect lets search engines converge invalid combinations onto the canonical address.
    // The cost is that browsers and intermediate proxies cache it indefinitely and the server cannot revoke it; if
    // categories are ever allowed to combine with pinned or highlighted, this must first become a temporary
    // redirect and wait for the cached 301s to expire naturally; changing the condition here alone is not enough.
    if query.category.is_some() && matches!(sort, Sort::Pinned | Sort::Featured) {
        let path = if search {
            urls::search(q, Sort::Latest, route.page())
        } else {
            urls::listing(&route, Sort::Latest)
        };
        return Redirect::permanent(&crate::site::href(category_url(path, query.category)))
            .into_response();
    }
    let threads = match content::cached_list_threads_for_viewer(
        &state.db,
        &list_scope,
        sort,
        search.then_some(q),
        query.category,
        &state.timed_cache(),
        route.page(),
        page_size,
        list_viewer,
    )
    .await
    {
        Ok(threads) => threads,
        Err(error) => return database_error(&state, error),
    };
    let page_count = threads.total.div_ceil(page_size).max(1);
    if route.page() > page_count {
        return error_page(
            &state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().common_error_no_such_page(),
        );
    }
    if !search && uri.path() != route.path() {
        // A sort the request spells out stays, even the canonical one: a visitor whose default is newest threads asks
        // for latest replies with `?sort=latest`, and the redirect is permanent, so its target must not depend on
        // who asked.
        let normalized = match query.sort {
            Some(sort) => format!("{}?sort={}", route.path(), sort.value()),
            None => route.path(),
        };
        return (
            StatusCode::MOVED_PERMANENTLY,
            [(
                header::LOCATION,
                crate::site::href(category_url(normalized, query.category)),
            )],
        )
            .into_response();
    }
    // List times follow the site's relative-time setting: when on and within range they read "3 hours ago",
    // otherwise an absolute time without the year. Computed on every request: `thread.time` in the cache was
    // formatted at the moment it was cached, so a relative form would go stale. The hover tooltip always gives the
    // absolute time with the year.
    let format = crate::clock::current();
    let absolute = crate::clock::TimeFormat {
        relative_hours: None,
        ..format
    };
    // Cross-forum level 2 and level 3 pins: shown as a separate block above the list only on page 1 of a forum
    // page. The family (the parent forum and its child forums) comes from the forum list in the page frame; pins in
    // forums the member hid or blocked do not appear (docs/queries.md, "Sidebars, the pinned block and feeds").
    // When no pin applies, the database is not queried.
    let cross_pins = match forum.filter(|_| !search && route.page() == 1) {
        Some(current) => {
            let forums: Vec<(i64, Option<i64>)> = state
                .frame
                .forums
                .iter()
                .map(|f| (f.fid, f.parent_fid))
                .collect();
            let scope = content::pins::Scope::for_forum(
                current.fid,
                current.parent_fid,
                &forums,
                &family_fids,
            );
            let index = match state.global.pins(&state.db, state.cache_ttl()).await {
                Ok(index) => index,
                Err(error) => return database_error(&state, error),
            };
            // Multi-domain sites: level 3 pins in out-of-scope forums are not merged in.
            let tids = if state.frame.scope.is_some() {
                let inside: Vec<_> = index
                    .iter()
                    .filter(|pin| state.frame.forum_by_fid(pin.fid).is_some())
                    .cloned()
                    .collect();
                scope.applicable(&inside)
            } else {
                scope.applicable(&index)
            };
            let found = if viewer_uid.is_some() {
                let viewer = content::ListViewer {
                    forums: &pin_forums,
                    ..list_viewer
                };
                content::pins::rows(&state.db, &tids, &scope, viewer).await
            } else {
                content::pins::cached_rows(
                    &state.db,
                    &state.timed_cache(),
                    &tids,
                    &scope,
                    &pin_forums,
                )
                .await
            };
            match found {
                Ok(found) => found,
                Err(error) => return database_error(&state, error),
            }
        }
        None => Vec::new(),
    };
    // A parent forum page rolls up the threads of its child forums; those not in this forum are
    // marked with their child forum. In child forum pages and in lists of forums without child forums every thread
    // is in this forum, and the home page and search mark nothing.
    let own_fid = forum.filter(|_| !search).map(|f| f.fid);
    let rows = threads
        .threads
        .iter()
        .map(|thread| Row {
            child_forum: own_fid.is_some_and(|fid| fid != thread.fid),
            ..list_row(&state, &reading, (&format, &absolute), thread)
        })
        .collect();
    let cross_pins = cross_pins
        .iter()
        .map(|thread| list_row(&state, &reading, (&format, &absolute), thread))
        .collect();
    let active_sort = sort;
    let tabs = [Sort::Latest, Sort::Created, Sort::Pinned, Sort::Featured]
        .into_iter()
        .map(|sort| Tab {
            label: sort.label(),
            // Sort entries never carry the category: the category navigation has no "All categories", so switching the
            // sort is the way to clear the filter. Pinned and highlighted are not allowed to combine with
            // categories anyway.
            href: link(&route.with_page(1), sort),
            active: active_sort == sort,
        })
        .collect();
    let category_sort = if sort == Sort::Created {
        Sort::Created
    } else {
        Sort::Latest
    };
    let heading = if search {
        crate::i18n::current().home_search_heading(q)
    } else {
        forum.map(|f| f.name.clone()).unwrap_or_default()
    };
    let title = paged_title(
        if heading.is_empty() {
            sort.label().into()
        } else {
            heading.clone()
        },
        route.page(),
    );
    let current_forum = forum;
    // The moderator list is only for display and comes from the process roster (docs/queries.md, "Process caches");
    // authorization is still checked against the database.
    let forum_moderators = match current_forum {
        Some(f) => match state.global.moderators(&state.db, state.cache_ttl()).await {
            Ok(roster) => crate::organization::forum_moderators(&roster, f.fid, f.parent_fid)
                .into_iter()
                .map(|entry| (entry.uid, entry.display_name()))
                .collect(),
            Err(error) => return database_error(&state, error),
        },
        None => Vec::new(),
    };
    let forum_follow = if let Some(f) = current_forum {
        match social::control(
            &state,
            crate::interaction::social::Relation::Forum,
            f.fid,
            path.clone(),
        )
        .await
        {
            Ok(value) => Some(value),
            Err(error) => return database_error(&state, error),
        }
    } else {
        None
    };
    let forum_filter = match (current_forum, &state.viewer.session) {
        (Some(f), Some(_)) => Some(forum_filters::control(&state, f, path.clone())),
        _ => None,
    };
    let forum_icon = current_forum
        .and_then(|f| f.icon_aid.map(|aid| format!("/forum-icon/{}/{aid}", f.fid)))
        .unwrap_or_default();
    // Child forum tabs and breadcrumbs: the parent/child relation comes from the forum list in
    // the page frame, with no extra query. The tabs list only child forums this visitor can read and has not
    // blocked; hidden ones are listed as usual (the same scope as the parent forum page roll-up).
    let family_root = current_forum.map(|f| state.frame.parent_of(f).unwrap_or(f));
    let forum_tabs: Vec<ForumTab> = match family_root {
        Some(root) => {
            let children: Vec<ForumTab> = state
                .frame
                .forums
                .iter()
                .filter(|child| {
                    child.parent_fid == Some(root.fid)
                        && !state.frame.forum_filters.is_blocked(child.fid)
                })
                .map(|child| ForumTab {
                    label: child.name.clone(),
                    href: format!("/{}", child.slug),
                    active: current_forum.is_some_and(|f| f.fid == child.fid),
                })
                .collect();
            if children.is_empty() {
                Vec::new()
            } else {
                std::iter::once(ForumTab {
                    label: crate::i18n::current().home_all().into(),
                    href: format!("/{}", root.slug),
                    active: current_forum.is_some_and(|f| f.fid == root.fid),
                })
                .chain(children)
                .collect()
            }
        }
        None => Vec::new(),
    };
    let breadcrumb = match (current_forum, family_root) {
        (Some(current), Some(root)) if current.fid != root.fid => vec![
            (
                crate::i18n::current().common_home().to_owned(),
                "/".to_owned(),
            ),
            (root.name.clone(), format!("/{}", root.slug)),
        ],
        _ => Vec::new(),
    };
    // A forum page's description uses the first 160 characters of the forum description, or the site description
    // when empty; the home page and search use the site description.
    let description = match forum {
        Some(f) if !search => description_or_site(&state, &f.description),
        _ => state.frame.settings.site.description.clone(),
    };
    let mut page_meta = meta(&state, title, description, &path, slug, q);
    page_meta.page_data = match current_forum {
        _ if search => page_data::PageData::list(page_data::Kind::Search, None, route.page()),
        Some(f) => page_data::PageData::list(page_data::Kind::Board, Some(f.fid), route.page()),
        None => page_data::PageData::list(page_data::Kind::Home, None, route.page()),
    };
    // A forum page's canonical points to its canonical domain (multi-domain sites).
    let forum_top = current_forum
        .filter(|_| !search)
        .map(|f| f.parent_fid.unwrap_or(f.fid));
    if forum_top.is_some() {
        page_meta.canonical = canonical_url(&state, forum_top, &path);
    }
    // The title of page 1 of the home page (canonical `/`) is just the site name, without the sort name.
    if path == "/" {
        page_meta.title = state.frame.settings.site.name.clone();
    }
    if !search {
        page_meta.feeds = syndication::feed_links(&state, current_forum);
    }
    // When the forum is readable by anonymous visitors, output `BreadcrumbList`: the items of the page breadcrumb
    // (a top-level forum has only "Home") plus this forum.
    if let Some(f) = current_forum.filter(|f| state.frame.anonymous_readable(f)) {
        let trail = if breadcrumb.is_empty() {
            vec![(
                crate::i18n::current().common_home().to_owned(),
                "/".to_owned(),
            )]
        } else {
            breadcrumb.clone()
        };
        let base =
            crate::site::canonical_base(forum_top).unwrap_or_else(|| state.site_url().to_owned());
        page_meta.json_ld = vec![syndication::breadcrumb_json_ld(
            base.trim_end_matches('/'),
            &state.frame.settings.site.name,
            &trail,
            &f.name,
        )];
    }
    if let Some(session) = &state.viewer.session {
        page_meta.member_summary = match state
            .member_summary(crate::identity::Actor {
                uid: session.member.uid,
                credentials_version: session.member.credentials_version,
                api_key: None,
            })
            .await
        {
            Ok(summary) => Some(summary),
            Err(error) => return database_error(&state, error),
        };
    }
    // The head of a board page goes to its board-head block (docs/skins.md, "Places and blocks"); search results
    // keep the same head with their heading only, outside places.
    let head = || blocks::BoardHead {
        heading: heading.clone(),
        icon: String::new(),
        follow: None,
        filter: None,
        description: String::new(),
        moderators: Vec::new(),
        guidance: Vec::new(),
    };
    let search_head = if search {
        match blocks::render(
            &page_meta,
            &blocks::BlockView::bare(blocks::Body::BoardHead(Box::new(head()))),
        ) {
            Ok(html) => Some(html),
            Err(error) => return database_error(&state, anyhow::Error::new(error)),
        }
    } else {
        None
    };
    let board_head = current_forum
        .filter(|_| !search)
        .map(|f| blocks::BoardHead {
            icon: forum_icon.clone(),
            follow: forum_follow,
            filter: forum_filter,
            description: f.description.clone(),
            moderators: forum_moderators,
            guidance: posting::guidance_lines(&f.guidance),
            ..head()
        });
    let placed = blocks::place(
        &state,
        &page_meta,
        blocks::Page {
            kind: if current_forum.is_some() && !search {
                crate::layout::Page::Board
            } else {
                crate::layout::Page::Home
            },
            board: current_forum.filter(|_| !search),
            thread: None,
            board_head,
            // Friend links are exclusive to the main site (multi-domain sites): home pages of other domains do not
            // show this block, nor do later pages or search.
            friend_links: slug.is_empty()
                && !search
                && route.page() == 1
                && crate::site::other(|_| ()).is_none(),
            list_revision: threads.site_revision,
            blocked: Some(blocked.clone()),
        },
    )
    .await;
    page_meta.places = match placed {
        Ok(placed) => placed,
        Err(error) => return database_error(&state, error),
    };
    render(&HomeTemplate {
        forum_tabs,
        breadcrumb,
        cross_pins,
        pinned_view: sort == Sort::Pinned,
        title_new_tab: reading.title_new_tab,
        p: page_meta,
        search_scope: search.then(|| member_search::SearchScope::new(q, false)),
        search_head,
        heading,
        tabs,
        categories: (1..=5)
            .map(|category| Tab {
                label: content::category_label(category),
                href: category_url(link(&route.with_page(1), category_sort), Some(category)),
                active: query.category == Some(category),
            })
            .collect(),
        rows,
        pages: page_window(route.page(), page_count)
            .map(|number| PageLink {
                number,
                href: category_url(link(&route.with_page(number), sort), query.category),
                active: number == route.page(),
            })
            .collect(),
    })
}

#[derive(Template)]
#[template(path = "errors/status.html")]
struct ErrorTemplate<'a> {
    p: PageMeta<'a>,
    status: u16,
    message: &'a str,
}

/// A simplified error page that does not depend on site settings: used when the page frame could not read the
/// settings (broken settings, an error in the middleware stage); the page has no site name, navigation or brand
/// images, so zero values never pose as settings. When settings are broken it states which key, what is in the
/// database and what it should be.
#[derive(Template)]
#[template(path = "errors/plain.html")]
struct PlainError<'a> {
    /// The page language; the page frame did not load, so this is the language resolved at the entry layer.
    t: crate::i18n::Lang,
    status: u16,
    heading: &'a str,
    message: &'a str,
    key: &'a str,
    found: String,
    expected: &'a str,
    asset_version: &'a str,
}

fn plain_error(
    state: &AppState,
    status: StatusCode,
    heading: &str,
    message: &str,
    setting: Option<&crate::runtime::SettingsError>,
) -> Response {
    let mut response = render(&PlainError {
        t: crate::i18n::current(),
        status: status.as_u16(),
        heading,
        message,
        key: setting.map_or("", |error| error.key.as_str()),
        found: setting
            .map(crate::runtime::SettingsError::found_text)
            .unwrap_or_default(),
        expected: setting.map_or("", |error| error.expected),
        asset_version: &state.asset_version,
    });
    *response.status_mut() = status;
    response
}

/// Health check for deployment and monitoring: returns `ok` only when the database (MySQL or SQLite) and Redis
/// (when enabled) both answer within 2 seconds; otherwise 503 with a redacted reason. This is what the deploy
/// script waits for after starting a new version.
/// The request body limit of the routes that take an image upload (attachments, avatars, branding): the image limit
/// (`[media] image_max_bytes`) plus room for the rest of the form. The no-JS editor sends its whole form with the file,
/// at most 40 fields of up to 60,000 bytes each (`media_routes::upload`). Each file is still checked against the image
/// limit itself.
fn upload_body_limit(media: &crate::media::MediaConfig) -> usize {
    const FORM_ALLOWANCE: u64 = 4 * 1024 * 1024;
    usize::try_from(media.image_max_bytes.saturating_add(FORM_ALLOWANCE)).unwrap_or(usize::MAX)
}

async fn healthz(State(state): State<Arc<AppState>>) -> Response {
    const LIMIT: std::time::Duration = std::time::Duration::from_secs(2);
    let database = tokio::time::timeout(
        LIMIT,
        crate::db::query_scalar::<i64>("SELECT 1").fetch_one(state.db.executor()),
    )
    .await;
    let backend = state.db.backend().label();
    let problem = match database {
        Ok(Ok(_)) => None,
        Ok(Err(error)) => Some(
            crate::i18n::current()
                .common_health_unavailable(backend, crate::debug::visible_error(&error.into())),
        ),
        Err(_) => Some(crate::i18n::current().common_health_timeout(backend)),
    };
    // With Redis off, `ensure_available` returns success directly, so no separate check is needed here.
    let problem = match problem {
        Some(problem) => Some(problem),
        None => match tokio::time::timeout(LIMIT, state.cache.ensure_available()).await {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(
                crate::i18n::current()
                    .common_health_unavailable("Redis", crate::debug::visible_error(&error)),
            ),
            Err(_) => Some(crate::i18n::current().common_health_timeout("Redis")),
        },
    };
    let headers = [(header::CACHE_CONTROL, "no-store")];
    match problem {
        None => (headers, "ok\n").into_response(),
        Some(problem) => (
            StatusCode::SERVICE_UNAVAILABLE,
            headers,
            format!("{problem}\n"),
        )
            .into_response(),
    }
}

/// The display response for avatars and forum icons, with the cache header [`crate::media::IMAGE_CACHE_CONTROL`]:
/// the browser does not request again within 5 minutes; after that it comes back with `If-None-Match` to
/// confirm, and the server answers 304 only after re-checking the current binding and visibility; invalidated ones
/// get 404 (a copy already received keeps showing during those 5 minutes).
fn display_image_response(image: crate::media::DisplayImage) -> Response {
    let mut response = match image.bytes {
        None => StatusCode::NOT_MODIFIED.into_response(),
        Some(bytes) => {
            let mut response = bytes.into_response();
            if let Ok(mime) = HeaderValue::from_str(&image.mime_type) {
                response.headers_mut().insert(header::CONTENT_TYPE, mime);
            }
            response
        }
    };
    if let Ok(etag) = HeaderValue::from_str(&image.etag) {
        response.headers_mut().insert(header::ETAG, etag);
    }
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(crate::media::IMAGE_CACHE_CONTROL),
    );
    response
}

fn if_none_match(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
}

fn error_page(state: &AppState, status: StatusCode, message: &str) -> Response {
    if !state.frame.loaded {
        return plain_error(
            state,
            status,
            crate::i18n::current().common_error_request_failed(),
            message,
            None,
        );
    }
    // The description uses a fixed short sentence rather than `message`: database errors are now shown directly in
    // the body, so putting another copy into the meta description would show the same text twice, and error pages
    // are `indexable = false` anyway.
    let mut page = meta(
        state,
        status.as_u16().to_string(),
        crate::i18n::current().common_error_request_failed().into(),
        "/",
        "",
        "",
    );
    page.indexable = false;
    // Error pages output no canonical: they are not the canonical version of any address.
    page.canonical.clear();
    page.verification.clear();
    // Error pages do not apply the personal style sheet: even with broken styles, what went wrong stays
    // readable.
    page.custom_style.clear();
    let mut response = render(&ErrorTemplate {
        p: page,
        status: status.as_u16(),
        message,
    });
    *response.status_mut() = status;
    response
}

fn database_error(state: &AppState, error: anyhow::Error) -> Response {
    // MySQL and Redis errors show their content instead of a "Please try again later": missing tables, failed
    // connections and insufficient privileges can be located with one look at the original error, while collapsing them
    // into a generic text forces everyone to dig through the server logs. Passwords in connection strings are redacted
    // by `visible_error`.
    // Broken settings (missing row, bad value, out of range): no fallback to defaults; state which key, what is in
    // the database and what it should be. Every page's frame reads the settings, so the whole site shows this
    // page, including the admin settings pages; the only way out is fixing the database by hand
    // (docs/settings-and-caching.md, "Reading settings").
    if let Some(setting) = error.downcast_ref::<crate::runtime::SettingsError>() {
        tracing::error!(key = %setting.key, found = %setting.found_text(), expected = setting.expected, "invalid site setting");
        return plain_error(
            state,
            StatusCode::INTERNAL_SERVER_ERROR,
            crate::i18n::current().common_error_settings(),
            &setting.to_string(),
            Some(setting),
        );
    }
    // The write is already committed; only notifying the other processes failed (docs/settings-and-caching.md,
    // "Invalidation"): 503, stating that it was saved and the staleness limit.
    if let Some(unpublished) = crate::cache::unpublished(&error) {
        tracing::error!(error = %unpublished, "saved; global cache invalidation broadcast failed");
        return error_page(
            state,
            StatusCode::SERVICE_UNAVAILABLE,
            &unpublished.to_string(),
        );
    }
    let detail = crate::debug::visible_error(&error);
    // Redis is a rebuildable cache, so its failure is a temporarily unavailable dependency; a 503 with Retry-After
    // is more accurate than a 500.
    // The log records the whole error chain (redacted): `%error` is only the outermost sentence, while the original
    // Redis and sqlx texts are in the inner layers.
    if error.downcast_ref::<crate::cache::Unavailable>().is_some() {
        tracing::error!(error = %detail, "cache unavailable");
        return crate::cache::unavailable_detail(&detail);
    }
    tracing::error!(error = %detail, "database read failed");
    error_page(state, StatusCode::INTERNAL_SERVER_ERROR, &detail)
}

/// After a successful commit, clears this process's pin index and broadcasts (pinning, moving and changing a
/// forum's scope move threads in and out of the index).
/// A failed broadcast gives 503 and states that it was saved (docs/settings-and-caching.md, "Invalidation").
async fn pins_changed(state: &AppState) -> Option<Response> {
    state
        .global
        .announce(&[crate::cache::Block::Pins])
        .await
        .err()
        .map(|error| database_error(state, error))
}

/// Whether a media service failure stays in the form or becomes an error page, split the same way as the admin
/// `form_failure`: validation, conflicts and temporarily unavailable storage are messages for the user and stay in
/// the form (422/409/503); everything else goes to [`media_failure`].
fn media_form_failure(
    error: crate::media::MediaError,
) -> Result<(StatusCode, String), crate::media::MediaError> {
    use crate::media::MediaError;
    match error {
        MediaError::Validation(message) => Ok((StatusCode::UNPROCESSABLE_ENTITY, message)),
        MediaError::Conflict(message) => Ok((StatusCode::CONFLICT, message)),
        MediaError::Unavailable(message) => Ok((StatusCode::SERVICE_UNAVAILABLE, message)),
        other => Err(other),
    }
}

/// The status code and user-facing text of a media error. The original text of internal errors (redacted) goes
/// only to the log; the text shown is "The attachment service is temporarily unavailable".
fn media_message(error: crate::media::MediaError) -> (StatusCode, String) {
    use crate::media::MediaError;
    let status = match &error {
        MediaError::Validation(_) => StatusCode::UNPROCESSABLE_ENTITY,
        MediaError::Forbidden => StatusCode::FORBIDDEN,
        MediaError::NotFound => StatusCode::NOT_FOUND,
        MediaError::Conflict(_) => StatusCode::CONFLICT,
        MediaError::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
        MediaError::Internal(inner) => {
            tracing::error!(error = %crate::debug::visible_error(inner), "attachment service internal error");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    (status, error.to_string())
}

/// The media error page; status code and text: see [`media_message`].
fn media_failure(state: &AppState, error: crate::media::MediaError) -> Response {
    // Replacing a brand image was committed and only the broadcast failed: like `database_error`, 503 stating that
    // it was saved.
    if let crate::media::MediaError::Internal(inner) = &error
        && let Some(unpublished) = crate::cache::unpublished(inner)
    {
        return error_page(
            state,
            StatusCode::SERVICE_UNAVAILABLE,
            &unpublished.to_string(),
        );
    }
    let (status, message) = media_message(error);
    error_page(state, status, &message)
}

async fn not_found(
    State(state): State<Arc<AppState>>,
    Extension(viewer): Extension<auth::Viewer>,
) -> Response {
    let state = match auth::page_state(&state, viewer).await {
        Ok(state) => state,
        Err(response) => return response,
    };
    error_page(
        &state,
        StatusCode::NOT_FOUND,
        crate::i18n::current().common_error_page_not_found(),
    )
}

struct Reply<'a> {
    post: &'a Post,
    avatar: AvatarView,
    /// The author's level badge; deleted authors have none.
    level: Option<crate::growth::Level>,
    /// The author's verification badge; filled in all at once by [`fill_badges`] after the floors are assembled.
    badge: Option<crate::directory::Badge>,
    /// The author's honor badges (docs/roles.md, "Honor groups").
    honors: Vec<crate::honors::HonorBadge>,
    href: String,
    root_href: String,
    root_position: usize,
    parent_label: String,
    parent_href: String,
    alternate_href: String,
    children: Vec<Reply<'a>>,
    html: String,
    reply_href: String,
    quote_href: String,
    pages: Vec<PageLink>,
    controls: ContentControls,
    can_reply: bool,
    can_accept: bool,
    can_quote: bool,
    reaction: interaction::ReactionControl,
}

struct ContentControls {
    report_href: String,
    pid: i64,
    version: i64,
    thread_version: i64,
    can_author: bool,
    can_manage: bool,
    /// Can edit and move threads on others' behalf (administrators, global moderators, moderators); trainee
    /// moderators only have the "Moderate" entry (docs/roles.md, "Trainee moderators").
    can_edit_manage: bool,
    withdrawn: bool,
    edit_href: String,
    request_key: String,
    view: &'static str,
}
struct TopicPermissions {
    can_accept: bool,
    uid: i64,
    can_contribute: bool,
    can_manage: bool,
    can_edit_manage: bool,
    now: i64,
    window: i64,
}
impl TopicPermissions {
    fn author(&self, authorid: Option<i64>, created: u64) -> bool {
        self.can_contribute
            && authorid == Some(self.uid)
            && self.now >= created as i64
            && self.now - (created as i64) < self.window
    }
}

#[derive(Template)]
#[template(path = "topic/detail.html", blocks = ["reply_fragment"])]
struct TopicTemplate<'a> {
    p: PageMeta<'a>,
    thread: &'a Thread,
    /// The breadcrumb items `(name, address)`: home page, parent forum (for threads in a child forum), the
    /// thread's forum.
    breadcrumb: Vec<(String, String)>,
    rows: Vec<Reply<'a>>,
    highlights: highlights::Section,
    opening_html: String,
    activity_html: String,
    opening_avatar: AvatarView,
    opening_level: Option<crate::growth::Level>,
    opening_badge: Option<crate::directory::Badge>,
    opening_honors: Vec<crate::honors::HonorBadge>,
    opening_controls: ContentControls,
    opening_reaction: interaction::ReactionControl,
    opening_downvote: interaction::DownvoteControl,
    tip: activities::TipView,
    opening_quote_href: String,
    can_quote_opening: bool,
    favorite: interaction::FavoriteControl,
    follow: social::Control,
    is_fragment: bool,
    e: posting::Editor,
    forums: Vec<posting::ForumChoice>,
    attachments: Vec<crate::media::Upload>,
    /// The shared editor template needs this field; the thread page only has the reply box and shows no visibility
    /// scope.
    forum_scope_note: &'static str,
    pages: Vec<PageLink>,
    tabs: Vec<Tab>,
}

/// The level of a floor's author; empty when there is no author or the author is deleted, and 0 when there is no
/// growth account.
fn author_level(
    state: &AppState,
    authorid: Option<i64>,
    deleted: bool,
    growth: &std::collections::HashMap<i64, i64>,
) -> Option<crate::growth::Level> {
    let uid = authorid.filter(|_| !deleted)?;
    Some(
        state
            .frame
            .settings
            .growth
            .level(growth.get(&uid).copied().unwrap_or(0)),
    )
}

/// The verification badge of a floor's author (including nested replies); deleted authors show none.
fn fill_badges(
    rows: &mut [Reply<'_>],
    badges: &std::collections::HashMap<i64, crate::directory::Badge>,
    honors: &std::collections::HashMap<i64, Vec<crate::honors::HonorBadge>>,
) {
    for row in rows {
        row.badge = author_badge(row.post.authorid, row.post.author_deleted, badges);
        row.honors = author_honors(row.post.authorid, row.post.author_deleted, honors);
        fill_badges(&mut row.children, badges, honors);
    }
}

fn author_honors(
    authorid: Option<i64>,
    deleted: bool,
    honors: &std::collections::HashMap<i64, Vec<crate::honors::HonorBadge>>,
) -> Vec<crate::honors::HonorBadge> {
    authorid
        .filter(|_| !deleted)
        .and_then(|uid| honors.get(&uid).cloned())
        .unwrap_or_default()
}

fn author_badge(
    authorid: Option<i64>,
    deleted: bool,
    badges: &std::collections::HashMap<i64, crate::directory::Badge>,
) -> Option<crate::directory::Badge> {
    badges.get(&authorid.filter(|_| !deleted)?).cloned()
}

fn post_location(state: &AppState, thread: &Thread, post: &Post, flat: bool) -> String {
    urls::post_location(
        thread,
        post.pid as i64,
        flat,
        urls::Paging::from_settings(&state.frame.settings),
    )
    .expect("the reply is known")
}

fn quote_location(location: &str, pid: i64) -> String {
    let path = location.split('#').next().unwrap_or(location);
    format!(
        "{path}{}quote={pid}#reply-editor",
        if path.contains('?') { "&" } else { "?" }
    )
}

#[allow(clippy::too_many_arguments)]
fn reply_view<'a>(
    thread: &'a Thread,
    post: &'a Post,
    flat: bool,
    rendered: &std::collections::BTreeMap<u64, String>,
    permissions: &TopicPermissions,
    state: &AppState,
    reactions: &std::collections::HashMap<i64, crate::interaction::ReactionState>,
    growth: &std::collections::HashMap<i64, i64>,
) -> Reply<'a> {
    Reply {
        post,
        level: author_level(state, post.authorid, post.author_deleted, growth),
        badge: None,
        honors: Vec::new(),
        // Anonymous floors go through the cache and get avatars by uid, so after an avatar change they never point
        // to an invalidated aid.
        avatar: list_avatar(post.authorid.filter(|_| !post.author_deleted), &post.author),
        href: post_location(state, thread, post, flat),
        root_href: post
            .root_pid
            .map(|pid| format!("/posts/{pid}"))
            .unwrap_or_default(),
        root_position: post.root_floor,
        parent_label: post.parent_label.clone(),
        parent_href: post
            .reply_to_pid
            .map(|pid| format!("/posts/{pid}"))
            .unwrap_or_default(),
        alternate_href: post_location(state, thread, post, !flat),
        html: rendered.get(&post.pid).cloned().unwrap_or_default(),
        reply_href: {
            let location = post_location(state, thread, post, flat);
            let path = location.split('#').next().unwrap_or(&location);
            format!(
                "{path}{}reply_to={}#reply-editor",
                if path.contains('?') { "&" } else { "?" },
                post.pid
            )
        },
        quote_href: quote_location(&post_location(state, thread, post, flat), post.pid as i64),
        children: vec![],
        pages: vec![],
        can_accept: permissions.can_accept
            && permissions.can_contribute
            && !post.hidden
            && !post.withdrawn
            && post.authorid != thread.authorid,
        can_reply: !thread.closed && !post.hidden && !post.erased && !post.blocked,
        can_quote: !thread.closed
            && !post.hidden
            && !post.withdrawn
            && !post.erased
            && !post.blocked,
        reaction: interaction::reaction_control(
            state,
            post.pid as i64,
            post.authorid,
            !post.hidden && !post.withdrawn,
            post_location(state, thread, post, flat),
            reactions.get(&(post.pid as i64)),
        ),
        controls: ContentControls {
            report_href: if !post.hidden
                && !post.withdrawn
                && permissions.can_contribute
                && post.authorid != Some(permissions.uid)
            {
                format!("/my/reports/new?target_type=reply&target_id={}", post.pid)
            } else {
                String::new()
            },
            pid: post.pid as i64,
            version: post.version,
            thread_version: thread.version,
            can_author: !post.hidden
                && !post.erased
                && permissions.author(post.authorid, post.dateline),
            can_manage: permissions.can_manage && !post.erased,
            can_edit_manage: permissions.can_edit_manage && !post.erased,
            withdrawn: post.withdrawn,
            edit_href: format!("/edit/post-{}", post.pid),
            request_key: crate::identity::random_token(),
            view: if flat { "flat" } else { "" },
        },
    }
}

async fn topic(
    state: &AppState,
    uri: &axum::http::Uri,
    query: &BrowseQuery,
    id: u64,
    page: usize,
) -> Response {
    topic_response(state, uri, query, id, page, false).await
}

async fn topic_response(
    state: &AppState,
    uri: &axum::http::Uri,
    query: &BrowseQuery,
    id: u64,
    page: usize,
    fragment: bool,
) -> Response {
    // Floors per page and nested replies per floor are settings (9.1); page numbers, floor location and floor
    // cache keys all follow this one copy.
    let paging = urls::Paging::from_settings(&state.frame.settings);
    let mut thread = match content::thread_for_viewer(&state.db, id, state.readable()).await {
        Ok(Some(thread)) => thread,
        Ok(None) => {
            return error_page(
                state,
                StatusCode::NOT_FOUND,
                crate::i18n::current().topic_error_not_found(),
            );
        }
        Err(error) => return database_error(state, error),
    };
    // When the thread's forum (including its parent forum) is visible to members only, the thread is likewise shown
    // only to members: the "Members only" mark, the share summary and the structured data all follow it (a forum's
    // visibility comes from the forum cache in the page frame; the thread row has no copy).
    if state
        .frame
        .any_forum_by_fid(thread.fid)
        .is_some_and(|forum| !state.frame.anonymous_readable(forum))
    {
        thread.members_only = true;
    }
    // Multi-domain sites: threads in out-of-scope forums redirect to the same address on the canonical domain.
    // Fragment requests (floor pagination) are only sent from a page that is
    // already in scope, so they are not checked again.
    if !fragment
        && let Some(forum) = state.frame.any_forum_by_fid(thread.fid)
        && let Some(target) = state.frame.elsewhere(forum)
    {
        return elsewhere_response(state, target, uri);
    }
    // Threads in blocked forums show no body, with the same notice as the forum page.
    if state.frame.forum_filters.is_blocked(thread.fid)
        && let Some(forum) = state.frame.forums.iter().find(|f| f.fid == thread.fid)
    {
        return forum_filters::forum_blocked_page(state, forum, uri.path());
    }
    if !["", "flat"].contains(&query.view.as_str()) {
        return error_page(
            state,
            StatusCode::BAD_REQUEST,
            crate::i18n::current().topic_error_view(),
        );
    }
    let flat = query.view == "flat";
    let sub_page = query.sub_page.unwrap_or(1);
    if query.sub_page.is_some() && query.sub_root.is_none() || sub_page == 0 {
        return error_page(
            state,
            StatusCode::BAD_REQUEST,
            crate::i18n::current().topic_error_sub_page(),
        );
    }
    let page_count = (if flat {
        thread.reply_count
    } else {
        thread.root_replies
    })
    .div_ceil(paging.replies)
    .max(1);
    if page == 0 || page > page_count {
        return error_page(
            state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().common_error_no_such_page(),
        );
    }
    if let Err(error) = content::cached_replies_for_viewer(
        &state.db,
        &state.timed_cache(),
        &mut thread,
        page,
        flat,
        query.sub_root,
        sub_page,
        (state.viewer.session.as_ref().map(|s| s.member.uid), paging),
    )
    .await
    {
        return database_error(state, error);
    }
    if let Err(error) = content::apply_blocks(
        &state.db,
        state.viewer.session.as_ref().map(|s| s.member.uid),
        &mut thread,
    )
    .await
    {
        return database_error(state, error);
    }
    let thread = &thread;
    let session = state.viewer.session.as_ref();
    // Moderator check (docs/queries.md, "Thread page"): role, freeze and account status use the member row read when
    // this request was authenticated, appointments come from the process moderator roster; it only decides whether
    // the moderation entries are shown, and moderation actions re-check under a lock on the appointment row in the
    // write transaction.
    let can_manage = match session {
        Some(session) => {
            let member = &session.member;
            let active = !member.frozen && member.account_state == "active";
            let parent = state
                .frame
                .any_forum_by_fid(thread.fid)
                .and_then(|forum| forum.parent_fid);
            let target = (member.uid, thread.fid, parent);
            if active && crate::identity::forum_role(member.adminid) {
                match state.global.moderators(&state.db, state.cache_ttl()).await {
                    Ok(roster) => {
                        crate::identity::manages_forum(member.adminid, active, &roster, target)
                    }
                    Err(error) => return database_error(state, error),
                }
            } else {
                crate::identity::manages_forum(member.adminid, active, &[], target)
            }
        }
        None => false,
    };
    let now = crate::identity::now();
    // Activity card: plain threads do not query the database; activity threads read only the activity row and the
    // detail table of its kind. The bounty accept button reuses this row (it only decides whether the button is
    // shown; accepting itself re-checks the locked state in the activity transaction).
    let card = match activities::load_card(state, thread).await {
        Ok(card) => card,
        Err(error) => return database_error(state, error),
    };
    let can_accept = card.as_ref().is_some_and(|card| card.can_accept);
    // The reader's state for this thread: favorite, follow, whether the original poster blocked the reader,
    // downvote, tipped or not, all read in one statement.
    let viewer_state = match session {
        Some(session) => match crate::interaction::viewer_thread_state(
            &state.db,
            session.member.uid,
            thread.id as i64,
            thread.first_pid,
        )
        .await
        {
            Ok(value) => value,
            Err(error) => return database_error(state, error),
        },
        None => crate::interaction::ViewerThreadState::default(),
    };
    let can_edit_manage = can_manage
        && session.is_some_and(|s| crate::content::edits::staff_can_edit(s.member.adminid));
    let permissions = TopicPermissions {
        can_accept,
        uid: session.map(|s| s.member.uid).unwrap_or(0),
        can_contribute: session.is_some_and(|s| s.member.can_contribute()),
        can_manage,
        can_edit_manage,
        now,
        window: crate::content::edits::edit_window(&state.frame.settings),
    };
    let favorite_active = viewer_state.favorited;
    // The favorite count reads the counter column on the thread row (docs/queries.md, "Counter columns") instead of
    // COUNTing the favorite details.
    let favorite_count = thread.favorites;
    let (opening_html, rendered) = match rendered_content(state, thread).await {
        Ok(value) => value,
        Err(error) => return database_error(state, error),
    };
    let visible: Vec<_> = thread
        .posts
        .iter()
        .filter(|p| flat || p.root_pid.is_none())
        .collect();
    let mut path = urls::thread(id, page, flat);
    if let Some(root) = query.sub_root {
        let Some(post) = visible
            .iter()
            .find(|p| p.pid == root && p.root_pid.is_none())
        else {
            return error_page(
                state,
                StatusCode::NOT_FOUND,
                crate::i18n::current().topic_error_root_page(),
            );
        };
        if flat || sub_page > post.sub_replies_count.div_ceil(paging.sub_replies).max(1) {
            return error_page(
                state,
                StatusCode::NOT_FOUND,
                crate::i18n::current().topic_error_no_sub_page(),
            );
        }
        path = format!("{path}?sub_root={root}&sub_page={sub_page}");
    }
    if uri.path() != (PageRoute::Thread { id, page }).path() {
        let location = query
            .quote
            .map(|pid| quote_location(&path, pid))
            .unwrap_or(path);
        return (
            StatusCode::MOVED_PERMANENTLY,
            [(header::LOCATION, crate::site::href(location))],
        )
            .into_response();
    }
    let reaction_pids: Vec<_> = std::iter::once(thread.first_pid)
        .chain(thread.posts.iter().map(|p| p.pid as i64))
        .collect();
    let reactions = match crate::interaction::reactions(
        &state.db,
        session.map(|s| s.member.uid),
        &reaction_pids,
    )
    .await
    {
        Ok(values) => values,
        Err(error) => return database_error(state, error),
    };
    // Growth value and account status of the floor authors: the authors of this page are read at once anchored on
    // the member primary key, not put into the shared cache (docs/queries.md, "Thread page").
    let authors: Vec<i64> = thread
        .authorid
        .into_iter()
        .chain(thread.posts.iter().filter_map(|p| p.authorid))
        .collect();
    let page_authors = match crate::growth::page_authors(&state.db, &authors).await {
        Ok(values) => values,
        Err(error) => return database_error(state, error),
    };
    let growth = &page_authors.growth;
    // Verification of the floor authors: reads only authors who are not frozen and in good standing, from a single
    // table by primary key; expired ones are filtered out in code.
    let badges =
        match crate::directory::badges_of_active(&state.db, &page_authors.active, now).await {
            Ok(values) => values,
            Err(error) => return auth::identity_error(state, error),
        };
    // Honor groups of the floor authors: the group directory comes from the process cache, the grant records are
    // read from a single table by primary key, and disabled or expired ones are filtered out in code.
    let honors = match state.global.honors(&state.db, state.cache_ttl()).await {
        Ok(catalog) => {
            match crate::honors::badges_of_active(&state.db, &catalog, &page_authors.active, now)
                .await
            {
                Ok(values) => values,
                Err(error) => return auth::identity_error(state, error),
            }
        }
        Err(error) => return database_error(state, error),
    };
    let highlights = match highlights::load_with_reactions(state, thread, flat, &reactions).await {
        Ok(value) => value,
        Err(error) => return database_error(state, error),
    };
    let mut rows: Vec<Reply<'_>> = visible
        .iter()
        .map(|post| {
            let mut row = reply_view(
                thread,
                post,
                flat,
                &rendered,
                &permissions,
                state,
                &reactions,
                growth,
            );
            if !flat {
                let children: Vec<_> = thread
                    .posts
                    .iter()
                    .filter(|p| p.root_pid == Some(post.pid))
                    .collect();
                let count = post.sub_replies_count.div_ceil(paging.sub_replies).max(1);
                let current = if query.sub_root == Some(post.pid) {
                    sub_page
                } else {
                    1
                };
                row.children = children
                    .into_iter()
                    .map(|p| {
                        reply_view(
                            thread,
                            p,
                            false,
                            &rendered,
                            &permissions,
                            state,
                            &reactions,
                            growth,
                        )
                    })
                    .collect();
                row.pages = page_window(current, count)
                    .map(|number| PageLink {
                        number,
                        href: format!(
                            "{}?sub_root={}&sub_page={number}#floor-{}",
                            urls::thread(id, page, false),
                            post.pid,
                            post.position
                        ),
                        active: number == current,
                    })
                    .collect();
            }
            row
        })
        .collect();
    fill_badges(&mut rows, &badges, &honors);
    let target = query
        .sub_root
        .and_then(|root| thread.posts.iter().find(|p| p.root_pid == Some(root)))
        .or_else(|| visible.first().copied());
    let tabs = [false, true]
        .into_iter()
        .map(|is_flat| Tab {
            label: if is_flat {
                crate::i18n::current().topic_view_flat()
            } else {
                crate::i18n::current().topic_view_nested()
            },
            href: target
                .map(|p| post_location(state, thread, p, is_flat))
                .unwrap_or_else(|| urls::thread(id, 1, is_flat)),
            active: is_flat == flat,
        })
        .collect();
    let Some(forum) = state.frame.forum_by_fid(thread.fid) else {
        return error_page(
            state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().topic_error_not_found(),
        );
    };
    // The breadcrumb "Home › [parent forum ›] forum"; the parent forum comes from the forum list in the page frame.
    let breadcrumb: Vec<(String, String)> = std::iter::once((
        crate::i18n::current().common_home().to_owned(),
        "/".to_owned(),
    ))
    .chain(
        state
            .frame
            .parent_of(forum)
            .into_iter()
            .chain(std::iter::once(forum))
            .map(|f| (f.name.clone(), format!("/{}", f.slug))),
    )
    .collect();
    let mut e = posting::Editor {
        tid: id as i64,
        view: query.view.clone(),
        location: path.clone(),
        ..Default::default()
    };
    if let Some(target) = query.reply_to {
        let Some(post) = thread.posts.iter().find(|p| p.pid == target) else {
            return error_page(
                state,
                StatusCode::NOT_FOUND,
                crate::i18n::current().topic_error_reply_target(),
            );
        };
        if post.hidden || post.erased || post.blocked || thread.closed {
            return error_page(
                state,
                StatusCode::FORBIDDEN,
                crate::i18n::current().topic_error_reply_closed(),
            );
        }
        e.root_pid = post.root_pid.unwrap_or(post.pid) as i64;
        e.reply_to_pid = post.pid as i64;
    }
    if let Some(pid) = query.quote {
        let available = if pid == thread.first_pid {
            !thread.withdrawn && !thread.erased && !thread.blocked
        } else {
            thread.posts.iter().any(|post| {
                post.pid as i64 == pid
                    && !post.hidden
                    && !post.withdrawn
                    && !post.erased
                    && !post.blocked
            })
        };
        if !available {
            return error_page(
                state,
                StatusCode::NOT_FOUND,
                crate::i18n::current().topic_error_quote(),
            );
        }
        if thread.closed {
            return error_page(
                state,
                StatusCode::FORBIDDEN,
                crate::i18n::current().topic_error_locked(),
            );
        }
        e.quote_pid = pid;
        e.body = format!(
            "[{}](post:{pid})\n\n",
            crate::i18n::current().topic_quote_link()
        );
        e.return_to = quote_location(&path, pid);
    }
    e.prepare();
    let forums = posting::choices(state);
    let follow = match social::control_known(
        state,
        crate::interaction::social::Relation::Thread,
        thread.id as i64,
        path.clone(),
        viewer_state.followed,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => return database_error(state, error),
    };
    // Whether the original poster blocked the reader: then neither downvoting nor tipping is available.
    let viewer_uid = session.map(|s| s.member.uid);
    let is_author = viewer_uid.is_some() && viewer_uid == thread.authorid;
    let blocked_by_author = !is_author && viewer_state.blocked_by_author;
    // Downvotes: the original poster sees the downvote count on the first post (counter column), others see
    // whether they downvoted.
    let downvote_value = crate::interaction::DownvoteState {
        active: !is_author && viewer_state.downvoted,
        count: is_author.then_some(thread.downvotes),
    };
    let viewer_state = crate::interaction::ViewerThreadState {
        blocked_by_author,
        ..viewer_state
    };
    let tip = match activities::tip_view(state, thread, &viewer_state).await {
        Ok(value) => value,
        Err(error) => return database_error(state, error),
    };
    let page_meta = {
        // Kept consistent with the condition for rendering editor/form.html in topic/detail.html.
        // The description takes the first 160 characters of the first post's plain text: inserting the Markdown
        // as is would write the whole body (up to about 60 KB), spoiler and quote syntax included, into the
        // head twice.
        // The summary and the structured data are bound by the same access permissions: a thread or forum
        // visible to members only is a 404 for anonymous visitors, signed-in members see the site introduction
        // as the share description, and no structured data is output.
        let public = !thread.members_only;
        // When the summary is empty (the first post has only images or attachments) the description falls back
        // to the site description, and the structured data carries no `text`.
        let excerpt = if public {
            crate::markdown::rendered_excerpt(&opening_html, 160)
        } else {
            String::new()
        };
        let mut p = meta(
            state,
            paged_title(thread.title.clone(), page),
            if excerpt.is_empty() {
                state.frame.settings.site.description.clone()
            } else {
                excerpt.clone()
            },
            &path,
            &forum.slug,
            "",
        );
        p.og_type = "article";
        p.page_data =
            page_data::PageData::thread(thread, (page, page_count), page_floors(thread, &rows));
        // The thread page's canonical and structured data point to the canonical domain of its forum (multi-domain
        // sites).
        let top = state
            .frame
            .any_forum_by_fid(thread.fid)
            .map(|forum| forum.parent_fid.unwrap_or(forum.fid));
        p.canonical = canonical_url(state, top, &path);
        let base = crate::site::canonical_base(top).unwrap_or_else(|| state.site_url().to_owned());
        if public && !thread.withdrawn && !thread.erased {
            let base = base.trim_end_matches('/');
            let url = format!("{base}{}", urls::thread(id, 1, false));
            let forum_url = format!("{base}/{}", forum.slug);
            // The breadcrumb structured data has the same condition as `DiscussionForumPosting` (thread visible
            // to anonymous visitors, not withdrawn): the items of the page breadcrumb plus this thread.
            p.json_ld = vec![
                syndication::thread_json_ld(thread, &url, &excerpt, (&forum.name, &forum_url)),
                syndication::breadcrumb_json_ld(
                    base,
                    &state.frame.settings.site.name,
                    &breadcrumb,
                    &thread.title,
                ),
            ];
        }
        let has_editor = !thread.closed && p.authenticated && !p.needs_verification;
        p.with_composer(has_editor)
    };
    // The thread page's places: its aside and page head (docs/skins.md, "Places and blocks").
    let mut page_meta = page_meta;
    let placed = blocks::place(
        state,
        &page_meta,
        blocks::Page {
            kind: crate::layout::Page::Thread,
            board: state.frame.any_forum_by_fid(thread.fid),
            thread: Some(thread),
            board_head: None,
            friend_links: false,
            list_revision: None,
            blocked: None,
        },
    )
    .await;
    page_meta.places = match placed {
        Ok(placed) => placed,
        Err(error) => return database_error(state, error),
    };
    let template = TopicTemplate {
        opening_downvote: interaction::downvote_control(
            state,
            thread.first_pid,
            thread.authorid,
            !thread.withdrawn && !thread.erased && !thread.blocked,
            blocked_by_author,
            format!("{}#floor-1", urls::thread(id, 1, flat)),
            downvote_value,
        ),
        tip,
        follow,
        p: page_meta,
        thread,
        breadcrumb,
        rows,
        highlights,
        activity_html: match activities::card_html(state, card) {
            Ok(v) => v,
            Err(e) => return database_error(state, e),
        },
        opening_html,
        opening_avatar: list_avatar(
            thread.authorid.filter(|_| !thread.author_deleted),
            &thread.author,
        ),
        opening_level: author_level(state, thread.authorid, thread.author_deleted, growth),
        opening_badge: author_badge(thread.authorid, thread.author_deleted, &badges),
        opening_honors: author_honors(thread.authorid, thread.author_deleted, &honors),
        opening_reaction: interaction::reaction_control(
            state,
            thread.first_pid,
            thread.authorid,
            !thread.withdrawn,
            format!("{}#floor-1", urls::thread(id, 1, flat)),
            reactions.get(&thread.first_pid),
        ),
        opening_quote_href: quote_location(&urls::thread(id, 1, flat), thread.first_pid),
        can_quote_opening: !thread.closed && !thread.withdrawn && !thread.erased && !thread.blocked,
        favorite: interaction::FavoriteControl {
            tid: thread.id as i64,
            active: favorite_active,
            count: Some(favorite_count),
            can_change: permissions.can_contribute && (favorite_active || !thread.withdrawn),
            location: path.clone(),
            request_key: crate::identity::random_token(),
        },
        opening_controls: ContentControls {
            report_href: if !thread.withdrawn
                && permissions.can_contribute
                && thread.authorid != Some(permissions.uid)
            {
                format!("/my/reports/new?target_type=thread&target_id={}", thread.id)
            } else {
                String::new()
            },
            pid: thread.first_pid,
            version: thread.first_version,
            thread_version: thread.version,
            can_author: !thread.erased && permissions.author(thread.authorid, thread.created),
            can_manage: can_manage && !thread.erased,
            can_edit_manage: can_edit_manage && !thread.erased,
            withdrawn: thread.withdrawn,
            edit_href: format!("/edit/thread-{}", thread.id),
            request_key: crate::identity::random_token(),
            view: if flat { "flat" } else { "" },
        },
        is_fragment: fragment,
        e,
        forums,
        attachments: vec![],
        forum_scope_note: "",
        pages: page_window(page, page_count)
            .map(|number| PageLink {
                number,
                href: urls::thread(id, number, flat),
                active: page == number,
            })
            .collect(),
        tabs,
    };
    if fragment {
        match template.as_reply_fragment().render() {
            Ok(html) => (
                [
                    (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                    (header::CACHE_CONTROL, "private, no-store"),
                    (header::VARY, "HX-Request"),
                ],
                Html(html),
            )
                .into_response(),
            Err(error) => database_error(state, error.into()),
        }
    } else {
        render(&template)
    }
}

/// The floors a thread page shows, for its page data block: the opening post, then each reply followed by its nested
/// replies, in page order.
fn page_floors(thread: &Thread, rows: &[Reply<'_>]) -> Vec<page_data::Floor> {
    let opening = page_data::Floor::new(
        1,
        u64::try_from(thread.first_pid).unwrap_or(0),
        thread.created,
    );
    std::iter::once(opening)
        .chain(rows.iter().flat_map(|row| {
            std::iter::once(row)
                .chain(row.children.iter())
                .map(|reply| {
                    page_data::Floor::new(reply.post.position, reply.post.pid, reply.post.dateline)
                })
        }))
        .collect()
}

fn category_url(mut path: String, category: Option<i64>) -> String {
    if let Some(category) = category {
        let sep = if path.contains('?') { '&' } else { '?' };
        path.push_str(&format!("{sep}category={category}"));
    }
    path
}

/// A bounty can still be accepted: the kind is bounty, the status open, and the deadline not yet reached.
async fn rendered_content(
    state: &AppState,
    thread: &Thread,
) -> anyhow::Result<(String, std::collections::BTreeMap<u64, String>)> {
    if state.viewer.session.is_some() || !state.cache.enabled() {
        return render_thread_bodies(state, thread).await;
    }
    let site_url = state.site_url();
    let variants: Vec<_> = thread
        .posts
        .iter()
        .map(|p| (p.pid, p.version, p.hidden, p.withdrawn))
        .collect();
    let key = crate::cache::CacheKey::new(
        &crate::cache::registry::RENDERED_BODIES,
        content::cache_revision(&state.db).await?,
        // The value is the HTML rendered by markdown.rs. When the rendered output changes (tags, attributes, class
        // names), bump the tag version in the registry (`registry::RENDERED_BODIES`, where each version's changes
        // are recorded); otherwise old HTML stays until the TTL after a deploy, and during rolling releases old and
        // new processes overwrite each other.
        &(
            &*crate::site::prefix(),
            site_url,
            crate::site::sibling_urls(),
            crate::clock::current(),
            thread.id,
            thread.cache_revision,
            variants,
        ),
    )?;
    state
        .timed_cache()
        .get_or_fill_json(&key, || render_thread_bodies(state, thread))
        .await
}

async fn render_thread_bodies(
    state: &AppState,
    thread: &Thread,
) -> anyhow::Result<(String, std::collections::BTreeMap<u64, String>)> {
    let site_url = state.site_url();
    let opening = thread.body.join("\n\n");
    let mut sources = vec![opening.as_str()];
    sources.extend(thread.posts.iter().map(|p| p.body.as_str()));
    let names = crate::markdown::member_names(&state.db, &sources).await?;
    let t = crate::i18n::current();
    let tombstone = |text: &str| crate::markdown::Rendered {
        html: format!("<p class=\"gone\">{}</p>", crate::i18n::Markup::text(text)),
        cards: Vec::new(),
    };
    let opening = if thread.erased {
        tombstone(t.topic_tombstone_erased())
    } else if thread.withdrawn {
        tombstone(t.topic_tombstone_withdrawn_thread())
    } else {
        crate::markdown::render_page(&opening, &names, Some(thread.first_pid), site_url)
    };
    let posts: Vec<(u64, crate::markdown::Rendered)> = thread
        .posts
        .iter()
        .map(|p| {
            (
                p.pid,
                if p.erased {
                    tombstone(t.topic_tombstone_erased())
                } else if p.hidden {
                    tombstone(t.topic_tombstone_hidden())
                } else if p.withdrawn {
                    tombstone(t.topic_tombstone_withdrawn())
                } else {
                    crate::markdown::render_page(&p.body, &names, Some(p.pid as i64), site_url)
                },
            )
        })
        .collect();
    // The card candidates of the whole page are deduplicated in floor order, at most 20 threads, fetched in one
    // query under the reader's permissions; no query when there are no candidates. The result for the anonymous
    // view is cached together with the render cache.
    let tids = content::cards::page_tids(
        std::iter::once(&opening)
            .chain(posts.iter().map(|(_, rendered)| rendered))
            .flat_map(|rendered| &rendered.cards),
    );
    let viewer_uid = state.viewer.session.as_ref().map(|s| s.member.uid);
    let cards =
        content::cards::topic_cards(&state.db, viewer_uid, &state.frame.all_forums, &tids).await?;
    let fill = |rendered: &crate::markdown::Rendered| {
        crate::markdown::fill_cards(&rendered.html, &rendered.cards, &cards)
    };
    Ok((
        fill(&opening),
        posts
            .iter()
            .map(|(pid, rendered)| (*pid, fill(rendered)))
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::page_window;

    #[test]
    fn pagination_lists_the_ends_and_two_pages_around_the_current_one() {
        let window = |current, last| page_window(current, last).collect::<Vec<_>>();
        assert_eq!(window(1, 1), [1]);
        assert_eq!(window(1, 0), [1]);
        assert_eq!(window(1, 5), [1, 2, 3, 4, 5]);
        assert_eq!(window(1, 1000), [1, 2, 3, 1000]);
        assert_eq!(window(500, 1000), [1, 498, 499, 500, 501, 502, 1000]);
        assert_eq!(window(1000, 1000), [1, 998, 999, 1000]);
        assert_eq!(window(5, 10), [1, 2, 3, 4, 5, 6, 7, 10]);
        // A current page beyond range counts as the last page, so no page numbers appear out of nowhere.
        assert_eq!(window(2000, 6), [1, 4, 5, 6]);
    }
}
