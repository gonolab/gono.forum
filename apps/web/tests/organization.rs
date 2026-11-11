use gono_forum_web::{
    content, db,
    identity::{self, Actor},
    organization,
    test_support::TestFixture,
    urls::Sort,
};

async fn upgrade_scalar(database: &db::Database, sql: &str) -> i64 {
    db::query_scalar(sql)
        .fetch_one(database.executor())
        .await
        .unwrap()
}

async fn fixture() -> (TestFixture, Actor) {
    let services = TestFixture::migrated().await.unwrap();
    content::seed_forums(&services.db).await.unwrap();
    content::seed_fixture(&services.db).await.unwrap();
    let uid: i64 = db::query_scalar("SELECT MIN(uid) FROM gonof_common_member")
        .fetch_one(services.db.executor())
        .await
        .unwrap();
    db::query(
        "UPDATE gonof_common_member SET adminid=1,password='organization-test-hash' WHERE uid=?",
    )
    .bind(uid)
    .execute(services.db.executor())
    .await
    .unwrap();
    (
        services,
        Actor {
            uid,
            credentials_version: 1,
            api_key: None,
        },
    )
}
type PostIdentity = (
    i64,
    i64,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
);
async fn positions(services: &TestFixture, tid: i64) -> Vec<PostIdentity> {
    db::query("SELECT pid,position,root_pid,reply_to_pid,first_tid,root_position,sub_position FROM gonof_forum_post WHERE tid=? ORDER BY position").bind(tid).fetch_all(services.db.executor()).await.unwrap().into_iter().map(|r|(r.try_get("pid").unwrap(),r.try_get("position").unwrap(),r.try_get("root_pid").unwrap(),r.try_get("reply_to_pid").unwrap(),r.try_get("first_tid").unwrap(),r.try_get("root_position").unwrap(),r.try_get("sub_position").unwrap())).collect()
}
#[tokio::test]
async fn move_preserves_all_positions_and_inherits_member_scope() {
    let (services, actor) = fixture().await;
    let database = &services.db;
    let parent = organization::save_forum(
        database,
        actor,
        &organization::ForumInput {
            name: "Members forum".into(),
            slug: "member-scope".into(),
            visibility: "members".into(),
            status: 1,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let child = organization::save_forum(
        database,
        actor,
        &organization::ForumInput {
            name: "Child forum".into(),
            slug: "child-scope".into(),
            parent_fid: parent,
            visibility: "public".into(),
            status: 1,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let old = positions(&services, 103).await;
    assert!(old.len() > 50);
    let version: i64 = db::query_scalar("SELECT version FROM gonof_forum_thread WHERE tid=103")
        .fetch_one(database.executor())
        .await
        .unwrap();
    let key = identity::random_token();
    organization::move_thread(
        database,
        actor,
        103,
        child,
        version,
        &key,
        "Moving keeps the floors",
    )
    .await
    .unwrap();
    assert_eq!(old, positions(&services, 103).await);
    assert_eq!(
        db::query_scalar::<i64>("SELECT COUNT(*) FROM gonof_forum_post WHERE tid=103 AND fid<>?")
            .bind(child)
            .fetch_one(database.executor())
            .await
            .unwrap(),
        0
    );
    assert!(content::thread(database, 103).await.unwrap().is_none());
    assert!(
        content::thread_for_viewer(
            database,
            103,
            content::Readable {
                uid: Some(actor.uid),
                forums: &content::readable_forums(database, Some(actor.uid))
                    .await
                    .unwrap(),
            },
        )
        .await
        .unwrap()
        .is_some()
    );
    assert_eq!(
        content::list_threads_for_viewer(
            database,
            "member-scope",
            Sort::Latest,
            None,
            None,
            1,
            20,
            Some(actor.uid),
            &[]
        )
        .await
        .unwrap()
        .total,
        0
    );
    assert_eq!(
        content::list_threads_for_viewer(
            database,
            "child-scope",
            Sort::Latest,
            None,
            None,
            1,
            20,
            Some(actor.uid),
            &[]
        )
        .await
        .unwrap()
        .total,
        1
    );
    organization::move_thread(
        database,
        actor,
        103,
        child,
        version,
        &key,
        "Moving keeps the floors",
    )
    .await
    .unwrap();
    // first_tid is only written when the first post is inserted and is immutable (pinned by the source scan); a move
    // does not change it.
    assert_eq!(
        upgrade_scalar(
            database,
            "SELECT first_tid FROM gonof_forum_post WHERE tid=103 AND first=1"
        )
        .await,
        103
    );
    assert!(
        db::query("UPDATE gonof_forum_post SET fid=? WHERE tid=103")
            .bind(parent)
            .execute(database.executor())
            .await
            .is_err()
    );
    let owner: i64 = db::query_scalar("SELECT authorid FROM gonof_forum_thread WHERE tid=103")
        .fetch_one(database.executor())
        .await
        .unwrap();
    let viewer: i64 = db::query_scalar("SELECT uid FROM gonof_common_member WHERE uid<>? LIMIT 1")
        .bind(owner)
        .fetch_one(database.executor())
        .await
        .unwrap();
    db::query("INSERT INTO gonof_home_member_block(uid,blocked_uid,dateline) VALUES(?,?,?)")
        .bind(viewer)
        .bind(owner)
        .bind(identity::now())
        .execute(database.executor())
        .await
        .unwrap();
    assert_eq!(
        content::list_threads_for_viewer(
            database,
            "child-scope",
            Sort::Latest,
            None,
            None,
            1,
            20,
            Some(viewer),
            &[]
        )
        .await
        .unwrap()
        .total,
        0
    );
    let forums = content::readable_forums(database, Some(viewer))
        .await
        .unwrap();
    let mut thread = content::thread_for_viewer(
        database,
        103,
        content::Readable {
            uid: Some(viewer),
            forums: &forums,
        },
    )
    .await
    .unwrap()
    .unwrap();
    let cache_key = gono_forum_web::cache::CacheKey::new(
        &gono_forum_web::cache::registry::FLOORS,
        content::cache_revision(database).await.unwrap(),
        &(
            thread.id,
            thread.cache_revision,
            1_usize,
            false,
            None::<u64>,
            1_usize,
        ),
    )
    .unwrap();
    content::cached_replies_for_viewer(
        database,
        &services.cache.timed(gono_forum_web::cache::Lifetimes {
            member: 30,
            page: 60,
            list: 60,
            statistics: 600,
            forum_statistics: 600,
        }),
        &mut thread,
        1,
        false,
        None,
        1,
        (Some(viewer), gono_forum_web::test_support::SEEDED_PAGING),
    )
    .await
    .unwrap();
    assert!(
        services
            .cache
            .get_json::<Vec<content::Post>>(&cache_key)
            .await
            .unwrap()
            .is_none(),
        "member content must not enter the shared reply cache"
    );
    content::apply_blocks(database, Some(viewer), &mut thread)
        .await
        .unwrap();
    assert!(thread.blocked);
    let pid: i64 = db::query_scalar(
        "SELECT pid FROM gonof_forum_post WHERE tid=103 AND first=0 ORDER BY position LIMIT 1",
    )
    .fetch_one(database.executor())
    .await
    .unwrap();
    db::query("UPDATE gonof_forum_post SET message='',deleted_at=?,erased_at=?,version=version+1 WHERE pid=?").bind(identity::now()).bind(identity::now()).bind(pid).execute(database.executor()).await.unwrap();
    let editor =
        content::edits::ContentEditor::new(database.clone(), services.config.content.clone(), 3600);
    assert!(editor.editable(actor, pid).await.is_err());
    // Every request record must store the request time; rows that miss it have dateline 0 and would be
    // deleted by the first cleanup.
    assert_eq!(
        upgrade_scalar(
            database,
            "SELECT COUNT(*) FROM gonof_forum_post_request WHERE dateline<=0"
        )
        .await,
        0,
        "the request record must carry the request time"
    );
    assert!(upgrade_scalar(database, "SELECT COUNT(*) FROM gonof_forum_post_request").await > 0);
    services.cleanup().await.unwrap();
}
