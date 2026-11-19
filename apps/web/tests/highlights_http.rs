#[path = "support/http_auth.rs"]
mod http_auth;

use std::{collections::BTreeSet, sync::Arc};

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode, header},
};
use gono_forum_web::{
    config::Config,
    content,
    db::Database,
    debug::trace::{self, QuerySource, RequestProfile},
    identity, router,
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const ORIGIN: &str = "http://127.0.0.1:58833";
const ADMIN: usize = 0;
const MEMBER: usize = 1;
const UNVERIFIED: usize = 2;
const ROOT: i64 = 103002;
const EARLY: i64 = 103003;
const LATE_ROOT: i64 = 103052;
const CHILD: i64 = 103076;
/// "\u{957f}" is the Chinese for "long": one character but three bytes in UTF-8, so the page description below is
/// checked to be cut by characters, not bytes.
const LONG: &str = "\u{957f}";

struct Fixture {
    _services: gono_forum_web::test_support::TestFixture,
    db: Database,
    app: Router,
    sessions: Vec<String>,
    csrf_tokens: Vec<String>,
    first_pid: i64,
    fid: i64,
}

struct Response {
    status: StatusCode,
    headers: HeaderMap,
    html: String,
}

impl Fixture {
    async fn new() -> Self {
        let services = gono_forum_web::test_support::TestFixture::migrated()
            .await
            .unwrap();
        let root = services.root.clone();
        let db = services.db.clone();
        let config: Config = services.config.clone();
        content::seed_forums(&db).await.unwrap();
        content::seed_fixture(&db).await.unwrap();
        let identity = identity::Identity::new(
            db.clone(),
            config.identity.clone(),
            root.clone(),
            gono_forum_web::test_support::detached_global(),
        );
        let mut sessions = Vec::new();
        let mut csrf_tokens = Vec::new();
        for uid in 9001_i64..=9016 {
            gono_forum_web::db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(?,?,'synthetic-hash',?,?,?,1)")
                .bind(uid).bind(format!("highlight-http-{uid}")).bind(format!("highlight-{uid}@example.test"))
                .bind(i64::from(uid != 9003)).bind(i64::from(uid == 9001)).execute(db.executor()).await.unwrap();
            if uid <= 9004 {
                let grant = http_auth::login(&identity, &db, uid).await;
                csrf_tokens.push(grant.csrf_token);
                sessions.push(grant.session_token);
            }
        }
        let first_pid = gono_forum_web::db::query_scalar(
            "SELECT pid FROM gonof_forum_post WHERE tid=103 AND first=1",
        )
        .fetch_one(db.executor())
        .await
        .unwrap();
        let fid =
            gono_forum_web::db::query_scalar("SELECT fid FROM gonof_forum_thread WHERE tid=103")
                .fetch_one(db.executor())
                .await
                .unwrap();
        gono_forum_web::db::query(
            "UPDATE gonof_forum_post SET message='Public top reply body' WHERE tid=103 AND first=0",
        )
        .execute(db.executor())
        .await
        .unwrap();
        // Shares the process cache with the fixture: after changing the database directly, `execute` clears it (the
        // thread page's readability check looks at the forum cache).
        let app = router::build_with_global(
            config,
            &root,
            db.clone(),
            services.cache.clone(),
            services.global.clone(),
        );
        Self {
            _services: services,
            db,
            app,
            sessions,
            csrf_tokens,
            first_pid,
            fid,
        }
    }

    /// After changing the database with raw SQL, synchronizes thread copies and invalidates caches the way the admin,
    /// moderation and rename entry points do (the copies are maintained only by code).
    async fn execute(&self, sql: &str) {
        gono_forum_web::db::raw_sql(sql)
            .execute(self.db.executor())
            .await
            .unwrap();
        gono_forum_web::test_support::resync_copies(&self.db)
            .await
            .unwrap();
        self._services.global.clear_local();
    }

    async fn like(&self, pid: i64, count: i64) {
        for uid in 9005..9005 + count {
            gono_forum_web::db::query(
                "INSERT INTO gonof_forum_post_reaction(pid,uid,dateline) VALUES(?,?,1)",
            )
            .bind(pid)
            .bind(uid)
            .execute(self.db.executor())
            .await
            .unwrap();
        }
        // Like counts live on the post row; after raw SQL writes the details, recompute from them.
        gono_forum_web::test_support::refresh_listing(&self.db)
            .await
            .unwrap();
    }

    async fn count(&self, pid: i64) -> i64 {
        gono_forum_web::db::query_scalar(
            "SELECT COUNT(*) FROM gonof_forum_post_reaction WHERE pid=?",
        )
        .bind(pid)
        .fetch_one(self.db.executor())
        .await
        .unwrap()
    }

    async fn call(
        &self,
        method: &str,
        path: &str,
        user: Option<usize>,
        fields: &[(&str, String)],
        hx: bool,
        origin: &str,
    ) -> Response {
        let mut request = Request::builder().method(method).uri(path);
        if hx {
            request = request.header("HX-Request", "true");
        }
        if let Some(user) = user {
            request = request.header(
                header::COOKIE,
                format!("gonof_session__port58833={}", self.sessions[user]),
            );
        }
        if method == "POST" {
            request = request
                .header(header::ORIGIN, origin)
                .header(header::HOST, gono_forum_web::test_support::host_of(ORIGIN))
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
        }
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(fields.iter())
            .finish();
        let response = self
            .app
            .clone()
            .oneshot(request.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let html = String::from_utf8(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        Response {
            status,
            headers,
            html,
        }
    }

    async fn get(&self, path: &str, user: Option<usize>) -> Response {
        self.call("GET", path, user, &[], false, ORIGIN).await
    }

    fn fields(&self, user: usize, desired: bool, location: &str) -> Vec<(&'static str, String)> {
        vec![
            ("_csrf", self.csrf_tokens[user].clone()),
            ("request_key", identity::random_token()),
            ("desired", desired.to_string()),
            ("location", location.into()),
        ]
    }

    async fn vote(
        &self,
        pid: i64,
        user: usize,
        desired: bool,
        location: &str,
        hx: bool,
    ) -> Response {
        self.call(
            "POST",
            &format!("/api/comments/{pid}/vote"),
            Some(user),
            &self.fields(user, desired, location),
            hx,
            ORIGIN,
        )
        .await
    }
}

fn attribute(tag: &str, name: &str) -> Option<String> {
    let prefix = format!(" {name}=\"");
    let value = tag.split_once(&prefix)?.1.split('"').next()?;
    Some(quick_xml::escape::unescape(value).unwrap().into_owned())
}

fn tags(html: &str) -> impl Iterator<Item = &str> {
    html.split('<')
        .skip(1)
        .filter_map(|part| part.split_once('>').map(|(tag, _)| tag))
}

fn highlights(html: &str) -> &str {
    let marker = html
        .find("id=\"topic-highlights\"")
        .expect("both the full page and the like response keep the top replies container");
    let start = html[..marker].rfind("<section").unwrap();
    let end = html[marker..].find("</section>").unwrap() + marker + "</section>".len();
    &html[start..end]
}

fn ranking(html: &str) -> Vec<(i64, i64)> {
    tags(highlights(html))
        .filter_map(|tag| {
            Some((
                attribute(tag, "data-highlight-pid")?.parse().unwrap(),
                attribute(tag, "data-highlight-count")?.parse().unwrap(),
            ))
        })
        .collect()
}

fn permalinks(html: &str) -> Vec<String> {
    tags(highlights(html))
        .filter(|tag| {
            tag.starts_with("a ")
                && attribute(tag, "class").is_some_and(|classes| {
                    classes
                        .split_whitespace()
                        .any(|class| class == "highlight-permalink")
                })
        })
        .map(|tag| attribute(tag, "href").unwrap())
        .collect()
}

fn assert_unique_ids(html: &str) {
    let mut ids = BTreeSet::new();
    for id in tags(html).filter_map(|tag| attribute(tag, "id")) {
        assert!(ids.insert(id.clone()), "duplicate DOM id: {id}");
    }
}

fn original_floors(html: &str) -> Vec<usize> {
    tags(html)
        .filter(|tag| {
            attribute(tag, "class").is_some_and(|classes| {
                classes
                    .split_whitespace()
                    .any(|class| class == "floor-card")
            })
        })
        .map(|tag| {
            attribute(tag, "id")
                .unwrap()
                .strip_prefix("floor-")
                .unwrap()
                .parse()
                .unwrap()
        })
        .collect()
}

fn assert_page(response: &Response) {
    assert_eq!(response.status, StatusCode::OK);
    assert!(
        response.headers[header::CACHE_CONTROL]
            .to_str()
            .unwrap()
            .contains("no-store")
    );
    assert_unique_ids(&response.html);
    assert!(!highlights(&response.html).contains("hx-swap-oob"));
}

#[tokio::test]
async fn highlights_follow_each_displayed_page_without_reordering_floors() {
    let f = Fixture::new().await;
    f.like(f.first_pid, 8).await;
    f.like(CHILD, 4).await;
    f.like(LATE_ROOT, 3).await;
    f.like(EARLY, 3).await;
    f.like(ROOT, 2).await;
    for (path, flat, floors, expected) in [
        (
            "/thread-103",
            false,
            (2..=51).collect::<Vec<_>>(),
            vec![(EARLY, 3), (ROOT, 2)],
        ),
        (
            "/thread-103-2",
            false,
            (52..=55).collect(),
            vec![(LATE_ROOT, 3)],
        ),
        (
            "/thread-103?view=flat",
            true,
            (2..=51).collect(),
            vec![(EARLY, 3), (ROOT, 2)],
        ),
        (
            "/thread-103-2?view=flat",
            true,
            (52..=79).collect(),
            vec![(CHILD, 4), (LATE_ROOT, 3)],
        ),
        (
            "/thread-103?sub_root=103002&sub_page=2",
            false,
            (2..=51).collect(),
            vec![(CHILD, 4), (EARLY, 3), (ROOT, 2)],
        ),
    ] {
        let profile = Arc::new(RequestProfile::new());
        let page = trace::scope(profile.clone(), f.get(path, None)).await;
        let reaction_queries = profile
            .snapshot()
            .into_iter()
            .filter(|query| {
                // Like counts are read from the count column on the post row; anonymous visitors read
                // only the post table.
                query.source == QuerySource::Application
                    && query
                        .sql
                        .starts_with("SELECT pid,reactions FROM gonof_forum_post WHERE pid IN")
            })
            .count();
        assert_eq!(
            reaction_queries, 1,
            "top replies and like buttons share one read: {path}"
        );
        assert_page(&page);
        assert_eq!(
            ranking(&page.html),
            expected,
            "top replies come only from the page shown: {path}"
        );
        assert_eq!(
            original_floors(&page.html),
            floors,
            "the reply list pages by stable floor numbers: {path}"
        );
        let links: Vec<_> = expected
            .iter()
            .map(|(pid, _)| match (*pid, flat) {
                (CHILD, true) => "/thread-103-2?view=flat#floor-76",
                (CHILD, false) => "/thread-103?sub_root=103002&sub_page=2#floor-76",
                (LATE_ROOT, true) => "/thread-103-2?view=flat#floor-52",
                (LATE_ROOT, false) => "/thread-103-2#floor-52",
                (EARLY, true) => "/thread-103?view=flat#floor-3",
                (EARLY, false) => "/thread-103#floor-3",
                (ROOT, true) => "/thread-103?view=flat#floor-2",
                (ROOT, false) => "/thread-103#floor-2",
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(permalinks(&page.html), links);
        for (pid, count) in &expected {
            let marker = format!("id=\"reaction-{pid}\"");
            let control = page.html.split_once(&marker).unwrap().1;
            let rendered_count = control
                .split_once("data-reaction-count>")
                .unwrap()
                .1
                .split_once("</span>")
                .unwrap()
                .0;
            assert_eq!(
                rendered_count,
                count.to_string(),
                "the top reply count matches the button: {pid}"
            );
        }
        assert!(!highlights(&page.html).contains("id=\"floor-"));
        assert!(!highlights(&page.html).contains("id=\"reaction-"));
    }
    f._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn highlight_visibility_follows_withdrawals_ancestors_and_public_permissions() {
    let f = Fixture::new().await;
    for (pid, count) in [(ROOT, 5), (CHILD, 4), (EARLY, 2)] {
        f.like(pid, count).await;
    }
    for (mutation, expected) in [
        (
            format!("UPDATE gonof_forum_post SET deleted_at=1 WHERE pid={ROOT}"),
            vec![(CHILD, 4), (EARLY, 2)],
        ),
        (
            format!("UPDATE gonof_forum_post SET invisible=-1 WHERE pid={ROOT}"),
            vec![(EARLY, 2)],
        ),
        (
            format!("UPDATE gonof_forum_post SET invisible=0 WHERE pid={ROOT}"),
            vec![(CHILD, 4), (EARLY, 2)],
        ),
        (
            format!("UPDATE gonof_forum_post SET deleted_at=1 WHERE pid={CHILD}"),
            vec![(EARLY, 2)],
        ),
        (
            format!("UPDATE gonof_forum_post SET deleted_at=0 WHERE pid={ROOT}"),
            vec![(ROOT, 5), (EARLY, 2)],
        ),
        (
            format!("UPDATE gonof_forum_post SET deleted_at=0 WHERE pid={CHILD}"),
            vec![(ROOT, 5), (CHILD, 4), (EARLY, 2)],
        ),
    ] {
        f.execute(&mutation).await;
        for user in [None, Some(ADMIN)] {
            let page = f.get("/thread-103?sub_root=103002&sub_page=2", user).await;
            assert_page(&page);
            assert_eq!(
                ranking(&page.html),
                expected,
                "the public ranking is not widened for staff: {mutation}"
            );
        }
    }
    f.execute("UPDATE gonof_forum_thread SET deleted_at=1 WHERE tid=103")
        .await;
    f.execute(&format!(
        "UPDATE gonof_forum_post SET deleted_at=1 WHERE pid={}",
        f.first_pid
    ))
    .await;
    assert_eq!(
        ranking(
            &f.get("/thread-103?sub_root=103002&sub_page=2", None)
                .await
                .html
        ),
        [(ROOT, 5), (CHILD, 4), (EARLY, 2)],
        "public replies stay after the author withdraws the first post"
    );
    for mutation in [
        "UPDATE gonof_forum_thread SET readperm=1 WHERE tid=103".to_owned(),
        "UPDATE gonof_forum_thread SET visibility='members' WHERE tid=103".into(),
        "UPDATE gonof_forum_thread SET publication_state='draft' WHERE tid=103".into(),
        "UPDATE gonof_forum_thread SET displayorder=-3 WHERE tid=103".into(),
        format!(
            "UPDATE gonof_forum_forum SET visibility='private' WHERE fid={}",
            f.fid
        ),
        format!(
            "UPDATE gonof_forum_post SET invisible=-1 WHERE pid={}",
            f.first_pid
        ),
    ] {
        f.execute(&mutation).await;
        for user in [None, Some(ADMIN)] {
            let page = f.get("/thread-103?sub_root=103002&sub_page=2", user).await;
            if user.is_some() && mutation.contains("visibility='members'") {
                assert_eq!(
                    page.status,
                    StatusCode::OK,
                    "a valid member may read a members-only thread"
                );
                assert_eq!(ranking(&page.html), [(ROOT, 5), (CHILD, 4), (EARLY, 2)]);
            } else {
                assert_eq!(
                    page.status,
                    StatusCode::NOT_FOUND,
                    "tightened access must block top replies the reader cannot read: {mutation}"
                );
                assert!(!page.html.contains("data-highlight-pid"));
            }
        }
        f.execute("UPDATE gonof_forum_thread SET readperm=0,visibility='public',publication_state='published',displayorder=0 WHERE tid=103").await;
        f.execute(&format!(
            "UPDATE gonof_forum_forum SET visibility='public' WHERE fid={}",
            f.fid
        ))
        .await;
        f.execute(&format!(
            "UPDATE gonof_forum_post SET invisible=0 WHERE pid={}",
            f.first_pid
        ))
        .await;
    }
    f._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn highlight_excerpts_use_current_rendering_without_source_labels_or_markup() {
    let f = Fixture::new().await;
    let body = "[@HIGHLIGHT_OLD_MEMBER_SECRET](member:9004 \"HIGHLIGHT_MEMBER_TITLE_SECRET\") [HIGHLIGHT_OLD_QUOTE_SECRET](post:103052 \"HIGHLIGHT_QUOTE_TITLE_SECRET\") [plain link](https://example.test/HIGHLIGHT_URL_SECRET) **emphasized text** <script>HIGHLIGHT_SCRIPT_SECRET</script>\n\n<spoiler>\nHIGHLIGHT_SPOILER_SECRET\n</spoiler>";
    gono_forum_web::db::query("UPDATE gonof_forum_post SET message=? WHERE pid=?")
        .bind(body)
        .bind(EARLY)
        .execute(f.db.executor())
        .await
        .unwrap();
    gono_forum_web::test_support::resync_copies(&f.db)
        .await
        .unwrap();
    f.like(EARLY, 1).await;
    for user in [None, Some(MEMBER), Some(ADMIN)] {
        let page = f.get("/thread-103", user).await;
        assert_page(&page);
        let excerpt = highlights(&page.html);
        assert!(excerpt.contains("highlight-excerpt"));
        assert!(excerpt.contains("highlight-http-9004"));
        for secret in [
            "HIGHLIGHT_OLD_MEMBER_SECRET",
            "HIGHLIGHT_MEMBER_TITLE_SECRET",
            "HIGHLIGHT_OLD_QUOTE_SECRET",
            "HIGHLIGHT_QUOTE_TITLE_SECRET",
            "HIGHLIGHT_URL_SECRET",
            "HIGHLIGHT_SCRIPT_SECRET",
            "HIGHLIGHT_SPOILER_SECRET",
            "member:9004",
            "post:103052",
            "<script",
            "<strong>",
            "<img",
        ] {
            // Look only at the summary paragraph: avatar slots serve images by uid and always carry an
            // `<img src="/avatar/…">`, which is this site's avatar, not markup leaked from the body.
            let summaries: String = excerpt
                .split("<p class=\"highlight-excerpt\">")
                .skip(1)
                .filter_map(|rest| rest.split("</p>").next())
                .collect();
            assert!(
                !summaries.is_empty() && !summaries.contains(secret),
                "the excerpt must not output raw reference labels, attributes or activity markup: {secret}"
            );
        }
    }
    f.execute("UPDATE gonof_common_member SET username='highlight-renamed' WHERE uid=9004")
        .await;
    let excerpt_page = f.get("/thread-103", None).await;
    assert!(highlights(&excerpt_page.html).contains("highlight-renamed"));
    assert!(!highlights(&excerpt_page.html).contains("highlight-http-9004"));

    f.execute("UPDATE gonof_forum_post SET message='<spoiler>\nHIGHLIGHT_ONLY_SPOILER_SECRET\n</spoiler>' WHERE pid=103003")
        .await;
    let spoiler_page = f.get("/thread-103", None).await;
    assert_page(&spoiler_page);
    let excerpt = highlights(&spoiler_page.html);
    assert!(excerpt.contains("View the reply"));
    assert!(!excerpt.contains("HIGHLIGHT_ONLY_SPOILER_SECRET"));
    assert_eq!(permalinks(&spoiler_page.html), ["/thread-103#floor-3"]);

    // The page description takes the first 160 characters of the first post's plain text, without Markdown syntax or
    // spoilers, not the whole body.
    // The body is assembled in code and then written (the same statement for both backends).
    gono_forum_web::db::query("UPDATE gonof_forum_post SET message=? WHERE tid=103 AND first=1")
        .bind(format!(
            "**META_OPENING** <spoiler>\nMETA_SPOILER_SECRET\n</spoiler>\n\n{}",
            LONG.repeat(5000)
        ))
        .execute(f.db.executor())
        .await
        .unwrap();
    gono_forum_web::test_support::resync_copies(&f.db)
        .await
        .unwrap();
    f.execute("UPDATE gonof_forum_thread SET cache_revision=cache_revision+1 WHERE tid=103")
        .await;
    let described = f.get("/thread-103", None).await;
    assert_page(&described);
    let description = described
        .html
        .split_once(r#"<meta name="description" content=""#)
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(value, _)| value.to_owned())
        .expect("meta description");
    assert!(
        description.starts_with(&format!("META_OPENING {LONG}{LONG}")),
        "{description}"
    );
    assert_eq!(description.chars().count(), 160);
    assert!(!description.contains("META_SPOILER_SECRET"));
    // Spoilers in the body are output as usual; they just do not go into head.
    assert!(described.html.contains("META_SPOILER_SECRET"));
    f._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn native_votes_redirect_to_real_floors_and_htmx_updates_clear_page_highlights() {
    let f = Fixture::new().await;
    let empty = f.get("/thread-103-2", None).await;
    assert_page(&empty);
    assert!(ranking(&empty.html).is_empty());
    assert!(
        tags(highlights(&empty.html))
            .next()
            .unwrap()
            .contains(" hidden"),
        "an empty ranking container must not leave an empty accessible region"
    );
    let native = f.vote(LATE_ROOT, MEMBER, true, "/thread-103", false).await;
    assert_eq!(native.status, StatusCode::SEE_OTHER);
    assert_eq!(native.headers[header::LOCATION], "/thread-103-2#floor-52");
    assert_eq!(
        ranking(&f.get("/thread-103-2", None).await.html),
        [(LATE_ROOT, 1)]
    );
    let native = f
        .vote(LATE_ROOT, MEMBER, false, "/thread-103-2", false)
        .await;
    assert_eq!(native.status, StatusCode::SEE_OTHER);
    assert!(ranking(&f.get("/thread-103", None).await.html).is_empty());

    let fields = f.fields(MEMBER, true, "/thread-103-2?view=flat#floor-76");
    let endpoint = format!("/api/comments/{CHILD}/vote");
    let voted = f
        .call("POST", &endpoint, Some(MEMBER), &fields, true, ORIGIN)
        .await;
    assert_eq!(voted.status, StatusCode::OK);
    assert_unique_ids(&voted.html);
    assert!(highlights(&voted.html).contains("hx-swap-oob=\"outerHTML\""));
    assert!(
        !tags(highlights(&voted.html))
            .next()
            .unwrap()
            .contains(" hidden")
    );
    assert_eq!(ranking(&voted.html), [(CHILD, 1)]);
    assert_eq!(
        permalinks(&voted.html),
        ["/thread-103-2?view=flat#floor-76"]
    );
    assert!(voted.html.contains(&format!("id=\"reaction-{CHILD}\"")));
    assert!(!voted.html.contains("<!doctype") && !voted.html.contains("<textarea"));

    for (user, origin) in [
        (Some(MEMBER), "https://foreign.example"),
        (Some(UNVERIFIED), ORIGIN),
        (None, ORIGIN),
    ] {
        let invalid_fields = f.fields(user.unwrap_or(MEMBER), false, "/thread-103");
        let denied = f
            .call("POST", &endpoint, user, &invalid_fields, true, origin)
            .await;
        assert!(denied.status.is_client_error());
        assert_eq!(f.count(CHILD).await, 1);
    }
    let canceled = f
        .vote(
            CHILD,
            MEMBER,
            false,
            "/thread-103-2?view=flat#floor-76",
            true,
        )
        .await;
    assert_eq!(canceled.status, StatusCode::OK);
    assert_unique_ids(&canceled.html);
    assert!(highlights(&canceled.html).contains("hx-swap-oob=\"outerHTML\""));
    assert!(
        ranking(&canceled.html).is_empty(),
        "removing the last like must explicitly swap in the empty container"
    );
    assert!(permalinks(&canceled.html).is_empty());
    assert!(
        tags(highlights(&canceled.html))
            .next()
            .unwrap()
            .contains(" hidden")
    );
    let replay = f
        .call("POST", &endpoint, Some(MEMBER), &fields, true, ORIGIN)
        .await;
    assert_eq!(replay.status, StatusCode::OK);
    assert!(
        ranking(&replay.html).is_empty(),
        "replaying an old like request must not bring back a removed like"
    );
    assert_eq!(f.count(CHILD).await, 0);
    assert!(
        ranking(
            &f.get("/thread-103?sub_root=103002&sub_page=2", None)
                .await
                .html
        )
        .is_empty()
    );
    f._services.cleanup_ref().await.unwrap();
}

/// A highlighted reply's summary is a bounded excerpt cut on the server (two lines are shown): the long reply is
/// sent in full only in its floor, and a like's response does not carry it at all.
#[tokio::test]
async fn highlight_summaries_send_a_bounded_excerpt_of_long_replies() {
    let f = Fixture::new().await;
    gono_forum_web::db::query("UPDATE gonof_forum_post SET message=? WHERE pid=?")
        .bind(format!(
            "HIGHLIGHT_LONG_START **{}** HIGHLIGHT_LONG_END",
            LONG.repeat(18_000)
        ))
        .bind(EARLY)
        .execute(f.db.executor())
        .await
        .unwrap();
    gono_forum_web::test_support::resync_copies(&f.db)
        .await
        .unwrap();
    f.like(EARLY, 1).await;
    let summary = |html: &str| -> String {
        highlights(html)
            .split_once("<p class=\"highlight-excerpt\">")
            .and_then(|(_, rest)| rest.split_once("</p>"))
            .map(|(summary, _)| summary.to_owned())
            .expect("a highlight summary")
    };
    for user in [None, Some(MEMBER)] {
        let page = f.get("/thread-103", user).await;
        assert_page(&page);
        let text = summary(&page.html);
        assert!(
            text.starts_with(&format!("HIGHLIGHT_LONG_START {LONG}")),
            "{text}"
        );
        assert!(text.chars().count() <= 300, "{}", text.chars().count());
        assert_eq!(page.html.matches("HIGHLIGHT_LONG_END").count(), 1);
    }
    let fields = f.fields(MEMBER, true, "/thread-103");
    let voted = f
        .call(
            "POST",
            &format!("/api/comments/{EARLY}/vote"),
            Some(MEMBER),
            &fields,
            true,
            ORIGIN,
        )
        .await;
    assert_eq!(voted.status, StatusCode::OK);
    assert!(summary(&voted.html).chars().count() <= 300);
    assert!(!voted.html.contains("HIGHLIGHT_LONG_END"));
    f._services.cleanup_ref().await.unwrap();
}
