//! Visitor scope of a single thread: when posting or editing, the author chooses public ("Same as the forum") or
//! members only ("Always members only"); members-only threads are invisible to anonymous visitors and visible to
//! validly signed-in members at every exit.
//!
//! Threads are all posted through the web form or the member API, never by changing the database directly: if the
//! write path did not store `visibility`, all the "anonymous cannot see" assertions below would fail.
#[path = "support/fake_s3.rs"]
mod fake_s3;
#[path = "support/http_auth.rs"]
mod http_auth;

use std::net::SocketAddr;

use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{HeaderMap, Request, StatusCode, header},
};
use fake_s3::FakeS3;
use gono_forum_web::{
    activities, content,
    db::{self, Database},
    i18n::Lang,
    identity::{self, Identity, SessionGrant},
    media::{Media, StorageInput},
    router, runtime,
    test_support::TestFixture,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const ORIGIN: &str = "http://127.0.0.1:58833";
/// The password `support/http_auth.rs` sets for synthetic members; generating an API token requires entering it again.
const PASSWORD: &str = "Http-fixture-password-2026!";
const BOUNDARY: &str = "gonof-thread-visibility";
const ADMIN: i64 = 9811;
const READER: i64 = 9812;
const AUTHOR: i64 = 9813;

struct Fixture {
    services: TestFixture,
    app: Router,
    identity: Identity,
    media: Media,
    s3: FakeS3,
    admin: SessionGrant,
    reader: SessionGrant,
    author: SessionGrant,
    fid: i64,
    slug: String,
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
    raw: Vec<u8>,
}

impl Reply {
    fn location(&self) -> &str {
        self.headers[header::LOCATION].to_str().unwrap()
    }
}

impl Fixture {
    async fn new() -> Self {
        let services = TestFixture::migrated().await.unwrap();
        let db = services.db.clone();
        content::seed_forums(&db).await.unwrap();
        content::seed_fixture(&db).await.unwrap();
        for (uid, adminid) in [(ADMIN, 1), (READER, 0), (AUTHOR, 0)] {
            db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(?,?,'synthetic-hash',?,1,?,1)")
                .bind(uid).bind(format!("vis-{uid}")).bind(format!("vis-{uid}@example.test")).bind(adminid)
                .execute(db.executor()).await.unwrap();
        }
        let config = services.config.clone();
        let identity = Identity::new(
            db.clone(),
            config.identity.clone(),
            services.root.clone(),
            gono_forum_web::test_support::detached_global(),
        );
        let admin = http_auth::login(&identity, &db, ADMIN).await;
        let reader = http_auth::login(&identity, &db, READER).await;
        let author = http_auth::login(&identity, &db, AUTHOR).await;
        let media = Media::new(db.clone(), config.media.clone(), services.root.clone());
        let s3 = FakeS3::new().await;
        media
            .save_settings_actor(
                admin.member.actor(),
                StorageInput {
                    revision: 0,
                    endpoint: s3.endpoint.clone(),
                    region: "us-east-1".into(),
                    bucket: "thread-visibility".into(),
                    prefix: "visibility/".into(),
                    path_style: true,
                    access_key: "test-only-key".into(),
                    secret_key: "test-only-secret".into(),
                },
            )
            .await
            .unwrap();
        let forum = db::query("SELECT fid,slug FROM gonof_forum_forum WHERE visibility='public' AND status=1 AND parent_fid IS NULL ORDER BY fid LIMIT 1")
            .fetch_one(db.executor())
            .await
            .unwrap();
        let app = router::build_with_global(
            config,
            &services.root,
            db.clone(),
            services.cache.clone(),
            services.global.clone(),
        );
        Self {
            fid: forum.try_get("fid").unwrap(),
            slug: forum.try_get("slug").unwrap(),
            services,
            app,
            identity,
            media,
            s3,
            admin,
            reader,
            author,
        }
    }

    fn db(&self) -> &Database {
        &self.services.db
    }

    async fn scalar(&self, sql: &str) -> i64 {
        db::query_scalar(sql)
            .fetch_one(self.db().executor())
            .await
            .unwrap()
    }

    async fn text(&self, sql: &str) -> String {
        db::query_scalar(sql)
            .fetch_one(self.db().executor())
            .await
            .unwrap()
    }

    async fn execute(&self, sql: &str) {
        db::query(sql).execute(self.db().executor()).await.unwrap();
    }

    async fn send(&self, mut request: Request<Body>) -> Reply {
        request.extensions_mut().insert(ConnectInfo(
            "192.0.2.45:48322".parse::<SocketAddr>().unwrap(),
        ));
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        Reply {
            status,
            headers,
            body: String::from_utf8_lossy(&body).into_owned(),
            raw: body.to_vec(),
        }
    }

    async fn get(&self, path: &str, session: Option<&SessionGrant>) -> Reply {
        let mut builder = Request::builder().uri(path);
        if let Some(session) = session {
            builder = builder.header(
                header::COOKIE,
                format!("gonof_session={}", session.session_token),
            );
        }
        self.send(builder.body(Body::empty()).unwrap()).await
    }

    async fn post(&self, path: &str, session: &SessionGrant, fields: &[(&str, String)]) -> Reply {
        let mut form = url::form_urlencoded::Serializer::new(String::new());
        form.append_pair("_csrf", &session.csrf_token);
        for (name, value) in fields {
            form.append_pair(name, value);
        }
        self.send(
            Request::builder()
                .method("POST")
                .uri(path)
                .header(
                    header::COOKIE,
                    format!("gonof_session={}", session.session_token),
                )
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::ORIGIN, ORIGIN)
                .header(header::HOST, gono_forum_web::test_support::host_of(ORIGIN))
                .body(Body::from(form.finish()))
                .unwrap(),
        )
        .await
    }

    async fn api(
        &self,
        method: &str,
        path: &str,
        secret: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let reply = self
            .send(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header(header::AUTHORIZATION, format!("Bearer {secret}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await;
        let value = serde_json::from_str(&reply.body).unwrap_or(Value::Null);
        (reply.status, value)
    }

    async fn api_key(&self, session: &SessionGrant) -> String {
        self.identity
            .create_api_key(&session.session_token, PASSWORD, 86_400)
            .await
            .unwrap()
            .secret
    }

    /// The author posts a thread through the web form; returns the TID. When `visibility` is None the field is not
    /// submitted.
    async fn publish(&self, fid: i64, title: &str, body: &str, visibility: Option<&str>) -> i64 {
        let mut fields = vec![
            ("request_key", identity::random_token()),
            ("fid", fid.to_string()),
            ("title", title.into()),
            ("body", body.into()),
            ("category", "0".into()),
        ];
        if let Some(visibility) = visibility {
            fields.push(("visibility", visibility.into()));
        }
        let reply = self.post("/new", &self.author, &fields).await;
        assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
        db::query_scalar("SELECT tid FROM gonof_forum_thread WHERE subject=?")
            .bind(title)
            .fetch_one(self.db().executor())
            .await
            .unwrap()
    }

    async fn reply(&self, session: &SessionGrant, tid: i64, body: &str) -> i64 {
        let reply = self
            .post(
                &format!("/api/thread-{tid}/comments"),
                session,
                &[
                    ("request_key", identity::random_token()),
                    ("body", body.into()),
                ],
            )
            .await;
        assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
        db::query_scalar("SELECT MAX(pid) FROM gonof_forum_post WHERE tid=? AND first=0")
            .bind(tid)
            .fetch_one(self.db().executor())
            .await
            .unwrap()
    }

    async fn visibility(&self, tid: i64) -> String {
        self.text(&format!(
            "SELECT visibility FROM gonof_forum_thread WHERE tid={tid}"
        ))
        .await
    }

    /// Edits a thread: title and body keep their current values, the version takes the current value; when
    /// `visibility` is None it is not submitted.
    async fn edit(
        &self,
        session: &SessionGrant,
        tid: i64,
        visibility: Option<&str>,
        reason: &str,
    ) -> Reply {
        let row = db::query("SELECT t.subject,t.version AS thread_version,p.message,p.version FROM gonof_forum_thread t JOIN gonof_forum_post p ON p.first_tid=t.tid WHERE t.tid=?")
            .bind(tid)
            .fetch_one(self.db().executor())
            .await
            .unwrap();
        let mut fields = vec![
            ("request_key", identity::random_token()),
            ("title", db::text(&row, "subject").unwrap()),
            ("body", db::text(&row, "message").unwrap()),
            (
                "expected_post_version",
                row.try_get::<i64, _>("version").unwrap().to_string(),
            ),
            (
                "expected_thread_version",
                row.try_get::<i64, _>("thread_version").unwrap().to_string(),
            ),
            ("reason", reason.into()),
        ];
        if let Some(visibility) = visibility {
            fields.push(("visibility", visibility.into()));
        }
        self.post(&format!("/edit/thread-{tid}"), session, &fields)
            .await
    }

    /// Submits a report through the web form.
    async fn report(&self, session: &SessionGrant, kind: &str, id: i64) -> Reply {
        self.post(
            "/my/reports",
            session,
            &[
                ("target_type", kind.to_owned()),
                ("target_id", id.to_string()),
                ("reason", "VISREPORT report reason".to_owned()),
                ("request_key", identity::random_token()),
            ],
        )
        .await
    }

    async fn reports_by_reader(&self, target_id: i64) -> i64 {
        self.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_report WHERE uid={READER} AND target_id={target_id}"
        ))
        .await
    }

    async fn upload(&self, session: &SessionGrant, fields: &[(&str, &str)], bytes: &[u8]) -> Reply {
        self.upload_form(session, fields, bytes, &[], true).await
    }

    /// A no-JS editor upload: `fields`, the file, then `tail`, in the order the editor form submits them. With
    /// `complete` false the body stops inside the file, as when the connection drops.
    async fn upload_form(
        &self,
        session: &SessionGrant,
        fields: &[(&str, &str)],
        bytes: &[u8],
        tail: &[(&str, &str)],
        complete: bool,
    ) -> Reply {
        let mut body = Vec::new();
        let key = identity::random_token();
        let mut parts = vec![
            ("_csrf", session.csrf_token.as_str()),
            ("request_key", &key),
        ];
        parts.extend_from_slice(fields);
        for (name, value) in parts {
            body.extend(
                format!(
                    "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
                )
                .as_bytes(),
            );
        }
        body.extend(
            format!(
                "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"photo.png\"\r\nContent-Type: application/octet-stream\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend(bytes);
        if complete {
            for (name, value) in tail {
                body.extend(
                    format!(
                        "\r\n--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}"
                    )
                    .as_bytes(),
                );
            }
            body.extend(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
        }
        self.send(
            Request::builder()
                .method("POST")
                .uri("/api/media")
                .header(
                    header::COOKIE,
                    format!("gonof_session={}", session.session_token),
                )
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={BOUNDARY}"),
                )
                .header(header::ORIGIN, ORIGIN)
                .header(header::HOST, gono_forum_web::test_support::host_of(ORIGIN))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
    }
}

fn png(side: u32) -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(side, side, image::Rgba([30, 90, 200, 255]));
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    bytes.into_inner()
}

/// The section of a right-sidebar thread card (hot or latest).
fn sidebar_card<'a>(page: &'a str, heading: &str) -> &'a str {
    let start = page
        .find(&format!(">{heading}</h2>"))
        .unwrap_or_else(|| panic!("the page has no \"{heading}\" card"));
    let rest = &page[start..];
    &rest[..rest.find("</ol>").unwrap()]
}

/// The four home page views, forum lists and search.
#[tokio::test]
async fn members_only_topics_stay_out_of_guest_lists_and_search() {
    let f = Fixture::new().await;
    const HIDDEN: &str = "VISHIDDEN4401 members-only list thread";
    const OPEN: &str = "VISOPEN4402 public list thread";
    let hidden = f
        .publish(f.fid, HIDDEN, "Body for members only", Some("members"))
        .await;
    // Without submitting the field, it is published as public.
    let open = f.publish(f.fid, OPEN, "Public body", None).await;
    assert_eq!(f.visibility(hidden).await, "members");
    assert_eq!(f.visibility(open).await, "public");
    f.execute(&format!(
        "UPDATE gonof_forum_thread SET displayorder=1,digest=1 WHERE tid IN ({hidden},{open})"
    ))
    .await;
    for path in [
        "/".to_owned(),
        "/?sort=created".into(),
        "/?sort=pinned".into(),
        "/?sort=featured".into(),
        format!("/{}", f.slug),
        "/search?q=VISHIDDEN4401".into(),
        "/search?q=VISOPEN4402".into(),
    ] {
        let guest = f.get(&path, None).await;
        assert_eq!(guest.status, StatusCode::OK, "{path}");
        // The search page echoes the search term in the input box, so judge by the full title.
        assert!(
            !guest.body.contains(HIDDEN),
            "anonymous must not see it: {path}"
        );
        let member = f.get(&path, Some(&f.reader)).await;
        assert_eq!(member.status, StatusCode::OK, "{path}");
        if path != "/search?q=VISOPEN4402" {
            assert!(
                member.body.contains(HIDDEN),
                "a member should see it: {path}"
            );
        }
        if path != "/search?q=VISHIDDEN4401" {
            assert!(
                guest.body.contains(OPEN),
                "anonymous sees the public thread: {path}"
            );
        }
    }
    f.services.cleanup().await.unwrap();
}

/// The hot and latest sidebar cards and the site statistics.
#[tokio::test]
async fn sidebar_cards_and_statistics_leave_out_members_only_topics() {
    let f = Fixture::new().await;
    let before = content::statistics_at(
        f.db(),
        identity::now() as u64,
        gono_forum_web::test_support::SEEDED_OFFSET_MINUTES,
    )
    .await
    .unwrap();
    const TITLE: &str = "VISSIDE4411 members-only sidebar thread";
    let tid = f
        .publish(f.fid, TITLE, "Sidebar body", Some("members"))
        .await;
    // Only threads with new replies enter hot.
    f.reply(&f.reader, tid, "A reply from a member").await;
    f.reply(&f.author, tid, "Another reply from the author")
        .await;
    let guest = f.get("/", None).await.body;
    assert!(!sidebar_card(&guest, "Popular threads").contains("VISSIDE4411"));
    assert!(!sidebar_card(&guest, "Latest threads").contains("VISSIDE4411"));
    let member = f.get("/", Some(&f.reader)).await.body;
    assert!(sidebar_card(&member, "Popular threads").contains(TITLE));
    assert!(sidebar_card(&member, "Latest threads").contains(TITLE));
    // The three statistics are the same for everyone and count only public content: members-only threads and their
    // replies are not counted.
    let after = content::statistics_at(
        f.db(),
        identity::now() as u64,
        gono_forum_web::test_support::SEEDED_OFFSET_MINUTES,
    )
    .await
    .unwrap();
    assert_eq!(after.total, before.total);
    assert_eq!(after.replies, before.replies);
    f.publish(
        f.fid,
        "VISSIDE4412 public control",
        "Public body",
        Some("public"),
    )
    .await;
    let public = content::statistics_at(
        f.db(),
        identity::now() as u64,
        gono_forum_web::test_support::SEEDED_OFFSET_MINUTES,
    )
    .await
    .unwrap();
    assert_eq!(public.total, before.total + 1);
    f.services.cleanup().await.unwrap();
}

/// Recent content, threads and replies collections of the personal space.
#[tokio::test]
async fn member_space_shows_members_only_content_to_members_only() {
    let f = Fixture::new().await;
    const TITLE: &str = "VISSPACE4421 members-only space thread";
    let tid = f.publish(f.fid, TITLE, "Space body", Some("members")).await;
    f.reply(&f.author, tid, "VISSPACEREPLY4421 reply from the author")
        .await;
    for path in [
        format!("/space/{AUTHOR}"),
        format!("/space/{AUTHOR}/threads"),
        format!("/space/{AUTHOR}/posts"),
    ] {
        let guest = f.get(&path, None).await;
        assert_eq!(guest.status, StatusCode::OK, "{path}");
        assert!(
            !guest.body.contains("VISSPACE4421"),
            "anonymous must not see the title: {path}"
        );
        assert!(!guest.body.contains("VISSPACEREPLY4421"), "{path}");
        let member = f.get(&path, Some(&f.reader)).await;
        assert!(
            member.body.contains(TITLE),
            "a member should see it: {path}"
        );
    }
    // The member's own list marks it "Members only".
    let own = f.get("/my/threads", Some(&f.author)).await.body;
    assert!(own.contains(TITLE));
    assert!(own.contains(">Members only</span>"), "{own}");
    f.services.cleanup().await.unwrap();
}

/// Thread details, share meta information and reply permalinks.
#[tokio::test]
async fn topic_pages_share_meta_and_reply_links_need_a_member() {
    let f = Fixture::new().await;
    const TITLE: &str = "VISDETAIL4431 members-only detail thread";
    const OPENING: &str = "VISOPENING4431 first post summary";
    let tid = f.publish(f.fid, TITLE, OPENING, Some("members")).await;
    let pid = f.reply(&f.reader, tid, "VISREPLY4431 reply body").await;
    let path = format!("/thread-{tid}");
    // Same as threads in member-visible forums: anonymous visitors get 404, with no title or summary in the page or head.
    let guest = f.get(&path, None).await;
    assert_eq!(guest.status, StatusCode::NOT_FOUND);
    for secret in ["VISDETAIL4431", "VISOPENING4431", "VISREPLY4431"] {
        assert!(!guest.body.contains(secret), "{secret}");
    }
    let member = f.get(&path, Some(&f.reader)).await;
    assert_eq!(member.status, StatusCode::OK);
    assert!(member.body.contains(TITLE));
    // Share summaries are also subject to access permission: what members see has the description replaced with the
    // site introduction, without the first post summary or structured data.
    let site_description = runtime::read(f.db()).await.unwrap().site.description;
    assert!(member.body.contains(&format!(
        "<meta property=\"og:description\" content=\"{site_description}\">"
    )));
    let head = &member.body[..member.body.find("</head>").unwrap()];
    assert!(!head.contains("VISOPENING4431"));
    assert!(!head.contains("application/ld+json"));
    assert!(
        member
            .body
            .contains("<span class=\"tag\">Members only</span>")
    );
    assert!(member.body.contains("VISREPLY4431"));
    // The reply box on the thread page carries no visibility scope.
    assert!(!member.body.contains("name=\"visibility\""));
    let permalink = format!("/posts/{pid}");
    assert_eq!(f.get(&permalink, None).await.status, StatusCode::NOT_FOUND);
    let member = f.get(&permalink, Some(&f.reader)).await;
    assert_eq!(member.status, StatusCode::FOUND);
    assert!(member.location().starts_with(&path));
    f.services.cleanup().await.unwrap();
}

/// Attachments in members-only threads, and the visibility scope not being lost in the no-JS upload round trip.
#[tokio::test]
async fn attachments_in_members_only_topics_download_only_for_members() {
    let f = Fixture::new().await;
    let fid = f.fid.to_string();
    let prizes = "First prize | 1 | Code | | Annual membership";
    let code = "MEMBER-A-123";
    let upload = f
        .upload(
            &f.author,
            &[
                ("fid", fid.as_str()),
                ("title", "VISMEDIA4441 with an attachment"),
                ("visibility", "members"),
                ("category", "3"),
                ("entry_mode", "click"),
                ("requirements", "Please confirm you can attend"),
                ("review", "1"),
                ("early_draw", "1"),
                ("prizes", prizes),
                ("lottery_codes", code),
            ],
            &png(8),
        )
        .await;
    assert_eq!(upload.status, StatusCode::OK, "{}", upload.body);
    let aid = f
        .scalar("SELECT MAX(aid) FROM gonof_forum_attachment")
        .await;
    assert!(
        upload
            .body
            .contains("<input type=\"hidden\" name=\"visibility\" value=\"members\">"),
        "the upload status page must carry the visibility: {}",
        upload.body
    );
    for field in [
        r#"name="entry_mode" value="click""#,
        r#"name="requirements" hidden>Please confirm you can attend</textarea>"#,
        r#"name="review" value="1""#,
        r#"name="early_draw" value="1""#,
    ] {
        assert!(
            upload.body.contains(field),
            "the upload check page should keep {field}"
        );
    }
    assert!(
        upload
            .body
            .contains(&format!("name=\"prizes\" hidden>{prizes}</textarea>"))
    );
    assert!(upload.body.contains(&format!(
        "name=\"lottery_codes\" autocomplete=\"off\" hidden>{code}</textarea>"
    )));
    f.media.process_pending(identity::now()).await.unwrap();
    let continued = f
        .post(
            &format!("/api/media/{aid}/continue"),
            &f.author,
            &[
                ("request_key", identity::random_token()),
                ("fid", fid.clone()),
                ("title", "VISMEDIA4441 with an attachment".into()),
                ("body", "Attachment body".into()),
                ("visibility", "members".into()),
                ("category", "3".into()),
                ("entry_mode", "click".into()),
                ("requirements", "Please confirm you can attend".into()),
                ("review", "1".into()),
                ("early_draw", "1".into()),
                ("prizes", prizes.into()),
                ("lottery_codes", code.into()),
            ],
        )
        .await;
    assert_eq!(continued.status, StatusCode::OK, "{}", continued.body);
    assert!(continued.body.contains(&format!(">{prizes}</textarea>")));
    assert!(continued.body.contains(&format!(">{code}</textarea>")));
    assert!(
        continued
            .body
            .contains("name=\"visibility\" value=\"members\" checked")
    );
    for field in [
        r#"name="entry_mode" value="click" checked"#,
        r#"name="requirements" rows="3">Please confirm you can attend</textarea>"#,
        r#"name="review" value="1" checked"#,
        r#"name="early_draw" value="1" checked"#,
    ] {
        assert!(
            continued.body.contains(field),
            "the editor should restore {field}"
        );
    }
    let tid = f
        .publish(
            f.fid,
            "VISMEDIA4441 with an attachment",
            &format!("![image](attachment:{aid})"),
            Some("members"),
        )
        .await;
    let pid = f
        .scalar(&format!(
            "SELECT pid FROM gonof_forum_post WHERE first_tid={tid}"
        ))
        .await;
    let target = format!("/media/{aid}?pid={pid}");
    assert_eq!(f.get(&target, None).await.status, StatusCode::NOT_FOUND);
    let member = f.get(&target, Some(&f.reader)).await;
    assert_eq!(member.status, StatusCode::OK);
    let key = f
        .text(&format!(
            "SELECT attachment FROM gonof_forum_attachment_0 WHERE aid={aid}"
        ))
        .await;
    assert_eq!(member.raw, f.s3.object_ending_with(&key));
    // Members can report attachments they can read, with the link pointing at that reference in the members-only
    // thread.
    let form = f
        .get(
            &format!("/my/reports/new?target_type=attachment&target_id={aid}"),
            Some(&f.reader),
        )
        .await;
    assert_eq!(form.status, StatusCode::OK, "{}", form.body);
    assert!(form.body.contains(&format!("/media/{aid}?pid={pid}")));
    let submitted = f.report(&f.reader, "attachment", aid).await;
    assert_eq!(
        submitted.status,
        StatusCode::SEE_OTHER,
        "{}",
        submitted.body
    );
    assert_eq!(f.reports_by_reader(aid).await, 1);
    f.services.cleanup().await.unwrap();
}

/// A no-JS upload that is refused still refills the edit form with the thread's audience. The editor form puts the
/// file before the audience field, and an editor refilled as "public" opens a members-only thread to guests on the
/// next save.
#[tokio::test]
async fn refused_no_script_uploads_keep_the_audience_of_an_edited_thread() {
    let f = Fixture::new().await;
    let title = "VISUPLOAD5521 members only";
    let tid = f
        .publish(f.fid, title, "Members body", Some("members"))
        .await;
    let pid = f
        .scalar(&format!(
            "SELECT pid FROM gonof_forum_post WHERE first_tid={tid}"
        ))
        .await;
    let (tid_text, pid_text, fid_text) = (tid.to_string(), pid.to_string(), f.fid.to_string());
    let head = [
        ("tid", tid_text.as_str()),
        ("fid", fid_text.as_str()),
        ("edit_pid", pid_text.as_str()),
        ("edit_thread", "true"),
        ("title", title),
        ("body", "Members body"),
    ];
    let members = r#"name="visibility" value="members" checked"#;
    let image_max = f.services.config.media.image_max_bytes as usize;

    // Too large: the rest of the file is drained and the audience after it is still read.
    let oversized = vec![0u8; image_max + 1];
    let refused = f
        .upload_form(
            &f.author,
            &head,
            &oversized,
            &[("visibility", "members")],
            true,
        )
        .await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.body);
    assert!(refused.body.contains(Lang::EN.media_error_too_large()));
    assert!(refused.body.contains(members), "{}", refused.body);

    // Cut off inside the file: the audience never arrives, so the edit form takes the stored one.
    let cut = f.upload_form(&f.author, &head, &png(8), &[], false).await;
    assert_eq!(cut.status, StatusCode::BAD_REQUEST, "{}", cut.body);
    assert!(cut.body.contains(members), "{}", cut.body);
    assert!(!cut.body.contains(r#"value="public" checked"#));

    // Neither attempt kept a staged file or an attachment, and the thread is still closed to guests.
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_forum_attachment")
            .await,
        0
    );
    assert_eq!(f.visibility(tid).await, "members");
    assert_eq!(
        f.get(&format!("/thread-{tid}"), None).await.status,
        StatusCode::NOT_FOUND
    );
    f.services.cleanup().await.unwrap();
}

/// Upload routes take bodies up to the image limit plus room for the form: with `[media] image_max_bytes` raised
/// above the old fixed route limit (27 MiB), a larger image reaches the upload handler, and a body past the derived
/// limit is cut off.
#[tokio::test]
async fn upload_routes_follow_the_image_limit() {
    let mut f = Fixture::new().await;
    let mut config = f.services.config.clone();
    config.media.image_max_bytes = 30 * 1024 * 1024;
    f.app = router::build_with_global(
        config,
        &f.services.root,
        f.db().clone(),
        f.services.cache.clone(),
        f.services.global.clone(),
    );
    let fid = f.fid.to_string();
    let fields = [("fid", fid.as_str()), ("title", "VISLIMIT6631")];
    let cut_off = |reply: &Reply| {
        [
            Lang::EN.media_error_interrupted(),
            Lang::EN.media_error_format(),
            Lang::EN.media_error_too_large(),
        ]
        .iter()
        .any(|message| reply.body.contains(message))
    };
    let mut image = png(8);
    image.resize(28 * 1024 * 1024, 0);
    let accepted = f.upload(&f.author, &fields, &image).await;
    assert!(!cut_off(&accepted), "{}", accepted.status);
    assert_eq!(accepted.status, StatusCode::OK, "{}", accepted.body);
    image.resize(35 * 1024 * 1024, 0);
    let refused = f.upload(&f.author, &fields, &image).await;
    assert!(cut_off(&refused), "{}", refused.status);
    f.services.cleanup().await.unwrap();
}

/// The sitemap includes only public threads.
#[tokio::test]
async fn sitemap_lists_public_topics_only() {
    let f = Fixture::new().await;
    runtime::update(
        f.db(),
        &f.services.global,
        f.admin.member.actor(),
        "site.settings.save",
        |s| s.seo.indexing = true,
    )
    .await
    .unwrap();
    let hidden = f
        .publish(f.fid, "VISMAP4451 members only", "Body", Some("members"))
        .await;
    let open = f
        .publish(f.fid, "VISMAP4452 public", "Body", Some("public"))
        .await;
    let page = (hidden.max(open) + 4999) / 5000;
    let xml = f.get(&format!("/sitemap.xml?page={page}"), None).await.body;
    assert!(xml.contains(&format!("/thread-{open}<")), "{xml}");
    assert!(!xml.contains(&format!("/thread-{hidden}<")));
    f.services.cleanup().await.unwrap();
}

/// Activity cards and the member API's read endpoints.
#[tokio::test]
async fn activity_cards_and_api_reads_need_a_member() {
    let f = Fixture::new().await;
    let secret = f.api_key(&f.author).await;
    let (status, created) = f
        .api(
            "POST",
            "/api/v1/threads",
            &secret,
            json!({
                "forum": f.fid,
                "title": "VISPOLL4461 members-only poll",
                "body": "Poll body",
                "category": 4,
                "options": ["A", "B"],
                "deadline": identity::now() + 3600,
                "visibility": "members",
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let tid = created["tid"].as_i64().unwrap();
    assert_eq!(f.visibility(tid).await, "members");
    assert!(
        activities::card(f.db(), tid, None).await.unwrap().is_none(),
        "anonymous gets no activity card for a members-only thread"
    );
    assert!(
        activities::card(f.db(), tid, Some(READER))
            .await
            .unwrap()
            .is_some()
    );
    let member = f.get(&format!("/thread-{tid}"), Some(&f.reader)).await;
    assert!(member.body.contains("0 voters · up to"), "{}", member.body);
    assert_eq!(
        f.get(&format!("/thread-{tid}"), None).await.status,
        StatusCode::NOT_FOUND
    );
    let reader_key = f.api_key(&f.reader).await;
    let (status, card) = f
        .api(
            "GET",
            &format!("/api/v1/threads/{tid}/activity"),
            &reader_key,
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{card}");
    f.services.cleanup().await.unwrap();
}

/// Notifications go only to members: members following the forum get the notification for the new members-only thread
/// and can open it.
#[tokio::test]
async fn forum_followers_are_notified_of_members_only_topics() {
    let f = Fixture::new().await;
    gono_forum_web::interaction::social::set_follow_forum(
        f.db(),
        f.reader.member.actor(),
        f.fid,
        true,
        &identity::random_token(),
    )
    .await
    .unwrap();
    const TITLE: &str = "VISNOTIFY4471 members-only follow notification";
    let tid = f
        .publish(f.fid, TITLE, "Notification body", Some("members"))
        .await;
    assert_eq!(f.visibility(tid).await, "members");
    assert_eq!(
        f.get(&format!("/thread-{tid}"), None).await.status,
        StatusCode::NOT_FOUND
    );
    let report = gono_forum_web::notifications::deliver_pending(f.db(), identity::now())
        .await
        .unwrap();
    assert_eq!(report.failed, 0);
    let pid = f
        .scalar(&format!(
            "SELECT pid FROM gonof_forum_post WHERE first_tid={tid}"
        ))
        .await;
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_home_notification WHERE uid={READER} AND pid={pid} AND type='follow_forum'"
        ))
        .await,
        1
    );
    let page = f.get("/my/notifications", Some(&f.reader)).await;
    let panel = page.body.split("<aside").next().unwrap();
    assert!(panel.contains(TITLE), "{panel}");
    // Anonymous visitors have no notification entry.
    assert_eq!(
        f.get("/my/notifications", None).await.status,
        StatusCode::SEE_OTHER
    );
    f.services.cleanup().await.unwrap();
}

/// The member API's posting and editing endpoints: optional `visibility`, public by default, unknown values 422.
#[tokio::test]
async fn member_api_accepts_visibility_and_rejects_unknown_values() {
    let f = Fixture::new().await;
    let secret = f.api_key(&f.author).await;
    let create = |title: &str, visibility: Option<&str>| {
        let mut body = json!({"forum": f.slug, "title": title, "body": "API body"});
        if let Some(visibility) = visibility {
            body["visibility"] = json!(visibility);
        }
        body
    };
    for bad in ["secret", "Members", "private", ""] {
        let (status, error) = f
            .api(
                "POST",
                "/api/v1/threads",
                &secret,
                create("VISAPI4480 invalid", Some(bad)),
            )
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}: {error}");
        assert_eq!(error["error"]["code"], "validation");
    }
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_forum_thread WHERE subject='VISAPI4480 invalid'")
            .await,
        0
    );
    let (status, members) = f
        .api(
            "POST",
            "/api/v1/threads",
            &secret,
            create("VISAPI4481 members only", Some("members")),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{members}");
    let hidden = members["tid"].as_i64().unwrap();
    assert_eq!(f.visibility(hidden).await, "members");
    assert_eq!(
        f.get(&format!("/thread-{hidden}"), None).await.status,
        StatusCode::NOT_FOUND
    );
    let (status, open) = f
        .api(
            "POST",
            "/api/v1/threads",
            &secret,
            create("VISAPI4482 default", None),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(f.visibility(open["tid"].as_i64().unwrap()).await, "public");

    // Edit: change only the visibility scope.
    let path = format!("/api/v1/threads/{hidden}");
    let (status, error) = f
        .api("PATCH", &path, &secret, json!({"visibility": "everyone"}))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{error}");
    let (status, edited) = f
        .api("PATCH", &path, &secret, json!({"visibility": "public"}))
        .await;
    assert_eq!(status, StatusCode::OK, "{edited}");
    assert_eq!(f.visibility(hidden).await, "public");
    assert_eq!(
        f.get(&format!("/thread-{hidden}"), None).await.status,
        StatusCode::OK
    );
    // Replies do not have this field.
    let reply = f.reply(&f.author, hidden, "API reply").await;
    let (status, _) = f
        .api(
            "PATCH",
            &format!("/api/v1/posts/{reply}"),
            &secret,
            json!({"body": "Edited", "visibility": "members"}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    f.services.cleanup().await.unwrap();
}

/// The author changes a public thread to members-only: only the thread version is bumped and a log written, and the
/// anonymous lists and thread page stop showing it immediately.
#[tokio::test]
async fn switching_to_members_only_hides_the_topic_from_guests_at_once() {
    let f = Fixture::new().await;
    const TITLE: &str = "VISEDIT4491 public first, then members only";
    let tid = f.publish(f.fid, TITLE, "Edit body", Some("public")).await;
    // First let the anonymous view put the list, thread page and rendered results into the shared cache.
    for _ in 0..2 {
        assert!(f.get("/", None).await.body.contains(TITLE));
        assert!(
            f.get(&format!("/{}", f.slug), None)
                .await
                .body
                .contains(TITLE)
        );
        assert_eq!(
            f.get(&format!("/thread-{tid}"), None).await.status,
            StatusCode::OK
        );
    }
    let form = f.get(&format!("/edit/thread-{tid}"), Some(&f.author)).await;
    assert!(
        form.body
            .contains("name=\"visibility\" value=\"public\" checked"),
        "{}",
        form.body
    );
    let before = db::query("SELECT t.version AS thread_version,p.version,p.edited_at FROM gonof_forum_thread t JOIN gonof_forum_post p ON p.first_tid=t.tid WHERE t.tid=?")
        .bind(tid).fetch_one(f.db().executor()).await.unwrap();
    let saved = f.edit(&f.author, tid, Some("members"), "").await;
    assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
    assert_eq!(f.visibility(tid).await, "members");
    let after = db::query("SELECT t.version AS thread_version,p.version,p.edited_at,p.pid FROM gonof_forum_thread t JOIN gonof_forum_post p ON p.first_tid=t.tid WHERE t.tid=?")
        .bind(tid).fetch_one(f.db().executor()).await.unwrap();
    assert_eq!(
        after.try_get::<i64, _>("thread_version").unwrap(),
        before.try_get::<i64, _>("thread_version").unwrap() + 1
    );
    // The body did not change: this floor's version is not bumped, it is not marked "Edited", and no body
    // revision is written.
    assert_eq!(
        after.try_get::<i64, _>("version").unwrap(),
        before.try_get::<i64, _>("version").unwrap()
    );
    assert_eq!(after.try_get::<i64, _>("edited_at").unwrap(), 0);
    let pid: i64 = after.try_get("pid").unwrap();
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_forum_post_revision WHERE pid={pid}"
        ))
        .await,
        0
    );
    let log: Value = serde_json::from_str(
        &f.text(&format!(
            "SELECT data FROM gonof_common_log WHERE type='content.edit' AND pid={pid} ORDER BY id DESC LIMIT 1"
        ))
        .await,
    )
    .unwrap();
    assert_eq!(log["before"]["visibility"], "public");
    assert_eq!(log["after"]["visibility"], "members");
    assert_eq!(log["result"], "applied");
    // The shared cache must not keep stale data: the anonymous lists and thread page stop showing it immediately, and
    // members see it as usual.
    assert!(!f.get("/", None).await.body.contains("VISEDIT4491"));
    assert!(
        !f.get(&format!("/{}", f.slug), None)
            .await
            .body
            .contains("VISEDIT4491")
    );
    assert_eq!(
        f.get(&format!("/thread-{tid}"), None).await.status,
        StatusCode::NOT_FOUND
    );
    assert!(f.get("/", Some(&f.reader)).await.body.contains(TITLE));
    let form = f.get(&format!("/edit/thread-{tid}"), Some(&f.author)).await;
    assert!(
        form.body
            .contains("name=\"visibility\" value=\"members\" checked")
    );
    // Changed back to public, anonymous visitors can see it again.
    let saved = f.edit(&f.author, tid, Some("public"), "").await;
    assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
    assert!(f.get("/", None).await.body.contains(TITLE));
    f.services.cleanup().await.unwrap();
}

/// Follows the edit time window and moderation hide rules; managers editing on the author's behalf matches management
/// editing, requiring a reason and notifying the author.
#[tokio::test]
async fn visibility_edits_follow_the_edit_window_and_governance_rules() {
    let f = Fixture::new().await;
    let tid = f
        .publish(f.fid, "VISRULE4501 rules", "Rules body", Some("public"))
        .await;
    let pid = f
        .scalar(&format!(
            "SELECT pid FROM gonof_forum_post WHERE first_tid={tid}"
        ))
        .await;
    // Invalid value: 422, database unchanged.
    let invalid = f.edit(&f.author, tid, Some("secret"), "").await;
    assert_eq!(invalid.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        invalid
            .body
            .contains("Visibility can only be public or members only")
    );
    assert_eq!(f.visibility(tid).await, "public");
    // After a moderation hide, the author cannot change it.
    f.execute(&format!(
        "UPDATE gonof_forum_post SET invisible=-1 WHERE pid={pid}"
    ))
    .await;
    let hidden = f.edit(&f.author, tid, Some("members"), "").await;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND, "{}", hidden.body);
    assert_eq!(f.visibility(tid).await, "public");
    f.execute(&format!(
        "UPDATE gonof_forum_post SET invisible=0 WHERE pid={pid}"
    ))
    .await;
    // After the edit time window passes, the author cannot change it.
    // The seeded author edit time window `settings.posting.edit_window_hours` (24 hours).
    let window = 24 * 3600;
    f.execute(&format!(
        "UPDATE gonof_forum_post SET dateline=dateline-{} WHERE pid={pid}",
        window + 60
    ))
    .await;
    let late = f.edit(&f.author, tid, Some("members"), "").await;
    assert_eq!(late.status, StatusCode::FORBIDDEN, "{}", late.body);
    assert!(
        late.body
            .contains("The time to edit, withdraw or restore this content has passed")
    );
    assert_eq!(f.visibility(tid).await, "public");
    // Managers go through the management edit branch: not limited by the time window, a reason is required, and the
    // author receives a moderation notification.
    let missing_reason = f.edit(&f.admin, tid, Some("members"), "").await;
    assert_eq!(missing_reason.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(f.visibility(tid).await, "public");
    let managed = f
        .edit(
            &f.admin,
            tid,
            Some("members"),
            "Mentions member details, made members only",
        )
        .await;
    assert_eq!(managed.status, StatusCode::SEE_OTHER, "{}", managed.body);
    assert_eq!(f.visibility(tid).await, "members");
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_home_notification WHERE uid={AUTHOR} AND type='moderation' AND event_key LIKE 'moderation.%.e' AND reason='Mentions member details, made members only'"
        ))
        .await,
        1
    );
    f.services.cleanup().await.unwrap();
}

/// Member-visible forums: the posting form marks the forum's scope; editing offers no option, only states that it
/// follows the forum, and submission keeps the original value.
#[tokio::test]
async fn forms_explain_members_only_forums_and_keep_the_stored_choice() {
    let f = Fixture::new().await;
    let form = f.get("/new", Some(&f.author)).await.body;
    assert!(form.contains("<legend class=\"option-group__legend\">Who can see it</legend>"));
    assert!(form.contains("name=\"visibility\" value=\"public\" checked"));
    // Submitting an unknown value when posting: 422, input kept, no thread created.
    let rejected = f
        .post(
            "/new",
            &f.author,
            &[
                ("request_key", identity::random_token()),
                ("fid", f.fid.to_string()),
                ("title", "VISFORM4511 invalid".into()),
                ("body", "Body that is kept".into()),
                ("visibility", "everyone".into()),
            ],
        )
        .await;
    assert_eq!(rejected.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        rejected
            .body
            .contains("Visibility can only be public or members only")
    );
    assert!(rejected.body.contains("Body that is kept"));
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_forum_thread WHERE subject='VISFORM4511 invalid'")
            .await,
        0
    );

    f.execute(&format!(
        "UPDATE gonof_forum_forum SET visibility='members' WHERE fid={}",
        f.fid
    ))
    .await;
    // Changing the database directly: readable forums are in the process cache, so this process's layer must be
    // cleared before it shows.
    f.services.global.clear_local();
    let name = f
        .text(&format!(
            "SELECT name FROM gonof_forum_forum WHERE fid={}",
            f.fid
        ))
        .await;
    let form = f.get("/new", Some(&f.author)).await.body;
    assert!(
        form.contains(&format!("{name} (members only)</option>")),
        "{form}"
    );
    let tid = f
        .publish(
            f.fid,
            "VISFORM4512 members forum",
            "Members forum body",
            None,
        )
        .await;
    assert_eq!(f.visibility(tid).await, "public");
    assert_eq!(
        f.get(&format!("/thread-{tid}"), None).await.status,
        StatusCode::NOT_FOUND
    );
    let member = f.get(&format!("/thread-{tid}"), Some(&f.reader)).await;
    assert!(
        member
            .body
            .contains("<span class=\"tag\">Members only</span>")
    );
    let edit = f
        .get(&format!("/edit/thread-{tid}"), Some(&f.author))
        .await
        .body;
    assert!(
        edit.contains(
            "this forum is members only, so the thread is shown to signed-in members only"
        ),
        "{edit}"
    );
    assert!(!edit.contains("name=\"visibility\""));
    let saved = f.edit(&f.author, tid, None, "").await;
    assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
    assert_eq!(f.visibility(tid).await, "public");
    // The reply edit form does not have this field.
    let reply = f.reply(&f.author, tid, "Members forum reply").await;
    let edit = f
        .get(&format!("/edit/post-{reply}"), Some(&f.author))
        .await
        .body;
    assert!(!edit.contains("name=\"visibility\""));
    f.services.cleanup().await.unwrap();
}

/// Admin content management lists and can open members-only threads as usual.
#[tokio::test]
async fn staff_tools_still_see_members_only_topics() {
    let f = Fixture::new().await;
    const TITLE: &str = "VISSTAFF4521 members-only admin";
    let tid = f.publish(f.fid, TITLE, "Admin body", Some("members")).await;
    let pid = f
        .scalar(&format!(
            "SELECT pid FROM gonof_forum_post WHERE first_tid={tid}"
        ))
        .await;
    let list = f
        .get("/manage/content?q=VISSTAFF4521", Some(&f.admin))
        .await;
    assert_eq!(list.status, StatusCode::OK);
    assert!(list.body.contains(TITLE), "{}", list.body);
    let detail = f
        .get(&format!("/manage/content/{pid}"), Some(&f.admin))
        .await;
    assert_eq!(detail.status, StatusCode::OK);
    assert!(detail.body.contains(TITLE));
    f.services.cleanup().await.unwrap();
}

/// Member reports are judged by "the reporter can read it": members-only threads, threads in member forums and the
/// replies in them can all be reported; anonymous visitors cannot report; private forums and floors hidden by
/// moderation are still 404, and the report entry does not reveal content that cannot be seen.
#[tokio::test]
async fn members_report_what_they_can_read_and_nothing_more() {
    let f = Fixture::new().await;
    // A members-only thread in a public forum and its replies.
    const TITLE: &str = "VISREPORT4531 members-only reported thread";
    let tid = f
        .publish(f.fid, TITLE, "Report body", Some("members"))
        .await;
    let reply = f.reply(&f.author, tid, "VISREPORTREPLY4531 reply").await;
    let page = f.get(&format!("/thread-{tid}"), Some(&f.reader)).await.body;
    let form_path = format!("/my/reports/new?target_type=thread&target_id={tid}");
    assert!(
        page.contains("/my/reports/new?target_type=thread")
            && page.contains(&format!("target_id={tid}\">Report</a>")),
        "the thread page must offer members a report link"
    );
    let form = f.get(&form_path, Some(&f.reader)).await;
    assert_eq!(form.status, StatusCode::OK, "{}", form.body);
    assert!(form.body.contains(TITLE));
    let submitted = f.report(&f.reader, "thread", tid).await;
    assert_eq!(
        submitted.status,
        StatusCode::SEE_OTHER,
        "{}",
        submitted.body
    );
    assert_eq!(f.reports_by_reader(tid).await, 1);
    // The member's own report details show the target as usual.
    let detail = f.get(submitted.location(), Some(&f.reader)).await;
    assert!(detail.body.contains(TITLE), "{}", detail.body);
    let submitted = f.report(&f.reader, "reply", reply).await;
    assert_eq!(
        submitted.status,
        StatusCode::SEE_OTHER,
        "{}",
        submitted.body
    );
    assert_eq!(f.reports_by_reader(reply).await, 1);

    // Anonymous visitors cannot report: the form requires sign-in, and submissions are not stored.
    let guest = f.get(&form_path, None).await;
    assert_eq!(guest.status, StatusCode::SEE_OTHER);
    assert!(guest.location().starts_with("/login"));
    assert!(!guest.body.contains("VISREPORT4531"));
    let anonymous = f
        .send(
            Request::builder()
                .method("POST")
                .uri("/my/reports")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::ORIGIN, ORIGIN)
                .header(header::HOST, gono_forum_web::test_support::host_of(ORIGIN))
                .body(Body::from(format!(
                    "target_type=thread&target_id={tid}&reason=x&request_key={}",
                    identity::random_token()
                )))
                .unwrap(),
        )
        .await;
    assert_ne!(anonymous.status, StatusCode::SEE_OTHER);
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_report WHERE target_id={tid}"
        ))
        .await,
        1
    );

    // A thread in a member-visible forum (the author chose public): an old issue, reportable as well.
    let members_forum = f
        .scalar(&format!(
            "SELECT fid FROM gonof_forum_forum WHERE status=1 AND parent_fid IS NULL AND fid<>{} ORDER BY fid LIMIT 1",
            f.fid
        ))
        .await;
    let forum_tid = f
        .publish(
            members_forum,
            "VISREPORT4532 members forum thread",
            "Body",
            None,
        )
        .await;
    f.execute(&format!(
        "UPDATE gonof_forum_forum SET visibility='members' WHERE fid={members_forum}"
    ))
    .await;
    // After raw SQL changes the forum, sync the copies and clear the process forum cache the way the admin save does.
    gono_forum_web::test_support::sync_forum_copies(f.db(), members_forum)
        .await
        .unwrap();
    f.services.global.clear_local();
    assert_eq!(
        f.get(&format!("/thread-{forum_tid}"), None).await.status,
        StatusCode::NOT_FOUND
    );
    let submitted = f.report(&f.reader, "thread", forum_tid).await;
    assert_eq!(
        submitted.status,
        StatusCode::SEE_OTHER,
        "{}",
        submitted.body
    );
    assert_eq!(f.reports_by_reader(forum_tid).await, 1);

    // A reply hidden by moderation: still not found, and the page does not reveal the body.
    f.execute(&format!(
        "UPDATE gonof_forum_post SET invisible=-1 WHERE pid={reply}"
    ))
    .await;
    let hidden = f
        .get(
            &format!("/my/reports/new?target_type=reply&target_id={reply}"),
            Some(&f.reader),
        )
        .await;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND);
    assert!(!hidden.body.contains("VISREPORTREPLY4531"));

    // A thread in a private forum: members cannot read it, and cannot report it either.
    let private_forum = f
        .scalar(&format!(
            "SELECT fid FROM gonof_forum_forum WHERE status=1 AND parent_fid IS NULL AND fid NOT IN ({},{members_forum}) ORDER BY fid LIMIT 1",
            f.fid
        ))
        .await;
    let private_tid = f
        .publish(
            private_forum,
            "VISREPORT4533 private forum thread",
            "Body",
            Some("members"),
        )
        .await;
    f.execute(&format!(
        "UPDATE gonof_forum_forum SET visibility='private' WHERE fid={private_forum}"
    ))
    .await;
    let form = f
        .get(
            &format!("/my/reports/new?target_type=thread&target_id={private_tid}"),
            Some(&f.reader),
        )
        .await;
    assert_eq!(form.status, StatusCode::NOT_FOUND);
    assert!(!form.body.contains("VISREPORT4533"));
    let submitted = f.report(&f.reader, "thread", private_tid).await;
    assert_eq!(submitted.status, StatusCode::NOT_FOUND);
    assert!(!submitted.body.contains("VISREPORT4533"));
    assert_eq!(f.reports_by_reader(private_tid).await, 0);
    f.services.cleanup().await.unwrap();
}
