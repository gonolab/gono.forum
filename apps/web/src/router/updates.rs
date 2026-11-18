//! Tools & maintenance › Updates: check for a newer release and upgrade in place. The work is in [`crate::updates`];
//! this page shows its state, starts a check or an upgrade, and explains why one-click upgrade is unavailable when it
//! is (source builds, not under systemd, read-only directory).
use super::extract::{AdminPage, CsrfForm};
use super::*;
use axum::routing::post;
use std::collections::HashMap;

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/manage/updates", get(page))
        .route("/manage/updates/check", post(check))
        .route("/manage/updates/apply", post(apply))
}

#[derive(Template)]
#[template(path = "manage/updates.html")]
struct UpdatesPage<'a> {
    p: PageMeta<'a>,
    version: &'static str,
    platform: &'static str,
    releases_url: &'a str,
    checked_at: String,
    check_error: String,
    latest: String,
    /// The package digest of `latest`, sent back with the upgrade form so that the upgrade installs what the page showed.
    latest_sha256: String,
    newer: bool,
    running: String,
    failure: String,
    notice: &'a str,
    alert: &'a str,
    blockers: Vec<&'static str>,
    previous: bool,
    last: Option<LastUpgrade>,
}

struct LastUpgrade {
    from: String,
    to: String,
    rolled_back: bool,
    /// The web binary was upgraded but the command-line tool does not report its version.
    incomplete: bool,
    /// Switched by hand with `forum.install.sh rollback`.
    switched: bool,
    at: String,
}

#[derive(Default, Deserialize)]
struct NoticeQuery {
    #[serde(default)]
    notice: String,
}

fn show(state: &AppState, notice: &str, alert: &str, status: StatusCode) -> Response {
    let format = crate::clock::current();
    let current = state.updater.status();
    let blockers = crate::updates::blockers(&state.instance);
    let last = crate::updates::last(&state.instance).map(|outcome| LastUpgrade {
        rolled_back: outcome.result == "rolled_back",
        incomplete: outcome.result == "incomplete",
        switched: outcome.result == "switched",
        from: outcome.from,
        to: outcome.to,
        at: if outcome.at > 0 {
            crate::clock::datetime(outcome.at, &format)
        } else {
            String::new()
        },
    });
    let mut response = render(&UpdatesPage {
        p: meta(
            state,
            crate::i18n::current().maintenance_updates_title().into(),
            crate::i18n::current()
                .maintenance_updates_description()
                .into(),
            "/manage/updates",
            "",
            "",
        ),
        version: crate::VERSION,
        platform: crate::updates::platform().unwrap_or(""),
        releases_url: &state.config.update.releases_url,
        checked_at: current
            .checked_at
            .map(|at| crate::clock::datetime(at, &format))
            .unwrap_or_default(),
        check_error: current.check_error,
        newer: current
            .release
            .as_ref()
            .is_some_and(crate::updates::Release::is_newer),
        latest_sha256: current
            .release
            .as_ref()
            .map(|release| release.sha256.clone())
            .unwrap_or_default(),
        latest: current
            .release
            .map(|release| release.version)
            .unwrap_or_default(),
        running: current.running,
        failure: current.failure,
        notice,
        alert,
        blockers,
        previous: state.instance.previous_exists(),
        last,
    });
    *response.status_mut() = status;
    response
}

async fn page(AdminPage(state): AdminPage, Query(query): Query<NoticeQuery>) -> Response {
    let notice = match query.notice.as_str() {
        // Only while the upgrade runs: the page keeps reloading this address, and once the new version answers (or
        // the upgrade stopped) it shows the outcome instead of "has started".
        "started" if !state.updater.status().running.is_empty() => {
            crate::i18n::current().maintenance_updates_started()
        }
        _ => "",
    };
    show(&state, notice, "", StatusCode::OK)
}

/// Reads `checksums.txt` from the release source. Nothing is written; the result lives in this process until the
/// next check.
async fn check(
    AdminPage(state): AdminPage,
    CsrfForm(_): CsrfForm<HashMap<String, String>>,
) -> Response {
    state.updater.check(&state.config.update).await;
    auth::redirect("/manage/updates")
}

/// Starts the upgrade to the release the page showed (the form's `version` and `sha256`); a check run since, from
/// another tab or by another administrator, that found another release makes it refuse with 409.
async fn apply(
    AdminPage(state): AdminPage,
    CsrfForm(form): CsrfForm<HashMap<String, String>>,
) -> Response {
    let field = |name: &str| form.get(name).cloned().unwrap_or_default();
    let confirmed = crate::updates::Confirmed {
        version: field("version"),
        sha256: field("sha256"),
    };
    match state.updater.start(
        state.config.update.clone(),
        state.instance.clone(),
        &confirmed,
    ) {
        Ok(()) => auth::redirect("/manage/updates?notice=started"),
        Err(message) => show(&state, "", &message, StatusCode::CONFLICT),
    }
}
