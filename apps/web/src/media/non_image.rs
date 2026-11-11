//! Stops serving non-image attachments already in the database: only image uploads are accepted, so such
//! attachments are unreferenced and their objects deleted.
//!
//! Used only by `gono-forum-cli purge-non-image-attachments`. By default it only counts and changes nothing; with
//! `--confirm` it opens a short write transaction per attachment: locks the attachment row, deletes post references,
//! marks it pending purge and queues it into `media.purge`; the objects are deleted by the Web process's purge job
//! (`purge_claim` in `worker.rs`). Links in post bodies are kept and open as 404; when an old post is edited, purged
//! attachments are not bound again (`content/edits.rs`).
use super::*;

/// A ready non-image attachment and the posts that reference it.
#[derive(Debug)]
pub struct NonImageAttachment {
    pub aid: i64,
    pub uid: i64,
    pub filename: String,
    pub mime_type: String,
    pub filesize: i64,
    pub pids: Vec<i64>,
}

#[derive(Debug, Default)]
pub struct NonImageReport {
    /// Attachments found (listed for both dry runs and confirmed runs).
    pub attachments: Vec<NonImageAttachment>,
    /// Number marked pending purge in a confirmed run.
    pub queued: usize,
    /// Number skipped in a confirmed run: the status changed while waiting for the lock, or it is still referenced by
    /// an avatar, brand image or forum icon.
    pub skipped: usize,
}

impl NonImageReport {
    pub fn total_bytes(&self) -> i64 {
        self.attachments.iter().map(|a| a.filesize).sum()
    }

    /// Number of posts involved (deduplicated).
    pub fn posts(&self) -> usize {
        self.attachments
            .iter()
            .flat_map(|a| a.pids.iter())
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    }
}

/// Maximum number of attachments whose references are fetched at once, so the `IN` list does not get too long.
const REFERENCE_BATCH: usize = 500;

impl Media {
    /// Lists (and, when `confirm` is true, purges) ready non-image attachments.
    ///
    /// Only `ready` is considered: only ready non-images are referenced by posts and have objects; queued ones are
    /// rejected by the processing job as unsupported, and rejected ones are purged under the usual orphan deadline.
    /// Candidates use the equality prefix of the `(lifecycle, purge_after)` index.
    pub async fn purge_non_image_attachments(&self, confirm: bool) -> Result<NonImageReport> {
        let rows = crate::db::query(
            "SELECT a.aid,a.uid,d.filename,d.mime_type,d.filesize FROM gonof_forum_attachment_0 d JOIN gonof_forum_attachment a ON a.aid=d.aid WHERE d.lifecycle='ready' AND d.mime_type NOT LIKE 'image/%' ORDER BY d.aid",
        )
        .fetch_all(self.db.executor())
        .await?;
        let mut report = NonImageReport::default();
        for row in &rows {
            report.attachments.push(NonImageAttachment {
                aid: row.try_get("aid")?,
                uid: row.try_get("uid")?,
                filename: db::text(row, "filename")?,
                mime_type: db::text(row, "mime_type")?,
                filesize: row.try_get("filesize")?,
                pids: Vec::new(),
            });
        }
        for chunk in report.attachments.chunks_mut(REFERENCE_BATCH) {
            let sql = format!(
                "SELECT aid,pid FROM gonof_forum_attachment_reference WHERE aid IN ({}) ORDER BY aid,pid",
                vec!["?"; chunk.len()].join(",")
            );
            let mut query = crate::db::query(&sql);
            for attachment in chunk.iter() {
                query = query.bind(attachment.aid);
            }
            let references = query.fetch_all(self.db.executor()).await?;
            for reference in references {
                let aid: i64 = reference.try_get("aid")?;
                if let Some(attachment) = chunk.iter_mut().find(|a| a.aid == aid) {
                    attachment.pids.push(reference.try_get("pid")?);
                }
            }
        }
        if !confirm {
            return Ok(report);
        }
        for attachment in &report.attachments {
            if self.queue_non_image(attachment.aid).await? {
                report.queued += 1;
            } else {
                report.skipped += 1;
            }
        }
        Ok(report)
    }

    /// One write transaction per attachment. Returns whether it was queued for purge; when the status changed or other
    /// references remain it is left alone and false is returned.
    async fn queue_non_image(&self, aid: i64) -> Result<bool> {
        let mut tx = self.db.begin_write().await?;
        // Same attachment row lock as binding, editing and cancelling; references are touched only after locking (the
        // lock order of `content/edits.rs`).
        let row = crate::db::query("SELECT a.uid,d.lifecycle,d.mime_type FROM gonof_forum_attachment a JOIN gonof_forum_attachment_0 d ON d.aid=a.aid WHERE a.aid=? FOR UPDATE")
            .bind(aid)
            .fetch_optional(self.db.observe(&mut *tx))
            .await?;
        let Some(row) = row else {
            tx.commit().await?;
            return Ok(false);
        };
        if db::text(&row, "lifecycle")? != "ready"
            || db::text(&row, "mime_type")?.starts_with("image/")
        {
            tx.commit().await?;
            return Ok(false);
        }
        let pids: Vec<i64> = crate::db::query_scalar(
            "SELECT pid FROM gonof_forum_attachment_reference WHERE aid=? ORDER BY pid",
        )
        .bind(aid)
        .fetch_all(self.db.observe(&mut *tx))
        .await?;
        crate::db::query("DELETE FROM gonof_forum_attachment_reference WHERE aid=?")
            .bind(aid)
            .execute(self.db.observe(&mut *tx))
            .await?;
        // Avatars, brand images and forum icons all require images, so normally none references a non-image; if one
        // does, the whole attachment is left alone.
        if self.attachment_reference_count(&mut tx, aid).await? != 0 {
            tx.rollback().await?;
            return Ok(false);
        }
        let now = identity::now();
        crate::db::query("UPDATE gonof_forum_attachment_0 SET lifecycle='purging',purge_after=GREATEST(dateline,?),updated_at=GREATEST(dateline,?) WHERE aid=?")
            .bind(now)
            .bind(now)
            .bind(aid)
            .execute(self.db.observe(&mut *tx))
            .await?;
        self.enqueue(
            &mut tx,
            aid,
            "media.purge",
            &format!("media.purge:{aid}"),
            serde_json::json!({}),
            now,
        )
        .await?;
        let uid: i64 = row.try_get("uid")?;
        self.audit(
            &mut tx,
            uid,
            "media.purge_non_image",
            serde_json::json!({"aid": aid, "pids": pids}),
            now,
        )
        .await?;
        tx.commit().await?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::tests::{FakeS3, Fixture, png};

    /// A dry run changes nothing; after confirmation references are removed and the attachment queued for purge, the
    /// purge job deletes the objects; image attachments are unaffected; links left over when editing an old post do
    /// not error.
    #[tokio::test]
    async fn non_image_attachments_are_unbound_and_purged_only_when_confirmed() {
        use crate::content::{
            edits::{ContentEditor, EditRequest},
            writes::{ContentWriter, NewThread},
        };
        let fixture = Fixture::new(MediaConfig::default()).await;
        let server = FakeS3::new(fixture.db.clone()).await;
        fixture.configure(&server.endpoint).await;
        // Once only images are open, non-images cannot be registered: upload two images first, then turn one of them
        // into a PDF processed before only images were open.
        let image = fixture
            .upload(&identity::random_token(), "photo.png", &png())
            .await
            .unwrap();
        let document = fixture
            .upload(
                &identity::random_token(),
                "report.png",
                &crate::media::tests::png_sized(3, 3),
            )
            .await
            .unwrap();
        fixture
            .media
            .process_pending(identity::now())
            .await
            .unwrap();
        crate::db::query("UPDATE gonof_forum_attachment_0 SET filename='report.pdf',mime_type='application/pdf' WHERE aid=?")
            .bind(document.aid)
            .execute(fixture.db.executor())
            .await
            .unwrap();
        crate::content::seed_forums(&fixture.db).await.unwrap();
        let fid: i64 = crate::db::query_scalar("SELECT MIN(fid) FROM gonof_forum_forum")
            .fetch_one(fixture.db.executor())
            .await
            .unwrap();
        let body = format!(
            "![picture](attachment:{}) [report.pdf](attachment:{})",
            image.aid, document.aid
        );
        let writer = ContentWriter::new(fixture.db.clone(), Default::default());
        let published = writer
            .publish(
                fixture.uid,
                NewThread {
                    visibility: Default::default(),
                    activity: Default::default(),
                    fid,
                    title: "A thread with attachments".into(),
                    body: body.clone(),
                    attachment_ids: vec![image.aid, document.aid],
                    request_key: identity::random_token(),
                },
            )
            .await
            .unwrap();
        let references = |aid: i64| {
            let db = fixture.db.clone();
            async move {
                crate::db::query_scalar::<i64>(
                    "SELECT COUNT(*) FROM gonof_forum_attachment_reference WHERE aid=?",
                )
                .bind(aid)
                .fetch_one(db.executor())
                .await
                .unwrap()
            }
        };

        let dry = fixture
            .media
            .purge_non_image_attachments(false)
            .await
            .unwrap();
        assert_eq!(dry.attachments.len(), 1);
        assert_eq!(dry.attachments[0].aid, document.aid);
        assert_eq!(dry.attachments[0].pids, vec![published.pid]);
        assert_eq!(dry.posts(), 1);
        assert_eq!((dry.queued, dry.skipped), (0, 0));
        assert_eq!(
            references(document.aid).await,
            1,
            "a dry run keeps the references"
        );

        let done = fixture
            .media
            .purge_non_image_attachments(true)
            .await
            .unwrap();
        assert_eq!((done.queued, done.skipped), (1, 0));
        assert_eq!(references(document.aid).await, 0);
        assert_eq!(
            references(image.aid).await,
            1,
            "image attachments are left alone"
        );
        let objects_before = server.state.objects.lock().unwrap().len();
        fixture
            .media
            .process_pending(identity::now())
            .await
            .unwrap();
        // The fake PDF was originally an image, so processing stored two objects, the original and the thumbnail;
        // purging deletes both.
        assert_eq!(
            server.state.objects.lock().unwrap().len(),
            objects_before - 2
        );
        let lifecycle: String =
            crate::db::query_scalar("SELECT lifecycle FROM gonof_forum_attachment_0 WHERE aid=?")
                .bind(document.aid)
                .fetch_one(fixture.db.executor())
                .await
                .unwrap();
        assert_eq!(lifecycle, "purged");
        assert!(matches!(
            fixture
                .media
                .read(document.aid, Some(published.pid), None)
                .await,
            Err(MediaError::NotFound)
        ));
        // Editing an old post: purged attachment links left in the body are not bound again, and it does not error.
        let actor = fixture.media.test_actor(fixture.uid).await.unwrap();
        let editor = ContentEditor::new(fixture.db.clone(), Default::default(), 86_400);
        let post = editor.editable(actor, published.pid).await.unwrap();
        editor
            .edit(
                actor,
                EditRequest {
                    pid: published.pid,
                    expected_post_version: post.version,
                    expected_thread_version: post.thread_version,
                    title: None,
                    body: Some(format!("{body}\n\nOne more line")),
                    request_key: identity::random_token(),
                    reason: String::new(),
                    visibility: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            references(document.aid).await,
            0,
            "a purged attachment is not bound again"
        );
        assert_eq!(references(image.aid).await, 1);
        let again = fixture
            .media
            .purge_non_image_attachments(true)
            .await
            .unwrap();
        assert!(
            again.attachments.is_empty(),
            "a second run finds nothing to purge"
        );
        fixture._services.cleanup_ref().await.unwrap();
    }
}
