//! HTTP tests for routes not covered elsewhere — the editor attachment endpoints, attachment downloads, moving
//! threads, retrying background tasks and data requests, brand images, data export downloads, and a smoke test of the
//! `redis.enabled = false` deployment shape. Object storage uses an in-process S3 stub.
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
    config::Config,
    content,
    db::{self, Database},
    identity::{self, Actor, Identity, SessionGrant},
    media::{Media, NewUpload, StorageInput},
    privacy::{self, Access, NewRequest},
    router,
    test_support::TestFixture,
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const ORIGIN: &str = "http://127.0.0.1:58833";
const PASSWORD: &str = "Http-fixture-password-2026!";
const BOUNDARY: &str = "gonof-route-coverage";

struct Fixture {
    services: TestFixture,
    config: Config,
    app: Router,
    media: Media,
    s3: FakeS3,
    admin: SessionGrant,
    member: SessionGrant,
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Reply {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    fn location(&self) -> &str {
        self.headers[header::LOCATION].to_str().unwrap()
    }
}

impl Fixture {
    async fn new(configure: impl FnOnce(&mut Config)) -> Self {
        let services = TestFixture::migrated().await.unwrap();
        let db = services.db.clone();
        content::seed_forums(&db).await.unwrap();
        content::seed_fixture(&db).await.unwrap();
        for (uid, adminid) in [(9801, 1), (9802, 0), (9803, 0)] {
            db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(?,?,'synthetic-hash',?,1,?,1)")
                .bind(uid).bind(format!("route-{uid}")).bind(format!("route-{uid}@example.test")).bind(adminid)
                .execute(db.executor()).await.unwrap();
        }
        let mut config = services.config.clone();
        configure(&mut config);
        let identity = Identity::new(
            db.clone(),
            config.identity.clone(),
            services.root.clone(),
            gono_forum_web::test_support::detached_global(),
        );
        let admin = http_auth::login(&identity, &db, 9801).await;
        let member = http_auth::login(&identity, &db, 9802).await;
        let media = Media::new(db.clone(), config.media.clone(), services.root.clone());
        let s3 = FakeS3::new().await;
        media
            .save_settings_actor(
                admin.member.actor(),
                StorageInput {
                    revision: 0,
                    endpoint: s3.endpoint.clone(),
                    region: "us-east-1".into(),
                    bucket: "route-coverage".into(),
                    prefix: "route/".into(),
                    path_style: true,
                    access_key: "test-only-key".into(),
                    secret_key: "test-only-secret".into(),
                },
            )
            .await
            .unwrap();
        let app = router::build(config.clone(), &services.root, db.clone());
        Self {
            services,
            config,
            app,
            media,
            s3,
            admin,
            member,
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

    async fn send(&self, request: Request<Body>) -> Reply {
        let mut request = request;
        request.extensions_mut().insert(ConnectInfo(
            "192.0.2.44:48321".parse::<SocketAddr>().unwrap(),
        ));
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec();
        Reply {
            status,
            headers,
            body,
        }
    }

    async fn get(&self, path: &str, session: Option<&SessionGrant>, htmx: bool) -> Reply {
        let mut builder = Request::builder().uri(path);
        if let Some(session) = session {
            builder = builder.header(
                header::COOKIE,
                format!("gonof_session__port58833={}", session.session_token),
            );
        }
        if htmx {
            builder = builder.header("HX-Request", "true");
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
                    format!("gonof_session__port58833={}", session.session_token),
                )
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::ORIGIN, ORIGIN)
                .header(header::HOST, gono_forum_web::test_support::host_of(ORIGIN))
                .body(Body::from(form.finish()))
                .unwrap(),
        )
        .await
    }

    /// Submits an attachment in the order of the editor script: token, request key, file.
    async fn upload(&self, session: &SessionGrant, filename: &str, bytes: &[u8]) -> Reply {
        let mut body = Vec::new();
        for (name, value) in [
            ("_csrf", session.csrf_token.clone()),
            ("request_key", identity::random_token()),
        ] {
            body.extend(
                format!(
                    "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
                )
                .as_bytes(),
            );
        }
        body.extend(
            format!(
                "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend(bytes);
        body.extend(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
        self.send(
            Request::builder()
                .method("POST")
                .uri("/api/media")
                .header(
                    header::COOKIE,
                    format!("gonof_session__port58833={}", session.session_token),
                )
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={BOUNDARY}"),
                )
                .header(header::ORIGIN, ORIGIN)
                .header(header::HOST, gono_forum_web::test_support::host_of(ORIGIN))
                .header("HX-Request", "true")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
    }

    async fn process_media(&self) {
        self.media.process_pending(identity::now()).await.unwrap();
    }

    async fn lifecycle(&self, aid: i64) -> String {
        db::query_scalar("SELECT lifecycle FROM gonof_forum_attachment_0 WHERE aid=?")
            .bind(aid)
            .fetch_one(self.db().executor())
            .await
            .unwrap()
    }
}

fn png(side: u32) -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(side, side, image::Rgba([200, 30, 90, 255]));
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    bytes.into_inner()
}

fn attachment_id(fragment: &str) -> i64 {
    fragment
        .split("data-attachment-id=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("no attachment id: {fragment}"))
}

async fn forum_of(db: &Database, tid: i64) -> (i64, i64) {
    let row = db::query("SELECT fid,version FROM gonof_forum_thread WHERE tid=?")
        .bind(tid)
        .fetch_one(db.executor())
        .await
        .unwrap();
    (row.try_get("fid").unwrap(), row.try_get("version").unwrap())
}

/// Status queries, continuing editing and cancellation after an `/api/media` upload, plus real downloads from
/// `/media/{aid}`.
#[tokio::test]
async fn editor_attachment_routes_report_continue_cancel_and_download() {
    let f = Fixture::new(|_| {}).await;
    let image = f.upload(&f.member, "photo.png", &png(8)).await;
    assert_eq!(image.status, StatusCode::OK, "{}", image.text());
    let aid = attachment_id(&image.text());
    assert!(image.text().contains("data-state=\"pending\""));

    let status = f
        .get(&format!("/api/media/{aid}"), Some(&f.member), true)
        .await;
    assert_eq!(status.status, StatusCode::OK);
    assert!(
        status.text().contains("Waiting for the check"),
        "{}",
        status.text()
    );
    // Others cannot query it, and without logging in it is 401 right away.
    assert_eq!(
        f.get(&format!("/api/media/{aid}"), Some(&f.admin), true)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        f.get(&format!("/api/media/{aid}"), None, true).await.status,
        StatusCode::UNAUTHORIZED
    );
    // An attachment that has not finished its checks cannot be downloaded.
    assert_eq!(
        f.get(&format!("/media/{aid}"), Some(&f.member), false)
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    f.process_media().await;
    assert_eq!(f.lifecycle(aid).await, "ready");
    let status = f
        .get(&format!("/api/media/{aid}"), Some(&f.member), true)
        .await;
    assert!(
        status.text().contains("Passed the check"),
        "{}",
        status.text()
    );
    assert!(status.text().contains("data-attachment-insert"));

    // The editor without JS: after the checks pass it returns to the editor, the attachment reference is appended to
    // the end of the body, and the draft is kept as is.
    let fid = forum_of(f.db(), 103).await.0;
    let continued = f
        .post(
            &format!("/api/media/{aid}/continue"),
            &f.member,
            &[
                ("request_key", identity::random_token()),
                ("fid", fid.to_string()),
                ("title", "Draft with an attachment".into()),
                ("body", "Draft body".into()),
            ],
        )
        .await;
    assert_eq!(continued.status, StatusCode::OK, "{}", continued.text());
    assert!(continued.text().contains("Draft body"));
    assert!(
        continued.text().contains(&format!("attachment:{aid}")),
        "{}",
        continued.text()
    );

    // The member downloads an unreferenced attachment: the bytes are exactly the checked copy in object storage.
    let download = f
        .get(&format!("/media/{aid}"), Some(&f.member), false)
        .await;
    assert_eq!(download.status, StatusCode::OK);
    assert_eq!(download.headers[header::CONTENT_TYPE], "image/png");
    // Images are not requested again within 5 minutes, and after that are revalidated with the ETag; the session
    // layer does not rewrite this to no-store.
    assert_eq!(
        download.headers[header::CACHE_CONTROL],
        gono_forum_web::media::IMAGE_CACHE_CONTROL
    );
    let etag = download.headers[header::ETAG].to_str().unwrap().to_owned();
    assert_eq!(etag, format!("\"media-{aid}\""));
    assert!(download.headers.get(header::CONTENT_DISPOSITION).is_none());
    let revalidated = f
        .send(
            Request::builder()
                .uri(format!("/media/{aid}"))
                .header(
                    header::COOKIE,
                    format!("gonof_session__port58833={}", f.member.session_token),
                )
                .header(header::IF_NONE_MATCH, format!("W/{etag}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(revalidated.status, StatusCode::NOT_MODIFIED);
    assert!(revalidated.body.is_empty());
    assert_eq!(
        revalidated.headers[header::CACHE_CONTROL],
        gono_forum_web::media::IMAGE_CACHE_CONTROL
    );
    let head = f
        .send(
            Request::builder()
                .method("HEAD")
                .uri(format!("/media/{aid}"))
                .header(
                    header::COOKIE,
                    format!("gonof_session__port58833={}", f.member.session_token),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(head.status, StatusCode::OK);
    assert!(head.body.is_empty());
    assert_eq!(
        head.headers[header::CONTENT_LENGTH],
        download.body.len().to_string()
    );
    // Someone else revalidating with the same entity tag also gets 404: permissions are checked before comparing tags.
    let foreign = f
        .send(
            Request::builder()
                .uri(format!("/media/{aid}"))
                .header(header::IF_NONE_MATCH, etag.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(foreign.status, StatusCode::NOT_FOUND);
    assert!(
        foreign.headers[header::CACHE_CONTROL]
            .to_str()
            .unwrap()
            .contains("no-store")
    );
    let stored: Vec<u8> = {
        let key: String =
            db::query_scalar("SELECT attachment FROM gonof_forum_attachment_0 WHERE aid=?")
                .bind(aid)
                .fetch_one(f.db().executor())
                .await
                .unwrap();
        f.s3.object_ending_with(&key)
    };
    assert_eq!(download.body, stored);
    for viewer in [None, Some(&f.admin)] {
        assert_eq!(
            f.get(&format!("/media/{aid}"), viewer, false).await.status,
            StatusCode::NOT_FOUND
        );
    }

    // Only images are allowed: non-images are rejected at registration, and no attachment row is
    // written.
    let text = f
        .upload(&f.member, "notes v1.txt", b"plain text body\n")
        .await;
    assert_eq!(
        text.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        text.text()
    );
    assert!(
        text.text()
            .contains(gono_forum_web::media::unsupported_format()),
        "{}",
        text.text()
    );

    // Cancellation: only the member can cancel, and after cancelling it goes to cleanup.
    let pending = f.upload(&f.member, "cancel.png", &png(4)).await;
    let cancel_aid = attachment_id(&pending.text());
    assert_eq!(
        f.post(&format!("/api/media/{cancel_aid}/cancel"), &f.admin, &[])
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let cancelled = f
        .post(&format!("/api/media/{cancel_aid}/cancel"), &f.member, &[])
        .await;
    assert_eq!(cancelled.status, StatusCode::OK, "{}", cancelled.text());
    assert!(cancelled.text().contains("Attachment cancelled"));
    assert_eq!(f.lifecycle(cancel_aid).await, "purging");
    f.services.cleanup().await.unwrap();
}

/// Permissions, version conflicts, redirects and operation log of moving threads in the admin area.
fn gif(side: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
        encoder
            .encode_frame(image::Frame::new(image::RgbaImage::from_pixel(
                side,
                side,
                image::Rgba([10, 200, 90, 255]),
            )))
            .unwrap();
    }
    bytes
}

/// Real thumbnails in thread lists take the static images in the first post in order of appearance (GIFs and those
/// inside spoilers do not count); `/thumb/{tid}/{aid}` checks permissions against the current state of the thread and
/// first post, with an ETag and 5-minute caching; an image whose thumbnail object is missing is scaled on the first
/// request and the result is stored.
#[tokio::test]
async fn thread_list_thumbnails_follow_the_first_post_and_its_visibility() {
    let f = Fixture::new(|_| {}).await;
    let mut aids = Vec::new();
    for (name, bytes) in [
        ("a.png", png(300)),
        ("spoiler.png", png(150)),
        ("motion.gif", gif(40)),
        ("b.png", png(200)),
    ] {
        let reply = f.upload(&f.member, name, &bytes).await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
        aids.push(attachment_id(&reply.text()));
    }
    f.process_media().await;
    let [a, spoiler, motion, b] = aids[..] else {
        unreachable!()
    };
    for aid in [a, spoiler, motion, b] {
        assert_eq!(f.lifecycle(aid).await, "ready");
    }
    // Thumbnail objects for static images were already stored during processing; the GIF has none.
    let key = |aid: i64| {
        let db = f.db().clone();
        async move {
            db::query_scalar::<String>(
                "SELECT attachment FROM gonof_forum_attachment_0 WHERE aid=?",
            )
            .bind(aid)
            .fetch_one(db.executor())
            .await
            .unwrap()
        }
    };
    assert_eq!(
        f.s3.count_ending_with(&format!("{}.thumb", key(a).await)),
        1
    );
    assert_eq!(
        f.s3.count_ending_with(&format!("{}.thumb", key(motion).await)),
        0
    );
    let fid = forum_of(f.db(), 103).await.0;
    let published = f
        .post(
            "/new",
            &f.member,
            &[
                ("request_key", identity::random_token()),
                ("fid", fid.to_string()),
                ("title", "Thread with thumbnails".into()),
                (
                    "body",
                    format!(
                        "![A](attachment:{a})\n\n<spoiler>\n\n![Spoiler](attachment:{spoiler})\n\n</spoiler>\n\n![Animation](attachment:{motion}) ![B](attachment:{b})"
                    ),
                ),
            ],
        )
        .await;
    assert_eq!(
        published.status,
        StatusCode::SEE_OTHER,
        "{}",
        published.text()
    );
    let tid: i64 = published
        .location()
        .split("/thread-")
        .nth(1)
        .and_then(|rest| rest.split(['#', '?', '/']).next())
        .and_then(|value| value.parse().ok())
        .unwrap();
    let stored: Vec<i64> =
        db::query_scalar("SELECT aid FROM gonof_forum_thread_image WHERE tid=? ORDER BY position")
            .bind(tid)
            .fetch_all(f.db().executor())
            .await
            .unwrap();
    assert_eq!(
        stored,
        [a, b],
        "in order of appearance, skipping the spoiler and the GIF"
    );
    let home = f.get("/", None, false).await;
    assert!(
        home.text().contains(&format!("/thumb/{tid}/{a}")),
        "{}",
        home.text()
    );
    assert!(home.text().contains(&format!("/thumb/{tid}/{b}")));
    assert!(!home.text().contains("/static/img/placeholder/"));

    let thumb = f.get(&format!("/thumb/{tid}/{a}"), None, false).await;
    assert_eq!(
        thumb.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&thumb.body)
    );
    assert_eq!(
        thumb.headers[header::CACHE_CONTROL],
        gono_forum_web::media::IMAGE_CACHE_CONTROL
    );
    let etag = thumb.headers[header::ETAG].to_str().unwrap().to_owned();
    assert_eq!(etag, format!("\"thumb-{a}-180\""));
    let decoded = image::load_from_memory(&thumb.body).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (180, 180));
    let again = f
        .send(
            Request::builder()
                .uri(format!("/thumb/{tid}/{a}"))
                .header(header::IF_NONE_MATCH, etag.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(again.status, StatusCode::NOT_MODIFIED);
    // Attachments not among this thread's thumbnails (inside spoilers, from other threads) are always 404.
    for path in [
        format!("/thumb/{tid}/{spoiler}"),
        format!("/thumb/{tid}/{motion}"),
        format!("/thumb/103/{a}"),
    ] {
        assert_eq!(
            f.get(&path, None, false).await.status,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    // An image whose thumbnail object is missing: scaled on the first request, and a copy is stored.
    let b_thumb = format!("{}.thumb", key(b).await);
    f.s3.remove_ending_with(&b_thumb);
    let regenerated = f.get(&format!("/thumb/{tid}/{b}"), None, false).await;
    assert_eq!(regenerated.status, StatusCode::OK);
    assert_eq!(
        image::load_from_memory(&regenerated.body).unwrap().height(),
        180
    );
    assert_eq!(
        f.s3.count_ending_with(&b_thumb),
        1,
        "scaled on request, then stored"
    );
    // After the first post is hidden or the thread withdrawn, revalidating with the tag does not get 304 either.
    for (hide, restore) in [
        (
            format!("UPDATE gonof_forum_post SET invisible=-1 WHERE first_tid={tid}"),
            format!("UPDATE gonof_forum_post SET invisible=0 WHERE first_tid={tid}"),
        ),
        (
            format!("UPDATE gonof_forum_thread SET deleted_at=1 WHERE tid={tid}"),
            format!("UPDATE gonof_forum_thread SET deleted_at=0 WHERE tid={tid}"),
        ),
    ] {
        db::query(&hide).execute(f.db().executor()).await.unwrap();
        let hidden = f
            .send(
                Request::builder()
                    .uri(format!("/thumb/{tid}/{a}"))
                    .header(header::IF_NONE_MATCH, etag.clone())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(hidden.status, StatusCode::NOT_FOUND, "{hide}");
        db::query(&restore)
            .execute(f.db().executor())
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn thread_move_form() {
    let f = Fixture::new(|_| {}).await;
    let (source, version) = forum_of(f.db(), 103).await;
    let target = f
        .scalar(&format!(
            "SELECT fid FROM gonof_forum_forum WHERE fid<>{source} AND status=1 ORDER BY fid LIMIT 1"
        ))
        .await;
    let form = |key: String, version: i64| {
        vec![
            ("target_fid", target.to_string()),
            ("version", version.to_string()),
            ("request_key", key),
            ("reason", "Move to a better forum".into()),
        ]
    };
    let refused = f
        .post(
            "/manage/threads/103/move",
            &f.member,
            &form(identity::random_token(), version),
        )
        .await;
    assert!(
        matches!(
            refused.status,
            StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
        ),
        "{}",
        refused.status
    );
    let stale = f
        .post(
            "/manage/threads/103/move",
            &f.admin,
            &form(identity::random_token(), version + 7),
        )
        .await;
    assert_eq!(stale.status, StatusCode::CONFLICT, "{}", stale.text());
    let moved = f
        .post(
            "/manage/threads/103/move",
            &f.admin,
            &form(identity::random_token(), version),
        )
        .await;
    assert_eq!(moved.status, StatusCode::SEE_OTHER, "{}", moved.text());
    assert_eq!(moved.location(), "/thread-103");
    assert_eq!(forum_of(f.db(), 103).await.0, target);
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_log WHERE type='content.thread.move'")
            .await,
        1
    );

    f.services.cleanup().await.unwrap();
}

/// The retry forms for background tasks and data requests.
#[tokio::test]
async fn task_and_data_request_retry_forms() {
    let f = Fixture::new(|_| {}).await;
    let now = identity::now();
    let task=db::query("INSERT INTO gonof_common_outbox(event_key,event_type,aggregate_type,aggregate_id,payload,status,attempts,available_at,lease_until,dateline) VALUES(?,'email.verify','member',9802,'{}','pending',2,?,0,?)")
        .bind(identity::random_token()).bind(now + 600).bind(now)
        .execute(f.db().executor()).await.unwrap().last_insert_id() as i64;
    let snapshot = |attempts: i64, available_at: i64| {
        vec![
            ("attempts", attempts.to_string()),
            ("available_at", available_at.to_string()),
            ("lease_until", "0".to_owned()),
            ("expected", "pending".to_owned()),
            ("reason", "The service is back".to_owned()),
        ]
    };
    assert_eq!(
        f.post(
            &format!("/manage/tasks/{task}/retry"),
            &f.member,
            &snapshot(2, now + 600)
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    let retried = f
        .post(
            &format!("/manage/tasks/{task}/retry"),
            &f.admin,
            &snapshot(2, now + 600),
        )
        .await;
    assert_eq!(retried.status, StatusCode::SEE_OTHER, "{}", retried.text());
    assert_eq!(retried.location(), format!("/manage/tasks/{task}"));
    assert!(
        f.scalar(&format!(
            "SELECT available_at FROM gonof_common_outbox WHERE eventid={task}"
        ))
        .await
            <= identity::now()
    );
    // The page snapshot is stale: stay on the form with a conflict notice.
    let stale = f
        .post(
            &format!("/manage/tasks/{task}/retry"),
            &f.admin,
            &snapshot(2, now + 600),
        )
        .await;
    assert_eq!(stale.status, StatusCode::CONFLICT, "{}", stale.text());

    let requester = http_auth::login(
        &Identity::new(
            f.db().clone(),
            f.config.identity.clone(),
            f.services.root.clone(),
            gono_forum_web::test_support::detached_global(),
        ),
        f.db(),
        9803,
    )
    .await;
    let request = privacy::submit(
        f.db(),
        &Access::Session(requester.member.actor()),
        NewRequest {
            kind: "export".into(),
            content_mode: "retain".into(),
            request_key: identity::random_token(),
            password: PASSWORD.into(),
        },
        now,
    )
    .await
    .unwrap();
    let retry = |reason: &str| vec![("reason", reason.to_owned())];
    let not_failed = f
        .post(
            &format!("/manage/data-requests/{request}/retry"),
            &f.admin,
            &retry("Not failed yet"),
        )
        .await;
    assert_eq!(
        not_failed.status,
        StatusCode::CONFLICT,
        "{}",
        not_failed.text()
    );
    // When a background task fails temporarily, the request is marked failed and the event goes back to pending.
    db::query("UPDATE gonof_common_data_request SET status='failed',version=version+1 WHERE id=?")
        .bind(request)
        .execute(f.db().executor())
        .await
        .unwrap();
    assert_eq!(
        f.post(
            &format!("/manage/data-requests/{request}/retry"),
            &f.member,
            &retry("Not allowed")
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        f.post(
            &format!("/manage/data-requests/{request}/retry"),
            &f.admin,
            &retry("")
        )
        .await
        .status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let retried = f
        .post(
            &format!("/manage/data-requests/{request}/retry"),
            &f.admin,
            &retry("Storage is back"),
        )
        .await;
    assert_eq!(retried.status, StatusCode::SEE_OTHER, "{}", retried.text());
    assert_eq!(retried.location(), "/manage/data-requests");
    let status: String =
        db::query_scalar("SELECT status FROM gonof_common_data_request WHERE id=?")
            .bind(request)
            .fetch_one(f.db().executor())
            .await
            .unwrap();
    assert_eq!(status, "processing");
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_log WHERE type='privacy.retry'")
            .await,
        1
    );
    f.services.cleanup().await.unwrap();
}

/// Public reads of brand images (originals and scaled sizes) and downloads of data exports.
#[tokio::test]
async fn brand_images_and_data_exports_download() {
    let f = Fixture::new(|_| {}).await;
    let (path, mut file) = f.media.staging_file().await.unwrap();
    tokio::io::AsyncWriteExt::write_all(&mut file, &png(64))
        .await
        .unwrap();
    file.sync_all().await.unwrap();
    drop(file);
    let upload = f
        .media
        .register_brand_upload(
            f.admin.member.actor(),
            NewUpload {
                request_key: identity::random_token(),
                filename: "brand.png".into(),
                description: "Forum brand image".into(),
                alt_text: "Sample Forum".into(),
                path,
            },
        )
        .await
        .unwrap();
    f.process_media().await;
    assert_eq!(f.lifecycle(upload.aid).await, "ready");
    for slot in ["logo", "favicon"] {
        let saved = f
            .post(
                &format!("/manage/branding/{slot}"),
                &f.admin,
                &[("aid", upload.aid.to_string()), ("version", "0".into())],
            )
            .await;
        assert_eq!(
            saved.status,
            StatusCode::SEE_OTHER,
            "{slot}: {}",
            saved.text()
        );
    }
    let source = f
        .get(&format!("/brand/logo/{}/source", upload.aid), None, false)
        .await;
    assert_eq!(source.status, StatusCode::OK);
    assert_eq!(source.headers[header::CONTENT_TYPE], "image/png");
    assert_eq!(
        source.headers[header::CACHE_CONTROL],
        "public, max-age=86400"
    );
    assert!(source.headers.get(header::SET_COOKIE).is_none());
    assert_eq!(image::load_from_memory(&source.body).unwrap().width(), 64);
    let icon = f
        .get(&format!("/brand/favicon/{}/32", upload.aid), None, false)
        .await;
    assert_eq!(icon.status, StatusCode::OK);
    let decoded = image::load_from_memory(&icon.body).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (32, 32));
    for path in [
        // The image is not bound to this purpose, and the size does not belong to this purpose either.
        format!("/brand/app_icon/{}/192", upload.aid),
        format!("/brand/logo/{}/32", upload.aid),
        format!("/brand/logo/{}/source", upload.aid + 1000),
    ] {
        let missing = f.get(&path, None, false).await;
        assert_eq!(missing.status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(missing.headers[header::CACHE_CONTROL], "no-store");
    }

    // Multi-domain sites: a domain's own logo and site icon, read according to the
    // current binding in the domain table.
    let on = f
        .post(
            "/manage/domains/switch",
            &f.admin,
            &[("enabled", "on".into())],
        )
        .await;
    assert_eq!(on.status, StatusCode::SEE_OTHER, "{}", on.text());
    let created = f
        .post(
            "/manage/domains",
            &f.admin,
            &[
                ("domain_id", "0".into()),
                ("version", "0".into()),
                ("site_url", "http://demo2.test".into()),
                ("site_name", "Customer Feedback Center".into()),
                ("canonical", "self".into()),
                ("enabled", "1".into()),
                ("logo_aid", upload.aid.to_string()),
                ("favicon_aid", upload.aid.to_string()),
            ],
        )
        .await;
    assert_eq!(created.status, StatusCode::SEE_OTHER, "{}", created.text());
    let domain: i64 = f
        .scalar("SELECT domain_id FROM gonof_common_site_domain WHERE site_url='http://demo2.test'")
        .await;
    let logo = f
        .get(
            &format!("/brand/domain/{domain}/logo/{}/source", upload.aid),
            None,
            false,
        )
        .await;
    assert_eq!(logo.status, StatusCode::OK);
    assert_eq!(logo.headers[header::CONTENT_TYPE], "image/png");
    let favicon = f
        .get(
            &format!("/brand/domain/{domain}/favicon/{}/32", upload.aid),
            None,
            false,
        )
        .await;
    assert_eq!(favicon.status, StatusCode::OK);
    for path in [
        format!("/brand/domain/{domain}/logo/{}/source", upload.aid + 1000),
        format!("/brand/domain/{}/logo/{}/source", domain + 1, upload.aid),
        format!("/brand/domain/{domain}/share/{}/source", upload.aid),
    ] {
        assert_eq!(
            f.get(&path, None, false).await.status,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    let home = f
        .send(
            Request::builder()
                .uri("/")
                .header(header::HOST, "demo2.test")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert!(
        home.text().contains(&format!(
            "/brand/domain/{domain}/logo/{}/source",
            upload.aid
        )) && home
            .text()
            .contains(&format!("/brand/domain/{domain}/favicon/{}/32", upload.aid)),
        "{}",
        home.text()
    );

    // Data export: only the member can download it, and the file is a zip.
    let now = identity::now();
    let actor: Actor = f.member.member.actor();
    let request = privacy::submit(
        f.db(),
        &Access::Session(actor),
        NewRequest {
            kind: "export".into(),
            content_mode: "retain".into(),
            request_key: identity::random_token(),
            password: PASSWORD.into(),
        },
        now,
    )
    .await
    .unwrap();
    let path = format!("/my/data/requests/{request}/download");
    // There is no downloadable file before processing finishes.
    assert_ne!(
        f.get(&path, Some(&f.member), false).await.status,
        StatusCode::OK
    );
    privacy::process_pending(
        f.db(),
        &f.services.global,
        &f.media,
        &f.services.root,
        &f.config.identity.local_directory,
        90,
        now,
    )
    .await
    .unwrap();
    let archive = f.get(&path, Some(&f.member), false).await;
    assert_eq!(archive.status, StatusCode::OK, "{}", archive.text());
    assert_eq!(archive.headers[header::CONTENT_TYPE], "application/zip");
    assert_eq!(
        archive.headers[header::CONTENT_DISPOSITION],
        format!("attachment; filename=gonof-data-{request}.zip")
    );
    assert_eq!(archive.headers[header::CACHE_CONTROL], "private, no-store");
    assert_eq!(
        archive.headers[header::CONTENT_LENGTH],
        archive.body.len().to_string()
    );
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive.body.clone())).unwrap();
    assert!(!zip.is_empty());
    assert!(zip.by_name("README.txt").is_ok());
    assert_eq!(
        f.get(&path, Some(&f.admin), false).await.status,
        StatusCode::NOT_FOUND,
        "another member's export cannot be downloaded"
    );
    let anonymous = f.get(&path, None, false).await;
    assert_eq!(anonymous.status, StatusCode::SEE_OTHER);
    assert_eq!(anonymous.location(), "/data/access");
    f.services.cleanup().await.unwrap();
}

/// `redis.enabled = false` (single-machine, single-process deployment): public pages, login pages and writes all take
/// the full MySQL path, without connecting to Redis or leaving any keys under this fixture's namespace.
#[tokio::test]
async fn redis_disabled_deployment_serves_pages_and_writes() {
    let f = Fixture::new(|config| {
        config.redis.enabled = false;
        // The connection string points to an unreachable address: as soon as any code path connects to Redis, the request
        // fails.
        config.redis.url = "redis://127.0.0.1:9/0".into();
    })
    .await;
    for path in [
        "/",
        "/ai",
        "/thread-103",
        "/thread-103?view=flat",
        "/search?q=fixture",
        "/search/users?q=fixture",
        "/sitemap.xml",
        "/healthz",
    ] {
        for htmx in [false, true] {
            let reply = f.get(path, None, htmx).await;
            assert_eq!(reply.status, StatusCode::OK, "{path}: {}", reply.text());
        }
    }
    for path in [
        "/my",
        "/my/profile",
        "/my/favorites",
        "/my/notifications",
        "/my/credits",
        "/search/users?q=fixture",
    ] {
        let reply = f.get(path, Some(&f.member), false).await;
        assert_eq!(reply.status, StatusCode::OK, "{path}");
    }
    let fid = forum_of(f.db(), 103).await.0;
    let published = f
        .post(
            "/new",
            &f.member,
            &[
                ("request_key", identity::random_token()),
                ("fid", fid.to_string()),
                ("title", "Posting works with Redis off".into()),
                (
                    "body",
                    "Smoke test body for a single-process deployment".into(),
                ),
            ],
        )
        .await;
    assert_eq!(
        published.status,
        StatusCode::SEE_OTHER,
        "{}",
        published.text()
    );
    let thread = f.get(published.location(), None, false).await;
    assert_eq!(thread.status, StatusCode::OK);
    assert!(
        thread
            .text()
            .contains("Smoke test body for a single-process deployment")
    );
    let home = f.get("/", None, false).await;
    assert!(
        home.text().contains("Posting works with Redis off"),
        "a new thread bypasses the cache and shows at once"
    );

    // Only look at this fixture's namespace: KEYS cannot be used on a shared instance, and other people's keys must not
    // be touched.
    let mut redis = redis::Client::open(f.services.config.redis.url.as_str())
        .unwrap()
        .get_multiplexed_async_connection()
        .await
        .unwrap();
    let mut cursor = 0_u64;
    let mut keys: Vec<String> = Vec::new();
    loop {
        let (next, batch): (u64, Vec<String>) = redis::cmd("SCAN")
            .arg(cursor)
            .arg("MATCH")
            .arg(format!("{}:*", f.services.config.redis.namespace))
            .arg("COUNT")
            .arg(1000)
            .query_async(&mut redis)
            .await
            .unwrap();
        keys.extend(batch);
        cursor = next;
        if cursor == 0 {
            break;
        }
    }
    assert!(
        keys.is_empty(),
        "nothing is cached with Redis off: {keys:?}"
    );
    f.services.cleanup().await.unwrap();
}

/// Email tickets for data requests are limited per email address: each normalised email may request at most 3 times
/// per hour. Over the limit the receipt is the same as a normal request,
/// only no email is sent; other spellings (full-width, case) count as the same email, and other emails are
/// unaffected.
#[tokio::test]
async fn data_access_tickets_are_capped_per_email_without_changing_the_reply() {
    let f = Fixture::new(|_| {}).await;
    for uid in [9802, 9803] {
        db::query("UPDATE gonof_common_member SET `freeze`=1 WHERE uid=?")
            .bind(uid)
            .execute(f.db().executor())
            .await
            .unwrap();
    }
    let page = f.get("/data/access", None, false).await;
    let csrf = page
        .headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|cookie| cookie.strip_prefix("gonof_csrf__port58833="))
        .and_then(|rest| rest.split(';').next())
        .expect("an anonymous page must issue the CSRF cookie")
        .to_owned();
    let request = |email: &str| {
        let form = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("_csrf", &csrf)
            .append_pair("email", email)
            .finish();
        Request::builder()
            .method("POST")
            .uri("/data/access")
            .header(header::COOKIE, format!("gonof_csrf__port58833={csrf}"))
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(header::ORIGIN, ORIGIN)
            .header(header::HOST, gono_forum_web::test_support::host_of(ORIGIN))
            .body(Body::from(form))
            .unwrap()
    };
    let fixture = &f;
    let mails = |uid: i64| async move {
        fixture
            .scalar(&format!(
                "SELECT COUNT(*) FROM gonof_common_outbox WHERE event_type='privacy_access' AND aggregate_id={uid}"
            ))
            .await
    };
    // The same account gets only one ticket within 60 seconds; move the issued one back in time to look at the hourly
    // limit per email on its own.
    let age_tickets = || async {
        db::query("UPDATE gonof_common_member_token SET dateline=dateline-120 WHERE purpose='privacy_access'")
            .execute(f.db().executor())
            .await
            .unwrap();
    };
    for (round, email) in [
        "route-9803@example.test",
        "ROUTE-9803@EXAMPLE.TEST",
        // "route" in full-width Latin letters.
        "\u{ff52}\u{ff4f}\u{ff55}\u{ff54}\u{ff45}-9803@example.test",
        "route-9803@example.test",
    ]
    .into_iter()
    .enumerate()
    {
        let reply = f.send(request(email)).await;
        assert_eq!(
            reply.status,
            StatusCode::SEE_OTHER,
            "{round}: {}",
            reply.text()
        );
        assert_eq!(reply.location(), "/data/access?sent=true", "{round}");
        assert_eq!(
            mails(9803).await,
            (round as i64 + 1).min(3),
            "request {}",
            round + 1
        );
        age_tickets().await;
    }
    // Other emails get their emails as usual.
    let reply = f.send(request("route-9802@example.test")).await;
    assert_eq!(reply.location(), "/data/access?sent=true");
    assert_eq!(mails(9802).await, 1);
    f.services.cleanup().await.unwrap();
}

/// Read-only mode only blocks public content: posting, changing the public profile,
/// avatars and private messages are rejected; actions on the account itself, such as changing profile visibility,
/// resending the verification email, generating and disabling API tokens, and logging out, work as usual.
#[tokio::test]
async fn read_only_mode_blocks_public_content_but_not_account_operations() {
    let f = Fixture::new(|_| {}).await;
    // Before read-only mode, upload an attachment and issue an invite code: during read-only mode they can be cancelled
    // and revoked, but no new ones can be uploaded or issued.
    let pending = f.upload(&f.member, "keep.png", &png(4)).await;
    let pending_aid = attachment_id(&pending.text());
    let identity = Identity::new(
        f.db().clone(),
        f.config.identity.clone(),
        f.services.root.clone(),
        gono_forum_web::test_support::detached_global(),
    );
    let invite = identity
        .generate_invite_for_session(&f.admin.session_token, "read-only-revoke")
        .await
        .unwrap();
    db::query("UPDATE gonof_common_member SET emailstatus=0 WHERE uid=9803")
        .execute(f.db().executor())
        .await
        .unwrap();
    let unverified = http_auth::login(&identity, f.db(), 9803).await;
    db::query(
        "UPDATE gonof_common_settings SET svalue='true' WHERE skey='settings.site.read_only'",
    )
    .execute(f.db().executor())
    .await
    .unwrap();
    let member_text = |column: &'static str| {
        let db = f.db().clone();
        async move {
            db::query_scalar::<Option<String>>(&format!(
                "SELECT {column} FROM gonof_common_member WHERE uid=9802"
            ))
            .fetch_one(db.executor())
            .await
            .unwrap()
            .unwrap_or_default()
        }
    };

    // Allowed: profile visibility.
    let saved = f
        .post(
            "/my/profile/privacy",
            &f.member,
            &[("space_visibility", "members".into())],
        )
        .await;
    assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.text());
    assert_eq!(member_text("space_visibility").await, "members");
    // Allowed: resending the verification email.
    let before = f
        .scalar("SELECT COUNT(*) FROM gonof_common_outbox WHERE event_type='email_verification' AND aggregate_id=9803")
        .await;
    let resent = f.post("/verify-email/resend", &unverified, &[]).await;
    assert_eq!(resent.status, StatusCode::SEE_OTHER, "{}", resent.text());
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_outbox WHERE event_type='email_verification' AND aggregate_id=9803")
            .await,
        before + 1
    );
    // Allowed: generating and disabling API tokens.
    let enabled = f
        .post(
            "/my/api/enable",
            &f.member,
            &[("current_password", PASSWORD.into())],
        )
        .await;
    // A successful generation is a 303 back to the settings page showing it once (one-time reveal cookie), without
    // rendering the plaintext directly.
    assert_eq!(enabled.status, StatusCode::SEE_OTHER, "{}", enabled.text());
    assert_eq!(
        f.scalar(
            "SELECT COUNT(*) FROM gonof_common_member_api_key WHERE uid=9802 AND revoked_at IS NULL"
        )
        .await,
        1
    );
    let disabled = f
        .post(
            "/my/api/disable",
            &f.member,
            &[("current_password", PASSWORD.into())],
        )
        .await;
    assert!(
        disabled.status.is_success() || disabled.status.is_redirection(),
        "{}",
        disabled.text()
    );
    assert_eq!(
        f.scalar(
            "SELECT COUNT(*) FROM gonof_common_member_api_key WHERE uid=9802 AND revoked_at IS NULL"
        )
        .await,
        0
    );

    // Allowed: check-in (only touches the member's own credits) and cancelling the member's unreferenced attachments.
    // Check-in rules are off by default, so turn them on first.
    db::query("UPDATE gonof_common_credit_rule SET enabled=1,amount=1,daily_limit=1 WHERE rule_key='signin'")
        .execute(f.db().executor())
        .await
        .unwrap();
    let signed = f.post("/api/check-in", &f.member, &[]).await;
    assert_eq!(signed.status, StatusCode::SEE_OTHER, "{}", signed.text());
    assert_eq!(signed.location(), "/my/credits?done=signin");
    let cancelled = f
        .post(&format!("/api/media/{pending_aid}/cancel"), &f.member, &[])
        .await;
    assert_eq!(cancelled.status, StatusCode::OK, "{}", cancelled.text());
    assert_eq!(f.lifecycle(pending_aid).await, "purging");
    // Allowed: revoking invite codes; blocked: generating new ones.
    let revoked = f
        .post(&format!("/my/invites/{}/revoke", invite.id), &f.admin, &[])
        .await;
    assert_eq!(revoked.status, StatusCode::SEE_OTHER, "{}", revoked.text());
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_invite WHERE id={} AND status='available'",
            invite.id
        ))
        .await,
        0
    );
    assert!(
        identity
            .generate_invite_for_session(&f.admin.session_token, "read-only-create")
            .await
            .is_err()
    );
    // Blocked: uploading new attachments.
    let uploads = f
        .scalar("SELECT COUNT(*) FROM gonof_forum_attachment")
        .await;
    let refused = f.upload(&f.member, "blocked.png", &png(4)).await;
    assert_ne!(refused.status, StatusCode::OK, "{}", refused.text());
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_forum_attachment")
            .await,
        uploads
    );

    // Blocked: the public profile (username, bio).
    let bio = member_text("bio").await;
    let refused = f
        .post(
            "/my/profile",
            &f.member,
            &[
                ("username", "route-9802".into()),
                ("bio", "Bio changed while read-only".into()),
            ],
        )
        .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.text());
    assert_eq!(member_text("bio").await, bio);
    // Blocked: posting and private messages.
    let fid = forum_of(f.db(), 103).await.0;
    let threads = f.scalar("SELECT COUNT(*) FROM gonof_forum_thread").await;
    let posted = f
        .post(
            "/new",
            &f.member,
            &[
                ("request_key", identity::random_token()),
                ("fid", fid.to_string()),
                ("title", "Thread while read-only".into()),
                ("body", "Must not be written".into()),
            ],
        )
        .await;
    assert_ne!(posted.status, StatusCode::SEE_OTHER, "{}", posted.text());
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_forum_thread").await,
        threads
    );

    // Logging out works as usual.
    let logout = f.post("/logout", &f.member, &[]).await;
    assert_eq!(logout.status, StatusCode::SEE_OTHER);
    f.services.cleanup().await.unwrap();
}
