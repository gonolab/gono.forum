use super::*;
use crate::db::DbRow;
use crate::db::Tx;
use aws_sdk_s3::{
    Client,
    config::{BehaviorVersion, Region},
    primitives::ByteStream,
};

impl Media {
    pub(super) async fn stored_in(&self, tx: &mut Tx<'_>) -> Result<StoredSettings> {
        let row = crate::db::query("SELECT svalue FROM gonof_common_settings WHERE skey=?")
            .bind(SETTING)
            .fetch_optional(self.db.observe(&mut **tx))
            .await?;
        row.map(|row| {
            serde_json::from_str(&db::text(&row, "svalue")?)
                .context("invalid media storage settings")
                .map_err(MediaError::from)
        })
        .transpose()
        .map(|s| s.unwrap_or_default())
    }

    pub(super) async fn stored(&self) -> Result<StoredSettings> {
        let row = crate::db::query("SELECT svalue FROM gonof_common_settings WHERE skey=?")
            .bind(SETTING)
            .fetch_optional(self.db.executor())
            .await?;
        row.map(|row| {
            serde_json::from_str(&db::text(&row, "svalue")?)
                .context("invalid media storage settings")
                .map_err(MediaError::from)
        })
        .transpose()
        .map(|s| s.unwrap_or_default())
    }

    #[cfg(test)]
    pub async fn settings(&self, uid: i64) -> Result<StorageSettings> {
        self.settings_actor(self.test_actor(uid).await?).await
    }

    /// The storage settings page is read-only: checks the administrator through the connection pool without opening a
    /// write transaction.
    pub async fn settings_actor(&self, actor: identity::Actor) -> Result<StorageSettings> {
        self.member_unlocked(actor, true).await?;
        Ok(summary(&self.stored().await?))
    }

    /// The profile a save would create, its credentials, and whether those are new keys rather than the current ones.
    async fn candidate(
        &self,
        input: &StorageInput,
        current: &StoredSettings,
    ) -> Result<(Profile, Credentials, bool)> {
        let endpoint = input.endpoint.trim().trim_end_matches('/').to_owned();
        let address = url::Url::parse(&endpoint).map_err(|_| {
            MediaError::Validation(crate::i18n::current().media_manage_error_endpoint().into())
        })?;
        let local = address.host_str().is_some_and(|s| {
            s == "localhost"
                || s.parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        if address.host_str().is_none()
            || !(address.scheme() == "https" || address.scheme() == "http" && local)
            || !address.username().is_empty()
            || address.password().is_some()
            || address.query().is_some()
            || address.fragment().is_some()
            || address.path() != "/"
        {
            return Err(MediaError::Validation(
                crate::i18n::current()
                    .media_manage_error_endpoint_https()
                    .into(),
            ));
        }
        let region = input.region.trim();
        let bucket = input.bucket.trim();
        let prefix = input.prefix.trim().trim_matches('/');
        if region.is_empty()
            || region.len() > 128
            || !region
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || bucket.len() < 3
            || bucket.len() > 63
            || !bucket
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'.'))
            || prefix.len() > 512
            || prefix.split('/').any(|part| matches!(part, "." | ".."))
            || prefix.chars().any(|c| c.is_control() || c == '\\')
        {
            return Err(MediaError::Validation(
                crate::i18n::current().media_manage_error_bucket().into(),
            ));
        }
        // Keys copied from a provider's page often carry a trailing space or newline; S3 keys never contain one.
        let access_key = input.access_key.trim();
        let secret_key = input.secret_key.trim();
        let new_keys = !(access_key.is_empty() && secret_key.is_empty());
        let credentials = if !new_keys {
            let previous = current.profiles.get(&current.active).ok_or_else(|| {
                MediaError::Validation(
                    crate::i18n::current()
                        .media_manage_error_keys_first()
                        .into(),
                )
            })?;
            self.credentials(previous).await?
        } else if access_key.is_empty() || secret_key.is_empty() {
            return Err(MediaError::Validation(
                crate::i18n::current().media_manage_error_keys_both().into(),
            ));
        } else {
            if access_key.len() > 512
                || secret_key.len() > 4096
                || access_key.chars().any(char::is_control)
                || secret_key.chars().any(char::is_control)
            {
                return Err(MediaError::Validation(
                    crate::i18n::current()
                        .media_manage_error_keys_shape()
                        .into(),
                ));
            }
            Credentials {
                access_key: access_key.to_owned(),
                secret_key: secret_key.to_owned(),
            }
        };
        let id = identity::random_token();
        Ok((
            Profile {
                id: id.clone(),
                endpoint,
                region: region.into(),
                bucket: bucket.into(),
                prefix: if prefix.is_empty() {
                    String::new()
                } else {
                    format!("{prefix}/")
                },
                path_style: input.path_style,
                secret_ref: id,
            },
            credentials,
            new_keys,
        ))
    }

    #[cfg(test)]
    pub async fn save_settings(&self, uid: i64, input: StorageInput) -> Result<StorageSettings> {
        self.save_settings_actor(self.test_actor(uid).await?, input)
            .await
    }

    pub async fn save_settings_actor(
        &self,
        actor: identity::Actor,
        input: StorageInput,
    ) -> Result<StorageSettings> {
        // The credentials file is written to disk before the transaction opens (on SQLite the whole site has only one
        // writer, and write transactions do not write to disk): first check the administrator
        // and version number through the connection pool, compute the candidate from this version of the configuration
        // (reusing the old credentials requires reading the old credentials file), write the file, then open the
        // transaction to check again.
        self.member_unlocked(actor, true).await?;
        let before = self.stored().await?;
        if before.revision != input.revision {
            return Err(MediaError::Conflict(
                crate::i18n::current().media_manage_error_stale().into(),
            ));
        }
        let (profile, credentials, new_keys) = self.candidate(&input, &before).await?;
        let directory = self.directory.join("secrets");
        private_directory(&directory).await?;
        let path = directory.join(&profile.secret_ref);
        let written = async {
            let mut file = private_file(&path).await?;
            let bytes = serde_json::to_vec(&credentials)
                .context("cannot encode the storage credentials")?;
            file.write_all(&bytes).await?;
            file.sync_all().await?;
            sync_directory(&directory).await
        }
        .await;
        let saved = match written {
            Ok(()) => {
                self.save_profile(actor, input.revision, profile, new_keys)
                    .await
            }
            Err(e) => Err(e),
        };
        let (tx, current, retired) = match saved {
            Ok(saved) => saved,
            Err(e) => {
                // Not committed; these credentials are not referenced by any configuration.
                let _ = tokio::fs::remove_file(&path).await;
                return Err(e);
            }
        };
        // On an uncertain commit keep the secret file: it may already be referenced by a committed profile.
        tx.commit().await?;
        // Credentials no profile uses any more. Removing the file is also what tells every process that still holds a
        // client built from it to build a new one (`object_client`).
        for secret_ref in retired {
            self.clients.forget(&secret_ref);
            if let Err(error) = tokio::fs::remove_file(directory.join(&secret_ref)).await
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(error = %error, "could not remove replaced storage credentials");
            }
        }
        Ok(summary(&current))
    }

    /// The database-writing part of the storage settings: checks the administrator and version number under the lock
    /// (if the version number has not changed, the configuration version the candidate was based on has not changed
    /// either) and writes the new configuration.
    ///
    /// New keys for the endpoint and bucket of earlier profiles replace theirs too: attachments keep the profile they
    /// were stored with, and once the old keys are revoked they could not be read any more. The transaction is handed
    /// back to the caller to commit, with the credentials no profile references any more.
    async fn save_profile(
        &self,
        actor: identity::Actor,
        revision: i64,
        profile: Profile,
        new_keys: bool,
    ) -> Result<(Tx<'static>, StoredSettings, Vec<String>)> {
        let uid = actor.uid;
        let mut tx = self.db.begin_write().await?;
        self.member_actor_locked(&mut tx, actor, true).await?;
        // Existing bootstrap row serializes the first media setting save across different admins too.
        crate::db::query(
            "SELECT skey FROM gonof_common_settings WHERE skey='identity.bootstrap_admin' FOR UPDATE",
        )
        .fetch_one(self.db.observe(&mut *tx))
        .await?;
        let mut current = self.stored_in(&mut tx).await?;
        if current.revision != revision {
            return Err(MediaError::Conflict(
                crate::i18n::current().media_manage_error_stale().into(),
            ));
        }
        current.revision += 1;
        current.active = profile.id.clone();
        let before: std::collections::BTreeSet<String> = current
            .profiles
            .values()
            .map(|p| p.secret_ref.clone())
            .collect();
        let mut rotated = Vec::new();
        if new_keys {
            for other in current.profiles.values_mut() {
                if other.endpoint == profile.endpoint && other.bucket == profile.bucket {
                    other.secret_ref = profile.secret_ref.clone();
                    rotated.push(other.id.clone());
                }
            }
        }
        current.profiles.insert(profile.id.clone(), profile);
        let retired: Vec<String> = before
            .into_iter()
            .filter(|secret_ref| {
                !current
                    .profiles
                    .values()
                    .any(|p| &p.secret_ref == secret_ref)
            })
            .collect();
        let value =
            serde_json::to_string(&current).context("cannot encode the storage settings")?;
        if current.revision == 1 {
            crate::db::query("INSERT INTO gonof_common_settings(skey,svalue) VALUES(?,?)")
                .bind(SETTING)
                .bind(value)
                .execute(self.db.observe(&mut *tx))
                .await?;
        } else {
            crate::db::query("UPDATE gonof_common_settings SET svalue=? WHERE skey=?")
                .bind(value)
                .bind(SETTING)
                .execute(self.db.observe(&mut *tx))
                .await?;
        }
        self.audit(
            &mut tx,
            uid,
            "media.storage.save",
            serde_json::json!({"revision":current.revision,"profile_id":current.active,"rotated_profiles":rotated}),
            identity::now(),
        )
        .await?;
        Ok((tx, current, retired))
    }

    #[cfg(test)]
    pub async fn test_settings(&self, uid: i64, input: StorageInput) -> Result<()> {
        self.test_settings_actor(self.test_actor(uid).await?, input)
            .await
    }

    pub async fn test_settings_actor(
        &self,
        actor: identity::Actor,
        input: StorageInput,
    ) -> Result<()> {
        // Only sends probe requests and does not write to the database: checks the administrator through the connection
        // pool without opening a write transaction.
        self.member_unlocked(actor, true).await?;
        let current = self.stored().await?;
        let (profile, credentials, _) = self.candidate(&input, &current).await?;
        let client = client(&profile, credentials);
        let key = format!("{}checks/{}", profile.prefix, identity::random_token());
        let bytes = b"gonof-private-storage-probe";
        let result = async {
            client
                .put_object()
                .bucket(&profile.bucket)
                .key(&key)
                .body(ByteStream::from_static(bytes))
                .send()
                .await
                .map_err(|error| s3_failure("write", &error))?;
            let result = client
                .get_object()
                .bucket(&profile.bucket)
                .key(&key)
                .send()
                .await
                .map_err(|error| s3_failure("read", &error))?;
            let received = result
                .body
                .collect()
                .await
                .map_err(|_| unavailable())?
                .into_bytes();
            if received.as_ref() != bytes {
                return Err(MediaError::Unavailable(
                    crate::i18n::current().media_manage_error_mismatch().into(),
                ));
            }
            Ok(())
        }
        .await;
        let deletion = client
            .delete_object()
            .bucket(&profile.bucket)
            .key(&key)
            .send()
            .await
            .map_err(|_| unavailable());
        result?;
        deletion?;
        Ok(())
    }

    async fn credentials(&self, profile: &Profile) -> Result<Credentials> {
        // Credentials file names are generated by `random_token`; only 64 lowercase hex digits are accepted, with no
        // path separators.
        if !(profile.secret_ref.len() == 64
            && profile
                .secret_ref
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
        {
            return Err(MediaError::Unavailable(
                crate::i18n::current().media_error_credentials_ref().into(),
            ));
        }
        let path = self.directory.join("secrets").join(&profile.secret_ref);
        let metadata = tokio::fs::symlink_metadata(&path).await.map_err(|_| {
            MediaError::Unavailable(crate::i18n::current().media_error_credentials().into())
        })?;
        if !metadata.is_file() {
            return Err(MediaError::Unavailable(
                crate::i18n::current().media_error_credentials().into(),
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(MediaError::Unavailable(
                    crate::i18n::current().media_error_credentials_mode().into(),
                ));
            }
        }
        let bytes = tokio::fs::read(path).await.map_err(|_| {
            MediaError::Unavailable(crate::i18n::current().media_error_credentials().into())
        })?;
        serde_json::from_slice(&bytes).map_err(|_| {
            MediaError::Unavailable(
                crate::i18n::current()
                    .media_error_credentials_format()
                    .into(),
            )
        })
    }

    /// Client for a storage configuration ID. On first use it reads the settings and credentials file and builds the
    /// client, then reuses the same client and its connection pool.
    ///
    /// A profile only ever changes its credentials, when new keys are saved for its bucket (`save_profile`); the save
    /// then removes the old credentials file. A cached client whose file is gone is therefore built again, in this
    /// process and in every other one sharing the directory, at the cost of one file check per use.
    pub(super) async fn object_client(&self, profile_id: &str) -> Result<(Client, Profile)> {
        if let Some(cached) = self.clients.get(profile_id) {
            let file = self.directory.join("secrets").join(&cached.1.secret_ref);
            if tokio::fs::try_exists(&file).await.unwrap_or(false) {
                return Ok(cached);
            }
        }
        let mut stored = self.stored().await?;
        let profile = stored.profiles.remove(profile_id).ok_or_else(|| {
            MediaError::Unavailable(crate::i18n::current().media_error_profile().into())
        })?;
        let credentials = self.credentials(&profile).await?;
        let built = (client(&profile, credentials), profile);
        self.clients.insert(profile_id, &built);
        Ok(built)
    }

    #[cfg(test)]
    pub async fn read(
        &self,
        aid: i64,
        pid: Option<i64>,
        viewer_uid: Option<i64>,
    ) -> Result<MediaDownload> {
        let actor = if let Some(uid) = viewer_uid {
            crate::db::query_scalar::<i64>(
                "SELECT credentials_version FROM gonof_common_member WHERE uid=?",
            )
            .bind(uid)
            .fetch_optional(self.db.executor())
            .await?
            .map(|credentials_version| identity::Actor {
                uid,
                credentials_version,
                api_key: None,
            })
        } else {
            None
        };
        self.read_for_actor(aid, pid, actor).await
    }

    /// For tests: verifies the visitor against the member row in the database (the step the session layer does), then
    /// goes through the same judgment as `/media`.
    #[cfg(test)]
    pub async fn read_for_actor(
        &self,
        aid: i64,
        pid: Option<i64>,
        actor: Option<identity::Actor>,
    ) -> Result<MediaDownload> {
        let viewer = match actor {
            Some(actor) => self.verified_viewer(actor).await?,
            None => None,
        };
        let image = self.authorize_image(aid, pid, viewer).await?;
        Box::pin(self.open_image(&image)).await
    }

    /// Verifies the visitor against the member row in the database: a mismatched credentials version, freeze or an
    /// abnormal account counts as anonymous. Callers whose session layer has already done this step construct
    /// [`MediaViewer`] directly.
    pub(super) async fn verified_viewer(
        &self,
        actor: identity::Actor,
    ) -> Result<Option<MediaViewer>> {
        let member = crate::db::query(
            "SELECT credentials_version,`freeze`,adminid,emailstatus,mobile,account_state FROM gonof_common_member WHERE uid=?",
        )
        .bind(actor.uid)
        .fetch_optional(self.db.executor())
        .await?;
        let Some(member) = member else {
            return Ok(None);
        };
        if member.try_get::<i64, _>("credentials_version")? != actor.credentials_version
            || member.try_get::<i64, _>("freeze")? != 0
            || db::text(&member, "account_state")? != "active"
        {
            return Ok(None);
        }
        let adminid: i64 = member.try_get("adminid")?;
        let contributes = adminid == 1
            || crate::login::permits(
                self.db.executor(),
                actor.uid,
                member.try_get::<i64, _>("emailstatus")? == 1,
                member.try_get::<Option<String>, _>("mobile")?.is_some(),
            )
            .await?;
        Ok(Some(MediaViewer {
            actor,
            adminid,
            contributes,
        }))
    }

    /// Whether this image may be given to this visitor (shared by `/media/{aid}` and list thumbnails): only ready
    /// images. With a pid it is judged by the post; without a pid only to the uploader, and only if no post references
    /// it (private preview). Does not read the object; the caller uses the result to answer 304, headers only, or to
    /// fetch the object.
    pub async fn authorize_image(
        &self,
        aid: i64,
        pid: Option<i64>,
        viewer: Option<MediaViewer>,
    ) -> Result<AuthorizedImage> {
        let row=crate::db::query(&format!("SELECT {COLUMNS} FROM gonof_forum_attachment a JOIN gonof_forum_attachment_0 d ON d.aid=a.aid WHERE a.aid=?"))
            .bind(aid).fetch_optional(self.db.executor()).await?.ok_or(MediaError::NotFound)?;
        let upload = decode_upload(&row)?;
        // Non-image attachments are not served: any left in the database are purged by
        // `gono-forum-cli purge-non-image-attachments`, and they are not served before the purge either.
        if !upload.is_ready() || !upload.mime_type.starts_with("image/") {
            return Err(MediaError::NotFound);
        }
        if let Some(pid) = pid {
            if !self.post_attachment_allowed(aid, pid, viewer).await? {
                return Err(MediaError::NotFound);
            }
        } else {
            let Some(viewer) = viewer else {
                return Err(MediaError::NotFound);
            };
            if viewer.actor.uid != upload.uid || !viewer.contributes {
                return Err(MediaError::NotFound);
            }
            let references: i64 = crate::db::query_scalar(
                "SELECT COUNT(*) FROM gonof_forum_attachment_reference WHERE aid=?",
            )
            .bind(aid)
            .fetch_one(self.db.executor())
            .await?;
            if references != 0 {
                return Err(MediaError::NotFound);
            }
        }
        Ok(AuthorizedImage { row, upload })
    }

    /// Fetches the object of an authorized image and streams it to the response.
    pub async fn open_image(&self, image: &AuthorizedImage) -> Result<MediaDownload> {
        Box::pin(self.download_row(&image.row)).await
    }

    pub(super) async fn download_row(&self, row: &DbRow) -> Result<MediaDownload> {
        let upload = decode_upload(row)?;
        let (client, profile) = self
            .object_client(&db::text(row, "storage_profile_id")?)
            .await?;
        let object = client
            .get_object()
            .bucket(profile.bucket)
            .key(db::text(row, "attachment")?)
            .send()
            .await
            .map_err(|_| unavailable())?;
        Ok(MediaDownload {
            filename: upload.filename,
            mime_type: upload.mime_type.clone(),
            filesize: object.content_length.unwrap_or(upload.filesize),
            is_image: upload.mime_type.starts_with("image/"),
            body: object.body,
        })
    }

    /// This attachment is indeed referenced by this post, and the post is visible to this visitor (the judgment for
    /// `/media/{aid}?pid=`, one join query).
    pub(super) async fn post_attachment_allowed(
        &self,
        aid: i64,
        pid: i64,
        viewer: Option<MediaViewer>,
    ) -> Result<bool> {
        let row = crate::db::query(&format!(
            "SELECT {POST_ACCESS_COLUMNS} FROM gonof_forum_attachment_reference r JOIN gonof_forum_post p ON p.pid=r.pid {POST_ACCESS_JOINS} WHERE r.aid=? AND r.pid=?"
        ))
        .bind(aid)
        .bind(pid)
        .fetch_optional(self.db.executor())
        .await?;
        match row {
            Some(row) => self.post_row_allowed(&row, viewer).await,
            None => Ok(false),
        }
    }

    /// The first post referencing this attachment, by pid, that this visitor can read: the post "My attachments"
    /// previews the image through and links to. The lowest pid may sit in a disabled board or be hidden while another
    /// post still shows the image. Walks the references in primary key order a page at a time.
    pub(super) async fn first_readable_post(
        &self,
        aid: i64,
        viewer: Option<MediaViewer>,
    ) -> Result<Option<i64>> {
        const PAGE: i64 = 20;
        let mut after = 0_i64;
        loop {
            let rows = crate::db::query(&format!(
                "SELECT r.pid,{POST_ACCESS_COLUMNS} FROM gonof_forum_attachment_reference r JOIN gonof_forum_post p ON p.pid=r.pid {POST_ACCESS_JOINS} WHERE r.aid=? AND r.pid>? ORDER BY r.pid LIMIT ?"
            ))
            .bind(aid)
            .bind(after)
            .bind(PAGE)
            .fetch_all(self.db.executor())
            .await?;
            for row in &rows {
                after = row.try_get("pid")?;
                if self.post_row_allowed(row, viewer).await? {
                    return Ok(Some(after));
                }
            }
            if i64::try_from(rows.len()).unwrap_or(0) < PAGE {
                return Ok(None);
            }
        }
    }

    /// Whether the post itself is visible to this visitor, ignoring attachment references: the editor judges once per
    /// post for multiple attachments on the same post (`editor_uploads`).
    pub(super) async fn post_allowed(&self, pid: i64, viewer: Option<MediaViewer>) -> Result<bool> {
        let row = crate::db::query(&format!(
            "SELECT {POST_ACCESS_COLUMNS} FROM gonof_forum_post p {POST_ACCESS_JOINS} WHERE p.pid=?"
        ))
        .bind(pid)
        .fetch_optional(self.db.executor())
        .await?;
        match row {
            Some(row) => self.post_row_allowed(&row, viewer).await,
            None => Ok(false),
        }
    }

    /// Judged by the current state of the post, thread, forum, parent forum, first post and parent floor (rows taken
    /// from [`POST_ACCESS_COLUMNS`]).
    async fn post_row_allowed(&self, row: &DbRow, viewer: Option<MediaViewer>) -> Result<bool> {
        let actor = viewer.map(|viewer| viewer.actor);
        if row.try_get::<i64, _>("erased_at")? > 0 {
            return Ok(false);
        }
        let visibility = |scope: &str| scope == "public" || (scope == "members" && actor.is_some());
        let published = forum_path_readable(row, actor.is_some())?
            && row.try_get::<i64, _>("readperm")? == 0
            && visibility(&db::text(row, "thread_visibility")?)
            && db::text(row, "publication_state")? == "published";
        if !published {
            return Ok(false);
        }
        let hidden = row.try_get::<i64, _>("invisible")? != 0
            || row.try_get::<i64, _>("displayorder")? < 0
            || row.try_get::<i64, _>("first_hidden")? != 0
            || row.try_get::<Option<i64>, _>("parent_hidden")?.unwrap_or(0) != 0;
        // Withdrawal masks only the target's contents; surviving replies keep their attachments.
        let withdrawn = row.try_get::<i64, _>("deleted_at")? > 0
            || (row.try_get::<i64, _>("first")? == 1
                && (row.try_get::<i64, _>("thread_deleted")? > 0
                    || row.try_get::<i64, _>("first_deleted")? > 0));
        if !hidden && !withdrawn {
            return Ok(true);
        }
        let Some(viewer) = viewer else {
            return Ok(false);
        };
        let actor = viewer.actor;
        if self
            .manages_forum(viewer, row.try_get("fid")?, row.try_get("parent_fid")?)
            .await?
        {
            return Ok(true);
        }
        if !viewer.contributes {
            return Ok(false);
        }
        if hidden || row.try_get::<Option<i64>, _>("authorid")? != Some(actor.uid) {
            return Ok(false);
        }
        // Within the edit window the author can still see attachments they withdrew. Only on reaching here (withdrawn
        // and the author themselves) are the settings read once: the window is the admin-area setting
        // `settings.posting.edit_window_hours`, which the media service does not hold a copy of at startup.
        let settings = crate::runtime::read(&self.db)
            .await
            .map_err(|error| match error {
                identity::IdentityError::Internal(inner) => MediaError::Internal(inner),
                other => MediaError::Internal(anyhow::anyhow!(other.to_string())),
            })?;
        let now = identity::now();
        let dateline = row.try_get::<i64, _>("dateline")?;
        Ok(withdrawn
            && now >= dateline
            && now < dateline.saturating_add(crate::content::edits::edit_window(&settings)))
    }
}

/// An image attachment that passed the permission check and may be sent ([`Media::authorize_image`]).
pub struct AuthorizedImage {
    row: DbRow,
    pub upload: Upload,
}

impl AuthorizedImage {
    /// The file for an aid never changes once processed, so the entity tag looks only at the aid.
    pub fn etag(&self) -> String {
        format!("\"media-{}\"", self.upload.aid)
    }
}

/// S3 clients cached by storage configuration ID (registry `storage-clients`).
#[derive(Default)]
pub(super) struct ClientCache(std::sync::Mutex<BTreeMap<String, (Client, Profile)>>);

impl fmt::Debug for ClientCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let count = self.0.lock().map_or(0, |clients| clients.len());
        write!(f, "ClientCache({count})")
    }
}

impl ClientCache {
    fn get(&self, profile_id: &str) -> Option<(Client, Profile)> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(profile_id)
            .cloned()
    }

    fn insert(&self, profile_id: &str, built: &(Client, Profile)) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(profile_id.to_owned(), built.clone());
    }

    /// Drops the clients built from these credentials.
    fn forget(&self, secret_ref: &str) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|_, (_, profile)| profile.secret_ref != secret_ref);
    }
}

/// Columns and joins needed for post attachment permissions (shared by `post_attachment_allowed` and `post_allowed`).
const POST_ACCESS_COLUMNS: &str = "p.authorid,p.dateline,p.deleted_at,p.erased_at,p.first,p.invisible,p.fid,
     t.displayorder,t.deleted_at AS thread_deleted,t.readperm,
     t.visibility AS thread_visibility,t.publication_state,
     f.status AS forum_status,f.visibility AS forum_visibility,f.parent_fid,
     ancestor.status AS ancestor_status,ancestor.visibility AS ancestor_visibility,
     firstpost.deleted_at AS first_deleted,firstpost.invisible AS first_hidden,parent.invisible AS parent_hidden";
const POST_ACCESS_JOINS: &str = "JOIN gonof_forum_thread t ON t.tid=p.tid
     JOIN gonof_forum_forum f ON f.fid=t.fid
     LEFT JOIN gonof_forum_forum ancestor ON ancestor.fid=f.parent_fid
     JOIN gonof_forum_post firstpost ON firstpost.first_tid=t.tid
     LEFT JOIN gonof_forum_post parent ON parent.pid=p.root_pid";

fn summary(stored: &StoredSettings) -> StorageSettings {
    match stored.profiles.get(&stored.active) {
        Some(p) => StorageSettings {
            revision: stored.revision,
            configured: true,
            profile_id: p.id.clone(),
            endpoint: p.endpoint.clone(),
            region: p.region.clone(),
            bucket: p.bucket.clone(),
            prefix: p.prefix.clone(),
            path_style: p.path_style,
        },
        None => StorageSettings {
            revision: stored.revision,
            region: "us-east-1".into(),
            path_style: true,
            ..StorageSettings::default()
        },
    }
}

fn client(profile: &Profile, credentials: Credentials) -> Client {
    let configuration = aws_sdk_s3::config::Builder::new()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new(profile.region.clone()))
        .endpoint_url(&profile.endpoint)
        .force_path_style(profile.path_style)
        .credentials_provider(aws_sdk_s3::config::Credentials::new(
            credentials.access_key,
            credentials.secret_key,
            None,
            None,
            "gonof-private-file",
        ))
        .request_checksum_calculation(aws_sdk_s3::config::RequestChecksumCalculation::WhenRequired)
        .timeout_config(
            aws_sdk_s3::config::timeout::TimeoutConfig::builder()
                .operation_timeout(Duration::from_secs(60))
                .operation_attempt_timeout(Duration::from_secs(30))
                .connect_timeout(Duration::from_secs(10))
                .build(),
        )
        .build();
    Client::from_conf(configuration)
}
/// The storage test says why it failed (a generic message gives the administrator nothing to go on); the full
/// SDK error goes to the log. Credentials never appear in either: the SDK does not put them in its errors. `step` is
/// `write` or `read`.
fn s3_failure<E>(step: &'static str, error: &aws_sdk_s3::error::SdkError<E>) -> MediaError
where
    E: aws_sdk_s3::error::ProvideErrorMetadata + std::error::Error + 'static,
{
    use aws_sdk_s3::error::{ProvideErrorMetadata, SdkError};
    tracing::warn!(step, error = %aws_sdk_s3::error::DisplayErrorContext(error), "S3 storage test failed");
    let t = crate::i18n::current();
    let reason = match error {
        SdkError::ConstructionFailure(_) => t.media_manage_s3_construction().to_owned(),
        SdkError::TimeoutError(_) => t.media_manage_s3_timeout().to_owned(),
        SdkError::DispatchFailure(failure) if failure.is_timeout() => {
            t.media_manage_s3_timeout().to_owned()
        }
        SdkError::DispatchFailure(failure) if failure.is_io() => {
            t.media_manage_s3_unreachable().to_owned()
        }
        SdkError::DispatchFailure(_) => t.media_manage_s3_not_sent().to_owned(),
        SdkError::ResponseError(_) => t.media_manage_s3_unrecognized().to_owned(),
        SdkError::ServiceError(_) => match error.code() {
            Some("NoSuchBucket") => t.media_manage_s3_no_bucket().to_owned(),
            Some("AccessDenied") => t.media_manage_s3_denied().to_owned(),
            Some("InvalidAccessKeyId") => t.media_manage_s3_no_key().to_owned(),
            Some("SignatureDoesNotMatch") => t.media_manage_s3_signature().to_owned(),
            Some(code) => match error.message() {
                Some(message) => t.media_manage_s3_service_message(code, message),
                None => t.media_manage_s3_service(code),
            },
            None => t.media_manage_s3_service_error().to_owned(),
        },
        _ => t.media_manage_s3_unknown().to_owned(),
    };
    let step = if step == "write" {
        t.media_manage_s3_write()
    } else {
        t.media_manage_s3_read()
    };
    MediaError::Unavailable(t.media_manage_s3_failed(step, reason))
}

pub(super) fn unavailable() -> MediaError {
    MediaError::Unavailable(crate::i18n::current().media_error_storage().into())
}
