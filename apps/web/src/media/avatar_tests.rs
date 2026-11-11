use super::*;
use crate::content::{
    edits::{ChangeRequest, ContentAction, ContentEditor, EditRequest},
    writes::{ContentWriter, NewThread},
};

async fn execute(db: &Database, statement: &str) {
    crate::db::raw_sql(statement)
        .execute(db.executor())
        .await
        .unwrap();
}

async fn current(fixture: &Fixture) -> Option<i64> {
    crate::db::query_scalar("SELECT avatar_aid FROM gonof_common_member WHERE uid=?")
        .bind(fixture.uid)
        .fetch_one(fixture.db.executor())
        .await
        .unwrap()
}

async fn upload(media: &Media, actor: identity::Actor, name: &str, bytes: &[u8]) -> Upload {
    let (path, mut file) = media.staging_file().await.unwrap();
    file.write_all(bytes).await.unwrap();
    file.sync_all().await.unwrap();
    drop(file);
    Box::pin(media.register_upload_actor(
        actor,
        NewUpload {
            request_key: identity::random_token(),
            filename: name.into(),
            description: String::new(),
            alt_text: String::new(),
            path,
        },
    ))
    .await
    .unwrap()
}

async fn ready(media: &Media, actor: identity::Actor) -> i64 {
    let candidate = Box::pin(upload(media, actor, "avatar.png", &png())).await;
    Box::pin(media.process_pending(identity::now()))
        .await
        .unwrap();
    assert!(
        media
            .upload_for_actor(actor, candidate.aid)
            .await
            .unwrap()
            .is_ready()
    );
    candidate.aid
}

async fn readable(media: &Media, uid: i64, aid: i64) {
    let image = Box::pin(media.read_avatar(uid, aid)).await.unwrap();
    assert!(image.is_image);
    assert_eq!(image.mime_type, "image/png");
    assert!(!image.body.collect().await.unwrap().into_bytes().is_empty());
}

async fn expiry(fixture: &Fixture, aid: i64) -> Option<i64> {
    crate::db::query_scalar("SELECT purge_after FROM gonof_forum_attachment_0 WHERE aid=?")
        .bind(aid)
        .fetch_one(fixture.db.executor())
        .await
        .unwrap()
}

pub(super) async fn contract(fixture: &Fixture, server: &FakeS3) {
    // The existing attachment quota contract has exhausted its deliberately tiny limit.
    let mut media = fixture.media.clone();
    media.config.daily_max_bytes = MediaConfig::default().daily_max_bytes;
    let actor = media.test_actor(fixture.uid).await.unwrap();
    assert_eq!(current(fixture).await, None);
    let pending = Box::pin(upload(&media, actor, "avatar.png", &png())).await;
    assert!(media.set_avatar(actor, Some(pending.aid)).await.is_err());
    assert!(
        Box::pin(media.read_avatar(actor.uid, pending.aid))
            .await
            .is_err()
    );
    Box::pin(media.process_pending(identity::now()))
        .await
        .unwrap();
    let first = pending.aid;
    media.set_avatar(actor, Some(first)).await.unwrap();
    assert_eq!(current(fixture).await, Some(first));
    assert_eq!(expiry(fixture, first).await, None);
    Box::pin(readable(&media, actor.uid, first)).await;
    assert!(
        Box::pin(media.read_avatar(actor.uid + 1, first))
            .await
            .is_err()
    );
    assert!(Box::pin(media.read_avatar(-1, first)).await.is_err());
    assert!(media.set_avatar(actor, Some(-1)).await.is_err());
    assert!(matches!(
        media.cancel_actor(actor, first).await,
        Err(MediaError::Conflict(_))
    ));
    let audits = fixture
        .scalar("SELECT COUNT(*) FROM gonof_common_log WHERE type='identity.avatar.update'")
        .await;
    media.set_avatar(actor, Some(first)).await.unwrap();
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) FROM gonof_common_log WHERE type='identity.avatar.update'")
            .await,
        audits,
        "setting the current avatar again writes no second audit row"
    );

    execute(&fixture.db, "INSERT INTO gonof_common_member(username,email,password,emailstatus) VALUES('avatar-outsider','avatar-outsider@example.test','contract-password-hash',1)").await;
    let outsider_uid = fixture
        .scalar("SELECT uid FROM gonof_common_member WHERE username='avatar-outsider'")
        .await;
    let outsider = media.test_actor(outsider_uid).await.unwrap();
    let outsider_aid = Box::pin(ready(&media, outsider)).await;
    assert!(matches!(
        media.set_avatar(actor, Some(outsider_aid)).await,
        Err(MediaError::NotFound)
    ));
    assert!(matches!(
        media.set_avatar(outsider, Some(first)).await,
        Err(MediaError::NotFound)
    ));
    assert_eq!(current(fixture).await, Some(first));
    assert!(
        Box::pin(media.read_avatar(actor.uid, outsider_aid))
            .await
            .is_err()
    );

    let candidate = Box::pin(ready(&media, actor)).await;
    for assignment in [
        "lifecycle='pending'",
        "lifecycle='processing'",
        "lifecycle='rejected'",
        "lifecycle='purging'",
        "lifecycle='purged'",
        "scan_status='pending'",
        "scan_status='error'",
        "scan_status='rejected'",
        "mime_type='text/plain'",
    ] {
        execute(
            &fixture.db,
            &format!("UPDATE gonof_forum_attachment_0 SET {assignment} WHERE aid={candidate}"),
        )
        .await;
        assert!(
            media.set_avatar(actor, Some(candidate)).await.is_err(),
            "the candidate must not be usable: {assignment}"
        );
        assert_eq!(current(fixture).await, Some(first));
        execute(&fixture.db, &format!("UPDATE gonof_forum_attachment_0 SET lifecycle='ready',scan_status='clean',mime_type='image/png' WHERE aid={candidate}")).await;
    }
    execute(
        &fixture.db,
        &format!(
            "UPDATE gonof_forum_attachment_0 SET dateline={},purge_after={} WHERE aid={candidate}",
            identity::now() - 2,
            identity::now() - 1
        ),
    )
    .await;
    assert!(media.set_avatar(actor, Some(candidate)).await.is_err());
    execute(
        &fixture.db,
        &format!(
            "UPDATE gonof_forum_attachment_0 SET purge_after={} WHERE aid={candidate}",
            identity::now() + media.config.orphan_seconds
        ),
    )
    .await;
    for assignment in [
        "credentials_version=2",
        "`freeze`=1",
        "adminid=0,emailstatus=0,email='avatar-owner@example.test'",
    ] {
        execute(
            &fixture.db,
            &format!(
                "UPDATE gonof_common_member SET {assignment} WHERE uid={}",
                actor.uid
            ),
        )
        .await;
        assert!(matches!(
            media.set_avatar(actor, Some(candidate)).await,
            Err(MediaError::Forbidden)
        ));
        assert!(matches!(
            media.set_avatar(actor, None).await,
            Err(MediaError::Forbidden)
        ));
        assert_eq!(current(fixture).await, Some(first));
        execute(&fixture.db, &format!("UPDATE gonof_common_member SET credentials_version=1,`freeze`=0,adminid=1 WHERE uid={}", actor.uid)).await;
    }
    Box::pin(assert_atomic_replacement(
        fixture, &media, actor, first, candidate,
    ))
    .await;

    let objects = server.state.objects.lock().unwrap().len();
    media.set_avatar(actor, Some(candidate)).await.unwrap();
    assert_eq!(current(fixture).await, Some(candidate));
    assert_eq!(expiry(fixture, candidate).await, None);
    assert!(
        expiry(fixture, first).await.unwrap() >= identity::now() + media.config.orphan_seconds - 1
    );
    assert!(Box::pin(media.read_avatar(actor.uid, first)).await.is_err());
    Box::pin(readable(&media, actor.uid, candidate)).await;
    media.set_avatar(actor, None).await.unwrap();
    assert_eq!(current(fixture).await, None);
    assert!(
        Box::pin(media.read_avatar(actor.uid, candidate))
            .await
            .is_err()
    );
    assert!(expiry(fixture, candidate).await.is_some());
    assert_eq!(
        server.state.objects.lock().unwrap().len(),
        objects,
        "replacing or removing must not delete the old image at once"
    );

    execute(&fixture.db, &format!("UPDATE gonof_common_member SET adminid=0,emailstatus=1,email='avatar-owner@example.test' WHERE uid={}", actor.uid)).await;
    media.set_avatar(actor, Some(first)).await.unwrap();
    execute(
        &fixture.db,
        &format!(
            "UPDATE gonof_common_member SET adminid=1 WHERE uid={}",
            actor.uid
        ),
    )
    .await;
    Box::pin(assert_post_references(fixture, &media, actor, first)).await;
    Box::pin(assert_cleanup_races(fixture, &media, actor, first)).await;
    Box::pin(assert_purge_protection(fixture, &media, actor, server)).await;
    Box::pin(assert_late_upload_states(fixture, &media, actor, server)).await;
}

async fn assert_late_upload_states(
    fixture: &Fixture,
    media: &Media,
    actor: identity::Actor,
    server: &FakeS3,
) {
    // Inject the final state just after object PUT so the worker observes it under its detail lock.
    for lifecycle in ["ready", "rejected", "purging", "purged"] {
        let candidate = Box::pin(upload(media, actor, "late-avatar.png", &png())).await;
        let objects = server.state.objects.lock().unwrap().len();
        *server.state.lifecycle_after_put.lock().unwrap() = Some((candidate.aid, lifecycle));
        let report = Box::pin(media.process_pending(identity::now()))
            .await
            .unwrap();
        assert_eq!(report.failed, 0);
        if lifecycle == "ready" {
            assert_eq!(
                server.state.objects.lock().unwrap().len(),
                objects + 2,
                "ready objects (original and thumbnail) can still be bound and must not be deleted after the detail lock is released"
            );
            media.set_avatar(actor, Some(candidate.aid)).await.unwrap();
            assert_eq!(current(fixture).await, Some(candidate.aid));
            Box::pin(readable(media, actor.uid, candidate.aid)).await;
        } else {
            assert_eq!(
                server.state.objects.lock().unwrap().len(),
                objects,
                "a final state that cannot be bound again still cleans up late objects: {lifecycle}"
            );
        }
    }
}

async fn assert_atomic_replacement(
    fixture: &Fixture,
    media: &Media,
    actor: identity::Actor,
    first: i64,
    candidate: i64,
) {
    let condition = format!(
        "NEW.uid={} AND NEW.type='identity.avatar.update'",
        actor.uid
    );
    crate::test_support::fail_when(
        &fixture.db,
        "reject_avatar_audit",
        "BEFORE INSERT",
        "gonof_common_log",
        &condition,
        "avatar audit failure",
    )
    .await
    .unwrap();
    let candidate_expiry = expiry(fixture, candidate).await;
    assert!(media.set_avatar(actor, Some(candidate)).await.is_err());
    assert_eq!(current(fixture).await, Some(first));
    assert_eq!(expiry(fixture, first).await, None);
    assert_eq!(expiry(fixture, candidate).await, candidate_expiry);
    assert!(media.set_avatar(actor, None).await.is_err());
    assert_eq!(current(fixture).await, Some(first));
    assert_eq!(expiry(fixture, first).await, None);
    crate::test_support::drop_trigger(&fixture.db, "reject_avatar_audit")
        .await
        .unwrap();
    Box::pin(readable(media, actor.uid, first)).await;
}

async fn assert_post_references(
    fixture: &Fixture,
    media: &Media,
    actor: identity::Actor,
    aid: i64,
) {
    let writer = ContentWriter::new(fixture.db.clone(), Default::default());
    let fid = fixture
        .scalar("SELECT MIN(fid) FROM gonof_forum_forum")
        .await;
    let post = writer
        .publish_actor(
            actor,
            NewThread {
                visibility: Default::default(),
                activity: Default::default(),
                fid,
                title: "Avatar and body share an image".into(),
                body: format!("![avatar](attachment:{aid})"),
                attachment_ids: vec![aid],
                request_key: identity::random_token(),
            },
        )
        .await
        .unwrap();
    media.set_avatar(actor, None).await.unwrap();
    assert_eq!(
        expiry(fixture, aid).await,
        None,
        "removing the avatar while a post still uses it must not schedule a purge"
    );
    media.set_avatar(actor, Some(aid)).await.unwrap();
    let editor = ContentEditor::new(
        fixture.db.clone(),
        Default::default(),
        media.config.orphan_seconds,
    );
    for action in [ContentAction::Withdraw, ContentAction::Restore] {
        let current = editor.editable(actor, post.pid).await.unwrap();
        editor
            .change(
                actor,
                ChangeRequest {
                    pid: post.pid,
                    expected_post_version: current.version,
                    expected_thread_version: current.thread_version,
                    request_key: identity::random_token(),
                    reason: String::new(),
                    action,
                },
            )
            .await
            .unwrap();
        assert_eq!(expiry(fixture, aid).await, None);
        Box::pin(readable(media, actor.uid, aid)).await;
    }
    let current = editor.editable(actor, post.pid).await.unwrap();
    editor
        .edit(
            actor,
            EditRequest {
                visibility: None,
                pid: post.pid,
                expected_post_version: current.version,
                expected_thread_version: current.thread_version,
                title: Some("Avatar kept, image removed from the body".into()),
                body: Some("The image was removed from the body".into()),
                request_key: identity::random_token(),
                reason: String::new(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        fixture
            .scalar(&format!(
                "SELECT COUNT(*) FROM gonof_forum_attachment_reference WHERE aid={aid}"
            ))
            .await,
        0
    );
    assert_eq!(
        expiry(fixture, aid).await,
        None,
        "removing the last post reference must not schedule the current avatar for purge"
    );
    Box::pin(readable(media, actor.uid, aid)).await;
}

async fn assert_cleanup_races(
    fixture: &Fixture,
    media: &Media,
    actor: identity::Actor,
    protected: i64,
) {
    // Even a previously scheduled deadline cannot delete an image while it is the current avatar.
    execute(
        &fixture.db,
        &format!(
            "UPDATE gonof_forum_attachment_0 SET purge_after={} WHERE aid={protected}",
            identity::now() + 1
        ),
    )
    .await;
    Box::pin(media.cleanup(identity::now() + 8 * 86400))
        .await
        .unwrap();
    Box::pin(readable(media, actor.uid, protected)).await;
    execute(
        &fixture.db,
        &format!("UPDATE gonof_forum_attachment_0 SET purge_after=NULL WHERE aid={protected}"),
    )
    .await;
    let candidate = Box::pin(ready(media, actor)).await;
    let (assigned, cancelled) = tokio::join!(
        media.set_avatar(actor, Some(candidate)),
        media.cancel_actor(actor, candidate),
    );
    match (assigned, cancelled) {
        (Ok(()), Err(MediaError::Conflict(_))) => {
            assert_eq!(current(fixture).await, Some(candidate))
        }
        (Err(MediaError::Validation(_)), Ok(())) => {
            assert_eq!(current(fixture).await, Some(protected))
        }
        result => {
            panic!("exactly one of setting and cancelling the avatar must succeed: {result:?}")
        }
    }
    let active = current(fixture).await.unwrap();
    Box::pin(readable(media, actor.uid, active)).await;
    let candidate = Box::pin(ready(media, actor)).await;
    let (assigned, cleaned) = tokio::join!(
        media.set_avatar(actor, Some(candidate)),
        Box::pin(media.cleanup(identity::now() + 8 * 86400)),
    );
    cleaned.unwrap();
    if assigned.is_ok() {
        assert_eq!(current(fixture).await, Some(candidate));
        assert_eq!(expiry(fixture, candidate).await, None);
    } else {
        assert_eq!(current(fixture).await, Some(active));
        assert_ne!(
            media
                .upload_for_actor(actor, candidate)
                .await
                .unwrap()
                .lifecycle,
            "ready"
        );
    }
    let active = current(fixture).await.unwrap();
    Box::pin(readable(media, actor.uid, active)).await;
}

async fn assert_purge_protection(
    fixture: &Fixture,
    media: &Media,
    actor: identity::Actor,
    server: &FakeS3,
) {
    let aid = current(fixture).await.unwrap();
    let count = server.state.objects.lock().unwrap().len();
    // Exercise a stale purge job explicitly: even inconsistent task state must never delete a current avatar.
    execute(
        &fixture.db,
        &format!("UPDATE gonof_forum_attachment_0 SET lifecycle='purging' WHERE aid={aid}"),
    )
    .await;
    execute(&fixture.db, &format!("INSERT INTO gonof_common_outbox(event_key,event_type,aggregate_type,aggregate_id,payload,dateline,available_at) VALUES('avatar-purge-protection-{aid}','media.purge','attachment',{aid},'{{}}',{}, {})", identity::now(), identity::now())).await;
    assert!(
        Box::pin(media.process_pending(identity::now()))
            .await
            .unwrap()
            .failed
            >= 1
    );
    assert_eq!(server.state.objects.lock().unwrap().len(), count);
    execute(
        &fixture.db,
        &format!(
            "UPDATE gonof_forum_attachment_0 SET lifecycle='purged',updated_at={} WHERE aid={aid}",
            identity::now()
        ),
    )
    .await;
    Box::pin(media.cleanup(identity::now() + 600))
        .await
        .unwrap();
    assert_eq!(
        server.state.objects.lock().unwrap().len(),
        count,
        "the final-state cleanup re-check must protect the current avatar too"
    );
    execute(
        &fixture.db,
        &format!(
            "UPDATE gonof_forum_attachment_0 SET lifecycle='ready',purge_after=NULL WHERE aid={aid}"
        ),
    )
    .await;
    Box::pin(readable(media, actor.uid, aid)).await;
}
