//! Title search lowers both sides with `LOWER` (docs/queries.md, "Search"): MySQL's mapping on MySQL, and on SQLite
//! the same mapping registered as `lower()` (`db/lower.rs`). These tests run on whichever backend the suite uses, so
//! the two runs together compare every code point of both backends with the one table.
use std::sync::Arc;

use gono_forum_web::{
    content,
    db::{self, lower},
    metrics::{self, RequestMetrics},
    test_support::TestFixture,
    urls::Sort,
};

/// Every code point except NUL, the comma and the surrogates, as many per statement as `CHUNK`, separated by commas
/// (which `LOWER` keeps), so a character that changed length would shift the rest and show.
#[tokio::test]
async fn lower_maps_every_code_point_as_the_table_says() {
    const CHUNK: usize = 2000;
    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    let points: Vec<char> = (1..=0x10_FFFF_u32)
        .filter_map(char::from_u32)
        .filter(|c| *c != ',')
        .collect();
    let mut differences = Vec::new();
    for chunk in points.chunks(CHUNK) {
        let text = chunk
            .iter()
            .map(char::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let lowered: String = db::query_scalar("SELECT LOWER(?)")
            .bind(&text)
            .fetch_one(db.executor())
            .await
            .unwrap();
        let got: Vec<&str> = lowered.split(',').collect();
        assert_eq!(
            got.len(),
            chunk.len(),
            "a comma was lowered into something else"
        );
        for (c, got) in chunk.iter().zip(got) {
            let expected = lower::lower_char(*c).to_string();
            if got != expected {
                differences.push(format!(
                    "U+{:04X}: database {:?}, table {:?}",
                    u32::from(*c),
                    got.chars().map(u32::from).collect::<Vec<_>>(),
                    u32::from(lower::lower_char(*c))
                ));
            }
        }
    }
    assert!(
        differences.is_empty(),
        "{} code points differ:\n{}",
        differences.len(),
        differences[..differences.len().min(40)].join("\n")
    );
    fixture.cleanup().await.unwrap();
}

/// The review's case: `İstanbul` with a dotted capital I finds `Istanbul sample` on both backends, and `ß` does not
/// find a title written with the capital sharp s `ẞ`, which MySQL leaves as it is.
#[tokio::test]
async fn title_search_matches_the_same_titles_on_both_backends() {
    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    content::seed_fixture(db).await.unwrap();
    let tids: Vec<i64> = db::query_scalar(
        "SELECT tid FROM gonof_forum_thread WHERE displayorder=0 AND visibility='public' AND deleted_at=0 ORDER BY tid LIMIT 2",
    )
    .fetch_all(db.executor())
    .await
    .unwrap();
    for (tid, subject) in tids
        .iter()
        .zip(["Istanbul sample", "Gro\u{1E9E}e Stra\u{1E9E}e"])
    {
        db::query("UPDATE gonof_forum_thread SET subject=? WHERE tid=?")
            .bind(subject)
            .bind(tid)
            .execute(db.executor())
            .await
            .unwrap();
    }
    let found = |search: &'static str| async move {
        content::list_threads(db, "", Sort::Latest, Some(search), 1, 20)
            .await
            .unwrap()
            .threads
            .iter()
            .map(|thread| thread.id as i64)
            .collect::<Vec<_>>()
    };
    // U+0130, the capital I with a dot above.
    assert_eq!(found("\u{130}stanbul").await, [tids[0]]);
    assert_eq!(found("ISTANBUL").await, [tids[0]]);
    assert_eq!(found("gro\u{1E9E}e").await, [tids[1]]);
    assert!(found("gro\u{DF}e").await.is_empty());
    fixture.cleanup().await.unwrap();
}

/// A title search with no match is answered by its capped count alone: the list statement would match the pattern
/// against the candidate titles a second time to find nothing.
#[tokio::test]
async fn an_empty_title_search_runs_one_statement() {
    let fixture = TestFixture::migrated().await.unwrap();
    let db = &fixture.db;
    content::seed_fixture(db).await.unwrap();
    let statements = |search: &'static str| async move {
        let counts = Arc::new(RequestMetrics::new());
        let page = metrics::scope(
            counts.clone(),
            content::list_threads(db, "", Sort::Latest, Some(search), 1, 20),
        )
        .await
        .unwrap();
        (page.total, page.threads.len(), counts.sql())
    };
    let (total, shown, empty) = statements("no title has this").await;
    assert_eq!((total, shown), (0, 0));
    let (total, shown, found) = statements("e").await;
    assert!(total > 0 && shown > 0);
    // Reading the readable boards comes first on both; the match adds the list (and its thumbnails).
    assert!(
        found > empty,
        "{empty} statements for no match, {found} for a match"
    );
    let baseline = {
        let counts = Arc::new(RequestMetrics::new());
        metrics::scope(counts.clone(), content::readable_forums(db, None))
            .await
            .unwrap();
        counts.sql()
    };
    assert_eq!(empty, baseline + 1, "only the capped count");
    fixture.cleanup().await.unwrap();
}
