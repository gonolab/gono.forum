use axum::{
    body::Body,
    extract::ConnectInfo,
    http::{HeaderMap, Request, StatusCode, header},
};
use gono_forum_web::{
    activities::ActivitySpec,
    content::{
        self,
        writes::{ContentWriter, NewReply, NewThread, WriteConfig},
    },
    db::{self, Database},
    identity::{self, Identity, IdentityError, SessionGrant},
    media::Media,
    privacy::{self, Access, NewRequest},
    test_support::TestFixture,
};
use http_body_util::BodyExt;
use std::io::Read;
use tower::ServiceExt;

// Keep one attachment waiting for object storage cleanup so the account deletion stops at deleting, to verify the
// key rotation at the start of deletion on its own.
#[tokio::test]
async fn anonymous_recent_members_drop_a_closing_account_before_final_cleanup() {
    use gono_forum_web::{cache::Lifetimes, content::home, runtime};

    let f = Fixture::new().await;
    let uid = f.user.member.uid;
    let now = identity::now();
    db::query("UPDATE gonof_common_member SET avatar=0,avatar_aid=NULL,space_visibility='public' WHERE uid=?")
        .bind(uid).execute(f.db.executor()).await.unwrap();
    let aid =
        db::query("INSERT INTO gonof_forum_attachment(uid,request_key,request_hash) VALUES(?,?,?)")
            .bind(uid)
            .bind(identity::random_token())
            .bind(identity::random_token())
            .execute(f.db.executor())
            .await
            .unwrap()
            .last_insert_id();
    db::query("INSERT INTO gonof_forum_attachment_0(aid,filename,filesize,mime_type,content_sha256,attachment,storage_profile_id,description,alt_text,dateline,updated_at,lifecycle,scan_status,scan_detail) VALUES(?,'attachment.pdf',10,'application/pdf',?,'object','test','','',?,?,'purging','clean','')")
        .bind(i64::try_from(aid).unwrap()).bind("a".repeat(64)).bind(now).bind(now).execute(f.db.executor()).await.unwrap();

    let cache = f.services.cache.timed(Lifetimes::from_settings(
        &runtime::read(&f.db).await.unwrap(),
    ));
    let listed = || async {
        home::layered_recent_members(&f.db, &f.services.global, &cache, None, (now, 8), None)
            .await
            .unwrap()
            .iter()
            .any(|m| m.uid == uid)
    };
    assert!(listed().await);
    privacy::submit(&f.db, &f.access(), f.request("close", "retain"), now)
        .await
        .unwrap();
    f.work(now + privacy::COOLING_SECONDS).await;
    let state: String =
        db::query_scalar("SELECT account_state FROM gonof_common_member WHERE uid=?")
            .bind(uid)
            .fetch_one(f.db.executor())
            .await
            .unwrap();
    assert_eq!(state, "deleting");
    assert!(!listed().await);
    f.services.cleanup().await.unwrap();
}

/// Account deletion removed a moderator row: after commit the moderator roster in the process cache is cleared and
/// broadcast, without waiting for the TTL.
#[tokio::test]
async fn closing_a_moderator_account_refreshes_the_roster_without_waiting_for_the_ttl() {
    let f = Fixture::new().await;
    let uid = f.user.member.uid;
    let now = identity::now();
    let fid: i64 = db::query_scalar("SELECT fid FROM gonof_forum_forum ORDER BY fid LIMIT 1")
        .fetch_one(f.db.executor())
        .await
        .unwrap();
    db::query("INSERT INTO gonof_forum_moderator(uid,fid) VALUES(?,?)")
        .bind(uid)
        .bind(fid)
        .execute(f.db.executor())
        .await
        .unwrap();
    // A TTL of one hour: if the roster were not cleared, the copy in memory would still be read after the deletion.
    let listed = || async {
        f.services
            .global
            .moderators(&f.db, 3600)
            .await
            .unwrap()
            .iter()
            .any(|entry| entry.uid == uid)
    };
    assert!(listed().await, "the roster is in the process cache");
    let id = privacy::submit(&f.db, &f.access(), f.request("close", "retain"), now)
        .await
        .unwrap();
    f.work(now + privacy::COOLING_SECONDS).await;
    let status: String =
        db::query_scalar("SELECT status FROM gonof_common_data_request WHERE id=?")
            .bind(id)
            .fetch_one(f.db.executor())
            .await
            .unwrap();
    assert_eq!(status, "completed");
    assert!(
        !listed().await,
        "the roster is refreshed as soon as the account deletion completes"
    );
    f.services.cleanup().await.unwrap();
}

const PASSWORD: &str = "Data-contract-password-2026!";
struct Fixture {
    services: TestFixture,
    db: Database,
    identity: Identity,
    admin: SessionGrant,
    user: SessionGrant,
    media: Media,
}
impl Fixture {
    async fn new() -> Self {
        let services = TestFixture::migrated().await.unwrap();
        let db = services.db.clone();
        content::seed_forums(&db).await.unwrap();
        gono_forum_web::test_support::raise_posting_quotas(&db)
            .await
            .unwrap();
        let identity = Identity::new(
            db.clone(),
            services.config.identity.clone(),
            services.root.clone(),
            // Shares the same process cache as `runtime::update`: after saving settings the registration precheck
            // reads the new value at once.
            services.global.clone(),
        );
        let admin_password = gono_forum_web::test_support::create_admin(&identity).await;
        let admin = identity.login("admin", &admin_password).await.unwrap();
        let invite = identity
            .generate_invite(admin.member.uid, &identity::random_token())
            .await
            .unwrap();
        let user = identity
            .register(
                "dataowner",
                "dataowner@example.test",
                PASSWORD,
                &invite.code,
            )
            .await
            .unwrap();
        db::query("UPDATE gonof_common_member SET emailstatus=1,regdate=1 WHERE uid=?")
            .bind(user.member.uid)
            .execute(db.executor())
            .await
            .unwrap();
        let media = Media::new(
            db.clone(),
            services.config.media.clone(),
            services.root.clone(),
        );
        Self {
            services,
            db,
            identity,
            admin,
            user,
            media,
        }
    }
    fn access(&self) -> Access {
        Access::Session(self.user.member.actor())
    }
    /// The member follows the administrator and the administrator follows the member.
    async fn follow_both_ways(&self, now: i64) {
        for (uid, followed) in [
            (self.user.member.uid, self.admin.member.uid),
            (self.admin.member.uid, self.user.member.uid),
        ] {
            db::query(
                "INSERT INTO gonof_home_member_follow(uid,followed_uid,dateline) VALUES(?,?,?)",
            )
            .bind(uid)
            .bind(followed)
            .bind(now)
            .execute(self.db.executor())
            .await
            .unwrap();
        }
    }
    /// The member is a verified blogger and has submitted two friend links: one pending, one on display (migration
    /// 26).
    async fn directory_rows(&self, now: i64) {
        db::query("INSERT INTO gonof_common_member_verification(uid,category,title,site_url,granted_by,granted_at,reviewed_at,expires_at) VALUES(?,'blogger','OWN_VERIFIED_TITLE','https://own.example.test/',?,?,?,NULL)")
            .bind(self.user.member.uid)
            .bind(self.admin.member.uid)
            .bind(now)
            .bind(now)
            .execute(self.db.executor())
            .await
            .unwrap();
        for (status, domain) in [
            ("pending", "pending.example.test"),
            ("active", "active.example.test"),
        ] {
            db::query("INSERT INTO gonof_common_friend_link(status,category,name,url,domain,live_domain,owner_uid,request_key,dateline,updated_at) VALUES(?,'blog',?,?,?,?,?,?,?,?)")
                .bind(status)
                .bind(format!("Site {status}"))
                .bind(format!("https://{domain}/"))
                .bind(domain)
                .bind(domain)
                .bind(self.user.member.uid)
                .bind(identity::random_token())
                .bind(now)
                .bind(now)
                .execute(self.db.executor())
                .await
                .unwrap();
        }
    }
    /// The member has two-factor authentication enabled: one status row and three recovery code digests; a GitHub
    /// account is bound as well.
    async fn two_factor_rows(&self, now: i64) {
        db::query("INSERT INTO gonof_common_member_external(provider,subject,uid,display,dateline,last_login_at) VALUES('github','4242',?,'octo-export',?,?)")
            .bind(self.user.member.uid)
            .bind(now)
            .bind(now)
            .execute(self.db.executor())
            .await
            .unwrap();
        db::query("INSERT INTO gonof_common_member_two_factor(uid,totp_secret,totp_step,totp_enabled_at,enabled_at) VALUES(?,'v1.0000000000000000.SECRET_CIPHERTEXT',1,?,?)")
            .bind(self.user.member.uid)
            .bind(now)
            .bind(now)
            .execute(self.db.executor())
            .await
            .unwrap();
        for _ in 0..3 {
            db::query(
                "INSERT INTO gonof_common_member_recovery_code(uid,code_hash,dateline) VALUES(?,?,?)",
            )
            .bind(self.user.member.uid)
            .bind(identity::random_token())
            .bind(now)
            .execute(self.db.executor())
            .await
            .unwrap();
        }
    }
    fn request(&self, kind: &str, mode: &str) -> NewRequest {
        NewRequest {
            kind: kind.into(),
            content_mode: mode.into(),
            request_key: identity::random_token(),
            password: PASSWORD.into(),
        }
    }
    async fn work(&self, now: i64) {
        privacy::process_pending(
            &self.db,
            &self.services.global,
            &self.media,
            &self.services.root,
            &self.services.config.identity.local_directory,
            90,
            now,
        )
        .await
        .unwrap();
    }
    async fn post(&self, text: &str) -> i64 {
        let fid: i64 = db::query_scalar("SELECT fid FROM gonof_forum_forum ORDER BY fid LIMIT 1")
            .fetch_one(self.db.executor())
            .await
            .unwrap();
        ContentWriter::new(self.db.clone(), WriteConfig::default())
            .publish(
                self.user.member.uid,
                NewThread {
                    visibility: Default::default(),
                    activity: Default::default(),
                    fid,
                    title: text.into(),
                    body: format!("{text} BODY"),
                    attachment_ids: vec![],
                    request_key: identity::random_token(),
                },
            )
            .await
            .unwrap()
            .pid
    }
    async fn scalar(&self, sql: &str) -> i64 {
        db::query_scalar(sql)
            .fetch_one(self.db.executor())
            .await
            .unwrap()
    }
    /// Another member with a verified email, registered through an invite like the data owner.
    async fn member(&self, username: &str) -> SessionGrant {
        let invite = self
            .identity
            .generate_invite(self.admin.member.uid, &identity::random_token())
            .await
            .unwrap();
        let grant = self
            .identity
            .register(
                username,
                &format!("{username}@example.test"),
                PASSWORD,
                &invite.code,
            )
            .await
            .unwrap();
        db::query("UPDATE gonof_common_member SET emailstatus=1,regdate=1 WHERE uid=?")
            .bind(grant.member.uid)
            .execute(self.db.executor())
            .await
            .unwrap();
        grant
    }
    /// `author` publishes a poll, a news thread and a one-click lottery (with a review step and an early draw) whose
    /// text all starts with `prefix`: the text that the export carries and permanent erasure overwrites. Returns the
    /// thread ids of the poll, the news thread and the lottery.
    async fn own_activities(&self, author: i64, prefix: &str, now: i64) -> (i64, i64, i64) {
        use gono_forum_web::activities::PrizeSpec;
        let fid: i64 = db::query_scalar("SELECT fid FROM gonof_forum_forum ORDER BY fid LIMIT 1")
            .fetch_one(self.db.executor())
            .await
            .unwrap();
        let writer = ContentWriter::new(self.db.clone(), WriteConfig::default())
            .with_codes(self.services.config.identity.two_factor.clone());
        let prize = |name: &str, note: &str| PrizeSpec {
            name: format!("{prefix} {name}"),
            winners: 1,
            kind: "item".into(),
            amount: 0,
            note: format!("{prefix} {note}"),
            codes: vec![],
        };
        let mut tids = vec![];
        for activity in [
            ActivitySpec {
                kind: 4,
                deadline: now + 3600,
                options: vec![format!("{prefix} option A"), format!("{prefix} option B")],
                ..Default::default()
            },
            ActivitySpec {
                kind: 5,
                source_url: "https://example.test/private-source".into(),
                source_name: format!("{prefix} source"),
                source_author: format!("{prefix} byline"),
                ..Default::default()
            },
            ActivitySpec {
                kind: 3,
                deadline: now + 3600,
                entry_mode: "click".into(),
                requirements: vec![format!("{prefix} condition")],
                review: true,
                early_draw: true,
                prizes: vec![
                    prize("prize A", "call me on 555-0100"),
                    prize("prize B", "note B"),
                ],
                ..Default::default()
            },
        ] {
            let published = writer
                .publish_at(
                    author,
                    NewThread {
                        visibility: Default::default(),
                        activity,
                        fid,
                        title: "Activity thread".into(),
                        body: "Activity body".into(),
                        attachment_ids: vec![],
                        request_key: identity::random_token(),
                    },
                    now,
                )
                .await
                .unwrap();
            tids.push(published.tid);
        }
        (tids[0], tids[1], tids[2])
    }
    /// The archive of a finished export request.
    async fn archive(&self, id: i64, now: i64) -> zip::ZipArchive<std::fs::File> {
        let dir = privacy::directory(
            &self.services.root,
            &self.services.config.identity.local_directory,
        );
        let (path, _) = privacy::download(&self.db, &self.access(), id, &dir, now)
            .await
            .unwrap();
        zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap()
    }
    /// One processed restore appeal of the member: the export includes the appeal and the
    /// outcome, without the hider or the handler; after account deletion it is kept like reports and appeals.
    async fn restore_request(&self, pid: i64, now: i64) {
        db::query("INSERT INTO gonof_forum_restore_request(uid,pid,state,hidden_by,reason,status,handler_uid,resolution,handled_at,dateline,request_key) VALUES(?,?,'hidden',?,'RESTORE_REASON','rejected',?,'RESTORE_RESOLUTION',?,?,?)")
            .bind(self.user.member.uid)
            .bind(pid)
            .bind(self.admin.member.uid)
            .bind(self.admin.member.uid)
            .bind(now)
            .bind(now)
            .bind(identity::random_token())
            .execute(self.db.executor())
            .await
            .unwrap();
    }
}
#[tokio::test]
async fn data_export_is_private_complete_and_idempotent() {
    let f = Fixture::new().await;
    let now = identity::now();
    let own_pid = f.post("OWN_EXPORT_TEXT").await;
    // The custom style is exported with the reading preference row.
    gono_forum_web::custom_css::save(
        &f.db,
        &f.services.cache,
        f.user.member.actor(),
        Some(".own-export { color: teal; }"),
        0,
    )
    .await
    .unwrap();
    // One followed member in each direction and one downvote by the member: the export only
    // includes the people the member follows.
    f.follow_both_ways(now).await;
    f.two_factor_rows(now).await;
    f.restore_request(own_pid, now).await;
    f.directory_rows(now).await;
    db::query("INSERT INTO gonof_forum_post_downvote(pid,uid,dateline) VALUES(?,?,?)")
        .bind(own_pid)
        .bind(f.user.member.uid)
        .bind(now)
        .execute(f.db.executor())
        .await
        .unwrap();
    let session = f
        .identity
        .session(&f.user.session_token)
        .await
        .unwrap()
        .unwrap();
    f.identity.touch_online(&session, "192.0.2.12").await;
    let request = f.request("export", "retain");
    let id = privacy::submit(&f.db, &f.access(), request.clone(), now)
        .await
        .unwrap();
    assert_eq!(
        privacy::submit(&f.db, &f.access(), request, now)
            .await
            .unwrap(),
        id
    );
    f.work(now).await;
    let dir = privacy::directory(
        &f.services.root,
        &f.services.config.identity.local_directory,
    );
    assert!(matches!(
        privacy::download(
            &f.db,
            &Access::Session(f.admin.member.actor()),
            id,
            &dir,
            now
        )
        .await,
        Err(IdentityError::NotFound)
    ));
    let (path, bytes) = privacy::download(&f.db, &f.access(), id, &dir, now)
        .await
        .unwrap();
    assert!(bytes > 0);
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut profile = String::new();
    archive
        .by_name("profile.json")
        .unwrap()
        .read_to_string(&mut profile)
        .unwrap();
    assert!(profile.contains("dataowner@example.test"));
    assert!(!profile.contains("password"));
    let mut posts = String::new();
    archive
        .by_name("posts.json")
        .unwrap()
        .read_to_string(&mut posts)
        .unwrap();
    assert!(posts.contains("OWN_EXPORT_TEXT"));
    assert!(!posts.contains(PASSWORD));
    let messages: serde_json::Value =
        serde_json::from_reader(archive.by_name("sent_messages.json").unwrap()).unwrap();
    assert_eq!(messages, serde_json::json!([]));
    // Last online is also the member's data; the export has one entry.
    let presence: serde_json::Value =
        serde_json::from_reader(archive.by_name("presence.json").unwrap()).unwrap();
    assert_eq!(presence.as_array().map(Vec::len), Some(1), "{presence}");
    let follows: serde_json::Value =
        serde_json::from_reader(archive.by_name("member_follows.json").unwrap()).unwrap();
    assert_eq!(follows.as_array().map(Vec::len), Some(1), "{follows}");
    assert_eq!(
        follows[0]["followed_uid"], f.admin.member.uid,
        "only the members this member follows"
    );
    // Two-factor authentication only exports whether it is enabled and when, plus each remaining recovery code's
    // creation time, without the secret ciphertext or digests.
    let two_factor: serde_json::Value =
        serde_json::from_reader(archive.by_name("two_factor.json").unwrap()).unwrap();
    assert_eq!(two_factor.as_array().map(Vec::len), Some(1), "{two_factor}");
    assert_eq!(two_factor[0]["enabled_at"], now);
    let codes: serde_json::Value =
        serde_json::from_reader(archive.by_name("recovery_codes.json").unwrap()).unwrap();
    assert_eq!(codes.as_array().map(Vec::len), Some(3), "{codes}");
    for text in [two_factor.to_string(), codes.to_string()] {
        assert!(!text.contains("SECRET_CIPHERTEXT") && !text.contains("code_hash"));
    }
    // Third-party bindings export the provider, external ID, display name and times.
    let external: serde_json::Value =
        serde_json::from_reader(archive.by_name("external_accounts.json").unwrap()).unwrap();
    assert_eq!(external.as_array().map(Vec::len), Some(1), "{external}");
    assert_eq!(external[0]["provider"], "github");
    assert_eq!(external[0]["subject"], "4242");
    assert_eq!(external[0]["display"], "octo-export");
    let downvotes: serde_json::Value =
        serde_json::from_reader(archive.by_name("downvotes.json").unwrap()).unwrap();
    assert_eq!(downvotes.as_array().map(Vec::len), Some(1), "{downvotes}");
    assert_eq!(downvotes[0]["pid"], own_pid);
    let restorations: serde_json::Value =
        serde_json::from_reader(archive.by_name("restorations.json").unwrap()).unwrap();
    assert_eq!(
        restorations.as_array().map(Vec::len),
        Some(1),
        "{restorations}"
    );
    assert_eq!(restorations[0]["pid"], own_pid);
    assert_eq!(restorations[0]["reason"], "RESTORE_REASON");
    assert_eq!(restorations[0]["resolution"], "RESTORE_RESOLUTION");
    assert!(
        restorations[0].get("hidden_by").is_none() && restorations[0].get("handler_uid").is_none()
    );
    assert!(
        presence[0]["last_seen"]
            .as_i64()
            .is_some_and(|seen| seen >= now)
    );
    // The growth account and ledger are also the member's data: one thread posted, growth 3, one posting ledger
    // entry.
    let growth: serde_json::Value =
        serde_json::from_reader(archive.by_name("growth.json").unwrap()).unwrap();
    assert_eq!(growth[0]["growth"], 3, "{growth}");
    let growth_ledger: serde_json::Value =
        serde_json::from_reader(archive.by_name("growth_ledger.json").unwrap()).unwrap();
    assert_eq!(growth_ledger.as_array().map(Vec::len), Some(1));
    assert_eq!(growth_ledger[0]["kind"], "publish", "{growth_ledger}");
    // Verification and friend links: content the member can see, without the administrators who granted or handled
    // it.
    let verification: serde_json::Value =
        serde_json::from_reader(archive.by_name("verification.json").unwrap()).unwrap();
    assert_eq!(
        verification.as_array().map(Vec::len),
        Some(1),
        "{verification}"
    );
    assert_eq!(verification[0]["title"], "OWN_VERIFIED_TITLE");
    assert!(
        verification[0].get("granted_by").is_none(),
        "{verification}"
    );
    let friend_links: serde_json::Value =
        serde_json::from_reader(archive.by_name("friend_links.json").unwrap()).unwrap();
    assert_eq!(
        friend_links.as_array().map(Vec::len),
        Some(2),
        "{friend_links}"
    );
    assert!(
        friend_links[0].get("handler_uid").is_none(),
        "{friend_links}"
    );
    let reading: serde_json::Value =
        serde_json::from_reader(archive.by_name("reading_preferences.json").unwrap()).unwrap();
    assert_eq!(
        reading[0]["custom_css"], ".own-export { color: teal; }",
        "{reading}"
    );
    assert_eq!(reading[0]["custom_css_version"], 1);
    assert!(reading[0].get("language").is_none());
    assert_eq!(reading[0]["timezone"], serde_json::Value::Null);
    // The download period runs from the archive's completion.
    let finished = f
        .scalar(&format!(
            "SELECT finished_at FROM gonof_common_data_request WHERE id={id}"
        ))
        .await;
    let expires = finished + privacy::EXPORT_SECONDS;
    assert!(
        privacy::download(&f.db, &f.access(), id, &dir, expires - 1)
            .await
            .is_ok()
    );
    assert!(matches!(
        privacy::download(&f.db, &f.access(), id, &dir, expires).await,
        Err(IdentityError::NotFound)
    ));
    f.services.cleanup_ref().await.unwrap();
}
/// Revisions are fetched once per page of posts instead of one query per post; the export content
/// is unchanged.
#[tokio::test]
async fn data_export_reads_revisions_per_page_instead_of_per_post() {
    use gono_forum_web::debug::trace::{self, RequestProfile};
    use std::sync::Arc;
    let f = Fixture::new().await;
    let now = identity::now();
    let mut pids = Vec::new();
    for index in 0..3 {
        pids.push(f.post(&format!("REVISION_EXPORT_{index}")).await);
    }
    for pid in &pids[..2] {
        for version in 1..=2 {
            db::query("INSERT INTO gonof_forum_post_revision(pid,version,editor_uid,subject,message,dateline) VALUES(?,?,?,?,?,?)")
                .bind(pid)
                .bind(version)
                .bind(f.user.member.uid)
                .bind(format!("Old title {pid}-{version}"))
                .bind(format!("Old body {pid}-{version}"))
                .bind(now)
                .execute(f.db.executor())
                .await
                .unwrap();
        }
    }
    let id = privacy::submit(&f.db, &f.access(), f.request("export", "retain"), now)
        .await
        .unwrap();
    let profile = Arc::new(RequestProfile::new());
    // Must be boxed: the data request future is about 77 KB and the three layers of `trace::scope` each store a copy
    // by value; unboxed it is about 232 KB, which overflows the 2 MiB test thread stack on every debug build run.
    trace::scope(profile.clone(), Box::pin(f.work(now))).await;
    let revision_queries = profile
        .snapshot()
        .iter()
        .filter(|record| record.sql.contains("FROM gonof_forum_post_revision"))
        .count();
    assert_eq!(
        revision_queries, 1,
        "one page of posts reads the revisions once"
    );
    let dir = privacy::directory(
        &f.services.root,
        &f.services.config.identity.local_directory,
    );
    let (path, _) = privacy::download(&f.db, &f.access(), id, &dir, now)
        .await
        .unwrap();
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let revisions: serde_json::Value = serde_json::from_reader(
        archive
            .by_name(&format!("revisions/post-{}.json", pids[0]))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        revisions,
        serde_json::json!([
            {"version": 1, "subject": format!("Old title {}-1", pids[0]), "message": format!("Old body {}-1", pids[0]), "dateline": now},
            {"version": 2, "subject": format!("Old title {}-2", pids[0]), "message": format!("Old body {}-2", pids[0]), "dateline": now},
        ])
    );
    let empty: serde_json::Value = serde_json::from_reader(
        archive
            .by_name(&format!("revisions/post-{}.json", pids[2]))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(empty, serde_json::json!([]));
    f.services.cleanup_ref().await.unwrap();
}
#[tokio::test]
async fn data_export_includes_only_sent_messages_across_batches_and_conversations() {
    let f = Fixture::new().await;
    let now = identity::now();
    let other_uid = 9201_i64;
    db::query(
        "INSERT INTO gonof_common_member(uid,username,regdate) VALUES(?,'message-recipient',1)",
    )
    .bind(other_uid)
    .execute(f.db.executor())
    .await
    .unwrap();
    let mut expected = Vec::new();
    // More than one batch's limit, sent interleaved across two conversations; received messages are also
    // interspersed in the ID sequence.
    for n in 0..101 {
        let counterpart_uid = if n % 2 == 0 {
            f.admin.member.uid
        } else {
            other_uid
        };
        let body = format!("My message {n}\nplain text <b>Coins 🍓</b> \"quotes\"");
        let thread_id = gono_forum_web::messages::send(
            &f.db,
            f.user.member.actor(),
            counterpart_uid,
            &body,
            &identity::random_token(),
        )
        .await
        .unwrap();
        expected.push((thread_id, counterpart_uid, body));
        gono_forum_web::messages::send(
            &f.db,
            f.admin.member.actor(),
            f.user.member.uid,
            &format!("RECEIVED_PRIVATE_MESSAGE_{n}"),
            &identity::random_token(),
        )
        .await
        .unwrap();
    }
    gono_forum_web::messages::send(
        &f.db,
        f.admin.member.actor(),
        other_uid,
        "UNRELATED_PRIVATE_MESSAGE",
        &identity::random_token(),
    )
    .await
    .unwrap();
    let unread_before = gono_forum_web::messages::unread_conversations(&f.db, f.user.member.uid)
        .await
        .unwrap();
    assert_eq!(unread_before, 1);

    let id = privacy::submit(&f.db, &f.access(), f.request("export", "retain"), now)
        .await
        .unwrap();
    f.work(now).await;
    let dir = privacy::directory(
        &f.services.root,
        &f.services.config.identity.local_directory,
    );
    let (path, _) = privacy::download(&f.db, &f.access(), id, &dir, now)
        .await
        .unwrap();
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let messages: Vec<serde_json::Value> =
        serde_json::from_reader(archive.by_name("sent_messages.json").unwrap()).unwrap();
    assert_eq!(messages.len(), expected.len());
    let mut previous_id = 0;
    for (message, (thread_id, counterpart_uid, body)) in messages.iter().zip(expected) {
        let message_id = message["id"].as_i64().unwrap();
        assert!(message_id > previous_id);
        previous_id = message_id;
        assert_eq!(message["thread_id"], thread_id);
        assert_eq!(message["sender_uid"], f.user.member.uid);
        assert_eq!(message["counterpart_uid"], counterpart_uid);
        assert_eq!(message["body"], body);
        assert!(message["dateline"].as_i64().unwrap() >= now);
        assert!(message.get("event_key").is_none());
    }
    // The export must not treat the conversation as opened, nor affect unread alerts.
    assert_eq!(
        gono_forum_web::messages::unread_conversations(&f.db, f.user.member.uid)
            .await
            .unwrap(),
        unread_before
    );
    f.services.cleanup_ref().await.unwrap();
}
#[tokio::test]
async fn independent_erasure_preserves_new_posts_and_cannot_restore_old_text() {
    let f = Fixture::new().await;
    let now = identity::now();
    let old = f.post("ERASE_OLD").await;
    let id = privacy::submit(&f.db, &f.access(), f.request("erase", "erase"), now)
        .await
        .unwrap();
    let mut tx = f.db.begin_write().await.unwrap();
    assert!(
        !privacy::funding_allowed(&f.db, &mut tx, f.user.member.uid)
            .await
            .unwrap()
    );
    tx.commit().await.unwrap();
    let new = f.post("KEEP_NEW").await;
    f.work(now).await;
    assert_eq!(
        f.scalar(&format!(
            "SELECT erased_at FROM gonof_forum_post WHERE pid={old}"
        ))
        .await,
        0
    );
    f.work(now + privacy::COOLING_SECONDS).await;
    assert!(
        f.scalar(&format!(
            "SELECT erased_at FROM gonof_forum_post WHERE pid={old}"
        ))
        .await
            > 0
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT erased_at FROM gonof_forum_post WHERE pid={new}"
        ))
        .await,
        0
    );
    // Erasure is irreversible: the restore and edit entry points reject erased posts under the lock, and `erased_at`
    // is only written by the erase statement (pinned by the source scan).
    let editor = content::edits::ContentEditor::new(
        f.db.clone(),
        f.services.config.content.clone(),
        f.services.config.media.orphan_seconds,
    );
    let versions = db::query("SELECT p.version,t.version AS thread_version FROM gonof_forum_post p JOIN gonof_forum_thread t ON t.tid=p.tid WHERE p.pid=?")
        .bind(old)
        .fetch_one(f.db.executor())
        .await
        .unwrap();
    let (post_version, thread_version): (i64, i64) = (
        versions.try_get("version").unwrap(),
        versions.try_get("thread_version").unwrap(),
    );
    for action in [
        content::edits::ContentAction::Restore,
        content::edits::ContentAction::Unhide,
    ] {
        assert!(matches!(
            editor
                .change(
                    f.admin.member.actor(),
                    content::edits::ChangeRequest {
                        pid: old,
                        action,
                        expected_post_version: post_version,
                        expected_thread_version: thread_version,
                        request_key: identity::random_token(),
                        reason: "Try to restore an erased post".into(),
                    },
                )
                .await,
            Err(content::writes::ContentWriteError::Forbidden(_))
        ));
    }
    assert!(matches!(
        editor
            .edit(
                f.admin.member.actor(),
                content::edits::EditRequest {
                    pid: old,
                    expected_post_version: post_version,
                    expected_thread_version: thread_version,
                    title: None,
                    body: Some("RESTORE".into()),
                    request_key: identity::random_token(),
                    reason: "Try to rewrite an erased post".into(),
                    visibility: None,
                },
            )
            .await,
        Err(content::writes::ContentWriteError::Forbidden(_))
    ));
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_forum_post WHERE pid={old} AND erased_at>0 AND message=''"
        ))
        .await,
        1
    );
    assert!(
        f.identity
            .session(&f.user.session_token)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        privacy::cancel(&f.db, &f.access(), id, now + privacy::COOLING_SECONDS)
            .await
            .is_err()
    );
    f.work(now + privacy::COOLING_SECONDS + 1).await;
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_data_request WHERE status='completed'")
            .await,
        1
    );
    // Erasure works like withdrawal: growth from the old post is deducted, the new post is unchanged, and the account
    // matches the ledger.
    let uid = f.user.member.uid;
    assert_eq!(
        f.scalar(&format!(
            "SELECT credited FROM gonof_common_growth_post WHERE pid={old}"
        ))
        .await,
        0
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_growth_ledger WHERE pid={old} AND kind='revoke' AND delta=-3"
        ))
        .await,
        1
    );
    assert_eq!(gono_forum_web::growth::growth(&f.db, uid).await.unwrap(), 3);
    assert!(
        gono_forum_web::growth::reconcile(&f.db)
            .await
            .unwrap()
            .is_consistent()
    );
    f.services.cleanup_ref().await.unwrap();
}
#[tokio::test]
async fn closure_keeps_uid_and_ledger_while_revoking_identity_and_preserving_content() {
    let f = Fixture::new().await;
    let now = identity::now();
    // The bound mobile number and the codes sent to it are cleared on account deletion, together with the files local
    // SMS delivery wrote for them; files of codes sent to other numbers stay.
    db::query("UPDATE gonof_common_member SET mobile='+8613500135000' WHERE uid=?")
        .bind(f.user.member.uid)
        .execute(f.db.executor())
        .await
        .unwrap();
    let sms_dir = f.services.root.join(".local/sms");
    std::fs::create_dir_all(&sms_dir).unwrap();
    let mut sms_files = Vec::new();
    for mobile in ["+8613500135000", "+8613500135001"] {
        let id = db::query("INSERT INTO gonof_common_sms_code(mobile,purpose,code_hash,dateline,expires_at) VALUES(?,'login',?,?,?)")
            .bind(mobile)
            .bind("a".repeat(64))
            .bind(now)
            .bind(now + 300)
            .execute(f.db.executor())
            .await
            .unwrap()
            .last_insert_id();
        let path = sms_dir.join(format!("{id}.txt"));
        std::fs::write(&path, format!("Mobile number: {mobile}\nCode: 135790\n")).unwrap();
        sms_files.push(path);
    }
    // The third-party binding (the GitHub one in `two_factor_rows`) is deleted on account deletion; afterwards signing
    // in with the same GitHub account counts as a new registration.
    let mut old_session = f
        .identity
        .session(&f.user.session_token)
        .await
        .unwrap()
        .unwrap();
    f.identity.touch_online(&old_session, "192.0.2.10").await;
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_session WHERE uid={}",
            f.user.member.uid
        ))
        .await,
        1
    );
    // Last online is only written back when expired sessions are cleaned up (or on export)
    // (docs/queries.md, "Last online"); requests only write the online table.
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_member_presence WHERE uid={}",
            f.user.member.uid
        ))
        .await,
        0
    );
    let eventid: i64 = db::query_scalar("SELECT eventid FROM gonof_common_outbox WHERE aggregate_type='member' AND aggregate_id=? ORDER BY eventid LIMIT 1")
        .bind(f.user.member.uid).fetch_one(f.db.executor()).await.unwrap();
    let mail_dir = f.services.root.join(".local/mail");
    let prepared = mail_dir.join("prepared");
    std::fs::create_dir_all(&prepared).unwrap();
    let temp = format!(".{eventid}.{}.tmp", identity::random_token());
    for dir in [&mail_dir, &prepared] {
        std::fs::write(dir.join(format!("{eventid}.eml")), "PRIVATE_BODY").unwrap();
        std::fs::write(dir.join(&temp), "PRIVATE_PARTIAL").unwrap();
    }
    let pid = f.post("ANONYMOUS_RETAIN").await;
    f.follow_both_ways(now).await;
    f.two_factor_rows(now).await;
    f.restore_request(pid, now).await;
    f.directory_rows(now).await;
    db::query("INSERT INTO gonof_forum_post_downvote(pid,uid,dateline) VALUES(?,?,?)")
        .bind(pid)
        .bind(f.user.member.uid)
        .bind(now)
        .execute(f.db.executor())
        .await
        .unwrap();
    // Before deletion, like the administrator's thread and add a leaderboard snapshot row; on deletion this member's
    // row in the snapshot disappears at once. The like stays and the administrator's growth is unchanged (account
    // deletion does not clean up likes, favorites and downvotes given).
    let admin_pid = ContentWriter::new(f.db.clone(), WriteConfig::default())
        .publish(
            f.admin.member.uid,
            NewThread {
                visibility: Default::default(),
                activity: Default::default(),
                fid: db::query_scalar("SELECT fid FROM gonof_forum_forum ORDER BY fid LIMIT 1")
                    .fetch_one(f.db.executor())
                    .await
                    .unwrap(),
                title: "ADMIN_LIKED".into(),
                body: "ADMIN_LIKED BODY".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
        )
        .await
        .unwrap()
        .pid;
    gono_forum_web::interaction::Interaction::new(f.db.clone())
        .set_reaction(
            f.user.member.actor(),
            admin_pid,
            true,
            &identity::random_token(),
        )
        .await
        .unwrap();
    assert_eq!(
        gono_forum_web::growth::growth(&f.db, f.admin.member.uid)
            .await
            .unwrap(),
        4
    );
    db::query("INSERT INTO gonof_rank_snapshot(board,period,position,uid,score,computed_at) VALUES('threads','all',1,?,1,?)")
        .bind(f.user.member.uid)
        .bind(now)
        .execute(f.db.executor())
        .await
        .unwrap();
    db::query("INSERT INTO gonof_common_credit_account(uid,available) VALUES(?,7)")
        .bind(f.user.member.uid)
        .execute(f.db.executor())
        .await
        .unwrap();
    db::query("INSERT INTO gonof_common_credit_ledger(event_key,uid,available_delta,frozen_delta,available_after,frozen_after,kind,dateline) VALUES('fixture',?,7,0,7,0,'adjust',?)").bind(f.user.member.uid).bind(now).execute(f.db.executor()).await.unwrap();
    // One unread notification: on account deletion notifications are deleted in bulk, and the unread count must drop
    // to zero with them (maintained by notifications::record).
    gono_forum_web::test_support::insert_notification(
        &f.db,
        &gono_forum_web::notifications::record::NewNotice {
            uid: f.user.member.uid,
            authorid: f.user.member.uid,
            kind: gono_forum_web::notifications::record::Kind::Reply,
            target: gono_forum_web::notifications::record::Target::Post(pid),
            event_key: "closure-unread",
            reason: "",
            dateline: now,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        f.scalar(&format!(
            "SELECT unread_notifications FROM gonof_common_member WHERE uid={}",
            f.user.member.uid
        ))
        .await,
        1
    );
    privacy::submit(&f.db, &f.access(), f.request("close", "retain"), now)
        .await
        .unwrap();
    f.work(now + privacy::COOLING_SECONDS).await;
    for dir in [&mail_dir, &prepared] {
        assert!(!dir.join(format!("{eventid}.eml")).exists());
        assert!(!dir.join(&temp).exists());
    }
    // Bypass the in-process throttle to emulate a late pre-closure request.
    old_session.online_id = identity::random_token();
    f.identity.touch_online(&old_session, "192.0.2.11").await;
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_session WHERE uid={}",
            f.user.member.uid
        ))
        .await,
        0
    );
    // Running the account deletion removes last online, and late requests from before the deletion cannot write it
    // back either.
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_member_presence WHERE uid={}",
            f.user.member.uid
        ))
        .await,
        0
    );
    assert!(
        f.identity
            .session(&f.user.session_token)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(f.scalar(&format!("SELECT COUNT(*) FROM gonof_common_member WHERE uid={} AND account_state='deleted' AND email IS NULL AND mobile IS NULL AND password IS NULL",f.user.member.uid)).await,1);
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_sms_code WHERE mobile='+8613500135000'")
            .await,
        0
    );
    assert!(
        !sms_files[0].exists(),
        "the local SMS file of the member's number is removed"
    );
    assert!(
        sms_files[1].exists(),
        "a code sent to another number keeps its file"
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_member_external WHERE uid={}",
            f.user.member.uid
        ))
        .await,
        0
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT unread_notifications + (SELECT COUNT(*) FROM gonof_home_notification WHERE uid={0}) FROM gonof_common_member WHERE uid={0}",
            f.user.member.uid
        ))
        .await,
        0,
        "account deletion removes all notifications and zeroes the unread count"
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_forum_restore_request WHERE uid={}",
            f.user.member.uid
        ))
        .await,
        1,
        "restore requests are kept, like reports and appeals"
    );
    // Account deletion removes followed members in both directions; the member's downvotes stay (see above).
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_forum_post_downvote WHERE uid={}",
            f.user.member.uid
        ))
        .await,
        1,
        "account deletion keeps the member's downvotes"
    );
    for (sql, what) in [
        (
            "SELECT COUNT(*) FROM gonof_home_member_follow WHERE uid={}",
            "members this member follows",
        ),
        (
            "SELECT COUNT(*) FROM gonof_home_member_follow WHERE followed_uid={}",
            "follows of this member by others",
        ),
        (
            "SELECT COUNT(*) FROM gonof_common_member_two_factor WHERE uid={}",
            "two-factor state",
        ),
        (
            "SELECT COUNT(*) FROM gonof_common_member_recovery_code WHERE uid={}",
            "recovery codes",
        ),
    ] {
        assert_eq!(
            f.scalar(&sql.replace("{}", &f.user.member.uid.to_string()))
                .await,
            0,
            "{what} remain after account deletion"
        );
    }
    assert_eq!(
        f.scalar(&format!(
            "SELECT available FROM gonof_common_credit_account WHERE uid={}",
            f.user.member.uid
        ))
        .await,
        0
    );
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_credit_ledger WHERE kind='privacy_close'")
            .await,
        1
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT erased_at FROM gonof_forum_post WHERE pid={pid}"
        ))
        .await,
        0
    );
    assert_eq!(
        f.scalar(
            "SELECT COUNT(*) FROM gonof_common_member_username_history WHERE username='dataowner'"
        )
        .await,
        1
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_forum_post_reaction WHERE uid={}",
            f.user.member.uid
        ))
        .await,
        1,
        "account deletion keeps the likes given"
    );
    assert_eq!(
        gono_forum_web::growth::growth(&f.db, f.admin.member.uid)
            .await
            .unwrap(),
        4,
        "the likes stay, so the administrator's growth is unchanged"
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_rank_snapshot WHERE uid={}",
            f.user.member.uid
        ))
        .await,
        0
    );
    // Verification is deleted; pending friend link applications are withdrawn and release the website, those on
    // display stay.
    let uid = f.user.member.uid;
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_member_verification WHERE uid={uid}"
        ))
        .await,
        0
    );
    assert_eq!(
        f.scalar(&format!("SELECT COUNT(*) FROM gonof_common_friend_link WHERE owner_uid={uid} AND domain='pending.example.test' AND status='withdrawn' AND live_domain IS NULL")).await,
        1
    );
    assert_eq!(
        f.scalar(&format!("SELECT COUNT(*) FROM gonof_common_friend_link WHERE owner_uid={uid} AND domain='active.example.test' AND status='active' AND live_domain='active.example.test'")).await,
        1
    );
    // The member's growth account and ledger stay (the historical ledger is not deleted); with the posts kept, growth
    // is unchanged too.
    assert_eq!(
        gono_forum_web::growth::growth(&f.db, f.user.member.uid)
            .await
            .unwrap(),
        3
    );
    assert!(
        gono_forum_web::growth::reconcile(&f.db)
            .await
            .unwrap()
            .is_consistent()
    );
    f.work(now + privacy::COOLING_SECONDS + 1).await;
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_credit_ledger WHERE kind='privacy_close'")
            .await,
        1
    );
    f.services.cleanup_ref().await.unwrap();
}
#[tokio::test]
async fn frozen_email_access_is_domain_separated_single_use_and_owner_scoped() {
    let f = Fixture::new().await;
    let now = identity::now();
    // Members who are not frozen get no ticket; they can only submit after signing in with their current password.
    privacy::request_access(&f.db, "dataowner@example.test", now)
        .await
        .unwrap();
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_member_token WHERE purpose='privacy_access'")
            .await,
        0
    );
    db::query("UPDATE gonof_common_member SET `freeze`=1,credentials_version=credentials_version+1 WHERE uid=?").bind(f.user.member.uid).execute(f.db.executor()).await.unwrap();
    privacy::request_access(&f.db, "dataowner@example.test", now)
        .await
        .unwrap();
    let payload: String = db::query_scalar(
        "SELECT payload FROM gonof_common_outbox WHERE event_type='privacy_access'",
    )
    .fetch_one(f.db.executor())
    .await
    .unwrap();
    let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
    let ticket = payload["token"].as_str().unwrap();
    privacy::check_access(&f.db, ticket, now).await.unwrap();
    assert!(
        privacy::list(&f.db, &Access::Token(ticket.into()), 0, now)
            .await
            .is_err()
    );
    let (raw, _) = privacy::exchange_access(&f.db, ticket, now).await.unwrap();
    assert!(privacy::exchange_access(&f.db, ticket, now).await.is_err());
    let access = Access::Token(raw.clone());
    assert_eq!(
        privacy::list(&f.db, &access, 0, now).await.unwrap().0,
        f.user.member.uid
    );
    let id = privacy::submit(&f.db, &access, f.request("erase", "erase"), now)
        .await
        .unwrap();
    privacy::cancel(&f.db, &access, id, now).await.unwrap();
    // After unfreezing, the redeemed credential becomes invalid at once, back to the normal sign-in-plus-password
    // entry.
    db::query("UPDATE gonof_common_member SET `freeze`=0 WHERE uid=?")
        .bind(f.user.member.uid)
        .execute(f.db.executor())
        .await
        .unwrap();
    assert!(privacy::list(&f.db, &access, 0, now).await.is_err());
    assert!(
        privacy::submit(&f.db, &access, f.request("erase", "erase"), now)
            .await
            .is_err()
    );
    db::query("UPDATE gonof_common_member SET `freeze`=1 WHERE uid=?")
        .bind(f.user.member.uid)
        .execute(f.db.executor())
        .await
        .unwrap();
    privacy::exit_access(&f.db, &raw, now).await.unwrap();
    assert!(privacy::list(&f.db, &access, 0, now).await.is_err());
    f.services.cleanup_ref().await.unwrap();
}

/// Requesting a new ticket only voids old tickets not yet redeemed: someone who knows the e-mail requesting again and
/// again must not invalidate the session the member already exchanged for a cookie.
/// An account closure woken before its cooling period ends stays cooling: the generic task retry keeps the request's
/// own time, and a task that runs early anyway waits again instead of turning the request into a failure.
#[tokio::test]
async fn closures_keep_their_cooling_period_through_retries() {
    let f = Fixture::new().await;
    let now = identity::now();
    let id = privacy::submit(&f.db, &f.access(), f.request("close", "retain"), now)
        .await
        .unwrap();
    let due = now + privacy::COOLING_SECONDS;
    let task = |field: &str| {
        format!("SELECT {field} FROM gonof_common_outbox WHERE event_key='privacy.request:{id}'")
    };
    let status = || async {
        db::query_scalar::<String>("SELECT status FROM gonof_common_data_request WHERE id=?")
            .bind(id)
            .fetch_one(f.db.executor())
            .await
            .unwrap()
    };
    let eventid = f.scalar(&task("eventid")).await;
    gono_forum_web::administration::retry_task(
        &f.db,
        f.admin.member.actor(),
        eventid,
        f.scalar(&task("attempts")).await,
        f.scalar(&task("available_at")).await,
        0,
        "pending",
        "Checking the queue",
    )
    .await
    .unwrap();
    assert_eq!(f.scalar(&task("available_at")).await, due);

    db::query("UPDATE gonof_common_outbox SET available_at=? WHERE eventid=?")
        .bind(now)
        .bind(eventid)
        .execute(f.db.executor())
        .await
        .unwrap();
    f.work(now).await;
    assert_eq!(status().await, "cooling");
    assert_eq!(f.scalar(&task("available_at")).await, due);
    assert_eq!(
        f.scalar(&task("COUNT(*)")).await,
        f.scalar(&format!("{} AND last_error IS NULL", task("COUNT(*)")))
            .await,
        "an early run records no error"
    );

    f.work(due).await;
    assert_eq!(status().await, "completed");
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn new_email_tickets_revoke_unused_tickets_but_keep_exchanged_sessions() {
    let f = Fixture::new().await;
    let now = identity::now();
    db::query("UPDATE gonof_common_member SET `freeze`=1,credentials_version=credentials_version+1 WHERE uid=?").bind(f.user.member.uid).execute(f.db.executor()).await.unwrap();
    let ticket = |at: i64| {
        let db = f.db.clone();
        async move {
            privacy::request_access(&db, "dataowner@example.test", at)
                .await
                .unwrap();
            let payload: String = db::query_scalar("SELECT payload FROM gonof_common_outbox WHERE event_type='privacy_access' ORDER BY eventid DESC LIMIT 1")
                .fetch_one(db.executor())
                .await
                .unwrap();
            let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
            payload["token"].as_str().unwrap().to_owned()
        }
    };
    // Redeemed in the same second as issued: the cookie row's issue and expiry times are identical to a new ticket's,
    // and it must still be recognized.
    let first = ticket(now).await;
    let (session, _) = privacy::exchange_access(&f.db, &first, now).await.unwrap();
    let access = Access::Token(session);
    // One ticket per account per minute; after a minute someone else requests again with the same e-mail.
    let second = ticket(now + 61).await;
    assert_ne!(second, first);
    assert_eq!(
        privacy::list(&f.db, &access, 0, now + 61).await.unwrap().0,
        f.user.member.uid,
        "a redeemed session is not affected by a new ticket"
    );
    let third = ticket(now + 122).await;
    assert!(
        privacy::check_access(&f.db, &second, now + 122)
            .await
            .is_err(),
        "a new ticket voids the old unredeemed one"
    );
    privacy::check_access(&f.db, &third, now + 122)
        .await
        .unwrap();
    assert_eq!(
        privacy::list(&f.db, &access, 0, now + 122).await.unwrap().0,
        f.user.member.uid
    );
    // Changes to the account's own credentials (password change, rebinding, role changes) still revoke it together
    // with the sessions; not changed here.
    f.services.cleanup_ref().await.unwrap();
}

/// A browser on the frozen member's data request pages: no forum session, only the CSRF cookie and the privacy cookie
/// that `/data/access` sets.
struct DataBrowser {
    app: axum::Router,
    cookies: std::collections::BTreeMap<String, String>,
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
    fn redirects_to(&self, target: &str) {
        assert_eq!(self.status, StatusCode::SEE_OTHER, "{}", self.text());
        assert_eq!(self.headers[header::LOCATION], target);
    }
}
impl DataBrowser {
    fn new(f: &Fixture) -> Self {
        Self {
            app: gono_forum_web::router::build(
                f.services.config.clone(),
                &f.services.root,
                f.db.clone(),
            ),
            cookies: Default::default(),
        }
    }
    async fn get(&mut self, path: &str) -> Reply {
        self.send("GET", path, &[]).await
    }
    /// Posts a form with the CSRF token of the cookie an earlier page set.
    async fn post(&mut self, path: &str, fields: &[(&str, &str)]) -> Reply {
        self.send("POST", path, fields).await
    }
    async fn send(&mut self, method: &str, path: &str, fields: &[(&str, &str)]) -> Reply {
        let origin = gono_forum_web::test_support::TEST_SITE_URL;
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, gono_forum_web::test_support::host_of(origin));
        let mut body = String::new();
        if method == "POST" {
            let csrf = self
                .cookies
                .get("gonof_csrf")
                .expect("an earlier page sets the CSRF cookie")
                .clone();
            body = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("_csrf", &csrf)
                .extend_pairs(fields.iter().copied())
                .finish();
            builder = builder
                .header(header::ORIGIN, origin)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
        }
        if !self.cookies.is_empty() {
            let jar: Vec<String> = self
                .cookies
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect();
            builder = builder.header(header::COOKIE, jar.join("; "));
        }
        let mut request = builder.body(Body::from(body)).unwrap();
        request.extensions_mut().insert(ConnectInfo(
            "192.0.2.61:48321".parse::<std::net::SocketAddr>().unwrap(),
        ));
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        for cookie in headers.get_all(header::SET_COOKIE) {
            let cookie = cookie.to_str().unwrap();
            let (pair, attributes) = cookie.split_once(';').unwrap_or((cookie, ""));
            let (name, value) = pair.split_once('=').unwrap();
            if value.is_empty() || attributes.contains("Max-Age=0") {
                self.cookies.remove(name);
            } else {
                self.cookies.insert(name.into(), value.into());
            }
        }
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
}
/// The newest email link issued for the member's data requests, if any.
async fn data_ticket(f: &Fixture, uid: i64) -> Option<String> {
    let payload: Option<String> = db::query_scalar("SELECT payload FROM gonof_common_outbox WHERE event_type='privacy_access' AND aggregate_id=? ORDER BY eventid DESC LIMIT 1")
        .bind(uid)
        .fetch_optional(f.db.executor())
        .await
        .unwrap();
    payload.map(|payload| {
        let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
        payload["token"].as_str().unwrap().to_owned()
    })
}

/// A member who signed up with GitHub has no password and, while frozen, cannot sign in to set one. The link sent to
/// the verified email lets them export their data and ask to close the account, without a password field anywhere.
#[tokio::test]
async fn frozen_members_without_a_password_export_and_close_through_the_email_link() {
    let f = Fixture::new().await;
    let uid = f.user.member.uid;
    db::query("INSERT INTO gonof_common_member_external(provider,subject,uid,display,dateline,last_login_at) VALUES('github','5150',?,'octo-frozen',1,1)")
        .bind(uid)
        .execute(f.db.executor())
        .await
        .unwrap();
    // `Fixture::new` marks the email verified, as a provider's email is at sign-up.
    db::query("UPDATE gonof_common_member SET password=NULL,`freeze`=1,credentials_version=credentials_version+1 WHERE uid=?")
        .bind(uid)
        .execute(f.db.executor())
        .await
        .unwrap();
    let t = gono_forum_web::i18n::current();
    let mut browser = DataBrowser::new(&f);
    let page = browser.get("/data/access").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text());
    assert!(
        page.text()
            .contains(t.governance_data_access_passwordless()),
        "the access page says which accounts need a verified email"
    );
    browser
        .post("/data/access", &[("email", "dataowner@example.test")])
        .await
        .redirects_to("/data/access?sent=true");
    let ticket = data_ticket(&f, uid)
        .await
        .expect("a frozen member without a password gets the link at a verified email");

    let page = browser.get(&format!("/data/access?token={ticket}")).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text());
    assert!(page.text().contains(&format!("value=\"{ticket}\"")));
    browser
        .post("/data/access", &[("token", &ticket)])
        .await
        .redirects_to("/my/data");
    assert!(browser.cookies.contains_key("gonof_privacy"));

    let page = browser.get("/my/data").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text());
    assert!(
        page.text().contains("action=\"/data/exit\""),
        "the restricted view without a forum session, with its exit button"
    );
    assert!(
        !page.text().contains("name=\"password\""),
        "the email link is the check; there is no password to ask for"
    );
    for (kind, mode) in [("export", "retain"), ("close", "retain")] {
        let key = identity::random_token();
        browser
            .post(
                "/my/data/requests",
                &[
                    ("kind", kind),
                    ("content_mode", mode),
                    ("request_key", &key),
                    ("confirm", "yes"),
                ],
            )
            .await
            .redirects_to("/my/data");
    }
    let rows =
        db::query("SELECT id,kind,status FROM gonof_common_data_request WHERE uid=? ORDER BY id")
            .bind(uid)
            .fetch_all(f.db.executor())
            .await
            .unwrap();
    let rows: Vec<(i64, String, String)> = rows
        .iter()
        .map(|row| {
            (
                row.try_get("id").unwrap(),
                db::text(row, "kind").unwrap(),
                db::text(row, "status").unwrap(),
            )
        })
        .collect();
    assert_eq!(
        rows.iter()
            .map(|(_, kind, status)| (kind.as_str(), status.as_str()))
            .collect::<Vec<_>>(),
        [("export", "pending"), ("close", "cooling")]
    );

    // The export is built and downloaded through the same access. The pages stamp the request with the clock when it
    // is posted, so the worker runs at the clock read after that, never at a time taken before.
    f.work(identity::now()).await;
    let export = rows[0].0;
    let download = browser
        .get(&format!("/my/data/requests/{export}/download"))
        .await;
    assert_eq!(download.status, StatusCode::OK, "{}", download.text());
    assert_eq!(download.headers[header::CONTENT_TYPE], "application/zip");
    assert!(download.body.starts_with(b"PK"));
    f.services.cleanup().await.unwrap();
}

/// Without a password the email link is the only check, so it goes only to an address the member has verified. The
/// receipt is the same either way and does not say whether a link was sent.
#[tokio::test]
async fn frozen_members_without_a_password_get_no_link_at_an_unverified_email() {
    let f = Fixture::new().await;
    let uid = f.user.member.uid;
    // Signed up with a text message code and typed an email that was never verified.
    db::query("UPDATE gonof_common_member SET password=NULL,mobile='+8613800138000',emailstatus=0,`freeze`=1,credentials_version=credentials_version+1 WHERE uid=?")
        .bind(uid)
        .execute(f.db.executor())
        .await
        .unwrap();
    let mut browser = DataBrowser::new(&f);
    browser.get("/data/access").await;
    browser
        .post("/data/access", &[("email", "dataowner@example.test")])
        .await
        .redirects_to("/data/access?sent=true");
    assert_eq!(data_ticket(&f, uid).await, None);
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_member_token WHERE purpose='privacy_access'")
            .await,
        0
    );

    // Once the address is verified, the link is sent.
    db::query("UPDATE gonof_common_member SET emailstatus=1 WHERE uid=?")
        .bind(uid)
        .execute(f.db.executor())
        .await
        .unwrap();
    browser
        .post("/data/access", &[("email", "dataowner@example.test")])
        .await
        .redirects_to("/data/access?sent=true");
    let ticket = data_ticket(&f, uid)
        .await
        .expect("a link at the verified email");
    // The same condition holds on every use, like the freeze: an issued link stops working for an account that has
    // neither a password nor a verified email. The page issued the link at the clock of its request, so each check
    // reads the clock afresh rather than a time taken before the link existed.
    privacy::check_access(&f.db, &ticket, identity::now())
        .await
        .unwrap();
    db::query("UPDATE gonof_common_member SET emailstatus=0 WHERE uid=?")
        .bind(uid)
        .execute(f.db.executor())
        .await
        .unwrap();
    assert!(matches!(
        privacy::check_access(&f.db, &ticket, identity::now()).await,
        Err(IdentityError::Unauthorized)
    ));
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn registration_modes_and_issue_switch_are_transactional() {
    let f = Fixture::new().await;
    let code = f
        .identity
        .generate_invite(f.admin.member.uid, &identity::random_token())
        .await
        .unwrap()
        .code;
    gono_forum_web::runtime::update(
        &f.db,
        &f.services.global,
        f.admin.member.actor(),
        "site.settings.save",
        |s| {
            s.signup.mode = "closed".into();
        },
    )
    .await
    .unwrap();
    assert!(
        f.identity
            .register("closeduser", "closed@example.test", PASSWORD, &code)
            .await
            .is_err()
    );
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_invite WHERE status='available'")
            .await,
        1
    );
    gono_forum_web::runtime::update(
        &f.db,
        &f.services.global,
        f.admin.member.actor(),
        "site.settings.save",
        |s| {
            s.signup.mode = "open".into();
            s.invite.enabled = false;
        },
    )
    .await
    .unwrap();
    assert!(
        f.identity
            .generate_invite(f.admin.member.uid, &identity::random_token())
            .await
            .is_err()
    );
    f.identity
        .register("openuser", "open@example.test", PASSWORD, "")
        .await
        .unwrap();
    assert!(
        f.identity
            .register(
                "badcodeuser",
                "badcode@example.test",
                PASSWORD,
                "invalid-code"
            )
            .await
            .is_err()
    );
    f.identity
        .register("codeduser", "coded@example.test", PASSWORD, &code)
        .await
        .unwrap();
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_common_invite WHERE status='available'")
            .await,
        0
    );
    gono_forum_web::runtime::update(
        &f.db,
        &f.services.global,
        f.admin.member.actor(),
        "site.settings.save",
        |s| {
            s.signup.mode = "invite".into();
        },
    )
    .await
    .unwrap();
    assert!(
        f.identity
            .register("emptyinvite", "emptyinvite@example.test", PASSWORD, "")
            .await
            .is_err()
    );
    gono_forum_web::runtime::update(
        &f.db,
        &f.services.global,
        f.admin.member.actor(),
        "site.settings.save",
        |s| {
            s.signup.mode = "open".into();
            s.site.read_only = true;
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        f.identity
            .register("readonlyuser", "readonly@example.test", PASSWORD, "")
            .await,
        Err(IdentityError::Suspended(_))
    ));
    assert!(matches!(
        f.identity
            .update_profile(&f.user.session_token, "renameduser", "bio")
            .await,
        Err(IdentityError::Suspended(_))
    ));
    f.identity
        .update_space_visibility(&f.user.session_token, "private")
        .await
        .unwrap();
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_member WHERE uid={} AND space_visibility='private'",
            f.user.member.uid
        ))
        .await,
        1
    );
    // Security operations and the separately authorized data-request path remain usable.
    privacy::submit(
        &f.db,
        &f.access(),
        f.request("export", "retain"),
        identity::now(),
    )
    .await
    .unwrap();
    f.identity.logout_all(&f.user.session_token).await.unwrap();
    f.services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn cooling_blocks_money_and_pending_rights_without_blocking_ordinary_posts() {
    use gono_forum_web::activities::{self, ActivitySpec};
    let f = Fixture::new().await;
    let now = identity::now();
    activities::adjust(
        &f.db,
        f.admin.member.actor(),
        f.admin.member.uid,
        20,
        &identity::random_token(),
        "fixture funds",
        now,
    )
    .await
    .unwrap();
    let fid: i64 = db::query_scalar("SELECT fid FROM gonof_forum_forum ORDER BY fid LIMIT 1")
        .fetch_one(f.db.executor())
        .await
        .unwrap();
    let writer = ContentWriter::new(f.db.clone(), WriteConfig::default());
    let bounty = writer
        .publish(
            f.admin.member.uid,
            NewThread {
                activity: ActivitySpec {
                    kind: 1,
                    budget: 5,
                    deadline: now + 600,
                    ..Default::default()
                },
                fid,
                title: "OPEN BOUNTY".into(),
                body: "BOUNTY BODY".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
                visibility: Default::default(),
            },
        )
        .await
        .unwrap();
    let id = privacy::submit(&f.db, &f.access(), f.request("erase", "erase"), now)
        .await
        .unwrap();
    f.post("ORDINARY_COOLING_POST").await;
    assert!(
        activities::check_in(&f.db, f.user.member.actor(), now)
            .await
            .is_err()
    );
    let reply = NewReply {
        tid: bounty.tid,
        root_pid: None,
        reply_to_pid: None,
        body: "NEW BOUNTY RIGHT".into(),
        attachment_ids: vec![],
        request_key: identity::random_token(),
    };
    assert!(
        writer
            .reply(f.user.member.uid, reply.clone())
            .await
            .is_err()
    );
    assert!(
        writer
            .publish(
                f.user.member.uid,
                NewThread {
                    activity: ActivitySpec {
                        kind: 1,
                        budget: 1,
                        deadline: now + 600,
                        ..Default::default()
                    },
                    fid,
                    title: "FORBIDDEN FUNDS".into(),
                    body: "FUNDS BODY".into(),
                    attachment_ids: vec![],
                    request_key: identity::random_token(),
                    visibility: Default::default(),
                }
            )
            .await
            .is_err()
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_forum_post WHERE authorid={}",
            f.user.member.uid
        ))
        .await,
        1
    );
    privacy::cancel(&f.db, &f.access(), id, now).await.unwrap();
    writer.reply(f.user.member.uid, reply).await.unwrap();
    assert!(matches!(
        privacy::submit(&f.db, &f.access(), f.request("close", "retain"), now).await,
        Err(IdentityError::Conflict(_))
    ));
    gono_forum_web::runtime::update(
        &f.db,
        &f.services.global,
        f.admin.member.actor(),
        "site.settings.save",
        |s| {
            s.site.read_only = true;
        },
    )
    .await
    .unwrap();
    assert!(
        activities::check_in(&f.db, f.user.member.actor(), now)
            .await
            .is_err()
    );
    // Background settlement is allowed during site read-only mode.
    activities::process_due(&f.db, now + 601, 10).await.unwrap();
    assert_eq!(
        f.scalar(&format!(
            "SELECT frozen FROM gonof_common_credit_account WHERE uid={}",
            f.admin.member.uid
        ))
        .await,
        0
    );
    f.services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn close_with_erasure_includes_cooling_posts_and_invalidates_existing_exports() {
    let f = Fixture::new().await;
    let now = identity::now();
    f.post("BEFORE_CLOSURE").await;
    let export = privacy::submit(&f.db, &f.access(), f.request("export", "retain"), now)
        .await
        .unwrap();
    f.work(now).await;
    let dir = privacy::directory(
        &f.services.root,
        &f.services.config.identity.local_directory,
    );
    let (archive, _) = privacy::download(&f.db, &f.access(), export, &dir, now)
        .await
        .unwrap();
    privacy::submit(&f.db, &f.access(), f.request("close", "erase"), now)
        .await
        .unwrap();
    f.post("DURING_COOLING").await;
    f.work(now + privacy::COOLING_SECONDS).await;
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_forum_post WHERE authorid={} AND erased_at>0 AND message=''",
            f.user.member.uid
        ))
        .await,
        2
    );
    assert!(!archive.exists());
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_member WHERE uid={} AND account_state='deleted'",
            f.user.member.uid
        ))
        .await,
        1
    );
    assert!(
        gono_forum_web::activities::adjust(
            &f.db,
            f.admin.member.actor(),
            f.user.member.uid,
            1,
            &identity::random_token(),
            "must not revive a closed wallet",
            now + privacy::COOLING_SECONDS
        )
        .await
        .is_err()
    );
    f.services.cleanup_ref().await.unwrap();
}

// Account deletion with body purge: attachment metadata, poll options, news sources and API tokens' last IPs are not
// kept.
#[tokio::test]
async fn close_with_erasure_scrubs_attachment_activity_and_api_key_traces() {
    let f = Fixture::new().await;
    let now = identity::now();
    let uid = f.user.member.uid;
    let key = f
        .identity
        .create_api_key(&f.user.session_token, PASSWORD, 86_400)
        .await
        .unwrap();
    f.identity
        .touch_api_key(key.key.id, "203.0.113.9", key.key.dateline)
        .await
        .unwrap();
    let fid: i64 = db::query_scalar("SELECT fid FROM gonof_forum_forum ORDER BY fid LIMIT 1")
        .fetch_one(f.db.executor())
        .await
        .unwrap();
    let writer = ContentWriter::new(f.db.clone(), WriteConfig::default());
    let mut tids = vec![];
    for activity in [
        ActivitySpec {
            kind: 4,
            deadline: now + 3600,
            options: vec!["Mine: option A".into(), "Mine: option B".into()],
            ..Default::default()
        },
        ActivitySpec {
            kind: 5,
            source_url: "https://example.test/private-source".into(),
            source_name: "Mine: source".into(),
            source_author: "Mine: byline".into(),
            ..Default::default()
        },
    ] {
        let published = writer
            .publish(
                uid,
                NewThread {
                    visibility: Default::default(),
                    activity,
                    fid,
                    title: "Activity thread".into(),
                    body: "Activity body".into(),
                    attachment_ids: vec![],
                    request_key: identity::random_token(),
                },
            )
            .await
            .unwrap();
        tids.push((published.tid, published.pid));
    }
    // Three attachments of the member: one referenced by the first post, one long purged, one in the trash.
    for (aid, lifecycle) in [(9701_i64, "ready"), (9702, "purged"), (9703, "ready")] {
        db::query(
            "INSERT INTO gonof_forum_attachment(aid,uid,request_key,request_hash) VALUES(?,?,?,?)",
        )
        .bind(aid)
        .bind(uid)
        .bind(identity::random_token())
        .bind(identity::random_token())
        .execute(f.db.executor())
        .await
        .unwrap();
        db::query("INSERT INTO gonof_forum_attachment_0(aid,filename,filesize,mime_type,content_sha256,attachment,storage_profile_id,description,alt_text,dateline,updated_at,lifecycle,scan_status,scan_detail) VALUES(?,'my-medical-record.pdf',10,'application/pdf',?,'object','test','Mine: description','Mine: alt text',?,?,?,'clean','')")
            .bind(aid).bind("a".repeat(64)).bind(now).bind(now).bind(lifecycle).execute(f.db.executor()).await.unwrap();
    }
    db::query("INSERT INTO gonof_forum_attachment_reference(aid,pid,dateline) VALUES(9701,?,?)")
        .bind(tids[0].1)
        .bind(now)
        .execute(f.db.executor())
        .await
        .unwrap();
    db::query("UPDATE gonof_forum_attachment_0 SET trashed_at=?,purge_after=? WHERE aid=9703")
        .bind(now)
        .bind(now + 30 * 86400)
        .execute(f.db.executor())
        .await
        .unwrap();

    privacy::submit(&f.db, &f.access(), f.request("close", "erase"), now)
        .await
        .unwrap();
    f.work(now + privacy::COOLING_SECONDS).await;
    // The cleanup task does not run in this test; the objects are deleted by hand so the account deletion can finish.
    // Attachments in the trash do not wait for the retention period and go into cleanup with the deletion.
    assert_eq!(
        f.scalar(
            "SELECT COUNT(*) FROM gonof_forum_attachment_0 WHERE aid=9703 AND lifecycle='purging'"
        )
        .await,
        1
    );
    db::query("UPDATE gonof_forum_attachment_0 SET lifecycle='purged' WHERE aid IN (9701,9703) AND lifecycle='purging'")
        .execute(f.db.executor())
        .await
        .unwrap();
    f.work(now + privacy::COOLING_SECONDS + 3).await;
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_member WHERE uid={uid} AND account_state='deleted'"
        ))
        .await,
        1
    );
    assert_eq!(
        f.scalar("SELECT COUNT(*) FROM gonof_forum_attachment_0 WHERE aid IN (9701,9702,9703) AND filename='Deleted attachment' AND description='' AND alt_text=''")
            .await,
        3
    );
    let (poll, news) = (tids[0].0, tids[1].0);
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_forum_poll_option WHERE tid={poll} AND label LIKE 'Mine%'"
        ))
        .await,
        0
    );
    // The placeholder text is "Option" followed by the option number: fetch it and compare in code, the same statement
    // on both backends.
    let options = db::query("SELECT option_no,label FROM gonof_forum_poll_option WHERE tid=?")
        .bind(poll)
        .fetch_all(f.db.executor())
        .await
        .unwrap();
    assert_eq!(
        options
            .iter()
            .filter(|row| {
                db::text(row, "label").unwrap()
                    == format!("Option {}", row.try_get::<i64, _>("option_no").unwrap())
            })
            .count(),
        2
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_forum_news WHERE tid={news} AND source_url='about:blank' AND source_name='Deleted' AND source_author='Deleted'"
        ))
        .await,
        1
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_member_api_key WHERE uid={uid} AND revoked_at IS NOT NULL AND last_used_ip=''"
        ))
        .await,
        1
    );
    f.services.cleanup_ref().await.unwrap();
}

// At most 20 tasks run in turn per batch; when a later task starts, the batch's now may be long past.
// Leases must be computed from each task's own start time, otherwise they are expired as soon as claimed and the
// export never finishes.
#[tokio::test]
async fn export_started_late_in_a_long_batch_still_completes() {
    let f = Fixture::new().await;
    let batch_started = identity::now() - 1000;
    let id = privacy::submit(
        &f.db,
        &f.access(),
        f.request("export", "retain"),
        batch_started,
    )
    .await
    .unwrap();
    f.work(batch_started).await;
    let row = db::query("SELECT status,last_error FROM gonof_common_data_request WHERE id=?")
        .bind(id)
        .fetch_one(f.db.executor())
        .await
        .unwrap();
    assert_eq!(
        (
            db::text(&row, "status").unwrap(),
            db::text(&row, "last_error").unwrap()
        ),
        ("ready".to_owned(), String::new())
    );
    f.services.cleanup_ref().await.unwrap();
}

/// A one-click lottery entry also counts as an unfinished campaign entitlement and blocks account deletion; the
/// member's export includes entries, removals and prizes won but no redemption codes, and the author's export does
/// not include codes others won either.
#[tokio::test]
async fn lottery_entries_block_closure_and_exports_never_carry_codes() {
    use gono_forum_web::activities::{self, PrizeSpec};
    let f = Fixture::new().await;
    let now = identity::now();
    let fid: i64 = db::query_scalar("SELECT fid FROM gonof_forum_forum ORDER BY fid LIMIT 1")
        .fetch_one(f.db.executor())
        .await
        .unwrap();
    let writer = ContentWriter::new(f.db.clone(), WriteConfig::default())
        .with_codes(f.services.config.identity.two_factor.clone());
    let lottery = |code: &str| ActivitySpec {
        kind: 3,
        deadline: now + 3600,
        entry_mode: "click".into(),
        early_draw: true,
        review: true,
        prizes: vec![PrizeSpec {
            name: "Member redemption code".into(),
            winners: 1,
            kind: "code".into(),
            amount: 0,
            note: String::new(),
            codes: vec![code.into()],
        }],
        ..Default::default()
    };
    let publish = |author: i64, code: &'static str| {
        let writer = writer.clone();
        let spec = lottery(code);
        async move {
            writer
                .publish_at(
                    author,
                    NewThread {
                        visibility: Default::default(),
                        activity: spec,
                        fid,
                        title: "Export lottery".into(),
                        body: "Export lottery body".into(),
                        attachment_ids: vec![],
                        request_key: identity::random_token(),
                    },
                    now,
                )
                .await
                .unwrap()
                .tid
        }
    };
    let theirs = publish(f.admin.member.uid, "OWNER-WINS-CODE-A").await;
    activities::enter(&f.db, f.user.member.actor(), theirs, &[], now + 1)
        .await
        .unwrap();
    // Before the draw: the member has only one one-click entry in flight (no own campaigns, no frozen credits) and
    // still cannot delete the account.
    assert!(matches!(
        privacy::submit(&f.db, &f.access(), f.request("close", "retain"), now + 2).await,
        Err(IdentityError::Conflict(_))
    ));
    let mine = publish(f.user.member.uid, "AUTHOR-GIVES-CODE-B").await;
    activities::enter(&f.db, f.admin.member.actor(), mine, &[], now + 1)
        .await
        .unwrap();
    activities::draw_now(&f.db, f.admin.member.actor(), theirs, now + 3)
        .await
        .unwrap();
    activities::draw_now(&f.db, f.user.member.actor(), mine, now + 3)
        .await
        .unwrap();
    assert_eq!(
        activities::process_due(&f.db, now + 5, 20).await.unwrap(),
        2
    );
    let id = privacy::submit(&f.db, &f.access(), f.request("export", "retain"), now + 6)
        .await
        .unwrap();
    f.work(now + 6).await;
    let dir = privacy::directory(
        &f.services.root,
        &f.services.config.identity.local_directory,
    );
    let (path, _) = privacy::download(&f.db, &f.access(), id, &dir, now + 6)
        .await
        .unwrap();
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut everything = String::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).unwrap();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        everything.push_str(&String::from_utf8_lossy(&bytes));
    }
    assert!(
        !everything.contains("OWNER-WINS-CODE-A"),
        "a code the member won is not in the export"
    );
    assert!(
        !everything.contains("AUTHOR-GIVES-CODE-B"),
        "the author's export does not carry codes others won"
    );
    let mut awards = String::new();
    archive
        .by_name("lottery_awards.json")
        .unwrap()
        .read_to_string(&mut awards)
        .unwrap();
    assert!(awards.contains("Member redemption code") && awards.contains(&format!("{theirs}")));
    assert!(!awards.contains(&format!("\"tid\":{mine}")));
    let mut signups = String::new();
    archive
        .by_name("lottery_signups.json")
        .unwrap()
        .read_to_string(&mut signups)
        .unwrap();
    assert!(signups.contains(&format!("{theirs}")));
    assert!(archive.by_name("lottery_exclusions.json").is_ok());
}

#[tokio::test]
async fn closing_an_account_retries_failed_member_cache_cleanup_before_completion() {
    use gono_forum_web::{
        cache::{Cache, GlobalCache},
        config::RedisConfig,
    };

    let f = Fixture::new().await;
    let now = identity::now();
    let id = privacy::submit(&f.db, &f.access(), f.request("close", "retain"), now)
        .await
        .unwrap();
    let unavailable = GlobalCache::new(
        Cache::new(&RedisConfig {
            enabled: true,
            url: "redis://127.0.0.1:1/0".into(),
            timeout_milliseconds: 100,
            ..f.services.config.redis.clone()
        })
        .unwrap(),
    );
    let due = now + privacy::COOLING_SECONDS;
    privacy::process_pending(
        &f.db,
        &unavailable,
        &f.media,
        &f.services.root,
        &f.services.config.identity.local_directory,
        90,
        due,
    )
    .await
    .unwrap();
    let status = || {
        db::query_scalar::<String>("SELECT status FROM gonof_common_data_request WHERE id=?")
            .bind(id)
            .fetch_one(f.db.executor())
    };
    assert_eq!(status().await.unwrap(), "failed");
    f.work(due + 61).await;
    assert_eq!(status().await.unwrap(), "completed");
    f.services.cleanup().await.unwrap();
}

/// The text of the member's own activities is in the export, because permanent erasure overwrites it: poll options,
/// the news source, the lottery's entry conditions and its prizes. Options and prizes page by (thread, number), so a
/// poll longer than one export page (100 rows) keeps every option; another member's activities stay out.
#[tokio::test]
async fn data_export_includes_the_text_of_own_activities() {
    let f = Fixture::new().await;
    let now = identity::now();
    let (poll, news, lottery) = f.own_activities(f.user.member.uid, "Mine:", now).await;
    let (theirs, _, _) = f.own_activities(f.admin.member.uid, "Theirs:", now).await;
    for option in 3..=150_i64 {
        db::query("INSERT INTO gonof_forum_poll_option(tid,option_no,label) VALUES(?,?,?)")
            .bind(poll)
            .bind(option)
            .bind(format!("Mine: option {option}"))
            .execute(f.db.executor())
            .await
            .unwrap();
    }
    let id = privacy::submit(&f.db, &f.access(), f.request("export", "retain"), now)
        .await
        .unwrap();
    f.work(now).await;
    let mut archive = f.archive(id, now).await;
    let mut json = |name: &str| -> serde_json::Value {
        serde_json::from_reader(
            archive
                .by_name(name)
                .unwrap_or_else(|error| panic!("{name}: {error}")),
        )
        .unwrap()
    };
    let options = json("poll_options.json");
    let options = options.as_array().unwrap();
    assert_eq!(
        options
            .iter()
            .map(|o| (o["tid"].as_i64().unwrap(), o["option_no"].as_i64().unwrap()))
            .collect::<Vec<_>>(),
        (1..=150).map(|number| (poll, number)).collect::<Vec<_>>(),
        "every option of the member's poll once, in order, and none of {theirs}"
    );
    assert_eq!(options[0]["label"], "Mine: option A");
    assert_eq!(options[149]["label"], "Mine: option 150");
    assert_eq!(
        json("news_sources.json"),
        serde_json::json!([{
            "tid": news,
            "source_url": "https://example.test/private-source",
            "source_name": "Mine: source",
            "source_author": "Mine: byline",
        }])
    );
    let lotteries = json("lotteries.json");
    assert_eq!(lotteries.as_array().map(Vec::len), Some(1), "{lotteries}");
    assert_eq!(lotteries[0]["tid"], lottery);
    assert_eq!(lotteries[0]["requirements"], r#"["Mine: condition"]"#);
    let prizes = json("lottery_prizes.json");
    assert_eq!(
        prizes
            .as_array()
            .unwrap()
            .iter()
            .map(|p| (
                p["tid"].as_i64().unwrap(),
                p["prize_no"].as_i64().unwrap(),
                p["name"].as_str().unwrap().to_owned(),
                p["note"].as_str().unwrap().to_owned()
            ))
            .collect::<Vec<_>>(),
        vec![
            (
                lottery,
                1,
                "Mine: prize A".to_owned(),
                "Mine: call me on 555-0100".to_owned()
            ),
            (
                lottery,
                2,
                "Mine: prize B".to_owned(),
                "Mine: note B".to_owned()
            ),
        ]
    );
    f.services.cleanup_ref().await.unwrap();
}

/// Erasing a lottery's first post also overwrites the lottery's text in the database: prize names become numbered
/// placeholders, notes and entry conditions are cleared, and removal reasons become the placeholder (the removed
/// entrant still sees that they were removed). An export made during the cooling period still has the original text,
/// and the winner's prize list shows the placeholder.
#[tokio::test]
async fn erasure_overwrites_the_prizes_conditions_and_removal_reasons_of_own_lotteries() {
    use gono_forum_web::activities;
    let f = Fixture::new().await;
    let now = identity::now();
    let (_, _, lottery) = f.own_activities(f.user.member.uid, "Mine:", now).await;
    let removed = f.member("removedentrant").await;
    for entrant in [f.admin.member.actor(), removed.member.actor()] {
        activities::enter(&f.db, entrant, lottery, &[1], now + 1)
            .await
            .unwrap();
    }
    activities::exclude(
        &f.db,
        f.user.member.actor(),
        lottery,
        removed.member.uid,
        "Mine: removal reason",
        now + 2,
    )
    .await
    .unwrap();
    activities::draw_now(&f.db, f.user.member.actor(), lottery, now + 3)
        .await
        .unwrap();
    activities::process_due(&f.db, now + 5, 20).await.unwrap();
    let erase = privacy::submit(&f.db, &f.access(), f.request("erase", "erase"), now + 6)
        .await
        .unwrap();

    let export = privacy::submit(&f.db, &f.access(), f.request("export", "retain"), now + 7)
        .await
        .unwrap();
    f.work(now + 7).await;
    let mut archive = f.archive(export, now + 7).await;
    let mut prizes = String::new();
    archive
        .by_name("lottery_prizes.json")
        .unwrap()
        .read_to_string(&mut prizes)
        .unwrap();
    assert!(prizes.contains("Mine: prize A") && prizes.contains("Mine: call me on 555-0100"));

    let due = now + 6 + privacy::COOLING_SECONDS;
    f.work(due).await;
    f.work(due + 3).await;
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_data_request WHERE id={erase} AND status='completed'"
        ))
        .await,
        1
    );
    let rows = db::query(
        "SELECT prize_no,name,note FROM gonof_forum_lottery_prize WHERE tid=? ORDER BY prize_no",
    )
    .bind(lottery)
    .fetch_all(f.db.executor())
    .await
    .unwrap();
    assert_eq!(
        rows.iter()
            .map(|row| (
                row.try_get::<i64, _>("prize_no").unwrap(),
                db::text(row, "name").unwrap(),
                db::text(row, "note").unwrap()
            ))
            .collect::<Vec<_>>(),
        vec![
            (1, "Prize 1".to_owned(), String::new()),
            (2, "Prize 2".to_owned(), String::new())
        ]
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_forum_lottery WHERE tid={lottery} AND requirements IS NULL"
        ))
        .await,
        1
    );
    let reason: String =
        db::query_scalar("SELECT reason FROM gonof_forum_lottery_exclusion WHERE tid=? AND uid=?")
            .bind(lottery)
            .bind(removed.member.uid)
            .fetch_one(f.db.executor())
            .await
            .unwrap();
    assert_eq!(reason, "Deleted");
    let (won, _) = activities::my_prizes(
        &f.db,
        &f.services.config.identity.two_factor,
        f.admin.member.uid,
        0,
    )
    .await
    .unwrap();
    assert_eq!(won.len(), 1);
    assert!(won[0].prize.starts_with("Prize ") && won[0].note.is_empty());
    f.services.cleanup_ref().await.unwrap();
}

/// The download period of an export runs from the moment its archive is finished, and so does the completion time the
/// member sees: an export that starts late in a batch, or takes long, keeps its whole period.
#[tokio::test]
async fn an_export_finished_late_in_a_batch_keeps_its_whole_download_period() {
    let f = Fixture::new().await;
    let batch_started = identity::now() - 1000;
    let id = privacy::submit(
        &f.db,
        &f.access(),
        f.request("export", "retain"),
        batch_started,
    )
    .await
    .unwrap();
    let started = identity::now();
    f.work(batch_started).await;
    let row = db::query(
        "SELECT status,finished_at,artifact_expires_at FROM gonof_common_data_request WHERE id=?",
    )
    .bind(id)
    .fetch_one(f.db.executor())
    .await
    .unwrap();
    let finished: i64 = row.try_get("finished_at").unwrap();
    assert_eq!(db::text(&row, "status").unwrap(), "ready");
    assert!(finished >= started, "{finished} < {started}");
    assert_eq!(
        row.try_get::<Option<i64>, _>("artifact_expires_at")
            .unwrap(),
        Some(finished + privacy::EXPORT_SECONDS)
    );
    f.services.cleanup_ref().await.unwrap();
}

/// A cancelled export keeps its state and cancellation time when a content erasure later withdraws the member's
/// exports: it never produced an archive.
#[tokio::test]
async fn erasure_leaves_a_cancelled_export_as_it_was() {
    let f = Fixture::new().await;
    let now = identity::now();
    f.post("CANCELLED_EXPORT").await;
    let export = privacy::submit(&f.db, &f.access(), f.request("export", "retain"), now)
        .await
        .unwrap();
    privacy::cancel(&f.db, &f.access(), export, now + 1)
        .await
        .unwrap();
    privacy::submit(&f.db, &f.access(), f.request("erase", "erase"), now + 2)
        .await
        .unwrap();
    f.work(now + 2 + privacy::COOLING_SECONDS).await;
    assert_eq!(
        f.scalar(
            "SELECT COUNT(*) FROM gonof_common_data_request WHERE kind='erase' AND stage<>'start'"
        )
        .await,
        1,
        "the erasure has started"
    );
    let row = db::query("SELECT status,finished_at FROM gonof_common_data_request WHERE id=?")
        .bind(export)
        .fetch_one(f.db.executor())
        .await
        .unwrap();
    assert_eq!(
        (
            db::text(&row, "status").unwrap(),
            row.try_get::<i64, _>("finished_at").unwrap()
        ),
        ("cancelled".to_owned(), now + 1)
    );
    f.services.cleanup_ref().await.unwrap();
}

/// An erasure of many posts goes back to the queue after each post at the time the step ended, and the worker takes
/// the task due longest: an export another member asks for meanwhile runs in the same batch instead of waiting for
/// the whole erasure (a batch runs at most 20 steps).
#[tokio::test]
async fn a_long_erasure_takes_turns_with_requests_made_meanwhile() {
    let f = Fixture::new().await;
    let now = identity::now();
    for index in 0..25 {
        f.post(&format!("LONG_ERASURE_{index}")).await;
    }
    let erase = privacy::submit(&f.db, &f.access(), f.request("erase", "erase"), now)
        .await
        .unwrap();
    let other = f.member("exportsmeanwhile").await;
    let due = now + privacy::COOLING_SECONDS;
    let export = privacy::submit(
        &f.db,
        &Access::Session(other.member.actor()),
        f.request("export", "retain"),
        due,
    )
    .await
    .unwrap();
    f.work(due + 10).await;
    let status = |id: i64| {
        db::query_scalar::<String>("SELECT status FROM gonof_common_data_request WHERE id=?")
            .bind(id)
            .fetch_one(f.db.executor())
    };
    assert_eq!(status(export).await.unwrap(), "ready");
    assert_eq!(status(erase).await.unwrap(), "processing");
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_forum_post WHERE authorid={} AND erased_at>0",
            f.user.member.uid
        ))
        .await,
        19,
        "the other 19 steps of the batch erased one post each"
    );
    f.services.cleanup_ref().await.unwrap();
}

/// The sweep for archives no request owns (left by a crash or an expired lease) lists the export directory at most
/// once every ten minutes per process, and looks the archives up in one batch rather than one query each: retained
/// downloads no longer slow down every round of the worker.
#[tokio::test]
async fn the_unregistered_archive_sweep_is_rate_limited_and_batched() {
    use gono_forum_web::debug::trace::{self, RequestProfile};
    use std::sync::Arc;
    let f = Fixture::new().await;
    let now = identity::now();
    let mut ready = vec![];
    for offset in 0..3 {
        let id = privacy::submit(
            &f.db,
            &f.access(),
            f.request("export", "retain"),
            now + offset,
        )
        .await
        .unwrap();
        f.work(now + offset).await;
        ready.push(id);
    }
    let dir = privacy::directory(
        &f.services.root,
        &f.services.config.identity.local_directory,
    );
    let leftover = dir.join(format!("request-999999-{}", identity::random_token()));
    std::fs::create_dir(&leftover).unwrap();
    let lookups = |profile: &RequestProfile| {
        profile
            .snapshot()
            .iter()
            .filter(|record| {
                record.sql.contains("FROM gonof_common_data_request")
                    && (record.sql.contains("WHERE id IN") || record.sql.contains("WHERE r.id="))
            })
            .count()
    };

    // Within ten minutes of the last sweep a round neither lists the directory nor looks anything up.
    let profile = Arc::new(RequestProfile::new());
    trace::scope(profile.clone(), Box::pin(f.work(now + 60))).await;
    assert!(leftover.exists());
    assert_eq!(lookups(&profile), 0);

    // Ten minutes on, one sweep removes the leftover, keeps the three downloads and looks them all up at once.
    let profile = Arc::new(RequestProfile::new());
    trace::scope(profile.clone(), Box::pin(f.work(now + 600))).await;
    assert!(!leftover.exists());
    assert_eq!(lookups(&profile), 1);
    for id in ready {
        let (path, _) = privacy::download(&f.db, &f.access(), id, &dir, now + 600)
            .await
            .unwrap();
        assert!(path.exists());
    }
    f.services.cleanup_ref().await.unwrap();
}
