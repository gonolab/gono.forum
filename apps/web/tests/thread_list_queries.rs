//! Thread list queries (docs/queries.md, "Checking consistency"): threads that are not listed have a negative
//! `displayorder`, forums are excluded by forum ids given by code, anonymous visitors are filtered by thread
//! visibility, totals read the count table, one single-table query, at most 1000 pages.
//!
//! The main test is a differential check: build a data set covering all kinds of readable states, pins, highlights,
//! categories, blocking and exclusions, compute the reference result in code by the original rules (the raw columns
//! of the forum table and thread rows), and compare it with the list page by page and with the totals; also check that
//! no statement reading in index order has a filesort.
use std::sync::Arc;

use gono_forum_web::{
    content::{self, ListScope, ListViewer, MAX_LIST_PAGES, SidebarParams},
    db::{self, Database},
    debug::trace::{self, QuerySource, RequestProfile},
    test_support::TestFixture,
    urls::Sort,
};

const VIEWER: i64 = 1;
const BLOCKED: i64 = 3;
const PAGE_SIZE: usize = 7;
const SIDEBAR: SidebarParams = SidebarParams {
    hot_days: 7,
    hot_count: 10,
    latest_count: 10,
    offset_minutes: 0,
};

/// Forums: `(fid, parent_fid, slug, visibility, status)`.
const FORUMS: &[(i64, Option<i64>, &str, &str, i64)] = &[
    (1, None, "alpha", "public", 1),
    (2, Some(1), "alpha-a", "public", 1),
    (3, Some(1), "alpha-b", "members", 1),
    (4, None, "beta", "members", 1),
    (5, None, "gamma", "public", 0),
    (6, None, "delta", "private", 1),
    (7, Some(4), "beta-a", "public", 1),
    (8, None, "omega", "public", 1),
    (9, Some(8), "omega-a", "public", 0),
];

#[derive(Clone, Debug)]
struct Row {
    tid: i64,
    fid: i64,
    authorid: Option<i64>,
    subject: String,
    dateline: i64,
    lastpost: i64,
    /// The pin level written to the database: -1 when the first post is hidden, -2 for reserved states, otherwise
    /// 0–3.
    displayorder: i64,
    digest: i64,
    category: i64,
    replies: i64,
    visibility: &'static str,
}

/// Deterministic pseudo-random numbers, so the same data is built every time.
struct Rng(u64);

impl Rng {
    fn next(&mut self, n: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) % n
    }
}

fn pick<T: Copy>(rng: &mut Rng, weighted: &[(u64, T)]) -> T {
    let total: u64 = weighted.iter().map(|(w, _)| w).sum();
    let mut roll = rng.next(total);
    for (weight, value) in weighted {
        if roll < *weight {
            return *value;
        }
        roll -= weight;
    }
    unreachable!()
}

fn rows(now: i64) -> Vec<(Row, &'static str, i64)> {
    let mut rng = Rng(20261106);
    (0..180)
        .map(|i| {
            let fid = pick(
                &mut rng,
                &[
                    (5, 1),
                    (4, 2),
                    (3, 3),
                    (3, 4),
                    (1, 5),
                    (1, 6),
                    (2, 7),
                    (3, 8),
                    (1, 9),
                ],
            );
            // Times are the same in clusters, so sorting relies on tid as the tiebreaker; half of the threads have
            // replies in the last few days, so hot has something to sort.
            let dateline = 1_700_000_000 + i64::try_from(rng.next(40)).unwrap() * 1000;
            let lastpost = if rng.next(2) == 0 {
                now - i64::try_from(rng.next(10)).unwrap() * 86_400 - 3600
            } else {
                dateline + i64::try_from(rng.next(4)).unwrap() * 500
            };
            let pinned = pick(&mut rng, &[(85, 0), (5, 1), (4, 2), (3, 3)]);
            let hidden = rng.next(20) == 0;
            let visibility = pick(&mut rng, &[(80, "public"), (15, "members"), (5, "private")]);
            let state = pick(&mut rng, &[(95, "published"), (5, "draft")]);
            let readperm = pick(&mut rng, &[(97, 0), (3, 1)]);
            let reserved = state != "published" || visibility == "private" || readperm != 0;
            let displayorder = if hidden {
                -1
            } else if reserved {
                -2
            } else {
                pinned
            };
            (
                Row {
                    tid: 1000 + i,
                    fid,
                    authorid: pick(
                        &mut rng,
                        &[(3, Some(2)), (2, Some(BLOCKED)), (3, Some(4)), (1, None)],
                    ),
                    subject: if rng.next(5) == 0 {
                        format!("Key Topic {i}")
                    } else {
                        format!("Plain {i}")
                    },
                    dateline,
                    lastpost,
                    displayorder,
                    digest: pick(&mut rng, &[(85, 0), (15, 1)]),
                    category: i64::try_from(rng.next(6)).unwrap(),
                    replies: i64::try_from(rng.next(4)).unwrap()
                        * i64::try_from(rng.next(8)).unwrap(),
                    visibility,
                },
                state,
                readperm,
            )
        })
        .collect()
}

async fn execute(db: &Database, sql: &str) {
    db::query(sql).execute(db.executor()).await.unwrap();
}

/// Forums and threads are written with raw SQL; the forum copy on thread rows (`family_fid`) gets a placeholder value
/// first and is then rewritten by the official copy code (`sync_forum_copies`);
/// the count table and reserved states are recomputed by the listing rules (`refresh_listing`).
async fn seed(db: &Database, now: i64) -> Vec<Row> {
    for uid in 1..=4 {
        execute(
            db,
            &format!(
                "INSERT INTO gonof_common_member(uid,username,regdate) VALUES({uid},'lister{uid}',1)"
            ),
        )
        .await;
    }
    execute(
        db,
        &format!("INSERT INTO gonof_home_member_block(uid,blocked_uid,dateline) VALUES({VIEWER},{BLOCKED},1)"),
    )
    .await;
    for (fid, parent, slug, visibility, status) in FORUMS {
        let parent = parent.map_or("NULL".to_owned(), |p| p.to_string());
        execute(
            db,
            &format!(
                "INSERT INTO gonof_forum_forum(fid,parent_fid,slug,name,description,guidance,displayorder,status,visibility) VALUES({fid},{parent},'{slug}','{slug}','','',{fid},{status},'{visibility}')"
            ),
        )
        .await;
    }
    let rows = rows(now);
    for (row, state, readperm) in &rows {
        let author = row.authorid.map_or("NULL".to_owned(), |a| a.to_string());
        execute(
            db,
            &format!(
                "INSERT INTO gonof_forum_thread(tid,fid,authorid,subject,dateline,lastpost,displayorder,digest,category,replies,visibility,publication_state,readperm,family_fid)
                 VALUES({},{},{author},'{}',{},{},{},{},{},{},'{}','{state}',{readperm},0)",
                row.tid, row.fid, row.subject, row.dateline, row.lastpost, row.displayorder, row.digest,
                row.category, row.replies, row.visibility
            ),
        )
        .await;
    }
    for (fid, parent, ..) in FORUMS {
        if parent.is_none() {
            gono_forum_web::test_support::sync_forum_copies(db, *fid)
                .await
                .unwrap();
        }
    }
    gono_forum_web::test_support::refresh_listing(db)
        .await
        .unwrap();
    rows.into_iter().map(|(row, ..)| row).collect()
}

/// Reference rule: computes from the forum table the forums this visitor can read (taking the stricter visibility of
/// this level and the parent level; enabled only when both levels are enabled).
fn readable_forums(member: bool) -> Vec<i64> {
    let rank = |v: &str| {
        ["public", "members", "private"]
            .iter()
            .position(|x| *x == v)
            .unwrap()
    };
    let limit = usize::from(member);
    FORUMS
        .iter()
        .filter(|forum| {
            let parent = forum.1.map(|p| FORUMS.iter().find(|f| f.0 == p).unwrap());
            forum.4 == 1
                && parent.is_none_or(|p| p.4 == 1)
                && rank(forum.3).max(parent.map_or(0, |p| rank(p.3))) <= limit
        })
        .map(|forum| forum.0)
        .collect()
}

fn in_scope(row: &Row, scope: &ListScope) -> bool {
    let family = FORUMS
        .iter()
        .find(|f| f.0 == row.fid)
        .unwrap()
        .1
        .unwrap_or(row.fid);
    match scope {
        ListScope::All => true,
        ListScope::Family(fid) => family == *fid,
        ListScope::Forum(fid) => row.fid == *fid,
    }
}

struct Case<'a> {
    viewer: ListViewer<'a>,
    scope: ListScope,
    sort: Sort,
    category: Option<i64>,
    search: Option<&'a str>,
}

/// Reference result: listable (not negative), allowed by thread visibility, forum in the list this visitor may see (a
/// single forum's page is confirmed readable by the route first), within scope, author not blocked; then sorted and
/// filtered.
fn expected(rows: &[Row], case: &Case<'_>) -> Vec<i64> {
    let member = case.viewer.uid.is_some();
    let mut found: Vec<&Row> = rows
        .iter()
        .filter(|row| row.displayorder >= 0)
        .filter(|row| row.visibility == "public" || (member && row.visibility == "members"))
        .filter(|row| {
            matches!(case.scope, ListScope::Forum(_)) || case.viewer.forums.contains(&row.fid)
        })
        .filter(|row| in_scope(row, &case.scope))
        .filter(|row| {
            row.authorid
                .is_none_or(|a| !case.viewer.blocked_uids.contains(&a))
        })
        .filter(|row| match case.sort {
            Sort::Pinned => row.displayorder > 0,
            Sort::Featured => row.digest > 0,
            _ => true,
        })
        .filter(|row| case.category.is_none_or(|c| row.category == c))
        .filter(|row| {
            case.search
                .is_none_or(|q| row.subject.to_lowercase().contains(&q.to_lowercase()))
        })
        .collect();
    let time = |row: &Row| {
        if case.sort == Sort::Created {
            row.dateline
        } else {
            row.lastpost
        }
    };
    found.sort_by_key(|row| std::cmp::Reverse((row.displayorder, time(row), row.tid)));
    found.iter().map(|row| row.tid).collect()
}

/// No list statement in this profile that reads in index order should have a filesort; hot sorts after locating by
/// design and does not count. Apart from search's capped count, none have UNION or nested queries.
async fn assert_index_order(db: &Database, profile: &RequestProfile) {
    // The debug record holds the text actually executed: on SQLite the rewriter removed FORCE INDEX, so list
    // statements are recognized by the thread table's alias.
    let sqlite = db.backend() == gono_forum_web::db::Backend::Sqlite;
    let statements: Vec<_> = profile
        .snapshot()
        .into_iter()
        .filter(|r| {
            r.source == QuerySource::Application
                && if sqlite {
                    r.sql.contains("FROM gonof_forum_thread t ")
                } else {
                    r.sql.contains("FORCE INDEX")
                }
        })
        .collect();
    assert!(!statements.is_empty());
    for record in statements {
        let searching = record.sql.contains("LIKE");
        assert!(
            !record.sql.contains("UNION") && (searching || !record.sql.contains("(SELECT")),
            "lists, counts and the sidebar have no UNION or nested query: {}",
            record.sql
        );
        // Hot sorts by reply count over this few days' active threads, and by design does not follow index order.
        if record.sql.contains("gonof_thread_global_active")
            || record.sql.contains("ORDER BY t.replies DESC")
        {
            continue;
        }
        if sqlite {
            let plan = gono_forum_web::test_support::sqlite_plan(db, &record.expanded_sql)
                .await
                .unwrap();
            gono_forum_web::test_support::assert_sqlite_index_order(&plan, &record.sql);
            continue;
        }
        let plan = db::query(&format!("EXPLAIN {}", record.expanded_sql))
            .fetch_all(db.executor())
            .await
            .unwrap();
        for row in plan {
            // SQLx only gets the column names of EXPLAIN at execution time, so looking up by name fails; convert to
            // indexes first.
            let text = |column: &str| {
                let ordinal = row
                    .columns()
                    .iter()
                    .position(|c| c.name() == column)
                    .unwrap();
                row.try_get::<Option<Vec<u8>>, _>(ordinal)
                    .unwrap()
                    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                    .unwrap_or_default()
            };
            if text("table") == "t" {
                assert!(
                    !text("Extra").contains("filesort"),
                    "the thread table must be read in index order: {}\n{}",
                    record.sql,
                    text("Extra")
                );
            }
        }
    }
}

#[tokio::test]
async fn lists_match_the_reference_rules_page_by_page_in_index_order() {
    let f = TestFixture::migrated().await.unwrap();
    let db = &f.db;
    let now = gono_forum_web::identity::now();
    let rows = seed(db, now).await;
    let blocked = content::blocked_authors(db, VIEWER).await.unwrap();
    assert_eq!(blocked, vec![BLOCKED]);
    let anonymous_forums = readable_forums(false);
    let member_forums = readable_forums(true);
    let excluded: Vec<i64> = member_forums
        .iter()
        .copied()
        .filter(|fid| ![2, 7].contains(fid))
        .collect();
    assert_eq!(
        content::anonymous_forums(db).await.unwrap(),
        anonymous_forums,
        "the reference rule in the test and the forum cache give the same forums"
    );
    let viewers = [
        ListViewer {
            forums: &anonymous_forums,
            ..ListViewer::default()
        },
        ListViewer {
            uid: Some(VIEWER),
            forums: &member_forums,
            blocked_uids: &blocked,
        },
        ListViewer {
            uid: Some(VIEWER),
            forums: &excluded,
            blocked_uids: &blocked,
        },
    ];
    let scopes = [
        ListScope::All,
        ListScope::Family(1),
        ListScope::Family(4),
        ListScope::Family(8),
        ListScope::Forum(2),
        ListScope::Forum(3),
        ListScope::Forum(7),
    ];
    let filters: [(Sort, Option<i64>, Option<&str>); 7] = [
        (Sort::Latest, None, None),
        (Sort::Created, None, None),
        (Sort::Pinned, None, None),
        (Sort::Featured, None, None),
        (Sort::Latest, Some(2), None),
        (Sort::Created, Some(4), None),
        (Sort::Latest, None, Some("key topic")),
    ];
    let profile = Arc::new(RequestProfile::new());
    let mut nonempty = 0;
    for viewer in viewers {
        for scope in &scopes {
            // A single forum's page is confirmed readable for this visitor by the route first; unreadable ones are not
            // queried here.
            if let ListScope::Forum(fid) = scope
                && !readable_forums(viewer.uid.is_some()).contains(fid)
            {
                continue;
            }
            // A top-level forum page passes only this family's forums (the same family_fids as the route).
            let family: Vec<i64> = match scope {
                ListScope::Family(top) => viewer
                    .forums
                    .iter()
                    .copied()
                    .filter(|fid| {
                        FORUMS
                            .iter()
                            .any(|f| f.0 == *fid && f.1.unwrap_or(f.0) == *top)
                    })
                    .collect(),
                _ => viewer.forums.to_vec(),
            };
            let viewer = ListViewer {
                forums: &family,
                ..viewer
            };
            for (sort, category, search) in filters {
                let case = Case {
                    viewer,
                    scope: scope.clone(),
                    sort,
                    category,
                    search,
                };
                let want = expected(&rows, &case);
                nonempty += usize::from(want.len() > PAGE_SIZE);
                let pages = want.len().div_ceil(PAGE_SIZE).max(1);
                let mut seen = Vec::new();
                for page in 1..=pages + 1 {
                    let listed = trace::scope(
                        profile.clone(),
                        Box::pin(content::list_scope_for_viewer(
                            db, scope, sort, search, category, page, PAGE_SIZE, viewer,
                        )),
                    )
                    .await
                    .unwrap();
                    assert_eq!(
                        listed.total,
                        want.len(),
                        "total (count table) {scope:?} {sort:?} {category:?} {search:?} {viewer:?}"
                    );
                    if page > pages {
                        assert!(
                            listed.threads.is_empty(),
                            "a page past the last one is empty"
                        );
                    }
                    seen.extend(listed.threads.iter().map(|t| t.id as i64));
                }
                assert_eq!(
                    seen, want,
                    "{scope:?} {sort:?} {category:?} {search:?} {viewer:?}"
                );
            }
        }
    }
    assert!(
        nonempty > 30,
        "most combinations must have more than one page: {nonempty}"
    );
    assert_index_order(db, &profile).await;

    // The two right-column cards: hot is threads with replies in the last 7 days by reply count, latest is the first
    // 10 by posting time.
    let since = now - SIDEBAR.hot_days * 86_400;
    let hot = |viewer: &ListViewer<'_>| {
        let case = Case {
            viewer: *viewer,
            scope: ListScope::All,
            sort: Sort::Latest,
            category: None,
            search: None,
        };
        let readable: Vec<i64> = expected(&rows, &case);
        let mut found: Vec<&Row> = rows
            .iter()
            .filter(|row| readable.contains(&row.tid) && row.lastpost >= since && row.replies > 0)
            .collect();
        found.sort_by_key(|row| std::cmp::Reverse((row.replies, row.tid)));
        found.iter().take(10).map(|row| row.tid).collect::<Vec<_>>()
    };
    let latest = |viewer: &ListViewer<'_>| {
        let case = Case {
            viewer: *viewer,
            scope: ListScope::All,
            sort: Sort::Created,
            category: None,
            search: None,
        };
        expected(&rows, &case)
            .into_iter()
            .take(10)
            .collect::<Vec<_>>()
    };
    let anonymous = content::sidebar_at(db, u64::try_from(now).unwrap(), SIDEBAR)
        .await
        .unwrap();
    let ids = |threads: &[content::Thread]| threads.iter().map(|t| t.id as i64).collect::<Vec<_>>();
    assert_eq!(ids(&anonymous.hot), hot(&viewers[0]));
    assert_eq!(ids(&anonymous.latest), latest(&viewers[0]));
    assert!(!anonymous.hot.is_empty());
    for viewer in &viewers[1..] {
        let (hot_cards, latest_cards) = content::sidebar_topics_for_viewer(db, *viewer, SIDEBAR)
            .await
            .unwrap();
        assert_eq!(ids(&hot_cards), hot(viewer), "{viewer:?}");
        assert_eq!(ids(&latest_cards), latest(viewer), "{viewer:?}");
    }
    f.cleanup().await.unwrap();
}

/// Page limit: totals count up to at most 1000 pages; page 1000 works as usual and page 1001 is an empty page (the
/// page returns 404 based on that).
#[tokio::test]
async fn list_totals_stop_at_the_page_cap() {
    let f = TestFixture::migrated().await.unwrap();
    let db = &f.db;
    execute(
        db,
        "INSERT INTO gonof_forum_forum(fid,slug,name,description,guidance) VALUES(1,'capped','capped','','')",
    )
    .await;
    // 1003 threads, with titles built in code and 500 rows inserted per statement (the same statement on both
    // backends).
    let numbers: Vec<i64> = (1..=1003).collect();
    for chunk in numbers.chunks(500) {
        let sql = format!(
            "INSERT INTO gonof_forum_thread(fid,subject,dateline,lastpost,family_fid) VALUES {}",
            vec!["(1,?,?,?,1)"; chunk.len()].join(",")
        );
        let mut insert = db::query(&sql);
        for n in chunk {
            insert = insert.bind(format!("Thread {n}")).bind(*n).bind(*n);
        }
        insert.execute(db.executor()).await.unwrap();
    }
    gono_forum_web::test_support::refresh_listing(db)
        .await
        .unwrap();
    assert_eq!(MAX_LIST_PAGES, 1000);
    let forums = [1];
    for scope in [ListScope::All, ListScope::Family(1), ListScope::Forum(1)] {
        let page = |page: usize| {
            content::list_scope_for_viewer(
                db,
                &scope,
                Sort::Latest,
                None,
                None,
                page,
                1,
                ListViewer {
                    forums: &forums,
                    ..ListViewer::default()
                },
            )
        };
        let first = page(1).await.unwrap();
        assert_eq!(first.total, 1000, "{scope:?}");
        assert_eq!(first.threads[0].title, "Thread 1003");
        let last = page(1000).await.unwrap();
        assert_eq!(last.threads.len(), 1);
        assert_eq!(last.threads[0].title, "Thread 4");
        let beyond = page(1001).await.unwrap();
        assert!(beyond.threads.is_empty());
        assert_eq!(beyond.total, 1000);
    }
    // With 2 per page the limit is 2000 rows, so all 1003 are counted.
    let all = content::list_threads(db, "", Sort::Latest, None, 1, 2)
        .await
        .unwrap();
    assert_eq!(all.total, 1003);
    f.cleanup().await.unwrap();
}
