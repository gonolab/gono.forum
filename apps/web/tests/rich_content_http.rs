//! Restricted rich media and in-site thread cards.
//!
//! YouTube links become only a local placeholder that loads on click; the page loads no external resources, and
//! the CSP only additionally allows frames from youtube-nocookie.com. In-site thread cards are generated according to
//! the reader's permissions, adding at most one query per page.
#[path = "support/http_auth.rs"]
mod http_auth;

use std::{net::SocketAddr, sync::Arc};

use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{HeaderMap, Request, StatusCode, header},
};
use gono_forum_web::{
    content::{
        self,
        writes::{ContentWriter, NewReply, NewThread, ThreadVisibility, WriteConfig},
    },
    db::{self, Database},
    debug::trace::{self, QuerySource, RequestProfile},
    identity::{self, Identity, SessionGrant},
    router,
    test_support::TestFixture,
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const ORIGIN: &str = "http://127.0.0.1:58833";
const READER: i64 = 9921;
const READER2: i64 = 9922;
const AUTHOR: i64 = 9923;
const TARGET: i64 = 9924;
const BLOCKED: i64 = 9925;

struct Fixture {
    services: TestFixture,
    app: Router,
    sessions: std::collections::BTreeMap<i64, SessionGrant>,
    fid: i64,
    now: i64,
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

fn config() -> WriteConfig {
    WriteConfig::default()
}

impl Fixture {
    async fn new() -> Self {
        let services = TestFixture::migrated().await.unwrap();
        let db = services.db.clone();
        content::seed_forums(&db).await.unwrap();
        gono_forum_web::test_support::raise_posting_quotas(&db)
            .await
            .unwrap();
        content::seed_fixture(&db).await.unwrap();
        for uid in [READER, READER2, AUTHOR, TARGET, BLOCKED] {
            db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(?,?,'synthetic-hash',?,1,0,1)")
                .bind(uid).bind(format!("rich-{uid}")).bind(format!("rich-{uid}@example.test"))
                .execute(db.executor()).await.unwrap();
        }
        let fid: i64 = db::query_scalar("SELECT fid FROM gonof_forum_forum WHERE visibility='public' AND status=1 AND parent_fid IS NULL ORDER BY fid LIMIT 1")
            .fetch_one(db.executor())
            .await
            .unwrap();
        let config = services.config.clone();
        let identity = Identity::new(
            db.clone(),
            config.identity.clone(),
            services.root.clone(),
            gono_forum_web::test_support::detached_global(),
        );
        let mut sessions = std::collections::BTreeMap::new();
        for uid in [READER, READER2, AUTHOR] {
            sessions.insert(uid, http_auth::login(&identity, &db, uid).await);
        }
        let app = router::build_with_global(
            config,
            &services.root,
            db.clone(),
            services.cache.clone(),
            services.global.clone(),
        );
        Self {
            services,
            app,
            sessions,
            fid,
            now: identity::now(),
        }
    }

    fn db(&self) -> &Database {
        &self.services.db
    }

    async fn scalar(&self, sql: &str) -> i64 {
        db::query_scalar(sql)
            .fetch_one(self.db().executor())
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
    }

    /// After changing the database with raw SQL, sync the thread copies and invalidate caches the way the edit,
    /// moderation and admin entry points do (the copies are maintained only by code).
    async fn execute(&self, sql: &str) {
        db::query(sql)
            .execute(self.db().executor())
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
        gono_forum_web::test_support::resync_copies(self.db())
            .await
            .unwrap();
    }

    async fn publish(
        &self,
        uid: i64,
        fid: i64,
        title: &str,
        body: &str,
        visibility: ThreadVisibility,
    ) -> i64 {
        ContentWriter::new(self.db().clone(), config())
            .publish_at(
                uid,
                NewThread {
                    visibility,
                    activity: Default::default(),
                    fid,
                    title: title.into(),
                    body: body.into(),
                    attachment_ids: vec![],
                    request_key: identity::random_token(),
                },
                self.now - 3_600,
            )
            .await
            .unwrap()
            .tid
    }

    async fn reply(&self, uid: i64, tid: i64, body: &str) {
        ContentWriter::new(self.db().clone(), config())
            .reply_at(
                uid,
                NewReply {
                    tid,
                    root_pid: None,
                    reply_to_pid: None,
                    body: body.into(),
                    attachment_ids: vec![],
                    request_key: identity::random_token(),
                },
                self.now - 1_800,
            )
            .await
            .unwrap();
    }

    async fn send(
        &self,
        mut request: Request<Body>,
        profile: Option<Arc<RequestProfile>>,
    ) -> Reply {
        request.extensions_mut().insert(ConnectInfo(
            "192.0.2.47:48322".parse::<SocketAddr>().unwrap(),
        ));
        let call = self.app.clone().oneshot(request);
        let response = match profile {
            Some(profile) => trace::scope(profile, call).await,
            None => call.await,
        }
        .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        Reply {
            status,
            headers,
            body: String::from_utf8_lossy(&body).into_owned(),
        }
    }

    fn request(&self, path: &str, uid: Option<i64>) -> Request<Body> {
        let mut builder = Request::builder().uri(path);
        if let Some(uid) = uid {
            builder = builder.header(
                header::COOKIE,
                format!(
                    "gonof_session__port58833={}",
                    self.sessions[&uid].session_token
                ),
            );
        }
        builder.body(Body::empty()).unwrap()
    }

    async fn get(&self, path: &str, uid: Option<i64>) -> Reply {
        let reply = self.send(self.request(path, uid), None).await;
        assert_eq!(reply.status, StatusCode::OK, "{path}: {}", reply.body);
        reply
    }
}

#[tokio::test]
async fn youtube_placeholders_load_nothing_until_clicked_and_csp_only_adds_the_player() {
    let f = Fixture::new().await;
    let tid = f
        .publish(
            AUTHOR,
            f.fid,
            "RICH-VIDEO",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=1m30s\n\n[My video](https://youtu.be/abcdefghijk)\n\nMid-sentence https://youtu.be/zzzzzzzzzzz is not expanded\n\n[Malicious](https://www.youtube.com.evil.example/watch?v=qqqqqqqqqqq)\n\n<iframe src=\"https://www.youtube-nocookie.com/embed/iiiiiiiiiii\"></iframe>\n\n<figure class=\"video-embed\" data-youtube=\"fffffffffff\"><button data-media-load>Fake button</button></figure>\n\n[Script](javascript:alert(1))",
            ThreadVisibility::Public,
        )
        .await;
    for uid in [None, Some(READER)] {
        let page = f.get(&format!("/thread-{tid}"), uid).await;
        let html = &page.body;
        assert_eq!(
            html.matches("<figure class=\"video-embed\"").count(),
            2,
            "{html}"
        );
        assert!(html.contains("data-youtube=\"dQw4w9WgXcQ\" data-start=\"90\""));
        assert!(html.contains("data-youtube=\"abcdefghijk\""));
        assert!(html.contains(">My video</figcaption>"));
        assert_eq!(html.matches("data-media-load").count(), 2);
        assert!(html.contains("YouTube (Google) receives your IP address and this site"));
        for forbidden in [
            "<iframe",
            "fffffffffff\"",
            "iiiiiiiiiii",
            "data-youtube=\"zzzzzzzzzzz\"",
            "data-youtube=\"qqqqqqqqqqq\"",
            "ytimg",
            "javascript:",
        ] {
            assert!(!html.contains(forbidden), "{forbidden}");
        }
        assert!(html.contains("href=\"https://www.youtube.com.evil.example/watch?v=qqqqqqqqqqq\""));
        // The page description does not include the placeholder's hint text.
        let description = html
            .split("<meta name=\"description\" content=\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .unwrap_or_default();
        assert!(!description.contains("YouTube (Google)"), "{description}");
        // The CSP only additionally allows frames from youtube-nocookie.com; scripts, images and styles are still
        // limited to this site.
        let csp = page.headers[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap();
        let directives: Vec<_> = csp.split(';').map(str::trim).collect();
        assert!(directives.contains(&"frame-src 'self' https://www.youtube-nocookie.com"));
        for directive in [
            "default-src 'self'",
            "img-src 'self' data:",
            "script-src 'self'",
            "connect-src 'self'",
            "object-src 'none'",
            "frame-ancestors 'none'",
        ] {
            assert!(directives.contains(&directive), "{csp}");
        }
        assert_eq!(
            csp.split_whitespace()
                .filter(|token| token.contains("://"))
                .collect::<Vec<_>>(),
            ["https://www.youtube-nocookie.com;"]
        );
    }
    // The editor preview likewise only gives the placeholder.
    let session = &f.sessions[&AUTHOR];
    let mut form = url::form_urlencoded::Serializer::new(String::new());
    for (name, value) in [
        ("_csrf", session.csrf_token.as_str()),
        ("body", "https://youtu.be/dQw4w9WgXcQ"),
        ("fid", &f.fid.to_string()),
        ("request_key", &identity::random_token()),
    ] {
        form.append_pair(name, value);
    }
    let preview = f
        .send(
            Request::builder()
                .method("POST")
                .uri("/api/preview")
                .header(
                    header::COOKIE,
                    format!("gonof_session__port58833={}", session.session_token),
                )
                .header("HX-Request", "true")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::ORIGIN, ORIGIN)
                .header(header::HOST, gono_forum_web::test_support::host_of(ORIGIN))
                .body(Body::from(form.finish()))
                .unwrap(),
            None,
        )
        .await;
    assert_eq!(preview.status, StatusCode::OK, "{}", preview.body);
    assert!(preview.body.contains("data-youtube=\"dQw4w9WgXcQ\""));
    assert!(!preview.body.contains("<iframe"));
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn anonymous_topic_cards_follow_the_request_site_and_clock_on_warm_redis() {
    use gono_forum_web::{clock, test_support};

    let f = Fixture::new().await;
    test_support::set_site_url(f.db(), "").await.unwrap();
    let target = f
        .publish(
            TARGET,
            f.fid,
            "Quoted thread",
            "Summary",
            ThreadVisibility::Public,
        )
        .await;
    let url = format!("http://a.review.test/thread-{target}");
    let source = f
        .publish(
            AUTHOR,
            f.fid,
            "Quoting thread",
            &format!("[Link]({url})"),
            ThreadVisibility::Public,
        )
        .await;
    let path = format!("/thread-{source}");
    let load = |host: &'static str| {
        let mut request = f.request(&path, None);
        request
            .headers_mut()
            .insert(header::HOST, host.parse().unwrap());
        f.send(request, None)
    };
    let card = |body: &str, time: &str| {
        body.split_once("class=\"topic-embed__meta\"")
            .and_then(|(_, rest)| rest.split_once("</p>"))
            .is_some_and(|(meta, _)| meta.contains(time))
    };
    let dateline = f.now - 3_600;
    let original = clock::TimeFormat::default();
    let a = load("a.review.test").await;
    assert_eq!(a.status, StatusCode::OK);
    assert!(card(&a.body, &clock::datetime(dateline, &original)));
    let b = load("b.review.test").await;
    assert_eq!(b.status, StatusCode::OK);
    assert!(!b.body.contains("class=\"topic-embed__meta\"") && b.body.contains(&url));

    test_support::set_setting(f.db(), "settings.time.date_format", "ymd-cn")
        .await
        .unwrap();
    f.services.global.clear_local();
    let changed = clock::TimeFormat {
        date: clock::DateStyle::YmdChinese,
        ..original
    };
    let a = load("a.review.test").await;
    assert_eq!(a.status, StatusCode::OK);
    assert!(card(&a.body, &clock::datetime(dateline, &changed)));
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn topic_cards_follow_reader_visibility_and_cost_one_bounded_query() {
    let f = Fixture::new().await;
    let other_fid = f
        .scalar(&format!(
            "SELECT fid FROM gonof_forum_forum WHERE status=1 AND parent_fid IS NULL AND fid<>{} ORDER BY fid LIMIT 1",
            f.fid
        ))
        .await;
    let public = f
        .publish(
            TARGET,
            f.fid,
            "CARDPUBLIC",
            "Public summary text\n\n<spoiler>\n\nSpoiler secret\n\n</spoiler>\n\nThe end",
            ThreadVisibility::Public,
        )
        .await;
    let members = f
        .publish(
            TARGET,
            f.fid,
            "CARDMEMBERS",
            "Members-only body",
            ThreadVisibility::Members,
        )
        .await;
    let private = f
        .publish(
            TARGET,
            other_fid,
            "CARDPRIVATE",
            "Private forum body",
            ThreadVisibility::Public,
        )
        .await;
    f.execute(&format!(
        "UPDATE gonof_forum_forum SET visibility='private' WHERE fid={other_fid}"
    ))
    .await;
    let withdrawn = f
        .publish(
            TARGET,
            f.fid,
            "CARDWITHDRAWN",
            "Withdrawn body",
            ThreadVisibility::Public,
        )
        .await;
    f.execute(&format!(
        "UPDATE gonof_forum_post SET deleted_at={0} WHERE first_tid={withdrawn}",
        f.now - 60
    ))
    .await;
    f.execute(&format!(
        "UPDATE gonof_forum_thread SET deleted_at={0} WHERE tid={withdrawn}",
        f.now - 60
    ))
    .await;
    let hidden = f
        .publish(
            TARGET,
            f.fid,
            "CARDHIDDEN",
            "Hidden body",
            ThreadVisibility::Public,
        )
        .await;
    f.execute(&format!(
        "UPDATE gonof_forum_post SET invisible=-1 WHERE first_tid={hidden}"
    ))
    .await;
    let blocked = f
        .publish(
            BLOCKED,
            f.fid,
            "CARDBLOCKED",
            "Body by a blocked author",
            ThreadVisibility::Public,
        )
        .await;
    f.execute(&format!(
        "INSERT INTO gonof_home_member_block(uid,blocked_uid,dateline) VALUES({READER},{BLOCKED},1)"
    ))
    .await;
    let source = f
        .publish(
            AUTHOR,
            f.fid,
            "CARDSOURCE",
            &format!(
                "[Link public](/thread-{public})\n\n[Link members]({ORIGIN}/thread-{members})\n\n[Link private](/thread-{private})"
            ),
            ThreadVisibility::Public,
        )
        .await;
    f.reply(
        AUTHOR,
        source,
        &format!(
            "[Link withdrawn](/thread-{withdrawn})\n\n[Link hidden](/thread-{hidden})\n\n[Link blocked](/thread-{blocked})"
        ),
    )
    .await;
    let card = |tid: i64, subject: &str| {
        format!("<a class=\"topic-embed__title\" href=\"/thread-{tid}\">{subject}</a>")
    };
    let plain = |tid: i64, prefix: &str, label: &str| {
        format!("<a href=\"{prefix}/thread-{tid}\" rel=\"nofollow ugc noreferrer\">{label}</a>")
    };

    // Anonymous: only public threads by authors who are not blocked get cards; the rest are plain links that do not
    // reveal the title.
    let anonymous = f.get(&format!("/thread-{source}"), None).await.body;
    assert!(
        anonymous.contains(&card(public, "CARDPUBLIC")),
        "{anonymous}"
    );
    assert!(anonymous.contains("Public summary text"));
    assert!(!anonymous.contains("Spoiler secret"));
    assert!(anonymous.contains(&card(blocked, "CARDBLOCKED")));
    for (tid, prefix, label, subject) in [
        (members, ORIGIN, "Link members", "CARDMEMBERS"),
        (private, "", "Link private", "CARDPRIVATE"),
        (withdrawn, "", "Link withdrawn", "CARDWITHDRAWN"),
        (hidden, "", "Link hidden", "CARDHIDDEN"),
    ] {
        assert!(anonymous.contains(&plain(tid, prefix, label)), "{label}");
        assert!(!anonymous.contains(subject), "{subject}");
    }
    // Logged-in member: members-only threads are added; a reader who blocked the author does not see that card,
    // other members do.
    let reader = f.get(&format!("/thread-{source}"), Some(READER)).await.body;
    assert!(reader.contains(&card(public, "CARDPUBLIC")));
    assert!(reader.contains(&card(members, "CARDMEMBERS")));
    assert!(reader.contains(&plain(blocked, "", "Link blocked")));
    assert!(!reader.contains("CARDBLOCKED"));
    for subject in ["CARDPRIVATE", "CARDWITHDRAWN", "CARDHIDDEN"] {
        assert!(!reader.contains(subject), "{subject}");
    }
    let reader2 = f
        .get(&format!("/thread-{source}"), Some(READER2))
        .await
        .body;
    assert!(reader2.contains(&card(blocked, "CARDBLOCKED")));

    // The anonymous view goes through the render cache: renaming the linked thread or making it members-only takes
    // effect immediately.
    f.execute(&format!(
        "UPDATE gonof_forum_thread SET subject='CARDRENAMED' WHERE tid={public}"
    ))
    .await;
    let renamed = f.get(&format!("/thread-{source}"), None).await.body;
    assert!(renamed.contains(&card(public, "CARDRENAMED")), "{renamed}");
    f.execute(&format!(
        "UPDATE gonof_forum_thread SET visibility='members' WHERE tid={public}"
    ))
    .await;
    let restricted = f.get(&format!("/thread-{source}"), None).await.body;
    assert!(!restricted.contains("CARDRENAMED"));
    assert!(restricted.contains(&plain(public, "", "Link public")));

    // Bounded: 24 candidate threads with 4 links per post; the page expands at most 20, at most 3 per post,
    // and the whole page adds only one card query; a page with no candidates does not query.
    let mut targets = Vec::new();
    for n in 0..24 {
        targets.push(
            f.publish(
                TARGET,
                f.fid,
                &format!("BOUND{n:02}"),
                "Body",
                ThreadVisibility::Public,
            )
            .await,
        );
    }
    let busy = f
        .publish(
            AUTHOR,
            f.fid,
            "CARDBUSY",
            "The first post has no links",
            ThreadVisibility::Public,
        )
        .await;
    let quiet = f
        .publish(
            AUTHOR,
            f.fid,
            "CARDQUIET",
            "The first post has no links",
            ThreadVisibility::Public,
        )
        .await;
    for chunk in targets.chunks(3) {
        let links: Vec<_> = chunk
            .iter()
            .chain(std::iter::once(&public))
            .map(|tid| format!("[Card]({ORIGIN}/thread-{tid})"))
            .collect();
        f.reply(AUTHOR, busy, &links.join("\n\n")).await;
        f.reply(
            AUTHOR,
            quiet,
            &format!("Mid-sentence {} are not expanded", links.join(" ")),
        )
        .await;
    }
    for (tid, expected_queries, expected_cards) in [(busy, 1, 20), (quiet, 0, 0)] {
        let profile = Arc::new(RequestProfile::new());
        let reply = f
            .send(
                f.request(&format!("/thread-{tid}"), Some(READER)),
                Some(profile.clone()),
            )
            .await;
        assert_eq!(reply.status, StatusCode::OK);
        let card_queries: Vec<_> = profile
            .snapshot()
            .into_iter()
            .filter(|r| {
                r.source == QuerySource::Application && r.sql.contains("firstpost.erased_at=0")
            })
            .collect();
        assert_eq!(card_queries.len(), expected_queries, "{tid}");
        if let Some(query) = card_queries.first() {
            assert_eq!(query.sql.matches('?').count(), 20, "{}", query.sql);
        }
        assert_eq!(
            reply.body.matches("<aside class=\"topic-embed\">").count(),
            expected_cards,
            "{tid}"
        );
    }
    f.services.cleanup().await.unwrap();
}

/// The anonymous render cache keeps where the cards go, not the cards: a reply to the linked thread shows in the
/// card's reply count on the next anonymous view, as it does for members, and every anonymous view reads the cards
/// with one query.
#[tokio::test]
async fn anonymous_cards_show_the_linked_threads_current_reply_count() {
    let f = Fixture::new().await;
    let target = f
        .publish(
            TARGET,
            f.fid,
            "CARDCOUNTED",
            "Body",
            ThreadVisibility::Public,
        )
        .await;
    let source = f
        .publish(
            AUTHOR,
            f.fid,
            "CARDCOUNTING",
            &format!("[Link](/thread-{target})"),
            ThreadVisibility::Public,
        )
        .await;
    let path = format!("/thread-{source}");
    let meta = |body: &str| {
        body.split_once("class=\"topic-embed__meta\">")
            .and_then(|(_, rest)| rest.split_once("</p>"))
            .map(|(meta, _)| meta.to_owned())
            .unwrap_or_default()
    };
    let card_queries = |profile: &RequestProfile| {
        profile
            .snapshot()
            .into_iter()
            .filter(|r| {
                r.source == QuerySource::Application && r.sql.contains("firstpost.erased_at=0")
            })
            .count()
    };
    for _ in 0..2 {
        let anonymous = f.get(&path, None).await.body;
        assert!(meta(&anonymous).contains("0 replies"), "{anonymous}");
    }
    f.reply(AUTHOR, target, "A reply to the linked thread")
        .await;
    let member = f.get(&path, Some(READER)).await.body;
    assert!(meta(&member).contains("1 reply"), "{member}");
    let profile = Arc::new(RequestProfile::new());
    let anonymous = f.send(f.request(&path, None), Some(profile.clone())).await;
    assert_eq!(anonymous.status, StatusCode::OK);
    assert!(
        meta(&anonymous.body).contains("1 reply"),
        "{}",
        anonymous.body
    );
    assert_eq!(card_queries(&profile), 1);
    assert!(
        !anonymous.body.contains("title=\"gonof-"),
        "{}",
        anonymous.body
    );
    f.services.cleanup().await.unwrap();
}
