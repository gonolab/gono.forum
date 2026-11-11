//! The admin module "Languages": the site language, how complete each compiled-in language is and where it is used,
//! and the mail templates and policies each language has customized (each links to its edit page with `?lang=`).
//! Administrators only. The domain language lives in the domain form, the brand words in "Global settings › Site
//! information".
use super::extract::{AdminPage, CsrfForm, csrf_forms};
use super::*;
use crate::{i18n::Lang, runtime};

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/manage/languages", get(page).post(save))
}

/// One compiled-in language.
struct LanguageRow {
    tag: &'static str,
    name: &'static str,
    percent: usize,
    translated: usize,
    total: usize,
    site: bool,
    /// The other domains whose own language it is.
    domains: Vec<String>,
    /// Customized texts in this language: label and edit address.
    texts: Vec<(String, String)>,
}

#[derive(Template)]
#[template(path = "manage/languages.html")]
struct LanguagesPage<'a> {
    p: PageMeta<'a>,
    rows: Vec<LanguageRow>,
    saved: bool,
    message: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct SavedQuery {
    saved: String,
}

/// The label of one customized kind (`mail.content`, `policy.terms`) and its edit address in that language.
fn text_link(kind: &str, lang: Lang) -> Option<(String, String)> {
    if let Some(mail) = kind.strip_prefix("mail.") {
        let label = (crate::mail::templates::KINDS
            .iter()
            .find(|(value, _)| *value == mail)?
            .1)(crate::i18n::current());
        return Some((
            crate::i18n::current().language_text_mail(label),
            format!("/manage/mail/templates/{mail}?lang={}", lang.tag()),
        ));
    }
    let policy = kind.strip_prefix("policy.")?;
    let t = crate::i18n::current();
    let label = match policy {
        "terms" => t.frame_footer_terms(),
        "privacy" => t.frame_footer_privacy(),
        "rules" => t.manage_tab_policy_rules(),
        "contact" => t.frame_footer_contact(),
        "verified" => t.manage_tab_policy_verified(),
        "links" => t.manage_tab_policy_links(),
        _ => return None,
    };
    Some((
        t.language_text_policy(label),
        format!("/manage/policies/{policy}?lang={}", lang.tag()),
    ))
}

async fn view(state: &AppState, saved: bool, message: String, status: StatusCode) -> Response {
    let (settings, rows, domains) = match tokio::try_join!(
        async { runtime::read(&state.db).await.map_err(anyhow::Error::new) },
        async {
            runtime::localized_rows(&state.db)
                .await
                .map_err(anyhow::Error::new)
        },
        crate::domains::DomainMap::load(&state.db),
    ) {
        Ok(found) => found,
        Err(error) => return database_error(state, error),
    };
    let site = settings.site.lang();
    let languages = Lang::all()
        .map(|lang| {
            let (translated, total) = lang.completion();
            LanguageRow {
                tag: lang.tag(),
                name: lang.name(),
                percent: lang.percent(),
                translated,
                total,
                site: lang == site,
                domains: domains
                    .rows()
                    .iter()
                    // The main domain row never has a language of its own (it follows the site).
                    .filter(|row| row.language == Some(lang))
                    .map(|row| row.site_url.clone())
                    .collect(),
                texts: rows
                    .iter()
                    .filter(|row| row.language == lang)
                    .filter_map(|row| text_link(&row.kind, lang))
                    .collect(),
            }
        })
        .collect();
    let mut response = render(&LanguagesPage {
        p: meta(
            state,
            crate::i18n::current().manage_tab_languages().into(),
            crate::i18n::current().language_description().into(),
            "/manage/languages",
            "",
            "",
        ),
        rows: languages,
        saved,
        message,
    });
    *response.status_mut() = status;
    response
}

async fn page(AdminPage(state): AdminPage, Query(query): Query<SavedQuery>) -> Response {
    view(&state, query.saved == "1", String::new(), StatusCode::OK).await
}

#[derive(Deserialize)]
struct LanguageForm {
    _csrf: String,
    language: String,
}
csrf_forms!(LanguageForm);

/// The signed-in administrator (`AdminPage` already required one).
fn actor(state: &AppState) -> crate::identity::Actor {
    state
        .viewer
        .session
        .as_ref()
        .expect("AdminPage requires a session")
        .member
        .actor()
}

/// Saves the site language through the site settings' own save (the same write gate, audit and broadcast as every
/// other setting).
async fn save(AdminPage(state): AdminPage, CsrfForm(form): CsrfForm<LanguageForm>) -> Response {
    let Some(lang) = Lang::parse(form.language.trim()) else {
        return view(
            &state,
            false,
            crate::i18n::current().language_error_unknown().into(),
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    };
    match runtime::update(
        &state.db,
        &state.global,
        actor(&state),
        "site.settings.save",
        |settings| settings.site.language = lang.tag().to_owned(),
    )
    .await
    {
        Ok(_) => auth::redirect("/manage/languages?saved=1"),
        Err(crate::identity::IdentityError::Validation(message)) => {
            view(
                &state,
                false,
                message.into(),
                StatusCode::UNPROCESSABLE_ENTITY,
            )
            .await
        }
        Err(error) => auth::identity_error(&state, error),
    }
}
