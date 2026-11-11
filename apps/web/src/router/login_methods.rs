//! Admin "Login methods": the site-wide default login entry points and
//! participation conditions, two-step verification for SMS and third-party logins, SMS limits (scalar settings, saved
//! through the global settings), the credentials of the SMS provider and of Google and GitHub (the `login.providers`
//! row), and the callback URLs each domain must register with the providers. Each domain's own login entry points and
//! participation conditions are on the "Multiple domains" edit page.
use std::collections::HashMap;

use super::extract::{AdminPage, CsrfForm, Page};
use super::*;
use crate::identity::IdentityError;
use crate::oauth::Provider;
use crate::runtime::settings::{EXTERNAL_TWO_FACTOR, LOGIN_METHODS, PARTICIPATION, set_contains};
use axum::extract::Path;
use axum::routing::post;

/// Maximum number of callback URLs a GitHub App can register.
const GITHUB_CALLBACK_LIMIT: usize = 10;

const PATH: &str = "/manage/login";

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(PATH, get(page).post(save))
        .route("/manage/login/sms", post(save_provider))
        .route("/manage/login/oauth/{provider}", post(save_oauth))
}

/// How one third-party login looks on the admin page.
struct OAuthView {
    value: &'static str,
    label: &'static str,
    app: crate::oauth::OAuthApp,
    /// Callback URLs to register for the sites (the site-wide default or a domain) that have this method checked.
    callbacks: Vec<String>,
    /// Some site has it checked, but the credentials are not configured yet: the login page does not show it.
    checked_unconfigured: bool,
    /// The callback URLs exceed the GitHub App's registration limit.
    over_limit: bool,
}

#[derive(Template)]
#[template(path = "manage/login.html")]
struct LoginTemplate<'a> {
    p: PageMeta<'a>,
    message: String,
    notice: &'static str,
    /// Login entry points and participation conditions: value, name, whether checked.
    methods: Vec<(&'static str, &'static str, bool)>,
    participation: Vec<(&'static str, &'static str, bool)>,
    external_two_factor: String,
    sms: crate::runtime::settings::SmsSettings,
    provider: crate::sms::SmsProvider,
    oauth: Vec<OAuthView>,
    revision: i64,
    transports: Vec<(&'static str, &'static str)>,
    key_configured: bool,
    sent_today: i64,
    domains_enabled: bool,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct PageQuery {
    saved: String,
}

async fn view(
    state: &AppState,
    shown: &crate::runtime::SiteSettings,
    provider: Option<crate::sms::Providers>,
    message: String,
    notice: &'static str,
    status: StatusCode,
) -> Response {
    let Some(session) = &state.viewer.session else {
        return auth::redirect("/login?return_to=/manage/login");
    };
    let providers = match provider {
        Some(providers) => providers,
        None => match crate::sms::read(&state.db, session.member.actor()).await {
            Ok(providers) => providers,
            Err(error) => return auth::identity_error(state, error),
        },
    };
    let sent_today = match state.identity.sms_sent_today().await {
        Ok(count) => count,
        Err(error) => return auth::identity_error(state, error),
    };
    let checked = |labels: &[(&'static str, &'static str)], set: &str| {
        labels
            .iter()
            .map(|(value, label)| (*value, *label, set_contains(set, value)))
            .collect()
    };
    let sites = match sites(state, shown).await {
        Ok(sites) => sites,
        Err(error) => return auth::identity_error(state, error),
    };
    let oauth = Provider::ALL
        .into_iter()
        .map(|provider| {
            let callbacks: Vec<String> = sites
                .iter()
                .filter(|(_, methods)| set_contains(methods, provider.value()))
                .map(|(url, _)| super::auth::callback_url(url, provider))
                .collect();
            let app = providers.oauth(provider).clone();
            OAuthView {
                value: provider.value(),
                label: provider.label(),
                checked_unconfigured: !callbacks.is_empty() && !app.configured(),
                over_limit: provider == Provider::Github && callbacks.len() > GITHUB_CALLBACK_LIMIT,
                app,
                callbacks,
            }
        })
        .collect();
    let mut response = render(&LoginTemplate {
        p: meta(
            state,
            crate::i18n::current().manage_tab_login_methods().into(),
            crate::i18n::current().account_manage_description().into(),
            PATH,
            "",
            "",
        ),
        message,
        notice,
        methods: checked(&crate::login::method_labels(), &shown.login.methods),
        participation: checked(
            &crate::login::participation_labels(),
            &shown.login.participation,
        ),
        external_two_factor: shown.login.external_two_factor.clone(),
        sms: shown.sms.clone(),
        provider: providers.sms.clone(),
        oauth,
        revision: providers.revision,
        transports: crate::sms::transports(),
        key_configured: state.identity.providers_key_configured(),
        sent_today,
        domains_enabled: shown.domains.enabled,
    });
    *response.status_mut() = status;
    response
}

/// Each site's URL and effective login entry points: the main domain (forum URL) uses the site-wide settings; with
/// multi-domain enabled, other enabled domains with their own values use those.
async fn sites(
    state: &AppState,
    shown: &crate::runtime::SiteSettings,
) -> Result<Vec<(String, String)>, IdentityError> {
    let main = state.site_url().trim_end_matches('/').to_owned();
    let mut sites = vec![(main.clone(), shown.login.methods.clone())];
    if shown.domains.enabled {
        let map = crate::domains::DomainMap::load(&state.db)
            .await
            .map_err(IdentityError::Internal)?;
        for row in map.rows() {
            if !row.enabled || row.site_url == main {
                continue;
            }
            let methods = row
                .login_methods
                .clone()
                .unwrap_or_else(|| shown.login.methods.clone());
            sites.push((row.site_url.clone(), methods));
        }
    }
    Ok(sites)
}

async fn page(AdminPage(state): AdminPage, Query(query): Query<PageQuery>) -> Response {
    let t = crate::i18n::current();
    let notice = match query.saved.as_str() {
        "1" => t.account_manage_saved(),
        "sms" => t.account_manage_sms_saved(),
        "google" => t.account_manage_google_saved(),
        "github" => t.account_manage_github_saved(),
        _ => "",
    };
    let shown = match crate::runtime::read(&state.db).await {
        Ok(settings) => settings,
        Err(error) => return auth::identity_error(&state, error),
    };
    view(&state, &shown, None, String::new(), notice, StatusCode::OK).await
}

/// The checked set joined with commas in constant order (`{prefix}{value}=1`).
fn picked(form: &HashMap<String, String>, prefix: &str, allowed: &[&str]) -> String {
    allowed
        .iter()
        .filter(|item| {
            form.get(&format!("{prefix}{item}"))
                .is_some_and(|value| value == "1")
        })
        .copied()
        .collect::<Vec<_>>()
        .join(",")
}

/// Applies the form to the settings: a failed number parse is an error; everything else is left to the overall
/// validation at save time.
fn apply(
    form: &HashMap<String, String>,
    settings: &mut crate::runtime::SiteSettings,
) -> Result<(), &'static str> {
    settings.login.methods = picked(form, "method_", LOGIN_METHODS);
    settings.login.participation = picked(form, "participation_", PARTICIPATION);
    let choice = form
        .get("external_two_factor")
        .map(String::as_str)
        .unwrap_or("");
    if !EXTERNAL_TWO_FACTOR.contains(&choice) {
        return Err(crate::i18n::current().account_manage_error_two_factor());
    }
    settings.login.external_two_factor = choice.to_owned();
    let number = |key: &str| -> Result<i64, &'static str> {
        form.get(key)
            .map(|value| value.trim())
            .and_then(|value| value.parse().ok())
            .ok_or(crate::i18n::current().account_manage_error_sms_integer())
    };
    settings.sms.mobile_interval_seconds = number("sms_mobile_interval_seconds")?;
    settings.sms.mobile_daily_limit = number("sms_mobile_daily_limit")?;
    settings.sms.ip_hourly_limit = number("sms_ip_hourly_limit")?;
    settings.sms.site_daily_limit = number("sms_site_daily_limit")?;
    settings.sms.code_ttl_minutes = number("sms_code_ttl_minutes")?;
    settings.sms.max_attempts = number("sms_max_attempts")?;
    Ok(())
}

async fn save(Page(state): Page, CsrfForm(form): CsrfForm<HashMap<String, String>>) -> Response {
    let Some(session) = &state.viewer.session else {
        return auth::redirect("/login?return_to=/manage/login");
    };
    let actor = session.member.actor();
    let mut shown = match crate::runtime::read(&state.db).await {
        Ok(settings) => settings,
        Err(error) => return auth::identity_error(&state, error),
    };
    let failure = match apply(&form, &mut shown) {
        Err(message) => Some(message.to_owned()),
        Ok(()) => match crate::runtime::settings::check(&shown) {
            Err(error) => Some(error.expected.to_owned()),
            Ok(_) => None,
        },
    };
    if let Some(message) = failure {
        return view(
            &state,
            &shown,
            None,
            message,
            "",
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    }
    match crate::runtime::update(
        &state.db,
        &state.global,
        actor,
        "site.settings.save",
        |settings| {
            let _ = apply(&form, settings);
        },
    )
    .await
    {
        Ok(_) => auth::redirect(&format!("{PATH}?saved=1")),
        Err(IdentityError::Validation(message) | IdentityError::Conflict(message)) => {
            view(
                &state,
                &shown,
                None,
                message.into(),
                "",
                StatusCode::UNPROCESSABLE_ENTITY,
            )
            .await
        }
        Err(error) => auth::identity_error(&state, error),
    }
}

async fn save_provider(
    Page(state): Page,
    CsrfForm(form): CsrfForm<HashMap<String, String>>,
) -> Response {
    let Some(session) = &state.viewer.session else {
        return auth::redirect("/login?return_to=/manage/login");
    };
    let text = |key: &str| form.get(key).cloned().unwrap_or_default();
    let input = crate::sms::SmsInput {
        revision: text("revision").trim().parse().unwrap_or(-1),
        sms: crate::sms::SmsProvider {
            transport: text("transport"),
            access_key_id: text("access_key_id"),
            sign_name: text("sign_name"),
            template_code: text("template_code"),
            has_secret: false,
        },
        secret: text("access_key_secret"),
    };
    match state
        .identity
        .save_sms_provider(session.member.actor(), input)
        .await
    {
        Ok(_) => auth::redirect(&format!("{PATH}?saved=sms")),
        Err(IdentityError::Internal(error))
            if error.downcast_ref::<crate::sms::FormError>().is_some() =>
        {
            // Form problems (required fields, format, key configuration) are shown on the page as is; other internal
            // errors go to the common error page.
            let message = error.to_string();
            let shown = match crate::runtime::read(&state.db).await {
                Ok(settings) => settings,
                Err(error) => return auth::identity_error(&state, error),
            };
            tracing::info!(error = %message, "SMS provider configuration was not saved");
            view(
                &state,
                &shown,
                None,
                message,
                "",
                StatusCode::UNPROCESSABLE_ENTITY,
            )
            .await
        }
        Err(IdentityError::Conflict(message)) => error_page(&state, StatusCode::CONFLICT, message),
        Err(error) => auth::identity_error(&state, error),
    }
}

async fn save_oauth(
    Page(state): Page,
    Path(provider): Path<String>,
    CsrfForm(form): CsrfForm<HashMap<String, String>>,
) -> Response {
    let Some(session) = &state.viewer.session else {
        return auth::redirect("/login?return_to=/manage/login");
    };
    let Some(provider) = Provider::parse(&provider) else {
        return error_page(
            &state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().common_error_page_not_found(),
        );
    };
    let text = |key: &str| form.get(key).cloned().unwrap_or_default();
    let input = crate::sms::OAuthInput {
        revision: text("revision").trim().parse().unwrap_or(-1),
        provider,
        client_id: text("client_id"),
        secret: text("client_secret"),
    };
    match state
        .identity
        .save_oauth_provider(session.member.actor(), input)
        .await
    {
        Ok(_) => auth::redirect(&format!("{PATH}?saved={}", provider.value())),
        Err(IdentityError::Internal(error))
            if error.downcast_ref::<crate::sms::FormError>().is_some() =>
        {
            let message = crate::i18n::current()
                .account_manage_oauth_error(provider.label(), error.to_string());
            let shown = match crate::runtime::read(&state.db).await {
                Ok(settings) => settings,
                Err(error) => return auth::identity_error(&state, error),
            };
            tracing::info!(error = %message, "third-party login credentials were not saved");
            view(
                &state,
                &shown,
                None,
                message,
                "",
                StatusCode::UNPROCESSABLE_ENTITY,
            )
            .await
        }
        Err(IdentityError::Conflict(message)) => error_page(&state, StatusCode::CONFLICT, message),
        Err(error) => auth::identity_error(&state, error),
    }
}
