//! Page frame data for authenticated members (registry [`super::registry::MEMBER_FRAME`]): per person, cached only when
//! Redis is enabled, read straight from the database when it is disabled. The shared key stores
//! only the raw values the member chose; the site defaults and the domain's forum tree are merged in on every request.
use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::{Cache, CacheKey, Lifetimes, registry};
use crate::{
    db::Database,
    interaction::forum_filter::{FilterMode, ForumFilters},
    reading::{self, Choices},
};

#[derive(Serialize, Deserialize)]
pub(crate) struct MemberFrame {
    pub filters: BTreeMap<i64, FilterMode>,
    pub reading: (Choices, i64, i64),
}

fn key(uid: i64) -> Result<CacheKey> {
    CacheKey::new(&registry::MEMBER_FRAME, 0, &uid)
}

pub(crate) async fn load(
    db: &Database,
    cache: &Cache,
    ttl: Lifetimes,
    uid: i64,
) -> Result<MemberFrame> {
    cache
        .get_or_fill_json(&key(uid)?, ttl, || async {
            Ok(MemberFrame {
                filters: ForumFilters::load_direct(db, uid).await?,
                reading: reading::load_choices(db, uid).await?,
            })
        })
        .await
}

pub(crate) async fn clear(cache: &Cache, uid: i64) -> Result<()> {
    cache.delete(&key(uid)?).await
}

/// Call after the transaction commits, including for replayed successful write requests, so that a key deletion that hit
/// a fault last time can be retried.
pub(crate) async fn invalidate(cache: &Cache, uid: i64) -> Result<()> {
    clear(cache, uid)
        .await
        .context(crate::i18n::current().common_cache_member_frame())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db, identity::Actor, metrics, test_support::TestFixture};
    use std::sync::Arc;

    #[tokio::test]
    async fn member_keys_are_isolated_invalidated_after_save_and_bypassed_at_zero() {
        let f = TestFixture::migrated().await.unwrap();
        let ttl = Lifetimes::from_settings(&crate::runtime::read(&f.db).await.unwrap());
        for uid in [71, 72] {
            db::query("INSERT INTO gonof_common_member(uid,username,password,email,regdate) VALUES(?,?,'fixture',?,1)")
                .bind(uid).bind(format!("viewer{uid}")).bind(format!("{uid}@example.test"))
                .execute(f.db.executor()).await.unwrap();
        }
        let actor = Actor {
            uid: 71,
            credentials_version: 1,
            api_key: None,
        };
        let cold = load(&f.db, &f.cache, ttl, actor.uid).await.unwrap();
        assert_eq!(cold.reading.0, Choices::default());
        let counts = Arc::new(metrics::RequestMetrics::new());
        metrics::scope(counts.clone(), load(&f.db, &f.cache, ttl, actor.uid))
            .await
            .unwrap();
        if f.cache.enabled() {
            assert_eq!((counts.sql(), counts.redis()), (0, 1));
        } else {
            assert_eq!((counts.sql(), counts.redis()), (2, 0));
        }

        let choices = Choices {
            flat: Some(true),
            ..Choices::default()
        };
        reading::save(&f.db, &f.cache, actor, &choices, 0)
            .await
            .unwrap();
        assert_eq!(
            load(&f.db, &f.cache, ttl, actor.uid).await.unwrap().reading,
            (choices, 1, 0)
        );
        assert_eq!(
            load(&f.db, &f.cache, ttl, 72).await.unwrap().reading.0,
            Choices::default()
        );
        crate::custom_css::save(&f.db, &f.cache, actor, Some("body { color: teal; }"), 0)
            .await
            .unwrap();
        assert_eq!(
            load(&f.db, &f.cache, ttl, actor.uid)
                .await
                .unwrap()
                .reading
                .2,
            1
        );

        let disabled = Lifetimes { member: 0, ..ttl };
        let counts = Arc::new(metrics::RequestMetrics::new());
        metrics::scope(counts.clone(), load(&f.db, &f.cache, disabled, actor.uid))
            .await
            .unwrap();
        assert_eq!((counts.sql(), counts.redis()), (2, 0));

        let broken = Cache::new(&crate::config::RedisConfig {
            enabled: true,
            url: "redis://127.0.0.1:1/0".into(),
            timeout_milliseconds: 100,
            ..f.config.redis.clone()
        })
        .unwrap();
        let err = reading::save(&f.db, &broken, actor, &Choices::default(), 1)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Saved, but"));
        assert_eq!(
            reading::load_choices(&f.db, actor.uid).await.unwrap(),
            (Choices::default(), 2, 1)
        );
    }
}
