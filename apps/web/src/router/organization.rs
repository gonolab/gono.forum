//! Native form entry points for forum management, thread moving and the forum directory.
use super::extract::{AdminPage, CsrfForm, Page, csrf_forms};
use super::*;
use crate::{content::writes::ContentWriteError, identity, organization as service};
use axum::{extract::Path as AxumPath, routing::post};

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/manage/forums", get(forums_page).post(save_forum))
        .route("/manage/forums/settings", post(save_forum_settings))
        .route("/manage/forums/order", post(reorder_forums))
        .route("/manage/forums/{fid}/delete", post(delete_forum))
        .route("/manage/forums/{fid}/merge", post(merge_forum))
        .route("/forums", get(directory))
        .route(
            "/manage/threads/{tid}/organization",
            get(thread_settings_manage),
        )
        .route("/manage/threads/{tid}/move", post(move_thread))
        .layer(axum::extract::DefaultBodyLimit::max(32 * 1024))
}
#[derive(Default, Deserialize)]
struct QueryForm {
    #[serde(default)]
    fid: i64,
    /// The message after a successful save: `settings` (forum settings), `deleted` (forum deleted), `merging` (merge
    /// queued).
    #[serde(default)]
    saved: String,
}
#[derive(Default, Deserialize)]
#[serde(default)]
struct ForumForm {
    _csrf: String,
    fid: i64,
    parent_fid: i64,
    icon_aid: i64,
    name: String,
    slug: String,
    description: String,
    guidance: String,
    displayorder: i64,
    status: i64,
    visibility: String,
    version: i64,
    /// The color picker always sends a value; the box beside it says whether the forum has a color at all.
    color: String,
    color_on: String,
    glyph: String,
}
csrf_forms!(ForumForm);
impl ForumForm {
    fn input(self) -> service::ForumInput {
        service::ForumInput {
            color: if self.color_on == "1" {
                self.color
            } else {
                String::new()
            },
            glyph: self.glyph,
            fid: self.fid,
            parent_fid: self.parent_fid,
            icon_aid: self.icon_aid,
            name: self.name,
            slug: self.slug,
            description: self.description,
            guidance: self.guidance,
            displayorder: self.displayorder,
            status: self.status,
            visibility: self.visibility,
            version: self.version,
        }
    }
}
struct IconChoice {
    aid: i64,
    name: String,
}
#[derive(Default, Deserialize)]
struct ThreadForm {
    _csrf: String,
    #[serde(default)]
    target_fid: i64,
    version: i64,
    request_key: String,
    #[serde(default)]
    reason: String,
}
csrf_forms!(ThreadForm);
#[allow(clippy::result_large_err)]
fn actor(state: &AppState) -> Result<identity::Actor, Response> {
    state
        .viewer
        .session
        .as_ref()
        .map(|s| s.member.actor())
        .ok_or_else(|| auth::redirect("/login?return_to=/manage"))
}
fn failure(state: &AppState, error: ContentWriteError) -> Response {
    let (status, message) = posting::write_error(error);
    error_page(state, status, &message)
}
/// One row of the admin forum list: grouped by parent and child, with child forums right after their parent.
struct ManagedRow {
    forum: service::ForumRecord,
    child: bool,
    /// Thread count and today's posts; numbers are only given for forums readable by anonymous visitors (statistics
    /// count only public content), and parent forums include their child forums.
    count: Option<content::ForumCount>,
    /// A "View" link is given when the front end can open it (readable by signed-in members).
    view: bool,
    /// Moderators appointed directly to this forum (moderator roster, display only): `(uid, username)`.
    moderators: Vec<(i64, String)>,
}
/// A merge job in progress.
struct MergeView {
    target: String,
    eventid: i64,
    /// The job's page in the task list.
    task_path: String,
}
/// Extra state for the list page: the top message and errors from the delete and merge forms.
#[derive(Default)]
struct ForumsExtra {
    saved: String,
    action_message: String,
    /// Errors from bulk ordering, shown above the list.
    order_message: String,
}
#[derive(Template)]
#[template(path = "manage/forums.html")]
struct ForumsTemplate<'a> {
    p: PageMeta<'a>,
    rows: Vec<service::ForumRecord>,
    listed: Vec<ManagedRow>,
    icons: Vec<IconChoice>,
    input: service::ForumInput,
    message: String,
    directory_statistics: bool,
    forum_statistics_seconds: i64,
    settings_saved: bool,
    notice: &'static str,
    action_message: String,
    order_message: String,
    merge: Option<MergeView>,
    /// Merge targets: every forum except this one, marked with level and state (disabled and admin-only forums can be
    /// chosen too, for archiving).
    targets: Vec<(i64, String)>,
    /// When editing an existing forum: the number of its threads a change of parent rewrites.
    rewrite: Option<i64>,
}

/// Enabled and public to anonymous visitors at both levels: forum statistics count only such forums (the definition in
/// `content::forum_statistics_at`).
fn anonymous_readable(forum: &service::ForumRecord, forums: &[service::ForumRecord]) -> bool {
    let public = |f: &service::ForumRecord| f.status == 1 && f.visibility == "public";
    public(forum)
        && forum
            .parent_fid
            .is_none_or(|parent| forums.iter().any(|f| f.fid == parent && public(f)))
}

/// Signed-in members can open it on the front end.
fn member_readable(forum: &service::ForumRecord, forums: &[service::ForumRecord]) -> bool {
    let open = |f: &service::ForumRecord| f.status == 1 && f.visibility != "private";
    open(forum)
        && forum
            .parent_fid
            .is_none_or(|parent| forums.iter().any(|f| f.fid == parent && open(f)))
}

/// Grouped by parent and child: see [`service::grouped_by_parent`].
pub(super) fn grouped(forums: &[service::ForumRecord]) -> Vec<(&service::ForumRecord, bool)> {
    service::grouped_by_parent(forums, |f| (f.fid, f.parent_fid))
}

/// Options for forum drop-downs (move targets, merge targets): grouped by parent and child, child forums with an indent
/// prefix, and the name followed by markers for disabled, admin-only and signed-in members (move targets allow
/// disabled and admin-only forums, for archiving). `skip` is the forum not listed (itself).
fn forum_options(forums: &[service::ForumRecord], skip: i64) -> Vec<(i64, String)> {
    grouped(forums)
        .into_iter()
        .filter(|(forum, _)| forum.fid != skip)
        .map(|(forum, child)| {
            let t = crate::i18n::current();
            let state = if forum.status == 0 {
                t.forum_option_disabled()
            } else if forum.visibility == "private" {
                t.forum_option_private()
            } else if forum.visibility == "members" {
                t.forum_option_members()
            } else {
                ""
            };
            let prefix = if child { service::CHILD_PREFIX } else { "" };
            (forum.fid, format!("{prefix}{}{state}", forum.name))
        })
        .collect()
}

/// Statistics for one forum: numbers only for forums readable by anonymous visitors; a parent forum totals itself and
/// its child forums readable by anonymous visitors.
fn forum_count(
    forum: &service::ForumRecord,
    forums: &[service::ForumRecord],
    statistics: &content::ForumStatistics,
    readable: impl Fn(&service::ForumRecord, &[service::ForumRecord]) -> bool,
) -> Option<content::ForumCount> {
    if !readable(forum, forums) {
        return None;
    }
    let children = forums
        .iter()
        .filter(|f| f.parent_fid == Some(forum.fid) && readable(f, forums))
        .map(|f| f.slug.as_str());
    Some(statistics.total(std::iter::once(forum.slug.as_str()).chain(children)))
}

async fn forums_response(
    state: &AppState,
    fid: i64,
    input: Option<service::ForumInput>,
    message: String,
    status: StatusCode,
) -> Response {
    forums_response_with(state, fid, input, message, status, ForumsExtra::default()).await
}

async fn forums_response_with(
    state: &AppState,
    fid: i64,
    input: Option<service::ForumInput>,
    message: String,
    status: StatusCode,
    extra: ForumsExtra,
) -> Response {
    let actor = match actor(state) {
        Ok(a) => a,
        Err(r) => return r,
    };
    if !state
        .viewer
        .session
        .as_ref()
        .is_some_and(|s| s.member.is_admin)
    {
        return error_page(
            state,
            StatusCode::FORBIDDEN,
            crate::i18n::current().forum_error_admin_only(),
        );
    }
    let rows = match service::managed_forums(&state.db, actor).await {
        Ok(r) => r,
        Err(e) => return failure(state, e),
    };
    let input = input.unwrap_or_else(|| {
        rows.iter()
            .find(|f| f.fid == fid)
            .map(|f| service::ForumInput {
                fid: f.fid,
                parent_fid: f.parent_value(),
                icon_aid: f.icon_value(),
                name: f.name.clone(),
                slug: f.slug.clone(),
                description: f.description.clone(),
                guidance: f.guidance.clone(),
                displayorder: f.displayorder,
                status: f.status,
                visibility: f.visibility.clone(),
                version: f.version,
                color: f.color.clone(),
                glyph: f.glyph.clone(),
            })
            .unwrap_or_else(|| service::ForumInput {
                status: 1,
                visibility: "public".into(),
                ..Default::default()
            })
    });
    let icons=match crate::db::query("SELECT a.aid,d.filename FROM gonof_forum_attachment a JOIN gonof_forum_attachment_0 d ON d.aid=a.aid WHERE (a.uid=? OR a.aid=?) AND d.lifecycle='ready' AND d.scan_status='clean' AND d.mime_type LIKE 'image/%' ORDER BY (a.aid=?) DESC,a.aid DESC LIMIT 100").bind(actor.uid).bind(input.icon_aid).bind(input.icon_aid).fetch_all(state.db.executor()).await {
        Ok(rows)=>rows.into_iter().map(|r|Ok(IconChoice{aid:r.try_get("aid")?,name:r.try_get("filename")?})).collect::<anyhow::Result<Vec<_>>>(),
        Err(e)=>Err(e.into()),
    };
    let icons = match icons {
        Ok(i) => i,
        Err(e) => return database_error(state, e),
    };
    // The edit page reads the current settings values (not the in-process cache), like the other settings pages.
    let settings = match crate::runtime::read(&state.db).await {
        Ok(settings) => settings,
        Err(error) => return auth::identity_error(state, error),
    };
    // Shares the same forum statistics cache as the directory page.
    let statistics = match state.forum_statistics().await {
        Ok(statistics) => statistics,
        Err(error) => return database_error(state, error),
    };
    // The moderator column comes from the in-process roster (display only, global cache 3.5): it lists direct
    // appointments, and a parent forum's moderators are not repeated on child forum rows.
    let roster = match state.global.moderators(&state.db, state.cache_ttl()).await {
        Ok(roster) => roster,
        Err(error) => return database_error(state, error),
    };
    let listed = grouped(&rows)
        .into_iter()
        .map(|(forum, child)| ManagedRow {
            count: forum_count(forum, &rows, &statistics, anonymous_readable),
            view: member_readable(forum, &rows),
            moderators: service::forum_moderators(&roster, forum.fid, None)
                .into_iter()
                .map(|entry| (entry.uid, entry.display_name()))
                .collect(),
            forum: forum.clone(),
            child,
        })
        .collect();
    let rewrite = if input.fid > 0 {
        match service::rewrite_count(&state.db, input.fid).await {
            Ok(counts) => Some(counts),
            Err(e) => return failure(state, e),
        }
    } else {
        None
    };
    let merge = if input.fid > 0 {
        match service::pending_merge(&state.db, input.fid).await {
            Ok(found) => found.map(|(target, eventid)| MergeView {
                target: rows
                    .iter()
                    .find(|f| f.fid == target)
                    .map(|f| f.name.clone())
                    .unwrap_or_else(|| format!("FID {target}")),
                eventid,
                task_path: format!("/manage/tasks/{eventid}"),
            }),
            Err(e) => return failure(state, e),
        }
    } else {
        None
    };
    let targets = forum_options(&rows, input.fid);
    let mut response = render(&ForumsTemplate {
        p: meta(
            state,
            crate::i18n::current().forum_forums_title().into(),
            crate::i18n::current().forum_forums_description().into(),
            "/manage/forums",
            "",
            "",
        ),
        rows,
        listed,
        icons,
        input,
        message,
        directory_statistics: settings.forum.directory_statistics,
        forum_statistics_seconds: settings.cache.forum_statistics_seconds,
        settings_saved: extra.saved == "settings",
        notice: match extra.saved.as_str() {
            "deleted" => crate::i18n::current().forum_forums_deleted(),
            "merging" => crate::i18n::current().forum_forums_merging(),
            "order" => crate::i18n::current().forum_forums_order_saved(),
            _ => "",
        },
        action_message: extra.action_message,
        order_message: extra.order_message,
        merge,
        targets,
        rewrite,
    });
    *response.status_mut() = status;
    response
}
async fn forums_page(Page(state): Page, Query(query): Query<QueryForm>) -> Response {
    forums_response_with(
        &state,
        query.fid,
        None,
        String::new(),
        StatusCode::OK,
        ForumsExtra {
            saved: query.saved,
            ..ForumsExtra::default()
        },
    )
    .await
}

#[derive(Deserialize)]
struct ForumSettingsForm {
    _csrf: String,
    #[serde(default)]
    directory_statistics: Option<String>,
}
csrf_forms!(ForumSettingsForm);

/// The "Forum settings" section at the top of `/manage/forums` (`settings.forum.`, global cache Q24, Q27):
/// saving goes through the site settings, locking the whole scalar settings range, separately from saving a single
/// forum. An unsubmitted checkbox means off.
async fn save_forum_settings(
    AdminPage(state): AdminPage,
    CsrfForm(form): CsrfForm<ForumSettingsForm>,
) -> Response {
    let actor = match actor(&state) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let enabled = form.directory_statistics.is_some();
    match crate::runtime::update(
        &state.db,
        &state.global,
        actor,
        "site.settings.save",
        |settings| settings.forum.directory_statistics = enabled,
    )
    .await
    {
        Ok(_) => auth::redirect("/manage/forums?saved=settings"),
        Err(error) => auth::identity_error(&state, error),
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ForumActionForm {
    _csrf: String,
    version: i64,
    target_fid: i64,
    reason: String,
}
csrf_forms!(ForumActionForm);

async fn forum_action_failure(state: &AppState, fid: i64, error: ContentWriteError) -> Response {
    let (status, message) = posting::write_error(error);
    forums_response_with(
        state,
        fid,
        None,
        String::new(),
        status,
        ForumsExtra {
            action_message: message,
            ..ForumsExtra::default()
        },
    )
    .await
}

/// Deletes an empty forum.
async fn delete_forum(
    Page(state): Page,
    AxumPath(fid): AxumPath<i64>,
    CsrfForm(form): CsrfForm<ForumActionForm>,
) -> Response {
    let actor = match actor(&state) {
        Ok(a) => a,
        Err(r) => return r,
    };
    match service::delete_forum(
        &state.db,
        actor,
        fid,
        form.version,
        &form.reason,
        state.config.media.orphan_seconds,
    )
    .await
    {
        // Moderator appointment rows are deleted with the forum, so the roster must be replaced; the forum cache must
        // drop it too.
        Ok(()) => match state
            .global
            .announce(&[
                crate::cache::Block::Moderators,
                crate::cache::Block::Forums,
                crate::cache::Block::Domains,
            ])
            .await
        {
            Ok(()) => auth::redirect("/manage/forums?saved=deleted"),
            Err(error) => database_error(&state, error),
        },
        Err(e) => forum_action_failure(&state, fid, e).await,
    }
}

/// Bulk ordering in the admin list: `order_{fid}` and `version_{fid}` per row, plus an operation reason.
async fn reorder_forums(
    Page(state): Page,
    CsrfForm(form): CsrfForm<std::collections::HashMap<String, String>>,
) -> Response {
    let actor = match actor(&state) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let mut orders = Vec::new();
    for (name, value) in &form {
        let Some(fid) = name
            .strip_prefix("order_")
            .and_then(|fid| fid.parse::<i64>().ok())
        else {
            continue;
        };
        let parsed = (
            value.trim().parse::<i64>(),
            form.get(&format!("version_{fid}"))
                .and_then(|version| version.parse::<i64>().ok()),
        );
        let (Ok(displayorder), Some(version)) = parsed else {
            return order_failure(
                &state,
                ContentWriteError::Validation(crate::i18n::current().forum_error_order_number()),
            )
            .await;
        };
        orders.push(service::ForumOrder {
            fid,
            displayorder,
            version,
        });
    }
    orders.sort_by_key(|order| order.fid);
    let why = form.get("reason").map(String::as_str).unwrap_or_default();
    match service::reorder_forums(&state.db, actor, &orders, why).await {
        // The order of the header navigation, drop-downs and directory page all comes from the in-process forum cache.
        Ok(_) => match state.global.announce(&[crate::cache::Block::Forums]).await {
            Ok(()) => auth::redirect("/manage/forums?saved=order"),
            Err(error) => database_error(&state, error),
        },
        Err(e) => order_failure(&state, e).await,
    }
}

async fn order_failure(state: &AppState, error: ContentWriteError) -> Response {
    let (status, message) = posting::write_error(error);
    forums_response_with(
        state,
        0,
        None,
        String::new(),
        status,
        ForumsExtra {
            order_message: message,
            ..ForumsExtra::default()
        },
    )
    .await
}

/// Queues a forum with content into an admin merge job.
async fn merge_forum(
    Page(state): Page,
    AxumPath(fid): AxumPath<i64>,
    CsrfForm(form): CsrfForm<ForumActionForm>,
) -> Response {
    let actor = match actor(&state) {
        Ok(a) => a,
        Err(r) => return r,
    };
    match service::request_merge(
        &state.db,
        actor,
        fid,
        form.target_fid,
        form.version,
        &form.reason,
    )
    .await
    {
        Ok(_) => auth::redirect(&format!("/manage/forums?fid={fid}&saved=merging")),
        Err(e) => forum_action_failure(&state, fid, e).await,
    }
}

/// One forum on the directory page.
struct DirectoryForum {
    slug: String,
    name: String,
    description: String,
    icon: String,
    count: Option<content::ForumCount>,
}
struct DirectoryGroup {
    forum: DirectoryForum,
    children: Vec<DirectoryForum>,
}
#[derive(Template)]
#[template(path = "site/forums.html")]
struct DirectoryTemplate<'a> {
    p: PageMeta<'a>,
    groups: Vec<DirectoryGroup>,
    statistics: bool,
}

/// The "All forums" directory page `/forums`: lists the forums this visitor can read,
/// grouped by parent forum, taken from the forum list in the page frame without querying the forum table again. Forums
/// the member has blocked (including child forums blocked along with their parent) are not listed: blocking means not
/// seeing it anywhere; hiding only affects the home page timeline, so hidden forums are listed as usual. When
/// `settings.forum.directory_statistics` is on, thread counts and today's posts are shown;
/// statistics count only public content readable by anonymous visitors, and members-only forums show no numbers.
async fn directory(Page(state): Page) -> Response {
    let forums = &state.frame.forums;
    let shown = |forum: &service::ForumRecord| !state.frame.forum_filters.is_blocked(forum.fid);
    let statistics = if state.frame.settings.forum.directory_statistics {
        match state.forum_statistics().await {
            Ok(statistics) => Some(statistics),
            Err(error) => return database_error(&state, error),
        }
    } else {
        None
    };
    let entry = |forum: &service::ForumRecord| DirectoryForum {
        slug: forum.slug.clone(),
        name: forum.name.clone(),
        description: forum.description.clone(),
        icon: forum
            .icon_aid
            .map(|aid| format!("/forum-icon/{}/{aid}", forum.fid))
            .unwrap_or_default(),
        count: statistics.as_ref().and_then(|statistics| {
            forum_count(forum, forums, statistics, |f, all| {
                anonymous_readable(f, all) && shown(f)
            })
        }),
    };
    let groups = forums
        .iter()
        .filter(|forum| forum.parent_fid.is_none() && shown(forum))
        .map(|parent| DirectoryGroup {
            forum: entry(parent),
            children: forums
                .iter()
                .filter(|child| child.parent_fid == Some(parent.fid) && shown(child))
                .map(entry)
                .collect(),
        })
        .collect();
    render(&DirectoryTemplate {
        p: meta(
            &state,
            crate::i18n::current().frame_all_forums().into(),
            crate::i18n::current().site_forums_description().into(),
            "/forums",
            "",
            "",
        ),
        groups,
        statistics: statistics.is_some(),
    })
}
async fn save_forum(Page(state): Page, CsrfForm(form): CsrfForm<ForumForm>) -> Response {
    let actor = match actor(&state) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let input = form.input();
    match service::save_forum_with_orphan_seconds(
        &state.db,
        actor,
        &input,
        state.config.media.orphan_seconds,
    )
    .await
    {
        // Changing the access scope, enabled state or parent moves pinned threads in or out of the index; name, URL,
        // description, guidance, icon, order and parent/child relations are all in the in-process forum cache (global
        // cache 3.4).
        Ok(fid) => match state
            .global
            .announce(&[
                crate::cache::Block::Pins,
                crate::cache::Block::Forums,
                // A new top-level forum joins the main domain; one turned into a child forum leaves every domain.
                crate::cache::Block::Domains,
            ])
            .await
        {
            Ok(()) => auth::redirect(&format!("/manage/forums?fid={fid}")),
            Err(error) => database_error(&state, error),
        },
        Err(e) => {
            let (status, message) = posting::write_error(e);
            forums_response(&state, input.fid, Some(input), message, status).await
        }
    }
}
#[derive(Template)]
#[template(path = "manage/thread_organization.html")]
struct ThreadTemplate<'a> {
    p: PageMeta<'a>,
    tid: i64,
    title: String,
    version: i64,
    /// Move targets: grouped by parent and child, marked disabled, admin-only and signed-in members.
    /// `(fid, display name)`.
    forums: Vec<(i64, String)>,
    fid: i64,
    key: String,
    message: String,
}
async fn thread_response(
    state: &AppState,
    tid: i64,
    form: Option<ThreadForm>,
    message: String,
    status: StatusCode,
) -> Response {
    let actor = match actor(state) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let pid =
        match crate::db::query_scalar::<i64>("SELECT pid FROM gonof_forum_post WHERE first_tid=?")
            .bind(tid)
            .fetch_optional(state.db.executor())
            .await
        {
            Ok(Some(p)) => p,
            Ok(None) => {
                return error_page(
                    state,
                    StatusCode::NOT_FOUND,
                    crate::i18n::current().forum_move_error_not_found(),
                );
            }
            Err(e) => return database_error(state, e.into()),
        };
    let editable = match editing::service(state).editable(actor, pid).await {
        Ok(p) => p,
        Err(e) => return failure(state, e),
    };
    // Trainee moderators and reviewers cannot move threads (user group decision Q9 of 2026-11-06); `require_scope`
    // refuses at submission as well.
    let full = state
        .viewer
        .session
        .as_ref()
        .is_some_and(|s| crate::content::edits::staff_can_edit(s.member.adminid));
    if !editable.can_manage || !full {
        return error_page(
            state,
            StatusCode::FORBIDDEN,
            crate::i18n::current().forum_move_error_forbidden(),
        );
    }
    let forums = match service::managed_forums(&state.db, actor).await {
        Ok(forums) => forum_options(&forums, 0),
        Err(e) => return failure(state, e),
    };
    let key = form
        .map(|f| f.request_key)
        .unwrap_or_else(identity::random_token);
    let mut response = render(&ThreadTemplate {
        p: meta(
            state,
            crate::i18n::current().forum_move_title().into(),
            crate::i18n::current().forum_move_description().into(),
            &format!("/manage/threads/{tid}/organization"),
            "",
            "",
        ),
        tid,
        title: editable.title,
        version: editable.thread_version,
        forums,
        fid: editable.fid,
        key,
        message,
    });
    *response.status_mut() = status;
    response
}
async fn thread_settings_manage(Page(state): Page, AxumPath(tid): AxumPath<i64>) -> Response {
    thread_response(&state, tid, None, String::new(), StatusCode::OK).await
}
async fn move_thread(
    Page(state): Page,
    AxumPath(tid): AxumPath<i64>,
    CsrfForm(form): CsrfForm<ThreadForm>,
) -> Response {
    let actor = match actor(&state) {
        Ok(a) => a,
        Err(r) => return r,
    };
    match service::move_thread(
        &state.db,
        actor,
        tid,
        form.target_fid,
        form.version,
        &form.request_key,
        &form.reason,
    )
    .await
    {
        // The moved level-2 pin has a new "home".
        Ok(()) => match pins_changed(&state).await {
            Some(response) => response,
            None => auth::redirect(&format!("/thread-{tid}")),
        },
        Err(e) => {
            let (status, message) = posting::write_error(e);
            thread_response(&state, tid, Some(form), message, status).await
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn native_forms_decode_numeric_fields() {
        use axum::extract::{Form, FromRequest};
        let request = axum::http::Request::builder().method("POST").header(header::CONTENT_TYPE,"application/x-www-form-urlencoded").body(axum::body::Body::from("_csrf=x&fid=12&parent_fid=3&icon_aid=4&status=1&version=2&name=hello&slug=hello&visibility=members")).unwrap();
        let input = Form::<ForumForm>::from_request(request, &())
            .await
            .unwrap()
            .0
            .input();
        assert_eq!(
            (input.fid, input.parent_fid, input.icon_aid, input.version),
            (12, 3, 4, 2)
        );
    }
}
