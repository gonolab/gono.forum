//! My attachments and the attachment trash: `/my/attachments`, `/my/attachments/trash` and three native form
//! actions.
//!
//! Rules: only attachments that passed the check and are not referenced can be moved to the trash; the trash keeps them
//! for `[media].trash_seconds` (30 days by default), during which they can be restored or deleted permanently; there is
//! no total space limit, usage is only displayed. The three actions affect only the member's own attachments and work
//! as usual in read-only mode.
use axum::{extract::Path as AxumPath, routing::post};

use super::extract::{CsrfForm, Page as PageState, csrf_forms};
use super::my::{Frame, fill_shell, frame};
use super::*;
use crate::media::{LIBRARY_PAGE_SIZE, LibraryItem, LibraryView, MediaError, Usage, UsageBucket};
use crate::rate_limit::Rule;

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/my/attachments", get(kept))
        .route("/my/attachments/trash", get(trash))
        .route("/my/attachments/{aid}/trash", post(move_to_trash))
        .route("/my/attachments/{aid}/restore", post(restore))
        .route("/my/attachments/{aid}/delete", post(delete))
}

#[derive(Deserialize)]
struct ListQuery {
    page: Option<usize>,
}

#[derive(Default, Deserialize)]
struct ActionForm {
    #[serde(default)]
    _csrf: String,
    /// List page number to return to after the action.
    #[serde(default)]
    page: usize,
    /// Which list the action was started from: `trash` is the trash, anything else the attachment list.
    #[serde(default)]
    from: String,
}
csrf_forms!(ActionForm);

/// One row in the usage table.
struct UsageLine {
    label: &'static str,
    files: i64,
    size: String,
    note: &'static str,
}

/// One row in the list; the status and available actions are all computed here, the template only displays them.
struct Row {
    aid: i64,
    filename: String,
    size: String,
    uploaded: String,
    state: String,
    href: Option<String>,
    post_href: Option<String>,
    can_trash: bool,
    can_delete: bool,
    blocked: &'static str,
    expires: String,
}

#[derive(Template)]
#[template(path = "my/attachments.html")]
struct AttachmentsTemplate<'a> {
    p: PageMeta<'a>,
    frame: Frame,
    trash_view: bool,
    usage: Vec<UsageLine>,
    total_files: i64,
    total_size: String,
    daily_used: String,
    daily_max: String,
    trash_days: i64,
    rows: Vec<Row>,
    total: usize,
    page: usize,
    message: String,
    previous: String,
    next: String,
}

/// Writes a byte count as a human-readable size, in steps of 1024.
pub(super) fn human_size(bytes: i64) -> String {
    let bytes = bytes.max(0) as f64;
    if bytes < 1024.0 {
        return format!("{bytes:.0} B");
    }
    let mut value = bytes / 1024.0;
    for unit in ["KiB", "MiB", "GiB"] {
        if value < 1024.0 {
            return format!("{value:.1} {unit}");
        }
        value /= 1024.0;
    }
    format!("{value:.1} TiB")
}

fn usage_lines(usage: &Usage) -> Vec<UsageLine> {
    let line = |label, bucket: UsageBucket, note| UsageLine {
        label,
        files: bucket.files,
        size: human_size(bucket.bytes),
        note,
    };
    let t = crate::i18n::current();
    let mut lines = vec![
        line(t.media_in_use(), usage.in_use, t.media_in_use_note()),
        line(t.media_unused(), usage.unused, t.media_unused_note()),
        line(t.media_in_trash(), usage.trash, t.media_in_trash_note()),
    ];
    if usage.checking.files > 0 {
        lines.push(line(
            t.media_checking(),
            usage.checking,
            t.media_checking_note(),
        ));
    }
    if usage.rejected > 0 {
        lines.push(UsageLine {
            label: t.media_rejected(),
            files: usage.rejected,
            size: "—".into(),
            note: t.media_rejected_note(),
        });
    }
    lines
}

fn row(item: LibraryItem, view: LibraryView, now: i64) -> Row {
    let format = crate::clock::current();
    let t = crate::i18n::current();
    let upload = item.upload;
    let ready = upload.is_ready();
    let in_use = ready && upload.purge_after.is_none();
    let state = match (view, upload.lifecycle.as_str()) {
        (LibraryView::Trash, _) => t.media_state_trash().to_owned(),
        (_, "pending" | "processing") => t.media_checking().to_owned(),
        (_, "rejected") => {
            if upload.scan_detail.is_empty() {
                t.media_rejected().to_owned()
            } else {
                t.media_state_rejected(&upload.scan_detail)
            }
        }
        _ if in_use && item.posts > 0 => t.media_state_posts(item.posts),
        _ if in_use && item.avatar => t.media_state_avatar().to_owned(),
        _ if in_use => t.media_state_site().to_owned(),
        _ => match upload.purge_after {
            Some(expiry) if expiry > now => {
                t.media_state_expires(crate::clock::date(expiry, &format))
            }
            _ => t.media_state_expiring().to_owned(),
        },
    };
    // A referenced attachment can only be read through the post's address; an unreferenced one is read as "owner
    // preview".
    let href = ready.then(|| match item.first_pid {
        Some(pid) => format!("/media/{}?pid={pid}", upload.aid),
        None => format!("/media/{}", upload.aid),
    });
    let blocked = if view == LibraryView::Kept && in_use {
        t.media_blocked()
    } else {
        ""
    };
    Row {
        aid: upload.aid,
        size: human_size(upload.filesize),
        uploaded: crate::clock::date(upload.dateline, &format),
        href,
        post_href: item.first_pid.map(|pid| format!("/posts/{pid}")),
        can_trash: view == LibraryView::Kept && ready && !in_use,
        can_delete: view == LibraryView::Trash || upload.lifecycle == "rejected",
        blocked,
        expires: match (view, upload.purge_after) {
            (LibraryView::Trash, Some(expiry)) => crate::clock::date(expiry, &format),
            _ => String::new(),
        },
        state,
        filename: upload.filename,
    }
}

async fn kept(PageState(state): PageState, Query(query): Query<ListQuery>) -> Response {
    list(&state, query.page, LibraryView::Kept, String::new()).await
}

async fn trash(PageState(state): PageState, Query(query): Query<ListQuery>) -> Response {
    list(&state, query.page, LibraryView::Trash, String::new()).await
}

fn view_path(view: LibraryView) -> &'static str {
    match view {
        LibraryView::Kept => "/my/attachments",
        LibraryView::Trash => "/my/attachments/trash",
    }
}

async fn list(
    state: &AppState,
    page: Option<usize>,
    view: LibraryView,
    message: String,
) -> Response {
    let path = view_path(view);
    let Some(session) = state.viewer.session.as_ref() else {
        return auth::redirect(&format!(
            "/login?return_to={}",
            url::form_urlencoded::byte_serialize(path.as_bytes()).collect::<String>()
        ));
    };
    let page = page.unwrap_or(1);
    if page == 0 {
        return error_page(
            state,
            StatusCode::BAD_REQUEST,
            crate::i18n::current().media_error_page(),
        );
    }
    let library = match state
        .media
        .library(
            session.member.actor(),
            view,
            page,
            state.frame.settings.time.offset_minutes,
        )
        .await
    {
        Ok(library) => library,
        Err(error) => return media_failure(state, error),
    };
    let last = library.total.div_ceil(LIBRARY_PAGE_SIZE).max(1);
    if page > last {
        return error_page(
            state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().common_error_no_such_page(),
        );
    }
    let page_id = match view {
        LibraryView::Kept => "attachments",
        LibraryView::Trash => "attachments-trash",
    };
    let entry = super::my::entry(page_id).expect("the directory must contain this page");
    let mut p = meta(
        state,
        entry.title().into(),
        crate::i18n::current().media_description().into(),
        path,
        "",
        "",
    );
    if let Some(error) = fill_shell(state, &mut p).await {
        return error;
    }
    let now = crate::identity::now();
    let stored = library.usage.stored();
    let rows = library
        .items
        .into_iter()
        .map(|item| row(item, view, now))
        .collect();
    render(&AttachmentsTemplate {
        p,
        frame: frame(page_id),
        trash_view: view == LibraryView::Trash,
        usage: usage_lines(&library.usage),
        total_files: stored.files,
        total_size: human_size(stored.bytes),
        daily_used: human_size(i64::try_from(library.usage.daily_used).unwrap_or(i64::MAX)),
        daily_max: human_size(i64::try_from(library.usage.daily_max).unwrap_or(i64::MAX)),
        trash_days: state.media.trash_seconds() / 86400,
        rows,
        total: library.total,
        page,
        message,
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

#[derive(Clone, Copy)]
enum Action {
    Trash,
    Restore,
    Delete,
}

async fn move_to_trash(
    state: PageState,
    aid: AxumPath<i64>,
    form: CsrfForm<ActionForm>,
) -> Response {
    act(state, aid, form, Action::Trash).await
}

async fn restore(state: PageState, aid: AxumPath<i64>, form: CsrfForm<ActionForm>) -> Response {
    act(state, aid, form, Action::Restore).await
}

async fn delete(state: PageState, aid: AxumPath<i64>, form: CsrfForm<ActionForm>) -> Response {
    act(state, aid, form, Action::Delete).await
}

/// A successful action returns to the original list (303); a business conflict stays on the list page with the
/// reason (409); anything else goes to the common error page.
async fn act(
    PageState(state): PageState,
    AxumPath(aid): AxumPath<i64>,
    CsrfForm(form): CsrfForm<ActionForm>,
    action: Action,
) -> Response {
    let Some(session) = state.viewer.session.as_ref() else {
        return auth::redirect("/login?return_to=%2Fmy%2Fattachments");
    };
    // Delete and restore are both rechecked under the attachment row lock; rate limiting only guards against
    // accidental operations hammering the database.
    match state
        .limit(&[Rule::new("attachment_library", session.member.uid, 60, 300)])
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            return error_page(
                &state,
                StatusCode::TOO_MANY_REQUESTS,
                crate::i18n::current().media_error_rate(),
            );
        }
        Err(unavailable) => return unavailable,
    }
    let actor = session.member.actor();
    let view = if form.from == "trash" {
        LibraryView::Trash
    } else {
        LibraryView::Kept
    };
    // "Delete permanently" in the trash and deleting an attachment that failed the check in the attachment
    // list are both the existing attachment cancellation: cleaned up immediately.
    let result = match action {
        Action::Trash => state.media.trash_actor(actor, aid).await,
        Action::Restore => state.media.restore_actor(actor, aid).await,
        Action::Delete => state.media.cancel_actor(actor, aid).await,
    };
    let mut page = form.page.max(1);
    match result {
        Ok(()) => {
            // After deleting the last item on the last page, the original page number may no longer exist; go back to
            // the new last page.
            if page > 1 {
                let offset = state.frame.settings.time.offset_minutes;
                let usage = match state.media.usage(actor, offset).await {
                    Ok(usage) => usage,
                    Err(error) => return media_failure(&state, error),
                };
                let total = match view {
                    LibraryView::Kept => usage.kept_files(),
                    LibraryView::Trash => usage.trash.files,
                };
                let last = usize::try_from(total)
                    .unwrap_or(0)
                    .div_ceil(LIBRARY_PAGE_SIZE)
                    .max(1);
                page = page.min(last);
            }
            let path = view_path(view);
            auth::redirect(&if page > 1 {
                format!("{path}?page={page}")
            } else {
                path.to_owned()
            })
        }
        Err(MediaError::Conflict(message)) => {
            let mut response = list(&state, Some(1), view, message).await;
            *response.status_mut() = StatusCode::CONFLICT;
            response
        }
        Err(error) => media_failure(&state, error),
    }
}

#[cfg(test)]
mod tests {
    use super::human_size;

    #[test]
    fn sizes_use_binary_units() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1024), "1.0 KiB");
        assert_eq!(human_size(5 * 1024 * 1024 + 512 * 1024), "5.5 MiB");
        assert_eq!(human_size(200 * 1024 * 1024), "200.0 MiB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }
}
