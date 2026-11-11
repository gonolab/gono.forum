use super::*;
use crate::db::Tx;

impl Media {
    /// The account and both attachment rows fence replacement against cancellation and cleanup.
    pub async fn set_avatar(&self, actor: identity::Actor, aid: Option<i64>) -> Result<()> {
        if aid.is_some_and(|value| value <= 0) {
            return Err(MediaError::NotFound);
        }
        let mut tx = self.db.begin_write().await?;
        crate::runtime::guard_write(&self.db, &mut tx, false)
            .await
            .map_err(|error| MediaError::Validation(error.to_string()))?;
        self.member_actor_locked(&mut tx, actor, false).await?;
        let member = crate::db::query(
            "SELECT avatar_aid,mute_until FROM gonof_common_member WHERE uid=? FOR UPDATE",
        )
        .bind(actor.uid)
        .fetch_one(self.db.observe(&mut *tx))
        .await?;
        let previous: Option<i64> = member.try_get("avatar_aid")?;
        let mute_until: i64 = member.try_get("mute_until")?;
        // No avatar change while muted; removing an avatar works as usual, so members can take down an inappropriate
        // avatar themselves.
        if aid.is_some() && crate::moderation::muted(mute_until, crate::identity::now()) {
            return Err(MediaError::Validation(crate::moderation::muted_message(
                mute_until,
            )));
        }
        let mut ids: Vec<i64> = previous.into_iter().chain(aid).collect();
        ids.sort_unstable();
        ids.dedup();
        let mut attachments = BTreeMap::new();
        for id in ids {
            let row = crate::db::query("SELECT a.uid,d.lifecycle,d.scan_status,d.mime_type,d.purge_after,d.updated_at,d.trashed_at
                 FROM gonof_forum_attachment a JOIN gonof_forum_attachment_0 d ON d.aid=a.aid WHERE a.aid=? FOR UPDATE")
            .bind(id)
            .fetch_optional(self.db.observe(&mut *tx))
            .await?
            .ok_or(MediaError::NotFound)?;
            if row.try_get::<i64, _>("uid")? != actor.uid {
                return Err(MediaError::NotFound);
            }
            attachments.insert(id, row);
        }
        let now = identity::now();
        if let Some(id) = aid {
            let row = &attachments[&id];
            if db::text(row, "lifecycle")? != "ready"
                || db::text(row, "scan_status")? != "clean"
                || !db::text(row, "mime_type")?.starts_with("image/")
                || row
                    .try_get::<Option<i64>, _>("purge_after")?
                    .is_some_and(|expiry| expiry <= now)
            {
                return Err(MediaError::Validation(
                    crate::i18n::current().media_error_avatar().into(),
                ));
            }
            if row.try_get::<Option<i64>, _>("trashed_at")?.is_some() {
                return Err(MediaError::Validation(
                    crate::i18n::current().media_error_image_trashed().into(),
                ));
            }
        }
        if previous == aid {
            tx.commit().await?;
            return Ok(());
        }
        crate::db::query("UPDATE gonof_common_member SET avatar_aid=? WHERE uid=?")
            .bind(aid)
            .bind(actor.uid)
            .execute(self.db.observe(&mut *tx))
            .await?;
        // List avatars get a URL by uid (thread rows hold no avatar copy), so changing the avatar does not rewrite
        // thread rows.
        if let Some(id) = aid {
            crate::db::query("UPDATE gonof_forum_attachment_0 SET purge_after=NULL WHERE aid=?")
                .bind(id)
                .execute(self.db.observe(&mut *tx))
                .await?;
        }
        if let Some(id) = previous
            && self.attachment_reference_count(&mut tx, id).await? == 0
        {
            let timestamp = now.max(attachments[&id].try_get::<i64, _>("updated_at")?);
            let expiry = timestamp
                .checked_add(self.config.orphan_seconds)
                .ok_or_else(|| {
                    MediaError::Internal(anyhow::anyhow!("avatar purge time out of range"))
                })?;
            crate::db::query("UPDATE gonof_forum_attachment_0 SET purge_after=GREATEST(dateline,?),updated_at=GREATEST(dateline,?) WHERE aid=? AND lifecycle NOT IN ('purging','purged')",)
                .bind(expiry)
                .bind(timestamp)
                .bind(id)
                .execute(self.db.observe(&mut *tx))
                .await?;
        }
        self.audit(
            &mut tx,
            actor.uid,
            "identity.avatar.update",
            serde_json::json!({"previous_aid":previous,"avatar_aid":aid}),
            now,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Only the current owner-bound, checked image is public; no object-storage URL escapes.
    pub async fn read_avatar(&self, uid: i64, aid: i64) -> Result<MediaDownload> {
        let (row, _) = self.current_avatar(uid, aid).await?;
        Box::pin(self.download_row(&row)).await
    }

    /// The avatar shown on pages: scaling, process cache and `ETag` negotiation, see `display.rs`.
    /// Always checks the current binding in the database first; 404 when the binding changed or the account was
    /// deleted.
    pub async fn avatar_image(
        &self,
        uid: i64,
        aid: i64,
        if_none_match: Option<&str>,
    ) -> Result<DisplayImage> {
        let (row, upload) = self.current_avatar(uid, aid).await?;
        Box::pin(self.display_image("avatar", &row, &upload, if_none_match)).await
    }

    /// The current avatar by uid (for lists): `None` when the account has no avatar, the
    /// avatar is not processed yet or the account was deleted; the route then returns a transparent image so the
    /// text block underneath in the list shows through. The member row and the attachment row of the current avatar
    /// are fetched in one query. Frozen accounts and accounts in deletion still get their avatar: a frozen member's
    /// posts remain and the avatar goes with the posts; only deleted (`deleted`) accounts get none.
    pub async fn current_avatar_image(
        &self,
        uid: i64,
        if_none_match: Option<&str>,
    ) -> Result<Option<DisplayImage>> {
        if uid <= 0 {
            return Ok(None);
        }
        let row = crate::db::query(&format!(
            "SELECT {COLUMNS}, m.account_state FROM gonof_common_member m
             JOIN gonof_forum_attachment_0 d ON d.aid=m.avatar_aid JOIN gonof_forum_attachment a ON a.aid=d.aid
             WHERE m.uid=?"
        ))
        .bind(uid)
        .fetch_optional(self.db.executor())
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let upload = decode_upload(&row)?;
        if db::text(&row, "account_state")? == "deleted"
            || upload.uid != uid
            || !upload.is_ready_image()
        {
            return Ok(None);
        }
        Box::pin(self.display_image("avatar", &row, &upload, if_none_match))
            .await
            .map(Some)
    }

    async fn current_avatar(&self, uid: i64, aid: i64) -> Result<(DbRow, Upload)> {
        if uid <= 0 || aid <= 0 {
            return Err(MediaError::NotFound);
        }
        let row = crate::db::query(&format!(
            "SELECT {COLUMNS}, m.avatar_aid, m.account_state FROM gonof_forum_attachment a
             JOIN gonof_forum_attachment_0 d ON d.aid=a.aid JOIN gonof_common_member m ON m.uid=a.uid
             WHERE a.aid=?"
        ))
        .bind(aid)
        .fetch_optional(self.db.executor())
        .await?
        .ok_or(MediaError::NotFound)?;
        let upload = decode_upload(&row)?;
        let current = upload.uid == uid
            && row.try_get::<Option<i64>, _>("avatar_aid")? == Some(aid)
            && db::text(&row, "account_state")? != "deleted";
        if !current || !upload.is_ready_image() {
            return Err(MediaError::NotFound);
        }
        Ok((row, upload))
    }

    /// Call after locking the attachment detail row, before any snapshot reads.
    /// Bind/unbind also takes that row, so both kinds of references are stable until commit.
    pub(super) async fn attachment_reference_count(
        &self,
        tx: &mut Tx<'_>,
        aid: i64,
    ) -> Result<i64> {
        let posts: i64 = crate::db::query_scalar(
            "SELECT COUNT(*) FROM gonof_forum_attachment_reference WHERE aid=?",
        )
        .bind(aid)
        .fetch_one(self.db.observe(&mut **tx))
        .await?;
        let avatars: i64 =
            crate::db::query_scalar("SELECT COUNT(*) FROM gonof_common_member WHERE avatar_aid=?")
                .bind(aid)
                .fetch_one(self.db.observe(&mut **tx))
                .await?;
        let brands: i64 =
            crate::db::query_scalar("SELECT COUNT(*) FROM gonof_common_site_asset WHERE aid=?")
                .bind(aid)
                .fetch_one(self.db.observe(&mut **tx))
                .await?;
        let forums: i64 =
            crate::db::query_scalar("SELECT COUNT(*) FROM gonof_forum_forum WHERE icon_aid=?")
                .bind(aid)
                .fetch_one(self.db.observe(&mut **tx))
                .await?;
        // Each multi-domain site domain's own logo and favicon.
        let domains: i64 = crate::db::query_scalar(
            "SELECT COUNT(*) FROM gonof_common_site_domain WHERE logo_aid=? OR favicon_aid=?",
        )
        .bind(aid)
        .bind(aid)
        .fetch_one(self.db.observe(&mut **tx))
        .await?;
        Ok(posts
            .saturating_add(avatars)
            .saturating_add(brands)
            .saturating_add(forums)
            .saturating_add(domains))
    }
}
