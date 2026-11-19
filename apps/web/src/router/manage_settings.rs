//! Admin settings tabs (docs/settings-and-caching.md, "Keys and groups" and "Saving settings").
//!
//! Each tab (or block within a page) only changes its own fields: saving goes through [`runtime::update`], which
//! changes this page's fields on the current values read under the lock, writes only the rows that changed, and after
//! commit clears this process's settings block and broadcasts. Edit pages read current values from the database, not
//! the cache.
//!
//! Numbers, switches, single choices and multi-line text are listed per tab as field tables ([`Field`]), rendered by
//! one section template and parsed by one piece of code:
//!
//! - The field table only maps "form name ↔ settings field" and holds input hints; value ranges and cross-field
//!   constraints follow the decoding rules of `runtime::settings` (on write the value is encoded and then decoded by
//!   the read rules), and the form's `min`/`max` are only hints.
//! - Only this page's fields are parsed. Numbers, single choices and text that were not submitted mean "unchanged" and
//!   keep the database's current value read under the lock (the same convention as the site page's `SiteForm`; the
//!   code has no defaults); an unsubmitted checkbox means unticked. A section with a display condition (the member API
//!   sub-block) that is not displayed is not submitted and is kept as-is on save.
use super::extract::{AdminPage, CsrfForm, csrf_forms};
use super::*;
use crate::i18n::Lang;
use crate::runtime::{self, SiteSettings};
use axum::routing::post;
use std::collections::HashMap;

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/manage/settings", get(site).post(save_site))
        .route(
            "/manage/settings/registration",
            get(registration).post(save_registration),
        )
        .route("/manage/settings/search", get(search))
        .route("/manage/settings/search/seo", post(save_seo))
        .route("/manage/settings/search/feed", post(save_feed))
        .route("/manage/settings/search/cache", post(save_cache))
        .route("/manage/settings/search/cache/refresh", post(refresh_cache))
        .route("/manage/security/posting", get(posting).post(save_posting))
        .route("/manage/security/account", get(account).post(save_account))
        .route(
            "/manage/appearance/parameters",
            get(parameters).post(save_parameters),
        )
        .route(
            "/manage/maintenance",
            get(maintenance).post(save_maintenance),
        )
        .route("/manage/links/rules", post(save_link_rules))
        .route("/manage/credits/growth-settings", post(save_growth))
}

fn actor(state: &AppState) -> crate::identity::Actor {
    state
        .viewer
        .session
        .as_ref()
        .expect("admin checked")
        .member
        .actor()
}

/// One feedback line at the top of the page: `status` on successful save, `alert` on failure, never both.
#[derive(Default)]
struct Outcome {
    saved: bool,
    message: String,
    /// A sentence worth pointing out after a successful save (non-blocking), e.g. robots blocking the whole site.
    notice: String,
}

impl Outcome {
    fn from_query(query: &SavedQuery, section: &str) -> Self {
        Self {
            saved: query.saved == section,
            message: String::new(),
            notice: String::new(),
        }
    }

    fn failed(message: String) -> Self {
        Self {
            saved: false,
            message,
            notice: String::new(),
        }
    }
}

#[derive(Default, Deserialize)]
struct SavedQuery {
    #[serde(default)]
    saved: String,
    #[serde(default)]
    notice: String,
}

/// Form refill: on a failed save, applies the form to the database's current values and shows them as-is for the
/// administrator to fix.
async fn redisplay(
    state: &AppState,
    apply: impl FnOnce(&mut SiteSettings),
) -> Result<SiteSettings, Response> {
    match runtime::read(&state.db).await {
        Ok(mut value) => {
            apply(&mut value);
            Ok(value)
        }
        Err(error) => Err(auth::identity_error(state, error)),
    }
}

/// Whether a failed save should refill and re-render, or go to the shared error page (same as
/// `administration::form_failure`).
fn form_failure(
    error: crate::identity::IdentityError,
) -> Result<(StatusCode, String), crate::identity::IdentityError> {
    match error {
        crate::identity::IdentityError::Validation(message) => {
            Ok((StatusCode::UNPROCESSABLE_ENTITY, message.to_owned()))
        }
        other => Err(other),
    }
}

// ---- Field tables ---------------------------------------------------------------------------

/// How one setting looks on the form, and the settings field it maps to.
pub(super) struct Field {
    /// Form field name, matching the last two segments of the settings key (`invite_daily_limit` ↔
    /// `settings.invite.daily_limit`).
    name: &'static str,
    label: Label,
    /// [`no_help`] when the field has no help text.
    help: Label,
    kind: Kind,
}

/// A text of a field table in the language passed in (a generated translation method such as
/// `Lang::config_field_registration`).
type Label = fn(Lang) -> &'static str;

/// The help of a field without one, and the intro of a section without one.
fn no_help(_: Lang) -> &'static str {
    ""
}

enum Kind {
    Number {
        get: fn(&SiteSettings) -> i64,
        set: fn(&mut SiteSettings, i64),
        /// Input hint, consistent with the hard bounds in `runtime::settings`.
        min: i64,
        max: i64,
    },
    Flag {
        get: fn(&SiteSettings) -> bool,
        set: fn(&mut SiteSettings, bool),
    },
    Choice {
        get: fn(&SiteSettings) -> &str,
        set: fn(&mut SiteSettings, String),
        options: &'static [(&'static str, Label)],
    },
    Text {
        get: fn(&SiteSettings) -> String,
        /// Returns a sentence for the administrator on parse failure; when it parses it is written into the candidate
        /// values, and the overall validation has the final say.
        set: fn(&mut SiteSettings, &str) -> Result<(), &'static str>,
        rows: u8,
        maxlength: usize,
    },
}

/// A group of fields. When `visible` is `Some`, it is only shown when the condition holds and only saved when
/// submitted (the member API sub-block).
pub(super) struct Section {
    id: &'static str,
    title: Label,
    intro: Label,
    fields: &'static [Field],
    visible: Option<fn(&SiteSettings) -> bool>,
}

/// Field view for templates.
pub(super) struct FieldView {
    pub(super) name: &'static str,
    pub(super) label: &'static str,
    pub(super) help: &'static str,
    /// `number` / `flag` / `choice` / `text`.
    pub(super) kind: &'static str,
    pub(super) value: String,
    pub(super) checked: bool,
    pub(super) min: i64,
    pub(super) max: i64,
    pub(super) rows: u8,
    pub(super) maxlength: usize,
    pub(super) options: Vec<OptionView>,
}

pub(super) struct OptionView {
    pub(super) value: &'static str,
    pub(super) label: &'static str,
    pub(super) selected: bool,
}

pub(super) struct SectionView {
    pub(super) id: &'static str,
    pub(super) title: &'static str,
    pub(super) intro: &'static str,
    pub(super) fields: Vec<FieldView>,
}

fn field_view(field: &Field, settings: &SiteSettings) -> FieldView {
    let t = crate::i18n::current();
    let mut view = FieldView {
        name: field.name,
        label: (field.label)(t),
        help: (field.help)(t),
        kind: "number",
        value: String::new(),
        checked: false,
        min: 0,
        max: 0,
        rows: 0,
        maxlength: 0,
        options: Vec::new(),
    };
    match &field.kind {
        Kind::Number { get, min, max, .. } => {
            view.value = get(settings).to_string();
            view.min = *min;
            view.max = *max;
        }
        Kind::Flag { get, .. } => {
            view.kind = "flag";
            view.checked = get(settings);
        }
        Kind::Choice { get, options, .. } => {
            view.kind = "choice";
            let current = get(settings);
            view.options = options
                .iter()
                .map(|(value, label)| OptionView {
                    value,
                    label: label(t),
                    selected: *value == current,
                })
                .collect();
        }
        Kind::Text {
            get,
            rows,
            maxlength,
            ..
        } => {
            view.kind = "text";
            view.value = get(settings);
            view.rows = *rows;
            view.maxlength = *maxlength;
        }
    }
    view
}

/// The sections to show on the page (display conditions judged by the database's current values).
fn section_views(sections: &[&'static Section], settings: &SiteSettings) -> Vec<SectionView> {
    sections
        .iter()
        .filter(|section| section.visible.is_none_or(|visible| visible(settings)))
        .map(|section| SectionView {
            id: section.id,
            title: (section.title)(crate::i18n::current()),
            intro: (section.intro)(crate::i18n::current()),
            fields: section
                .fields
                .iter()
                .map(|field| field_view(field, settings))
                .collect(),
        })
        .collect()
}

/// Applies the form to the candidate values. Numbers, single choices and text that were not submitted keep the current
/// values in the candidate; with `strict` a parse failure is an error (saving), otherwise whatever applies is applied
/// and the rest skipped (refill after failure). A section with a display condition is only applied when all its fields
/// are in the form (an unsubmitted checkbox means unticked, so when the whole section is hidden they must not be taken
/// as "unticked").
fn apply_form(
    sections: &[&'static Section],
    form: &HashMap<String, String>,
    settings: &mut SiteSettings,
    strict: bool,
) -> Result<(), String> {
    for section in sections {
        if section.visible.is_some()
            && !section.fields.iter().all(|field| {
                matches!(field.kind, Kind::Flag { .. }) || form.contains_key(field.name)
            })
        {
            continue;
        }
        for field in section.fields {
            let value = form.get(field.name).map(|value| value.trim());
            let result = match (&field.kind, value) {
                (Kind::Flag { set, .. }, value) => {
                    set(settings, value == Some("true"));
                    Ok(())
                }
                (_, None) => Ok(()),
                (Kind::Number { set, .. }, Some(value)) => match value.parse::<i64>() {
                    Ok(number) => {
                        set(settings, number);
                        Ok(())
                    }
                    Err(_) => Err(crate::i18n::current()
                        .config_error_integer((field.label)(crate::i18n::current()))),
                },
                (Kind::Choice { set, .. }, Some(value)) => {
                    set(settings, value.to_owned());
                    Ok(())
                }
                (Kind::Text { set, .. }, Some(_)) => {
                    // Multi-line text is not trimmed: line breaks are handled uniformly by normalization on save.
                    let raw = form.get(field.name).map_or("", String::as_str);
                    set(settings, raw).map_err(str::to_owned)
                }
            };
            if let Err(message) = result
                && strict
            {
                return Err(message);
            }
        }
    }
    Ok(())
}

/// A tab made of field tables: title, description, sections, and where to return after saving.
struct FieldPage {
    path: &'static str,
    title: Label,
    summary: Label,
    sections: &'static [&'static Section],
    /// Items listed on the page that do not have form fields yet; not shown when empty.
    upcoming: &'static [&'static str],
}

#[derive(Template)]
#[template(path = "manage/settings_fields.html")]
struct FieldPageTemplate<'a> {
    p: PageMeta<'a>,
    heading: &'static str,
    summary: &'static str,
    action: &'static str,
    sections: Vec<SectionView>,
    outcome: Outcome,
    upcoming: &'static [&'static str],
}

fn field_page_response(
    state: &AppState,
    page: &FieldPage,
    settings: &SiteSettings,
    outcome: Outcome,
    status: StatusCode,
) -> Response {
    let t = crate::i18n::current();
    let mut response = render(&FieldPageTemplate {
        p: meta(
            state,
            (page.title)(t).into(),
            (page.summary)(t).into(),
            page.path,
            "",
            "",
        ),
        heading: (page.title)(t),
        summary: (page.summary)(t),
        action: page.path,
        sections: section_views(page.sections, settings),
        outcome,
        upcoming: page.upcoming,
    });
    *response.status_mut() = status;
    response
}

async fn show_field_page(state: &AppState, page: &FieldPage, query: &SavedQuery) -> Response {
    match runtime::read(&state.db).await {
        Ok(value) => field_page_response(
            state,
            page,
            &value,
            Outcome::from_query(query, "1"),
            StatusCode::OK,
        ),
        Err(error) => auth::identity_error(state, error),
    }
}

/// A failed field-table save: the form itself has missing or bad values (one sentence with the field name, 422 with
/// refill), or another error during saving.
enum SaveError {
    Form(String),
    Identity(crate::identity::IdentityError),
}

impl SaveError {
    /// Those to refill give a status code and message; the rest go to the shared error page.
    fn into_form(self) -> Result<(StatusCode, String), crate::identity::IdentityError> {
        match self {
            Self::Form(message) => Ok((StatusCode::UNPROCESSABLE_ENTITY, message)),
            Self::Identity(error) => form_failure(error),
        }
    }
}

/// Saves a group of sections: the form is first parsed by this page's fields, then applied field by field to the
/// current values under the lock; normalization and overall validation are done by [`runtime::update`].
async fn update_sections(
    state: &AppState,
    sections: &'static [&'static Section],
    form: &HashMap<String, String>,
) -> Result<SiteSettings, SaveError> {
    // Report parse failures first, without entering the save transaction (parsing only looks at the form, not at the
    // database values).
    let mut probe = runtime::read(&state.db)
        .await
        .map_err(SaveError::Identity)?;
    apply_form(sections, form, &mut probe, true).map_err(SaveError::Form)?;
    runtime::update(
        &state.db,
        &state.global,
        actor(state),
        "site.settings.save",
        |settings| {
            let _ = apply_form(sections, form, settings, true);
        },
    )
    .await
    .map_err(SaveError::Identity)
}

async fn save_field_page(
    state: &AppState,
    page: &'static FieldPage,
    form: HashMap<String, String>,
) -> Response {
    match update_sections(state, page.sections, &form).await {
        Ok(_) => auth::redirect(&format!("{}?saved=1", page.path)),
        Err(error) => match error.into_form() {
            Ok((status, message)) => {
                match redisplay(state, |s| {
                    let _ = apply_form(page.sections, &form, s, false);
                })
                .await
                {
                    Ok(shown) => {
                        field_page_response(state, page, &shown, Outcome::failed(message), status)
                    }
                    Err(response) => response,
                }
            }
            Err(other) => auth::identity_error(state, other),
        },
    }
}

/// A failed block save (friend-link application rules, growth points): form problems get an error page stating the
/// reason; the rest are handled like the shared error page.
fn block_failure(state: &AppState, error: SaveError) -> Response {
    match error.into_form() {
        Ok((status, message)) => error_page(state, status, &message),
        Err(other) => auth::identity_error(state, other),
    }
}

macro_rules! number {
    ($name:literal, $label:expr, $help:expr, $group:ident . $field:ident, $min:expr, $max:expr) => {
        Field {
            name: $name,
            label: $label,
            help: $help,
            kind: Kind::Number {
                get: |s| s.$group.$field,
                set: |s, v| s.$group.$field = v,
                min: $min,
                max: $max,
            },
        }
    };
}
macro_rules! flag {
    ($name:literal, $label:expr, $help:expr, $group:ident . $field:ident) => {
        Field {
            name: $name,
            label: $label,
            help: $help,
            kind: Kind::Flag {
                get: |s| s.$group.$field,
                set: |s, v| s.$group.$field = v,
            },
        }
    };
}
macro_rules! choice {
    ($name:literal, $label:expr, $help:expr, $group:ident . $field:ident, $options:expr) => {
        Field {
            name: $name,
            label: $label,
            help: $help,
            kind: Kind::Choice {
                get: |s| s.$group.$field.as_str(),
                set: |s, v| s.$group.$field = v,
                options: $options,
            },
        }
    };
}

// ---- Global settings › Site -----------------------------------------------------------------

#[derive(Template)]
#[template(path = "manage/settings.html")]
struct SitePage<'a> {
    p: PageMeta<'a>,
    settings: SiteSettings,
    outcome: Outcome,
    /// Placeholders of the brand words: what an empty field shows in the site language (docs/architecture.md,
    /// "Languages").
    product_name: &'static str,
    default_credit: String,
    default_index: String,
    /// The address derived for this request (from `Host`) when the forum URL is left empty, used as the input's
    /// placeholder.
    request_site_url: String,
    /// Site time zone options (the 11 time zones of `clock::SITE_ZONES` merged by offset); when the database value is
    /// not among them there is an extra "current value" option, so saving never silently changes the time zone.
    zones: Vec<crate::clock::ZoneChoice>,
    /// The second confirmation when the forum URL's path is about to change; normally empty.
    confirm: Option<PrefixConfirm>,
}

/// The second confirmation before changing the forum URL's path prefix: old and new common addresses, the nginx
/// snippet, the order of switching prefixes, and the rescue command.
struct PrefixConfirm {
    message: String,
    /// The current prefix seen at confirmation, carried back as-is and checked under the lock on save.
    from: String,
    from_label: String,
    to_label: String,
    /// Page, current address, address after saving.
    rows: Vec<(&'static str, String, String)>,
    nginx: String,
    /// nginx snippet that 301-redirects the old prefix to the new one; empty when it was a root deployment (the root
    /// path may belong to another program, so it is not redirected on that program's behalf).
    redirect: String,
    rescue: String,
    to_root: bool,
}

impl PrefixConfirm {
    /// `from_url` and `to_url` are the forum URLs before and after the change; an empty one is filled with the origin
    /// derived from this request.
    fn new(state: &AppState, from_url: &str, to_url: &str, message: String) -> Self {
        let origin = state.viewer.origin.clone().unwrap_or_default();
        let shown = |url: &str| {
            if url.is_empty() {
                origin.clone()
            } else {
                url.to_owned()
            }
        };
        let (from_shown, to_shown) = (shown(from_url), shown(to_url));
        let from = crate::site::prefix_of(from_url).to_owned();
        let to = crate::site::prefix_of(to_url).to_owned();
        let t = crate::i18n::current();
        let rows = [
            (t.config_prefix_home(), "/"),
            (t.config_prefix_login(), "/login"),
            (t.config_prefix_admin(), "/manage"),
            (t.config_prefix_settings(), "/manage/settings"),
            (t.config_prefix_feed(), "/feed.xml"),
            (t.config_prefix_sitemap(), "/sitemap.xml"),
            (t.config_prefix_health(), "/healthz"),
        ]
        .into_iter()
        .map(|(label, path)| {
            (
                label,
                format!("{from_shown}{path}"),
                format!("{to_shown}{path}"),
            )
        })
        .collect();
        let listen = state.config.listen;
        let nginx = if to.is_empty() {
            format!(
                "location / {{\n    proxy_pass http://{listen};\n    proxy_set_header Host $host;\n    proxy_set_header X-Forwarded-Proto $scheme;\n    proxy_set_header X-Real-IP $remote_addr;\n}}"
            )
        } else {
            format!(
                "location {to} {{\n    proxy_pass http://{listen};\n    proxy_set_header Host $host;\n    proxy_set_header X-Forwarded-Proto $scheme;\n    proxy_set_header X-Real-IP $remote_addr;\n}}"
            )
        };
        let redirect = if from.is_empty() {
            String::new()
        } else {
            format!("location ^~ {from} {{\n    rewrite ^{from}(?:/(.*))?$ {to}/$1 permanent;\n}}")
        };
        let label = |prefix: &str| {
            if prefix.is_empty() {
                t.config_prefix_root().to_owned()
            } else {
                prefix.to_owned()
            }
        };
        Self {
            message,
            from_label: label(&from),
            to_label: label(&to),
            to_root: to.is_empty(),
            from,
            rows,
            nginx,
            redirect,
            rescue: format!(
                "gono-forum-cli set-site-url {}",
                if from_url.is_empty() {
                    "\"\""
                } else {
                    from_url
                }
            ),
        }
    }
}

fn site_page(
    state: &AppState,
    settings: SiteSettings,
    outcome: Outcome,
    status: StatusCode,
    confirm: Option<PrefixConfirm>,
) -> Response {
    let mut response = render(&SitePage {
        p: meta(
            state,
            crate::i18n::current().manage_tab_settings().into(),
            crate::i18n::current().config_site_description().into(),
            "/manage/settings",
            "",
            "",
        ),
        outcome,
        product_name: crate::i18n::PRODUCT_NAME,
        default_credit: settings.site.lang().brand_credit_name().to_owned(),
        default_index: settings.site.lang().brand_index_name().to_owned(),
        request_site_url: state.viewer.origin.clone().unwrap_or_default(),
        zones: crate::clock::zone_choices(settings.time.offset_minutes),
        settings,
        confirm,
    });
    *response.status_mut() = status;
    response
}

async fn site(AdminPage(state): AdminPage, Query(query): Query<SavedQuery>) -> Response {
    match runtime::read(&state.db).await {
        Ok(value) => site_page(
            &state,
            value,
            Outcome::from_query(&query, "1"),
            StatusCode::OK,
            None,
        ),
        Err(error) => auth::identity_error(&state, error),
    }
}

#[derive(Deserialize)]
struct SiteForm {
    _csrf: String,
    site_name: String,
    #[serde(default)]
    contact_email: String,
    #[serde(default)]
    read_only: bool,
    #[serde(default)]
    read_only_message: String,
    #[serde(default)]
    site_url: String,
    // The brand words (docs/architecture.md, "Languages"): not submitted means unchanged, so older forms and scripts
    // that post only the other fields keep them.
    brand_text: Option<String>,
    credit_name: Option<String>,
    index_name: Option<String>,
    // The following are not checkboxes: not submitted means "unchanged", keeping the database's current value (the
    // code has no defaults).
    timezone_offset: Option<i64>,
    date_format: Option<String>,
    #[serde(default)]
    clock24: bool,
    #[serde(default)]
    relative_time: bool,
    relative_hours: Option<i64>,
    /// The current prefix carried back in the second-confirmation round; absent means an ordinary save.
    prefix_from: Option<String>,
    /// The `confirm` typed in by hand at the second confirmation.
    #[serde(default)]
    confirm_prefix: String,
}
csrf_forms!(SiteForm);

impl SiteForm {
    fn apply(&self, settings: &mut SiteSettings) {
        settings.site.name.clone_from(&self.site_name);
        settings.site.contact_email.clone_from(&self.contact_email);
        settings.site.read_only = self.read_only;
        settings
            .site
            .read_only_message
            .clone_from(&self.read_only_message);
        settings.site.url.clone_from(&self.site_url);
        for (value, field) in [
            (&self.brand_text, &mut settings.site.brand_text),
            (&self.credit_name, &mut settings.site.credit_name),
            (&self.index_name, &mut settings.site.index_name),
        ] {
            if let Some(value) = value {
                field.clone_from(value);
            }
        }
        if let Some(offset) = self.timezone_offset {
            settings.time.offset_minutes = offset;
        }
        if let Some(format) = &self.date_format {
            settings.time.date_format.clone_from(format);
        }
        settings.time.clock24 = self.clock24;
        settings.time.relative = self.relative_time;
        if let Some(hours) = self.relative_hours {
            settings.time.relative_hours = hours;
        }
    }
}

async fn save_site(AdminPage(state): AdminPage, CsrfForm(form): CsrfForm<SiteForm>) -> Response {
    // When the forum URL's path is about to change, go through the second confirmation first: it only saves once
    // `confirm` is typed by hand, and checks under the lock the current prefix seen at confirmation. An ordinary save
    // that meets a path change returns `runtime::prefix_needs_confirm()` and moves to the confirmation page.
    let confirmed = form
        .prefix_from
        .as_deref()
        .filter(|_| form.confirm_prefix.trim() == "confirm");
    if form.prefix_from.is_some() && confirmed.is_none() {
        return prefix_confirm(
            &state,
            &form,
            crate::i18n::current().config_prefix_type_confirm().into(),
        )
        .await;
    }
    let saved = match confirmed {
        Some(from) => {
            runtime::update_confirmed(
                &state.db,
                &state.global,
                actor(&state),
                "site.settings.save",
                from,
                |s| form.apply(s),
            )
            .await
        }
        None => {
            runtime::update(
                &state.db,
                &state.global,
                actor(&state),
                "site.settings.save",
                |s| form.apply(s),
            )
            .await
        }
    };
    match saved {
        // The path prefix was just changed: this request's prefix is now the old one, so redirect to the settings page
        // under the new prefix.
        Ok(saved) => (
            StatusCode::SEE_OTHER,
            [(
                header::LOCATION,
                crate::site::href_under(
                    crate::site::prefix_of(&saved.site.url),
                    "/manage/settings?saved=1",
                ),
            )],
        )
            .into_response(),
        Err(crate::identity::IdentityError::Validation(message))
            if message == runtime::prefix_needs_confirm() =>
        {
            prefix_confirm(&state, &form, String::new()).await
        }
        Err(crate::identity::IdentityError::Conflict(message))
            if message == runtime::prefix_moved() =>
        {
            prefix_confirm(&state, &form, message.into()).await
        }
        Err(error) => match form_failure(error) {
            Ok((status, message)) => match redisplay(&state, |s| form.apply(s)).await {
                Ok(shown) => site_page(&state, shown, Outcome::failed(message), status, None),
                Err(response) => response,
            },
            Err(other) => auth::identity_error(&state, other),
        },
    }
}

/// Refills the form with the second confirmation shown above it. The current value is the forum URL now in the
/// database, and the new value is the one in the form (already validated).
async fn prefix_confirm(state: &AppState, form: &SiteForm, message: String) -> Response {
    let current = match runtime::read(&state.db).await {
        Ok(current) => current,
        Err(error) => return auth::identity_error(state, error),
    };
    let to_url = crate::config::normalize_forum_url(form.site_url.trim()).unwrap_or_default();
    let to_url = if form.site_url.trim().is_empty() {
        String::new()
    } else {
        to_url
    };
    let message = if message.is_empty() {
        let t = crate::i18n::current();
        let from = crate::site::prefix_of(&current.site.url);
        let to = crate::site::prefix_of(&to_url);
        t.config_prefix_change(
            if from.is_empty() {
                t.config_prefix_root()
            } else {
                from
            },
            if to.is_empty() {
                t.config_prefix_root()
            } else {
                to
            },
        )
    } else {
        message
    };
    let confirm = PrefixConfirm::new(state, &current.site.url, &to_url, message);
    match redisplay(state, |s| form.apply(s)).await {
        Ok(shown) => site_page(
            state,
            shown,
            Outcome::default(),
            StatusCode::OK,
            Some(confirm),
        ),
        Err(response) => response,
    }
}

// ---- Global settings › Registration and permissions -------------------------------------------

const REGISTRATION_MODES: &[(&str, Label)] = &[
    ("closed", Lang::config_option_registration_closed),
    ("invite", Lang::config_option_registration_invite),
    ("open", Lang::config_option_registration_open),
];

static REGISTRATION: Section = Section {
    id: "registration",
    title: Lang::config_section_registration,
    intro: Lang::config_section_registration_intro,
    fields: &[
        choice!(
            "registration",
            Lang::config_field_registration,
            Lang::config_field_registration_help,
            signup.mode,
            REGISTRATION_MODES
        ),
        number!(
            "signup_ip_hourly_limit",
            Lang::config_field_signup_ip_hourly_limit,
            Lang::config_field_signup_ip_hourly_limit_help,
            signup.ip_hourly_limit,
            1,
            1000
        ),
        flag!(
            "invitations_enabled",
            Lang::config_field_invitations_enabled,
            no_help,
            invite.enabled
        ),
        number!(
            "invite_daily_limit",
            Lang::config_field_invite_daily_limit,
            Lang::config_field_invite_daily_limit_help,
            invite.daily_limit,
            1,
            100
        ),
        number!(
            "invite_active_limit",
            Lang::config_field_invite_active_limit,
            no_help,
            invite.active_limit,
            1,
            100
        ),
        number!(
            "invite_ttl_days",
            Lang::config_field_invite_ttl_days,
            Lang::config_field_invite_ttl_days_help,
            invite.ttl_days,
            1,
            365
        ),
        Field {
            name: "invite_prefixes",
            label: Lang::config_field_invite_prefixes,
            help: Lang::config_field_invite_prefixes_help,
            kind: Kind::Text {
                get: |s| s.invite.prefixes.join("\n"),
                set: |s, value| {
                    // Surrounding whitespace, blank lines and duplicates are handled by normalization on save, and
                    // the format is checked by the overall validation.
                    s.invite.prefixes = value.lines().map(str::to_owned).collect();
                    Ok(())
                },
                rows: 4,
                maxlength: 2000,
            },
        },
        number!(
            "username_cooldown_days",
            Lang::config_field_username_cooldown_days,
            Lang::config_field_username_cooldown_days_help,
            username.cooldown_days,
            1,
            365
        ),
        number!(
            "username_history_days",
            Lang::config_field_username_history_days,
            Lang::config_field_username_history_days_help,
            username.history_days,
            1,
            365
        ),
        flag!(
            "api_enabled",
            Lang::config_field_api_enabled,
            Lang::config_field_api_enabled_help,
            api.enabled
        ),
    ],
    visible: None,
};

static API: Section = Section {
    id: "api",
    title: Lang::config_section_api,
    intro: Lang::config_section_api_intro,
    fields: &[
        number!(
            "api_key_ttl_days",
            Lang::config_field_api_key_ttl_days,
            Lang::config_field_api_key_ttl_days_help,
            api.key_ttl_days,
            1,
            365
        ),
        number!(
            "api_requests_per_minute",
            Lang::config_field_api_requests_per_minute,
            no_help,
            api.requests_per_minute,
            1,
            10000
        ),
        number!(
            "api_writes_per_minute",
            Lang::config_field_api_writes_per_minute,
            Lang::config_field_api_writes_per_minute_help,
            api.writes_per_minute,
            1,
            10000
        ),
        number!(
            "api_auth_failures_per_5min",
            Lang::config_field_api_auth_failures_per_5min,
            Lang::config_field_api_auth_failures_per_5min_help,
            api.auth_failures_per_5min,
            5,
            100
        ),
        number!(
            "api_max_body_kib",
            Lang::config_field_api_max_body_kib,
            Lang::config_field_api_max_body_kib_help,
            api.max_body_kib,
            1,
            1024
        ),
    ],
    visible: Some(|s| s.api.enabled),
};

static REGISTRATION_PAGE: FieldPage = FieldPage {
    path: "/manage/settings/registration",
    title: Lang::manage_tab_settings_registration,
    summary: Lang::config_page_registration_summary,
    sections: &[&REGISTRATION, &API],
    upcoming: &[],
};

async fn registration(AdminPage(state): AdminPage, Query(query): Query<SavedQuery>) -> Response {
    show_field_page(&state, &REGISTRATION_PAGE, &query).await
}

async fn save_registration(
    AdminPage(state): AdminPage,
    CsrfForm(form): CsrfForm<HashMap<String, String>>,
) -> Response {
    save_field_page(&state, &REGISTRATION_PAGE, form).await
}

// ---- Global settings › Search and performance -------------------------------------------------

#[derive(Template)]
#[template(path = "manage/settings_search.html")]
struct SearchPage<'a> {
    p: PageMeta<'a>,
    settings: SiteSettings,
    seo: Outcome,
    feed: Outcome,
    cache: Outcome,
    /// With Redis off the floor and list items have no effect, which the page notes.
    redis_enabled: bool,
    /// Names site verification may not use (this site's head already outputs them).
    reserved: &'static [&'static str],
    /// For sub-path deployment, the full robots text to merge into the domain root (rules already prefixed); empty
    /// and hidden for a root deployment.
    robots_merge: String,
}

fn search_page(
    state: &AppState,
    settings: SiteSettings,
    (seo, feed, cache): (Outcome, Outcome, Outcome),
    status: StatusCode,
) -> Response {
    let prefix = crate::site::prefix();
    // When multi-domain sites are on and this host also registers the root path, the root path row's robots already
    // merges this prefix's rules, so no manual merge is needed.
    let merged_by_root = crate::site::domains().is_some_and(|domains| {
        domains.row().is_some_and(|current| {
            domains.map.rows().iter().any(|row| {
                row.enabled && row.authority() == current.authority() && row.prefix().is_empty()
            })
        })
    });
    let robots_merge = if prefix.is_empty() || merged_by_root {
        String::new()
    } else {
        super::site::robots_body(&settings, state.site_url(), &prefix)
    };
    let mut response = render(&SearchPage {
        p: meta(
            state,
            crate::i18n::current().manage_tab_settings_search().into(),
            crate::i18n::current().config_search_description().into(),
            "/manage/settings/search",
            "",
            "",
        ),
        settings,
        seo,
        feed,
        cache,
        redis_enabled: state.cache.enabled(),
        reserved: runtime::VERIFICATION_RESERVED,
        robots_merge,
    });
    *response.status_mut() = status;
    response
}

async fn search(AdminPage(state): AdminPage, Query(query): Query<SavedQuery>) -> Response {
    match runtime::read(&state.db).await {
        Ok(value) => {
            let mut seo = Outcome::from_query(&query, "seo");
            if seo.saved && query.notice == "robots-blocks-all" {
                seo.notice = crate::i18n::current().config_robots_blocks_all().into();
            }
            let mut cache = Outcome::from_query(&query, "cache");
            if query.saved == "refresh" {
                cache.notice = if state.cache.enabled() {
                    crate::i18n::current().config_cache_refreshed_shared()
                } else {
                    crate::i18n::current().config_cache_refreshed()
                }
                .into();
            }
            search_page(
                &state,
                value,
                (seo, Outcome::from_query(&query, "feed"), cache),
                StatusCode::OK,
            )
        }
        Err(error) => auth::identity_error(&state, error),
    }
}

/// The sample meta element in the help of site verification, written out as text (`<meta name="…" content="…">`).
/// Templates pass the language by reference.
pub(super) fn verification_sample(t: &Lang) -> crate::i18n::Markup {
    use crate::i18n::Markup;
    Markup::trusted(format!(
        "&lt;meta name=\"{}\" content=\"{}\"&gt;",
        Markup::text(t.config_seo_meta_name()),
        Markup::text(t.config_seo_meta_value())
    ))
}

/// Whether the `User-agent: *` group contains `Disallow: /`. Read by the robots grouping rules: consecutive
/// `User-agent` lines form a group, and the rule lines after them belong to that group. Only for the notice; it does
/// not block saving.
fn robots_blocks_all(text: &str) -> bool {
    let mut agents: Vec<String> = Vec::new();
    let mut in_rules = false;
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let (name, value) = (name.trim().to_ascii_lowercase(), value.trim());
        if name == "user-agent" {
            if in_rules {
                agents.clear();
                in_rules = false;
            }
            agents.push(value.to_owned());
        } else {
            in_rules = true;
            if name == "disallow" && value == "/" && agents.iter().any(|agent| agent == "*") {
                return true;
            }
        }
    }
    false
}

#[derive(Deserialize)]
struct SeoForm {
    _csrf: String,
    #[serde(default)]
    indexing_enabled: bool,
    #[serde(default)]
    description: String,
    #[serde(default)]
    robots: String,
    #[serde(default)]
    verification: String,
}
csrf_forms!(SeoForm);

impl SeoForm {
    /// The SEO block writes back 4 rows at once: indexing, robots, site verification and site description.
    fn apply(&self, settings: &mut SiteSettings) {
        settings.seo.indexing = self.indexing_enabled;
        settings.site.description.clone_from(&self.description);
        settings.seo.robots.clone_from(&self.robots);
        settings.seo.verification.clone_from(&self.verification);
    }
}

async fn save_seo(AdminPage(state): AdminPage, CsrfForm(form): CsrfForm<SeoForm>) -> Response {
    match runtime::update(
        &state.db,
        &state.global,
        actor(&state),
        "site.settings.save",
        |s| form.apply(s),
    )
    .await
    {
        Ok(saved) if robots_blocks_all(&saved.seo.robots) => {
            auth::redirect("/manage/settings/search?saved=seo&notice=robots-blocks-all")
        }
        Ok(_) => auth::redirect("/manage/settings/search?saved=seo"),
        Err(error) => match form_failure(error) {
            Ok((status, message)) => match redisplay(&state, |s| form.apply(s)).await {
                Ok(shown) => search_page(
                    &state,
                    shown,
                    (
                        Outcome::failed(message),
                        Outcome::default(),
                        Outcome::default(),
                    ),
                    status,
                ),
                Err(response) => response,
            },
            Err(other) => auth::identity_error(&state, other),
        },
    }
}

#[derive(Deserialize)]
struct FeedForm {
    _csrf: String,
    feed_items: i64,
}
csrf_forms!(FeedForm);

async fn save_feed(AdminPage(state): AdminPage, CsrfForm(form): CsrfForm<FeedForm>) -> Response {
    let apply = |s: &mut SiteSettings| s.feed.items = form.feed_items;
    match runtime::update(
        &state.db,
        &state.global,
        actor(&state),
        "site.settings.save",
        apply,
    )
    .await
    {
        Ok(_) => auth::redirect("/manage/settings/search?saved=feed"),
        Err(error) => match form_failure(error) {
            Ok((status, message)) => match redisplay(&state, apply).await {
                Ok(shown) => search_page(
                    &state,
                    shown,
                    (
                        Outcome::default(),
                        Outcome::failed(message),
                        Outcome::default(),
                    ),
                    status,
                ),
                Err(response) => response,
            },
            Err(other) => auth::identity_error(&state, other),
        },
    }
}

#[derive(Deserialize)]
struct CacheForm {
    _csrf: String,
    ttl_seconds: i64,
    page_ttl_seconds: i64,
    list_ttl_seconds: i64,
    statistics_seconds: i64,
    forum_statistics_seconds: i64,
}
csrf_forms!(CacheForm);

impl CacheForm {
    fn apply(&self, settings: &mut SiteSettings) {
        settings.cache.ttl_seconds = self.ttl_seconds;
        settings.cache.page_ttl_seconds = self.page_ttl_seconds;
        settings.cache.list_ttl_seconds = self.list_ttl_seconds;
        settings.cache.statistics_seconds = self.statistics_seconds;
        settings.cache.forum_statistics_seconds = self.forum_statistics_seconds;
    }
}

async fn save_cache(AdminPage(state): AdminPage, CsrfForm(form): CsrfForm<CacheForm>) -> Response {
    match runtime::update(
        &state.db,
        &state.global,
        actor(&state),
        "site.settings.save",
        |s| form.apply(s),
    )
    .await
    {
        Ok(_) => auth::redirect("/manage/settings/search?saved=cache"),
        Err(error) => match form_failure(error) {
            Ok((status, message)) => match redisplay(&state, |s| form.apply(s)).await {
                Ok(shown) => search_page(
                    &state,
                    shown,
                    (
                        Outcome::default(),
                        Outcome::default(),
                        Outcome::failed(message),
                    ),
                    status,
                ),
                Err(response) => response,
            },
            Err(other) => auth::identity_error(&state, other),
        },
    }
}

/// "Refresh caches": clears the process cache and, with Redis enabled,
/// tells other processes to clear theirs too. Writes nothing to the database and bypasses the write gate; admin saves
/// already clear on the spot, so this button is for use after changing the database by hand or settings from the
/// command line.
async fn refresh_cache(
    AdminPage(state): AdminPage,
    CsrfForm(_): CsrfForm<HashMap<String, String>>,
) -> Response {
    match state.global.refresh_all().await {
        Ok(()) => auth::redirect("/manage/settings/search?saved=refresh"),
        Err(unpublished) => {
            tracing::error!(error = %unpublished.detail, "refresh cache: this process is cleared, but other processes could not be notified");
            error_page(
                &state,
                StatusCode::SERVICE_UNAVAILABLE,
                &crate::i18n::current().config_cache_refresh_unpublished(&unpublished.detail),
            )
        }
    }
}

// ---- Security › Anti-spam and governance ------------------------------------------------------

static POSTING: Section = Section {
    id: "posting",
    title: Lang::config_section_posting,
    intro: Lang::config_section_posting_intro,
    fields: &[
        number!(
            "posting_first_day_threads",
            Lang::config_field_posting_first_day_threads,
            Lang::config_field_posting_first_day_threads_help,
            posting.first_day_threads,
            1,
            1000
        ),
        number!(
            "posting_first_day_replies",
            Lang::config_field_posting_first_day_replies,
            no_help,
            posting.first_day_replies,
            1,
            1000
        ),
        number!(
            "posting_daily_threads",
            Lang::config_field_posting_daily_threads,
            Lang::config_field_posting_daily_threads_help,
            posting.daily_threads,
            1,
            1000
        ),
        number!(
            "posting_daily_replies",
            Lang::config_field_posting_daily_replies,
            Lang::config_field_posting_daily_replies_help,
            posting.daily_replies,
            1,
            10000
        ),
        number!(
            "posting_new_member_hours",
            Lang::config_field_posting_new_member_hours,
            Lang::config_field_posting_new_member_hours_help,
            posting.new_member_hours,
            1,
            720
        ),
        number!(
            "posting_body_max_bytes",
            Lang::config_field_posting_body_max_bytes,
            Lang::config_field_posting_body_max_bytes_help,
            posting.body_max_bytes,
            1000,
            60000
        ),
        number!(
            "posting_edit_window_hours",
            Lang::config_field_posting_edit_window_hours,
            Lang::config_field_posting_edit_window_hours_help,
            posting.edit_window_hours,
            1,
            8760
        ),
        flag!(
            "posting_duplicate_check",
            Lang::config_field_posting_duplicate_check,
            Lang::config_field_posting_duplicate_check_help,
            posting.duplicate_check
        ),
        number!(
            "posting_duplicate_window_minutes",
            Lang::config_field_posting_duplicate_window_minutes,
            Lang::config_field_posting_duplicate_window_minutes_help,
            posting.duplicate_window_minutes,
            1,
            1440
        ),
        flag!(
            "posting_new_member_link_block",
            Lang::config_field_posting_new_member_link_block,
            Lang::config_field_posting_new_member_link_block_help,
            posting.new_member_link_block
        ),
        number!(
            "posting_new_member_link_days",
            Lang::config_field_posting_new_member_link_days,
            Lang::config_field_posting_new_member_link_days_help,
            posting.new_member_link_days,
            1,
            90
        ),
    ],
    visible: None,
};

static RATES: Section = Section {
    id: "rates",
    title: Lang::config_section_rates,
    intro: Lang::config_section_rates_intro,
    fields: &[
        number!(
            "posting_thread_interval_seconds",
            Lang::config_field_posting_thread_interval_seconds,
            Lang::config_field_posting_thread_interval_seconds_help,
            posting.thread_interval_seconds,
            0,
            3600
        ),
        number!(
            "posting_reply_interval_seconds",
            Lang::config_field_posting_reply_interval_seconds,
            Lang::config_field_posting_reply_interval_seconds_help,
            posting.reply_interval_seconds,
            0,
            600
        ),
        number!(
            "posting_hourly_limit",
            Lang::config_field_posting_hourly_limit,
            Lang::config_field_posting_hourly_limit_help,
            posting.hourly_limit,
            0,
            1000
        ),
        number!(
            "limit_message_per_minute",
            Lang::config_field_limit_message_per_minute,
            Lang::config_field_limit_message_per_minute_help,
            limit.message_per_minute,
            1,
            1000
        ),
        number!(
            "limit_message_peer_per_minute",
            Lang::config_field_limit_message_peer_per_minute,
            Lang::config_field_limit_message_peer_per_minute_help,
            limit.message_peer_per_minute,
            1,
            1000
        ),
        number!(
            "limit_interaction_per_minute",
            Lang::config_field_limit_interaction_per_minute,
            no_help,
            limit.interaction_per_minute,
            1,
            1000
        ),
        number!(
            "limit_interaction_per_hour",
            Lang::config_field_limit_interaction_per_hour,
            Lang::config_field_limit_interaction_per_hour_help,
            limit.interaction_per_hour,
            1,
            100000
        ),
        number!(
            "limit_social_per_5min",
            Lang::config_field_limit_social_per_5min,
            no_help,
            limit.social_per_5min,
            1,
            1000
        ),
    ],
    visible: None,
};

static GOVERNANCE: Section = Section {
    id: "governance",
    title: Lang::config_section_governance,
    intro: Lang::config_section_governance_intro,
    fields: &[
        number!(
            "limit_report_daily",
            Lang::config_field_limit_report_daily,
            Lang::config_field_limit_report_daily_help,
            limit.report_daily,
            1,
            100
        ),
        number!(
            "limit_restoration_daily",
            Lang::config_field_limit_restoration_daily,
            no_help,
            limit.restoration_daily,
            1,
            100
        ),
        number!(
            "governance_restoration_cooldown_days",
            Lang::config_field_governance_restoration_cooldown_days,
            Lang::config_field_governance_restoration_cooldown_days_help,
            governance.restoration_cooldown_days,
            0,
            365
        ),
        number!(
            "governance_restoration_due_days",
            Lang::config_field_governance_restoration_due_days,
            Lang::config_field_governance_restoration_due_days_help,
            governance.restoration_due_days,
            1,
            90
        ),
        number!(
            "governance_appeal_window_days",
            Lang::config_field_governance_appeal_window_days,
            Lang::config_field_governance_appeal_window_days_help,
            governance.appeal_window_days,
            1,
            365
        ),
        number!(
            "governance_appeal_handling_days",
            Lang::config_field_governance_appeal_handling_days,
            Lang::config_field_governance_appeal_handling_days_help,
            governance.appeal_handling_days,
            1,
            90
        ),
    ],
    visible: None,
};

static POSTING_PAGE: FieldPage = FieldPage {
    path: "/manage/security/posting",
    title: Lang::manage_tab_security_posting,
    summary: Lang::config_page_posting_summary,
    sections: &[&POSTING, &RATES, &GOVERNANCE],
    upcoming: &[],
};

async fn posting(AdminPage(state): AdminPage, Query(query): Query<SavedQuery>) -> Response {
    show_field_page(&state, &POSTING_PAGE, &query).await
}

async fn save_posting(
    AdminPage(state): AdminPage,
    CsrfForm(form): CsrfForm<HashMap<String, String>>,
) -> Response {
    save_field_page(&state, &POSTING_PAGE, form).await
}

// ---- Security › Account security --------------------------------------------------------------

static SESSIONS: Section = Section {
    id: "sessions",
    title: Lang::config_section_sessions,
    intro: Lang::config_section_sessions_intro,
    fields: &[
        number!(
            "account_session_idle_days",
            Lang::config_field_account_session_idle_days,
            Lang::config_field_account_session_idle_days_help,
            account.session_idle_days,
            1,
            90
        ),
        number!(
            "account_session_absolute_days",
            Lang::config_field_account_session_absolute_days,
            Lang::config_field_account_session_absolute_days_help,
            account.session_absolute_days,
            1,
            365
        ),
        number!(
            "account_verification_ttl_hours",
            Lang::config_field_account_verification_ttl_hours,
            Lang::config_field_account_verification_ttl_hours_help,
            account.verification_ttl_hours,
            1,
            168
        ),
        number!(
            "account_reset_ttl_minutes",
            Lang::config_field_account_reset_ttl_minutes,
            no_help,
            account.reset_ttl_minutes,
            10,
            1440
        ),
        number!(
            "account_email_change_ttl_hours",
            Lang::config_field_account_email_change_ttl_hours,
            no_help,
            account.email_change_ttl_hours,
            1,
            168
        ),
        number!(
            "account_password_min_length",
            Lang::config_field_account_password_min_length,
            Lang::config_field_account_password_min_length_help,
            account.password_min_length,
            8,
            64
        ),
    ],
    visible: None,
};

static ACCOUNT_LIMITS: Section = Section {
    id: "account-limits",
    title: Lang::config_section_account_limits,
    intro: Lang::config_section_account_limits_intro,
    fields: &[
        number!(
            "account_login_ip_per_5min",
            Lang::config_field_account_login_ip_per_5min,
            no_help,
            account.login_ip_per_5min,
            5,
            100
        ),
        number!(
            "account_login_name_per_5min",
            Lang::config_field_account_login_name_per_5min,
            no_help,
            account.login_name_per_5min,
            3,
            30
        ),
        number!(
            "account_two_factor_ip_per_5min",
            Lang::config_field_account_two_factor_ip_per_5min,
            Lang::config_field_account_two_factor_ip_per_5min_help,
            account.two_factor_ip_per_5min,
            5,
            100
        ),
        number!(
            "account_password_change_account_per_5min",
            Lang::config_field_account_password_change_account_per_5min,
            Lang::config_field_account_password_change_account_per_5min_help,
            account.password_change_account_per_5min,
            3,
            20
        ),
        number!(
            "account_password_change_ip_per_5min",
            Lang::config_field_account_password_change_ip_per_5min,
            no_help,
            account.password_change_ip_per_5min,
            5,
            100
        ),
        number!(
            "account_forgot_email_per_minute",
            Lang::config_field_account_forgot_email_per_minute,
            no_help,
            account.forgot_email_per_minute,
            1,
            5
        ),
        number!(
            "account_forgot_email_per_hour",
            Lang::config_field_account_forgot_email_per_hour,
            no_help,
            account.forgot_email_per_hour,
            1,
            20
        ),
        number!(
            "account_forgot_ip_per_hour",
            Lang::config_field_account_forgot_ip_per_hour,
            no_help,
            account.forgot_ip_per_hour,
            5,
            100
        ),
        number!(
            "account_reset_token_per_5min",
            Lang::config_field_account_reset_token_per_5min,
            no_help,
            account.reset_token_per_5min,
            3,
            30
        ),
        number!(
            "account_reset_ip_per_5min",
            Lang::config_field_account_reset_ip_per_5min,
            no_help,
            account.reset_ip_per_5min,
            5,
            100
        ),
    ],
    visible: None,
};

static ACCOUNT_PAGE: FieldPage = FieldPage {
    path: "/manage/security/account",
    title: Lang::manage_tab_security_account,
    summary: Lang::config_page_account_summary,
    sections: &[&SESSIONS, &ACCOUNT_LIMITS],
    upcoming: &[],
};

async fn account(AdminPage(state): AdminPage, Query(query): Query<SavedQuery>) -> Response {
    show_field_page(&state, &ACCOUNT_PAGE, &query).await
}

async fn save_account(
    AdminPage(state): AdminPage,
    CsrfForm(form): CsrfForm<HashMap<String, String>>,
) -> Response {
    save_field_page(&state, &ACCOUNT_PAGE, form).await
}

// ---- Interface settings › Display settings ----------------------------------------------------

static DISPLAY: Section = Section {
    id: "display",
    title: Lang::config_section_display,
    intro: Lang::config_section_display_intro,
    fields: &[
        number!(
            "display_threads_per_page",
            Lang::config_field_display_threads_per_page,
            Lang::config_field_display_threads_per_page_help,
            display.threads_per_page,
            10,
            100
        ),
        number!(
            "display_replies_per_page",
            Lang::config_field_display_replies_per_page,
            Lang::config_field_display_replies_per_page_help,
            display.replies_per_page,
            10,
            100
        ),
        number!(
            "display_sub_replies_per_page",
            Lang::config_field_display_sub_replies_per_page,
            no_help,
            display.sub_replies_per_page,
            5,
            50
        ),
        number!(
            "display_hot_days",
            Lang::config_field_display_hot_days,
            Lang::config_field_display_hot_days_help,
            display.hot_days,
            1,
            30
        ),
        number!(
            "display_hot_count",
            Lang::config_field_display_hot_count,
            no_help,
            display.hot_count,
            1,
            20
        ),
        number!(
            "display_latest_count",
            Lang::config_field_display_latest_count,
            no_help,
            display.latest_count,
            1,
            20
        ),
        number!(
            "display_recent_members",
            Lang::config_field_display_recent_members,
            no_help,
            display.recent_members,
            1,
            20
        ),
        number!(
            "display_highlight_min_likes",
            Lang::config_field_display_highlight_min_likes,
            Lang::config_field_display_highlight_min_likes_help,
            display.highlight_min_likes,
            1,
            100
        ),
        number!(
            "display_highlight_count",
            Lang::config_field_display_highlight_count,
            Lang::config_field_display_highlight_count_help,
            display.highlight_count,
            0,
            10
        ),
        number!(
            "display_ranks_shown",
            Lang::config_field_display_ranks_shown,
            Lang::config_field_display_ranks_shown_help,
            display.ranks_shown,
            10,
            100
        ),
        number!(
            "display_list_thumbnails",
            Lang::config_field_display_list_thumbnails,
            Lang::config_field_display_list_thumbnails_help,
            display.list_thumbnails,
            0,
            5
        ),
        flag!(
            "display_footer_metrics",
            Lang::config_field_display_footer_metrics,
            Lang::config_field_display_footer_metrics_help,
            display.footer_metrics
        ),
    ],
    visible: None,
};

static READING: Section = Section {
    id: "reading",
    title: Lang::config_section_reading,
    intro: Lang::config_section_reading_intro,
    fields: &[
        choice!(
            "reading_list_sort",
            Lang::config_field_reading_list_sort,
            no_help,
            reading.list_sort,
            &[
                ("latest", Lang::settings_sort_latest),
                ("created", Lang::settings_sort_created)
            ]
        ),
        choice!(
            "reading_thread_view",
            Lang::config_field_reading_thread_view,
            no_help,
            reading.thread_view,
            &[
                ("nested", Lang::settings_view_nested),
                ("flat", Lang::settings_view_flat)
            ]
        ),
        flag!(
            "reading_title_latest",
            Lang::config_field_reading_title_latest,
            no_help,
            reading.title_latest
        ),
        flag!(
            "reading_title_new_tab",
            Lang::config_field_reading_title_new_tab,
            no_help,
            reading.title_new_tab
        ),
        choice!(
            "reading_avatar_shape",
            Lang::config_field_reading_avatar_shape,
            no_help,
            reading.avatar_shape,
            &[
                ("rounded", Lang::settings_shape_rounded),
                ("circle", Lang::settings_shape_circle)
            ]
        ),
    ],
    visible: None,
};

static PARAMETERS_PAGE: FieldPage = FieldPage {
    path: "/manage/appearance/parameters",
    title: Lang::manage_tab_appearance_parameters,
    summary: Lang::config_page_parameters_summary,
    sections: &[&DISPLAY, &READING],
    upcoming: &[],
};

async fn parameters(AdminPage(state): AdminPage, Query(query): Query<SavedQuery>) -> Response {
    show_field_page(&state, &PARAMETERS_PAGE, &query).await
}

async fn save_parameters(
    AdminPage(state): AdminPage,
    CsrfForm(form): CsrfForm<HashMap<String, String>>,
) -> Response {
    save_field_page(&state, &PARAMETERS_PAGE, form).await
}

// ---- Tools and maintenance › Maintenance ------------------------------------------------------

static MAINTENANCE: Section = Section {
    id: "maintenance",
    title: Lang::config_section_maintenance,
    intro: Lang::config_section_maintenance_intro,
    fields: &[
        flag!(
            "debug_panel",
            Lang::config_field_debug_panel,
            Lang::config_field_debug_panel_help,
            debug.panel
        ),
        number!(
            "maintenance_notification_read_retention_days",
            Lang::config_field_maintenance_notification_read_retention_days,
            Lang::config_field_maintenance_notification_read_retention_days_help,
            maintenance.notification_read_retention_days,
            1,
            3650
        ),
        number!(
            "maintenance_notification_unread_retention_days",
            Lang::config_field_maintenance_notification_unread_retention_days,
            Lang::config_field_maintenance_notification_unread_retention_days_help,
            maintenance.notification_unread_retention_days,
            1,
            3650
        ),
        number!(
            "maintenance_outbox_retention_days",
            Lang::config_field_maintenance_outbox_retention_days,
            Lang::config_field_maintenance_outbox_retention_days_help,
            maintenance.outbox_retention_days,
            1,
            3650
        ),
        number!(
            "maintenance_prepared_mail_retention_days",
            Lang::config_field_maintenance_prepared_mail_retention_days,
            Lang::config_field_maintenance_prepared_mail_retention_days_help,
            maintenance.prepared_mail_retention_days,
            1,
            3650
        ),
        number!(
            "maintenance_request_retention_days",
            Lang::config_field_maintenance_request_retention_days,
            Lang::config_field_maintenance_request_retention_days_help,
            maintenance.request_retention_days,
            7,
            3650
        ),
    ],
    visible: None,
};

static MAINTENANCE_PAGE: FieldPage = FieldPage {
    path: "/manage/maintenance",
    title: Lang::manage_tab_maintenance,
    summary: Lang::config_page_maintenance_summary,
    sections: &[&MAINTENANCE],
    upcoming: &[],
};

async fn maintenance(AdminPage(state): AdminPage, Query(query): Query<SavedQuery>) -> Response {
    show_field_page(&state, &MAINTENANCE_PAGE, &query).await
}

async fn save_maintenance(
    AdminPage(state): AdminPage,
    CsrfForm(form): CsrfForm<HashMap<String, String>>,
) -> Response {
    save_field_page(&state, &MAINTENANCE_PAGE, form).await
}

// ---- Interface settings › Friend links: application rules block -------------------------------

static LINK_RULES: Section = Section {
    id: "link-rules",
    title: Lang::config_section_link_rules,
    intro: no_help,
    fields: &[
        flag!(
            "link_applications",
            Lang::config_field_link_applications,
            Lang::config_field_link_applications_help,
            links.applications
        ),
        number!(
            "links_home_count",
            Lang::config_field_links_home_count,
            Lang::config_field_links_home_count_help,
            links.home_count,
            1,
            50
        ),
        number!(
            "links_live_per_member",
            Lang::config_field_links_live_per_member,
            Lang::config_field_links_live_per_member_help,
            links.live_per_member,
            1,
            20
        ),
        number!(
            "links_reject_cooldown_days",
            Lang::config_field_links_reject_cooldown_days,
            Lang::config_field_links_reject_cooldown_days_help,
            links.reject_cooldown_days,
            0,
            365
        ),
        number!(
            "links_min_account_days",
            Lang::config_field_links_min_account_days,
            Lang::config_field_links_min_account_days_help,
            links.min_account_days,
            0,
            3650
        ),
        number!(
            "links_apply_per_hour",
            Lang::config_field_links_apply_per_hour,
            no_help,
            links.apply_per_hour,
            1,
            100
        ),
        number!(
            "links_apply_ip_per_hour",
            Lang::config_field_links_apply_ip_per_hour,
            Lang::config_field_links_apply_ip_per_hour_help,
            links.apply_ip_per_hour,
            1,
            1000
        ),
    ],
    visible: None,
};

/// The "Application rules" block of the friend-link page, rendered by the admin list page in `router/directory.rs`.
pub(super) fn link_rules_section(settings: &SiteSettings) -> SectionView {
    section_views(&[&LINK_RULES], settings)
        .pop()
        .expect("the application rules block is always shown")
}

async fn save_link_rules(
    AdminPage(state): AdminPage,
    CsrfForm(form): CsrfForm<HashMap<String, String>>,
) -> Response {
    static SECTIONS: [&Section; 1] = [&LINK_RULES];
    match update_sections(&state, &SECTIONS, &form).await {
        Ok(_) => auth::redirect("/manage/links?rules=saved"),
        Err(error) => block_failure(&state, error),
    }
}

// ---- Global settings › Credits: growth block --------------------------------------------------

static GROWTH: Section = Section {
    id: "growth",
    title: Lang::config_section_growth,
    intro: Lang::config_section_growth_intro,
    fields: &[
        number!(
            "growth_thread",
            Lang::config_field_growth_thread,
            no_help,
            growth.thread,
            0,
            1_000_000
        ),
        number!(
            "growth_reply",
            Lang::config_field_growth_reply,
            Lang::config_field_growth_reply_help,
            growth.reply,
            0,
            1_000_000
        ),
        number!(
            "growth_like",
            Lang::config_field_growth_like,
            no_help,
            growth.like,
            0,
            1_000_000
        ),
        number!(
            "growth_digest",
            Lang::config_field_growth_digest,
            no_help,
            growth.digest,
            0,
            1_000_000
        ),
        number!(
            "growth_accept",
            Lang::config_field_growth_accept,
            no_help,
            growth.accept,
            0,
            1_000_000
        ),
        Field {
            name: "growth_levels",
            label: Lang::config_field_growth_levels,
            help: Lang::config_field_growth_levels_help,
            kind: Kind::Text {
                get: |s| runtime::list_text(&s.growth.levels),
                set: |s, value| {
                    // The full-width comma (U+FF0C) separates as well, as typed with a Chinese input method.
                    let levels = value
                        .split([',', '\u{ff0c}'])
                        .map(|part| part.trim().parse::<i64>())
                        .collect::<std::result::Result<Vec<_>, _>>()
                        .map_err(|_| crate::i18n::current().config_error_levels_integers())?;
                    s.growth.levels = levels;
                    Ok(())
                },
                rows: 1,
                maxlength: 300,
            },
        },
        Field {
            name: "growth_level_names",
            label: Lang::config_field_growth_level_names,
            help: Lang::config_field_growth_level_names_help,
            kind: Kind::Text {
                get: |s| s.growth.level_names.join("\n"),
                set: |s, value| {
                    s.growth.level_names = value
                        .lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .map(str::to_owned)
                        .collect();
                    Ok(())
                },
                rows: 8,
                maxlength: 500,
            },
        },
        number!(
            "growth_link_level",
            Lang::config_field_growth_link_level,
            Lang::config_field_growth_link_level_help,
            growth.link_level,
            1,
            20
        ),
        number!(
            "growth_invite_level",
            Lang::config_field_growth_invite_level,
            Lang::config_field_growth_invite_level_help,
            growth.invite_level,
            1,
            20
        ),
        number!(
            "growth_activity_level",
            Lang::config_field_growth_activity_level,
            Lang::config_field_growth_activity_level_help,
            growth.activity_level,
            1,
            20
        ),
        number!(
            "ranks_refresh_seconds",
            Lang::config_field_ranks_refresh_seconds,
            Lang::config_field_ranks_refresh_seconds_help,
            ranks.refresh_seconds,
            10,
            86400
        ),
    ],
    visible: None,
};

/// The "Growth points" block of the credits page, rendered by the admin page in `router/activities.rs`.
pub(super) fn growth_section(settings: &SiteSettings) -> SectionView {
    section_views(&[&GROWTH], settings)
        .pop()
        .expect("the growth block is always shown")
}

async fn save_growth(
    AdminPage(state): AdminPage,
    CsrfForm(form): CsrfForm<HashMap<String, String>>,
) -> Response {
    static SECTIONS: [&Section; 1] = [&GROWTH];
    match update_sections(&state, &SECTIONS, &form).await {
        Ok(_) => auth::redirect("/manage/credits?done=growth-settings"),
        Err(error) => block_failure(&state, error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every field-table item can be read from the seed values and written back unchanged, and form names are unique.
    #[test]
    fn field_tables_round_trip_and_names_are_unique() {
        let pages: [&[&Section]; 6] = [
            REGISTRATION_PAGE.sections,
            POSTING_PAGE.sections,
            ACCOUNT_PAGE.sections,
            PARAMETERS_PAGE.sections,
            MAINTENANCE_PAGE.sections,
            &[&LINK_RULES, &GROWTH],
        ];
        let mut names = std::collections::HashSet::new();
        let seeded = runtime::settings::tests::seeded();
        for sections in pages {
            let mut form = HashMap::new();
            for section in sections {
                for field in section.fields {
                    assert!(
                        names.insert(field.name),
                        "repeated form name: {}",
                        field.name
                    );
                    let view = field_view(field, &seeded);
                    match view.kind {
                        "flag" => {
                            if view.checked {
                                form.insert(field.name.to_owned(), "true".to_owned());
                            }
                        }
                        "choice" => {
                            let selected = view.options.iter().find(|o| o.selected).unwrap();
                            form.insert(field.name.to_owned(), selected.value.to_owned());
                        }
                        _ => {
                            form.insert(field.name.to_owned(), view.value);
                        }
                    }
                }
            }
            let mut copy = seeded.clone();
            apply_form(sections, &form, &mut copy, true).unwrap();
            assert_eq!(copy, seeded);
        }
    }

    #[test]
    fn missing_fields_keep_values_and_malformed_ones_are_named() {
        let mut settings = runtime::settings::tests::seeded();
        settings.debug.panel = true;
        apply_form(&[&MAINTENANCE], &HashMap::new(), &mut settings, true).unwrap();
        assert!(
            !settings.debug.panel,
            "a checkbox that is not submitted is unchecked"
        );
        assert_eq!(
            settings.maintenance,
            runtime::settings::tests::seeded().maintenance,
            "a number that is not submitted keeps its value"
        );
        let form = HashMap::from([
            (
                "maintenance_outbox_retention_days".to_owned(),
                "thirty".to_owned(),
            ),
            (
                "maintenance_prepared_mail_retention_days".to_owned(),
                "7".to_owned(),
            ),
            (
                "maintenance_request_retention_days".to_owned(),
                "30".to_owned(),
            ),
        ]);
        let error = apply_form(&[&MAINTENANCE], &form, &mut settings, true).unwrap_err();
        assert!(
            error.contains("Notification and mail event retention")
                && error.contains("must be a whole number"),
            "{error}"
        );
        // When the member API sub-block is not submitted, the whole block stays untouched.
        let mut settings = runtime::settings::tests::seeded();
        settings.api.key_ttl_days = 99;
        let mut form = HashMap::new();
        for field in REGISTRATION.fields {
            if let Kind::Number { get, .. } = field.kind {
                form.insert(field.name.to_owned(), get(&settings).to_string());
            }
        }
        form.insert("registration".to_owned(), "invite".to_owned());
        apply_form(&[&REGISTRATION, &API], &form, &mut settings, true).unwrap();
        assert_eq!(settings.api.key_ttl_days, 99);
    }

    #[test]
    fn robots_warning_only_for_the_wildcard_group() {
        assert!(robots_blocks_all("User-agent: *\nDisallow: /"));
        assert!(robots_blocks_all(
            "User-agent: Googlebot\nUser-agent: *\nDisallow: / # all"
        ));
        assert!(!robots_blocks_all("User-agent: *\nDisallow: /manage"));
        assert!(!robots_blocks_all(
            "User-agent: BadBot\nDisallow: /\n\nUser-agent: *\nDisallow: /my"
        ));
        assert!(!robots_blocks_all(""));
    }
}
