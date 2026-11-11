//! The project uses no triggers (docs/architecture.md, "Data"): after running all migrations, the project database
//! must contain not a single trigger.
//!
//! Creating triggers, routines, events and `DROP TRIGGER` are already blocked at the source level for all migrations
//! by the unit tests in `db::migrations`; this actually queries the database once more, to catch forms the detector
//! misses. Triggers that tests create for fault injection (`test_support::fail_when`) live in each case's own
//! temporary database, not in the database this case creates, so they do not interfere.

use gono_forum_web::{db, test_support::TestFixture};

#[tokio::test]
async fn migrated_schema_has_no_triggers() {
    let fixture = TestFixture::migrated().await.unwrap();
    // SQLite registers triggers in sqlite_master.
    let remaining: Vec<String> = db::query(
        "SELECT TRIGGER_NAME FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA=DATABASE() ORDER BY TRIGGER_NAME",
    )
    .sqlite("SELECT name AS TRIGGER_NAME FROM sqlite_master WHERE type='trigger' ORDER BY name")
    .fetch_all(fixture.db.executor())
    .await
    .unwrap()
    .iter()
    .map(|row| db::text(row, "TRIGGER_NAME").unwrap())
    .collect();
    assert!(
        remaining.is_empty(),
        "triggers remain after all migrations (a drop migration is missing, or a trigger was added): {remaining:?}"
    );
    fixture.cleanup().await.unwrap();
}
