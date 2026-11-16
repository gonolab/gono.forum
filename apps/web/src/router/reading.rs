//! `/my/reading`: reading and list preferences, a native form submitted as a whole page, saved by version.
//!
//! The same form can also set the avatar shape. Every item can choose "Follow the site" (NULL in the database),
//! following the admin-area site defaults `settings.reading.*`. Members have no personal time zone; the whole site
//! uses only the site time zone.
use super::extract::{CsrfForm, Page};
use super::*;
use crate::rate_limit::Rule;
use crate::reading::{self, Choices, Reading};
use axum::extract::DefaultBodyLimit;
use std::collections::HashMap;

const PATH: &str = "/my/reading";

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(PATH, get(page).post(save))
        .layer(DefaultBodyLimit::max(16 * 1024))
}

#[derive(Template)]
#[template(path = "settings/reading.html")]
struct ReadingTemplate<'a> {
    p: PageMeta<'a>,
    frame: super::my::Frame,
    settings: Reading,
    message: String,
    success: bool,
    /// The shape the current skin fixes for every avatar (docs/skins.md, "Frame"), worded; `None` when the skin
    /// leaves it to the member.
    skin_avatar: Option<&'static str>,
}

fn login() -> Response {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("return_to", PATH)
        .finish();
    auth::redirect(&format!("/login?{query}"))
}

async fn respond(
    state: &AppState,
    draft: Option<(Choices, i64)>,
    message: String,
    status: StatusCode,
) -> Response {
    if state.viewer.session.is_none() {
        return login();
    }
    let settings = match draft {
        // Fill back the unsaved choices: merge with the site defaults into actual values, only so the form shows them
        // as they were.
        Some((chosen, version)) => Reading::resolve(
            chosen,
            state.frame.reading.site,
            version,
            state.frame.reading.css_version,
        ),
        None => state.frame.reading.clone(),
    };
    let mut p = meta(
        state,
        crate::i18n::current().settings_reading_title().into(),
        crate::i18n::current().settings_reading_description().into(),
        PATH,
        "",
        "",
    );
    if let Some(error) = super::my::fill_shell(state, &mut p).await {
        return error;
    }
    let mut response = render(&ReadingTemplate {
        frame: super::my::frame("reading"),
        p,
        settings,
        message,
        success: status.is_success(),
        skin_avatar: {
            let t = crate::i18n::current();
            match state.frame.skin.manifest.frame.avatar_shape {
                crate::skins::AvatarShape::Member => None,
                crate::skins::AvatarShape::Circle => Some(t.skin_avatar_circle()),
                crate::skins::AvatarShape::Rounded => Some(t.skin_avatar_rounded()),
                crate::skins::AvatarShape::Square => Some(t.skin_avatar_square()),
            }
        },
    });
    *response.status_mut() = status;
    response
}

#[derive(Default, Deserialize)]
struct PageQuery {
    #[serde(default)]
    saved: bool,
}

async fn page(Page(state): Page, Query(query): Query<PageQuery>) -> Response {
    let message = if query.saved {
        crate::i18n::current().settings_reading_saved().into()
    } else {
        String::new()
    };
    respond(&state, None, message, StatusCode::OK).await
}

/// Whole-page submission: each item is a group of radio buttons; an empty value (or the item not submitted) means
/// follow the site; if present it must be a known value.
fn parse(form: &HashMap<String, String>) -> Result<(Choices, i64), &'static str> {
    let version = form
        .get("version")
        .and_then(|v| v.parse::<i64>().ok())
        .ok_or(crate::i18n::current().settings_error_reading_version())?;
    fn pick<T>(
        form: &HashMap<String, String>,
        key: &str,
        parse: impl Fn(&str) -> Option<T>,
        message: &'static str,
    ) -> Result<Option<T>, &'static str> {
        match form.get(key).map(String::as_str) {
            None | Some("") => Ok(None),
            Some(value) => parse(value).map(Some).ok_or(message),
        }
    }
    let flag = |value: &str| match value {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    };
    Ok((
        Choices {
            list_sort: pick(
                form,
                "list_sort",
                Reading::parse_sort,
                crate::i18n::current().settings_error_reading_sort(),
            )?,
            flat: pick(
                form,
                "thread_view",
                Reading::parse_view,
                crate::i18n::current().settings_error_reading_view(),
            )?,
            title_latest: pick(
                form,
                "title_latest",
                flag,
                crate::i18n::current().settings_error_reading_option(),
            )?,
            title_new_tab: pick(
                form,
                "title_new_tab",
                flag,
                crate::i18n::current().settings_error_reading_option(),
            )?,
            avatar_circle: pick(
                form,
                "avatar_shape",
                Reading::parse_avatar_shape,
                crate::i18n::current().settings_error_reading_avatar(),
            )?,
        },
        version,
    ))
}

async fn save(Page(state): Page, CsrfForm(form): CsrfForm<HashMap<String, String>>) -> Response {
    let Some(session) = &state.viewer.session else {
        return login();
    };
    match state
        .limit(&[Rule::new("reading-settings", session.member.uid, 30, 300)])
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            return error_page(
                &state,
                StatusCode::TOO_MANY_REQUESTS,
                crate::i18n::current().settings_error_save_too_many(),
            );
        }
        Err(unavailable) => return unavailable,
    }
    let draft = match parse(&form) {
        Ok(draft) => draft,
        Err(message) => {
            // When filling back, keep the saved values and only state the error clearly.
            return respond(
                &state,
                None,
                message.into(),
                StatusCode::UNPROCESSABLE_ENTITY,
            )
            .await;
        }
    };
    match reading::save(
        &state.db,
        &state.cache,
        session.member.actor(),
        &draft.0,
        draft.1,
    )
    .await
    {
        Ok(()) => auth::redirect(&format!("{PATH}?saved=true")),
        Err(error) => {
            let (status, message) = posting::write_error(error);
            respond(&state, Some(draft), message, status).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn form_requires_known_values_and_treats_blank_as_follow_site() {
        let (parsed, version) = parse(&form(&[
            ("version", "3"),
            ("list_sort", "created"),
            ("thread_view", "flat"),
            ("title_new_tab", "true"),
            ("title_latest", "false"),
        ]))
        .unwrap();
        assert_eq!(version, 3);
        assert_eq!(parsed.list_sort, Some(crate::urls::Sort::Created));
        assert_eq!(parsed.flat, Some(true));
        assert_eq!(
            (parsed.title_new_tab, parsed.title_latest),
            (Some(true), Some(false))
        );
        assert_eq!(parsed.avatar_circle, None);
        let display = parse(&form(&[
            ("version", "0"),
            ("list_sort", ""),
            ("thread_view", ""),
            ("avatar_shape", "circle"),
        ]))
        .unwrap()
        .0;
        assert_eq!(display.avatar_circle, Some(true));
        assert_eq!(
            (display.list_sort, display.flat),
            (None, None),
            "empty values follow the site"
        );
        for bad in [
            form(&[("version", "0"), ("list_sort", "pinned")]),
            form(&[("version", "0"), ("thread_view", "tree")]),
            form(&[("version", "x"), ("list_sort", "latest")]),
            form(&[("version", "0"), ("title_latest", "on")]),
            form(&[("version", "0"), ("avatar_shape", "square")]),
        ] {
            assert!(parse(&bad).is_err());
        }
    }
}
