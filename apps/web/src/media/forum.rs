//! Safe images bound to the current forum; every read rechecks the inherited access scope.
use super::*;

impl Media {
    /// Forum icon for page display: scaling, in-process cache and `ETag` negotiation, see `display.rs`.
    /// Always queries the current binding and forum visibility first.
    pub async fn forum_icon_image(
        &self,
        fid: i64,
        aid: i64,
        actor: Option<identity::Actor>,
        if_none_match: Option<&str>,
    ) -> Result<DisplayImage> {
        let (row, upload) = self.current_forum_icon(fid, aid, actor).await?;
        Box::pin(self.display_image("forum-icon", &row, &upload, if_none_match)).await
    }

    async fn current_forum_icon(
        &self,
        fid: i64,
        aid: i64,
        actor: Option<identity::Actor>,
    ) -> Result<(DbRow, Upload)> {
        if fid <= 0 || aid <= 0 {
            return Err(MediaError::NotFound);
        }
        let viewer_uid = match actor {
            Some(actor) if self.active_actor(actor).await? => Some(actor.uid),
            _ => None,
        };
        // Fetch the current icon and the raw state of the forum and its parent by forum primary key; visibility and
        // attachment state are decided in code, the same way storage.rs reads post attachments.
        let row = db::query(&format!(
            "SELECT {COLUMNS}, f.status AS forum_status, f.visibility AS forum_visibility, f.parent_fid,
             ancestor.status AS ancestor_status, ancestor.visibility AS ancestor_visibility
             FROM gonof_forum_forum f LEFT JOIN gonof_forum_forum ancestor ON ancestor.fid=f.parent_fid
             JOIN gonof_forum_attachment a ON a.aid=f.icon_aid JOIN gonof_forum_attachment_0 d ON d.aid=a.aid
             WHERE f.fid=?"
        ))
        .bind(fid)
        .fetch_optional(self.db.executor())
        .await?
        .ok_or(MediaError::NotFound)?;
        let upload = decode_upload(&row)?;
        if upload.aid != aid
            || !forum_path_readable(&row, viewer_uid.is_some())?
            || !upload.is_ready_image()
        {
            return Err(MediaError::NotFound);
        }
        Ok((row, upload))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn attachments_respect_members_ancestors_and_permanent_erasure() {
        let fixture = crate::test_support::TestFixture::migrated().await.unwrap();
        crate::content::seed_forums(&fixture.db).await.unwrap();
        crate::content::seed_fixture(&fixture.db).await.unwrap();
        let row = db::query("SELECT p.pid,p.fid,p.authorid FROM gonof_forum_post p WHERE p.tid=103 AND p.first=0 ORDER BY p.position LIMIT 1")
            .fetch_one(fixture.db.executor()).await.unwrap();
        let uid: i64 = row.try_get("authorid").unwrap();
        let pid: i64 = row.try_get("pid").unwrap();
        let fid: i64 = row.try_get("fid").unwrap();
        let aid = db::query(
            "INSERT INTO gonof_forum_attachment(uid,request_key,request_hash) VALUES(?,?,?)",
        )
        .bind(uid)
        .bind(identity::random_token())
        .bind(identity::random_token())
        .execute(fixture.db.executor())
        .await
        .unwrap()
        .last_insert_id() as i64;
        let uploaded_at = identity::now();
        db::query("INSERT INTO gonof_forum_attachment_0(aid,filename,filesize,mime_type,content_sha256,attachment,storage_profile_id,description,alt_text,dateline,updated_at,lifecycle,scan_status,scan_detail) VALUES(?,'test.png',10,'image/png',?,'test','test','','',?,?,'ready','clean','')")
            .bind(aid).bind(identity::random_token()).bind(uploaded_at).bind(uploaded_at).execute(fixture.db.executor()).await.unwrap();
        db::query("INSERT INTO gonof_forum_attachment_reference(aid,pid,dateline) VALUES(?,?,?)")
            .bind(aid)
            .bind(pid)
            .bind(identity::now())
            .execute(fixture.db.executor())
            .await
            .unwrap();
        let media = Media::new(
            fixture.db.clone(),
            fixture.config.media.clone(),
            fixture.root.clone(),
        );
        let actor = identity::Actor {
            uid,
            credentials_version: 1,
            api_key: None,
        };
        assert!(media.post_attachment_allowed(aid, pid, None).await.unwrap());
        let parent = db::query("INSERT INTO gonof_forum_forum(name,slug,visibility,description,guidance) VALUES('Members parent','attachment-parent','members','','')")
            .execute(fixture.db.executor()).await.unwrap().last_insert_id() as i64;
        db::query("UPDATE gonof_forum_forum SET parent_fid=? WHERE fid=?")
            .bind(parent)
            .bind(fid)
            .execute(fixture.db.executor())
            .await
            .unwrap();
        assert!(!media.post_attachment_allowed(aid, pid, None).await.unwrap());
        assert!(
            media
                .post_attachment_allowed(aid, pid, media.verified_viewer(actor).await.unwrap())
                .await
                .unwrap()
        );
        let invalid = identity::Actor {
            credentials_version: 2,
            ..actor
        };
        assert!(
            !media
                .post_attachment_allowed(aid, pid, media.verified_viewer(invalid).await.unwrap())
                .await
                .unwrap()
        );
        db::query("UPDATE gonof_forum_forum SET visibility='private' WHERE fid=?")
            .bind(parent)
            .execute(fixture.db.executor())
            .await
            .unwrap();
        assert!(
            !media
                .post_attachment_allowed(aid, pid, media.verified_viewer(actor).await.unwrap())
                .await
                .unwrap()
        );
        db::query("UPDATE gonof_forum_forum SET visibility='public' WHERE fid=?")
            .bind(parent)
            .execute(fixture.db.executor())
            .await
            .unwrap();
        db::query("UPDATE gonof_forum_post SET message='',deleted_at=?,erased_at=? WHERE pid=?")
            .bind(identity::now())
            .bind(identity::now())
            .bind(pid)
            .execute(fixture.db.executor())
            .await
            .unwrap();
        db::query(
            "UPDATE gonof_common_member SET adminid=1,password='organization-test-hash' WHERE uid=?",
        )
        .bind(uid)
        .execute(fixture.db.executor())
        .await
        .unwrap();
        assert!(
            !media
                .post_attachment_allowed(aid, pid, media.verified_viewer(actor).await.unwrap())
                .await
                .unwrap()
        );
        // A live brand and forum binding each protect the image after its post reference disappears.
        db::query("DELETE FROM gonof_forum_attachment_reference WHERE aid=?")
            .bind(aid)
            .execute(fixture.db.executor())
            .await
            .unwrap();
        db::query("UPDATE gonof_forum_forum SET icon_aid=? WHERE fid=?")
            .bind(aid)
            .bind(fid)
            .execute(fixture.db.executor())
            .await
            .unwrap();
        db::query(
            "UPDATE gonof_common_settings SET svalue='true' WHERE skey='settings.site.read_only'",
        )
        .execute(fixture.db.executor())
        .await
        .unwrap();
        assert!(media.set_avatar(actor, Some(aid)).await.is_err());
        media
            .set_brand(&fixture.global, actor, "logo", Some(aid), 0)
            .await
            .unwrap();
        db::query("UPDATE gonof_forum_attachment_0 SET purge_after=? WHERE aid=?")
            .bind(uploaded_at + 1)
            .bind(aid)
            .execute(fixture.db.executor())
            .await
            .unwrap();
        let mut tx = fixture.db.begin_write().await.unwrap();
        assert_eq!(
            media
                .attachment_reference_count(&mut tx, aid)
                .await
                .unwrap(),
            2
        );
        tx.commit().await.unwrap();
        media.cleanup(uploaded_at + 2).await.unwrap();
        let row =
            db::query("SELECT lifecycle,purge_after FROM gonof_forum_attachment_0 WHERE aid=?")
                .bind(aid)
                .fetch_one(fixture.db.executor())
                .await
                .unwrap();
        assert_eq!(db::text(&row, "lifecycle").unwrap(), "ready");
        assert_eq!(row.try_get::<Option<i64>, _>("purge_after").unwrap(), None);
        let (path, mut file) = media.staging_file().await.unwrap();
        file.write_all(b"readonly upload fixture").await.unwrap();
        // tokio finishes file writes on a background thread; dropping the handle without a sync first means a size read
        // right afterwards may be 0, and the upload would be stopped by "The file is empty or larger than the upload
        // limit" before ever reaching the read-only check. Kept consistent with the other staging helpers.
        file.sync_all().await.unwrap();
        drop(file);
        let upload = media
            .register_upload_actor(
                actor,
                NewUpload {
                    request_key: identity::random_token(),
                    filename: "read-only.png".into(),
                    description: String::new(),
                    alt_text: String::new(),
                    path,
                },
            )
            .await;
        // Include the actual return value: without the sync_all above, the MySQL 5.7 two-thread run occasionally got
        // Validation("The file is empty…"); "assertion failed" alone cannot tell whether the read-only switch had no
        // effect or that step was never reached.
        assert!(
            matches!(upload,Err(MediaError::Validation(ref message)) if message.contains("read-only")),
            "an upload while read-only must fail validation, got {upload:?}"
        );
        db::query("UPDATE gonof_common_member SET account_state='deleting' WHERE uid=?")
            .bind(uid)
            .execute(fixture.db.executor())
            .await
            .unwrap();
        assert!(!media.active_actor(actor).await.unwrap());
        assert!(!media.contributing_actor(actor).await.unwrap());
        let mut tx = fixture.db.begin_write().await.unwrap();
        assert!(
            media
                .member_actor_locked(&mut tx, actor, true)
                .await
                .is_err()
        );
        tx.rollback().await.unwrap();
        fixture.cleanup().await.unwrap();
    }
}
