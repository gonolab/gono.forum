//! User groups (docs/roles.md): mute, level thresholds, trainee moderators, reviewers and honor groups.
use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use gono_forum_web::{
    content::{
        self,
        edits::{ChangeRequest, ContentAction, ContentEditor, EditRequest},
        writes::{ContentWriteError, ContentWriter, NewReply, NewThread, WriteConfig},
    },
    db::{self, Database},
    honors,
    i18n::Lang,
    identity::{self, Actor, Identity, IdentityError, SessionGrant},
    interaction::Interaction,
    messages,
    moderation::{self, ReportTarget, SanctionRequest},
    test_support::{TEST_SITE_URL, TestFixture, host_of, set_setting},
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const PASSWORD: &str = "Member-groups-password-2026!";

async fn scalar(db: &Database, sql: &str) -> i64 {
    db::query_scalar(sql)
        .fetch_one(db.executor())
        .await
        .unwrap()
}

async fn execute(db: &Database, sql: &str) {
    db::query(sql).execute(db.executor()).await.unwrap();
}

/// A member with a verified e-mail, registered for at least 30 days; `role` is the adminid (0–5).
async fn member(db: &Database, uid: i64, role: i64) {
    let salt = SaltString::encode_b64(b"member-groups-salt").unwrap();
    let hash = Argon2::default()
        .hash_password(PASSWORD.as_bytes(), &salt)
        .unwrap()
        .to_string();
    db::query("INSERT INTO gonof_common_member (uid, username, email, password, emailstatus, adminid, regdate) VALUES (?, ?, ?, ?, 1, ?, ?)")
        .bind(uid)
        .bind(format!("group-{uid}"))
        .bind(format!("group-{uid}@example.test"))
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
        credentials_version: version(db, uid).await,
        api_key: None,
    }
}

async fn version(db: &Database, uid: i64) -> i64 {
    scalar(
        db,
        &format!("SELECT credentials_version FROM gonof_common_member WHERE uid={uid}"),
    )
    .await
}

async fn forum(db: &Database, slug: &str) -> i64 {
    scalar(
        db,
        &format!("SELECT fid FROM gonof_forum_forum WHERE slug='{slug}'"),
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

fn reply(tid: i64, body: &str) -> NewReply {
    NewReply {
        tid,
        root_pid: None,
        reply_to_pid: None,
        body: body.into(),
        attachment_ids: vec![],
        request_key: identity::random_token(),
    }
}

async fn generous_quotas(db: &Database) {
    for (key, value) in [
        ("settings.posting.daily_threads", "1000"),
        ("settings.posting.daily_replies", "10000"),
    ] {
        set_setting(db, key, value).await.unwrap();
    }
}

fn writer(db: &Database) -> ContentWriter {
    ContentWriter::new(db.clone(), WriteConfig::default()).with_site_url(TEST_SITE_URL)
}

fn editor(db: &Database) -> ContentEditor {
    ContentEditor::new(db.clone(), WriteConfig::default(), 86_400)
}

async fn change(
    editor: &ContentEditor,
    actor: Actor,
    pid: i64,
    action: ContentAction,
) -> Result<(), ContentWriteError> {
    let post = editor.editable(actor, pid).await?;
    editor
        .change(
            actor,
            ChangeRequest {
                pid,
                expected_post_version: post.version,
                expected_thread_version: post.thread_version,
                request_key: identity::random_token(),
                reason: "User group test".into(),
                action,
            },
        )
        .await
        .map(drop)
}

/// Judges state directly from the database row: when a management action cannot be read through `editable`
/// (no permission), it cannot be used to get the version.
async fn change_with(
    editor: &ContentEditor,
    db: &Database,
    actor: Actor,
    pid: i64,
    action: ContentAction,
) -> Result<(), ContentWriteError> {
    let post_version = scalar(
        db,
        &format!("SELECT version FROM gonof_forum_post WHERE pid={pid}"),
    )
    .await;
    let thread_version = scalar(
        db,
        &format!("SELECT t.version FROM gonof_forum_thread t JOIN gonof_forum_post p ON p.tid=t.tid WHERE p.pid={pid}"),
    )
    .await;
    editor
        .change(
            actor,
            ChangeRequest {
                pid,
                expected_post_version: post_version,
                expected_thread_version: thread_version,
                request_key: identity::random_token(),
                reason: "User group test".into(),
                action,
            },
        )
        .await
        .map(drop)
}

fn restricted(result: Result<impl std::fmt::Debug, ContentWriteError>) -> String {
    match result {
        Err(ContentWriteError::Restricted(message)) => message,
        other => panic!("expected Restricted, got {other:?}"),
    }
}

fn mute_request(uid: i64, version: i64, kind: &str, hours: i64) -> SanctionRequest {
    SanctionRequest {
        uid,
        expected_version: version,
        kind: kind.into(),
        hours,
        reason: "User group test".into(),
        request_key: identity::random_token(),
    }
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
            .login(&format!("group-{uid}"), PASSWORD)
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

/// Mute: posting threads, replying, editing, private messages, likes and profile edits are all refused,
/// favorites work as usual, and every page shows a notice; global moderators can only mute and only revoke mutes,
/// and cannot mute global moderators; on expiry the mute lifts immediately by the end time on the member row, and
/// the background task adds the end record afterwards.
#[tokio::test]
async fn mute_blocks_public_expression_and_global_moderators_mute_only() {
    let f = TestFixture::migrated().await.unwrap();
    let db = f.db.clone();
    content::seed_forums(&db).await.unwrap();
    generous_quotas(&db).await;
    let daily = forum(&db, "daily").await;
    // 9501 administrator, 9502 global moderator, 9503 the muted member, 9504 another member, 9505 another global
    // moderator.
    for (uid, role) in [(9501, 1), (9502, 2), (9503, 0), (9504, 0), (9505, 2)] {
        member(&db, uid, role).await;
    }
    let writer = writer(&db);
    let own = writer
        .publish(9503, topic(daily, "Thread before the mute"))
        .await
        .unwrap();
    let other = writer
        .publish(9504, topic(daily, "Thread by someone else"))
        .await
        .unwrap();

    // A global moderator cannot warn or freeze, but can mute; cannot mute a global moderator.
    let global = actor(&db, 9502).await;
    let target_version = version(&db, 9503).await;
    for kind in ["warning", "freeze"] {
        let hours = if kind == "warning" { 0 } else { 24 };
        assert!(matches!(
            moderation::issue_sanction(
                &db,
                global,
                &mute_request(9503, target_version, kind, hours),
                identity::now()
            )
            .await,
            Err(ContentWriteError::Forbidden(_))
        ));
    }
    assert!(matches!(
        moderation::issue_sanction(
            &db,
            global,
            &mute_request(9505, version(&db, 9505).await, "mute", 24),
            identity::now()
        )
        .await,
        Err(ContentWriteError::Forbidden(
            "Global moderators cannot be muted"
        ))
    ));
    let sanction = moderation::issue_sanction(
        &db,
        global,
        &mute_request(9503, target_version, "mute", 24),
        identity::now(),
    )
    .await
    .unwrap();
    let until = scalar(
        &db,
        "SELECT mute_until FROM gonof_common_member WHERE uid=9503",
    )
    .await;
    assert!(
        until > identity::now() + 23 * 3600,
        "the member row records the end time"
    );
    assert_eq!(
        version(&db, 9503).await,
        target_version,
        "a mute does not revoke sessions"
    );
    // A member already muted cannot be muted again.
    assert!(matches!(
        moderation::issue_sanction(
            &db,
            global,
            &mute_request(9503, target_version, "mute", 0),
            identity::now()
        )
        .await,
        Err(ContentWriteError::Conflict(_))
    ));

    let muted = actor(&db, 9503).await;
    assert!(
        restricted(
            writer
                .publish(9503, topic(daily, "Thread while muted"))
                .await
        )
        .contains("You are muted until")
    );
    assert!(
        restricted(
            writer
                .reply(9503, reply(other.tid, "Reply while muted"))
                .await
        )
        .contains("You are muted until")
    );
    let editor = editor(&db);
    let post = editor.editable(muted, own.pid).await.unwrap();
    restricted(
        editor
            .edit(
                muted,
                EditRequest {
                    pid: own.pid,
                    expected_post_version: post.version,
                    expected_thread_version: post.thread_version,
                    title: None,
                    body: Some("Edited body while muted".into()),
                    request_key: identity::random_token(),
                    reason: String::new(),
                    visibility: None,
                },
            )
            .await,
    );
    restricted(
        messages::send(
            &db,
            muted,
            9504,
            "Message while muted",
            &identity::random_token(),
        )
        .await,
    );
    let interaction = Interaction::new(db.clone());
    restricted(
        interaction
            .set_reaction(muted, other.pid, true, &identity::random_token())
            .await,
    );
    interaction
        .set_favorite(muted, other.tid, true, &identity::random_token())
        .await
        .expect("favorites work as usual");

    let http = Http::new(f);
    let grant = http.login(9503).await;
    assert!(
        matches!(
            http.identity
                .update_profile(&grant.session_token, "group-9503-new", "")
                .await,
            Err(IdentityError::Conflict(
                "While muted you cannot change your username or bio"
            ))
        ),
        "the rename is refused"
    );
    assert!(matches!(
        http.identity
            .update_profile(&grant.session_token, "group-9503", "New bio")
            .await,
        Err(IdentityError::Conflict(_))
    ));
    http.identity
        .update_profile(&grant.session_token, "group-9503", "")
        .await
        .expect("submitting unchanged values works");
    let (status, home) = http.page("/", Some(&grant)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        home.contains("data-mute-banner"),
        "every page shows the mute notice"
    );
    let (_, other_home) = http.page("/", Some(&http.login(9504).await)).await;
    assert!(!other_home.contains("data-mute-banner"));

    // A global moderator can open the sanction page and sees mute as the only option; after the mute is revoked
    // posting works as usual.
    let global_grant = http.login(9502).await;
    let (status, page) = http
        .page("/manage/sanctions?uid=9504", Some(&global_grant))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        page.contains(r#"<option value="mute""#) && !page.contains(r#"<option value="freeze""#)
    );
    moderation::revoke_sanction(
        &http.f.db,
        global,
        sanction,
        "Test revocation",
        identity::now(),
    )
    .await
    .unwrap();
    assert_eq!(
        scalar(
            &http.f.db,
            "SELECT mute_until FROM gonof_common_member WHERE uid=9503"
        )
        .await,
        0
    );
    writer
        .reply(9503, reply(other.tid, "Reply after the revocation"))
        .await
        .unwrap();

    // A one-hour mute imposed two hours ago: the end time on the member row has passed, so posting works immediately;
    // the background task adds "Your mute has expired" afterwards.
    let past = identity::now() - 7200;
    let expired = moderation::issue_sanction(
        &http.f.db,
        global,
        &mute_request(9503, version(&http.f.db, 9503).await, "mute", 1),
        past,
    )
    .await
    .unwrap();
    writer
        .reply(9503, reply(other.tid, "Reply after the mute expired"))
        .await
        .unwrap();
    assert_eq!(
        moderation::expire_sanctions(&http.f.db, identity::now())
            .await
            .unwrap(),
        1
    );
    assert!(
        scalar(
            &http.f.db,
            &format!("SELECT revoked_at FROM gonof_common_member_sanction WHERE id={expired}")
        )
        .await
            > 0
    );
    assert_eq!(
        scalar(
            &http.f.db,
            "SELECT mute_until FROM gonof_common_member WHERE uid=9503"
        )
        .await,
        0
    );
    assert_eq!(
        // The row keeps only the reason, in the site language; "Your mute has expired." is put in when the list is
        // shown (docs/architecture.md, "Languages").
        scalar(&http.f.db, &format!("SELECT COUNT(*) FROM gonof_home_notification WHERE uid=9503 AND event_key='sanction.{expired}.ended' AND reason='{}'", Lang::EN.governance_notice_mute_over())).await,
        1
    );
    http.f.cleanup().await.unwrap();
}

/// Level thresholds: seeded as 1, meaning no limit; once raised, external links, invite codes and red
/// packets/lotteries are blocked by growth level, with a notice stating the required level; moderators and trainee
/// moderators are not restricted in their own forums, reviewers are not exempt; all staff are exempt for invite
/// codes.
#[tokio::test]
async fn level_gates_follow_settings_and_exempt_staff() {
    let f = TestFixture::migrated().await.unwrap();
    let db = f.db.clone();
    content::seed_forums(&db).await.unwrap();
    generous_quotas(&db).await;
    let daily = forum(&db, "daily").await;
    // 9511 member, 9512 moderator (daily), 9513 trainee moderator (daily), 9514 reviewer, 9515 member (growth 80,
    // Lv3).
    for (uid, role) in [(9511, 0), (9512, 3), (9513, 4), (9514, 5), (9515, 0)] {
        member(&db, uid, role).await;
    }
    execute(
        &db,
        &format!("INSERT INTO gonof_forum_moderator(uid,fid) VALUES(9512,{daily}),(9513,{daily})"),
    )
    .await;
    execute(
        &db,
        "INSERT INTO gonof_common_growth_account(uid,growth,version) VALUES(9515,80,0)",
    )
    .await;
    let writer = writer(&db);
    let thread = writer
        .publish(9511, topic(daily, "Level gate test"))
        .await
        .unwrap();
    let linked = "Look here https://example.com/page";

    // Seeded as 1: growth is not read and everything passes as usual.
    writer.reply(9511, reply(thread.tid, linked)).await.unwrap();
    for (key, value) in [
        ("settings.growth.link_level", "2"),
        ("settings.growth.invite_level", "3"),
        ("settings.growth.activity_level", "2"),
    ] {
        set_setting(&db, key, value).await.unwrap();
    }
    assert_eq!(
        restricted(writer.reply(9511, reply(thread.tid, linked)).await),
        "You can post links to other sites once you reach Lv2 (Two-Star Member)"
    );
    writer
        .reply(9511, reply(thread.tid, "No external link"))
        .await
        .expect("a reply without external links is not affected");
    writer
        .reply(9511, reply(thread.tid, "Internal link /t/1"))
        .await
        .unwrap();
    for staff in [9512, 9513] {
        writer
            .reply(staff, reply(thread.tid, linked))
            .await
            .expect("staff who manage the forum are not limited");
    }
    restricted(writer.reply(9514, reply(thread.tid, linked)).await);
    writer
        .reply(9515, reply(thread.tid, linked))
        .await
        .expect("Lv3 reaches Lv2");
    // When the observation period is also on and neither condition is met, both are stated together.
    set_setting(&db, "settings.posting.new_member_link_block", "true")
        .await
        .unwrap();
    set_setting(&db, "settings.posting.new_member_link_days", "60")
        .await
        .unwrap();
    assert_eq!(
        restricted(writer.reply(9511, reply(thread.tid, linked)).await),
        "You can post links to other sites 60 days after signing up, once you reach Lv2 (Two-Star Member)"
    );
    set_setting(&db, "settings.posting.new_member_link_block", "false")
        .await
        .unwrap();

    // Red packets and lotteries: the threshold is checked before the activity validation.
    let mut packet = topic(daily, "Red packet thread");
    packet.activity.kind = 2;
    assert_eq!(
        restricted(writer.publish(9511, packet.clone()).await),
        "You need to reach Lv2 (Two-Star Member) to start red packets and lotteries"
    );
    packet.request_key = identity::random_token();
    assert!(
        !matches!(
            writer.publish(9512, packet).await,
            Err(ContentWriteError::Restricted(_))
        ),
        "moderators are not level-gated in their own forums (the activity checks after that are another matter)"
    );
    let mut lottery = topic(daily, "Lottery thread");
    lottery.activity.kind = 3;
    restricted(writer.publish(9514, lottery).await);

    // Invite codes: Lv1 is refused, Lv3 is allowed; all staff are exempt.
    set_setting(&db, "settings.invite.enabled", "true")
        .await
        .unwrap();
    let http = Http::new(f);
    assert!(matches!(
        http.identity
            .generate_invite(9511, &identity::random_token())
            .await,
        Err(IdentityError::Conflict(
            "Your growth level is not high enough to create invite codes yet"
        ))
    ));
    http.identity
        .generate_invite(9515, &identity::random_token())
        .await
        .unwrap();
    http.identity
        .generate_invite(9514, &identity::random_token())
        .await
        .expect("reviewers are staff too");
    http.f.global.clear_local();
    let (status, page) = http
        .page("/my/invites", Some(&http.login(9511).await))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains(
        "You can create invite codes from Lv3 (Three-Star Member) on; you are now Lv1 (One-Star Member)."
    ));
    assert!(page.contains("disabled"));
    let (_, page) = http
        .page("/my/invites", Some(&http.login(9515).await))
        .await;
    assert!(!page.contains("data-invite-level"));
    http.f.cleanup().await.unwrap();
}

/// Trainee moderators: appointed to forums, where they can only hide and unhide, lock and
/// unlock, and handle reports; they cannot edit on behalf of others, move, pin or highlight; they have no reach
/// outside their forums; the thread page only has the "Moderate" entry; they can accept restore requests that
/// only need unhiding.
#[tokio::test]
async fn intern_moderators_hide_and_lock_only() {
    let f = TestFixture::migrated().await.unwrap();
    let db = f.db.clone();
    content::seed_forums(&db).await.unwrap();
    generous_quotas(&db).await;
    let daily = forum(&db, "daily").await;
    let showcase = forum(&db, "showcase").await;
    for (uid, role) in [(9521, 4), (9522, 0), (9523, 0), (9524, 3)] {
        member(&db, uid, role).await;
    }
    execute(
        &db,
        &format!("INSERT INTO gonof_forum_moderator(uid,fid) VALUES(9521,{daily}),(9524,{daily})"),
    )
    .await;
    let writer = writer(&db);
    let inside = writer
        .publish(9522, topic(daily, "Thread in the trainee forum"))
        .await
        .unwrap();
    let outside = writer
        .publish(9522, topic(showcase, "Thread in another forum"))
        .await
        .unwrap();
    let answer = writer
        .reply(9522, reply(inside.tid, "A reply"))
        .await
        .unwrap();
    assert!(identity::can_manage_forum(&db, 9521, daily).await.unwrap());
    assert!(
        !identity::can_manage_forum(&db, 9521, showcase)
            .await
            .unwrap()
    );

    let intern = actor(&db, 9521).await;
    let editor = editor(&db);
    change(&editor, intern, answer.pid, ContentAction::Hide)
        .await
        .unwrap();
    change(&editor, intern, answer.pid, ContentAction::Unhide)
        .await
        .unwrap();
    change(&editor, intern, inside.pid, ContentAction::Lock)
        .await
        .unwrap();
    change(&editor, intern, inside.pid, ContentAction::Unlock)
        .await
        .unwrap();
    for action in [
        ContentAction::Pin,
        ContentAction::Pin3,
        ContentAction::Feature,
    ] {
        assert!(
            matches!(
                change_with(&editor, &db, intern, inside.pid, action).await,
                Err(ContentWriteError::Forbidden(
                    "Trainee moderators can only hide, unhide, lock and unlock"
                ))
            ),
            "{action:?}"
        );
    }
    assert!(matches!(
        change_with(&editor, &db, intern, outside.pid, ContentAction::Hide).await,
        Err(ContentWriteError::NotFound)
    ));
    assert!(matches!(
        editor
            .edit(
                intern,
                EditRequest {
                    pid: answer.pid,
                    expected_post_version: scalar(
                        &db,
                        &format!(
                            "SELECT version FROM gonof_forum_post WHERE pid={}",
                            answer.pid
                        )
                    )
                    .await,
                    expected_thread_version: scalar(
                        &db,
                        &format!(
                            "SELECT version FROM gonof_forum_thread WHERE tid={}",
                            inside.tid
                        )
                    )
                    .await,
                    title: None,
                    body: Some("Edited on behalf".into()),
                    request_key: identity::random_token(),
                    reason: "Trying to edit on behalf".into(),
                    visibility: None,
                },
            )
            .await,
        Err(ContentWriteError::Forbidden(
            "Trainee moderators and reviewers cannot edit content on the author's behalf"
        ))
    ));

    // Reports: those inside the forum can be handled.
    let reporter = actor(&db, 9523).await;
    let report = moderation::submit_report(
        &db,
        reporter,
        &ReportTarget {
            target_type: "reply".into(),
            target_id: answer.pid,
        },
        "Report reason",
        &identity::random_token(),
        identity::now(),
        100,
    )
    .await
    .unwrap();
    moderation::resolve_report(&db, intern, report, "resolved", "Handled", identity::now())
        .await
        .unwrap();

    // Restore requests: one that only needs unhiding can be accepted (hidden by a moderator; the trainee moderator
    // did not do the hiding, so need not recuse).
    change(
        &editor,
        actor(&db, 9524).await,
        answer.pid,
        ContentAction::Hide,
    )
    .await
    .unwrap();
    let settings = gono_forum_web::runtime::read(&db).await.unwrap();
    let request = moderation::submit_restoration(
        &db,
        actor(&db, 9522).await,
        answer.pid,
        "Please restore",
        &identity::random_token(),
        &moderation::RestorationRules::from_settings(&settings),
        identity::now(),
    )
    .await
    .unwrap();

    let http = Http::new(f);
    let grant = http.login(9521).await;
    let (status, list) = http.page("/manage/restorations", Some(&grant)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(list.contains(&format!("/manage/restorations/{request}")));
    moderation::resolve_restoration(
        &http.f.db,
        &editor,
        intern,
        request,
        "accepted",
        "Unhidden",
        identity::now(),
    )
    .await
    .unwrap();
    assert_eq!(
        scalar(
            &http.f.db,
            &format!(
                "SELECT invisible FROM gonof_forum_post WHERE pid={}",
                answer.pid
            )
        )
        .await,
        0
    );

    // Thread page: has "Moderate" but no edit-on-behalf or move thread; the move page is 404; the content detail page
    // offers no pin or highlight.
    let (_, page) = http
        .page(
            &gono_forum_web::urls::thread(inside.tid as u64, 1, false),
            Some(&grant),
        )
        .await;
    assert!(page.contains(&format!("/manage/content/{}", answer.pid)));
    assert!(!page.contains("/organization\">Move thread"));
    assert!(!page.contains(&format!("/edit/post-{}", answer.pid)));
    let (status, _) = http
        .page(
            &format!("/manage/threads/{}/organization", inside.tid),
            Some(&grant),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, detail) = http
        .page(&format!("/manage/content/{}", inside.pid), Some(&grant))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        detail.contains(r#"value="lock""#)
            && !detail.contains(r#"value="pin""#)
            && !detail.contains(r#"value="feature""#)
    );
    assert!(!detail.contains("Edit this content"));
    // The moderator list in the forum page header marks the trainee as "(trainee)".
    let (_, forum_page) = http.page("/daily", None).await;
    assert!(forum_page.contains("group-9521 (trainee)"));
    http.f.cleanup().await.unwrap();
}

/// Reviewers: can see and handle all reports site-wide, and can also mark profile
/// and attachment reports; can only hide and unhide the reported item itself; get no restore requests or action
/// log; recuse from reports about themselves.
#[tokio::test]
async fn reviewers_handle_reports_site_wide() {
    let f = TestFixture::migrated().await.unwrap();
    let db = f.db.clone();
    content::seed_forums(&db).await.unwrap();
    generous_quotas(&db).await;
    let daily = forum(&db, "daily").await;
    for (uid, role) in [(9531, 5), (9532, 0), (9533, 0)] {
        member(&db, uid, role).await;
    }
    let writer = writer(&db);
    let thread = writer
        .publish(9532, topic(daily, "Reported thread"))
        .await
        .unwrap();
    let reported = writer
        .reply(9532, reply(thread.tid, "Reported reply"))
        .await
        .unwrap();
    let quiet = writer
        .reply(9532, reply(thread.tid, "Reply nobody reported"))
        .await
        .unwrap();
    let reporter = actor(&db, 9533).await;
    let report = |kind: &'static str, id: i64| {
        let db = db.clone();
        async move {
            moderation::submit_report(
                &db,
                reporter,
                &ReportTarget {
                    target_type: kind.into(),
                    target_id: id,
                },
                "Report reason",
                &identity::random_token(),
                identity::now(),
                100,
            )
            .await
            .unwrap()
        }
    };
    let reply_report = report("reply", reported.pid).await;
    let profile_report = report("profile", 9532).await;
    let own_profile = report("profile", 9531).await;

    let reviewer = actor(&db, 9531).await;
    let queue: Vec<i64> = moderation::report_list(&db, reviewer, true, "", 0)
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert!(queue.contains(&reply_report) && queue.contains(&profile_report));
    let editor = editor(&db);
    change(&editor, reviewer, reported.pid, ContentAction::Hide)
        .await
        .unwrap();
    change(&editor, reviewer, reported.pid, ContentAction::Unhide)
        .await
        .unwrap();
    assert!(matches!(
        change_with(&editor, &db, reviewer, quiet.pid, ContentAction::Hide).await,
        Err(ContentWriteError::NotFound)
    ));
    report("thread", thread.tid).await;
    assert!(matches!(
        change_with(&editor, &db, reviewer, thread.pid, ContentAction::Lock).await,
        Err(ContentWriteError::Forbidden(
            "Reviewers can only hide or unhide reported content"
        ))
    ));
    change(&editor, reviewer, thread.pid, ContentAction::Hide)
        .await
        .unwrap();
    moderation::resolve_report(
        &db,
        reviewer,
        reply_report,
        "resolved",
        "Hidden",
        identity::now(),
    )
    .await
    .unwrap();
    moderation::resolve_report(
        &db,
        reviewer,
        profile_report,
        "dismissed",
        "The profile is fine",
        identity::now(),
    )
    .await
    .unwrap();
    assert!(matches!(
        moderation::resolve_report(
            &db,
            reviewer,
            own_profile,
            "dismissed",
            "Recusing",
            identity::now()
        )
        .await,
        Err(ContentWriteError::Forbidden(_))
    ));

    let http = Http::new(f);
    let grant = http.login(9531).await;
    let (status, overview) = http.page("/manage", Some(&grant)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(overview.contains("Pending reports") && !overview.contains("Active members"));
    for (path, expected) in [
        ("/manage/reports", StatusCode::OK),
        ("/manage/restorations", StatusCode::FORBIDDEN),
        ("/manage/sanctions", StatusCode::FORBIDDEN),
    ] {
        assert_eq!(http.page(path, Some(&grant)).await.0, expected, "{path}");
    }
    let (status, content_list) = http.page("/manage/content", Some(&grant)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(content_list.contains(&format!("/manage/content/{}", reported.pid)));
    assert!(
        !content_list.contains(&format!("/manage/content/{}\"", quiet.pid)),
        "content nobody reported is not listed"
    );
    assert_eq!(
        http.page(&format!("/manage/content/{}", quiet.pid), Some(&grant))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    http.f.cleanup().await.unwrap();
}

/// Honor groups: after an administrator creates and grants one, floors and profiles show a small badge;
/// not shown after disabling, expiry or revocation; group names are unique; only administrators can enter; included
/// in data exports and deleted when account deletion is carried out.
#[tokio::test]
async fn honor_groups_show_badges_and_follow_status() {
    let f = TestFixture::migrated().await.unwrap();
    let db = f.db.clone();
    content::seed_forums(&db).await.unwrap();
    generous_quotas(&db).await;
    let daily = forum(&db, "daily").await;
    for (uid, role) in [(9541, 1), (9542, 0), (9543, 2)] {
        member(&db, uid, role).await;
    }
    // The baseline seeds no honor groups; these two come from the sample site (`test_support::restore_sample_site`).
    let seeded: Vec<String> = honors::honors(&db)
        .await
        .unwrap()
        .into_iter()
        .map(|h| h.name)
        .collect();
    assert_eq!(seeded, ["Builder", "Sponsor"]);
    let thread = writer(&db)
        .publish(9542, topic(daily, "Author with honors"))
        .await
        .unwrap();
    let http = Http::new(f);
    let admin = http.login(9541).await;
    let (status, _, location) = http
        .form(
            "/manage/honors",
            &admin,
            &[
                ("name", "Developer"),
                ("description", "Works on the community software"),
                ("displayorder", "0"),
                ("enabled", "1"),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location.as_deref(), Some("/manage/honors?done=created"));
    let (status, _, _) = http
        .form(
            "/manage/honors",
            &admin,
            &[
                ("name", "Developer"),
                ("description", "Duplicate"),
                ("enabled", "1"),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "group names are unique");
    let developer = scalar(
        &http.f.db,
        "SELECT id FROM gonof_common_honor WHERE name='Developer'",
    )
    .await;
    let sponsor = scalar(
        &http.f.db,
        "SELECT id FROM gonof_common_honor WHERE name='Sponsor'",
    )
    .await;
    for id in [developer, sponsor] {
        let (status, _, location) = http
            .form(
                "/manage/honors/members/9542",
                &admin,
                &[("honor_id", &id.to_string()), ("expires", "")],
            )
            .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(
            location.as_deref(),
            Some("/manage/honors/members/9542?done=granted")
        );
    }
    let (status, _, _) = http
        .form(
            "/manage/honors/members/9542",
            &admin,
            &[
                ("honor_id", &developer.to_string()),
                ("expires", "2020-01-01"),
            ],
        )
        .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "the expiry date must be in the future"
    );

    let topic_path = gono_forum_web::urls::thread(thread.tid as u64, 1, false);
    let badge = |name: &str, description: &str| {
        format!(
            r#"<span class="tag tag--honor" title="{description}" data-honor-badge>{name}</span>"#
        )
    };
    let (_, page) = http.page(&topic_path, None).await;
    let developer_at = page
        .find(&badge("Developer", "Works on the community software"))
        .expect("the floor shows Developer");
    let sponsor_at = page
        .find(&badge("Sponsor", "Sponsors the running of the community"))
        .expect("the floor shows Sponsor");
    assert!(developer_at < sponsor_at, "badges follow the group order");
    let (_, profile) = http.page("/space/9542", None).await;
    assert!(profile.contains(&badge("Developer", "Works on the community software")));

    // Not shown after disabling; shown again after re-enabling.
    let honor_version = scalar(
        &http.f.db,
        &format!("SELECT version FROM gonof_common_honor WHERE id={developer}"),
    )
    .await;
    let (status, _, _) = http
        .form(
            &format!("/manage/honors/{developer}"),
            &admin,
            &[
                ("name", "Developer"),
                ("description", "Works on the community software"),
                ("displayorder", "0"),
                ("version", &honor_version.to_string()),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (_, page) = http.page(&topic_path, None).await;
    assert!(!page.contains(">Developer</span>") && page.contains(">Sponsor</span>"));
    // A disabled group cannot be newly granted.
    assert!(matches!(
        honors::grant(
            &http.f.db,
            actor(&http.f.db, 9541).await,
            9541,
            developer,
            None,
            identity::now()
        )
        .await,
        Err(IdentityError::Validation(
            "This honor group is disabled and cannot be granted"
        ))
    ));
    // Not shown after expiry.
    execute(
        &http.f.db,
        &format!(
            "UPDATE gonof_common_member_honor SET expires_at=1 WHERE uid=9542 AND honor_id={sponsor}"
        ),
    )
    .await;
    let (_, page) = http.page(&topic_path, None).await;
    assert!(!page.contains("data-honor-badge"));
    // Revocation requires a reason, and the record is gone afterwards.
    let (status, _, location) = http
        .form(
            "/manage/honors/members/9542/revoke",
            &admin,
            &[
                ("honor_id", &sponsor.to_string()),
                ("reason", "Test revocation"),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(
        location.as_deref(),
        Some("/manage/honors/members/9542?done=revoked")
    );
    assert_eq!(
        scalar(
            &http.f.db,
            &format!(
                "SELECT COUNT(*) FROM gonof_common_member_honor WHERE uid=9542 AND honor_id={sponsor}"
            )
        )
        .await,
        0
    );
    assert_eq!(
        scalar(
            &http.f.db,
            "SELECT COUNT(*) FROM gonof_common_log WHERE uid=9542 AND type LIKE 'identity.honor.%'"
        )
        .await,
        3,
        "two grants and one revocation"
    );
    // Administrators only.
    let global = http.login(9543).await;
    assert_eq!(
        http.page("/manage/honors", Some(&global)).await.0,
        StatusCode::FORBIDDEN
    );
    let (status, list) = http.page("/manage/honors", Some(&admin)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(list.contains(">Developer</a>") && list.contains(" · Disabled</p>"));
    let (status, holders) = http
        .page(&format!("/manage/honors/{developer}"), Some(&admin))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(holders.contains("group-9542"));
    http.f.cleanup().await.unwrap();
}
