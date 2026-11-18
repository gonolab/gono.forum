//! `/my/feed`: turning on, regenerating and disabling the personal feed URL, and the "Include members-only threads"
//! switch.
//! The token plaintext is shown only once after generation; afterwards the page only shows its first 8 characters.
//! A successful generation does not render the plaintext directly: the plaintext goes into a one-time encrypted cookie
//! sent only to this page, followed by a 303 back to GET this page, which clears it once read; a refresh shows the GET
//! page and does not resubmit the generation form (see `identity::SecretReveal`).
use super::extract::{CheckedForm, CsrfForm, Page, csrf_forms};
use super::*;
use crate::rate_limit::Rule;
use crate::{
    content::{feed_tokens, writes::ContentWriteError},
    urls::FeedFormat,
};
use axum::{http::HeaderMap, routing::post};

const PAGE: &str = "/my/feed";
/// Generating and regenerating: 10 times per 5 minutes per account.
const ISSUE_LIMIT: (u32, i64) = (10, 300);

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(PAGE, get(feed_page))
        .route("/my/feed/enable", post(issue))
        .route("/my/feed/rotate", post(issue))
        .route("/my/feed/members", post(members))
        .route("/my/feed/disable", post(disable))
}

#[derive(Default, Deserialize)]
struct ActionForm {
    #[serde(default)]
    _csrf: String,
}
csrf_forms!(ActionForm);

#[derive(Default, Deserialize)]
struct MembersForm {
    #[serde(default)]
    _csrf: String,
    #[serde(default)]
    include_members: bool,
}
csrf_forms!(MembersForm);

#[derive(Default, Deserialize)]
struct FeedQuery {
    #[serde(default)]
    disabled: bool,
    #[serde(default)]
    saved: bool,
}

struct TokenView {
    hint: String,
    created: String,
    include_members: bool,
}

/// The one URL shown right after generation.
struct AddressView {
    label: String,
    note: &'static str,
    rss: String,
    atom: String,
}

#[derive(Template)]
#[template(path = "settings/feed.html")]
struct FeedTemplate<'a> {
    p: PageMeta<'a>,
    frame: crate::router::my::Frame,
    token: Option<TokenView>,
    addresses: Vec<AddressView>,
    message: &'a str,
    success: bool,
}

/// One for the whole site, plus one for each forum the member can read right now and has not blocked.
fn addresses(state: &AppState, secret: &str) -> Vec<AddressView> {
    let base = state.site_url().trim_end_matches('/').to_owned();
    let address =
        |slug: &str, format| format!("{base}{}", urls::private_feed(secret, slug, format));
    let filters = &state.frame.forum_filters;
    let mut list = vec![AddressView {
        label: crate::i18n::current().settings_feed_whole_site().into(),
        note: if state
            .frame
            .forums
            .iter()
            .any(|forum| filters.effective(forum.fid).is_some())
        {
            crate::i18n::current().settings_feed_whole_site_note()
        } else {
            ""
        },
        rss: address("", FeedFormat::Rss),
        atom: address("", FeedFormat::Atom),
    }];
    // Grouped by parent and child, with child forums carrying an indentation prefix; a top-level forum's feed includes
    // its child forums (blocked child forums are left out).
    for (forum, child) in
        crate::organization::grouped_by_parent(&state.frame.forums, |f| (f.fid, f.parent_fid))
    {
        if filters.is_blocked(forum.fid) {
            continue;
        }
        list.push(AddressView {
            label: if child {
                format!("{}{}", crate::organization::CHILD_PREFIX, forum.name)
            } else {
                forum.name.clone()
            },
            // Members-only forums return 404 like public feeds while "Include members-only threads" is off.
            note: if super::syndication::forum_public(forum, &state.frame.forums) {
                ""
            } else {
                crate::i18n::current().settings_feed_members_forum_note()
            },
            rss: address(&forum.slug, FeedFormat::Rss),
            atom: address(&forum.slug, FeedFormat::Atom),
        });
    }
    list
}

async fn feed_response(
    state: &AppState,
    secret: &str,
    message: &str,
    success: bool,
    status: StatusCode,
) -> Response {
    let Some(session) = &state.viewer.session else {
        return login();
    };
    let token = match feed_tokens::get(&state.db, session.member.uid).await {
        Ok(token) => token.map(|token| TokenView {
            hint: token.hint,
            created: crate::moderation::display_time(token.dateline),
            include_members: token.include_members,
        }),
        Err(error) => return database_error(state, error),
    };
    // The plaintext in the reveal cookie must still be the current token: once regenerated or disabled since, it is
    // no longer shown.
    let secret = match &token {
        Some(token) if !secret.is_empty() && secret.starts_with(&token.hint) => secret,
        _ => "",
    };
    let (message, success) = if secret.is_empty() {
        (message, success)
    } else {
        (crate::i18n::current().settings_feed_created_notice(), true)
    };
    let mut p = meta(
        state,
        crate::i18n::current().settings_feed_title().into(),
        crate::i18n::current().settings_feed_description().into(),
        PAGE,
        "",
        "",
    );
    if let Some(error) = crate::router::my::fill_shell(state, &mut p).await {
        return error;
    }
    let mut response = render(&FeedTemplate {
        frame: crate::router::my::frame("feed"),
        p,
        token,
        addresses: if secret.is_empty() {
            Vec::new()
        } else {
            addresses(state, secret)
        },
        message,
        success,
    });
    *response.status_mut() = status;
    response
}

fn login() -> Response {
    auth::redirect("/login?return_to=%2Fmy%2Ffeed")
}

/// Toggling or disabling before it has been turned on gets one clear sentence; an expired login goes back to the
/// login page; everything else follows the posting error wording.
async fn failure(state: &AppState, error: ContentWriteError) -> Response {
    let (status, message) = match error {
        ContentWriteError::Unauthorized => return login(),
        ContentWriteError::NotFound => (
            StatusCode::UNPROCESSABLE_ENTITY,
            crate::i18n::current().settings_error_feed_off().to_owned(),
        ),
        other => posting::write_error(other),
    };
    feed_response(state, "", &message, false, status).await
}

/// The single GET after generation takes the plaintext from the reveal cookie, shows it and clears the cookie: it does
/// not appear again on refresh.
async fn feed_page(
    Page(state): Page,
    headers: HeaderMap,
    Query(query): Query<FeedQuery>,
) -> Response {
    let (message, success) = if query.disabled {
        (crate::i18n::current().settings_feed_disabled_notice(), true)
    } else if query.saved {
        (crate::i18n::current().settings_feed_saved_notice(), true)
    } else {
        ("", false)
    };
    let secret = state
        .viewer
        .session
        .as_ref()
        .and_then(|session| auth::FEED_REVEAL.read(&state, &headers, session.member.uid))
        .unwrap_or_default();
    let mut response = feed_response(&state, &secret, message, success, StatusCode::OK).await;
    auth::FEED_REVEAL.set(&mut response, &state.viewer, "", 0);
    response
}

/// Turning on and regenerating use the same transaction: the old digest is replaced under the lock; the new URL's
/// plaintext goes into the reveal cookie and a 303 back to this page shows it once.
async fn issue(Page(state): Page, form: CheckedForm<ActionForm>) -> Response {
    match form {
        CheckedForm::Valid(_) => {}
        CheckedForm::Malformed => {
            return feed_response(
                &state,
                "",
                crate::i18n::current().common_error_form(),
                false,
                StatusCode::BAD_REQUEST,
            )
            .await;
        }
        CheckedForm::StaleToken(..) => {
            return feed_response(
                &state,
                "",
                crate::i18n::current().account_error_stale(),
                false,
                StatusCode::FORBIDDEN,
            )
            .await;
        }
    }
    let Some(session) = &state.viewer.session else {
        return login();
    };
    match state
        .limit(&[Rule::new(
            "feed-token",
            session.member.uid,
            ISSUE_LIMIT.0,
            ISSUE_LIMIT.1,
        )])
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            return feed_response(
                &state,
                "",
                crate::i18n::current().settings_error_too_frequent(),
                false,
                StatusCode::TOO_MANY_REQUESTS,
            )
            .await;
        }
        Err(unavailable) => return unavailable,
    }
    match feed_tokens::issue(&state.db, session.member.actor()).await {
        Ok(secret) => auth::FEED_REVEAL.redirect(&state, session.member.uid, &secret, PAGE),
        Err(error) => failure(&state, error).await,
    }
}

async fn members(Page(state): Page, CsrfForm(form): CsrfForm<MembersForm>) -> Response {
    let Some(session) = &state.viewer.session else {
        return login();
    };
    match feed_tokens::set_include_members(&state.db, session.member.actor(), form.include_members)
        .await
    {
        Ok(()) => auth::redirect("/my/feed?saved=true"),
        Err(error) => failure(&state, error).await,
    }
}

async fn disable(Page(state): Page, CsrfForm(_): CsrfForm<ActionForm>) -> Response {
    let Some(session) = &state.viewer.session else {
        return login();
    };
    match feed_tokens::disable(&state.db, session.member.actor()).await {
        Ok(_) => auth::redirect("/my/feed?disabled=true"),
        Err(error) => failure(&state, error).await,
    }
}
