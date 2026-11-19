use super::*;
#[path = "avatar_tests.rs"]
mod avatar_tests;
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

pub(super) struct Fixture {
    pub(super) _services: crate::test_support::TestFixture,
    pub(super) root: PathBuf,
    pub(super) db: Database,
    pub(super) media: Media,
    pub(super) uid: i64,
}
impl Fixture {
    pub(super) async fn new(config: MediaConfig) -> Self {
        Self::with_services(config).await
    }

    async fn with_services(config: MediaConfig) -> Self {
        let services = crate::test_support::TestFixture::migrated().await.unwrap();
        let root = services.root.clone();
        let db = services.db.clone();
        crate::db::query("INSERT INTO gonof_common_member(username,password,adminid,regdate) VALUES('mediaadmin','unusable-media-contract-password',1,1)").execute(db.executor()).await.unwrap();
        let uid = crate::db::query_scalar(
            "SELECT uid FROM gonof_common_member WHERE username='mediaadmin'",
        )
        .fetch_one(db.executor())
        .await
        .unwrap();
        let media = Media::new(db.clone(), config, root.clone());
        Self {
            _services: services,
            root,
            db,
            media,
            uid,
        }
    }
    pub(super) async fn configure(&self, endpoint: &str) -> StorageSettings {
        self.media
            .save_settings(self.uid, storage_input(endpoint, 0))
            .await
            .unwrap()
    }
    pub(super) async fn upload(&self, key: &str, name: &str, bytes: &[u8]) -> Result<Upload> {
        self.upload_as(self.media.test_actor(self.uid).await?, key, name, bytes)
            .await
    }
    pub(super) async fn upload_as(
        &self,
        actor: identity::Actor,
        key: &str,
        name: &str,
        bytes: &[u8],
    ) -> Result<Upload> {
        let (path, mut file) = self.media.staging_file().await?;
        file.write_all(bytes).await?;
        file.sync_all().await?;
        drop(file);
        self.media
            .register_upload_actor(
                actor,
                NewUpload {
                    request_key: key.into(),
                    filename: name.into(),
                    description: String::new(),
                    alt_text: "Test image".into(),
                    path,
                },
            )
            .await
    }
    async fn scalar(&self, sql: &str) -> i64 {
        crate::db::query_scalar(sql)
            .fetch_one(self.db.executor())
            .await
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn storage_input(endpoint: &str, revision: i64) -> StorageInput {
    StorageInput {
        revision,
        endpoint: endpoint.into(),
        region: "us-east-1".into(),
        bucket: "test-media".into(),
        prefix: "media".into(),
        path_style: true,
        access_key: "contract-access-key".into(),
        secret_key: "contract-secret-key".into(),
    }
}
pub(super) fn png() -> Vec<u8> {
    png_sized(2, 2)
}
/// A PNG of the given width and height: two images with different content need different sizes.
pub(super) fn png_sized(width: u32, height: u32) -> Vec<u8> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(width, height)
        .write_to(&mut cursor, image::ImageFormat::Png)
        .unwrap();
    cursor.into_inner()
}

#[derive(Clone)]
pub(super) struct FakeState {
    pub(super) objects: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    /// Number of DELETE requests received.
    pub(super) deletes: Arc<std::sync::atomic::AtomicUsize>,
    fail_db: Arc<AtomicBool>,
    /// Once set, every PUT returns 403: simulates object storage being temporarily unavailable (without triggering
    /// SDK retries).
    pub(super) fail_put: Arc<AtomicBool>,
    lifecycle_after_put: Arc<Mutex<Option<(i64, &'static str)>>>,
    /// An Access Key the service no longer accepts: every request signed with it gets 403.
    pub(super) revoked: Arc<Mutex<Option<String>>>,
    db: Database,
}
pub(super) struct FakeS3 {
    pub(super) endpoint: String,
    pub(super) state: FakeState,
    task: tokio::task::JoinHandle<()>,
}
impl FakeS3 {
    pub(super) async fn new(db: Database) -> Self {
        let state = FakeState {
            objects: Default::default(),
            deletes: Default::default(),
            fail_db: Default::default(),
            fail_put: Default::default(),
            lifecycle_after_put: Default::default(),
            revoked: Default::default(),
            db,
        };
        let router = Router::new()
            .fallback(fake_object)
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            endpoint,
            state,
            task,
        }
    }
}
impl Drop for FakeS3 {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn fake_object(State(state): State<FakeState>, request: Request) -> Response {
    let path = request.uri().path().to_owned();
    let revoked = state.revoked.lock().unwrap().clone();
    if let Some(key) = revoked {
        let signed_with = request
            .headers()
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.contains(&format!("Credential={key}/")));
        if signed_with {
            return (
                StatusCode::FORBIDDEN,
                "<Error><Code>InvalidAccessKeyId</Code><Message>revoked</Message></Error>",
            )
                .into_response();
        }
    }
    match *request.method() {
        Method::PUT if state.fail_put.load(Ordering::SeqCst) => {
            StatusCode::FORBIDDEN.into_response()
        }
        Method::PUT => {
            let bytes = to_bytes(request.into_body(), 32 * 1024 * 1024)
                .await
                .unwrap();
            state.objects.lock().unwrap().insert(path, bytes.to_vec());
            if state.fail_db.swap(false, Ordering::SeqCst) {
                crate::test_support::fail_when(
                    &state.db,
                    "fail_media_ready",
                    "BEFORE UPDATE",
                    "gonof_forum_attachment_0",
                    "NEW.lifecycle='ready'",
                    "injected after PUT",
                )
                .await
                .unwrap();
            }
            let transition = state.lifecycle_after_put.lock().unwrap().take();
            if let Some((aid, lifecycle)) = transition {
                crate::db::query("UPDATE gonof_forum_attachment_0 SET lifecycle=?,scan_status='clean',mime_type='image/png' WHERE aid=?")
                    .bind(lifecycle).bind(aid).execute(state.db.executor()).await.unwrap();
            }
            (StatusCode::OK, [("etag", "\"contract-etag\"")]).into_response()
        }
        Method::GET => {
            let bytes = state.objects.lock().unwrap().get(&path).cloned();
            match bytes {
                Some(bytes) => (
                    [("content-type", "application/octet-stream")],
                    Body::from(bytes),
                )
                    .into_response(),
                None => StatusCode::NOT_FOUND.into_response(),
            }
        }
        Method::DELETE => {
            state.deletes.fetch_add(1, Ordering::SeqCst);
            state.objects.lock().unwrap().remove(&path);
            StatusCode::NO_CONTENT.into_response()
        }
        Method::HEAD => if state.objects.lock().unwrap().contains_key(&path) {
            StatusCode::OK
        } else {
            StatusCode::NOT_FOUND
        }
        .into_response(),
        _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
    }
}

/// The storage test page reports why S3 refused, trims pasted credentials, and an IP endpoint works with or without
/// path style (the SDK switches to path style for IP hosts itself).
#[tokio::test]
async fn storage_tests_say_why_they_failed() {
    let fixture = Fixture::new(MediaConfig::default()).await;
    let server = FakeS3::new(fixture.db.clone()).await;
    let closed = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    let error = fixture
        .media
        .test_settings(fixture.uid, storage_input(&closed, 0))
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("S3 write failed: the endpoint cannot be reached"),
        "{error}"
    );
    server.state.fail_put.store(true, Ordering::SeqCst);
    let error = fixture
        .media
        .test_settings(fixture.uid, storage_input(&server.endpoint, 0))
        .await
        .unwrap_err();
    assert!(
        error.to_string().starts_with("S3 write failed: "),
        "{error}"
    );
    server.state.fail_put.store(false, Ordering::SeqCst);
    let mut input = storage_input(&server.endpoint, 0);
    input.access_key = format!(" {} \n", input.access_key);
    input.secret_key = format!("{}\n", input.secret_key);
    input.path_style = false;
    fixture
        .media
        .test_settings(fixture.uid, input)
        .await
        .unwrap();
    fixture._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn brand_reads_are_cached_but_still_checked_against_the_current_binding() {
    let fixture = Fixture::new(MediaConfig::default()).await;
    let server = FakeS3::new(fixture.db.clone()).await;
    fixture.configure(&server.endpoint).await;
    let square = |size: u32| {
        let mut cursor = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(size, size)
            .write_to(&mut cursor, image::ImageFormat::Png)
            .unwrap();
        cursor.into_inner()
    };
    let first = fixture
        .upload(&identity::random_token(), "icon.png", &square(64))
        .await
        .unwrap();
    let second = fixture
        .upload(&identity::random_token(), "icon-2.png", &square(96))
        .await
        .unwrap();
    fixture
        .media
        .process_pending(identity::now())
        .await
        .unwrap();
    let actor = fixture.media.test_actor(fixture.uid).await.unwrap();
    fixture
        .media
        .set_brand(
            &fixture._services.global,
            actor,
            "favicon",
            Some(first.aid),
            0,
        )
        .await
        .unwrap();
    let (bytes, mime) = fixture
        .media
        .read_brand("favicon", first.aid, "32")
        .await
        .unwrap();
    assert_eq!(mime, "image/png");
    let decoded = image::load_from_memory(&bytes).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (32, 32));
    // Still readable after the object is deleted from storage: the second read neither downloaded the original again
    // nor resized it again.
    server.state.objects.lock().unwrap().clear();
    let (again, _) = fixture
        .media
        .read_brand("favicon", first.aid, "32")
        .await
        .unwrap();
    assert_eq!(again, bytes);
    // After rebinding, the old aid is still in the cache, but the database check no longer passes.
    fixture
        .media
        .set_brand(
            &fixture._services.global,
            actor,
            "favicon",
            Some(second.aid),
            1,
        )
        .await
        .unwrap();
    assert!(matches!(
        fixture.media.read_brand("favicon", first.aid, "32").await,
        Err(MediaError::NotFound)
    ));
    assert!(matches!(
        fixture.media.read_brand("favicon", second.aid, "512").await,
        Err(MediaError::NotFound)
    ));
    fixture._services.cleanup_ref().await.unwrap();
}

/// The cleanup candidate query uses the `(lifecycle, purge_after)` index to fetch due rows by range instead of
/// scanning the whole table by primary key; tombstones that keep `purge_after` do not fall into the range either.
#[tokio::test]
async fn cleanup_candidates_use_the_purge_index() {
    // Only verifies the MySQL execution plan.
    crate::mysql_only!();
    use crate::debug::trace::{self, QuerySource, RequestProfile};
    let fixture = Fixture::new(MediaConfig::default()).await;
    let now = identity::now();
    let digits = "(SELECT 0 n UNION ALL SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3 UNION ALL SELECT 4 UNION ALL SELECT 5 UNION ALL SELECT 6 UNION ALL SELECT 7 UNION ALL SELECT 8 UNION ALL SELECT 9)";
    crate::db::query(&format!("INSERT INTO gonof_forum_attachment(uid,request_key,request_hash) SELECT ?,SHA2(CONCAT('purge-index-',a.n+10*b.n+100*c.n+1000*d.n),256),SHA2('purge-index',256) FROM {digits} a,{digits} b,{digits} c,{digits} d"))
        .bind(fixture.uid)
        .execute(fixture.db.executor())
        .await
        .unwrap();
    // 4000 rows: most are bound (purge_after is NULL), one tenth are purged tombstones, and only a scattered few are due
    // for cleanup.
    crate::db::query("INSERT INTO gonof_forum_attachment_0(aid,filename,filesize,mime_type,content_sha256,attachment,storage_profile_id,description,alt_text,dateline,updated_at,purge_after,lifecycle,scan_status,scan_detail)
        SELECT aid,'f.png',1,'image/png',SHA2(aid,256),'k','p','','',?,?,
          CASE WHEN aid%10=0 THEN ? WHEN aid%1000=1 THEN ? ELSE NULL END,
          CASE WHEN aid%10=0 THEN 'purged' ELSE 'ready' END,'clean',''
        FROM gonof_forum_attachment WHERE uid=?")
        .bind(now - 30 * 86400)
        .bind(now - 30 * 86400)
        .bind(now - 20 * 86400)
        .bind(now - 60)
        .bind(fixture.uid)
        .execute(fixture.db.executor())
        .await
        .unwrap();
    crate::db::raw_sql("ANALYZE TABLE gonof_forum_attachment_0")
        .execute(&fixture.db.pool)
        .await
        .unwrap();
    let due = fixture
        .scalar(&format!("SELECT COUNT(*) FROM gonof_forum_attachment_0 WHERE lifecycle='ready' AND purge_after<={now}"))
        .await;
    assert!(due > 0 && due < 100);
    let profile = Arc::new(RequestProfile::new());
    trace::scope(profile.clone(), Box::pin(fixture.media.cleanup(now)))
        .await
        .unwrap();
    let candidates: Vec<_> = profile
        .snapshot()
        .into_iter()
        .filter(|r| {
            r.source == QuerySource::Application
                && r.sql
                    .starts_with("SELECT aid FROM gonof_forum_attachment_0 WHERE")
                && r.sql.contains("ORDER BY aid LIMIT 100")
        })
        .collect();
    assert_eq!(candidates.len(), 1);
    assert_eq!(
        candidates[0].rows_returned,
        u64::try_from(due).unwrap(),
        "every due row is a candidate and tombstones are not"
    );
    let plan = sqlx::query(&format!("EXPLAIN {}", candidates[0].expanded_sql))
        .persistent(false)
        .fetch_all(
            fixture
                .db
                .pool
                .as_mysql()
                .expect("EXPLAIN is only checked on MySQL"),
        )
        .await
        .unwrap();
    let column = |name: &str| {
        use sqlx::{Column, Row};
        let ordinal = plan[0]
            .columns()
            .iter()
            .position(|c| c.name() == name)
            .unwrap();
        plan[0]
            .try_get::<Option<Vec<u8>>, _>(ordinal)
            .unwrap()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default()
    };
    assert_eq!(column("key"), "gonof_attachment_purge_due");
    assert_eq!(column("type"), "range");
    fixture._services.cleanup_ref().await.unwrap();
}

/// Avatar and forum icon resizing, process cache and ETag negotiation; revalidating after invalidation
/// always gets 404.
#[tokio::test]
async fn avatar_and_forum_icon_display_is_resized_cached_and_revalidated() {
    let fixture = Fixture::new(MediaConfig::default()).await;
    let server = FakeS3::new(fixture.db.clone()).await;
    fixture.configure(&server.endpoint).await;
    let picture = |width: u32, height: u32| {
        let mut cursor = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(width, height)
            .write_to(&mut cursor, image::ImageFormat::Png)
            .unwrap();
        cursor.into_inner()
    };
    let small = fixture
        .upload(&identity::random_token(), "small.png", &picture(64, 64))
        .await
        .unwrap();
    let large = fixture
        .upload(&identity::random_token(), "large.png", &picture(600, 400))
        .await
        .unwrap();
    let icon = fixture
        .upload(&identity::random_token(), "icon.png", &picture(512, 512))
        .await
        .unwrap();
    Box::pin(fixture.media.process_pending(identity::now()))
        .await
        .unwrap();
    let actor = fixture.media.test_actor(fixture.uid).await.unwrap();
    let decoded = |image: &DisplayImage| {
        let picture = image::load_from_memory(image.bytes.as_ref().unwrap()).unwrap();
        (picture.width(), picture.height())
    };

    // An image no larger than the display size is served as is.
    fixture
        .media
        .set_avatar(actor, Some(small.aid))
        .await
        .unwrap();
    let shown = Box::pin(fixture.media.avatar_image(actor.uid, small.aid, None))
        .await
        .unwrap();
    assert_eq!(decoded(&shown), (64, 64));
    let small_tag = shown.etag;

    // A large image is resized to a 128-pixel square PNG.
    fixture
        .media
        .set_avatar(actor, Some(large.aid))
        .await
        .unwrap();
    let shown = Box::pin(fixture.media.avatar_image(actor.uid, large.aid, None))
        .await
        .unwrap();
    assert_eq!(shown.mime_type, "image/png");
    assert_eq!(decoded(&shown), (128, 128));
    assert_eq!(shown.etag, format!("\"avatar-{}-128\"", large.aid));
    // The second read hits the process cache: still readable after the object is deleted, without accessing S3 again.
    let objects = std::mem::take(&mut *server.state.objects.lock().unwrap());
    let again = Box::pin(fixture.media.avatar_image(actor.uid, large.aid, None))
        .await
        .unwrap();
    assert_eq!(again.bytes, shown.bytes);
    // A browser revalidating with a still-valid ETag gets only a 304.
    let revalidated = Box::pin(fixture.media.avatar_image(
        actor.uid,
        large.aid,
        Some(&format!("W/{}", shown.etag)),
    ))
    .await
    .unwrap();
    assert!(revalidated.bytes.is_none());
    // A replaced old avatar revalidating with its old ETag also gets 404.
    assert!(matches!(
        Box::pin(
            fixture
                .media
                .avatar_image(actor.uid, small.aid, Some(&small_tag))
        )
        .await,
        Err(MediaError::NotFound)
    ));
    // After the account deletion erasure (the same as the last step in privacy/worker.rs), neither the cache nor the
    // browser copy may keep exposing the avatar.
    crate::db::query("UPDATE gonof_common_member SET account_state='deleting' WHERE uid=?")
        .bind(actor.uid)
        .execute(fixture.db.executor())
        .await
        .unwrap();
    crate::db::query("UPDATE gonof_common_member SET username='deleted-display',email=NULL,password=NULL,bio='',avatar=0,avatar_aid=NULL,emailstatus=0,adminid=0,space_visibility='private',account_state='deleted',credentials_version=credentials_version+1 WHERE uid=?")
        .bind(actor.uid)
        .execute(fixture.db.executor())
        .await
        .unwrap();
    assert!(matches!(
        Box::pin(
            fixture
                .media
                .avatar_image(actor.uid, large.aid, Some(&shown.etag))
        )
        .await,
        Err(MediaError::NotFound)
    ));
    assert!(matches!(
        Box::pin(fixture.media.avatar_image(actor.uid, large.aid, None)).await,
        Err(MediaError::NotFound)
    ));

    // Forum icon: resized and negotiated the same way; after the forum becomes members-only, an anonymous
    // revalidation gets 404.
    *server.state.objects.lock().unwrap() = objects;
    crate::db::query("INSERT INTO gonof_forum_forum(name,slug,description,guidance,displayorder,status,visibility,icon_aid) VALUES('Icon board','icon-forum','','',0,1,'public',?)")
        .bind(icon.aid)
        .execute(fixture.db.executor())
        .await
        .unwrap();
    let fid: i64 = fixture
        .scalar("SELECT fid FROM gonof_forum_forum WHERE slug='icon-forum'")
        .await;
    let shown = Box::pin(fixture.media.forum_icon_image(fid, icon.aid, None, None))
        .await
        .unwrap();
    assert_eq!(decoded(&shown), (128, 128));
    assert!(
        Box::pin(
            fixture
                .media
                .forum_icon_image(fid, icon.aid, None, Some(&shown.etag))
        )
        .await
        .unwrap()
        .bytes
        .is_none()
    );
    crate::db::query("UPDATE gonof_forum_forum SET visibility='members' WHERE fid=?")
        .bind(fid)
        .execute(fixture.db.executor())
        .await
        .unwrap();
    assert!(matches!(
        Box::pin(
            fixture
                .media
                .forum_icon_image(fid, icon.aid, None, Some(&shown.etag))
        )
        .await,
        Err(MediaError::NotFound)
    ));
    fixture._services.cleanup_ref().await.unwrap();
}

/// The daily quota counts the larger of the original size and the processed size. A lossless WebP with an alpha
/// channel grows when converted to PNG during processing, and the extra part is charged once processing completes
/// (an opaque static WebP is converted to JPEG and gets smaller, so this test uses one with transparency).
#[tokio::test]
async fn daily_quota_counts_the_larger_of_source_and_processed_size() {
    let fixture = Fixture::new(MediaConfig::default()).await;
    let server = FakeS3::new(fixture.db.clone()).await;
    fixture.configure(&server.endpoint).await;
    let gradient = image::RgbaImage::from_fn(256, 256, |x, y| {
        image::Rgba([x as u8, y as u8, ((x * y) % 251) as u8, (255 - x / 2) as u8])
    });
    let mut webp = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(gradient)
        .write_to(&mut webp, image::ImageFormat::WebP)
        .unwrap();
    let webp = webp.into_inner();
    let first = fixture
        .upload(&identity::random_token(), "gradient.webp", &webp)
        .await
        .unwrap();
    Box::pin(fixture.media.process_pending(identity::now()))
        .await
        .unwrap();
    let processed = fixture.media.upload(fixture.uid, first.aid).await.unwrap();
    assert!(processed.is_ready());
    let original = webp.len() as u64;
    let stored = processed.filesize as u64;
    assert!(
        stored > original,
        "a lossless WebP with transparency grows when converted to PNG: {original} → {stored}"
    );
    // Set the quota so that "original size + second file" just fits but "processed size + second file" does not.
    let second = png();
    let mut media = fixture.media.clone();
    media.config.daily_max_bytes = original + second.len() as u64 + (stored - original) / 2;
    let actor = media.test_actor(fixture.uid).await.unwrap();
    let (path, mut file) = media.staging_file().await.unwrap();
    file.write_all(&second).await.unwrap();
    file.sync_all().await.unwrap();
    drop(file);
    let result = media
        .register_upload_actor(
            actor,
            NewUpload {
                request_key: identity::random_token(),
                filename: "second.png".into(),
                description: String::new(),
                alt_text: String::new(),
                path,
            },
        )
        .await;
    assert!(
        matches!(result, Err(MediaError::Conflict(_))),
        "the growth from processing counts against the quota: {result:?}"
    );
    fixture._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn upload_idempotency_is_concurrent_and_source_quota_survives_reencoding() {
    let mut bytes = png();
    bytes.extend(vec![b'x'; 2000]);
    let fixture = Fixture::new(MediaConfig {
        daily_max_bytes: bytes.len() as u64 + 10,
        ..Default::default()
    })
    .await;
    let server = FakeS3::new(fixture.db.clone()).await;
    fixture.configure(&server.endpoint).await;
    let key = identity::random_token();
    // The request key only accepts 64 lowercase hex digits, consistent with other write entry points.
    assert!(matches!(
        fixture
            .upload(&key.to_uppercase(), "photo.png", &bytes)
            .await,
        Err(MediaError::Validation(_))
    ));
    let (first, second) = tokio::join!(
        fixture.upload(&key, "photo.png", &bytes),
        fixture.upload(&key, "photo.png", &bytes)
    );
    let first = first.unwrap();
    assert_eq!(first.aid, second.unwrap().aid);
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) FROM gonof_forum_attachment")
            .await,
        1
    );
    fixture
        .media
        .process_pending(identity::now())
        .await
        .unwrap();
    let ready = fixture.media.upload(fixture.uid, first.aid).await.unwrap();
    assert!(ready.is_ready());
    assert!(ready.filesize < bytes.len() as i64);
    assert!(matches!(
        fixture
            .upload(&identity::random_token(), "second.png", &bytes)
            .await,
        Err(MediaError::Conflict(_))
    ));
    assert_eq!(
        fixture.upload(&key, "photo.png", &bytes).await.unwrap().aid,
        first.aid
    );
    assert!(matches!(
        fixture.upload(&key, "different.png", &bytes).await,
        Err(MediaError::Conflict(_))
    ));
    assert!(matches!(
        fixture.media.read(first.aid, None, None).await,
        Err(MediaError::NotFound)
    ));
    let download = fixture
        .media
        .read(first.aid, None, Some(fixture.uid))
        .await
        .unwrap();
    assert_eq!(download.mime_type, "image/png");
    assert!(
        !download
            .body
            .collect()
            .await
            .unwrap()
            .into_bytes()
            .is_empty()
    );
    fixture._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn successful_put_and_failed_database_confirmation_recover_without_duplicate_objects() {
    let fixture = Fixture::new(MediaConfig::default()).await;
    let server = FakeS3::new(fixture.db.clone()).await;
    fixture.configure(&server.endpoint).await;
    let key = identity::random_token();
    let upload = fixture.upload(&key, "photo.png", &png()).await.unwrap();
    server.state.fail_db.store(true, Ordering::SeqCst);
    let now = identity::now();
    let report = fixture.media.process_pending(now).await.unwrap();
    assert_eq!(report.failed, 1);
    assert_eq!(
        server.state.objects.lock().unwrap().len(),
        2,
        "two objects: the original and the thumbnail"
    );
    assert!(
        !fixture
            .media
            .upload(fixture.uid, upload.aid)
            .await
            .unwrap()
            .is_ready()
    );
    assert_eq!(
        fixture.upload(&key, "photo.png", &png()).await.unwrap().aid,
        upload.aid
    );
    crate::test_support::drop_trigger(&fixture.db, "fail_media_ready")
        .await
        .unwrap();
    let report = fixture.media.process_pending(now + 3600).await.unwrap();
    assert_eq!(report.processed, 1);
    assert!(
        fixture
            .media
            .upload(fixture.uid, upload.aid)
            .await
            .unwrap()
            .is_ready()
    );
    assert_eq!(
        server.state.objects.lock().unwrap().len(),
        2,
        "two objects: the original and the thumbnail"
    );
    fixture.media.cancel(fixture.uid, upload.aid).await.unwrap();
    fixture.media.process_pending(now + 7200).await.unwrap();
    assert_eq!(
        fixture
            .media
            .upload(fixture.uid, upload.aid)
            .await
            .unwrap()
            .lifecycle,
        "purged"
    );
    assert!(server.state.objects.lock().unwrap().is_empty());
    fixture._services.cleanup_ref().await.unwrap();
}

// Before purging, the unique key media.process:{aid} is used to confirm processing has finished: a processing job
// whose lease has not expired blocks the purge, which happens after it ends; the same key is also used to recover the
// source file name in the quarantine.
#[tokio::test]
async fn purge_waits_for_a_live_process_lease_found_by_event_key() {
    let fixture = Fixture::new(MediaConfig::default()).await;
    let server = FakeS3::new(fixture.db.clone()).await;
    fixture.configure(&server.endpoint).await;
    let upload = fixture
        .upload(&identity::random_token(), "photo.png", &png())
        .await
        .unwrap();
    let now = identity::now();
    fixture.media.process_pending(now).await.unwrap();
    assert_eq!(
        server.state.objects.lock().unwrap().len(),
        2,
        "two objects: the original and the thumbnail"
    );
    fixture.media.cancel(fixture.uid, upload.aid).await.unwrap();
    let key = format!("media.process:{}", upload.aid);
    media_execute(
        &fixture.db,
        &format!("UPDATE gonof_common_outbox SET status='processing',lease_token='other-worker',lease_until={} WHERE event_key='{key}'", now + 86_400),
    )
    .await;
    fixture.media.process_pending(now + 7200).await.unwrap();

    assert_eq!(
        fixture
            .media
            .upload(fixture.uid, upload.aid)
            .await
            .unwrap()
            .lifecycle,
        "purging"
    );
    assert_eq!(
        server.state.objects.lock().unwrap().len(),
        2,
        "two objects: the original and the thumbnail"
    );
    media_execute(
        &fixture.db,
        &format!("UPDATE gonof_common_outbox SET status='sent',lease_token=NULL,lease_until=0 WHERE event_key='{key}'"),
    )
    .await;
    fixture
        .media
        .process_pending(now + 2 * 86_400)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .media
            .upload(fixture.uid, upload.aid)
            .await
            .unwrap()
            .lifecycle,
        "purged"
    );
    assert!(server.state.objects.lock().unwrap().is_empty());
    fixture._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn storage_profiles_preserve_old_objects_and_secrets_never_enter_sql() {
    let fixture = Fixture::new(MediaConfig::default()).await;
    let first = FakeS3::new(fixture.db.clone()).await;
    let second = FakeS3::new(fixture.db.clone()).await;
    let profile = Arc::new(crate::debug::trace::RequestProfile::new());
    crate::debug::trace::scope(profile.clone(), fixture.configure(&first.endpoint)).await;
    for query in profile.snapshot() {
        assert!(!query.expanded_sql.contains("contract-secret-key"));
        assert!(!query.expanded_sql.contains("contract-access-key"));
    }
    let upload = fixture
        .upload(&identity::random_token(), "photo.png", &png())
        .await
        .unwrap();
    fixture
        .media
        .process_pending(identity::now())
        .await
        .unwrap();
    fixture
        .media
        .save_settings(fixture.uid, storage_input(&second.endpoint, 1))
        .await
        .unwrap();
    let download = fixture
        .media
        .read(upload.aid, None, Some(fixture.uid))
        .await
        .unwrap();
    assert!(
        !download
            .body
            .collect()
            .await
            .unwrap()
            .into_bytes()
            .is_empty()
    );
    assert!(second.state.objects.lock().unwrap().is_empty());
    assert!(matches!(
        fixture
            .media
            .save_settings(fixture.uid, storage_input(&second.endpoint, 1))
            .await,
        Err(MediaError::Conflict(_))
    ));
    let settings = fixture.media.settings(fixture.uid).await.unwrap();
    assert_eq!(settings.revision, 2);
    assert_eq!(settings.endpoint, second.endpoint);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut files = tokio::fs::read_dir(fixture.media.directory.join("secrets"))
            .await
            .unwrap();
        while let Some(file) = files.next_entry().await.unwrap() {
            assert_eq!(
                file.metadata().await.unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
    fixture._services.cleanup_ref().await.unwrap();
}

/// New keys for the bucket of earlier attachments replace the keys those attachments are read with: once the old keys
/// are revoked, attachments stored before the change still load, in this process and in another one that had already
/// built a client with the old keys. The old credentials file is removed.
#[tokio::test]
async fn new_keys_for_the_same_bucket_also_serve_earlier_attachments() {
    let fixture = Fixture::new(MediaConfig::default()).await;
    let server = FakeS3::new(fixture.db.clone()).await;
    fixture.configure(&server.endpoint).await;
    let upload = fixture
        .upload(&identity::random_token(), "photo.png", &png())
        .await
        .unwrap();
    fixture
        .media
        .process_pending(identity::now())
        .await
        .unwrap();
    // Another process on the same directory, which has read the attachment with the old keys.
    let other = Media::new(
        fixture.db.clone(),
        MediaConfig::default(),
        fixture.root.clone(),
    );
    let read = |media: &Media| {
        let media = media.clone();
        let (aid, uid) = (upload.aid, fixture.uid);
        async move {
            let download = media.read(aid, None, Some(uid)).await?;
            Ok::<_, MediaError>(download.body.collect().await.unwrap().into_bytes())
        }
    };
    assert!(!read(&other).await.unwrap().is_empty());
    let secrets = fixture.media.directory.join("secrets");
    let files = || std::fs::read_dir(&secrets).unwrap().count();
    assert_eq!(files(), 1);

    let mut rotated = storage_input(&server.endpoint, 1);
    rotated.access_key = "rotated-access-key".into();
    rotated.secret_key = "rotated-secret-key".into();
    fixture
        .media
        .save_settings(fixture.uid, rotated)
        .await
        .unwrap();
    *server.state.revoked.lock().unwrap() = Some("contract-access-key".into());
    assert_eq!(files(), 1, "the old credentials file is removed");
    for media in [&fixture.media, &other] {
        assert!(!read(media).await.unwrap().is_empty());
    }

    // Keys for another bucket leave the earlier attachments on the keys of theirs.
    let mut elsewhere = storage_input(&server.endpoint, 2);
    elsewhere.bucket = "other-media".into();
    elsewhere.access_key = "elsewhere-access-key".into();
    elsewhere.secret_key = "elsewhere-secret-key".into();
    fixture
        .media
        .save_settings(fixture.uid, elsewhere)
        .await
        .unwrap();
    *server.state.revoked.lock().unwrap() = Some("elsewhere-access-key".into());
    assert_eq!(files(), 2);
    assert!(!read(&other).await.unwrap().is_empty());
    fixture._services.cleanup_ref().await.unwrap();
}

/// Spellings of one endpoint are one endpoint: it is stored in canonical form, and new keys saved with any spelling
/// replace the keys earlier attachments are read with, also for a profile an earlier version saved as spelled.
#[tokio::test]
async fn equivalent_endpoint_spellings_share_key_rotation() {
    let fixture = Fixture::new(MediaConfig::default()).await;
    let server = FakeS3::new(fixture.db.clone()).await;
    let shouting = server.endpoint.replace("http://", "HTTP://");
    fixture
        .media
        .save_settings(fixture.uid, storage_input(&format!("{shouting}/"), 0))
        .await
        .unwrap();
    assert_eq!(
        fixture.media.settings(fixture.uid).await.unwrap().endpoint,
        server.endpoint,
        "stored in canonical form"
    );
    let upload = fixture
        .upload(&identity::random_token(), "photo.png", &png())
        .await
        .unwrap();
    fixture
        .media
        .process_pending(identity::now())
        .await
        .unwrap();
    // As an earlier version stored it: the endpoint exactly as typed.
    let stored: String =
        crate::db::query_scalar("SELECT svalue FROM gonof_common_settings WHERE skey=?")
            .bind(SETTING)
            .fetch_one(fixture.db.executor())
            .await
            .unwrap();
    crate::db::query("UPDATE gonof_common_settings SET svalue=? WHERE skey=?")
        .bind(stored.replace(&server.endpoint, &shouting))
        .bind(SETTING)
        .execute(fixture.db.executor())
        .await
        .unwrap();
    let mut rotated = storage_input(&server.endpoint, 1);
    rotated.access_key = "rotated-access-key".into();
    rotated.secret_key = "rotated-secret-key".into();
    fixture
        .media
        .save_settings(fixture.uid, rotated)
        .await
        .unwrap();
    *server.state.revoked.lock().unwrap() = Some("contract-access-key".into());
    let download = fixture
        .media
        .read(upload.aid, None, Some(fixture.uid))
        .await
        .expect("the earlier attachment is read with the new keys");
    assert!(
        !download
            .body
            .collect()
            .await
            .unwrap()
            .into_bytes()
            .is_empty()
    );
    fixture._services.cleanup_ref().await.unwrap();
}

/// Saving the storage settings as they are writes nothing, and a configuration saved before becomes active again
/// instead of being added once more, so the one settings row holding every profile does not grow with each save.
#[tokio::test]
async fn unchanged_storage_saves_do_not_grow_the_settings_record() {
    let fixture = Fixture::new(MediaConfig::default()).await;
    let server = FakeS3::new(fixture.db.clone()).await;
    let first = fixture.configure(&server.endpoint).await;
    let secrets = fixture.media.directory.join("secrets");
    let files = || std::fs::read_dir(&secrets).unwrap().count();
    let keep_keys = |revision: i64| StorageInput {
        access_key: String::new(),
        secret_key: String::new(),
        ..storage_input(&server.endpoint, revision)
    };
    let record = || async {
        crate::db::query_scalar::<String>("SELECT svalue FROM gonof_common_settings WHERE skey=?")
            .bind(SETTING)
            .fetch_one(fixture.db.executor())
            .await
            .unwrap()
    };
    let saved = record().await;
    for _ in 0..30 {
        let settings = fixture
            .media
            .save_settings(fixture.uid, keep_keys(1))
            .await
            .unwrap();
        assert_eq!(settings.revision, 1, "an unchanged save changes nothing");
    }
    assert_eq!(record().await, saved);
    assert_eq!(files(), 1);
    // Another prefix is another configuration; switching back uses the first one again.
    let moved = fixture
        .media
        .save_settings(
            fixture.uid,
            StorageInput {
                prefix: "elsewhere".into(),
                ..keep_keys(1)
            },
        )
        .await
        .unwrap();
    assert_eq!(moved.revision, 2);
    assert_ne!(moved.profile_id, first.profile_id);
    let back = fixture
        .media
        .save_settings(fixture.uid, keep_keys(2))
        .await
        .unwrap();
    assert_eq!(back.revision, 3);
    assert_eq!(back.profile_id, first.profile_id);
    assert_eq!(fixture.media.stored().await.unwrap().profiles.len(), 2);
    assert_eq!(files(), 1, "keeping the keys writes no copy of them");
    // New keys for the same bucket rotate both profiles, and the active one is kept rather than copied.
    let rotated = fixture
        .media
        .save_settings(
            fixture.uid,
            StorageInput {
                access_key: "rotated-access-key".into(),
                secret_key: "rotated-secret-key".into(),
                ..storage_input(&server.endpoint, 3)
            },
        )
        .await
        .unwrap();
    assert_eq!(rotated.revision, 4);
    assert_eq!(rotated.profile_id, first.profile_id);
    assert_eq!(fixture.media.stored().await.unwrap().profiles.len(), 2);
    assert_eq!(files(), 1);
    let again = fixture
        .media
        .save_settings(fixture.uid, keep_keys(4))
        .await
        .unwrap();
    assert_eq!(again.revision, 4);
    fixture._services.cleanup_ref().await.unwrap();
}

/// A muted member cannot upload (docs/roles.md, "Mutes"); the upload is rejected before the attachment row
/// is written and takes no quota; after the mute expires, uploads work as usual.
#[tokio::test]
async fn muted_members_cannot_upload() {
    let fixture = Fixture::new(MediaConfig::default()).await;
    let server = FakeS3::new(fixture.db.clone()).await;
    fixture.configure(&server.endpoint).await;
    let mute = |until: i64| {
        let db = fixture.db.clone();
        let uid = fixture.uid;
        async move {
            crate::db::query("UPDATE gonof_common_member SET mute_until=? WHERE uid=?")
                .bind(until)
                .bind(uid)
                .execute(db.executor())
                .await
                .unwrap();
        }
    };
    mute(identity::now() + 3600).await;
    assert!(matches!(
        fixture.upload(&identity::random_token(), "photo.png", &png()).await,
        Err(MediaError::Validation(message)) if message.contains("You are muted until")
    ));
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) FROM gonof_forum_attachment")
            .await,
        0
    );
    mute(identity::now() - 1).await;
    assert!(
        fixture
            .upload(&identity::random_token(), "photo.png", &png())
            .await
            .is_ok()
    );
    fixture._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn unavailable_storage_keeps_uploads_private_and_orphan_cleanup_obeys_references() {
    let fixture = Fixture::new(MediaConfig::default()).await;
    let server = FakeS3::new(fixture.db.clone()).await;
    fixture.configure(&server.endpoint).await;
    server.state.fail_put.store(true, Ordering::SeqCst);
    let upload = fixture
        .upload(&identity::random_token(), "photo.png", &png())
        .await
        .unwrap();
    let now = identity::now();
    assert_eq!(fixture.media.process_pending(now).await.unwrap().failed, 1);
    assert_eq!(
        fixture
            .media
            .upload(fixture.uid, upload.aid)
            .await
            .unwrap()
            .scan_status,
        "error"
    );
    assert!(server.state.objects.lock().unwrap().is_empty());
    assert!(
        fixture
            .media
            .read(upload.aid, None, Some(fixture.uid))
            .await
            .is_err()
    );
    fixture.media.cleanup(now + 8 * 86400).await.unwrap();
    assert_eq!(
        fixture
            .media
            .upload(fixture.uid, upload.aid)
            .await
            .unwrap()
            .lifecycle,
        "purged"
    );
    fixture._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
#[ignore = "needs GONOF_TEST_S3_CONFIG pointing at made-up credentials for a separate test S3"]
async fn local_s3_contract() {
    let path = std::env::var("GONOF_TEST_S3_CONFIG")
        .expect("set GONOF_TEST_S3_CONFIG to the local S3 test config path");
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let fixture = Fixture::new(MediaConfig::default()).await;
    let endpoint = value["endpoint"].as_str().unwrap();
    let mut input = storage_input(endpoint, 0);
    input.access_key = value["access_key"].as_str().unwrap().into();
    input.secret_key = value["secret_key"].as_str().unwrap().into();
    input.bucket = format!("gonof-contract-{}", &identity::random_token()[..20]);
    let settings = fixture
        .media
        .save_settings(fixture.uid, input)
        .await
        .unwrap();
    let (client, profile) = fixture
        .media
        .object_client(&settings.profile_id)
        .await
        .unwrap();
    client
        .create_bucket()
        .bucket(&profile.bucket)
        .send()
        .await
        .unwrap();
    let mut test = storage_input(endpoint, 1);
    test.bucket = profile.bucket.clone();
    test.access_key.clear();
    test.secret_key.clear();
    fixture
        .media
        .test_settings(fixture.uid, test)
        .await
        .unwrap();
    let upload = fixture
        .upload(&identity::random_token(), "photo.png", &png())
        .await
        .unwrap();
    let report = fixture
        .media
        .process_pending(identity::now())
        .await
        .unwrap();
    assert_eq!(report.failed, 0);
    assert!(
        fixture
            .media
            .upload(fixture.uid, upload.aid)
            .await
            .unwrap()
            .is_ready()
    );
    let download = fixture
        .media
        .read(upload.aid, None, Some(fixture.uid))
        .await
        .unwrap();
    assert!(
        !download
            .body
            .collect()
            .await
            .unwrap()
            .into_bytes()
            .is_empty()
    );
    let object: String =
        crate::db::query_scalar("SELECT attachment FROM gonof_forum_attachment_0 WHERE aid=?")
            .bind(upload.aid)
            .fetch_one(fixture.db.executor())
            .await
            .unwrap();
    fixture.media.cancel(fixture.uid, upload.aid).await.unwrap();
    fixture
        .media
        .process_pending(identity::now())
        .await
        .unwrap();
    assert!(
        client
            .head_object()
            .bucket(&profile.bucket)
            .key(object)
            .send()
            .await
            .unwrap_err()
            .as_service_error()
            .is_some_and(|error| error.is_not_found())
    );
    client
        .delete_bucket()
        .bucket(&profile.bucket)
        .send()
        .await
        .unwrap();
}

async fn media_database_contract(fixture: &Fixture) {
    use crate::content::{
        self,
        writes::{ContentWriter, NewReply, NewThread},
    };
    use futures_util::future::join_all;

    let server = FakeS3::new(fixture.db.clone()).await;
    fixture.configure(&server.endpoint).await;
    let bytes = png();
    let key = identity::random_token();
    let uploads = join_all((0..8).map(|_| fixture.upload(&key, "photo.png", &bytes))).await;
    let aid = uploads[0].as_ref().unwrap().aid;
    assert!(uploads.into_iter().all(|upload| upload.unwrap().aid == aid));
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) FROM gonof_forum_attachment")
            .await,
        1
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) FROM gonof_common_outbox WHERE event_type='media.process'")
            .await,
        1
    );
    assert!(matches!(
        fixture.media.read(aid, None, Some(fixture.uid)).await,
        Err(MediaError::NotFound)
    ));
    let (left, right) = tokio::join!(
        Box::pin(fixture.media.process_pending(identity::now())),
        Box::pin(fixture.media.process_pending(identity::now()))
    );
    assert_eq!(left.unwrap().processed + right.unwrap().processed, 1);
    assert!(
        fixture
            .media
            .upload(fixture.uid, aid)
            .await
            .unwrap()
            .is_ready()
    );
    assert!(matches!(
        Box::pin(fixture.media.read(aid, None, None)).await,
        Err(MediaError::NotFound)
    ));
    assert!(matches!(
        Box::pin(fixture.media.read(aid, None, Some(fixture.uid + 1))).await,
        Err(MediaError::NotFound)
    ));
    let stale_actor = identity::Actor {
        uid: fixture.uid,
        credentials_version: 2,
        api_key: None,
    };
    assert!(matches!(
        fixture.media.upload_for_actor(stale_actor, aid).await,
        Err(MediaError::NotFound)
    ));
    assert!(matches!(
        fixture
            .media
            .read_for_actor(aid, None, Some(stale_actor))
            .await,
        Err(MediaError::NotFound)
    ));
    let original_actor = identity::Actor {
        uid: fixture.uid,
        credentials_version: 1,
        api_key: None,
    };
    for (lifecycle, scan_status) in [
        ("processing", "clean"),
        ("ready", "pending"),
        ("ready", "rejected"),
        ("ready", "error"),
    ] {
        crate::db::query(
            "UPDATE gonof_forum_attachment_0 SET lifecycle=?,scan_status=? WHERE aid=?",
        )
        .bind(lifecycle)
        .bind(scan_status)
        .bind(aid)
        .execute(fixture.db.executor())
        .await
        .unwrap();
        assert!(matches!(
            fixture
                .media
                .read_for_actor(aid, None, Some(original_actor))
                .await,
            Err(MediaError::NotFound)
        ));
    }
    crate::db::query(
        "UPDATE gonof_forum_attachment_0 SET lifecycle='ready',scan_status='clean' WHERE aid=?",
    )
    .bind(aid)
    .execute(fixture.db.executor())
    .await
    .unwrap();
    let log_count = fixture
        .scalar("SELECT COUNT(*) FROM gonof_common_log")
        .await;
    media_execute(
        &fixture.db,
        &format!(
            "UPDATE gonof_common_member SET credentials_version=2 WHERE uid={}",
            fixture.uid
        ),
    )
    .await;
    for rejected_key in [&key, &identity::random_token()] {
        assert!(matches!(
            fixture
                .upload_as(original_actor, rejected_key, "photo.png", &bytes)
                .await,
            Err(MediaError::Forbidden)
        ));
    }
    assert!(matches!(
        fixture.media.cancel_actor(original_actor, aid).await,
        Err(MediaError::Forbidden)
    ));
    assert!(matches!(
        fixture
            .media
            .save_settings_actor(original_actor, storage_input(&server.endpoint, 1))
            .await,
        Err(MediaError::Forbidden)
    ));
    assert!(matches!(
        fixture
            .media
            .test_settings_actor(original_actor, storage_input(&server.endpoint, 1))
            .await,
        Err(MediaError::Forbidden)
    ));
    assert_eq!(
        fixture.media.settings(fixture.uid).await.unwrap().revision,
        1
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) FROM gonof_forum_attachment")
            .await,
        1
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) FROM gonof_common_outbox")
            .await,
        1
    );
    assert_eq!(
        fixture
            .scalar("SELECT COUNT(*) FROM gonof_common_log")
            .await,
        log_count
    );
    assert!(
        fixture
            .media
            .upload(fixture.uid, aid)
            .await
            .unwrap()
            .is_ready()
    );
    media_execute(
        &fixture.db,
        &format!(
            "UPDATE gonof_common_member SET credentials_version=1 WHERE uid={}",
            fixture.uid
        ),
    )
    .await;
    assert!(matches!(
        fixture.upload(&key, "changed.png", &bytes).await,
        Err(MediaError::Conflict(_))
    ));

    content::seed_forums(&fixture.db).await.unwrap();
    let fid = fixture
        .scalar("SELECT MIN(fid) FROM gonof_forum_forum")
        .await;
    let writer = ContentWriter::new(fixture.db.clone(), Default::default());
    let first = writer
        .publish(
            fixture.uid,
            NewThread {
                visibility: Default::default(),
                activity: Default::default(),
                fid,
                title: "Attachment visibility".into(),
                body: format!("![picture](attachment:{aid})"),
                attachment_ids: vec![aid],
                request_key: identity::random_token(),
            },
        )
        .await
        .unwrap();
    let root = writer
        .reply(
            fixture.uid,
            NewReply {
                tid: first.tid,
                root_pid: None,
                reply_to_pid: None,
                body: "Opening post".into(),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
        )
        .await
        .unwrap();
    let child = writer
        .reply(
            fixture.uid,
            NewReply {
                tid: first.tid,
                root_pid: Some(root.pid),
                reply_to_pid: Some(root.pid),
                body: format!("![picture](attachment:{aid})"),
                attachment_ids: vec![aid],
                request_key: identity::random_token(),
            },
        )
        .await
        .unwrap();
    for pid in [first.pid, child.pid] {
        assert_public_download(fixture, aid, pid).await;
    }
    assert!(matches!(
        fixture.media.read(aid, None, Some(fixture.uid)).await,
        Err(MediaError::NotFound)
    ));
    assert!(matches!(
        fixture.media.upload(fixture.uid, aid).await,
        Err(MediaError::NotFound)
    ));
    assert!(matches!(
        Box::pin(fixture.media.read(aid, Some(root.pid), None)).await,
        Err(MediaError::NotFound)
    ));
    assert!(matches!(
        Box::pin(fixture.media.read(aid, Some(999_999), None)).await,
        Err(MediaError::NotFound)
    ));
    assert!(matches!(
        fixture.media.cancel(fixture.uid, aid).await,
        Err(MediaError::Conflict(_))
    ));

    // A concrete post context must stay public through every ancestor and publication gate.
    let changes = [
        ("gonof_forum_forum", "fid", fid, "status", "0", "1"),
        (
            "gonof_forum_forum",
            "fid",
            fid,
            "visibility",
            "'private'",
            "'public'",
        ),
        (
            "gonof_forum_thread",
            "tid",
            first.tid,
            "displayorder",
            "-1",
            "0",
        ),
        ("gonof_forum_thread", "tid", first.tid, "readperm", "1", "0"),
        (
            "gonof_forum_thread",
            "tid",
            first.tid,
            "visibility",
            "'private'",
            "'public'",
        ),
        (
            "gonof_forum_thread",
            "tid",
            first.tid,
            "publication_state",
            "'draft'",
            "'published'",
        ),
        ("gonof_forum_post", "pid", first.pid, "invisible", "1", "0"),
        ("gonof_forum_post", "pid", root.pid, "invisible", "1", "0"),
        ("gonof_forum_post", "pid", child.pid, "invisible", "1", "0"),
        ("gonof_forum_post", "pid", child.pid, "deleted_at", "1", "0"),
        (
            "gonof_forum_attachment_0",
            "aid",
            aid,
            "scan_status",
            "'pending'",
            "'clean'",
        ),
    ];
    for (table, id_column, id, column, hidden, visible) in changes {
        media_execute(
            &fixture.db,
            &format!("UPDATE {table} SET {column}={hidden} WHERE {id_column}={id}"),
        )
        .await;
        assert!(
            matches!(
                Box::pin(fixture.media.read(aid, Some(child.pid), None)).await,
                Err(MediaError::NotFound)
            ),
            "download ignored {table}.{column}"
        );
        media_execute(
            &fixture.db,
            &format!("UPDATE {table} SET {column}={visible} WHERE {id_column}={id}"),
        )
        .await;
    }
    for (table, id_column, id) in [
        ("gonof_forum_thread", "tid", first.tid),
        ("gonof_forum_post", "pid", first.pid),
        ("gonof_forum_post", "pid", root.pid),
    ] {
        media_execute(
            &fixture.db,
            &format!("UPDATE {table} SET deleted_at=1 WHERE {id_column}={id}"),
        )
        .await;
        assert_public_download(fixture, aid, child.pid).await;
        media_execute(
            &fixture.db,
            &format!("UPDATE {table} SET deleted_at=0 WHERE {id_column}={id}"),
        )
        .await;
    }
    private_media_access_contract(fixture, aid, first.pid, root.pid, child.pid, fid).await;
    media_execute(
        &fixture.db,
        &format!(
            "UPDATE gonof_forum_thread SET closed=1 WHERE tid={}",
            first.tid
        ),
    )
    .await;
    assert_public_download(fixture, aid, child.pid).await;
    media_execute(
        &fixture.db,
        &format!(
            "UPDATE gonof_forum_thread SET closed=0 WHERE tid={}",
            first.tid
        ),
    )
    .await;
    Box::pin(fixture.media.cleanup(identity::now() + 8 * 86400))
        .await
        .unwrap();
    assert_public_download(fixture, aid, child.pid).await;
    assert_eq!(fixture.scalar(&format!("SELECT COUNT(*) FROM gonof_forum_attachment_0 WHERE aid={aid} AND purge_after IS NULL")).await,1);

    let second = fixture
        .upload(&identity::random_token(), "second.png", &bytes)
        .await
        .unwrap();
    Box::pin(fixture.media.process_pending(identity::now()))
        .await
        .unwrap();
    let request = NewReply {
        tid: first.tid,
        root_pid: None,
        reply_to_pid: None,
        body: format!("![contested image](attachment:{})", second.aid),
        attachment_ids: vec![second.aid],
        request_key: identity::random_token(),
    };
    let (posted, cancelled) = tokio::join!(
        writer.reply(fixture.uid, request),
        fixture.media.cancel(fixture.uid, second.aid)
    );
    match (posted, cancelled) {
        (Ok(post), Err(MediaError::Conflict(_))) => {
            assert_public_download(fixture, second.aid, post.pid).await
        }
        (Err(crate::content::writes::ContentWriteError::Validation(_)), Ok(())) => {
            assert_eq!(
                fixture
                    .scalar(&format!(
                        "SELECT COUNT(*) FROM gonof_forum_attachment_reference WHERE aid={}",
                        second.aid
                    ))
                    .await,
                0
            );
            Box::pin(fixture.media.process_pending(identity::now() + 3600))
                .await
                .unwrap();
            assert_eq!(
                fixture
                    .media
                    .upload(fixture.uid, second.aid)
                    .await
                    .unwrap()
                    .lifecycle,
                "purged"
            );
        }
        outcome => panic!("exactly one of binding and cancelling must succeed: {outcome:?}"),
    }
    assert!(matches!(
        fixture
            .upload(&identity::random_token(), "quota.png", &bytes)
            .await,
        Err(MediaError::Conflict(_))
    ));
    assert_eq!(
        fixture.upload(&key, "photo.png", &bytes).await.unwrap().aid,
        aid
    );
    Box::pin(avatar_tests::contract(fixture, &server)).await;
}

async fn private_media_access_contract(
    fixture: &Fixture,
    aid: i64,
    first_pid: i64,
    root_pid: i64,
    child_pid: i64,
    fid: i64,
) {
    let actor = identity::Actor {
        uid: fixture.uid,
        credentials_version: 1,
        api_key: None,
    };
    assert!(
        fixture
            .media
            .upload_for_post(actor, aid, child_pid)
            .await
            .unwrap()
            .is_ready()
    );
    media_execute(
        &fixture.db,
        &format!("UPDATE gonof_forum_post SET invisible=1 WHERE pid={child_pid}"),
    )
    .await;
    assert!(
        fixture
            .media
            .read_for_actor(aid, Some(child_pid), Some(actor))
            .await
            .is_ok()
    );
    // Staff permissions, current credentials, verification and freeze status are reloaded.
    for assignment in [
        "credentials_version=2",
        "`freeze`=1",
        "adminid=0,email='media-contract@example.test'",
    ] {
        media_execute(
            &fixture.db,
            &format!(
                "UPDATE gonof_common_member SET {assignment} WHERE uid={}",
                fixture.uid
            ),
        )
        .await;
        assert!(matches!(
            fixture
                .media
                .read_for_actor(aid, Some(child_pid), Some(actor))
                .await,
            Err(MediaError::NotFound)
        ));
        assert!(matches!(
            fixture.media.upload_for_post(actor, aid, child_pid).await,
            Err(MediaError::NotFound)
        ));
        media_execute(
            &fixture.db,
            &format!("UPDATE gonof_common_member SET credentials_version=1,`freeze`=0,adminid=1 WHERE uid={}", fixture.uid),
        )
        .await;
    }
    media_execute(
        &fixture.db,
        &format!("UPDATE gonof_common_member SET adminid=3,email='media-contract@example.test',emailstatus=1 WHERE uid={}", fixture.uid),
    )
    .await;
    assert!(matches!(
        fixture
            .media
            .read_for_actor(aid, Some(child_pid), Some(actor))
            .await,
        Err(MediaError::NotFound)
    ));
    media_execute(
        &fixture.db,
        &format!(
            "INSERT INTO gonof_forum_moderator(uid,fid) VALUES({},{fid})",
            fixture.uid
        ),
    )
    .await;
    assert!(
        fixture
            .media
            .read_for_actor(aid, Some(child_pid), Some(actor))
            .await
            .is_ok()
    );
    media_execute(
        &fixture.db,
        &format!(
            "UPDATE gonof_common_member SET emailstatus=0 WHERE uid={}",
            fixture.uid
        ),
    )
    .await;
    assert!(
        fixture
            .media
            .read_for_actor(aid, Some(child_pid), Some(actor))
            .await
            .is_ok()
    );
    assert!(
        fixture
            .media
            .upload_for_post(actor, aid, child_pid)
            .await
            .is_ok()
    );
    media_execute(
        &fixture.db,
        &format!(
            "DELETE FROM gonof_forum_moderator WHERE uid={}",
            fixture.uid
        ),
    )
    .await;
    media_execute(
        &fixture.db,
        &format!(
            "UPDATE gonof_common_member SET adminid=0,emailstatus=1 WHERE uid={}",
            fixture.uid
        ),
    )
    .await;
    media_execute(
        &fixture.db,
        &format!("UPDATE gonof_forum_post SET invisible=0,deleted_at=1 WHERE pid={child_pid}"),
    )
    .await;
    assert!(
        fixture
            .media
            .read_for_actor(aid, Some(child_pid), Some(actor))
            .await
            .is_ok()
    );
    media_execute(
        &fixture.db,
        &format!(
            "UPDATE gonof_common_member SET emailstatus=0 WHERE uid={}",
            fixture.uid
        ),
    )
    .await;
    assert!(matches!(
        fixture
            .media
            .read_for_actor(aid, Some(child_pid), Some(actor))
            .await,
        Err(MediaError::NotFound)
    ));
    assert!(matches!(
        fixture.media.upload_for_post(actor, aid, child_pid).await,
        Err(MediaError::NotFound)
    ));
    media_execute(
        &fixture.db,
        &format!(
            "UPDATE gonof_common_member SET emailstatus=1 WHERE uid={}",
            fixture.uid
        ),
    )
    .await;
    assert!(matches!(
        fixture.media.read(aid, Some(child_pid), None).await,
        Err(MediaError::NotFound)
    ));
    // Past the author's edit window (`settings.posting.edit_window_hours`, seeded as 24 hours) it is no longer
    // visible: move this floor's post time outside the window, check, then move it back.
    media_execute(
        &fixture.db,
        &format!("UPDATE gonof_forum_post SET dateline=dateline-2*86400 WHERE pid={child_pid}"),
    )
    .await;
    assert!(matches!(
        fixture
            .media
            .read_for_actor(aid, Some(child_pid), Some(actor))
            .await,
        Err(MediaError::NotFound)
    ));
    media_execute(
        &fixture.db,
        &format!("UPDATE gonof_forum_post SET dateline=dateline+2*86400 WHERE pid={child_pid}"),
    )
    .await;
    // A hidden parent or first post blocks the author's private withdrawal preview too.
    for ancestor in [first_pid, root_pid] {
        media_execute(
            &fixture.db,
            &format!("UPDATE gonof_forum_post SET invisible=1 WHERE pid={ancestor}"),
        )
        .await;
        assert!(matches!(
            fixture
                .media
                .read_for_actor(aid, Some(child_pid), Some(actor))
                .await,
            Err(MediaError::NotFound)
        ));
        media_execute(
            &fixture.db,
            &format!("UPDATE gonof_forum_post SET invisible=0 WHERE pid={ancestor}"),
        )
        .await;
    }
    media_execute(
        &fixture.db,
        &format!("UPDATE gonof_forum_post SET deleted_at=0 WHERE pid={child_pid}"),
    )
    .await;
    media_execute(
        &fixture.db,
        &format!(
            "UPDATE gonof_common_member SET adminid=1 WHERE uid={}",
            fixture.uid
        ),
    )
    .await;
}

async fn assert_public_download(fixture: &Fixture, aid: i64, pid: i64) {
    let file = Box::pin(fixture.media.read(aid, Some(pid), None))
        .await
        .unwrap();
    assert_eq!(file.mime_type, "image/png");
    assert!(!file.body.collect().await.unwrap().into_bytes().is_empty());
}
async fn media_execute(db: &Database, sql: &str) {
    crate::db::query(sql).execute(db.executor()).await.unwrap();
}
fn database_media_config() -> MediaConfig {
    MediaConfig {
        daily_max_bytes: png().len() as u64 * 2 + 10,
        ..Default::default()
    }
}

#[tokio::test]
async fn mysql_media_database_contract() {
    let fixture = Fixture::new(database_media_config()).await;
    Box::pin(media_database_contract(&fixture)).await;
    fixture._services.cleanup_ref().await.unwrap();
}

#[tokio::test]
async fn contributing_actor_checks_credentials_and_freeze_before_contribution_role() {
    let fixture = Fixture::new(MediaConfig::default()).await;
    let actor = identity::Actor {
        uid: fixture.uid,
        credentials_version: 1,
        api_key: None,
    };
    assert!(
        fixture.media.contributing_actor(actor).await.unwrap(),
        "an administrator can upload without a verified email"
    );
    for denied in [
        identity::Actor {
            credentials_version: 2,
            ..actor
        },
        identity::Actor {
            uid: fixture.uid + 1,
            ..actor
        },
    ] {
        assert!(!fixture.media.contributing_actor(denied).await.unwrap());
    }
    for (role, verified, frozen, allowed) in [
        (0, 0, 0, false),
        (0, 1, 0, true),
        (1, 0, 1, false),
        (0, 1, 1, false),
    ] {
        crate::db::query("UPDATE gonof_common_member SET email='media-actor@example.test',adminid=?,emailstatus=?,`freeze`=? WHERE uid=?")
            .bind(role).bind(verified).bind(frozen).bind(actor.uid).execute(fixture.db.executor()).await.unwrap();
        assert_eq!(
            fixture.media.contributing_actor(actor).await.unwrap(),
            allowed
        );
    }
    fixture._services.cleanup_ref().await.unwrap();
}

/// The value rules of attachment details, which live in code: the upload entry point and the re-check before an
/// inspection result is written enforce them.
#[test]
fn attachment_values_keep_the_rules_the_trigger_had() {
    let sha = "a".repeat(64);
    assert!(check_inspection("image/png", &sha, 10, Some(1), Some(2)).is_ok());
    assert!(check_inspection("application/pdf", &sha, 0, None, None).is_ok());
    for (mime, digest, size, width, height) in [
        ("", sha.as_str(), 10, None, None),
        (&*"m".repeat(128), sha.as_str(), 10, None, None),
        ("image/png", "short", 10, None, None),
        ("image/png", sha.as_str(), -1, None, None),
        ("image/png", sha.as_str(), 10, Some(1), None),
        ("image/png", sha.as_str(), 10, None, Some(1)),
        ("image/png", sha.as_str(), 10, Some(0), Some(2)),
        ("image/png", sha.as_str(), 10, Some(2), Some(-1)),
    ] {
        assert!(
            check_inspection(mime, digest, size, width, height).is_err(),
            "{mime:?} {digest} {size} {width:?} {height:?}"
        );
    }
    let upload = |name: &str, path: &str, profile: &str, dateline: i64, purge: i64| {
        check_new_upload(
            name,
            10,
            "application/octet-stream",
            &sha,
            path,
            profile,
            dateline,
            purge,
        )
    };
    assert!(upload("a.png", "objects/1-x", "p", 100, 100).is_ok());
    assert!(upload("", "objects/1-x", "p", 100, 200).is_err());
    assert!(upload(&"x".repeat(256), "objects/1-x", "p", 100, 200).is_err());
    assert!(upload("a.png", "", "p", 100, 200).is_err());
    assert!(upload("a.png", &"o".repeat(1025), "p", 100, 200).is_err());
    assert!(upload("a.png", "objects/1-x", &"p".repeat(65), 100, 200).is_err());
    assert!(upload("a.png", "objects/1-x", "p", 100, 99).is_err());
    assert!(upload("a.png", "objects/1-x", "p", -1, 99).is_err());
    // The state values are a fixed set.
    assert_eq!(
        LIFECYCLES,
        [
            "pending",
            "processing",
            "ready",
            "rejected",
            "purging",
            "purged"
        ]
    );
    assert_eq!(SCAN_STATUSES, ["pending", "clean", "rejected", "error"]);
}
