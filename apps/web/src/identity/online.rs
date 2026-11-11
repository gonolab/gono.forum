//! The MEMORY table keeps only recent online activity; a failed write must not undo or block authentication.
use crate::db::Tx;
use std::{
    collections::{HashMap, VecDeque},
    sync::Mutex,
    time::Duration,
};

use super::{AuthSession, Identity, now};

const TOUCH_INTERVAL: i64 = 60;
/// Activity within this many seconds counts as "online"; the online table is also cleaned by it.
pub const ONLINE_WINDOW: i64 = 15 * 60;
const MAX_THROTTLE_ENTRIES: usize = 16_384;

#[derive(Default)]
pub(super) struct OnlineState {
    throttle: Mutex<Throttle>,
}

impl std::fmt::Debug for OnlineState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnlineState").finish_non_exhaustive()
    }
}

#[derive(Default)]
struct Throttle {
    last_touch: HashMap<String, i64>,
    insertion_order: VecDeque<String>,
    last_cleanup: i64,
}

impl Throttle {
    fn touch(&mut self, id: &str, timestamp: i64) -> bool {
        if self
            .last_touch
            .get(id)
            .is_some_and(|previous| timestamp.saturating_sub(*previous) < TOUCH_INTERVAL)
        {
            return false;
        }
        if !self.last_touch.contains_key(id) {
            while self.last_touch.len() >= MAX_THROTTLE_ENTRIES {
                if let Some(oldest) = self.insertion_order.pop_front() {
                    self.last_touch.remove(&oldest);
                } else {
                    break;
                }
            }
            self.insertion_order.push_back(id.to_owned());
        }
        self.last_touch.insert(id.to_owned(), timestamp);
        true
    }
}

impl Identity {
    /// Updated at most once a minute per sign-in credential per process; unauthenticated state is written to MEMORY.
    pub async fn touch_online(&self, session: &AuthSession, client_ip: &str) {
        let timestamp = now();
        let allowed = self
            .online
            .throttle
            .lock()
            .map(|mut state| state.touch(&session.online_id, timestamp))
            .unwrap_or(false);
        if !allowed {
            return;
        }
        let ip = client_ip
            .parse::<std::net::IpAddr>()
            .map(|ip| ip.to_string())
            .unwrap_or_default();
        // The current-read write predicate prevents a slow, previously authenticated
        // request from recreating an IP record after account erasure has begun.
        let update = crate::db::query("INSERT INTO gonof_common_session (sid, uid, lastactivity, client_ip) SELECT ?,uid,?,? FROM gonof_common_member WHERE uid=? AND account_state='active' AND credentials_version=? AND freeze=0 ON DUPLICATE KEY UPDATE uid = VALUES(uid), lastactivity = GREATEST(lastactivity, VALUES(lastactivity)), client_ip = VALUES(client_ip)")
            .bind(&session.online_id).bind(timestamp).bind(ip).bind(session.member.uid).bind(session.member.credentials_version)
            .execute(self.db.executor());
        if !matches!(
            tokio::time::timeout(Duration::from_secs(1), update).await,
            Ok(Ok(_))
        ) {
            tracing::warn!("online activity recording unavailable; authentication unaffected");
        }
        // Last online (docs/queries.md, "Last online"): requests write only the online table, and before the cleanup
        // job below deletes an expired session, the last activity time is written back to the InnoDB last-online
        // table. A MySQL restart loses the online table, and with it up to 15 minutes of last-online times before the
        // restart.
    }

    /// Called before the member's own data export: first writes this member's last activity time in the online table
    /// back to the last-online table, so the last-online time in the export is not missing or stale because the cleanup
    /// job has not run yet. The caller has already confirmed in the write transaction that the account is normal.
    pub(crate) async fn flush_presence(
        db: &crate::db::Database,
        tx: &mut Tx<'_>,
        uid: i64,
    ) -> anyhow::Result<()> {
        crate::db::query("INSERT INTO gonof_common_member_presence (uid, last_seen) SELECT uid,MAX(lastactivity) FROM gonof_common_session WHERE uid=? GROUP BY uid ON DUPLICATE KEY UPDATE last_seen = GREATEST(last_seen, VALUES(last_seen))")
            .bind(uid)
            .execute(db.observe(&mut **tx))
            .await?;
        Ok(())
    }

    /// Called by the per-minute background job; deletes online records older than 15 minutes.
    pub async fn cleanup_online(&self) -> anyhow::Result<()> {
        let timestamp = now();
        let allowed = self
            .online
            .throttle
            .lock()
            .map(|mut state| {
                if timestamp.saturating_sub(state.last_cleanup) < TOUCH_INTERVAL {
                    return false;
                }
                state.last_cleanup = timestamp;
                true
            })
            .unwrap_or(false);
        if !allowed {
            return Ok(());
        }
        // Take the oldest 1000 expired sessions first, write back the last activity time for this batch only, then
        // delete by sid, so a backlog over 1000 is not rewritten every round and each statement's row count is bounded.
        // Only members whose account is normal are written (no write-back once account deletion has started);
        // GREATEST guarantees it only moves forward under concurrent processes, and repeated write-backs are harmless;
        // the member table is not written: the member row lock of posting transactions should not be disturbed by
        // per-minute activity. The delete checks expiry once more: sessions active between the two statements are
        // kept and written back in the next round.
        let expired = timestamp.saturating_sub(ONLINE_WINDOW);
        let sids: Vec<String> = tokio::time::timeout(
            Duration::from_secs(2),
            crate::db::query_scalar(
                "SELECT sid FROM gonof_common_session WHERE lastactivity < ? ORDER BY lastactivity LIMIT 1000",
            )
            .bind(expired)
            .fetch_all(self.db.executor()),
        )
        .await
        .map_err(|_| anyhow::anyhow!("online record cleanup timed out"))??;
        if sids.is_empty() {
            return Ok(());
        }
        let marks = vec!["?"; sids.len()].join(",");
        let sql = format!(
            "INSERT INTO gonof_common_member_presence (uid, last_seen) SELECT s.uid,MAX(s.lastactivity) FROM gonof_common_session s JOIN gonof_common_member m ON m.uid=s.uid WHERE s.lastactivity < ? AND s.sid IN ({marks}) AND m.account_state='active' GROUP BY s.uid ON DUPLICATE KEY UPDATE last_seen = GREATEST(last_seen, VALUES(last_seen))"
        );
        let mut write_back = crate::db::query(&sql).bind(expired);
        for sid in &sids {
            write_back = write_back.bind(sid.as_str());
        }
        tokio::time::timeout(
            Duration::from_secs(2),
            write_back.execute(self.db.executor()),
        )
        .await
        .map_err(|_| anyhow::anyhow!("writing back last-seen times timed out"))??;
        let sql =
            format!("DELETE FROM gonof_common_session WHERE lastactivity < ? AND sid IN ({marks})");
        let mut delete = crate::db::query(&sql).bind(expired);
        for sid in &sids {
            delete = delete.bind(sid.as_str());
        }
        tokio::time::timeout(Duration::from_secs(2), delete.execute(self.db.executor()))
            .await
            .map_err(|_| anyhow::anyhow!("online record cleanup timed out"))??;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn online_writes_are_throttled_and_process_memory_is_bounded() {
        let mut throttle = Throttle::default();
        assert!(throttle.touch("same-login", 100));
        assert!(!throttle.touch("same-login", 159));
        assert!(throttle.touch("same-login", 160));
        for id in 0..MAX_THROTTLE_ENTRIES + 5 {
            assert!(throttle.touch(&id.to_string(), 200));
        }
        assert_eq!(throttle.last_touch.len(), MAX_THROTTLE_ENTRIES);
        assert_eq!(throttle.insertion_order.len(), MAX_THROTTLE_ENTRIES);
        assert!(!throttle.last_touch.contains_key("same-login"));
    }
}
