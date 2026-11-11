//! Home page display data; reads only existing structures, and personal statistics never enter the shared cache.
use super::*;

pub struct MemberSummary {
    pub available: i64,
    pub frozen: i64,
    pub threads: i64,
    pub replies: i64,
    pub favorites: i64,
    pub checked_in: bool,
    pub signin_enabled: bool,
    pub signin_amount: i64,
    /// Idempotency key of the check-in form, regenerated on every render of the card; used only by this card and
    /// independent of the credits page.
    pub unread_messages: i64,
}

/// `rules` is all rows of the credit reward rules, which the page takes from the in-process global cache (check-in
/// switch and amount); `offset_minutes` is the site time zone, and "checked in today or not" uses its calendar day.
pub async fn member_summary(
    db: &Database,
    actor: crate::identity::Actor,
    rules: &[crate::activities::Rule],
    offset_minutes: i64,
) -> Result<MemberSummary> {
    ensure!(
        crate::identity::valid_actor(db, actor).await?,
        crate::i18n::current().common_error_session()
    );
    let account =
        crate::db::query("SELECT available,frozen FROM gonof_common_credit_account WHERE uid=?")
            .bind(actor.uid)
            .fetch_optional(db.executor())
            .await?;
    let (available, frozen) = match account {
        Some(row) => (row.try_get("available")?, row.try_get("frozen")?),
        None => (0, 0),
    };
    // Thread and reply counts read the member count table `gonof_common_member_count` instead of counting
    // threads and posts on every page.
    let stats = super::member_counts(db, actor.uid).await?;
    let counts = [i64::try_from(stats.threads)?, i64::try_from(stats.replies)?];
    let favorites =
        crate::db::query_scalar::<i64>("SELECT COUNT(*) FROM gonof_home_favorite WHERE uid=?")
            .bind(actor.uid)
            .fetch_one(db.executor())
            .await?;
    let signin = crate::activities::signin_state(
        db,
        actor.uid,
        crate::identity::now(),
        offset_minutes,
        rules,
    )
    .await?;
    Ok(MemberSummary {
        available,
        frozen,
        threads: counts[0],
        replies: counts[1],
        favorites,
        checked_in: signin.checked_in,
        signin_enabled: signin.enabled,
        signin_amount: signin.amount,
        unread_messages: crate::messages::unread_conversations(db, actor.uid).await?,
    })
}

/// Data of the `/my` overview page: the six numbers the cards already show are not repeated here; it only adds what
/// the cards lack.
pub struct MyOverview {
    pub bio: String,
    pub regdate: i64,
    pub following_threads: i64,
    pub following_forums: i64,
}

pub async fn my_overview(db: &Database, actor: crate::identity::Actor) -> Result<MyOverview> {
    ensure!(
        crate::identity::valid_actor(db, actor).await?,
        crate::i18n::current().common_error_session()
    );
    let member = crate::db::query("SELECT bio,regdate FROM gonof_common_member WHERE uid=?")
        .bind(actor.uid)
        .fetch_one(db.executor())
        .await?;
    let mut counts = [0_i64; 2];
    for (index, table) in ["gonof_home_thread_follow", "gonof_home_forum_follow"]
        .into_iter()
        .enumerate()
    {
        counts[index] =
            crate::db::query_scalar::<i64>(&format!("SELECT COUNT(*) FROM {table} WHERE uid=?"))
                .bind(actor.uid)
                .fetch_one(db.executor())
                .await?;
    }
    Ok(MyOverview {
        bio: decode_text(&member, "bio")?,
        regdate: member.try_get("regdate")?,
        following_threads: counts[0],
        following_forums: counts[1],
    })
}

pub use super::members::ListedMember as RecentMember;

/// Newest users under the home page statistics card, at most `limit` of them (`settings.display.recent_members`).
/// Visibility is the same as member search ([`super::members::listed_scope_sql`]), and only normal accounts are
/// listed: frozen accounts and accounts being deleted do not get the home page spot.
///
/// Taken by uid descending: members are only created by registration and uid auto-increments, matching registration
/// order; the member table has no `regdate` index, so sorting by it would scan and sort the whole table, while
/// reading the primary key backwards stops once enough rows are read.
pub async fn recent_members(
    db: &Database,
    viewer_uid: Option<i64>,
    limit: i64,
) -> Result<Vec<RecentMember>> {
    crate::db::query(&format!(
        "SELECT m.uid,m.username,m.avatar,m.avatar_aid,m.regdate FROM gonof_common_member m
         WHERE m.account_state='active' AND m.freeze=0 AND {}
         ORDER BY m.uid DESC LIMIT {}",
        super::members::listed_scope_sql(viewer_uid),
        limit
    ))
    .fetch_all(db.executor())
    .await?
    .iter()
    .map(|row| super::members::decode_listed(row, false))
    .collect()
}

/// Newest users for anonymous visitors go through the shared cache, in the same bucket as the site-wide statistics
/// (`settings.cache.statistics_seconds`). Privacy, account status and renames advance the site-wide content version
/// inside the write transaction; avatar changes do not — the page serves avatars by uid, and the cached `avatar_aid`
/// is not used to build the URL; for logged-in members the result includes "Logged-in members" and is filtered by the
/// member's own blocks, so it queries the database every time and never enters the shared cache.
pub async fn cached_recent_members(
    db: &Database,
    cache: &crate::cache::TimedCache,
    viewer_uid: Option<i64>,
    (now, limit): (i64, i64),
) -> Result<Vec<RecentMember>> {
    if viewer_uid.is_some() || !cache.cache().enabled() {
        return recent_members(db, viewer_uid, limit).await;
    }
    anonymous_recent_members(
        db,
        cache,
        super::cache_revision(db).await?,
        cache.lifetimes().statistics_bucket(now),
        limit,
    )
    .await
}

async fn anonymous_recent_members(
    db: &Database,
    cache: &crate::cache::TimedCache,
    revision: i64,
    bucket: i64,
    limit: i64,
) -> Result<Vec<RecentMember>> {
    let key = crate::cache::CacheKey::new(
        &crate::cache::registry::RECENT_MEMBERS,
        revision,
        &(bucket, limit),
    )?;
    cache
        .get_or_fill_json(&key, || recent_members(db, None, limit))
        .await
}

/// For pages: both layers share the content version read from MySQL in this request, reusing it when the forum list
/// already fetched it. New members appear per statistics period; tightened privacy, freezes and account deletion take
/// effect on the next request, even if the process has not yet received the broadcast.
pub async fn layered_recent_members(
    db: &Database,
    global: &crate::cache::GlobalCache,
    cache: &crate::cache::TimedCache,
    viewer_uid: Option<i64>,
    (now, limit): (i64, i64),
    list_revision: Option<i64>,
) -> Result<Vec<RecentMember>> {
    if viewer_uid.is_some() || (global.statistics_bypass() && !cache.cache().enabled()) {
        return recent_members(db, viewer_uid, limit).await;
    }
    let revision = match list_revision {
        Some(revision) => revision,
        None => super::cache_revision(db).await?,
    };
    let bucket = cache.lifetimes().statistics_bucket(now);
    global
        .recent_members((revision, bucket, limit), || {
            anonymous_recent_members(db, cache, revision, bucket, limit)
        })
        .await
}
