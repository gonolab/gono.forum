//! Thread row copies, cache versions and `first_tid` are maintained only by code (docs/architecture.md, "Data", and
//! docs/queries.md, "Copy columns"). Each test changes the source tables through business entry points, then asserts
//! that the copies follow and that versions increment by the layering rules, and finally runs the content check
//! script (`test_support::content_drift`), every row of which must be 0.
//!
//! The thread row carries no copies of the avatar, the forum name and URL, or the inherited forum visibility and
//! enabled state; only the author name, the last replier and the top-level forum id are on the thread row. Changing a
//! forum's name, URL, visibility or enabled state, or changing an avatar, does not rewrite thread rows; read paths
//! decide readability by fid from the forum table (or the in-process forum cache), and avatars are taken from the
//! member row by uid.

mod support;

use std::time::Duration;

use gono_forum_web::{
    content::{
        self,
        copies::DELETED_AUTHOR,
        edits::{ChangeRequest, ContentAction, ContentEditor, EditRequest},
        writes::{ContentWriter, NewReply, NewThread, Published, WriteConfig},
    },
    db::{self, Database},
    identity::{self, Actor, Identity, SessionGrant},
    media::Media,
    organization::{self, ForumInput},
    privacy::{self, Access, NewRequest},
    project_root, router,
    test_support::TestFixture,
    urls::Sort,
};

const PASSWORD: &str = "Thread-copies-password-2026!";

struct Fixture {
    services: TestFixture,
    db: Database,
    identity: Identity,
    media: Media,
    writer: ContentWriter,
    editor: ContentEditor,
    admin: SessionGrant,
}

/// The copies on the thread row and the two version-related columns.
#[derive(Debug, PartialEq)]
struct Copies {
    fid: i64,
    family_fid: i64,
    author: String,
    displayorder: i64,
    last_reply_author: String,
    cache_revision: i64,
}

impl Fixture {
    async fn new() -> Self {
        let services = TestFixture::migrated().await.unwrap();
        let db = services.db.clone();
        let identity = Identity::new(
            db.clone(),
            services.config.identity.clone(),
            services.root.clone(),
            gono_forum_web::test_support::detached_global(),
        );
        let admin_password = gono_forum_web::test_support::create_admin(&identity).await;
        let admin = identity.login("admin", &admin_password).await.unwrap();
        Self {
            media: Media::new(
                db.clone(),
                services.config.media.clone(),
                services.root.clone(),
            ),
            writer: ContentWriter::new(db.clone(), WriteConfig::default()),
            editor: ContentEditor::new(
                db.clone(),
                services.config.content.clone(),
                services.config.media.orphan_seconds,
            ),
            services,
            db,
            identity,
            admin,
        }
    }

    fn admin(&self) -> Actor {
        self.admin.member.actor()
    }

    /// A normal member registered by invitation with a verified e-mail; the registration time is set far back so the
    /// new-member posting quota does not apply.
    async fn member(&self, username: &str) -> SessionGrant {
        let invite = self
            .identity
            .generate_invite(self.admin.member.uid, &identity::random_token())
            .await
            .unwrap();
        let member = self
            .identity
            .register(
                username,
                &format!("{username}@example.test"),
                PASSWORD,
                &invite.code,
            )
            .await
            .unwrap();
        self.execute(&format!(
            "UPDATE gonof_common_member SET emailstatus=1,regdate=1 WHERE uid={}",
            member.member.uid
        ))
        .await;
        member
    }

    async fn execute(&self, sql: &str) {
        db::query(sql).execute(self.db.executor()).await.unwrap();
    }

    async fn scalar(&self, sql: &str) -> i64 {
        db::query_scalar(sql)
            .fetch_one(self.db.executor())
            .await
            .unwrap()
    }

    async fn forum(&self, name: &str, slug: &str, visibility: &str, parent: i64) -> i64 {
        organization::save_forum(
            &self.db,
            self.admin(),
            &ForumInput {
                name: name.into(),
                slug: slug.into(),
                visibility: visibility.into(),
                status: 1,
                parent_fid: parent,
                ..Default::default()
            },
        )
        .await
        .unwrap()
    }

    /// Reads the forum's current values, changes a few items, and saves through the admin entry point.
    async fn edit_forum(&self, fid: i64, change: impl FnOnce(&mut ForumInput)) {
        let row = db::query("SELECT name,slug,description,guidance,displayorder,status,visibility,parent_fid,icon_aid,version FROM gonof_forum_forum WHERE fid=?")
            .bind(fid)
            .fetch_one(self.db.executor())
            .await
            .unwrap();
        let mut input = ForumInput {
            fid,
            parent_fid: row
                .try_get::<Option<i64>, _>("parent_fid")
                .unwrap()
                .unwrap_or(0),
            icon_aid: row
                .try_get::<Option<i64>, _>("icon_aid")
                .unwrap()
                .unwrap_or(0),
            name: db::text(&row, "name").unwrap(),
            slug: db::text(&row, "slug").unwrap(),
            description: db::text(&row, "description").unwrap(),
            guidance: db::text(&row, "guidance").unwrap(),
            displayorder: row.try_get("displayorder").unwrap(),
            status: row.try_get("status").unwrap(),
            visibility: db::text(&row, "visibility").unwrap(),
            version: row.try_get("version").unwrap(),
        };
        change(&mut input);
        organization::save_forum(&self.db, self.admin(), &input)
            .await
            .unwrap();
    }

    async fn publish(&self, author: &SessionGrant, fid: i64, title: &str) -> Published {
        self.writer
            .publish(
                author.member.uid,
                NewThread {
                    visibility: Default::default(),
                    activity: Default::default(),
                    fid,
                    title: title.into(),
                    body: format!("{title} body"),
                    attachment_ids: vec![],
                    request_key: identity::random_token(),
                },
            )
            .await
            .unwrap()
    }

    async fn reply(&self, author: &SessionGrant, tid: i64, body: &str) -> Published {
        self.writer
            .reply(
                author.member.uid,
                NewReply {
                    tid,
                    root_pid: None,
                    reply_to_pid: None,
                    body: body.into(),
                    attachment_ids: vec![],
                    request_key: identity::random_token(),
                },
            )
            .await
            .unwrap()
    }

    async fn change(&self, actor: Actor, pid: i64, action: ContentAction) {
        let post = self.editor.editable(actor, pid).await.unwrap();
        self.editor
            .change(
                actor,
                ChangeRequest {
                    pid,
                    action,
                    expected_post_version: post.version,
                    expected_thread_version: post.thread_version,
                    request_key: identity::random_token(),
                    reason: if actor.uid == self.admin.member.uid {
                        "Copies test moderation".into()
                    } else {
                        String::new()
                    },
                },
            )
            .await
            .unwrap();
    }

    async fn edit_body(&self, actor: Actor, pid: i64, body: &str) {
        let post = self.editor.editable(actor, pid).await.unwrap();
        self.editor
            .edit(
                actor,
                EditRequest {
                    pid,
                    expected_post_version: post.version,
                    expected_thread_version: post.thread_version,
                    title: None,
                    body: Some(body.into()),
                    request_key: identity::random_token(),
                    reason: String::new(),
                    visibility: None,
                },
            )
            .await
            .unwrap();
    }

    /// An image attachment the member uploaded that passed the check (for the avatar).
    async fn image(&self, uid: i64) -> i64 {
        let key = identity::random_token();
        let aid = db::query(
            "INSERT INTO gonof_forum_attachment(uid,request_key,request_hash) VALUES(?,?,?)",
        )
        .bind(uid)
        .bind(&key)
        .bind(&key)
        .execute(self.db.executor())
        .await
        .unwrap()
        .last_insert_id() as i64;
        db::query("INSERT INTO gonof_forum_attachment_0(aid,filename,filesize,mime_type,content_sha256,attachment,storage_profile_id,width,height,description,alt_text,dateline,updated_at,checked_at,lifecycle,scan_status,scan_detail) VALUES(?,'avatar.png',100,'image/png',?,?,'test-avatar',32,32,'','avatar',1,1,1,'ready','clean','')")
            .bind(aid)
            .bind(&key)
            .bind(format!("synthetic/avatar-{aid}.png"))
            .execute(self.db.executor())
            .await
            .unwrap();
        aid
    }

    async fn copies(&self, tid: i64) -> Copies {
        let row = db::query("SELECT fid,family_fid,author,displayorder,last_reply_author,cache_revision FROM gonof_forum_thread WHERE tid=?")
            .bind(tid)
            .fetch_one(self.db.executor())
            .await
            .unwrap();
        Copies {
            fid: row.try_get("fid").unwrap(),
            family_fid: row.try_get("family_fid").unwrap(),
            author: db::text(&row, "author").unwrap(),
            displayorder: row.try_get("displayorder").unwrap(),
            last_reply_author: db::text(&row, "last_reply_author").unwrap(),
            cache_revision: row.try_get("cache_revision").unwrap(),
        }
    }

    /// These thread rows are exactly as before saving: neither the copies nor the thread version moved.
    async fn assert_untouched(&self, threads: &[(i64, &Copies)]) {
        for (tid, before) in threads {
            assert_eq!(
                &self.copies(*tid).await,
                *before,
                "thread {tid} row was rewritten"
            );
        }
    }

    /// Threads an anonymous visitor reads via the forum URL (only this forum itself): the URL is resolved by the forum
    /// table, not by the thread row.
    async fn listed(&self, slug: &str) -> Vec<i64> {
        content::list_threads(&self.db, slug, Sort::Latest, None, 1, 100)
            .await
            .unwrap()
            .threads
            .into_iter()
            .map(|thread| thread.id as i64)
            .collect()
    }

    /// Whether an anonymous visitor and a member (`Some(uid)`) can each read this thread: readable forums are computed
    /// from the forum table on the fly (including inheritance from the parent forum); the thread row has no copies of
    /// them.
    async fn readable(&self, tid: i64, viewer: Option<i64>) -> bool {
        let forums = content::readable_forums(&self.db, viewer).await.unwrap();
        content::thread_for_viewer(
            &self.db,
            tid as u64,
            content::Readable {
                uid: viewer,
                forums: &forums,
            },
        )
        .await
        .unwrap()
        .is_some()
    }

    async fn site(&self) -> i64 {
        content::cache_revision(&self.db).await.unwrap()
    }

    async fn thread_version(&self, tid: i64) -> i64 {
        self.scalar(&format!(
            "SELECT version FROM gonof_forum_thread WHERE tid={tid}"
        ))
        .await
    }

    /// Every item of the content check script (except the final trigger count).
    async fn drift(&self) -> Vec<(String, i64)> {
        let mut conn = self.db.pool.acquire().await.unwrap();
        gono_forum_web::test_support::content_drift(&mut conn)
            .await
            .unwrap()
    }

    async fn assert_consistent(&self) {
        // The check script runs only on MySQL.
        if gono_forum_web::test_support::sqlite_tests() {
            return;
        }
        let drift = self.drift().await;
        assert!(
            drift.len() >= 22,
            "check script is missing items: {drift:?}"
        );
        let bad: Vec<_> = drift
            .into_iter()
            .filter(|(_, amount)| *amount != 0)
            .collect();
        assert!(bad.is_empty(), "check script found drift: {bad:?}");
    }
}

#[tokio::test]
async fn publishing_and_replying_write_every_copy_without_the_site_revision() {
    let f = Fixture::new().await;
    let author = f.member("copyauthor").await;
    let replier = f.member("copyreplier").await;
    let parent = f.forum("Copy parent", "copy-parent", "public", 0).await;
    let child = f.forum("Copy child", "copy-child", "members", parent).await;
    let site = f.site().await;
    let (_, watermark) = content::list_revision(&f.db).await.unwrap();

    // Only two copies are written on the thread row, the author name and the top-level forum id; the scope of
    // members-only forums is decided by read paths from the forum table.
    let opening = f.publish(&author, child, "Copy thread").await;
    let copies = f.copies(opening.tid).await;
    assert_eq!(
        copies,
        Copies {
            fid: child,
            family_fid: parent,
            author: "copyauthor".into(),
            displayorder: 0,
            last_reply_author: String::new(),
            cache_revision: copies.cache_revision,
        }
    );
    assert!(
        !f.readable(opening.tid, None).await,
        "anonymous cannot read a members-only forum"
    );
    assert!(f.readable(opening.tid, Some(replier.member.uid)).await);
    assert!(copies.cache_revision >= 1);
    assert_eq!(
        f.scalar(&format!(
            "SELECT first_tid FROM gonof_forum_post WHERE pid={}",
            opening.pid
        ))
        .await,
        opening.tid
    );
    assert_eq!(
        f.site().await,
        site,
        "a new thread does not bump the site revision"
    );
    assert_eq!(
        content::list_revision(&f.db).await.unwrap(),
        (site, opening.pid),
        "the list changes keys by the new-post watermark"
    );
    assert!(opening.pid > watermark);

    let reply = f.reply(&replier, opening.tid, "Copy reply").await;
    let after = f.copies(opening.tid).await;
    assert_eq!(after.last_reply_author, "copyreplier");
    assert!(
        after.cache_revision > copies.cache_revision,
        "a reply bumps the thread revision"
    );
    assert_eq!(
        f.site().await,
        site,
        "a reply does not touch the site revision"
    );
    assert_eq!(
        content::list_revision(&f.db).await.unwrap(),
        (site, reply.pid)
    );
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_forum_post WHERE pid={} AND first_tid IS NULL",
            reply.pid
        ))
        .await,
        1
    );
    f.assert_consistent().await;
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn forum_saves_rewrite_copies_only_when_they_change() {
    let f = Fixture::new().await;
    let author = f.member("forumcopies").await;
    let member = Some(author.member.uid);
    let parent = f.forum("Parent forum", "copy-root", "public", 0).await;
    let child = f.forum("Child forum", "copy-leaf", "public", parent).await;
    let other = f.forum("Other top level", "copy-other", "public", 0).await;
    let in_parent = f.publish(&author, parent, "Parent forum thread").await.tid;
    let in_child = f.publish(&author, child, "Child forum thread").await.tid;
    assert_eq!(f.copies(in_child).await.family_fid, parent);
    let (before_parent, before_child) = (f.copies(in_parent).await, f.copies(in_child).await);
    let untouched = [(in_parent, &before_parent), (in_child, &before_child)];
    let readable = |tid: i64, viewer: Option<i64>| f.readable(tid, viewer);

    // Changing only the order and description: no thread is rewritten and the site-wide version is not incremented.
    // The same goes for saving once unchanged.
    let site = f.site().await;
    f.edit_forum(parent, |input| {
        input.displayorder = 9;
        input.description = "Description only".into();
    })
    .await;
    f.edit_forum(child, |_| {}).await;
    assert_eq!(f.site().await, site);
    f.assert_untouched(&untouched).await;

    // The parent forum is changed to members-only, then disabled: not a single thread row is rewritten (the thread
    // row has no copies of the inherited scope), and each save increments the site-wide version by one;
    // read paths compute the inherited scope from the forum table on the fly, so threads of the forum and of its
    // child forums become unreadable together.
    f.edit_forum(parent, |input| input.visibility = "members".into())
        .await;
    assert_eq!(f.site().await, site + 1);
    f.assert_untouched(&untouched).await;
    for tid in [in_parent, in_child] {
        assert!(
            !readable(tid, None).await,
            "parent is members-only, anonymous cannot read {tid}"
        );
        assert!(readable(tid, member).await, "a member still reads {tid}");
    }
    f.edit_forum(parent, |input| input.status = 0).await;
    assert_eq!(f.site().await, site + 2);
    f.assert_untouched(&untouched).await;
    for tid in [in_parent, in_child] {
        assert!(
            !readable(tid, member).await,
            "parent is disabled, a member cannot read {tid} either"
        );
    }
    f.assert_consistent().await;
    f.edit_forum(parent, |input| {
        input.status = 1;
        input.visibility = "public".into();
    })
    .await;
    assert_eq!(f.site().await, site + 3, "one save adds exactly one");
    f.assert_untouched(&untouched).await;
    for tid in [in_parent, in_child] {
        assert!(
            readable(tid, None).await,
            "anonymous reads {tid} again after restoring"
        );
    }

    // Renaming the child forum and changing its URL: thread rows are not rewritten, and the list resolves the new URL
    // by the forum table; then changing it to private makes only the child forum's threads unreadable.
    f.edit_forum(child, |input| {
        input.name = "Renamed child forum".into();
        input.slug = "copy-leaf-renamed".into();
    })
    .await;
    assert_eq!(f.site().await, site + 4);
    f.assert_untouched(&untouched).await;
    assert_eq!(f.listed("copy-leaf-renamed").await, vec![in_child]);
    assert!(
        f.listed("copy-leaf").await.is_empty(),
        "the old URL no longer resolves"
    );
    f.edit_forum(child, |input| input.visibility = "private".into())
        .await;
    assert_eq!(f.site().await, site + 5);
    f.assert_untouched(&untouched).await;
    assert!(
        !readable(in_child, member).await,
        "nobody reads a private forum"
    );
    assert!(
        readable(in_parent, None).await,
        "the parent is not affected by the child"
    );

    // Moving the child forum under another, members-only top-level forum: only a change of parent forum rewrites the
    // top-level forum id of this forum's threads, and the site-wide version still goes up by just one.
    f.edit_forum(other, |input| input.visibility = "members".into())
        .await;
    assert_eq!(f.site().await, site + 6);
    f.edit_forum(child, |input| {
        input.visibility = "public".into();
        input.parent_fid = other;
    })
    .await;
    assert_eq!(f.site().await, site + 7);
    let moved = f.copies(in_child).await;
    assert_eq!(moved.family_fid, other);
    assert!(moved.cache_revision > before_child.cache_revision);
    assert_eq!(f.copies(in_parent).await, before_parent);
    assert!(
        !readable(in_child, None).await,
        "inherits members-only from the new parent"
    );
    assert!(readable(in_child, member).await);
    f.edit_forum(other, |input| input.status = 0).await;
    assert!(
        !readable(in_child, member).await,
        "the new parent is disabled"
    );
    assert!(readable(in_parent, None).await);
    f.assert_consistent().await;

    // Changing it back to a top-level forum: the top-level forum id is itself.
    f.edit_forum(child, |input| input.parent_fid = 0).await;
    assert_eq!(f.copies(in_child).await.family_fid, child);
    assert!(
        readable(in_child, None).await,
        "no longer inherits the disabled parent"
    );
    f.assert_consistent().await;
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn moving_a_thread_carries_the_target_forum_copies() {
    let f = Fixture::new().await;
    let author = f.member("movecopies").await;
    let source = f.forum("Move source", "copy-source", "public", 0).await;
    let parent = f
        .forum("Move target parent", "copy-target-root", "private", 0)
        .await;
    let target = f
        .forum("Move target", "copy-target", "public", parent)
        .await;
    let published = f.publish(&author, source, "Thread to move").await;
    f.reply(&author, published.tid, "Reply before the move")
        .await;
    let (site, before) = (f.site().await, f.copies(published.tid).await);
    assert_eq!(before.family_fid, source);
    assert!(f.readable(published.tid, None).await);
    organization::move_thread(
        &f.db,
        f.admin(),
        published.tid,
        target,
        f.thread_version(published.tid).await,
        &identity::random_token(),
        "Copies test move",
    )
    .await
    .unwrap();
    // The thread row carries only the target forum id and its top-level forum id; the target forum is public but its
    // parent is private, so read paths inherit private from the forum table.
    let moved = f.copies(published.tid).await;
    assert_eq!((moved.fid, moved.family_fid), (target, parent));
    assert!(moved.cache_revision > before.cache_revision);
    assert!(
        !f.readable(published.tid, Some(author.member.uid)).await,
        "the target is public but its parent is private, so it inherits private"
    );
    assert_eq!(f.site().await, site + 1);
    assert_eq!(
        f.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_forum_post WHERE tid={} AND fid={target}",
            published.tid
        ))
        .await,
        2,
        "posts follow to the target forum through the foreign key cascade"
    );
    f.assert_consistent().await;
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn post_state_changes_keep_listing_order_and_versions() {
    let f = Fixture::new().await;
    let author = f.member("statecopies").await;
    let replier = f.member("statereplier").await;
    let fid = f.forum("State forum", "copy-state", "public", 0).await;
    let opening = f
        .publish(&author, fid, "Thread with a hidden first post")
        .await;
    let middle = f.reply(&replier, opening.tid, "Middle reply").await;
    let last = f.reply(&replier, opening.tid, "Last reply").await;
    let listed = |threads: Vec<content::Thread>| {
        threads.iter().any(|thread| thread.id == opening.tid as u64)
    };
    let list = || content::list_threads(&f.db, "", Sort::Latest, None, 1, 100);
    assert!(listed(list().await.unwrap().threads));

    // Moderation hides the first post: displayorder becomes -1, the thread leaves the public list at once, and the
    // site-wide version goes up by one;
    // after unhiding it goes back to 0, and the original pin level is not kept. The count table and member counts go
    // up and down with it (checked by assert_consistent).
    f.change(f.admin(), opening.pid, ContentAction::Pin2).await;
    assert_eq!(f.copies(opening.tid).await.displayorder, 2);
    let site = f.site().await;
    f.change(f.admin(), opening.pid, ContentAction::Hide).await;
    assert_eq!(f.copies(opening.tid).await.displayorder, -1);
    assert_eq!(f.site().await, site + 1);
    assert!(!listed(list().await.unwrap().threads));
    f.assert_consistent().await;
    f.change(f.admin(), opening.pid, ContentAction::Unhide)
        .await;
    assert_eq!(
        f.copies(opening.tid).await.displayorder,
        0,
        "the pin level is not kept"
    );
    assert_eq!(f.site().await, site + 2);
    assert!(listed(list().await.unwrap().threads));
    f.assert_consistent().await;

    // Editing a middle floor only increments the thread version; editing the last reply (the list excerpt changes) or
    // the first post increments the site-wide version.
    let (site, revision) = (f.site().await, f.copies(opening.tid).await.cache_revision);
    f.edit_body(replier.member.actor(), middle.pid, "Middle reply edited")
        .await;
    assert_eq!(f.site().await, site);
    let revision_after_middle = f.copies(opening.tid).await.cache_revision;
    assert!(
        revision_after_middle > revision,
        "the floor cache must be invalidated"
    );
    f.edit_body(replier.member.actor(), last.pid, "Last reply edited")
        .await;
    assert_eq!(f.site().await, site + 1);
    f.edit_body(author.member.actor(), opening.pid, "First post body edited")
        .await;
    assert_eq!(f.site().await, site + 2);

    // Locking the thread, hiding a reply (recomputing valid replies and the last reply), the author withdrawing the
    // first post: all increment the site-wide version.
    f.change(f.admin(), opening.pid, ContentAction::Lock).await;
    assert_eq!(f.site().await, site + 3);
    f.change(f.admin(), last.pid, ContentAction::Hide).await;
    assert_eq!(f.site().await, site + 4);
    f.change(author.member.actor(), opening.pid, ContentAction::Withdraw)
        .await;
    assert_eq!(f.site().await, site + 5);
    f.assert_consistent().await;
    f.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn renames_avatars_and_closures_rewrite_member_copies() {
    let f = Fixture::new().await;
    let author = f.member("membercopies").await;
    let replier = f.member("memberreplier").await;
    let fid = f.forum("Member forum", "copy-member", "public", 0).await;
    let theirs = f.publish(&author, fid, "Someone else's thread").await.tid;
    let reply = f.reply(&replier, theirs, "Ending with me").await;
    let own_post = f.publish(&replier, fid, "My thread").await;
    let own = own_post.tid;

    // Renaming: the author name of the member's threads and the last replier of threads ending with the member's
    // reply; the site-wide version goes up by one.
    let (site, revision) = (f.site().await, f.copies(own).await.cache_revision);
    f.identity
        .update_profile(&replier.session_token, "memberrenamed", "")
        .await
        .unwrap();
    assert_eq!(f.copies(own).await.author, "memberrenamed");
    assert_eq!(f.copies(theirs).await.last_reply_author, "memberrenamed");
    assert!(f.copies(own).await.cache_revision > revision);
    assert_eq!(f.site().await, site + 1);
    // Changing only the bio is not a rename: neither the copies nor the site-wide version move.
    f.identity
        .update_profile(&replier.session_token, "memberrenamed", "New bio")
        .await
        .unwrap();
    assert_eq!(f.site().await, site + 1);

    // Changing the avatar: list avatars are served by uid (thread rows have no avatar copy), not a single thread row
    // is rewritten, and the site-wide version does not move either; the thread page
    // reads the new avatar from the member row.
    let aid = f.image(replier.member.uid).await;
    // The image is also attached to the member's thread: after account deletion clears the avatar it is still
    // referenced, so it does not enter the media cleanup queue, and deletion completes in one round.
    f.execute(&format!(
        "INSERT INTO gonof_forum_attachment_reference(aid,pid,dateline) VALUES({aid},{},1)",
        own_post.pid
    ))
    .await;
    let (own_before, theirs_before) = (f.copies(own).await, f.copies(theirs).await);
    f.media
        .set_avatar(replier.member.actor(), Some(aid))
        .await
        .unwrap();
    f.assert_untouched(&[(own, &own_before), (theirs, &theirs_before)])
        .await;
    assert_eq!(
        f.site().await,
        site + 1,
        "changing the avatar does not bump the site revision"
    );
    let page = content::thread(&f.db, own as u64).await.unwrap().unwrap();
    assert_eq!(
        page.avatar_aid, aid,
        "the thread page reads the current avatar from the member row"
    );
    f.assert_consistent().await;

    // Account deletion: the author copy and the last replier of threads ending with the member's reply are both
    // written as the deleted-author marker (`DELETED_AUTHOR`), and the site-wide version increments.
    let now = identity::now();
    privacy::submit(
        &f.db,
        &Access::Session(replier.member.actor()),
        NewRequest {
            kind: "close".into(),
            content_mode: "retain".into(),
            request_key: identity::random_token(),
            password: PASSWORD.into(),
        },
        now,
    )
    .await
    .unwrap();
    privacy::process_pending(
        &f.db,
        &f.services.global,
        &f.media,
        &f.services.root,
        &f.services.config.identity.local_directory,
        90,
        now + privacy::COOLING_SECONDS,
    )
    .await
    .unwrap();
    let pseudonym = db::query_scalar::<String>(&format!(
        "SELECT username FROM gonof_common_member WHERE uid={} AND account_state='deleted'",
        replier.member.uid
    ))
    .fetch_one(f.db.executor())
    .await
    .unwrap();
    assert!(pseudonym.starts_with("deleted-"));
    // After account deletion the author copy on the thread row is written directly as `DELETED_AUTHOR`, not the
    // pseudonym generated at deletion time; the thread page shows it as "Deleted user" and, given the account status,
    // no avatar.
    let closed = f.copies(own).await;
    assert_eq!(closed.author, DELETED_AUTHOR);
    assert!(closed.cache_revision > own_before.cache_revision);
    assert_eq!(f.copies(theirs).await.last_reply_author, DELETED_AUTHOR);
    assert!(f.site().await > site + 1);
    let page = content::thread(&f.db, own as u64).await.unwrap().unwrap();
    assert_eq!((page.author.as_str(), page.avatar_aid), ("Deleted user", 0));
    assert_eq!(
        f.scalar(&format!(
            "SELECT last_reply_pid FROM gonof_forum_thread WHERE tid={theirs}"
        ))
        .await,
        reply.pid
    );
    f.assert_consistent().await;
    f.services.cleanup().await.unwrap();
}

/// The top-level forum id `family_fid` has no default: an INSERT that omits it fails directly with 1364 and never
/// silently lands in some forum family. It is the only copy on the thread row without a default.
#[tokio::test]
async fn thread_rows_without_the_scope_columns_are_rejected() {
    let f = Fixture::new().await;
    let fid = f
        .forum("Default value forum", "copy-default", "public", 0)
        .await;
    let uid = f.admin.member.uid;
    let omitted = db::query(&format!(
        "INSERT INTO gonof_forum_thread(fid,authorid,subject) VALUES({fid},{uid},'Copy omitted')"
    ))
    .execute(f.db.executor())
    .await;
    let error = omitted.expect_err("omitting family_fid must fail");
    // A column without a default: MySQL reports 1364, SQLite a NOT NULL constraint (extended code 1299).
    let database = error.as_database_error();
    let mysql = database
        .and_then(|error| error.try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>())
        .map(|error| error.number());
    let sqlite = database
        .and_then(|error| error.try_downcast_ref::<sqlx::sqlite::SqliteError>())
        .and_then(|error| sqlx::error::DatabaseError::code(error).map(|code| code.into_owned()));
    assert!(
        mysql == Some(1364) || sqlite.as_deref() == Some("1299"),
        "omitted family_fid: {error}"
    );
    db::query(&format!(
        "INSERT INTO gonof_forum_thread(fid,authorid,subject,family_fid) VALUES({fid},{uid},'Copy complete',{fid})"
    ))
    .execute(f.db.executor())
    .await
    .expect("inserting with every column succeeds");
    f.services.cleanup().await.unwrap();
}

/// The check script recognizes every kind of copy drift: remove some piece of sync code, and the `assert_consistent`
/// at the end of the test fails.
#[tokio::test]
async fn the_check_script_reports_each_kind_of_drift() {
    // Only checks the check script, which runs on MySQL only.
    gono_forum_web::mysql_only!();
    let f = Fixture::new().await;
    let author = f.member("driftauthor").await;
    let fid = f.forum("Drift forum", "copy-drift", "members", 0).await;
    let opening = f.publish(&author, fid, "Drift thread").await;
    let reply = f.reply(&author, opening.tid, "Drift reply").await;
    f.assert_consistent().await;
    let tid = opening.tid;
    for (sql, item) in [
        (
            format!("UPDATE gonof_forum_thread SET author='someone else' WHERE tid={tid}"),
            "threads whose author name copy disagrees with the member table",
        ),
        (
            format!(
                "UPDATE gonof_forum_thread SET last_reply_author='someone else' WHERE tid={tid}"
            ),
            "threads whose last replier disagrees with the author of the last reply",
        ),
        (
            format!("UPDATE gonof_forum_thread SET displayorder=-1 WHERE tid={tid}"),
            "threads whose negative displayorder disagrees with the hidden or reserved state of the first post",
        ),
        (
            format!("UPDATE gonof_forum_thread SET deleted_at=1 WHERE tid={tid}"),
            "threads whose withdrawal disagrees with the withdrawal of the first post",
        ),
        (
            format!(
                "UPDATE gonof_forum_post SET root_invisible=-1 WHERE pid={}",
                reply.pid
            ),
            "nested replies whose copy of the parent hidden state disagrees with the parent",
        ),
        (
            format!("UPDATE gonof_forum_thread_count SET threads=threads+1 WHERE fid={fid}"),
            "forum thread count cells that disagree with the listable threads",
        ),
        (
            format!(
                "UPDATE gonof_common_member_count SET replies=replies+1 WHERE uid={}",
                author.member.uid
            ),
            "members whose counters disagree with their own posts",
        ),
        (
            format!("UPDATE gonof_forum_thread SET family_fid=0 WHERE tid={tid}"),
            "threads whose top-level forum id copy disagrees with the forum table",
        ),
        (
            format!(
                "UPDATE gonof_forum_post SET first_tid=NULL WHERE pid={}",
                opening.pid
            ),
            "posts whose first_tid disagrees with the first-post flag",
        ),
        (
            format!("UPDATE gonof_forum_thread SET valid_replies=5 WHERE tid={tid}"),
            "threads whose visible reply count disagrees with the visible replies",
        ),
        (
            format!(
                "UPDATE gonof_forum_post SET sub_position=1 WHERE pid={}",
                reply.pid
            ),
            "posts with invalid values or shape",
        ),
    ] {
        f.execute(&sql).await;
        let drift = f.drift().await;
        let found: Vec<_> = drift.iter().filter(|(_, amount)| *amount != 0).collect();
        assert!(
            found
                .iter()
                .any(|(name, amount)| name == item && *amount == 1),
            "{sql} should report 1 under \"{item}\": {found:?}"
        );
        // No multi-table UPDATE: copy values come from subqueries, the same statement on both
        // backends.
        f.execute("UPDATE gonof_forum_thread SET author=(SELECT m.username FROM gonof_common_member m WHERE m.uid=gonof_forum_thread.authorid),family_fid=(SELECT COALESCE(fo.parent_fid,fo.fid) FROM gonof_forum_forum fo WHERE fo.fid=gonof_forum_thread.fid),deleted_at=0,last_reply_author=(SELECT m.username FROM gonof_common_member m WHERE m.uid=gonof_forum_thread.authorid),valid_replies=1").await;
        gono_forum_web::test_support::refresh_listing(&f.db)
            .await
            .unwrap();
        f.execute(&format!(
            "UPDATE gonof_forum_post SET first_tid=IF(first=1,tid,NULL),sub_position=NULL WHERE tid={tid}"
        ))
        .await;
        f.assert_consistent().await;
    }
    f.services.cleanup().await.unwrap();
}

/// Withdrawing and hiding invalidate the anonymous list, feeds and floor cache at commit.
/// First, raw SQL that increments no version changes the title and body, proving the anonymous pages really come from
/// the cache; then going through the business entry point changes the cache keys at once.
#[tokio::test]
async fn withdrawals_and_hides_leave_anonymous_caches_at_once() {
    let f = Fixture::new().await;
    content::seed_forums(&f.db).await.unwrap();
    let fid = f
        .scalar("SELECT fid FROM gonof_forum_forum ORDER BY fid LIMIT 1")
        .await;
    let author = f.member("cacheauthor").await;
    let replier = f.member("cachereplier").await;
    let hidden = f.publish(&author, fid, "CACHE_HIDDEN_TITLE_6001").await;
    let withdrawn = f.publish(&author, fid, "CACHE_WITHDRAWN_TITLE_6002").await;
    let kept = f.publish(&author, fid, "CACHE_KEPT_TITLE_6004").await;
    let reply = f.reply(&replier, kept.tid, "CACHE_REPLY_BODY_6003").await;
    let app = router::build(support::config_for(&f.db), &project_root(), f.db.clone());
    let page = gono_forum_web::urls::thread(kept.tid as u64, 1, false);
    let fetch = |path: String| {
        let app = app.clone();
        async move { support::fetch(&app, &path, false).await.2 }
    };
    for body in [fetch("/".into()).await, fetch("/feed.xml".into()).await] {
        assert!(body.contains("CACHE_HIDDEN_TITLE_6001"), "{body}");
        assert!(body.contains("CACHE_WITHDRAWN_TITLE_6002"));
    }
    assert!(fetch(page.clone()).await.contains("CACHE_REPLY_BODY_6003"));

    // Raw SQL increments no version: the anonymous pages still show the old cached content.
    f.execute(&format!(
        "UPDATE gonof_forum_thread SET subject='CACHE_PROBE_TITLE' WHERE tid={}",
        hidden.tid
    ))
    .await;
    f.execute(&format!(
        "UPDATE gonof_forum_post SET message='CACHE_PROBE_BODY' WHERE pid={}",
        reply.pid
    ))
    .await;
    for body in [fetch("/".into()).await, fetch("/feed.xml".into()).await] {
        assert!(
            body.contains("CACHE_HIDDEN_TITLE_6001"),
            "the list and the feed come from the cache"
        );
        assert!(!body.contains("CACHE_PROBE_TITLE"));
    }
    let cached = fetch(page.clone()).await;
    assert!(cached.contains("CACHE_REPLY_BODY_6003") && !cached.contains("CACHE_PROBE_BODY"));

    // New threads and new replies do not increment the site-wide version; the new-post watermark makes the home page
    // list, the sidebar latest and feeds each change keys at once.
    let cache = &f.services.cache.timed(gono_forum_web::cache::Lifetimes {
        member: 30,
        page: 60,
        list: 60,
        statistics: 600,
        forum_statistics: 600,
    });
    let fresh = |titles: Vec<String>| titles.iter().any(|title| title == "CACHE_FRESH_TITLE_6005");
    let listed = || async {
        let page = content::cached_list_threads(&f.db, "", Sort::Latest, None, None, cache, 1, 20)
            .await
            .unwrap();
        page.threads
            .into_iter()
            .map(|thread| thread.title)
            .collect::<Vec<_>>()
    };
    let latest = || async {
        let overview =
            content::cached_sidebar(&f.db, cache, gono_forum_web::test_support::SEEDED_SIDEBAR)
                .await
                .unwrap();
        overview
            .latest
            .into_iter()
            .map(|thread| thread.title)
            .collect::<Vec<_>>()
    };
    let feed = || async {
        let forums = content::anonymous_forums(&f.db).await.unwrap();
        let items =
            content::feeds::cached_items(&f.db, cache, &content::ListScope::All, &forums, 30)
                .await
                .unwrap();
        items.into_iter().map(|item| item.title).collect::<Vec<_>>()
    };
    assert!(!fresh(listed().await) && !fresh(latest().await) && !fresh(feed().await));
    let site = f.site().await;
    f.publish(&replier, fid, "CACHE_FRESH_TITLE_6005").await;
    assert!(fresh(listed().await), "home page list");
    assert!(fresh(latest().await), "sidebar latest");
    assert!(fresh(feed().await), "feed");
    for body in [fetch("/".into()).await, fetch("/feed.xml".into()).await] {
        assert!(body.contains("CACHE_FRESH_TITLE_6005"), "{body}");
    }
    assert_eq!(f.site().await, site);

    // Moderation hiding the first post, the author withdrawing the first post, moderation hiding a reply: anonymous
    // visitors stop seeing it right after commit.
    f.change(f.admin(), hidden.pid, ContentAction::Hide).await;
    f.change(
        author.member.actor(),
        withdrawn.pid,
        ContentAction::Withdraw,
    )
    .await;
    for body in [fetch("/".into()).await, fetch("/feed.xml".into()).await] {
        assert!(!body.contains("CACHE_HIDDEN_TITLE_6001"), "{body}");
        assert!(!body.contains("CACHE_PROBE_TITLE"));
        assert!(!body.contains("CACHE_WITHDRAWN_TITLE_6002"));
    }
    f.change(f.admin(), reply.pid, ContentAction::Hide).await;
    let body = fetch(page).await;
    assert!(
        body.contains("CACHE_KEPT_TITLE_6004"),
        "the thread page itself is still visible"
    );
    assert!(!body.contains("CACHE_REPLY_BODY_6003") && !body.contains("CACHE_PROBE_BODY"));
    f.services.cleanup().await.unwrap();
}

/// Replies do not touch the site-wide version row. (1) When another transaction locks this row, replies and new
/// threads still complete; (2) two transactions reply to different threads, one of them stops in the middle (thread
/// row already changed, not yet committed), and the other does not wait for it. If a reply locked the site-wide
/// version row until commit, the later transaction would have to queue for the lock in both cases.
#[tokio::test]
async fn replies_to_different_threads_never_wait_for_each_other() {
    // Only checks MySQL row locks (SQLite queues writes as a whole).
    gono_forum_web::mysql_only!();
    let f = Fixture::new().await;
    let fid = f
        .forum("Concurrency forum", "copy-concurrency", "public", 0)
        .await;
    let owner_a = f.member("ownera").await;
    let owner_b = f.member("ownerb").await;
    let replier_a = f.member("repliera").await;
    let replier_b = f.member("replierb").await;
    let thread_a = f.publish(&owner_a, fid, "Concurrent thread A").await.tid;
    let thread_b = f.publish(&owner_b, fid, "Concurrent thread B").await.tid;
    let site = f.site().await;
    let limit = Duration::from_secs(10);

    // (1) An outside transaction locks the site-wide version row.
    let mut holder = f.db.pool.begin().await.unwrap();
    gono_forum_web::db::query(
        "SELECT revision FROM gonof_common_content_revision WHERE id=1 FOR UPDATE",
    )
    .fetch_one(&mut *holder)
    .await
    .unwrap();
    tokio::time::timeout(
        limit,
        f.reply(
            &replier_a,
            thread_a,
            "Reply while the revision row is locked",
        ),
    )
    .await
    .expect("a reply must not wait for the site revision row");
    tokio::time::timeout(
        limit,
        f.publish(&replier_b, fid, "Thread while the revision row is locked"),
    )
    .await
    .expect("publishing must not wait for the site revision row");
    holder.rollback().await.unwrap();
    assert_eq!(f.site().await, site);

    // (2) Fault injection in the test database: replier A's reply stops while writing the request row,
    // waiting for the outside transaction to release the gonof_test_stall row.
    // By then replier A has already changed the thread row, and the transaction is still open.
    gono_forum_web::db::raw_sql("CREATE TABLE gonof_test_stall(id INT PRIMARY KEY) ENGINE=InnoDB; INSERT INTO gonof_test_stall VALUES(1)")
        .execute(&f.db.pool)
        .await
        .unwrap();
    gono_forum_web::db::raw_sql(&format!(
        "CREATE TRIGGER stall_reply_a BEFORE INSERT ON gonof_forum_post_request FOR EACH ROW BEGIN IF NEW.uid={} THEN SELECT id INTO @stalled FROM gonof_test_stall WHERE id=1 FOR UPDATE; END IF; END",
        replier_a.member.uid
    ))
    .execute(&f.db.pool)
    .await
    .unwrap();
    let mut gate = f.db.pool.begin().await.unwrap();
    gono_forum_web::db::query("SELECT id FROM gonof_test_stall WHERE id=1 FOR UPDATE")
        .fetch_one(&mut *gate)
        .await
        .unwrap();
    let writer = ContentWriter::new(f.db.clone(), WriteConfig::default());
    let uid_a = replier_a.member.uid;
    let stalled = tokio::spawn(async move {
        writer
            .reply(
                uid_a,
                NewReply {
                    tid: thread_a,
                    root_pid: None,
                    reply_to_pid: None,
                    body: "Reply stalled mid-transaction".into(),
                    attachment_ids: vec![],
                    request_key: identity::random_token(),
                },
            )
            .await
    });
    // Replier A is stuck on the locking read inside the trigger (the 5.7 process list shows the outer INSERT).
    let waiting = "SELECT COUNT(*) FROM information_schema.PROCESSLIST WHERE DB=DATABASE() AND (INFO LIKE '%@stalled%' OR INFO LIKE 'INSERT INTO gonof_forum_post_request%')";
    let started = std::time::Instant::now();
    while f.scalar(waiting).await == 0 {
        assert!(
            !stalled.is_finished(),
            "reply A did not stall: {:?}",
            stalled.await
        );
        assert!(
            started.elapsed() < limit,
            "reply A did not stall on the request row"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let revision_a = f.copies(thread_a).await.cache_revision;
    tokio::time::timeout(
        limit,
        f.reply(&replier_b, thread_b, "Reply that need not wait for A"),
    )
    .await
    .expect("B replying to another thread must not wait for A's transaction");
    assert!(!stalled.is_finished(), "A is still stalled mid-transaction");
    assert_eq!(f.site().await, site);
    gate.rollback().await.unwrap();
    let published = tokio::time::timeout(limit, stalled)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(published.tid, thread_a);
    assert!(f.copies(thread_a).await.cache_revision > revision_a);
    assert_eq!(
        f.site().await,
        site,
        "neither reply touched the site revision"
    );
    gono_forum_web::db::raw_sql("DROP TRIGGER stall_reply_a; DROP TABLE gonof_test_stall")
        .execute(&f.db.pool)
        .await
        .unwrap();
    f.assert_consistent().await;
    f.services.cleanup().await.unwrap();
}
