//! Display versions of avatars and forum icons.
//!
//! These used to be served as the original image with `no-store`: every avatar on a page cost one database query, one
//! read of the storage credentials file and one GET to S3, returning an original of up to 10 MiB. Now:
//!
//! - Static images are scaled to a [`DISPLAY_SIZE`]-square PNG (pages always use `object-fit: cover` and display at
//!   most 64 px, so this size is enough for 2x screens); GIFs may be animated and are served as the original; images
//!   that are small anyway are also served as is.
//! - Results are cached in process by aid; a hit reads no credentials and does not touch S3.
//! - Responses carry an `ETag`, with the cache header [`IMAGE_CACHE_CONTROL`] (`private, max-age=300` since
//!   2026-11-08): the browser does not ask again within 5 minutes; when it comes back to revalidate after that, the
//!   server first queries the current binding and the account and forum state, then compares the `ETag`: revalidation
//!   after an avatar change, a reset to default, account deletion or erasure (binding cleared), or the forum becoming
//!   members-only always gets 404; when still valid it only answers 304 without the image. A copy already fetched
//!   keeps showing during those 5 minutes.
use super::*;

/// Side length of the display version (pixels).
pub(super) const DISPLAY_SIZE: u32 = 128;

/// A scaled avatar is usually a few tens of KB; an original GIF may be larger, and anything over the per-entry limit is
/// not cached.
const DISPLAY_CACHE_ENTRIES: usize = 2048;
const DISPLAY_CACHE_BYTES: usize = 32 << 20;
const DISPLAY_ENTRY_BYTES: usize = 1 << 20;

/// In-process cache of display versions (registry entry `display-images`).
pub(super) fn cache() -> crate::cache::ImageCache {
    crate::cache::ImageCache::with_limits(
        DISPLAY_CACHE_ENTRIES,
        DISPLAY_CACHE_BYTES,
        DISPLAY_ENTRY_BYTES,
    )
}

/// An image for display. `bytes` being `None` means the browser's copy is still valid; answer 304.
#[derive(Debug)]
pub struct DisplayImage {
    pub etag: String,
    pub mime_type: String,
    pub bytes: Option<bytes::Bytes>,
}

impl Media {
    /// The caller has confirmed by the current binding and visibility that the image `row` may be shown.
    pub(super) async fn display_image(
        &self,
        kind: &str,
        row: &DbRow,
        upload: &Upload,
        if_none_match: Option<&str>,
    ) -> Result<DisplayImage> {
        // The file for an aid never changes once processed; a new image always gets a new aid.
        let etag = format!("\"{kind}-{}-{DISPLAY_SIZE}\"", upload.aid);
        if if_none_match.is_some_and(|value| etag_matches(value, &etag)) {
            return Ok(DisplayImage {
                etag,
                mime_type: upload.mime_type.clone(),
                bytes: None,
            });
        }
        let variant = DISPLAY_SIZE.to_string();
        if let Some((bytes, mime_type)) = self.display_cache.get(kind, upload.aid, &variant) {
            return Ok(DisplayImage {
                etag,
                mime_type,
                bytes: Some(bytes),
            });
        }
        let file = self.download_row(row).await?;
        let bytes = file
            .body
            .collect()
            .await
            // The object stream broke off: storage is unavailable, as when the request itself fails.
            .map_err(|_| super::storage::unavailable())?
            .into_bytes();
        let small = |side: Option<i64>| side.is_some_and(|v| v <= i64::from(DISPLAY_SIZE));
        let (bytes, mime_type) =
            if file.mime_type == "image/gif" || (small(upload.width) && small(upload.height)) {
                (bytes, file.mime_type)
            } else {
                let limits = inspect::limits(&self.config);
                tokio::task::spawn_blocking(move || {
                    branding::resize_brand(&bytes, DISPLAY_SIZE, DISPLAY_SIZE, limits)
                })
                .await
                .map_err(|e| MediaError::Internal(e.into()))??
            };
        self.display_cache
            .insert(kind, upload.aid, &variant, &bytes, &mime_type);
        Ok(DisplayImage {
            etag,
            mime_type,
            bytes: Some(bytes),
        })
    }
}

#[cfg(test)]
mod display_tests {
    use crate::media::etag_matches as matches;

    #[test]
    fn entity_tags_match_lists_weak_forms_and_wildcards() {
        let tag = "\"avatar-7-128\"";
        assert!(matches(tag, tag));
        assert!(matches("\"other\", \"avatar-7-128\"", tag));
        assert!(matches("W/\"avatar-7-128\"", tag));
        assert!(matches("*", tag));
        assert!(!matches("\"avatar-8-128\"", tag));
        assert!(!matches("", tag));
    }
}
