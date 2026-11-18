use super::extract::{AdminPage, CsrfForm, Page, csrf_forms};
use super::*;
use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Multipart, Path},
    http::HeaderMap,
    routing::post,
};
use tokio::io::AsyncWriteExt;

pub(super) fn routes(upload_limit: usize) -> Router<Arc<AppState>> {
    Router::new()
        .route("/terms", get(policy))
        .route("/privacy", get(policy))
        .route("/rules", get(policy))
        .route("/contact", get(policy))
        .route("/robots.txt", get(robots))
        .route("/sitemap.xml", get(sitemap))
        .route("/forum-icon/{fid}/{aid}", get(forum_icon))
        .route("/site.webmanifest", get(manifest))
        .route("/brand/{slot}/{aid}/{variant}", get(brand))
        .route(
            "/brand/domain/{domain_id}/{slot}/{aid}/{variant}",
            get(domain_brand),
        )
        .route("/manage/branding", get(branding))
        .route("/manage/branding/upload", post(upload_brand))
        .route("/manage/branding/{slot}", post(save_brand))
        .route("/manage/branding/preview/{aid}", get(preview_brand))
        .layer(DefaultBodyLimit::max(upload_limit))
}
#[derive(Template)]
#[template(path = "site/policy.html")]
struct Policy<'a> {
    p: PageMeta<'a>,
    title: String,
    body: String,
    /// 0 means this document has never been saved; the page shows no update date.
    updated_at: i64,
    updated: String,
    /// Contact e-mail and in-site entry points always appended below the contact page body; `false` for
    /// the other policy pages.
    contact: bool,
    contact_email: String,
}
/// The built-in title and body of a policy (`kind` without the `policy.` prefix: terms, contact, privacy, rules,
/// verified, links) in `t`: what the public page shows while that language is not customized, and the placeholder of
/// the admin edit form. Numbers come from the settings at render time: the privacy notice's retention days for former
/// usernames (`settings.username.history_days`) and the friend-link rules; the cooling-off and export retention
/// periods are code constants and written out as is.
pub(super) fn builtin_policy(
    kind: &str,
    t: crate::i18n::Lang,
    settings: &crate::runtime::SiteSettings,
) -> (String, String) {
    match kind {
        "terms" => (
            t.frame_footer_terms().into(),
            t.policy_terms_body(crate::site::site_name(settings)),
        ),
        "contact" => (
            t.frame_footer_contact().into(),
            t.policy_contact_body().into(),
        ),
        "privacy" => (
            t.frame_footer_privacy().into(),
            t.policy_privacy_body(settings.username.history_days),
        ),
        "verified" => (
            t.policy_verified_title().into(),
            t.policy_verified_body().into(),
        ),
        "links" => (
            t.policy_links_title().into(),
            t.policy_links_body(
                settings.links.live_per_member,
                settings.links.reject_cooldown_days,
                settings.links.min_account_days,
            ),
        ),
        _ => (t.frame_footer_rules().into(), t.policy_rules_body().into()),
    }
}

async fn policy(Page(state): Page, OriginalUri(uri): OriginalUri) -> Response {
    let kind = uri.path().trim_start_matches('/');
    let mut doc = match state
        .global
        .policy(&state.db, &format!("policy.{kind}"), state.cache_ttl())
        .await
    {
        Ok(d) => (*d).clone(),
        Err(e) => return auth::identity_error(&state, e),
    };
    if doc.updated_at == 0 {
        (doc.title, doc.body) = builtin_policy(kind, crate::i18n::current(), &state.frame.settings);
    }
    let body = crate::markdown::render(&doc.body, &std::collections::BTreeMap::new(), None);
    // The description is the first 160 characters of the rendered body as plain text (same as thread
    // excerpts); when the body is empty it falls back to the site description.
    let description = match crate::markdown::rendered_excerpt(&body, 160) {
        excerpt if excerpt.trim().is_empty() => state.frame.settings.site.description.clone(),
        excerpt => excerpt,
    };
    render(&Policy {
        p: meta(&state, doc.title.clone(), description, uri.path(), "", ""),
        title: doc.title,
        body,
        updated_at: doc.updated_at,
        updated: crate::clock::date(doc.updated_at, &crate::clock::current()),
        contact: kind == "contact",
        contact_email: state.frame.settings.site.contact_email.clone(),
    })
}
/// robots.txt. When indexing is off it is always `Disallow: /` and
/// `settings.seo.robots` is not read, so the test-site policy cannot be bypassed by edited content; when
/// indexing is on it outputs the text edited in the admin area, and the system appends `Sitemap:` lines at
/// the end (they follow the site URL automatically; `Sitemap:` lines the administrator wrote are kept as
/// is, the protocol allows several).
async fn robots(
    State(state): State<Arc<AppState>>,
    auth::RequestOrigin(origin): auth::RequestOrigin,
) -> Response {
    // Not through the page frame: settings come from the process cache.
    let setting = match state.global.settings(&state.db).await {
        Ok(block) => block.settings.clone(),
        Err(e) => return plain_settings_error(e),
    };
    let base = if setting.seo.indexing {
        match crate::runtime::site_origin(&setting, origin.as_deref()) {
            Ok(base) => base,
            Err(error) => {
                return plain_settings_error(crate::identity::IdentityError::Internal(
                    anyhow::Error::new(error),
                ));
            }
        }
    } else {
        String::new()
    };
    // Other domains whose canonical is the main domain have no sitemap.
    let base = if no_sitemap() { String::new() } else { base };
    let mut body = robots_body(&setting, &base, &crate::site::prefix());
    // The same host has a root-path entry: the root-path entry also outputs the rules and `Sitemap:` of the
    // host's sub-paths.
    if setting.seo.indexing {
        for (prefix, url) in host_sub_paths() {
            let sitemap = if url.is_empty() { String::new() } else { url };
            body.push('\n');
            body.push_str(&robots_body(&setting, &sitemap, &prefix));
        }
    }
    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], body).into_response()
}

/// The site of this request is another domain whose canonical is the main domain: it has no sitemap.
fn no_sitemap() -> bool {
    crate::site::other(|row| row.canonical == crate::domains::Canonical::Main).unwrap_or(false)
}

/// When this request falls on the root-path entry of a host, the other enabled sub-paths of the same host:
/// `(prefix, site URL used for the sitemap)`; an empty URL means that entry has no sitemap. Empty when not
/// the root path or when multi-domain sites are off.
fn host_sub_paths() -> Vec<(String, String)> {
    let Some(domains) = crate::site::domains() else {
        return Vec::new();
    };
    let Some(current) = domains.row().filter(|row| row.prefix().is_empty()) else {
        return Vec::new();
    };
    let main = domains.main().map(|main| main.domain_id);
    domains
        .map
        .rows()
        .iter()
        .filter(|row| {
            row.enabled && row.authority() == current.authority() && !row.prefix().is_empty()
        })
        .map(|row| {
            let own_sitemap =
                Some(row.domain_id) == main || row.canonical == crate::domains::Canonical::Own;
            (
                row.prefix().to_owned(),
                if own_sitemap {
                    row.site_url.clone()
                } else {
                    String::new()
                },
            )
        })
        .collect()
}

/// robots.txt body. When indexing is off it is only `Disallow: {prefix}/`; when on it is the admin-area text
/// (normalized to LF with trailing blank lines removed when saved), plus the system `Sitemap:` (`base` is the
/// site URL, already prefixed; omitted when empty, i.e. for multi-domain sites whose canonical is the main
/// domain).
///
/// In a sub-path deployment (`prefix` non-empty) `Allow` and `Disallow` values starting with `/` get the
/// prefix: crawlers only read the robots of the domain root, so the administrator has to merge this one in,
/// and the admin-area "SEO" page shows the same text. The stored rules are
/// still written relative to the root.
pub(super) fn robots_body(
    setting: &crate::runtime::SiteSettings,
    base: &str,
    prefix: &str,
) -> String {
    if !setting.seo.indexing {
        return format!("User-agent: *\nDisallow: {prefix}/\n");
    }
    let sitemap = if base.is_empty() {
        String::new()
    } else {
        format!("Sitemap: {base}/sitemap.xml\n")
    };
    let rules = prefixed_rules(&setting.seo.robots, prefix);
    if rules.is_empty() {
        sitemap
    } else {
        format!("{rules}\n{sitemap}")
    }
}

/// Adds the prefix to values starting with `/` in `Allow` and `Disallow` (case-insensitive); other lines
/// are kept as they are.
fn prefixed_rules(rules: &str, prefix: &str) -> String {
    if prefix.is_empty() {
        return rules.to_owned();
    }
    rules
        .split('\n')
        .map(|line| {
            let Some((key, value)) = line.split_once(':') else {
                return line.to_owned();
            };
            let field = key.trim();
            let path = value.trim();
            if (field.eq_ignore_ascii_case("allow") || field.eq_ignore_ascii_case("disallow"))
                && path.starts_with('/')
            {
                format!("{key}: {prefix}{path}")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// robots and the sitemap bypass the session and the page frame, so errors come back as plain text instead of
/// an HTML error page that depends on the frame. A broken setting still names the key (like the error page
/// of `router::database_error`).
fn plain_settings_error(error: crate::identity::IdentityError) -> Response {
    let message = match &error {
        crate::identity::IdentityError::Internal(inner) => {
            match inner.downcast_ref::<crate::runtime::SettingsError>() {
                Some(setting) => {
                    tracing::error!(key = %setting.key, found = %setting.found_text(), expected = setting.expected, "invalid site setting");
                    format!(
                        "{}\n",
                        crate::i18n::current().site_plain_settings(
                            setting.key.as_str(),
                            setting.found_text(),
                            setting.expected
                        )
                    )
                }
                None => {
                    let detail = crate::debug::visible_error(inner);
                    tracing::error!(error = %detail, "failed to read site settings");
                    format!(
                        "{}\n",
                        crate::i18n::current().site_plain_settings_failed(detail)
                    )
                }
            }
        }
        other => format!("{other}\n"),
    };
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        message,
    )
        .into_response()
}
#[derive(Default, Deserialize)]
struct SitemapQuery {
    page: Option<i64>,
}
async fn sitemap(
    State(state): State<Arc<AppState>>,
    auth::RequestOrigin(origin): auth::RequestOrigin,
    Query(query): Query<SitemapQuery>,
) -> Response {
    // Not through the page frame: settings come from the process cache.
    let setting = match state.global.settings(&state.db).await {
        Ok(block) => block.settings.clone(),
        Err(e) => return plain_settings_error(e),
    };
    if !setting.seo.indexing {
        return ([(header::CONTENT_TYPE,"application/xml; charset=utf-8")],"<?xml version=\"1.0\" encoding=\"UTF-8\"?><urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\"></urlset>").into_response();
    }
    if no_sitemap() {
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            format!("{}\n", crate::i18n::current().site_plain_no_sitemap()),
        )
            .into_response();
    }
    // Forums readable by anonymous visitors come from the anonymous copy of the process forum cache, and a
    // thread's readable scope is judged by it (the thread row has no copy of its forum's visibility);
    // multi-domain sites list only forums and threads whose canonical site is this one.
    let forums = match state
        .global
        .forums(&state.db, false, setting.cache_ttl())
        .await
    {
        Ok(v) => v,
        Err(e) => return database_error(&state, e),
    };
    let readable: Vec<i64> = forums.iter().map(|forum| forum.fid).collect();
    let homed: Vec<&crate::organization::ForumRecord> = forums
        .iter()
        .filter(|forum| {
            let top = forum.parent_fid.unwrap_or(forum.fid);
            crate::site::shows(top) && crate::site::canonical_base(Some(top)).is_none()
        })
        .collect();
    let thread_scope = if homed.len() == forums.len() {
        String::new()
    } else if homed.is_empty() {
        content::scope_sql("t.fid", &[0])
    } else {
        content::scope_sql(
            "t.fid",
            &homed.iter().map(|forum| forum.fid).collect::<Vec<_>>(),
        )
    };
    let base = match crate::runtime::site_origin(&setting, origin.as_deref()) {
        Ok(base) => crate::markdown::escape(&base),
        Err(error) => {
            return plain_settings_error(crate::identity::IdentityError::Internal(
                anyhow::Error::new(error),
            ));
        }
    };
    // A withdrawn thread shows only "Thread withdrawn" to the outside and is not put into the sitemap.
    let clause = format!(
        "FROM gonof_forum_thread t JOIN gonof_forum_post firstpost ON firstpost.first_tid=t.tid WHERE {}{thread_scope} AND t.deleted_at = 0 AND firstpost.deleted_at = 0",
        content::public_threads(&readable)
    );
    // Segmented by thread id: page N covers tid ∈ ((N-1)·5000, N·5000]. Indexing and paging need only
    // MAX(tid) and primary-key range scans, with no full-table COUNT each time and no OFFSET that gets deeper
    // and deeper; the cost is that some pages have fewer than 5000 entries or are even empty, which the
    // protocol allows.
    const SPAN: i64 = 5000;
    let max_tid =
        match crate::db::query_scalar::<Option<i64>>("SELECT MAX(tid) FROM gonof_forum_thread")
            .fetch_one(state.db.executor())
            .await
        {
            Ok(n) => n.unwrap_or(0),
            Err(e) => return database_error(&state, e.into()),
        };
    let pages = ((max_tid + SPAN - 1) / SPAN).clamp(1, 50000);
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>");
    if query.page.is_none() {
        xml.push_str("<sitemapindex xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">");
        for page in 1..=pages {
            xml.push_str(&format!(
                "<sitemap><loc>{base}/sitemap.xml?page={page}</loc></sitemap>"
            ));
        }
        xml.push_str("</sitemapindex>");
    } else {
        let page = query.page.unwrap_or(1);
        if page < 1 || page > pages {
            // The sitemap bypasses the session and the page frame; an invalid page number returns a
            // plain-text 404 instead of an HTML error page that depends on the frame.
            return (
                StatusCode::NOT_FOUND,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                format!("{}\n", crate::i18n::current().site_plain_sitemap_page()),
            )
                .into_response();
        }
        // `<lastmod>` is the later of the last-reply time and the first-post edit time: only two more
        // raw columns are selected and compared in code.
        let rows = match crate::db::query(&format!(
            "SELECT t.tid,t.lastpost,firstpost.edited_at {clause} AND t.tid > ? AND t.tid <= ? ORDER BY t.tid"
        ))
        .bind((page - 1) * SPAN)
        .bind(page * SPAN)
        .fetch_all(state.db.executor())
        .await
        {
            Ok(v) => v,
            Err(e) => return database_error(&state, e.into()),
        };
        let mut entries = Vec::with_capacity(rows.len());
        for row in rows {
            let decoded = (|| -> Result<(i64, i64), sqlx::Error> {
                let lastpost: i64 = row.try_get("lastpost")?;
                let edited: i64 = row.try_get("edited_at")?;
                Ok((row.try_get("tid")?, lastpost.max(edited)))
            })();
            match decoded {
                Ok(entry) => entries.push(entry),
                Err(e) => return database_error(&state, e.into()),
            }
        }
        xml.push_str("<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">");
        if page == 1 {
            // Verified members and friend links belong only to the main site and appear only in the main
            // domain's sitemap (multi-domain sites).
            let on_other = crate::site::other(|_| ()).is_some();
            for path in [
                "/",
                "/terms",
                "/privacy",
                "/rules",
                "/contact",
                "/feeds",
                "/forums",
                "/verified",
                "/links",
            ] {
                if on_other && crate::site::main_only(path) {
                    continue;
                }
                xml.push_str(&format!("<url><loc>{base}{path}</loc></url>"));
            }
            for forum in homed.iter() {
                xml.push_str(&format!(
                    "<url><loc>{base}/{}</loc></url>",
                    crate::markdown::escape(&forum.slug)
                ));
            }
        }
        for (tid, modified) in entries {
            xml.push_str(&format!("<url><loc>{base}/thread-{tid}</loc>"));
            if let Some(at) = chrono::DateTime::from_timestamp(modified, 0) {
                xml.push_str(&format!(
                    "<lastmod>{}</lastmod>",
                    at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                ));
            }
            xml.push_str("</url>");
        }
        xml.push_str("</urlset>");
    }
    (
        [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
        xml,
    )
        .into_response()
}
fn image_response(bytes: bytes::Bytes, mime: String) -> Response {
    let mut r = bytes.into_response();
    if let Ok(v) = HeaderValue::from_str(&mime) {
        r.headers_mut().insert(header::CONTENT_TYPE, v);
    }
    r.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=86400"),
    );
    r
}
async fn brand(
    State(state): State<Arc<AppState>>,
    Path((slot, aid, variant)): Path<(String, i64, String)>,
) -> Response {
    match state.media.read_brand(&slot, aid, &variant).await {
        Ok((bytes, mime)) => image_response(bytes, mime),
        // This route does not go through the session middleware, so there is no page frame to render; an
        // image request has no use for an HTML error page either.
        Err(_) => (
            StatusCode::NOT_FOUND,
            [(header::CACHE_CONTROL, "no-store")],
            format!("{}\n", crate::i18n::current().site_plain_no_brand_image()),
        )
            .into_response(),
    }
}
/// A multi-domain site's own logo and favicon for one domain.
async fn domain_brand(
    State(state): State<Arc<AppState>>,
    Path((domain_id, slot, aid, variant)): Path<(i64, String, i64, String)>,
) -> Response {
    match state
        .media
        .read_domain_brand(domain_id, &slot, aid, &variant)
        .await
    {
        Ok((bytes, mime)) => image_response(bytes, mime),
        Err(_) => (
            StatusCode::NOT_FOUND,
            [(header::CACHE_CONTROL, "no-store")],
            format!("{}\n", crate::i18n::current().site_plain_no_brand_image()),
        )
            .into_response(),
    }
}

async fn manifest(State(state): State<Arc<AppState>>) -> Response {
    // Not through the page frame: settings and brand images come from the process cache.
    let settings = match state.global.settings(&state.db).await {
        Ok(block) => block.settings.clone(),
        Err(error) => return auth::identity_error(&state, error),
    };
    let assets = match state
        .global
        .branding(&state.media, settings.cache_ttl())
        .await
    {
        Ok(a) => a,
        Err(e) => return database_error(&state, e),
    };
    let aid = assets
        .iter()
        .find(|a| a.slot == "app_icon")
        .and_then(|a| a.aid);
    // All URLs get this request's path prefix; `scope` is `prefix/`, the same as the home page.
    let icons:Vec<_>=[192,512].into_iter().map(|size|serde_json::json!({"src":crate::site::href(aid.map(|id|format!("/brand/app_icon/{id}/{size}")).unwrap_or_else(||format!("/static/img/app-{size}.png"))),"sizes":format!("{size}x{size}"),"type":"image/png"})).collect();
    let home = crate::site::href("/");
    // Other domains use their own site name (multi-domain sites); the short name equals the header
    // text.
    let name = crate::site::site_name(&settings);
    let short_name = crate::site::other(|row| row.site_name.clone())
        .flatten()
        .unwrap_or_else(|| settings.site.header_brand().to_owned());
    ([(header::CONTENT_TYPE,"application/manifest+json")],serde_json::json!({"name":name,"short_name":short_name,"start_url":home,"scope":home,"display":"standalone","icons":icons}).to_string()).into_response()
}
#[derive(Template)]
#[template(path = "manage/branding.html")]
struct Branding<'a> {
    p: PageMeta<'a>,
    assets: Vec<crate::media::BrandAsset>,
    uploads: Vec<crate::media::Upload>,
    key: String,
    message: String,
    selected_slot: String,
    selected_aid: i64,
}
async fn brand_view(state: &AppState, message: String, status: StatusCode) -> Response {
    brand_view_with_selection(state, message, status, None).await
}
async fn brand_view_with_selection(
    state: &AppState,
    message: String,
    status: StatusCode,
    selected: Option<(String, i64)>,
) -> Response {
    let Some(session) = &state.viewer.session else {
        return auth::redirect("/login?return_to=/manage/branding");
    };
    if let Err(e) = crate::administration::admin(&state.db, session.member.actor()).await {
        return auth::identity_error(state, e);
    }
    let assets = match state.media.brand_assets().await {
        Ok(v) => v,
        Err(e) => return super::media_failure(state, e),
    };
    let uploads = match state.media.uploads(session.member.uid).await {
        Ok(v) => v
            .into_iter()
            .filter(|u| {
                u.mime_type.starts_with("image/")
                    || u.lifecycle == "pending"
                    || u.lifecycle == "processing"
            })
            .collect(),
        Err(e) => return super::media_failure(state, e),
    };
    let (selected_slot, selected_aid) = selected.unwrap_or_default();
    let mut r = render(&Branding {
        selected_slot,
        selected_aid,
        p: meta(
            state,
            crate::i18n::current().manage_tab_branding().into(),
            crate::i18n::current()
                .media_manage_brand_description()
                .into(),
            "/manage/branding",
            "",
            "",
        ),
        assets,
        uploads,
        key: crate::identity::random_token(),
        message,
    });
    *r.status_mut() = status;
    r
}
async fn branding(Page(state): Page) -> Response {
    brand_view(&state, String::new(), StatusCode::OK).await
}
#[derive(Deserialize)]
struct BrandForm {
    _csrf: String,
    aid: i64,
    version: i64,
}
csrf_forms!(BrandForm);
async fn save_brand(
    Page(state): Page,
    Path(slot): Path<String>,
    CsrfForm(form): CsrfForm<BrandForm>,
) -> Response {
    if form.aid < 0 {
        return brand_view_with_selection(
            &state,
            crate::i18n::current().media_manage_brand_error_aid().into(),
            StatusCode::UNPROCESSABLE_ENTITY,
            Some((slot, form.aid)),
        )
        .await;
    }
    let Some(session) = &state.viewer.session else {
        return auth::redirect("/login");
    };
    match state
        .media
        .set_brand(
            &state.global,
            session.member.actor(),
            &slot,
            (form.aid > 0).then_some(form.aid),
            form.version,
        )
        .await
    {
        Ok(()) => auth::redirect("/manage/branding"),
        Err(e) => match super::media_form_failure(e) {
            Ok((status, message)) => {
                brand_view_with_selection(&state, message, status, Some((slot, form.aid))).await
            }
            Err(e) => super::media_failure(&state, e),
        },
    }
}
async fn preview_brand(
    State(state): State<Arc<AppState>>,
    Extension(viewer): Extension<auth::Viewer>,
    Path(aid): Path<i64>,
) -> Response {
    let Some(session) = &viewer.session else {
        return auth::redirect("/login");
    };
    if let Err(e) = crate::administration::admin(&state.db, session.member.actor()).await {
        return auth::identity_error(&state, e);
    }
    match state.media.export_owned(aid, session.member.uid).await {
        Ok(file) if file.is_image => {
            let mut r = Body::from_stream(tokio_util::io::ReaderStream::new(
                file.body.into_async_read(),
            ))
            .into_response();
            if let Ok(v) = HeaderValue::from_str(&file.mime_type) {
                r.headers_mut().insert(header::CONTENT_TYPE, v);
            }
            r
        }
        _ => error_page(
            &state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().site_error_no_image(),
        ),
    }
}
async fn upload_brand(
    AdminPage(state): AdminPage,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    // AdminPage guarantees the visitor is signed in and an administrator; Multipart takes the body, and axum
    // guarantees it runs after the extractors.
    let session = state
        .viewer
        .session
        .as_ref()
        .expect("AdminPage has verified the sign-in");
    // Same order as the avatar upload: the origin is checked before the body is read, the token must come
    // before the file, and the staging file is written only after it is verified.
    // This site's forms put `_csrf` before the file input, and browsers submit in document order.
    if let Some(reason) = auth::origin_failure(&headers) {
        return auth::csrf_error(&state, &reason);
    }
    let mut csrf_checked = false;
    let mut key = String::new();
    let mut upload = None;
    enum Refusal {
        /// Token problem, returns 403.
        Token(String),
        /// Form problem, returns 422.
        Form(String),
    }
    let received: Result<(), Refusal> = async {
        let t = crate::i18n::current();
        let invalid = |message: &str| Refusal::Form(message.to_owned());
        let mut fields = 0;
        while let Some(mut part) = multipart
            .next_field()
            .await
            .map_err(|_| invalid(t.media_manage_error_upload_form()))?
        {
            fields += 1;
            if fields > 3 {
                return Err(invalid(t.media_error_fields()));
            }
            let name = part.name().unwrap_or("").to_string();
            if name == "file" {
                if !csrf_checked {
                    return Err(Refusal::Token(t.settings_error_upload_token_order().into()));
                }
                if upload.is_some() {
                    return Err(invalid(t.media_manage_error_one_image()));
                }
                let filename = part.file_name().unwrap_or("").to_string();
                let (path, mut file) = state
                    .media
                    .staging_file()
                    .await
                    .map_err(|e| invalid(&e.to_string()))?;
                upload = Some((path, filename));
                let mut size = 0;
                while let Some(chunk) = part
                    .chunk()
                    .await
                    .map_err(|_| invalid(t.media_manage_error_receive()))?
                {
                    size += chunk.len() as u64;
                    if size > state.config.media.image_max_bytes {
                        return Err(invalid(t.media_manage_error_too_large()));
                    }
                    file.write_all(&chunk)
                        .await
                        .map_err(|_| invalid(t.media_manage_error_staging()))?;
                }
                file.sync_all()
                    .await
                    .map_err(|_| invalid(t.media_manage_error_staging()))?;
            } else {
                // Read chunk by chunk and stop as soon as it is too long, instead of collecting the whole field
                // into memory before measuring its length.
                let mut bytes = Vec::new();
                while let Some(chunk) = part
                    .chunk()
                    .await
                    .map_err(|_| invalid(t.media_manage_error_field()))?
                {
                    bytes.extend_from_slice(&chunk);
                    if bytes.len() > 128 {
                        return Err(invalid(t.media_manage_error_field_long()));
                    }
                }
                let value =
                    String::from_utf8(bytes).map_err(|_| invalid(t.media_manage_error_field()))?;
                match name.as_str() {
                    // Only the first token counts: a verified token cannot be overwritten by a later field
                    // with the same name.
                    "_csrf" if !csrf_checked => {
                        if let Some(reason) = auth::token_failure(&state, &value) {
                            return Err(Refusal::Token(reason));
                        }
                        csrf_checked = true;
                    }
                    "request_key" => key = value,
                    _ => return Err(invalid(t.media_manage_error_upload_field())),
                }
            }
        }
        if !csrf_checked {
            let reason = auth::token_failure(&state, "").unwrap_or_default();
            return Err(Refusal::Token(reason));
        }
        Ok(())
    }
    .await;
    if let Err(refusal) = received {
        if let Some((path, _)) = upload {
            let _ = tokio::fs::remove_file(path).await;
        }
        return match refusal {
            Refusal::Token(reason) => auth::csrf_error(&state, &reason),
            Refusal::Form(message) => {
                brand_view(&state, message, StatusCode::UNPROCESSABLE_ENTITY).await
            }
        };
    }
    let Some((path, filename)) = upload else {
        return brand_view(
            &state,
            crate::i18n::current().media_manage_error_choose().into(),
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    };
    match state
        .media
        .register_brand_upload(
            session.member.actor(),
            crate::media::NewUpload {
                request_key: key,
                filename,
                description: crate::i18n::current()
                    .media_manage_brand_upload_description()
                    .into(),
                alt_text: state.frame.settings.site.display_name().into(),
                path,
            },
        )
        .await
    {
        Ok(_) => auth::redirect("/manage/branding"),
        Err(e) => match super::media_form_failure(e) {
            Ok((status, message)) => brand_view(&state, message, status).await,
            Err(e) => super::media_failure(&state, e),
        },
    }
}

async fn forum_icon(
    State(state): State<Arc<AppState>>,
    Extension(viewer): Extension<auth::Viewer>,
    Path((fid, aid)): Path<(i64, i64)>,
    headers: axum::http::HeaderMap,
) -> Response {
    match state
        .media
        .forum_icon_image(
            fid,
            aid,
            viewer.session.as_ref().map(|s| s.member.actor()),
            super::if_none_match(&headers),
        )
        .await
    {
        Ok(image) => super::display_image_response(image),
        Err(_) => error_page(
            &state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().site_error_no_forum_icon(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn robots_rules_get_the_path_prefix() {
        let rules = "User-agent: *\nDisallow: /manage\nallow: /manage/x\nDisallow:\nDisallow: *.pdf\n# Disallow: /x";
        assert_eq!(prefixed_rules(rules, ""), rules);
        assert_eq!(
            prefixed_rules(rules, "/bbs"),
            "User-agent: *\nDisallow: /bbs/manage\nallow: /bbs/manage/x\nDisallow:\nDisallow: *.pdf\n# Disallow: /x"
        );
        let mut setting = crate::runtime::SiteSettings::blank();
        setting.seo.indexing = false;
        assert_eq!(
            robots_body(&setting, "", "/bbs"),
            "User-agent: *\nDisallow: /bbs/\n"
        );
        assert_eq!(
            robots_body(&setting, "", ""),
            "User-agent: *\nDisallow: /\n"
        );
        setting.seo.indexing = true;
        setting.seo.robots = "User-agent: *\nDisallow: /my".into();
        assert_eq!(
            robots_body(&setting, "https://demo.com/bbs", "/bbs"),
            "User-agent: *\nDisallow: /bbs/my\nSitemap: https://demo.com/bbs/sitemap.xml\n"
        );
    }
}
