use gono_forum_web::{
    content::{self, MemberContentKind, home},
    db,
    identity::Actor,
    test_support::TestFixture,
};

#[tokio::test]
async fn homepage_data_respects_identity_privacy_and_collection_counts() {
    let fixture = TestFixture::migrated().await.unwrap();
    let database = &fixture.db;
    content::seed_forums(database).await.unwrap();
    content::import_fixture(database, &content::fixtures())
        .await
        .unwrap();
    let members = home::recent_members(database, None, 8).await.unwrap();
    assert!(!members.is_empty());
    assert!(members.len() <= 8);
    let uid = members[0].uid;
    let version: i64 =
        db::query_scalar("SELECT credentials_version FROM gonof_common_member WHERE uid=?")
            .bind(uid)
            .fetch_one(database.executor())
            .await
            .unwrap();
    let actor = Actor {
        uid,
        credentials_version: version,
        api_key: None,
    };
    let rules = gono_forum_web::activities::rules(database).await.unwrap();
    let summary = home::member_summary(
        database,
        actor,
        &rules,
        gono_forum_web::test_support::SEEDED_OFFSET_MINUTES,
    )
    .await
    .unwrap();
    let forums = content::readable_forums(database, Some(uid)).await.unwrap();
    let readable = content::Readable {
        uid: Some(uid),
        forums: &forums,
    };
    let threads = content::member_content_for_viewer(
        database,
        uid,
        MemberContentKind::Threads,
        1,
        1,
        readable,
        &[],
        gono_forum_web::test_support::SEEDED_PAGING,
    )
    .await
    .unwrap();
    let replies = content::member_content_for_viewer(
        database,
        uid,
        MemberContentKind::Posts,
        1,
        1,
        readable,
        &[],
        gono_forum_web::test_support::SEEDED_PAGING,
    )
    .await
    .unwrap();
    assert_eq!(summary.threads, threads.total as i64);
    assert_eq!(summary.replies, replies.total as i64);
    assert!(
        home::member_summary(
            database,
            Actor {
                credentials_version: version + 1,
                ..actor
            },
            &rules,
            gono_forum_web::test_support::SEEDED_OFFSET_MINUTES,
        )
        .await
        .is_err()
    );
    db::query("UPDATE gonof_common_member SET space_visibility='private' WHERE uid=?")
        .bind(uid)
        .execute(database.executor())
        .await
        .unwrap();
    assert!(
        home::recent_members(database, None, 8)
            .await
            .unwrap()
            .iter()
            .all(|member| member.uid != uid)
    );
    assert!(
        home::recent_members(database, Some(uid), 8)
            .await
            .unwrap()
            .iter()
            .all(|member| member.uid != uid)
    );
    db::query("UPDATE gonof_common_member SET freeze=1 WHERE uid=?")
        .bind(uid)
        .execute(database.executor())
        .await
        .unwrap();
    assert!(
        home::member_summary(
            database,
            actor,
            &rules,
            gono_forum_web::test_support::SEEDED_OFFSET_MINUTES
        )
        .await
        .is_err()
    );
    // The fixture's last replies are all on 2026-09-08 and hot threads only cover the last 7 days, so take the
    // fixture's day.
    let sidebar = content::sidebar_at(
        database,
        1788864000,
        gono_forum_web::test_support::SEEDED_SIDEBAR,
    )
    .await
    .unwrap();
    assert_eq!(sidebar.hot.len(), 10);
    assert_eq!(sidebar.latest.len(), 10);
    fixture.cleanup().await.unwrap();
}
