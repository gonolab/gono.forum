//! Extractors for page requests: the state assembly and CSRF check repeated at the start of every handler move into
//! types.
//!
//! `Page` builds this request's page state, and `CsrfForm` parses the form and checks the token. Their order is
//! guaranteed by axum: `FromRequestParts` always runs before `FromRequest`, which takes the body, so `CsrfForm` always
//! gets the state `Page` put into the request extensions.
//!
//! This turns CSRF from "every handler remembers to call it" into a fact of the signature: writing `CsrfForm` means it
//! was checked, and forgetting it no longer silently loses protection; you simply cannot get the form.
use std::sync::Arc;

use axum::{
    extract::{FromRequest, FromRequestParts, Request},
    http::request::Parts,
    response::{IntoResponse, Response},
};

use super::{
    AppState,
    auth::{self, Viewer},
};

/// This request's state that `Page` puts into the request extensions, for reuse by the body extractor that follows.
///
/// The separate wrapper keeps it distinct from the router's own `Arc<AppState>` in the extension map.
#[derive(Clone)]
struct RequestPage(AppState);

/// This request's page state: attaches the visitor and takes one page frame snapshot.
pub(super) struct Page(pub(super) AppState);

impl FromRequestParts<Arc<AppState>> for Page {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        // The identity middleware writes a Viewer for every non-static request; if missing it is treated as anonymous,
        // with no separate error.
        let viewer = parts
            .extensions
            .get::<Viewer>()
            .cloned()
            .unwrap_or_default();
        let state = auth::page_state(state, viewer).await?;
        parts.extensions.insert(RequestPage(state.clone()));
        Ok(Self(state))
    }
}

/// Admin page state: an administrator check on top of [`Page`].
///
/// Admin handlers use it instead of `Page`, so "must be an administrator", like "must check CSRF", lives in the
/// signature rather than relying on every handler remembering to call it.
///
/// It is as fresh as the session snapshot (`session.member.is_admin`): `Identity::session` already rereads the member
/// row on every request and checks freeze and credentials version. This extractor's value is putting the decision into
/// the signature and unifying the 403 message; pages that need a custom error message or want to avoid one more query
/// keep using the session snapshot, and the in-transaction recheck on write paths is the service layer's job. None of
/// the three replaces the others.
pub(super) struct AdminPage(pub(super) AppState);

impl FromRequestParts<Arc<AppState>> for AdminPage {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let Page(state) = Page::from_request_parts(parts, state).await?;
        let Some(session) = &state.viewer.session else {
            return Err(auth::redirect("/login?return_to=/manage"));
        };
        crate::administration::admin(&state.db, session.member.actor())
            .await
            .map_err(|e| auth::identity_error(&state, e))?;
        Ok(Self(state))
    }
}

/// A form carrying a CSRF token.
pub(super) trait CsrfToken {
    fn csrf_token(&self) -> &str;
}

/// Implements [`CsrfToken`] for a batch of form types, taking their `_csrf` field.
macro_rules! csrf_forms {
    ($($ty:ty),+ $(,)?) => {
        $(impl $crate::router::extract::CsrfToken for $ty {
            fn csrf_token(&self) -> &str {
                &self._csrf
            }
        })+
    };
}
pub(super) use csrf_forms;

/// Forms without fixed fields (activity voting, role assignment) take the token by key.
impl CsrfToken for std::collections::HashMap<String, String> {
    fn csrf_token(&self) -> &str {
        self.get("_csrf").map_or("", String::as_str)
    }
}

/// A form that has passed the CSRF check.
pub(super) struct CsrfForm<T>(pub(super) T);

impl<T> FromRequest<Arc<AppState>> for CsrfForm<T>
where
    T: CsrfToken + serde::de::DeserializeOwned + Send,
{
    type Rejection = Response;

    async fn from_request(
        request: Request,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let headers = request.headers().clone();
        let checker = verifier(request.extensions(), state);
        let axum::extract::Form(value) = axum::extract::Form::<T>::from_request(request, state)
            .await
            .map_err(IntoResponse::into_response)?;
        if let Some(reason) = auth::csrf_failure(&checker, &headers, value.csrf_token()) {
            return Err(auth::csrf_error(&checker, &reason));
        }
        Ok(Self(value))
    }
}

/// The state the token check relies on.
///
/// When a handler assembles state some other way (the admin area and forum management each had one such place), the
/// extensions hold no Page. The check must not be skipped because of that, so it falls back to the base state plus the
/// Viewer written by the identity middleware: `csrf_ok` only needs those two, and the only difference is an error page
/// without the page frame.
fn verifier(extensions: &axum::http::Extensions, state: &Arc<AppState>) -> AppState {
    match extensions.get::<RequestPage>() {
        Some(RequestPage(state)) => state.clone(),
        None => {
            let mut fallback = AppState::clone(state);
            fallback.viewer = extensions.get::<Viewer>().cloned().unwrap_or_default();
            fallback
        }
    }
}

/// A form together with its token check result.
///
/// Parse failures and expired tokens are kept apart: on an expired token a handler usually re-renders the form page
/// with what the user already filled in, rather than dropping a uniform error page as [`CsrfForm`] does. The check is
/// still done by the extractor; the handler only decides how to respond, and without `Valid` there are no usable form
/// values.
pub(super) enum CheckedForm<T> {
    Valid(T),
    Malformed,
    StaleToken(T, String),
}

impl<T> FromRequest<Arc<AppState>> for CheckedForm<T>
where
    T: CsrfToken + serde::de::DeserializeOwned + Send,
{
    type Rejection = std::convert::Infallible;

    async fn from_request(
        request: Request,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let headers = request.headers().clone();
        let checker = verifier(request.extensions(), state);
        let Ok(axum::extract::Form(value)) =
            axum::extract::Form::<T>::from_request(request, state).await
        else {
            return Ok(Self::Malformed);
        };
        Ok(
            match auth::csrf_failure(&checker, &headers, value.csrf_token()) {
                None => Self::Valid(value),
                Some(reason) => Self::StaleToken(value, reason),
            },
        )
    }
}
