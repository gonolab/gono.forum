#[path = "support/http_auth.rs"]
mod http_auth;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use gono_forum_web::{
    config::Config,
    content::{
        self,
        writes::{ContentWriter, NewThread, WriteConfig},
    },
    db::Database,
    identity, manage, router,
};
use http_body_util::BodyExt;
use std::path::PathBuf;
use tower::ServiceExt;

struct Fixture {
    _services: gono_forum_web::test_support::TestFixture,
    root: PathBuf,
    db: Database,
    app: Router,
    admin: String,
    moderator: String,
    member: String,
    csrf_tokens: std::collections::BTreeMap<String, String>,
    pids: Vec<i64>,
    forums: Vec<i64>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
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
        let forums: Vec<i64> =
            gono_forum_web::db::query_scalar("SELECT fid FROM gonof_forum_forum ORDER BY fid")
                .fetch_all(db.executor())
                .await
                .unwrap();
        for (uid, role) in [(1, 1), (2, 3), (3, 0)] {
            gono_forum_web::db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(?,?,'password-hash',?,1,?,1)").bind(uid).bind(format!("manager-{uid}")).bind(format!("manager-{uid}@example.test")).bind(role).execute(db.executor()).await.unwrap();
        }
        gono_forum_web::db::query("INSERT INTO gonof_forum_moderator(uid,fid) VALUES(2,?)")
            .bind(forums[0])
            .execute(db.executor())
            .await
            .unwrap();
        let identity = identity::Identity::new(
            db.clone(),
            config.identity.clone(),
            root.clone(),
            gono_forum_web::test_support::detached_global(),
        );
        let mut tokens = Vec::new();
        let mut csrf_tokens = std::collections::BTreeMap::new();
        let now = identity::now();
        for uid in [1, 2, 3] {
            let grant = http_auth::login(&identity, &db, uid).await;
            csrf_tokens.insert(grant.session_token.clone(), grant.csrf_token);
            tokens.push(grant.session_token);
        }
        let writer = ContentWriter::new(db.clone(), WriteConfig::default());
        let mut pids = Vec::new();
        for (fid, title) in [(forums[0], "Scope Alpha"), (forums[1], "Scope Beta")] {
            pids.push(
                writer
                    .publish(
                        1,
                        NewThread {
                            visibility: Default::default(),
                            activity: Default::default(),
                            fid,
                            title: title.into(),
                            body: "<script>raw-current-secret</script>".into(),
                            attachment_ids: vec![],
                            request_key: identity::random_token(),
                        },
                    )
                    .await
                    .unwrap()
                    .pid,
            );
        }
        gono_forum_web::db::query("INSERT INTO gonof_forum_post_revision(pid,version,editor_uid,subject,message,dateline) VALUES(?,7,1,'Old title','<script>raw-revision-secret</script>',?)").bind(pids[0]).bind(now).execute(db.executor()).await.unwrap();
        let app = router::build(config, &root, db.clone());
        Self {
            _services: services,
            root,
            db,
            app,
            admin: tokens[0].clone(),
            moderator: tokens[1].clone(),
            member: tokens[2].clone(),
            csrf_tokens,
            pids,
            forums,
        }
    }
    async fn call(
        &self,
        method: &str,
        path: &str,
        session: Option<&str>,
        fields: &[(String, String)],
    ) -> (StatusCode, String, String) {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(raw) = session {
            request = request.header(header::COOKIE, format!("gonof_session={raw}"));
        }
        if method == "POST" {
            request = request
                .header(header::ORIGIN, "http://127.0.0.1:58833")
                .header(
                    header::HOST,
                    gono_forum_web::test_support::host_of("http://127.0.0.1:58833"),
                )
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
        assert!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .is_some_and(|value| value.to_str().unwrap().contains("no-store"))
        );
        let location = response
            .headers()
            .get(header::LOCATION)
            .map(|value| value.to_str().unwrap().to_owned())
            .unwrap_or_default();
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
        (status, html, location)
    }
    fn action(&self, raw: &str, action: &str) -> Vec<(String, String)> {
        [
            ("_csrf", self.csrf_tokens[raw].clone()),
            ("expected_post_version", "1".into()),
            ("expected_thread_version", "1".into()),
            ("request_key", identity::random_token()),
            ("reason", "Test handling of a rule violation".into()),
            ("action", action.into()),
        ]
        .into_iter()
        .map(|(name, value)| (name.into(), value))
        .collect()
    }
}

#[tokio::test]
async fn management_pages_scope_raw_revisions_and_mutations() {
    let f = Fixture::new().await;
    assert_eq!(
        f.call("GET", "/manage/content", None, &[]).await.0,
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        f.call("GET", "/manage/content", Some(&f.member), &[])
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (status, html, _) = f
        .call(
            "GET",
            "/manage/content?fid=&kind=&state=&q=",
            Some(&f.moderator),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("Scope Alpha"));
    assert!(!html.contains("Scope Beta"));
    // The moderators tab is for administrators only.
    assert!(!html.contains("href=\"/manage/moderators\">"));
    let (status, html, _) = f
        .call(
            "GET",
            &format!("/manage/content/{}", f.pids[0]),
            Some(&f.moderator),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("raw-current-secret"));
    assert!(html.contains("raw-revision-secret"));
    assert!(!html.contains("<script>raw-"));
    assert!(!html.contains("value=\"withdraw\""));
    assert_eq!(
        f.call(
            "GET",
            &format!("/manage/content/{}", f.pids[1]),
            Some(&f.moderator),
            &[]
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        f.call("GET", "/manage/moderators", Some(&f.moderator), &[])
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let path = format!("/manage/posts/{}/actions", f.pids[0]);
    let mut fields = f.action(&f.admin, "hide");
    fields[0].1 = "invalid-csrf".into();
    assert_eq!(
        f.call("POST", &path, Some(&f.admin), &fields).await.0,
        StatusCode::FORBIDDEN
    );
    fields[0].1 = f.csrf_tokens[&f.admin].clone();
    assert_eq!(
        f.call("POST", &path, Some(&f.admin), &fields).await.0,
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        f.call("POST", &path, Some(&f.admin), &fields).await.0,
        StatusCode::SEE_OTHER
    );
    let count: i64 = gono_forum_web::db::query_scalar(
        "SELECT COUNT(*) FROM gonof_common_log WHERE type='content.hide'",
    )
    .fetch_one(f.db.executor())
    .await
    .unwrap();
    assert_eq!(count, 1);
    fields[4].1 = "A different reason with the same request key".into();
    assert_eq!(
        f.call("POST", &path, Some(&f.admin), &fields).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        f.call(
            "POST",
            &path,
            Some(&f.admin),
            &f.action(&f.admin, "withdraw")
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (status, html, _) = f.call("GET", "/manage/logs", Some(&f.moderator), &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("Test handling of a rule violation"));

    // Since in-site times were unified into one formatting entry point, hard-coded suffixes like "(UTC+8)" are gone —
    // the time zone is decided by the settings, and a hard-coded suffix would sooner or later lie.
    gono_forum_web::db::query(
        "INSERT INTO gonof_common_log(uid,operationuid,type,data,source,dateline) VALUES(1,1,'site.settings.save','{}','web',1700000000)",
    )
    .execute(f.db.executor())
    .await
    .unwrap();
    let (status, html, _) = f
        .call("GET", "/manage/logs?kind=site", Some(&f.admin), &[])
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        html.contains("2023-11-15 06:13"),
        "log times are shown in the display time zone"
    );
    assert!(!html.contains("UTC+8"), "no hard-coded time zone suffix");
    // Logs without a target object, such as site configuration, leave no empty "· ·" segment.
    assert!(
        !html.contains("·  ·"),
        "a log row must not have an empty target segment"
    );

    // Post revisions used to be rendered in UTC with a "UTC" suffix, 8 hours earlier than the other times on the same
    // page. 1700000000 in UTC is 2023-11-14 22:13; in the display time zone (UTC+8) it is 06:13 on the 15th.
    gono_forum_web::db::query("INSERT INTO gonof_forum_post_revision(pid,version,editor_uid,subject,message,dateline) VALUES(?,9,1,'Old title','Old body',1700000000)")
        .bind(f.pids[0])
        .execute(f.db.executor())
        .await
        .unwrap();
    let (status, html, _) = f
        .call(
            "GET",
            &format!("/manage/content/{}", f.pids[0]),
            Some(&f.admin),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        html.contains("2023-11-15 06:13"),
        "revisions must use the same time zone as the other times on the site"
    );
    assert!(!html.contains("2023-11-14 22:13"));
    assert!(!html.contains("UTC"), "no hard-coded time zone suffix");
    f._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn published_threads_reach_the_audit_log_under_the_name_that_is_written() {
    let f = Fixture::new().await;
    let filter = manage::LogFilter {
        kind: "content".into(),
        ..Default::default()
    };
    let (logs, total) = manage::logs_filtered(
        &f.db,
        identity::Actor {
            uid: 1,
            credentials_version: 1,
            api_key: None,
        },
        &filter,
    )
    .await
    .unwrap();
    // The fixture posted two threads. Previously content_events filtered on content.thread, which was never written,
    // and the actually written content.thread.publish was in none of the lists, so nothing at all was found here.
    assert_eq!(total, 2, "published threads must be found in the admin log");
    assert!(
        logs.iter()
            .all(|row| row.operation == "content.thread.publish")
    );
    assert_eq!(logs[0].label(), "Thread posted");
    assert!(logs[0].target.starts_with("PID "), "{}", logs[0].target);
    f._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn role_assignment_native_form_replay_conflict_and_session_revocation() {
    let f = Fixture::new().await;
    let (status, html, _) = f
        .call("GET", "/manage/moderators?uid=2", Some(&f.admin), &[])
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains(&format!("name=\"forum_{}\"", f.forums[0])));
    let fields: Vec<(String, String)> = [
        ("_csrf", f.csrf_tokens[&f.admin].clone()),
        ("target_uid", "2".into()),
        ("expected_version", "1".into()),
        ("request_key", identity::random_token()),
        ("reason", "Make them a global moderator".into()),
        ("adminid", "2".into()),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v))
    .collect();
    let (status, _, location) = f
        .call("POST", "/manage/moderators", Some(&f.admin), &fields)
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location, "/manage/moderators?uid=2");
    assert_eq!(
        f.call("POST", "/manage/moderators", Some(&f.admin), &fields)
            .await
            .0,
        StatusCode::SEE_OTHER
    );
    let count: i64 = gono_forum_web::db::query_scalar(
        "SELECT COUNT(*) FROM gonof_common_log WHERE type='identity.role.assign'",
    )
    .fetch_one(f.db.executor())
    .await
    .unwrap();
    assert_eq!(count, 1);
    assert_eq!(
        f.call("GET", "/manage/content", Some(&f.moderator), &[])
            .await
            .0,
        StatusCode::SEE_OTHER
    );
    let mut altered = fields.clone();
    altered[4].1 = "Another reason".into();
    let (status, html, _) = f
        .call("POST", "/manage/moderators", Some(&f.admin), &altered)
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(html.contains("Another reason"));
    f._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn revision_pages_are_bounded_scoped_and_escape_nullable_raw_fields() {
    let f = Fixture::new().await;
    let pid = f.pids[0];
    for version in 8..=48 {
        gono_forum_web::db::query("INSERT INTO gonof_forum_post_revision(pid,version,editor_uid,subject,message,dateline) VALUES(?,?,1,NULL,?,?)")
            .bind(pid).bind(version).bind(format!("<script>revision-{version:02}</script>"))
            .bind(identity::now()).execute(f.db.executor()).await.unwrap();
    }
    gono_forum_web::db::query("UPDATE gonof_forum_post SET authorid=NULL WHERE pid=?")
        .bind(pid)
        .execute(f.db.executor())
        .await
        .unwrap();
    let actor = identity::Actor {
        uid: 2,
        credentials_version: 1,
        api_key: None,
    };
    let (_, first) = manage::post(&f.db, actor, pid, 1).await.unwrap();
    assert_eq!(first.total, 42);
    assert_eq!(first.rows.len(), 20);
    assert_eq!(first.rows[0].version, 48);
    assert_eq!(first.rows[19].version, 29);
    assert!(
        first
            .rows
            .iter()
            .all(|revision| revision.subject.is_empty())
    );
    let (_, second) = manage::post(&f.db, actor, pid, 2).await.unwrap();
    assert_eq!(second.rows.len(), 20);
    assert_eq!(second.rows[0].version, 28);
    assert_eq!(second.rows[19].version, 9);
    let (_, last) = manage::post(&f.db, actor, pid, 3).await.unwrap();
    assert_eq!(
        last.rows
            .iter()
            .map(|revision| revision.version)
            .collect::<Vec<_>>(),
        vec![8, 7]
    );
    let path = format!("/manage/content/{pid}");
    let (status, html, _) = f.call("GET", &path, Some(&f.moderator), &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("Deleted user"));
    assert!(html.contains("revision-48") && html.contains("revision-29"));
    assert!(!html.contains("revision-28") && !html.contains("raw-revision-secret"));
    assert!(!html.contains("<script>revision-"));
    assert!(html.contains("revisions_page=2#revisions"));
    assert!(!html.contains(">Previous</a>"));
    let (_, html, _) = f
        .call(
            "GET",
            &format!("{path}?revisions_page=3"),
            Some(&f.moderator),
            &[],
        )
        .await;
    assert!(html.contains("revision-08") && html.contains("raw-revision-secret"));
    assert!(html.contains(">Previous</a>") && !html.contains(">Next</a>"));
    for page in [0, -1, i64::MAX] {
        assert_eq!(
            f.call(
                "GET",
                &format!("{path}?revisions_page={page}"),
                Some(&f.moderator),
                &[]
            )
            .await
            .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    assert_eq!(
        f.call(
            "GET",
            &format!("{path}?revisions_page=4"),
            Some(&f.moderator),
            &[]
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        f.call(
            "GET",
            &format!("/manage/content/{}?revisions_page=2", f.pids[1]),
            Some(&f.moderator),
            &[]
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        f.call("GET", &path, Some(&f.member), &[]).await.0,
        StatusCode::FORBIDDEN
    );
    let (_, empty) = manage::post(
        &f.db,
        identity::Actor {
            uid: 1,
            credentials_version: 1,
            api_key: None,
        },
        f.pids[1],
        1,
    )
    .await
    .unwrap();
    assert_eq!(empty.total, 0);
    assert!(empty.rows.is_empty());
    f._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn staff_validation_preserves_unauthorized_and_forbidden_errors() {
    let f = Fixture::new().await;
    for (uid, role) in [(1, 1), (2, 3)] {
        assert_eq!(
            manage::staff(
                &f.db,
                identity::Actor {
                    uid,
                    credentials_version: 1,
                    api_key: None,
                }
            )
            .await
            .unwrap(),
            role
        );
    }
    assert!(matches!(
        manage::staff(
            &f.db,
            identity::Actor {
                uid: 3,
                credentials_version: 1,
                api_key: None,
            }
        )
        .await,
        Err(identity::IdentityError::Forbidden)
    ));
    for (uid, version) in [(1, 2), (3, 2), (999, 1)] {
        assert!(matches!(
            manage::staff(
                &f.db,
                identity::Actor {
                    uid,
                    credentials_version: version,
                    api_key: None,
                }
            )
            .await,
            Err(identity::IdentityError::Unauthorized)
        ));
    }
    gono_forum_web::db::query("UPDATE gonof_common_member SET `freeze`=1 WHERE uid IN (1,3)")
        .execute(f.db.executor())
        .await
        .unwrap();
    for uid in [1, 3] {
        assert!(matches!(
            manage::staff(
                &f.db,
                identity::Actor {
                    uid,
                    credentials_version: 1,
                    api_key: None,
                }
            )
            .await,
            Err(identity::IdentityError::Unauthorized)
        ));
    }
    f._services.cleanup_ref().await.unwrap();
}

/// Visitors opening any admin URL go to the sign-in page and return to the original page (with query string) after
/// signing in.
#[tokio::test]
async fn guests_opening_any_manage_page_go_to_login_and_come_back() {
    let f = Fixture::new().await;
    for path in [
        "/manage",
        "/manage/logs",
        "/manage/forums?fid=1",
        "/manage/reports",
        "/manage/credits",
        "/manage/settings",
        "/manage/restorations?status=rejected",
    ] {
        let (status, _, location) = f.call("GET", path, None, &[]).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{path}");
        let back = url::form_urlencoded::byte_serialize(path.as_bytes()).collect::<String>();
        assert_eq!(location, format!("/login?return_to={back}"), "{path}");
    }
    // Signed-in ordinary members are still stopped by each page's gate, without redirecting to sign-in.
    let (status, _, _) = f.call("GET", "/manage/logs", Some(&f.member), &[]).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    f._services.cleanup_ref().await.unwrap();
}

/// Moving threads is for moderators; the entry point is not shown when the author has no forum management permission.
/// The historical tag paths have been removed.
#[tokio::test]
async fn moderators_can_move_threads_and_tag_routes_are_gone() {
    let f = Fixture::new().await;
    let tid = ContentWriter::new(f.db.clone(), WriteConfig::default())
        .publish(
            3,
            NewThread {
                fid: f.forums[0],
                title: "A thread by a member".into(),
                body: "Checks the entry point for moving threads".into(),
                visibility: Default::default(),
                activity: Default::default(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
        )
        .await
        .unwrap()
        .tid;
    let path = format!("/manage/threads/{tid}/organization");
    let link = format!("href=\"{path}\"");
    for (session, can_move) in [(&f.moderator, true), (&f.member, false)] {
        let (status, html, _) = f
            .call("GET", &format!("/thread-{tid}"), Some(session), &[])
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(html.contains(&link), can_move);
    }

    let (status, html, _) = f.call("GET", &path, Some(&f.moderator), &[]).await;
    assert_eq!(status, StatusCode::OK);
    let canonical = html
        .split_once("rel=\"canonical\" href=\"")
        .unwrap()
        .1
        .split('"')
        .next()
        .unwrap();
    assert!(canonical.ends_with(&path), "{canonical}");
    assert!(html.contains(&format!("action=\"/manage/threads/{tid}/move\"")));
    assert_eq!(
        f.call("GET", &path, Some(&f.member), &[]).await.0,
        StatusCode::FORBIDDEN
    );

    let thread_tags = format!("/threads/{tid}/tags");
    for (method, path, who) in [
        ("GET", "/tags", None),
        ("GET", "/tag-example", None),
        ("GET", thread_tags.as_str(), Some(f.member.as_str())),
        ("POST", thread_tags.as_str(), Some(f.member.as_str())),
        ("GET", "/manage/tags", Some(f.admin.as_str())),
        ("POST", "/manage/tags", Some(f.admin.as_str())),
        ("POST", "/manage/tags/1/merge", Some(f.admin.as_str())),
        (
            "POST",
            "/manage/tags/1/aliases/remove",
            Some(f.admin.as_str()),
        ),
    ] {
        assert_eq!(
            f.call(method, path, who, &[]).await.0,
            StatusCode::NOT_FOUND,
            "{method} {path}"
        );
    }
    f._services.cleanup_ref().await.unwrap();
}

/// Extracts the HTML inside a given nav element. Counting `aria-current` globally is wrong — the header navigation and
/// the type navigation in the mail template page also produce it, so slice by block before counting.
fn section<'a>(html: &'a str, class: &str) -> &'a str {
    let start = html
        .split_once(&format!("class=\"{class}\""))
        .unwrap_or_else(|| panic!("no {class} on the page"))
        .1;
    start.split_once("</nav>").expect("nav is not closed").0
}

/// Two-level admin navigation: the left modules narrow down by role, and the top tabs land on the current page.
#[tokio::test]
async fn manage_shell_renders_two_level_navigation_per_role() {
    let f = Fixture::new().await;

    // Administrators see all 10 modules.
    let (status, html, _) = f.call("GET", "/manage", Some(&f.admin), &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(html.matches("class=\"admin-menu__item\"").count(), 10);
    // Module names are matched as menu labels: English words such as "Users" or "Review" also occur elsewhere.
    let label = |title: &str| format!("<span class=\"manage-nav-label\">{title}</span>");
    for title in [
        "Workbench",
        "Global settings",
        "Appearance",
        "Review",
        "Users",
        "Forums",
        "Multiple domains",
        "Security",
        "Tools &#38; maintenance",
    ] {
        assert!(
            html.contains(&label(title)),
            "the menu lacks module {title}: {}",
            section(&html, "admin-menu")
        );
    }
    assert!(
        html.contains("manage-icon-sprite"),
        "a page with navigation carries the sprite"
    );
    assert!(
        !html.contains("manage-frame--plain"),
        "an admin page must not fall back to one column"
    );

    // One current-state mark each: one on the left, one on the tabs.
    assert_eq!(
        section(&html, "admin-menu")
            .matches("aria-current=\"page\"")
            .count(),
        1
    );
    assert_eq!(
        section(&html, "section-tabs")
            .matches("aria-current=\"page\"")
            .count(),
        1
    );

    // Moderators have only four modules left, and none of the administrator-only ones appear.
    let (status, html, _) = f.call("GET", "/manage", Some(&f.moderator), &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(html.matches("class=\"admin-menu__item\"").count(), 4);
    for title in [
        "Global settings",
        "Appearance",
        "Users",
        "Multiple domains",
        "Security",
    ] {
        assert!(
            !html.contains(&label(title)),
            "a moderator must not see module {title}"
        );
    }

    // Detail pages land on the tab of the list they belong to, rather than having no highlight.
    let pid = f.pids[0];
    for (path, tab) in [
        (format!("/manage/content/{pid}"), "/manage/content"),
        ("/manage/users/2".into(), "/manage/users"),
        (
            "/manage/registration-sources".into(),
            "/manage/registration-sources",
        ),
        ("/manage/settings".into(), "/manage/settings"),
        // Eleven kinds share one tab, landing on email_verification.
        (
            "/manage/mail/templates/content".into(),
            "/manage/mail/templates/email_verification",
        ),
    ] {
        let (status, html, _) = f.call("GET", &path, Some(&f.admin), &[]).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(
            section(&html, "section-tabs")
                .contains(&format!("href=\"{tab}\" aria-current=\"page\"")),
            "{path} should land on tab {tab}"
        );
    }
    f._services.cleanup_ref().await.unwrap();
}

/// The other half of the shell: URLs not belonging to the admin area fall back to a single column, and the two pages
/// that had no navigation get it.
#[tokio::test]
async fn manage_shell_falls_back_to_one_column_outside_the_backend() {
    let f = Fixture::new().await;
    let tid: i64 =
        gono_forum_web::db::query_scalar("SELECT first_tid FROM gonof_forum_post WHERE pid=?")
            .bind(f.pids[0])
            .fetch_one(f.db.executor())
            .await
            .unwrap();

    // The same report detail template is used in the personal center and the admin area; only the admin entry has
    // management navigation.
    let fields = vec![
        ("_csrf".into(), f.csrf_tokens[&f.member].clone()),
        ("target_type".into(), "thread".into()),
        ("target_id".into(), tid.to_string()),
        ("reason".into(), "Reported content".into()),
        ("request_key".into(), identity::random_token()),
    ];
    let (status, _, report) = f
        .call("POST", "/my/reports", Some(&f.member), &fields)
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let id = report
        .strip_prefix("/my/reports/")
        .unwrap()
        .parse::<i64>()
        .unwrap();
    for (path, session, nav) in [
        (
            format!("/manage/threads/{tid}/organization"),
            &f.admin,
            true,
        ),
        (format!("/manage/reports/{id}"), &f.admin, true),
        (report, &f.member, false),
    ] {
        let (status, html, _) = f.call("GET", &path, Some(session), &[]).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(
            html.contains("class=\"admin-menu__item\""),
            nav,
            "{path} should {}show the admin navigation",
            if nav { "" } else { "not " }
        );
        assert_eq!(
            html.contains("manage-frame--plain"),
            !nav,
            "{path} falls back to one column wrongly"
        );
        // The sprite is sent only when there really is navigation; single-column pages do not carry those bytes.
        assert_eq!(html.contains("manage-icon-sprite"), nav, "{path}");
    }

    // Attachment storage and credits management used to use narrow-page auth-standalone: you could not click into them
    // from /manage, nor get back out once in; they now belong to the global settings module and share the same row of
    // tabs as the site settings.
    for path in ["/manage/storage", "/manage/credits"] {
        let (status, html, _) = f.call("GET", path, Some(&f.admin), &[]).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(
            !html.contains("auth-standalone"),
            "{path} is still a standalone narrow card"
        );
        assert_eq!(
            html.matches("class=\"admin-menu__item\"").count(),
            10,
            "{path}"
        );
        assert!(
            section(&html, "section-tabs")
                .contains(&format!("href=\"{path}\" aria-current=\"page\"")),
            "{path} tab does not mark the current page"
        );
    }
    f._services.cleanup_ref().await.unwrap();
}

/// A storage test or a refused save shows the submitted values again instead of emptying the form: the Access Key
/// is refilled, the Secret Key is not and the page asks for it, and the failure says why.
#[tokio::test]
async fn storage_form_keeps_what_was_typed_when_it_does_not_save() {
    let f = Fixture::new().await;
    let closed = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    let form = |operation: &str, region: &str| -> Vec<(String, String)> {
        [
            ("_csrf", f.csrf_tokens[&f.admin].clone()),
            ("revision", "0".into()),
            ("endpoint", closed.clone()),
            ("region", region.into()),
            ("bucket", "typed-bucket".into()),
            ("prefix", "typed-prefix".into()),
            ("path_style", "true".into()),
            ("access_key", "typed-access-key".into()),
            ("secret_key", "typed-secret-key".into()),
            ("operation", operation.into()),
        ]
        .into_iter()
        .map(|(name, value)| (name.into(), value))
        .collect()
    };
    for (operation, region, reason) in [
        ("test", "us-east-1", "the endpoint cannot be reached"),
        (
            "save",
            "not a region",
            "The S3 region, bucket or object prefix is not valid",
        ),
    ] {
        let (status, html, _) = f
            .call(
                "POST",
                "/manage/storage",
                Some(&f.admin),
                &form(operation, region),
            )
            .await;
        assert_ne!(status, StatusCode::OK, "{operation}");
        assert!(html.contains(reason), "{operation}: {html}");
        for kept in [
            format!("value=\"{closed}\""),
            "value=\"typed-bucket\"".to_owned(),
            "value=\"typed-prefix\"".to_owned(),
            "value=\"typed-access-key\"".to_owned(),
        ] {
            assert!(html.contains(&kept), "{operation} did not keep {kept}");
        }
        assert!(
            html.contains("name=\"path_style\" value=\"true\" checked"),
            "{operation}"
        );
        assert!(
            !html.contains("typed-secret-key"),
            "the Secret Key is not filled back in"
        );
        assert!(
            html.contains("For safety the Secret Key is not filled back in"),
            "{operation}"
        );
    }
    // Opening the page again shows the saved configuration (still none), not the last submission.
    let (_, html, _) = f.call("GET", "/manage/storage", Some(&f.admin), &[]).await;
    assert!(!html.contains("typed-bucket") && !html.contains("typed-access-key"));
    f._services.cleanup_ref().await.unwrap();
}
