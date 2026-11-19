use super::*;
/// The name of a brand image purpose in the language passed in.
type SlotName = fn(crate::i18n::Lang) -> &'static str;

/// Brand image purposes and their names (by language).
pub const BRAND_SLOTS: &[(&str, SlotName)] = &[
    ("logo", |_| "Logo"),
    ("favicon", crate::i18n::Lang::media_manage_slot_favicon),
    ("app_icon", crate::i18n::Lang::media_manage_slot_app_icon),
    ("share", crate::i18n::Lang::media_manage_slot_share),
    ("email", crate::i18n::Lang::media_manage_slot_email),
];
#[derive(Clone, Debug)]
pub struct BrandAsset {
    pub slot: String,
    pub label: String,
    pub aid: Option<i64>,
    pub version: i64,
    /// Width and height of the original image (raw columns of the attachment table, may be empty). The share image
    /// uses them to output `og:image:width/height`; nothing is output when empty.
    pub width: Option<i64>,
    pub height: Option<i64>,
    /// Multi-domain sites: which domain this image is the own Logo or favicon of;
    /// `None` for site-wide brand images.
    pub domain: Option<i64>,
}
impl BrandAsset {
    pub fn source(&self) -> String {
        self.path("source")
    }

    /// Image URL (root-relative, empty string when there is no image): site-wide brand images are
    /// `/brand/{slot}/{aid}/{variant}`, a domain's own are `/brand/domain/{domain id}/{slot}/{aid}/{variant}`.
    pub fn path(&self, variant: &str) -> String {
        match (self.aid, self.domain) {
            (Some(aid), None) => format!("/brand/{}/{aid}/{variant}", self.slot),
            (Some(aid), Some(domain)) => {
                format!("/brand/domain/{domain}/{}/{aid}/{variant}", self.slot)
            }
            (None, _) => String::new(),
        }
    }
}
impl Media {
    pub async fn brand_assets(&self) -> Result<Vec<BrandAsset>> {
        // Width and height come from the attachment table by primary key `aid` in the same query, with no extra
        // lookup.
        let rows = db::query(
            "SELECT s.slot,s.aid,s.version,d.width,d.height FROM gonof_common_site_asset s LEFT JOIN gonof_forum_attachment_0 d ON d.aid=s.aid ORDER BY s.slot LIMIT 5",
        )
        .fetch_all(self.db.executor())
        .await?;
        let mut result = Vec::new();
        for (slot, label) in BRAND_SLOTS {
            let row = rows
                .iter()
                .find(|r| db::text(r, "slot").ok().as_deref() == Some(*slot));
            result.push(BrandAsset {
                slot: slot.to_string(),
                label: label(crate::i18n::current()).to_owned(),
                aid: row.map(|r| r.try_get("aid")).transpose()?.flatten(),
                version: row.map(|r| r.try_get("version")).transpose()?.unwrap_or(0),
                width: row.map(|r| r.try_get("width")).transpose()?.flatten(),
                height: row.map(|r| r.try_get("height")).transpose()?.flatten(),
                domain: None,
            });
        }
        Ok(result)
    }
    /// Replaces a brand image. After commit it clears this process's brand image block and broadcasts (the URLs
    /// referenced by pages come from the process cache; the image bytes are still looked up by the current binding
    /// every time, so the old aid returns 404 right after a replacement).
    pub async fn set_brand(
        &self,
        global: &crate::cache::GlobalCache,
        actor: identity::Actor,
        slot: &str,
        aid: Option<i64>,
        version: i64,
    ) -> Result<()> {
        if !BRAND_SLOTS.iter().any(|(s, _)| *s == slot)
            || version < 0
            || aid.is_some_and(|v| v <= 0)
        {
            return Err(MediaError::Validation(
                crate::i18n::current().media_manage_error_slot().into(),
            ));
        }
        let mut tx = self.db.begin_write().await?;
        // This used to borrow the site settings row as a mutex, although this function never writes that table.
        // The real mutual exclusion is the FOR UPDATE on the asset row plus the version check below; the borrowed
        // lock only created an artificial "brand save -> site settings" dependency edge.
        crate::runtime::admin_locked(&self.db, &mut tx, actor)
            .await
            .map_err(|_| MediaError::Forbidden)?;
        let row =
            db::query("SELECT aid,version FROM gonof_common_site_asset WHERE slot=? FOR UPDATE")
                .bind(slot)
                .fetch_optional(self.db.observe(&mut *tx))
                .await?;
        let old: Option<i64> = row
            .as_ref()
            .map(|r| r.try_get("aid"))
            .transpose()?
            .flatten();
        let current: i64 = row
            .as_ref()
            .map(|r| r.try_get("version"))
            .transpose()?
            .unwrap_or(0);
        if current != version {
            return Err(MediaError::Conflict(
                crate::i18n::current()
                    .media_manage_error_brand_stale()
                    .into(),
            ));
        }
        let now = identity::now();
        for id in std::collections::BTreeSet::from_iter(old.into_iter().chain(aid)) {
            // A nonexistent image id is a form problem, not an internal error.
            let row=db::query("SELECT a.uid,d.lifecycle,d.scan_status,d.mime_type,d.width,d.height,d.purge_after,d.trashed_at FROM gonof_forum_attachment a JOIN gonof_forum_attachment_0 d ON d.aid=a.aid WHERE a.aid=? FOR UPDATE")
                .bind(id).fetch_optional(self.db.observe(&mut *tx)).await?
                 .ok_or_else(|| MediaError::Validation(crate::i18n::current().media_manage_error_choose_checked().into()))?;
            if Some(id) == aid {
                if (Some(id) != old && row.try_get::<i64, _>("uid")? != actor.uid)
                    || db::text(&row, "lifecycle")? != "ready"
                    || db::text(&row, "scan_status")? != "clean"
                    || !db::text(&row, "mime_type")?.starts_with("image/")
                    || row
                        .try_get::<Option<i64>, _>("purge_after")?
                        .is_some_and(|t| t <= now)
                {
                    return Err(MediaError::Validation(
                        crate::i18n::current()
                            .media_manage_error_choose_checked()
                            .into(),
                    ));
                }
                if row.try_get::<Option<i64>, _>("trashed_at")?.is_some() {
                    return Err(MediaError::Validation(
                        crate::i18n::current().media_manage_error_trashed().into(),
                    ));
                }
                let width = row.try_get::<Option<i64>, _>("width")?.unwrap_or(0);
                let height = row.try_get::<Option<i64>, _>("height")?.unwrap_or(0);
                if matches!(slot, "favicon" | "app_icon")
                    && (width != height || width < if slot == "app_icon" { 512 } else { 32 })
                {
                    return Err(MediaError::Validation(
                        crate::i18n::current().media_manage_error_icon_size().into(),
                    ));
                }
            }
        }
        db::query("INSERT INTO gonof_common_site_asset(slot,aid,version,dateline) VALUES(?,?,?,?) ON DUPLICATE KEY UPDATE aid=VALUES(aid),version=VALUES(version),dateline=VALUES(dateline)")
            .bind(slot).bind(aid).bind(current+1).bind(now).execute(self.db.observe(&mut *tx)).await?;
        if let Some(id) = aid {
            db::query("UPDATE gonof_forum_attachment_0 SET purge_after=NULL WHERE aid=?")
                .bind(id)
                .execute(self.db.observe(&mut *tx))
                .await?;
        }
        if let Some(id) = old
            && self.attachment_reference_count(&mut tx, id).await? == 0
        {
            db::query("UPDATE gonof_forum_attachment_0 SET purge_after=GREATEST(dateline,?),updated_at=GREATEST(dateline,?) WHERE aid=? AND lifecycle NOT IN ('purging','purged')")
                .bind(now+self.config.orphan_seconds).bind(now).bind(id).execute(self.db.observe(&mut *tx)).await?;
        }
        self.audit(
            &mut tx,
            actor.uid,
            "site.branding.save",
            serde_json::json!({"slot":slot,"aid":aid,"version":current+1}),
            now,
        )
        .await?;
        tx.commit().await?;
        global
            .announce(&[crate::cache::Block::Branding])
            .await
            .map_err(MediaError::Internal)
    }
    /// Reads a brand image. Each time it first looks up the current binding in the database: after unbinding,
    /// replacing or clearing, the old aid returns 404 even if it is still cached. On a cache hit the original is
    /// neither downloaded nor decoded and resized again.
    pub async fn read_brand(
        &self,
        slot: &str,
        aid: i64,
        variant: &str,
    ) -> Result<(bytes::Bytes, String)> {
        if !BRAND_SLOTS.iter().any(|(s, _)| *s == slot) {
            return Err(MediaError::NotFound);
        }
        // Fetch the currently bound attachment by slot primary key; whether it is the same image and whether it
        // can be shown is decided in code.
        let row = db::query(&format!(
            "SELECT {COLUMNS} FROM gonof_common_site_asset s JOIN gonof_forum_attachment a ON a.aid=s.aid
             JOIN gonof_forum_attachment_0 d ON d.aid=a.aid WHERE s.slot=?"
        ))
        .bind(slot)
        .fetch_optional(self.db.executor())
        .await?
        .ok_or(MediaError::NotFound)?;
        self.render_brand(row, slot, aid, variant).await
    }

    /// Reads a domain's own Logo or favicon for multi-domain sites. Like
    /// [`Media::read_brand`]: each time it first looks up the current binding in the domain table, so after an image
    /// replacement or domain deletion the old URL returns 404 immediately.
    pub async fn read_domain_brand(
        &self,
        domain_id: i64,
        slot: &str,
        aid: i64,
        variant: &str,
    ) -> Result<(bytes::Bytes, String)> {
        let column = match slot {
            "logo" => "logo_aid",
            "favicon" => "favicon_aid",
            _ => return Err(MediaError::NotFound),
        };
        let row = db::query(&format!(
            "SELECT {COLUMNS} FROM gonof_common_site_domain s JOIN gonof_forum_attachment a ON a.aid=s.{column}
             JOIN gonof_forum_attachment_0 d ON d.aid=a.aid WHERE s.domain_id=?"
        ))
        .bind(domain_id)
        .fetch_optional(self.db.executor())
        .await?
        .ok_or(MediaError::NotFound)?;
        self.render_brand(row, slot, aid, variant).await
    }

    /// After the currently bound attachment row is fetched: checks that it is the same, displayable image, then
    /// resizes it to the variant (cached).
    async fn render_brand(
        &self,
        row: crate::db::DbRow,
        slot: &str,
        aid: i64,
        variant: &str,
    ) -> Result<(bytes::Bytes, String)> {
        let dimensions = brand_dimensions(slot, variant)?;
        let upload = decode_upload(&row)?;
        if upload.aid != aid || !upload.is_ready_image() {
            return Err(MediaError::NotFound);
        }
        if let Some(hit) = self.brand_cache.get(slot, aid, variant) {
            return Ok(hit);
        }
        let file = self.download_row(&row).await?;
        let bytes = file
            .body
            .collect()
            .await
            // The object stream broke off: storage is unavailable, as when the request itself fails.
            .map_err(|_| super::storage::unavailable())?
            .into_bytes();
        let rendered = match dimensions {
            None => (bytes, file.mime_type),
            Some((width, height)) => {
                let limits = inspect::limits(&self.config);
                tokio::task::spawn_blocking(move || resize_brand(&bytes, width, height, limits))
                    .await
                    .map_err(|e| MediaError::Internal(e.into()))??
            }
        };
        self.brand_cache
            .insert(slot, aid, variant, &rendered.0, &rendered.1);
        Ok(rendered)
    }
    pub async fn register_brand_upload(
        &self,
        actor: identity::Actor,
        input: NewUpload,
    ) -> Result<Upload> {
        let path = input.path.clone();
        let result = if !inspect::allowed_extension(&inspect::extension(&input.filename)) {
            Err(MediaError::Validation(
                crate::i18n::current()
                    .media_manage_error_image_only()
                    .into(),
            ))
        } else {
            self.register(actor, input, true).await
        };
        let _ = tokio::fs::remove_file(path).await;
        result
    }
}

/// Allowed outputs per brand image slot: `None` outputs the original as is, `Some` resizes it to a PNG of that size.
fn brand_dimensions(slot: &str, variant: &str) -> Result<Option<(u32, u32)>> {
    Ok(match (slot, variant) {
        ("share", "source") => Some((1200, 630)),
        (_, "source") => None,
        ("favicon", "32") => Some((32, 32)),
        ("app_icon", "180") => Some((180, 180)),
        ("app_icon", "192") => Some((192, 192)),
        ("app_icon", "512") => Some((512, 512)),
        _ => return Err(MediaError::NotFound),
    })
}

/// Decodes with the same pixel and memory limits as the upload check, then crops and resizes to a PNG.
pub(super) fn resize_brand(
    bytes: &[u8],
    width: u32,
    height: u32,
    limits: image::Limits,
) -> Result<(bytes::Bytes, String)> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| MediaError::Internal(e.into()))?;
    reader.limits(limits);
    let picture = reader
        .decode()
        .map_err(|e| MediaError::Internal(e.into()))?;
    let resized = picture.resize_to_fill(width, height, image::imageops::FilterType::Lanczos3);
    let mut output = std::io::Cursor::new(Vec::new());
    resized
        .write_to(&mut output, image::ImageFormat::Png)
        .map_err(|e| MediaError::Internal(e.into()))?;
    Ok((bytes::Bytes::from(output.into_inner()), "image/png".into()))
}

/// Process cache for brand image bytes ([`crate::cache::ImageCache`], registry entry `brand-images`).
pub(super) fn image_cache() -> crate::cache::ImageCache {
    crate::cache::ImageCache::with_limits(BRAND_CACHE_ENTRIES, BRAND_CACHE_BYTES, BRAND_ENTRY_BYTES)
}

const BRAND_CACHE_ENTRIES: usize = 32;
const BRAND_CACHE_BYTES: usize = 8 << 20;
const BRAND_ENTRY_BYTES: usize = 2 << 20;

#[cfg(test)]
mod brand_tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut output = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(width, height)
            .write_to(&mut output, image::ImageFormat::Png)
            .unwrap();
        output.into_inner()
    }

    #[test]
    fn brand_variants_are_resized_within_upload_limits() {
        assert_eq!(brand_dimensions("logo", "source").unwrap(), None);
        assert_eq!(
            brand_dimensions("share", "source").unwrap(),
            Some((1200, 630))
        );
        assert_eq!(
            brand_dimensions("app_icon", "512").unwrap(),
            Some((512, 512))
        );
        assert!(brand_dimensions("logo", "32").is_err());
        let (bytes, mime) = resize_brand(&png(64, 64), 32, 32, image::Limits::default()).unwrap();
        assert_eq!(mime, "image/png");
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (32, 32));
        let mut tight = image::Limits::default();
        tight.max_image_width = Some(16);
        assert!(resize_brand(&png(64, 64), 32, 32, tight).is_err());
    }
}
