//! Parent and child forums: parent forums aggregate their child forums (lists, feeds, follow notifications,
//! admin content filters, child forum limit), the "this forum" card in the forum page's right column, the moderator
//! list in the forum page header and posting guidelines, admin batch ordering, the rewritten-thread-count notice and
//! moderator column, layered dropdowns and the member API's `parent_fid`, status labels on move targets, navigation
//! document notices, and three rules: the admin content list's hidden check follows a members-only parent forum,
//! description length limits count characters, and blocked followed forums show as placeholders.
use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use gono_forum_web::{
    content::{
        self,
        writes::{ContentWriteError, ContentWriter, NewThread, WriteConfig},
    },
    db::{self, Database},
    identity::{self, Actor, Identity, SessionGrant},
    interaction::forum_filter::FilterMode,
    manage, organization,
    test_support::{TEST_SITE_URL, TestFixture, host_of, set_setting},
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const PASSWORD: &str = "Forums-batch-two-password-2026!";
const ADMIN: i64 = 9200;

async fn scalar(db: &Database, sql: &str) -> i64 {
    db::query_scalar(sql)
        .fetch_one(db.executor())
        .await
        .unwrap()
}

async fn execute(db: &Database, sql: &str) {
    db::query(sql).execute(db.executor()).await.unwrap();
}

async fn member(db: &Database, uid: i64, role: i64) {
    let salt = SaltString::encode_b64(b"forums-batch-two-salt").unwrap();
    let hash = Argon2::default()
        .hash_password(PASSWORD.as_bytes(), &salt)
        .unwrap()
        .to_string();
    db::query("INSERT INTO gonof_common_member (uid, username, email, password, emailstatus, adminid, regdate) VALUES (?, ?, ?, ?, 1, ?, ?)")
        .bind(uid)
        .bind(format!("family-{uid}"))
        .bind(format!("family-{uid}@example.test"))
        .bind(hash)
        .bind(role)
        .bind(identity::now() - 30 * 86_400)
        .execute(db.executor())
        .await
        .unwrap();
}

async fn actor(db: &Database, uid: i64) -> Actor {
    Actor {
        uid,
        credentials_version: scalar(
            db,
            &format!("SELECT credentials_version FROM gonof_common_member WHERE uid={uid}"),
        )
        .await,
        api_key: None,
    }
}

async fn forum(db: &Database, slug: &str) -> i64 {
    scalar(
        db,
        &format!("SELECT fid FROM gonof_forum_forum WHERE slug='{slug}'"),
    )
    .await
}

async fn version(db: &Database, fid: i64) -> i64 {
    scalar(
        db,
        &format!("SELECT version FROM gonof_forum_forum WHERE fid={fid}"),
    )
    .await
}

fn topic(fid: i64, title: &str) -> NewThread {
    NewThread {
        visibility: Default::default(),
        activity: Default::default(),
        fid,
        title: title.into(),
        body: format!("Body of {title}"),
        attachment_ids: vec![],
        request_key: identity::random_token(),
    }
}

/// Fixture: of the six forums, ai is placed under showcase; posting quota is loosened; 10 rows per page.
async fn family(f: &TestFixture) -> (i64, i64, i64) {
    content::seed_forums(&f.db).await.unwrap();
    for (key, value) in [
        ("settings.posting.daily_threads", "1000"),
        ("settings.posting.daily_replies", "10000"),
        ("settings.display.threads_per_page", "10"),
    ] {
        set_setting(&f.db, key, value).await.unwrap();
    }
    let showcase = forum(&f.db, "showcase").await;
    let ai = forum(&f.db, "ai").await;
    let hosting = forum(&f.db, "hosting").await;
    execute(
        &f.db,
        &format!("UPDATE gonof_forum_forum SET parent_fid={showcase} WHERE fid={ai}"),
    )
    .await;
    (showcase, ai, hosting)
}

fn writer(db: &Database) -> ContentWriter {
    ContentWriter::new(db.clone(), WriteConfig::default()).with_site_url(TEST_SITE_URL)
}

struct Http {
    f: TestFixture,
    app: Router,
    identity: Identity,
}

impl Http {
    fn new(f: TestFixture) -> Self {
        let identity = Identity::new(
            f.db.clone(),
            f.config.identity.clone(),
            f.root.clone(),
            f.global.clone(),
        );
        let app = gono_forum_web::router::build_with_global(
            f.config.clone(),
            &f.root,
            f.db.clone(),
            f.cache.clone(),
            f.global.clone(),
        );
        Self { f, app, identity }
    }

    async fn login(&self, uid: i64) -> SessionGrant {
        self.identity
            .login(&format!("family-{uid}"), PASSWORD)
            .await
            .unwrap()
    }

    async fn page(&self, path: &str, grant: Option<&SessionGrant>) -> (StatusCode, String) {
        let mut request = Request::builder().uri(path);
        if let Some(grant) = grant {
            request = request.header(
                header::COOKIE,
                format!("gonof_session={}", grant.session_token),
            );
        }
        let response = self
            .app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    async fn form(
        &self,
        path: &str,
        grant: &SessionGrant,
        fields: &[(&str, &str)],
    ) -> (StatusCode, String, Option<String>) {
        let mut body = url::form_urlencoded::Serializer::new(String::new());
        body.append_pair("_csrf", &grant.csrf_token);
        for (name, value) in fields {
            body.append_pair(name, value);
        }
        let request = Request::builder()
            .method("POST")
            .uri(path)
            .header("HX-Request", "false")
            .header(header::ORIGIN, TEST_SITE_URL)
            .header(header::HOST, host_of(TEST_SITE_URL))
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(
                header::COOKIE,
                format!("gonof_session={}", grant.session_token),
            )
            .body(Body::from(body.finish()))
            .unwrap();
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let location = response
            .headers()
            .get(header::LOCATION)
            .map(|value| value.to_str().unwrap().to_owned());
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(bytes.to_vec()).unwrap(), location)
    }
}

fn listed(page: &str, tid: i64) -> bool {
    list(page).contains(&format!("data-thread-id=\"{tid}\""))
}

fn list(page: &str) -> &str {
    page.find("aria-label=\"Threads\"")
        .map(|at| &page[at..at + page[at..].find("</ul>").unwrap()])
        .unwrap_or_default()
}

/// The HTML of this row in the thread list.
fn row(page: &str, tid: i64) -> &str {
    let list = list(page);
    let at = list.find(&format!("data-thread-id=\"{tid}\"")).unwrap();
    &list[at..at + list[at..].find("</li>").unwrap()]
}

/// The URLs of the three posting entry points on the page (list toolbar, sidebar member card, mobile drawer).
fn post_links(page: &str) -> Vec<&str> {
    ["data-compose-entry href=\""]
        .iter()
        .flat_map(|marker| {
            page.match_indices(marker).map(|(at, _)| {
                let at = at + marker.len();
                &page[at..at + page[at..].find('"').unwrap()]
            })
        })
        .filter(|href| *href == "/new" || href.starts_with("/new/"))
        .collect()
}

/// Parent forum pages, feeds, follow notifications and admin content filters all include child forums; a child forum
/// page lists only itself; a child forum the member has blocked does not appear on the parent forum page;
/// the parent forum page merges and paginates across forums by last reply; the right column only looks at its own
/// family; each parent forum has at most 10 child forums.
#[tokio::test]
async fn parent_forums_aggregate_their_children_everywhere() {
    let f = TestFixture::migrated().await.unwrap();
    let (showcase, ai, hosting) = family(&f).await;
    member(&f.db, ADMIN, 1).await;
    // 9201 follows showcase; 9202 follows both ai and showcase; 9203 is the author; 9204 has blocked ai.
    for uid in [9201, 9202, 9203, 9204] {
        member(&f.db, uid, 0).await;
    }
    let now = identity::now();
    for (uid, fid) in [(9201, showcase), (9202, ai), (9202, showcase)] {
        execute(
            &f.db,
            &format!(
                "INSERT INTO gonof_home_forum_follow(uid,fid,dateline) VALUES({uid},{fid},{now})"
            ),
        )
        .await;
    }
    let writer = writer(&f.db);
    // 12 threads, posted alternately in parent and child forum with increasing times: page 1 of the parent forum page
    // is the newest 10, page 2 has 2.
    let mut tids = Vec::new();
    for index in 0..12_i64 {
        let fid = if index % 2 == 0 { showcase } else { ai };
        tids.push((
            fid,
            writer
                .publish_at(
                    9203,
                    topic(fid, &format!("Family thread {index}")),
                    now - 600 + index,
                )
                .await
                .unwrap()
                .tid,
        ));
    }
    let other = writer
        .publish_at(9203, topic(hosting, "Other family thread"), now)
        .await
        .unwrap()
        .tid;

    // Follow notifications: new threads in a child forum also go to the parent forum's followers; those following both
    // levels get only one.
    for _ in 0..5 {
        gono_forum_web::notifications::deliver_pending(&f.db, identity::now())
            .await
            .unwrap();
    }
    let child_first = tids[1].1;
    let child_pid = scalar(
        &f.db,
        &format!("SELECT pid FROM gonof_forum_post WHERE first_tid={child_first}"),
    )
    .await;
    let key = format!("notification.content:{child_pid}");
    for (uid, expected) in [(9201, 1), (9202, 1), (9204, 0)] {
        assert_eq!(
            scalar(
                &f.db,
                &format!("SELECT COUNT(*) FROM gonof_home_notification WHERE uid={uid} AND event_key='{key}'")
            )
            .await,
            expected,
            "uid {uid}"
        );
    }
    assert_eq!(
        scalar(
            &f.db,
            "SELECT COUNT(*) FROM gonof_home_notification WHERE uid=9201"
        )
        .await,
        12,
        "the parent forum's follower gets all 12 new threads of both levels"
    );

    // Admin content filter: the parent forum includes its child forums.
    let admin = actor(&f.db, ADMIN).await;
    let filtered = manage::content(
        &f.db,
        admin,
        &manage::ContentQuery {
            fid: Some(showcase),
            kind: "thread".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(filtered.total, 12);
    assert!(filtered.rows.iter().any(|row| row.fid == ai));

    // The 11th child forum cannot be saved.
    for index in 0..9 {
        organization::save_forum(
            &f.db,
            admin,
            &organization::ForumInput {
                name: format!("Child {index}"),
                slug: format!("kid{index}x"),
                parent_fid: showcase,
                status: 1,
                visibility: "public".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }
    assert!(matches!(
        organization::save_forum(
            &f.db,
            admin,
            &organization::ForumInput {
                name: "Eleventh".into(),
                slug: "kideleven".into(),
                parent_fid: showcase,
                status: 1,
                visibility: "public".into(),
                ..Default::default()
            },
        )
        .await,
        Err(ContentWriteError::Validation(message)) if message.contains("at most 10 child forums")
    ));
    execute(
        &f.db,
        "DELETE FROM gonof_forum_forum WHERE slug LIKE 'kid%x'",
    )
    .await;

    let h = Http::new(f);
    let (status, first) = h.page("/showcase", None).await;
    assert_eq!(status, StatusCode::OK);
    for (index, (_, tid)) in tids.iter().enumerate() {
        assert_eq!(
            listed(&first, *tid),
            index >= 2,
            "page 1 has the newest 10 ({index})"
        );
    }
    assert!(!listed(&first, other));
    // Rows on the parent forum page label child forum threads with their child forum, linking to the child forum page;
    // threads of the forum itself are not labelled.
    for (fid, tid) in tids.iter().skip(2) {
        let line = row(&first, *tid);
        assert_eq!(
            line.contains("</svg><a href=\"/ai\">AI</a></span>"),
            *fid == ai,
            "{line}"
        );
        assert_eq!(line.contains("data-child-forum"), *fid == ai, "{line}");
        if *fid == ai {
            let meta = &line[line.find("class=\"row-meta\">").unwrap()..];
            assert!(
                meta.find("data-child-forum") < meta.find("translate=\"no\""),
                "the child forum comes before the author: {line}"
            );
        }
    }
    let (status, second) = h.page("/showcase-2", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(listed(&second, tids[0].1) && listed(&second, tids[1].1));
    // The child forum page lists only itself.
    let (_, child) = h.page("/ai", None).await;
    for (fid, tid) in &tids {
        assert_eq!(listed(&child, *tid), *fid == ai);
    }
    assert!(
        !child.contains("data-child-forum"),
        "every thread on the child forum page is in that forum"
    );
    // Latest in the right column: the parent forum page only looks at its own family (other families' threads appear
    // nowhere on the page), while the home page stays site-wide as usual.
    assert!(
        !first.contains("Other family thread"),
        "the right column leaves out other families' threads"
    );
    let (_, home) = h.page("/", None).await;
    assert!(home.contains("Other family thread"));
    assert!(
        !home.contains("data-child-forum"),
        "the home page does not label child forums"
    );
    // Feeds: the parent forum includes its child forums, a child forum only itself.
    let (status, feed) = h.page("/showcase/feed.xml", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(feed.contains("Family thread 11") && feed.contains("Family thread 10"));
    assert!(!feed.contains("Other family thread"));
    let (_, child_feed) = h.page("/ai/feed.xml", None).await;
    assert!(child_feed.contains("Family thread 11") && !child_feed.contains("Family thread 10"));
    // The member blocks a child forum: the parent forum page does not list its threads.
    gono_forum_web::interaction::forum_filter::set(
        &h.f.db,
        &h.f.cache,
        actor(&h.f.db, 9204).await,
        ai,
        Some(FilterMode::Block),
        &identity::random_token(),
    )
    .await
    .unwrap();
    let blocker = h.login(9204).await;
    let (_, blocked) = h.page("/showcase", Some(&blocker)).await;
    for (fid, tid) in &tids {
        assert_eq!(listed(&blocked, *tid), *fid == showcase, "{tid}");
    }
    // Posting entry points: forum pages and thread pages preselect this page's forum, the home page and other pages do
    // not; all three entry points agree. A blocked forum page is a block page and does not preselect.
    let follower = h.login(9201).await;
    let child_thread = format!("/thread-{child_first}");
    for (path, grant, expected) in [
        ("/showcase", &follower, "/new/showcase"),
        ("/ai", &follower, "/new/ai"),
        (child_thread.as_str(), &follower, "/new/ai"),
        ("/", &follower, "/new"),
        ("/my", &follower, "/new"),
        ("/showcase", &blocker, "/new/showcase"),
        ("/ai", &blocker, "/new"),
    ] {
        let (status, page) = h.page(path, Some(grant)).await;
        assert!(status.is_success(), "{path} {status}");
        let links = post_links(&page);
        assert!(!links.is_empty(), "{path} has a posting entry");
        assert!(
            links.iter().all(|href| *href == expected),
            "{path}: {links:?}"
        );
    }
    let (status, editor) = h.page("/new/ai", Some(&follower)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        editor.contains(&format!("<option value=\"{ai}\" selected")),
        "the editor preselects the child forum"
    );
    h.f.cleanup().await.unwrap();
}

/// The forum page header's moderator list (including those inherited from the parent forum) and posting guidelines;
/// the admin list's moderator column, batch ordering and version conflicts, and the rewritten-thread-count notice;
/// layered dropdowns and status labels for move targets and content filters; `/feeds` layering; navigation document
/// notices; the hidden check, character-based length limits and blocked-forum placeholders.
#[tokio::test]
async fn forum_header_backend_order_dropdowns_and_fixes() {
    let f = TestFixture::migrated().await.unwrap();
    let (showcase, ai, hosting) = family(&f).await;
    let daily = forum(&f.db, "daily").await;
    member(&f.db, ADMIN, 1).await;
    for (uid, role) in [(9211, 3), (9212, 3), (9213, 0)] {
        member(&f.db, uid, role).await;
    }
    execute(
        &f.db,
        &format!("INSERT INTO gonof_forum_moderator(uid,fid) VALUES(9211,{showcase}),(9212,{ai})"),
    )
    .await;
    execute(
        &f.db,
        &format!(
            "UPDATE gonof_forum_forum SET guidance='First line <b>not bold</b>\nSecond line',version=version+1 WHERE fid={ai}"
        ),
    )
    .await;
    execute(
        &f.db,
        &format!("UPDATE gonof_forum_forum SET guidance='',version=version+1 WHERE fid={showcase}"),
    )
    .await;
    let writer = writer(&f.db);
    let now = identity::now();
    let thread = writer
        .publish_at(9213, topic(ai, "Thread in the child forum"), now)
        .await
        .unwrap();
    let h = Http::new(f);

    // Forum page header: a child forum shows its own and its parent forum's moderators, a parent forum only its own;
    // guidelines are plain text, by line.
    let (_, child) = h.page("/ai", None).await;
    let moderators = child
        .find("data-forum-moderators")
        .map(|at| &child[at..at + child[at..].find("</p>").unwrap()])
        .unwrap();
    assert!(moderators.contains("family-9212") && moderators.contains("family-9211"));
    assert!(
        moderators.find("family-9212") < moderators.find("family-9211"),
        "the forum's own moderators come first"
    );
    assert!(child.contains("<summary>Posting guidelines</summary>"));
    assert!(child.contains("First line &#60;b&#62;not bold&#60;/b&#62;<br>Second line"));
    let (_, parent) = h.page("/showcase", None).await;
    let moderators = parent
        .find("data-forum-moderators")
        .map(|at| &parent[at..at + parent[at..].find("</p>").unwrap()])
        .unwrap();
    assert!(moderators.contains("family-9211") && !moderators.contains("family-9212"));
    assert!(
        !parent.contains("data-forum-guidance"),
        "no guidelines, no block"
    );

    // Admin list: moderator column, indented child forums; the rewritten thread count on the edit page.
    let admin = h.login(ADMIN).await;
    let (_, page) = h.page("/manage/forums", Some(&admin)).await;
    let row = |page: &str, fid: i64| {
        let at = page.find(&format!("data-forum-row=\"{fid}\"")).unwrap();
        page[at..at + page[at..].find("</li>").unwrap()].to_owned()
    };
    assert!(row(&page, showcase).contains("family-9211"));
    assert!(row(&page, ai).contains("family-9212") && !row(&page, ai).contains("family-9211"));
    assert!(row(&page, daily).contains("Moderators: none"));
    let (_, edit) = h
        .page(&format!("/manage/forums?fid={showcase}"), Some(&admin))
        .await;
    for form_page in [&page, &edit] {
        assert!(!form_page.contains(">Reason<"));
    }
    // Only a new parent rewrites threads, and only the forum's own: a forum with child forums cannot be moved.
    assert!(
        edit.contains("Changing the parent rewrites the copies on this forum&#39;s 0 threads in the same transaction"),
        "{edit}"
    );

    // Batch ordering: only forums that changed are updated; resubmitting with the old version is a conflict.
    let before = (
        version(&h.f.db, daily).await,
        version(&h.f.db, hosting).await,
    );
    let daily_version = before.0.to_string();
    let hosting_version = before.1.to_string();
    let hosting_order = scalar(
        &h.f.db,
        &format!("SELECT displayorder FROM gonof_forum_forum WHERE fid={hosting}"),
    )
    .await
    .to_string();
    let (status, _, location) = h
        .form(
            "/manage/forums/order",
            &admin,
            &[
                (&format!("order_{daily}"), "99"),
                (&format!("version_{daily}"), &daily_version),
                (&format!("order_{hosting}"), &hosting_order),
                (&format!("version_{hosting}"), &hosting_version),
                ("reason", "Reorder"),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location.as_deref(), Some("/manage/forums?saved=order"));
    assert_eq!(
        scalar(
            &h.f.db,
            &format!("SELECT displayorder FROM gonof_forum_forum WHERE fid={daily}")
        )
        .await,
        99
    );
    assert_eq!(version(&h.f.db, daily).await, before.0 + 1);
    assert_eq!(
        version(&h.f.db, hosting).await,
        before.1,
        "unchanged forums are not written"
    );
    let (status, page, _) = h
        .form(
            "/manage/forums/order",
            &admin,
            &[
                (&format!("order_{daily}"), "1"),
                (&format!("version_{daily}"), &daily_version),
                ("reason", "Reorder again"),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(page.contains("The forum has been changed. Refresh and try again."));

    // Move targets: layered, disabled ones labelled; content filter dropdowns layered; /feeds layered.
    execute(
        &h.f.db,
        &format!("UPDATE gonof_forum_forum SET status=0,version=version+1 WHERE fid={hosting}"),
    )
    .await;
    let (_, organization_page) = h
        .page(
            &format!("/manage/threads/{}/organization", thread.tid),
            Some(&admin),
        )
        .await;
    assert!(
        organization_page.contains("└ "),
        "child forums are indented"
    );
    assert!(organization_page.contains(" (disabled)"));
    let (_, content_page) = h.page("/manage/content", Some(&admin)).await;
    assert!(content_page.contains(&format!("<option value=\"{ai}\">└ ")));
    let (_, feeds) = h.page("/feeds", None).await;
    assert!(feeds.contains("└ "));

    // A navigation document referencing a disabled forum saves as usual, and the edit page lists the items that
    // will not be shown.
    let navigation = format!("Hosting | forum:{hosting}\nDaily | forum:{daily}");
    let (status, _, _) = h
        .form(
            "/manage/navigation",
            &admin,
            &[("title", ""), ("body", &navigation)],
        )
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (_, document) = h.page("/manage/navigation", Some(&admin)).await;
    let hidden = document
        .find("data-navigation-hidden")
        .map(|at| &document[at..at + document[at..].find("</p>").unwrap()])
        .unwrap();
    assert!(hidden.contains("“Hosting”") && hidden.contains("the forum is disabled"));
    assert!(!hidden.contains("“Daily”"));

    // When the parent forum becomes members-only, posts in its child forums count as "hidden" in the admin content
    // list.
    execute(
        &h.f.db,
        &format!("UPDATE gonof_forum_forum SET status=1,version=version+1 WHERE fid={hosting}"),
    )
    .await;
    let admin_actor = actor(&h.f.db, ADMIN).await;
    let current = version(&h.f.db, showcase).await;
    organization::save_forum(
        &h.f.db,
        admin_actor,
        &organization::ForumInput {
            fid: showcase,
            name: "Showcase".into(),
            slug: "showcase".into(),
            status: 1,
            visibility: "members".into(),
            version: current,
            displayorder: 1,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let hidden = manage::content(
        &h.f.db,
        admin_actor,
        &manage::ContentQuery {
            state: "hidden".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(hidden.rows.iter().any(|row| row.tid == thread.tid));

    // The description length limit is by characters: 2000 CJK characters (6000 bytes) can be stored. "\u{8bf4}" is
    // the Chinese for "say", three bytes in UTF-8.
    let current = version(&h.f.db, daily).await;
    organization::save_forum(
        &h.f.db,
        admin_actor,
        &organization::ForumInput {
            fid: daily,
            name: "Daily".into(),
            slug: "daily".into(),
            description: "\u{8bf4}".repeat(2000),
            status: 1,
            visibility: "public".into(),
            version: current,
            displayorder: 99,
            ..Default::default()
        },
    )
    .await
    .expect("the limit counts characters");

    // A followed forum that has been blocked shows as a placeholder in the follow list.
    execute(
        &h.f.db,
        &format!(
            "INSERT INTO gonof_home_forum_follow(uid,fid,dateline) VALUES(9213,{daily},{now})"
        ),
    )
    .await;
    gono_forum_web::interaction::forum_filter::set(
        &h.f.db,
        &h.f.cache,
        actor(&h.f.db, 9213).await,
        daily,
        Some(FilterMode::Block),
        &identity::random_token(),
    )
    .await
    .unwrap();
    let reader = h.login(9213).await;
    let (status, following) = h.page("/my/following/forums", Some(&reader)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(following.contains("Content not available"));
    h.f.cleanup().await.unwrap();
}
