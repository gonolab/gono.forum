//! Thread detail page queries (docs/queries.md, "Thread page" and "Checking consistency"): the like, downvote,
//! favorite and tip count columns are maintained through the regular write paths and match the details and
//! ledger (check scripts all 0); the thread page reads these columns with no nested queries or UNION, no GROUP BY over
//! like details, no counting over favorite details or tip ledger entries, no activity table query for ordinary threads,
//! the reader's 5 states read in one query, and moderator checks without database queries.
#[path = "support/http_auth.rs"]
mod http_auth;

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use gono_forum_web::{
    activities, content,
    db::{self, Database},
    debug::trace::{self, QuerySource, RequestProfile},
    identity::{self, Actor},
    interaction::Interaction,
    router,
};
use http_body_util::BodyExt;
use tower::ServiceExt;

const TID: i64 = 103;
const NOW: i64 = 1_790_000_000;

fn actor(uid: i64) -> Actor {
    Actor {
        uid,
        credentials_version: 1,
        api_key: None,
    }
}

async fn scalar(db: &Database, sql: &str) -> i64 {
    db::query_scalar(sql)
        .fetch_one(db.executor())
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
}

async fn page(app: &Router, path: &str, session: Option<&str>) -> (StatusCode, String) {
    let mut request = Request::builder().uri(path);
    if let Some(session) = session {
        request = request.header(header::COOKIE, format!("gonof_session={session}"));
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

/// The application SQL in one thread page request.
async fn statements(app: &Router, path: &str, session: Option<&str>) -> Vec<String> {
    let profile = Arc::new(RequestProfile::new());
    let (status, _) = trace::scope(profile.clone(), Box::pin(page(app, path, session))).await;
    assert_eq!(status, StatusCode::OK, "{path}");
    profile
        .snapshot()
        .into_iter()
        .filter(|record| record.source == QuerySource::Application)
        .map(|record| record.sql)
        .collect()
}

#[tokio::test]
async fn counters_follow_marks_and_the_thread_page_reads_them_in_simple_queries() {
    let services = gono_forum_web::test_support::TestFixture::migrated()
        .await
        .unwrap();
    let db = services.db.clone();
    content::seed_forums(&db).await.unwrap();
    content::seed_fixture(&db).await.unwrap();
    // 9100 is an administrator (gives readers credits), 9101–9104 are ordinary members with verified emails; none is
    // the author of thread 103.
    for uid in 9100_i64..=9104 {
        db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(?,?,'synthetic-hash',?,1,?,?)")
            .bind(uid)
            .bind(format!("thread-page-{uid}"))
            .bind(format!("thread-page-{uid}@example.test"))
            .bind(i64::from(uid == 9100))
            .bind(NOW - 172_800)
            .execute(db.executor())
            .await
            .unwrap();
    }
    let first = scalar(
        &db,
        &format!("SELECT pid FROM gonof_forum_post WHERE tid={TID} AND first=1"),
    )
    .await;
    let reply = scalar(
        &db,
        &format!("SELECT pid FROM gonof_forum_post WHERE tid={TID} AND first=0 AND root_pid IS NULL ORDER BY position LIMIT 1"),
    )
    .await;
    let key = identity::random_token;

    // Through the regular write paths: like, duplicate like, unlike, like after downvoting (removing the downvote),
    // favorite and unfavorite, tip.
    let service = Interaction::new(db.clone());
    service
        .set_reaction(actor(9101), reply, true, &key())
        .await
        .unwrap();
    service
        .set_reaction(actor(9102), reply, true, &key())
        .await
        .unwrap();
    service
        .set_reaction(actor(9102), reply, true, &key())
        .await
        .unwrap();
    service
        .set_reaction(actor(9103), reply, true, &key())
        .await
        .unwrap();
    service
        .set_reaction(actor(9103), reply, false, &key())
        .await
        .unwrap();
    service
        .set_downvote(actor(9103), first, true, &key())
        .await
        .unwrap();
    service
        .set_reaction(actor(9103), first, true, &key())
        .await
        .unwrap();
    service
        .set_downvote(actor(9104), first, true, &key())
        .await
        .unwrap();
    service
        .set_downvote(actor(9104), first, true, &key())
        .await
        .unwrap();
    service
        .set_favorite(actor(9101), TID, true, &key())
        .await
        .unwrap();
    service
        .set_favorite(actor(9102), TID, true, &key())
        .await
        .unwrap();
    service
        .set_favorite(actor(9102), TID, false, &key())
        .await
        .unwrap();
    service
        .set_favorite(actor(9103), TID, true, &key())
        .await
        .unwrap();
    for uid in [9101, 9102] {
        activities::adjust(&db, actor(9100), uid, 100, &key(), "Test tip budget", NOW)
            .await
            .unwrap();
    }
    activities::tip(&db, actor(9101), TID, 10, &key(), NOW)
        .await
        .unwrap();
    activities::tip(&db, actor(9102), TID, 5, &key(), NOW)
        .await
        .unwrap();
    assert!(
        activities::tip(&db, actor(9101), TID, 10, &key(), NOW)
            .await
            .is_err(),
        "each member tips a thread only once"
    );

    let post = |pid: i64, column: &'static str| {
        let db = db.clone();
        async move {
            scalar(
                &db,
                &format!("SELECT {column} FROM gonof_forum_post WHERE pid={pid}"),
            )
            .await
        }
    };
    assert_eq!(post(reply, "reactions").await, 2);
    assert_eq!(post(first, "reactions").await, 1);
    assert_eq!(
        post(first, "downvotes").await,
        1,
        "the like removed the member's own dislike"
    );
    let thread = db::query(&format!(
        "SELECT favorites,tips,tip_total FROM gonof_forum_thread WHERE tid={TID}"
    ))
    .fetch_one(db.executor())
    .await
    .unwrap();
    assert_eq!(
        (
            thread.try_get::<i64, _>("favorites").unwrap(),
            thread.try_get::<i64, _>("tips").unwrap(),
            thread.try_get::<i64, _>("tip_total").unwrap()
        ),
        (2, 2, 15)
    );
    let mut conn = db.pool.acquire().await.unwrap();
    let drift = gono_forum_web::test_support::content_drift(&mut conn)
        .await
        .unwrap();
    drop(conn);
    assert!(
        drift.iter().all(|(_, amount)| *amount == 0),
        "counters agree with the rows and the ledger: {drift:?}"
    );

    // Thread page: viewed once as a member and once anonymously.
    let identity = identity::Identity::new(
        db.clone(),
        services.config.identity.clone(),
        services.root.clone(),
        gono_forum_web::test_support::detached_global(),
    );
    let session = http_auth::login(&identity, &db, 9101).await.session_token;
    let app = router::build(services.config.clone(), &services.root, db.clone());
    let path = format!("/thread-{TID}");
    let (status, html) = page(&app, &path, Some(&session)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        html.contains("data-favorite-count>2<"),
        "the favorite count reads the counter column"
    );
    assert!(
        html.contains("data-reaction-count>2<"),
        "the like count reads the counter column"
    );
    for (who, session) in [("member", Some(session.as_str())), ("anonymous", None)] {
        // The first pass warms the process and Redis caches; the second is the usual state.
        statements(&app, &path, session).await;
        let sql = statements(&app, &path, session).await;
        let listed = sql.join("\n");
        for statement in &sql {
            assert!(
                !statement.contains("UNION")
                    && !statement.contains("(SELECT")
                    && !statement.contains("EXISTS"),
                "{who}: the thread page has no UNION or nested query: {statement}"
            );
            assert!(
                !(statement.contains("gonof_forum_post_reaction")
                    && statement.contains("GROUP BY")),
                "{who}: the like count reads the counter column: {statement}"
            );
            assert!(
                !statement.contains("COUNT(*) FROM gonof_home_favorite")
                    && !statement.contains("COUNT(*) FROM gonof_forum_post_downvote")
                    && !statement.contains("COUNT(*) AS tips"),
                "{who}: favorites, dislikes and tips read the counter columns: {statement}"
            );
            assert!(
                !statement.contains("gonof_forum_activity"),
                "{who}: a plain thread does not query the activity table: {statement}"
            );
            assert!(
                !statement.contains("gonof_forum_moderator"),
                "{who}: moderator checks use the in-process roster: {statement}"
            );
        }
        let state_queries = sql
            .iter()
            .filter(|statement| statement.contains("LEFT JOIN gonof_home_favorite fav"))
            .count();
        assert_eq!(
            state_queries,
            usize::from(who == "member"),
            "{who}: the reader state is read in one query\n{listed}"
        );
        assert!(
            sql.len() <= if who == "member" { 12 } else { 8 },
            "{who}: number of thread page queries\n{listed}"
        );
    }
    // A member who has not tipped yet is offered a tip once the thread page has also checked that the author has no
    // account deletion or content erasure pending: one more statement, within the same budget.
    let session = http_auth::login(&identity, &db, 9103).await.session_token;
    statements(&app, &path, Some(&session)).await;
    let sql = statements(&app, &path, Some(&session)).await;
    let listed = sql.join("\n");
    assert_eq!(
        sql.iter()
            .filter(|statement| statement.contains("FROM gonof_common_data_request"))
            .count(),
        1,
        "{listed}"
    );
    assert!(
        sql.len() <= 12,
        "member who may tip: number of thread page queries\n{listed}"
    );
    services.cleanup().await.unwrap();
}
