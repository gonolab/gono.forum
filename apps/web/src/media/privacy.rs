//! Read a safe stored rendition for a private, owner-scoped archive, never a quarantine source.
use super::*;
impl Media {
    /// The original file of an attachment, for the owner's export.
    ///
    /// Returns `NotFound` when the attachment can no longer be exported, or when the object is missing from
    /// storage (NoSuchKey, 404): both cases stay the same no matter how often they are retried, so the export
    /// skips it and notes it in the archive. Other storage errors are reported as unavailable as usual, and the
    /// whole export is retried later.
    pub(crate) async fn export_owned(&self, aid: i64, uid: i64) -> Result<MediaDownload> {
        let row=db::query(&format!("SELECT {COLUMNS} FROM gonof_forum_attachment a JOIN gonof_forum_attachment_0 d ON d.aid=a.aid WHERE a.aid=? AND a.uid=?"))
            .bind(aid).bind(uid).fetch_optional(self.db.executor()).await?.ok_or(MediaError::NotFound)?;
        if db::text(&row, "lifecycle")? != "ready" || db::text(&row, "scan_status")? != "clean" {
            return Err(MediaError::NotFound);
        }
        let upload = decode_upload(&row)?;
        let (client, profile) = self
            .object_client(&db::text(&row, "storage_profile_id")?)
            .await?;
        let object = client
            .get_object()
            .bucket(profile.bucket)
            .key(db::text(&row, "attachment")?)
            .send()
            .await
            .map_err(|error| {
                let missing = error
                    .as_service_error()
                    .is_some_and(|error| error.is_no_such_key())
                    || error
                        .raw_response()
                        .is_some_and(|response| response.status().as_u16() == 404);
                if missing {
                    MediaError::NotFound
                } else {
                    MediaError::Unavailable(crate::i18n::current().media_error_storage().into())
                }
            })?;
        Ok(MediaDownload {
            filename: upload.filename,
            mime_type: upload.mime_type.clone(),
            filesize: object.content_length.unwrap_or(upload.filesize),
            is_image: upload.mime_type.starts_with("image/"),
            body: object.body,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::privacy::{self, Access, NewRequest};
    use std::io::Read;

    #[tokio::test]
    async fn owner_archive_includes_safe_renditions_and_excludes_pending_objects() {
        let fixture = super::super::tests::Fixture::new(MediaConfig::default()).await;
        let s3 = super::super::tests::FakeS3::new(fixture.db.clone()).await;
        fixture.configure(&s3.endpoint).await;
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(2, 2)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let bytes = png.into_inner();
        let ready = Box::pin(fixture.upload(&identity::random_token(), "ready.png", &bytes))
            .await
            .unwrap();
        Box::pin(fixture.media.process_pending(identity::now()))
            .await
            .unwrap();
        let pending = Box::pin(fixture.upload(&identity::random_token(), "pending.png", &bytes))
            .await
            .unwrap();
        assert!(
            Box::pin(fixture.media.export_owned(ready.aid, fixture.uid + 1))
                .await
                .is_err()
        );
        assert!(
            Box::pin(fixture.media.export_owned(pending.aid, fixture.uid))
                .await
                .is_err()
        );
        // Data request mailbox tickets are only issued to frozen members.
        crate::db::query(
            "UPDATE gonof_common_member SET email='archive@example.test',`freeze`=1 WHERE uid=?",
        )
        .bind(fixture.uid)
        .execute(fixture.db.executor())
        .await
        .unwrap();
        let now = identity::now();
        privacy::request_access(&fixture.db, "archive@example.test", now)
            .await
            .unwrap();
        let payload: String = crate::db::query_scalar(
            "SELECT payload FROM gonof_common_outbox WHERE event_type='privacy_access'",
        )
        .fetch_one(fixture.db.executor())
        .await
        .unwrap();
        let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
        let (token, _) =
            privacy::exchange_access(&fixture.db, payload["token"].as_str().unwrap(), now)
                .await
                .unwrap();
        let access = Access::Token(token);
        let id = privacy::submit(
            &fixture.db,
            &access,
            NewRequest {
                kind: "export".into(),
                content_mode: "retain".into(),
                request_key: identity::random_token(),
                password: String::new(),
            },
            now,
        )
        .await
        .unwrap();
        Box::pin(privacy::process_pending(
            &fixture.db,
            &fixture._services.global,
            &fixture.media,
            &fixture.root,
            Path::new(".local"),
            90,
            now,
        ))
        .await
        .unwrap();
        let dir = privacy::directory(&fixture.root, Path::new(".local"));
        let (path, _) = privacy::download(&fixture.db, &access, id, &dir, now)
            .await
            .unwrap();
        let mut zip = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
        let mut archived = Vec::new();
        zip.by_name(&format!("attachments/{}.bin", ready.aid))
            .unwrap()
            .read_to_end(&mut archived)
            .unwrap();
        let expected = fixture
            .media
            .export_owned(ready.aid, fixture.uid)
            .await
            .unwrap()
            .body
            .collect()
            .await
            .unwrap()
            .into_bytes();
        assert_eq!(archived, expected.as_ref());
        assert!(
            zip.by_name(&format!("attachments/{}.bin", pending.aid))
                .is_err()
        );
        let mut manifest = String::new();
        zip.by_name("attachments.json")
            .unwrap()
            .read_to_string(&mut manifest)
            .unwrap();
        assert!(manifest.contains("ready.png") && manifest.contains("pending.png"));
        fixture._services.cleanup_ref().await.unwrap();
    }

    /// When the storage object of a single attachment is lost, the export skips it and notes it,
    /// instead of making the whole export retry forever.
    #[tokio::test]
    async fn export_skips_an_attachment_whose_object_is_missing() {
        let fixture = super::super::tests::Fixture::new(MediaConfig::default()).await;
        let s3 = super::super::tests::FakeS3::new(fixture.db.clone()).await;
        fixture.configure(&s3.endpoint).await;
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(2, 2)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let bytes = png.into_inner();
        let lost = Box::pin(fixture.upload(&identity::random_token(), "lost.png", &bytes))
            .await
            .unwrap();
        Box::pin(fixture.media.process_pending(identity::now()))
            .await
            .unwrap();
        s3.state.objects.lock().unwrap().clear();
        crate::db::query(
            "UPDATE gonof_common_member SET email='archive@example.test',`freeze`=1 WHERE uid=?",
        )
        .bind(fixture.uid)
        .execute(fixture.db.executor())
        .await
        .unwrap();
        let now = identity::now();
        privacy::request_access(&fixture.db, "archive@example.test", now)
            .await
            .unwrap();
        let payload: String = crate::db::query_scalar(
            "SELECT payload FROM gonof_common_outbox WHERE event_type='privacy_access'",
        )
        .fetch_one(fixture.db.executor())
        .await
        .unwrap();
        let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
        let (token, _) =
            privacy::exchange_access(&fixture.db, payload["token"].as_str().unwrap(), now)
                .await
                .unwrap();
        let access = Access::Token(token);
        let id = privacy::submit(
            &fixture.db,
            &access,
            NewRequest {
                kind: "export".into(),
                content_mode: "retain".into(),
                request_key: identity::random_token(),
                password: String::new(),
            },
            now,
        )
        .await
        .unwrap();
        Box::pin(privacy::process_pending(
            &fixture.db,
            &fixture._services.global,
            &fixture.media,
            &fixture.root,
            Path::new(".local"),
            90,
            now,
        ))
        .await
        .unwrap();
        let dir = privacy::directory(&fixture.root, Path::new(".local"));
        let (path, _) = privacy::download(&fixture.db, &access, id, &dir, now)
            .await
            .expect("the export completes even with an attachment object missing");
        let mut zip = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
        assert!(
            zip.by_name(&format!("attachments/{}.bin", lost.aid))
                .is_err()
        );
        let missing: serde_json::Value =
            serde_json::from_reader(zip.by_name("attachments-missing.json").unwrap()).unwrap();
        assert_eq!(missing, serde_json::json!([lost.aid]));
        fixture._services.cleanup_ref().await.unwrap();
    }
}
