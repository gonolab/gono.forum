//! Thumbnails for thread lists (the images shown for each thread are listed in `gonof_forum_thread_image`).
//!
//! - **Generation**: the checking subprocess shrinks a copy while decoding a static image ([`render`]), and the
//!   processing task stores it as a second object (key name [`object_key`], derived from the original object key, no
//!   table change); an image without a thumbnail object (for example one processed by an older version) is shrunk
//!   and stored on the first request. Cleaning up an attachment deletes both objects (`delete_object` in `worker.rs`).
//! - **Size**: height [`THUMB_HEIGHT`] (the page shows it at 90 px, enough for 2x screens), width in the original
//!   proportion and capped at [`THUMB_MAX_WIDTH`], with too-wide images cropped in the center; images that are not that
//!   tall are not enlarged. Without transparency the output is JPEG (quality 80), with transparency PNG. GIFs get no
//!   thumbnail.
//! - **Serving**: `/thumb/{tid}/{aid}` ([`Media::thread_thumbnail`]): one query confirms the image really is the
//!   thread's thumbnail and that the thread and first post are neither hidden nor withdrawn; forum readability is
//!   checked against the in-process forum cache, with the same rules as the list. Cached by aid in the process cache
//!   (registry `thumbnails`, 64 MiB); responses carry `ETag` and [`IMAGE_CACHE_CONTROL`].
use super::*;
use crate::cache::ImageCache;

/// Thumbnail height (pixels).
pub(super) const THUMB_HEIGHT: u32 = 180;
/// Maximum thumbnail width (pixels); wider ones are cropped in the center.
pub(super) const THUMB_MAX_WIDTH: u32 = 420;
/// JPEG thumbnail quality.
const THUMB_QUALITY: u8 = 80;

/// Process cache of its own (64 MiB): a thumbnail is usually a dozen to a few dozen KB.
const THUMB_CACHE_ENTRIES: usize = 8192;
const THUMB_CACHE_BYTES: usize = 64 << 20;
const THUMB_ENTRY_BYTES: usize = 512 << 10;
const VARIANT: &str = "180";

/// Process cache of thumbnails (registry `thumbnails`).
pub(super) fn cache() -> ImageCache {
    ImageCache::with_limits(THUMB_CACHE_ENTRIES, THUMB_CACHE_BYTES, THUMB_ENTRY_BYTES)
}

/// Key of the thumbnail object: the original object key plus a suffix.
pub(super) fn object_key(attachment: &str) -> String {
    format!("{attachment}.thumb")
}

/// Shrinks one image by the size rules and returns the bytes and type.
pub(super) fn render(image: &image::DynamicImage) -> anyhow::Result<(Vec<u8>, &'static str)> {
    let (width, height) = (image.width().max(1), image.height().max(1));
    let target_height = height.min(THUMB_HEIGHT);
    let proportional = (u64::from(width) * u64::from(target_height))
        .div_ceil(u64::from(height))
        .max(1);
    let resized = if proportional > u64::from(THUMB_MAX_WIDTH) {
        image.resize_to_fill(
            THUMB_MAX_WIDTH,
            target_height,
            image::imageops::FilterType::Lanczos3,
        )
    } else if target_height == height {
        image.clone()
    } else {
        image.resize_exact(
            u32::try_from(proportional)?,
            target_height,
            image::imageops::FilterType::Lanczos3,
        )
    };
    let mut output = std::io::Cursor::new(Vec::new());
    if resized.color().has_alpha() {
        resized.write_to(&mut output, image::ImageFormat::Png)?;
        Ok((output.into_inner(), "image/png"))
    } else {
        let encoder =
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, THUMB_QUALITY);
        resized.to_rgb8().write_with_encoder(encoder)?;
        Ok((output.into_inner(), "image/jpeg"))
    }
}

impl Media {
    /// List thumbnail `/thumb/{tid}/{aid}`: first decides from the current state whether it may be given to this
    /// visitor, then compares `ETag`, reads the cache or fetches the object.
    pub async fn thread_thumbnail(
        &self,
        tid: i64,
        aid: i64,
        viewer: Option<MediaViewer>,
        if_none_match: Option<&str>,
    ) -> Result<DisplayImage> {
        let row = crate::db::query(&format!(
            "SELECT {COLUMNS},t.fid,t.visibility AS thread_visibility,t.publication_state,t.readperm,t.displayorder,
             t.deleted_at AS thread_deleted,p.deleted_at AS first_deleted,p.invisible AS first_invisible,p.erased_at AS first_erased
             FROM gonof_forum_thread_image ti JOIN gonof_forum_thread t ON t.tid=ti.tid
             JOIN gonof_forum_post p ON p.first_tid=t.tid
             JOIN gonof_forum_attachment a ON a.aid=ti.aid JOIN gonof_forum_attachment_0 d ON d.aid=a.aid
             WHERE ti.tid=? AND ti.aid=?"
        ))
        .bind(tid)
        .bind(aid)
        .fetch_optional(self.db.executor())
        .await?
        .ok_or(MediaError::NotFound)?;
        let upload = decode_upload(&row)?;
        let member = viewer.is_some();
        let thread_visible = |scope: &str| scope == "public" || (scope == "members" && member);
        let listed = upload.is_ready()
            && crate::content::thread_images::thumbnail_type(&upload.mime_type)
            && db::text(&row, "publication_state")? == "published"
            && row.try_get::<i64, _>("readperm")? == 0
            && row.try_get::<i64, _>("displayorder")? >= 0
            && thread_visible(&db::text(&row, "thread_visibility")?)
            && row.try_get::<i64, _>("thread_deleted")? == 0
            && row.try_get::<i64, _>("first_deleted")? == 0
            && row.try_get::<i64, _>("first_invisible")? == 0
            && row.try_get::<i64, _>("first_erased")? == 0;
        if !listed || !self.forum_readable(row.try_get("fid")?, member).await? {
            return Err(MediaError::NotFound);
        }
        let etag = format!("\"thumb-{aid}-{THUMB_HEIGHT}\"");
        if if_none_match.is_some_and(|value| etag_matches(value, &etag)) {
            return Ok(DisplayImage {
                etag,
                mime_type: String::new(),
                bytes: None,
            });
        }
        if let Some((bytes, mime_type)) = self.thumb_cache.get("thumb", aid, VARIANT) {
            return Ok(DisplayImage {
                etag,
                mime_type,
                bytes: Some(bytes),
            });
        }
        let (bytes, mime_type) = Box::pin(self.thumbnail_object(&row)).await?;
        self.thumb_cache
            .insert("thumb", aid, VARIANT, &bytes, &mime_type);
        Ok(DisplayImage {
            etag,
            mime_type,
            bytes: Some(bytes),
        })
    }

    /// Whether the forum is readable for this kind of visitor: reads the in-process forum cache (the same one as lists
    /// and the page frame); in tests without the global cache it reads directly.
    async fn forum_readable(&self, fid: i64, member: bool) -> Result<bool> {
        let forums = match &self.global {
            Some(global) => {
                let ttl = global
                    .settings(&self.db)
                    .await
                    .map_err(|error| anyhow::anyhow!("{error}"))?
                    .settings
                    .cache_ttl();
                global.forums(&self.db, member, ttl).await?
            }
            None => Arc::new(crate::organization::readable_forums(&self.db, member).await?),
        };
        Ok(forums.iter().any(|forum| forum.fid == fid))
    }

    /// Fetches the thumbnail object; when there is none yet (an image processed by an older version), it reads the
    /// original, shrinks it and stores a copy; a failed store does not affect this response.
    async fn thumbnail_object(&self, row: &DbRow) -> Result<(bytes::Bytes, String)> {
        let (client, profile) = self
            .object_client(&db::text(row, "storage_profile_id")?)
            .await?;
        let key = object_key(&db::text(row, "attachment")?);
        match client
            .get_object()
            .bucket(&profile.bucket)
            .key(&key)
            .send()
            .await
        {
            Ok(object) => {
                let mime_type = object
                    .content_type()
                    .filter(|value| value.starts_with("image/"))
                    .unwrap_or("image/jpeg")
                    .to_owned();
                let bytes = object
                    .body
                    .collect()
                    .await
                    .map_err(|e| MediaError::Internal(e.into()))?
                    .into_bytes();
                return Ok((bytes, mime_type));
            }
            // No such object: the standard NoSuchKey, also accepting a 404 without an error body (some S3-compatible
            // implementations reply that way).
            Err(error)
                if error
                    .as_service_error()
                    .is_some_and(|error| error.is_no_such_key())
                    || error
                        .raw_response()
                        .is_some_and(|response| response.status().as_u16() == 404) => {}
            Err(_) => {
                return Err(MediaError::Unavailable(
                    crate::i18n::current().media_error_storage().into(),
                ));
            }
        }
        let original = self.download_row(row).await?;
        let source = original
            .body
            .collect()
            .await
            .map_err(|e| MediaError::Internal(e.into()))?
            .into_bytes();
        let limits = inspect::limits(&self.config);
        let (bytes, mime_type) = tokio::task::spawn_blocking(move || {
            let mut reader = image::ImageReader::new(std::io::Cursor::new(&source[..]))
                .with_guessed_format()
                .map_err(anyhow::Error::from)?;
            reader.limits(limits);
            render(&reader.decode()?)
        })
        .await
        .map_err(|e| MediaError::Internal(e.into()))??;
        let stored = client
            .put_object()
            .bucket(&profile.bucket)
            .key(&key)
            .content_type(mime_type)
            .body(aws_sdk_s3::primitives::ByteStream::from(bytes.clone()))
            .send()
            .await;
        if let Err(error) = stored {
            tracing::warn!(error = ?error, "failed to store the thumbnail; will retry next time");
        }
        Ok((bytes::Bytes::from(bytes), mime_type.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoded(bytes: &[u8]) -> image::DynamicImage {
        image::load_from_memory(bytes).unwrap()
    }

    #[test]
    fn thumbnails_keep_height_ratio_cap_width_and_never_upscale() {
        let wide = image::DynamicImage::new_rgb8(4000, 1000);
        let (bytes, mime) = render(&wide).unwrap();
        assert_eq!(mime, "image/jpeg");
        let out = decoded(&bytes);
        assert_eq!((out.width(), out.height()), (THUMB_MAX_WIDTH, THUMB_HEIGHT));
        let tall = image::DynamicImage::new_rgb8(600, 1200);
        let out = decoded(&render(&tall).unwrap().0);
        assert_eq!((out.width(), out.height()), (90, THUMB_HEIGHT));
        let small = image::DynamicImage::new_rgb8(40, 30);
        let out = decoded(&render(&small).unwrap().0);
        assert_eq!((out.width(), out.height()), (40, 30), "never upscales");
        let transparent = image::DynamicImage::new_rgba8(300, 300);
        assert_eq!(render(&transparent).unwrap().1, "image/png");
    }
}
