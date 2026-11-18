//! The most recent channel from a referral link. Only collected when a visitor opens a public web page, and only
//! sent back on this site's registration path.
use axum::{
    extract::{MatchedPath, Request},
    http::{HeaderMap, HeaderValue, Method, header},
    response::Response,
};

use super::Viewer;
use crate::registration_sources::valid_channel;

const THIRTY_DAYS: i64 = 30 * 24 * 3600;

pub(super) fn landing(request: &Request) -> Option<String> {
    let query = request.uri().query()?;
    if request.method() != Method::GET {
        return None;
    }
    let headers = request.headers();
    if headers.contains_key("sec-purpose") || headers.contains_key("purpose") {
        return None; // Prefetching and prerendering are not an actual visit.
    }
    let boosted = headers.get("hx-boosted").is_some_and(|v| v == "true");
    if !boosted
        && (headers.contains_key("hx-request")
            || headers
                .get("sec-fetch-dest")
                .is_some_and(|v| v != "document"))
    {
        return None;
    }
    // Only public entry points that already matched count. /{segment} covers pagination of forums, threads and lists;
    // the handler writes the cookie only after confirming the URL exists (including canonical redirects). SMS sign-in
    // is a sign-up page too: a new number goes on to "Finish signing up". `/auth/{provider}` is left out: it is not a
    // page but the start of an authorization that leaves the site at once, and the sign-in and sign-up pages that link
    // to it are the places to land on.
    let route = request.extensions().get::<MatchedPath>()?;
    if !matches!(
        route.as_str(),
        "/" | "/{segment}"
            | "/register"
            | "/login"
            | "/login/sms"
            | "/forums"
            | "/feeds"
            | "/search"
            | "/search/users"
            | "/space/{uid}"
            | "/space/{uid}/{collection}"
            | "/posts/{pid}"
            | "/ranks"
            | "/credits/rules"
            | "/verified"
            | "/links"
            | "/terms"
            | "/rules"
            | "/privacy"
            | "/contact"
    ) {
        return None;
    }
    let mut values =
        url::form_urlencoded::parse(query.as_bytes()).filter(|(key, _)| key == "utm_source");
    let (_, value) = values.next()?;
    if values.next().is_some() {
        return None;
    }
    valid_channel(&value).map(str::to_owned)
}

fn cookie_name(viewer: &Viewer) -> Option<String> {
    // Named by the path prefix and the non-default port of the actual origin, like every cookie of this forum
    // ([`super::cookie_name`]); without a usable origin there is no channel to keep.
    url::Url::parse(viewer.origin.as_deref()?).ok()?;
    Some(viewer.path_cookie("gonof_registration_source"))
}

pub(super) fn read(headers: &HeaderMap, viewer: &Viewer) -> Option<String> {
    // At most 64 characters, each UTF-8 character at most 4 bytes, each byte encoded as %XX.
    let raw = super::unique_cookie(headers, &cookie_name(viewer)?, 64 * 4 * 3)?;
    let decoded = percent_encoding::percent_decode_str(raw)
        .decode_utf8()
        .ok()?;
    valid_channel(&decoded).map(str::to_owned)
}

pub(super) fn write(response: &mut Response, viewer: &Viewer, channel: Option<&str>) {
    let Some(name) = cookie_name(viewer) else {
        return;
    };
    let value: String =
        url::form_urlencoded::byte_serialize(channel.unwrap_or_default().as_bytes()).collect();
    let ttl = if channel.is_some() { THIRTY_DAYS } else { 0 };
    let secure_flag = if viewer.secure { "; Secure" } else { "" };
    let cookie = format!(
        "{name}={value}; Path={}; HttpOnly; SameSite=Lax; Max-Age={ttl}{secure_flag}",
        crate::site::href("/register")
    );
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).expect("channel and path were validated and encoded"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, body::Body, routing::get};
    use tower::ServiceExt;

    #[tokio::test]
    async fn hosts_with_multiple_ports_read_and_clear_only_their_own_channel() {
        let viewer = |origin: &str| Viewer {
            secure: true,
            origin: Some(origin.to_owned()),
            ..Viewer::default()
        };
        let usual = viewer("https://example.test");
        let other = viewer("https://example.test:8443");
        assert_eq!(
            cookie_name(&usual),
            cookie_name(&viewer("https://example.test:443"))
        );
        for prefix in ["", "/bbs"] {
            let site = crate::site::Site {
                prefix: prefix.into(),
                ..Default::default()
            };
            crate::site::scope(site, async {
                let mut response = Response::new(Body::empty());
                write(&mut response, &usual, Some("main"));
                write(&mut response, &other, Some("secondary"));
                let values: Vec<_> = response
                    .headers()
                    .get_all(header::SET_COOKIE)
                    .iter()
                    .map(|v| v.to_str().unwrap().split(';').next().unwrap())
                    .collect();
                let mut headers = HeaderMap::new();
                headers.insert(header::COOKIE, values.join("; ").parse().unwrap());
                assert_eq!(read(&headers, &usual).as_deref(), Some("main"));
                assert_eq!(read(&headers, &other).as_deref(), Some("secondary"));
                let mut clear = Response::new(Body::empty());
                write(&mut clear, &other, None);
                assert_eq!(
                    clear.headers()[header::SET_COOKIE].to_str().unwrap(),
                    format!(
                        "{}=; Path={prefix}/register; HttpOnly; SameSite=Lax; Max-Age=0; Secure",
                        cookie_name(&other).unwrap()
                    )
                );
            })
            .await;
        }
    }

    #[tokio::test]
    async fn only_visits_to_public_pages_can_replace_the_channel() {
        async fn check(request: Request) -> String {
            landing(&request).unwrap_or_default()
        }
        let app = Router::new()
            .route("/{segment}", get(check).post(check))
            .route("/login/sms", get(check))
            .route("/manage", get(check));
        for (method, path, headers, expected) in [
            ("GET", "/thread-1?utm_source=ad", vec![], "ad"),
            ("GET", "/thread-1?utm_source=a&utm_source=b", vec![], ""),
            ("GET", "/login/sms?utm_source=sms", vec![], "sms"),
            ("GET", "/manage?utm_source=ad", vec![], ""),
            ("POST", "/thread-1?utm_source=ad", vec![], ""),
            (
                "GET",
                "/thread-1?utm_source=ad",
                vec![("sec-purpose", "prefetch")],
                "",
            ),
            (
                "GET",
                "/thread-1?utm_source=ad",
                vec![("sec-fetch-dest", "image")],
                "",
            ),
            (
                "GET",
                "/thread-1?utm_source=ad",
                vec![("hx-request", "true")],
                "",
            ),
            (
                "GET",
                "/thread-1?utm_source=ad",
                vec![("hx-request", "true"), ("hx-boosted", "true")],
                "ad",
            ),
        ] {
            let mut req = Request::builder().method(method).uri(path);
            for (name, value) in headers {
                req = req.header(name, value);
            }
            let result = app
                .clone()
                .oneshot(req.body(Body::empty()).unwrap())
                .await
                .unwrap();
            let body = axum::body::to_bytes(result.into_body(), 1024)
                .await
                .unwrap();
            assert_eq!(body, expected, "{method} {path}");
        }
    }
}
