//! Member center group: attachments moving in and out of the trash, expiry cleanup, restore and
//! quota; profile statistics and recent activity respect personal space privacy and thread visibility, last online
//! is shown by day; the contact page reuses the policy pages' admin editing.
//!
//! Attachments are uploaded through `/api/media`, threads are posted through the web form, and delete and restore
//! go through native form submissions on `/my/attachments`, without building state by editing the database; only
//! "30 days have passed" and "online a few days ago" are simulated by editing the database.
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
    content,
    db::{self, Database},
    identity::{self, Identity, SessionGrant},
    media::{Media, MediaError, StorageInput},
    router,
    test_support::TestFixture,
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const ORIGIN: &str = "http://127.0.0.1:58833";
const BOUNDARY: &str = "gonof-member-center";
const ADMIN: i64 = 9831;
const READER: i64 = 9832;
const AUTHOR: i64 = 9833;

struct Fixture {
    services: TestFixture,
    app: Router,
    media: Media,
    /// In-process S3 stub; must not be dropped before the fixture ends.
    s3: FakeS3,
    orphan_seconds: i64,
    trash_seconds: i64,
    admin: SessionGrant,
    reader: SessionGrant,
    author: SessionGrant,
    fid: i64,
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
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
        for (uid, adminid) in [(ADMIN, 1), (READER, 0), (AUTHOR, 0)] {
            db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(?,?,'synthetic-hash',?,1,?,1)")
                .bind(uid).bind(format!("center-{uid}")).bind(format!("center-{uid}@example.test")).bind(adminid)
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
                    bucket: "member-center".into(),
                    prefix: "center/".into(),
                    path_style: true,
                    access_key: "test-only-key".into(),
                    secret_key: "test-only-secret".into(),
                },
            )
            .await
            .unwrap();
        let fid = db::query_scalar("SELECT fid FROM gonof_forum_forum WHERE visibility='public' AND status=1 AND parent_fid IS NULL ORDER BY fid LIMIT 1")
            .fetch_one(db.executor())
            .await
            .unwrap();
        let app = router::build_with_global(
            config.clone(),
            &services.root,
            db.clone(),
            services.cache.clone(),
            services.global.clone(),
        );
        Self {
            services,
            app,
            media,
            s3,
            orphan_seconds: config.media.orphan_seconds,
            trash_seconds: config.media.trash_seconds,
            admin,
            reader,
            author,
            fid,
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

    async fn optional(&self, sql: &str) -> Option<i64> {
        db::query_scalar(sql)
            .fetch_one(self.db().executor())
            .await
            .unwrap()
    }

    /// Edits the database directly. Pages read data such as settings, documents and friend links from the in-process
    /// global cache, so clear this process's cache afterwards, equivalent to "takes effect in this process right
    /// after saving".
    async fn execute(&self, sql: &str) {
        db::query(sql).execute(self.db().executor()).await.unwrap();
        self.services.global.clear_local();
    }

    async fn send(&self, mut request: Request<Body>) -> Reply {
        request.extensions_mut().insert(ConnectInfo(
            "192.0.2.46:48322".parse::<SocketAddr>().unwrap(),
        ));
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        Reply {
            status,
            headers,
            body: String::from_utf8_lossy(&body).into_owned(),
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

    /// Uploads an image through `/api/media`, runs the checks to completion and returns the AID.
    async fn upload(&self, session: &SessionGrant, name: &str, side: u32) -> i64 {
        let mut body = Vec::new();
        let key = identity::random_token();
        for (field, value) in [
            ("_csrf", session.csrf_token.as_str()),
            ("request_key", key.as_str()),
        ] {
            body.extend(
                format!(
                    "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{field}\"\r\n\r\n{value}\r\n"
                )
                .as_bytes(),
            );
        }
        body.extend(
            format!(
                "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend(png(side));
        body.extend(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
        let reply = self
            .send(
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
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
        let aid: i64 = db::query_scalar("SELECT d.aid FROM gonof_forum_attachment_0 d JOIN gonof_forum_attachment a ON a.aid=d.aid WHERE a.uid=? AND d.filename=?")
            .bind(session.member.uid)
            .bind(name)
            .fetch_one(self.db().executor())
            .await
            .unwrap();
        self.media.process_pending(identity::now()).await.unwrap();
        assert_eq!(self.lifecycle(aid).await, "ready");
        aid
    }

    async fn lifecycle(&self, aid: i64) -> String {
        db::query_scalar("SELECT lifecycle FROM gonof_forum_attachment_0 WHERE aid=?")
            .bind(aid)
            .fetch_one(self.db().executor())
            .await
            .unwrap()
    }

    /// The author posts a thread through the web form; returns the response, which is 303 on success.
    async fn publish(
        &self,
        session: &SessionGrant,
        title: &str,
        body: &str,
        visibility: &str,
    ) -> Reply {
        self.post(
            "/new",
            session,
            &[
                ("request_key", identity::random_token()),
                ("fid", self.fid.to_string()),
                ("title", title.into()),
                ("body", body.into()),
                ("category", "0".into()),
                ("visibility", visibility.into()),
                ("attachment_ids", String::new()),
            ],
        )
        .await
    }

    async fn tid(&self, title: &str) -> i64 {
        db::query_scalar("SELECT tid FROM gonof_forum_thread WHERE subject=?")
            .bind(title)
            .fetch_one(self.db().executor())
            .await
            .unwrap()
    }

    async fn library_action(
        &self,
        session: &SessionGrant,
        aid: i64,
        action: &str,
        from: &str,
    ) -> Reply {
        self.post(
            &format!("/my/attachments/{aid}/{action}"),
            session,
            &[("from", from.into())],
        )
        .await
    }
}

fn png(side: u32) -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(side, side, image::Rgba([200, 60, 90, 255]));
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    bytes.into_inner()
}

/// Moving to the trash, restore, expiry cleanup and permanent deletion; attachments in use cannot be deleted;
/// those in the trash cannot be referenced.
#[tokio::test]
async fn attachments_move_through_the_trash_and_expire() {
    let f = Fixture::new().await;
    let unused = f.upload(&f.author, "unused.png", 8).await;
    let used = f.upload(&f.author, "used.png", 9).await;
    let published = f
        .publish(
            &f.author,
            "CENTERUSE5101 Thread that uses an attachment",
            &format!("![image](attachment:{used})"),
            "public",
        )
        .await;
    assert_eq!(
        published.status,
        StatusCode::SEE_OTHER,
        "{}",
        published.body
    );

    // List: both are there, one in use and one unused; anonymous visitors are redirected to login.
    let guest = f.get("/my/attachments", None).await;
    assert!(guest.status.is_redirection());
    assert!(guest.location().starts_with("/login?return_to="));
    let page = f.get("/my/attachments", Some(&f.author)).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(
        page.body
            .contains(&format!("data-attachment-row=\"{unused}\""))
    );
    assert!(
        page.body
            .contains(&format!("data-attachment-row=\"{used}\""))
    );
    assert!(page.body.contains("In use: 1 post"));
    assert!(page.body.contains("Unused, "));
    assert!(page.body.contains("data-usage-files=\"2\""));
    assert!(page.body.contains("upload quota: "));

    // An attachment in use cannot be deleted: 409 stays on the list page explaining why, and the database is unchanged.
    let refused = f.library_action(&f.author, used, "trash", "").await;
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert!(
        refused.body.contains("Edit the post to remove it"),
        "{}",
        refused.body
    );
    assert_eq!(
        f.optional(&format!(
            "SELECT trashed_at FROM gonof_forum_attachment_0 WHERE aid={used}"
        ))
        .await,
        None
    );
    // Someone else's attachment is treated as nonexistent.
    let foreign = f.library_action(&f.reader, unused, "trash", "").await;
    assert_eq!(foreign.status, StatusCode::NOT_FOUND);

    // Move to the trash: lifecycle is unchanged, expiry time = deletion time + retention period.
    let before = f
        .media
        .usage(
            f.author.member.actor(),
            gono_forum_web::test_support::SEEDED_OFFSET_MINUTES,
        )
        .await
        .unwrap();
    let trashed = f.library_action(&f.author, unused, "trash", "").await;
    assert_eq!(trashed.status, StatusCode::SEE_OTHER, "{}", trashed.body);
    assert_eq!(trashed.location(), "/my/attachments");
    let row = db::query(
        "SELECT lifecycle,trashed_at,purge_after FROM gonof_forum_attachment_0 WHERE aid=?",
    )
    .bind(unused)
    .fetch_one(f.db().executor())
    .await
    .unwrap();
    assert_eq!(db::text(&row, "lifecycle").unwrap(), "ready");
    let at: i64 = row
        .try_get::<Option<i64>, _>("trashed_at")
        .unwrap()
        .unwrap();
    assert_eq!(
        row.try_get::<Option<i64>, _>("purge_after").unwrap(),
        Some(at + f.trash_seconds)
    );
    // A repeated submission does nothing.
    let again = f.library_action(&f.author, unused, "trash", "").await;
    assert_eq!(again.status, StatusCode::SEE_OTHER);
    let kept = f.get("/my/attachments", Some(&f.author)).await.body;
    assert!(!kept.contains(&format!("data-attachment-row=\"{unused}\"")));
    // While in the trash the object stays in storage as is, so restoring needs no re-upload or re-check.
    let key: String =
        db::query_scalar("SELECT attachment FROM gonof_forum_attachment_0 WHERE aid=?")
            .bind(unused)
            .fetch_one(f.db().executor())
            .await
            .unwrap();
    assert!(!f.s3.object_ending_with(&key).is_empty());
    let trash = f.get("/my/attachments/trash", Some(&f.author)).await.body;
    assert!(trash.contains(&format!("data-attachment-row=\"{unused}\"")));
    assert!(trash.contains("In the trash"));
    // Quota: the trash is listed separately and the total is unchanged; the day's upload quota is not refunded.
    let after = f
        .media
        .usage(
            f.author.member.actor(),
            gono_forum_web::test_support::SEEDED_OFFSET_MINUTES,
        )
        .await
        .unwrap();
    assert_eq!(after.trash.files, 1);
    assert_eq!(after.unused.files, before.unused.files - 1);
    assert_eq!(after.stored().files, before.stored().files);
    assert_eq!(after.stored().bytes, before.stored().bytes);
    assert_eq!(after.daily_used, before.daily_used);
    assert!(after.daily_used > 0);

    // Attachments in the trash cannot be referenced by posts or avatars.
    let blocked = f
        .publish(
            &f.author,
            "CENTERTRASH5102 Referencing a trashed attachment",
            &format!("![image](attachment:{unused})"),
            "public",
        )
        .await;
    assert_eq!(
        blocked.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        blocked.body
    );
    assert!(
        blocked
            .body
            .contains("The attachment is in the trash. Restore it first")
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_forum_attachment_reference WHERE aid={unused}"
        ))
        .await,
        0
    );
    match f
        .media
        .set_avatar(f.author.member.actor(), Some(unused))
        .await
    {
        Err(MediaError::Validation(message)) => {
            assert!(message.contains("in the trash"), "{message}")
        }
        other => panic!("an image in the trash must not become an avatar: {other:?}"),
    }

    // Restore: back to unused, the unused period restarts, and it is not re-checked.
    let restored = f
        .library_action(&f.author, unused, "restore", "trash")
        .await;
    assert_eq!(restored.status, StatusCode::SEE_OTHER, "{}", restored.body);
    assert_eq!(restored.location(), "/my/attachments/trash");
    let row = db::query("SELECT lifecycle,scan_status,trashed_at,purge_after FROM gonof_forum_attachment_0 WHERE aid=?")
        .bind(unused)
        .fetch_one(f.db().executor())
        .await
        .unwrap();
    assert_eq!(db::text(&row, "lifecycle").unwrap(), "ready");
    assert_eq!(db::text(&row, "scan_status").unwrap(), "clean");
    assert_eq!(row.try_get::<Option<i64>, _>("trashed_at").unwrap(), None);
    let expiry = row
        .try_get::<Option<i64>, _>("purge_after")
        .unwrap()
        .unwrap();
    assert!((expiry - (identity::now() + f.orphan_seconds)).abs() <= 5);
    assert_eq!(
        f.media
            .usage(
                f.author.member.actor(),
                gono_forum_web::test_support::SEEDED_OFFSET_MINUTES
            )
            .await
            .unwrap()
            .daily_used,
        before.daily_used
    );
    // After restoring it can be referenced normally.
    let reused = f
        .publish(
            &f.author,
            "CENTERREUSE5103 Referencing it again after restoring",
            &format!("![image](attachment:{unused})"),
            "public",
        )
        .await;
    assert_eq!(reused.status, StatusCode::SEE_OTHER, "{}", reused.body);

    // Expiry cleanup: upload another one and move it to the trash, move time past the retention period, and the
    // existing cleanup task takes over by purge_after.
    let expiring = f.upload(&f.author, "expiring.png", 10).await;
    assert_eq!(
        f.library_action(&f.author, expiring, "trash", "")
            .await
            .status,
        StatusCode::SEE_OTHER
    );
    f.media
        .cleanup(identity::now() + f.trash_seconds - 3600)
        .await
        .unwrap();
    assert_eq!(
        f.lifecycle(expiring).await,
        "ready",
        "nothing is cleaned up before it expires"
    );
    f.media
        .cleanup(identity::now() + f.trash_seconds + 60)
        .await
        .unwrap();
    assert_ne!(f.lifecycle(expiring).await, "ready");
    let late = f
        .library_action(&f.author, expiring, "restore", "trash")
        .await;
    assert_eq!(late.status, StatusCode::CONFLICT);
    assert!(late.body.contains("past the trash retention period"));
    let trash = f.get("/my/attachments/trash", Some(&f.author)).await.body;
    assert!(!trash.contains(&format!("data-attachment-row=\"{expiring}\"")));

    // One that has expired but not yet been reached by the cleanup cannot be restored either.
    let stale = f.upload(&f.author, "stale.png", 11).await;
    f.library_action(&f.author, stale, "trash", "").await;
    // The check script requires purge_after not to be earlier than the upload time; the upload time already
    // means "expired".
    f.execute(&format!(
        "UPDATE gonof_forum_attachment_0 SET purge_after=dateline WHERE aid={stale}"
    ))
    .await;
    let late = f.library_action(&f.author, stale, "restore", "trash").await;
    assert_eq!(late.status, StatusCode::CONFLICT);

    // Permanent deletion: goes into cleanup immediately and is no longer listed.
    let gone = f.upload(&f.author, "gone.png", 12).await;
    f.library_action(&f.author, gone, "trash", "").await;
    let deleted = f.library_action(&f.author, gone, "delete", "trash").await;
    assert_eq!(deleted.status, StatusCode::SEE_OTHER, "{}", deleted.body);
    assert_eq!(deleted.location(), "/my/attachments/trash");
    assert_eq!(f.lifecycle(gone).await, "purging");
    f.services.cleanup().await.unwrap();
}

/// Profile statistics, recent activity and last online: respect thread visibility and personal space privacy.
#[tokio::test]
async fn member_space_stats_activity_and_presence_follow_visibility() {
    let f = Fixture::new().await;
    for (title, visibility) in [
        ("CENTERPUBLIC5201 Public thread", "public"),
        ("CENTERMEMBERS5202 Members-only thread", "members"),
    ] {
        let reply = f
            .publish(&f.author, title, "Body for the statistics", visibility)
            .await;
        assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    }
    let public = f.tid("CENTERPUBLIC5201 Public thread").await;
    let members = f.tid("CENTERMEMBERS5202 Members-only thread").await;
    for tid in [public, members] {
        let reply = f
            .post(
                &format!("/api/thread-{tid}/comments"),
                &f.author,
                &[
                    ("request_key", identity::random_token()),
                    ("body", format!("CENTERREPLY{tid} Reply by the author")),
                ],
            )
            .await;
        assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    }
    f.execute(&format!(
        "UPDATE gonof_forum_thread SET digest=1 WHERE tid IN ({public},{members})"
    ))
    .await;
    // The highlight count in the member counts: after highlighting with raw SQL, recompute by the real rules.
    gono_forum_web::test_support::refresh_listing(f.db())
        .await
        .unwrap();
    let stat = |body: &str, name: &str| -> String {
        let marker = format!("data-stat=\"{name}\">");
        let start = body
            .find(&marker)
            .unwrap_or_else(|| panic!("missing {name}: {body}"))
            + marker.len();
        body[start..start + body[start..].find('<').unwrap()].to_owned()
    };

    // Anonymous: the statistics read the member count table, the same number for every viewer; members-only threads
    // and the replies in them are still not listed.
    let space = format!("/space/{AUTHOR}");
    let guest = f.get(&space, None).await;
    assert_eq!(guest.status, StatusCode::OK);
    assert_eq!(stat(&guest.body, "threads"), "2");
    assert_eq!(stat(&guest.body, "replies"), "2");
    assert_eq!(stat(&guest.body, "digests"), "2");
    assert!(!guest.body.contains("CENTERMEMBERS5202"));
    assert!(!guest.body.contains(&format!("CENTERREPLY{members}")));
    assert!(guest.body.contains("data-member-action>Posted</span>"));
    assert!(guest.body.contains("data-member-action>Replied to</span>"));
    assert!(guest.body.contains(">Recent activity</a>"));
    assert!(
        !guest.body.contains("data-credits-entry"),
        "other people do not see the points entry"
    );
    // Tab totals use the same basis as the statistics (entries the list cannot read are not shown, so the last page
    // may not be full).
    let threads = f.get(&format!("{space}/threads"), None).await.body;
    assert!(threads.contains("data-member-total=\"2\""), "{threads}");
    assert!(!threads.contains("CENTERMEMBERS5202"));
    // Member: everything is counted.
    let member = f.get(&space, Some(&f.reader)).await.body;
    assert_eq!(stat(&member, "threads"), "2");
    assert_eq!(stat(&member, "replies"), "2");
    assert_eq!(stat(&member, "digests"), "2");
    assert!(member.contains("CENTERMEMBERS5202"));
    let threads = f
        .get(&format!("{space}/threads"), Some(&f.reader))
        .await
        .body;
    assert!(threads.contains("data-member-total=\"2\""));
    // The member's own profile links to their points.
    let own = f.get(&space, Some(&f.author)).await.body;
    assert!(own.contains("data-credits-entry"));
    assert!(own.contains("href=\"/my/credits\""));

    // The author just visited (the request with a session above wrote the online table), so "Online" is shown. Last
    // online is only written back when expired sessions are cleaned up
    // (docs/queries.md, "Last online"), so the author is not in the last-online table yet.
    assert!(
        f.optional(&format!(
            "SELECT MAX(lastactivity) FROM gonof_common_session WHERE uid={AUTHOR}"
        ))
        .await
        .is_some()
    );
    assert!(
        f.optional(&format!(
            "SELECT MAX(last_seen) FROM gonof_common_member_presence WHERE uid={AUTHOR}"
        ))
        .await
        .is_none()
    );
    let online = f.get(&space, None).await.body;
    assert!(
        online.contains("<span data-presence>Online</span>"),
        "{online}"
    );
    // Online three days ago: after the session expires the cleanup task writes the last activity time back to the
    // last-online table and deletes the session; the page only shows the day, not the time.
    let three_days_ago = identity::now() - 3 * 86400;
    f.execute(&format!(
        "UPDATE gonof_common_session SET lastactivity={three_days_ago} WHERE uid={AUTHOR}"
    ))
    .await;
    Identity::new(
        f.db().clone(),
        f.services.config.identity.clone(),
        f.services.root.clone(),
        gono_forum_web::test_support::detached_global(),
    )
    .cleanup_online()
    .await
    .unwrap();
    assert_eq!(
        f.optional(&format!(
            "SELECT MAX(last_seen) FROM gonof_common_member_presence WHERE uid={AUTHOR}"
        ))
        .await,
        Some(three_days_ago)
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_session WHERE uid={AUTHOR}"
        ))
        .await,
        0
    );
    let seen = f.get(&space, None).await.body;
    assert!(
        seen.contains("<span data-presence>Last seen 3 days ago</span>"),
        "{seen}"
    );
    // A member who has never logged in has no record, and no online status is shown.
    f.execute("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,regdate) VALUES(9834,'center-9834','synthetic-hash','center-9834@example.test',1,1)").await;
    let quiet = f.get("/space/9834", None).await;
    assert_eq!(quiet.status, StatusCode::OK);
    assert!(!quiet.body.contains("data-presence"));

    // The viewer has blocked the author: the statistics are all 0, matching the tab totals.
    let block = f
        .post(
            &format!("/api/users/{AUTHOR}/block"),
            &f.reader,
            &[
                ("request_key", identity::random_token()),
                ("desired", "true".into()),
                ("location", space.clone()),
            ],
        )
        .await;
    assert!(
        block.status.is_redirection() || block.status.is_success(),
        "{}",
        block.body
    );
    let blocked = f.get(&space, Some(&f.reader)).await.body;
    assert_eq!(stat(&blocked, "threads"), "0");
    assert_eq!(stat(&blocked, "replies"), "0");

    // Personal space visible only to the member: the gate comes before the statistics and online status, so neither
    // anonymous visitors nor others see them.
    f.execute(&format!(
        "UPDATE gonof_common_member SET space_visibility='private' WHERE uid={AUTHOR}"
    ))
    .await;
    for session in [None, Some(&f.admin)] {
        let denied = f.get(&space, session).await;
        assert_eq!(denied.status, StatusCode::FORBIDDEN);
        assert!(!denied.body.contains("data-member-stats"));
        assert!(!denied.body.contains("data-presence"));
        assert!(!denied.body.contains("Last seen"));
    }
    // Logged-in users only: anonymous visitors are redirected to login and no statistics are output.
    f.execute(&format!(
        "UPDATE gonof_common_member SET space_visibility='members' WHERE uid={AUTHOR}"
    ))
    .await;
    let redirected = f.get(&space, None).await;
    assert!(redirected.status.is_redirection());
    assert!(!redirected.body.contains("data-member-stats"));
    f.services.cleanup().await.unwrap();
}

/// The contact page reuses the policy pages: the body is edited in the admin area, the contact e-mail and on-site
/// entry are appended below the body, and the footer links to it.
#[tokio::test]
async fn contact_page_is_edited_like_the_other_policies() {
    let f = Fixture::new().await;
    let page = f.get("/contact", None).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("<h1>Contact</h1>"));
    assert!(page.body.contains("data-contact-channels"));
    assert!(page.body.contains("href=\"/appeals\""));
    assert!(page.body.contains("href=\"/data/access\""));
    assert!(
        !page.body.contains("data-contact-email"),
        "no contact email is shown while none is set"
    );
    assert!(
        page.body.contains("<a href=\"/contact\">Contact</a>"),
        "the footer links to the contact page"
    );

    // Non-administrators cannot open the admin edit page.
    assert_ne!(
        f.get("/manage/policies/contact", Some(&f.reader))
            .await
            .status,
        StatusCode::OK
    );
    let editor = f.get("/manage/policies/contact", Some(&f.admin)).await;
    assert_eq!(editor.status, StatusCode::OK, "{}", editor.body);
    let saved = f
        .post(
            "/manage/policies/contact",
            &f.admin,
            &[
                ("title", "Contact the staff".into()),
                ("body", "CENTERCONTACT5301 We reply on weekdays".into()),
            ],
        )
        .await;
    assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
    assert_eq!(saved.location(), "/manage/policies/contact");
    f.execute(
        "UPDATE gonof_common_settings SET svalue='staff@example.test' WHERE skey='settings.site.contact_email'",
    )
    .await;
    let page = f.get("/contact", None).await.body;
    assert!(page.contains("<h1>Contact the staff</h1>"));
    assert!(page.contains("CENTERCONTACT5301 We reply on weekdays"));
    assert!(page.contains("Last updated: "));
    assert!(page.contains("href=\"mailto:staff@example.test\""));
    // Other policy pages do not append contact channels.
    assert!(
        !f.get("/terms", None)
            .await
            .body
            .contains("data-contact-channels")
    );
    f.services.cleanup().await.unwrap();
}
