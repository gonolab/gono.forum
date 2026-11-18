//! Board structure and its edges: merge jobs that overlap or meet a new child board, promoting and creating
//! top-level boards on a multi-domain site (domain links, versions and the top-level limit), subsite paths against
//! board page addresses, the thread move form, and the board icon picker.
#[path = "support/site.rs"]
mod site;

use axum::http::StatusCode;
use gono_forum_web::{
    content::writes::{ContentWriteError, ContentWriter, NewThread, WriteConfig},
    db,
    domains::DomainMap,
    identity::{self, Actor},
    organization::{self, ForumInput, MergeReport},
    test_support::{TEST_SITE_URL, raise_posting_quotas},
};
use site::{Answer, Site};

async fn admin(site: &Site) -> Actor {
    let row = db::query(
        "SELECT uid,credentials_version FROM gonof_common_member WHERE username='tester-1'",
    )
    .fetch_one(site.db().executor())
    .await
    .unwrap();
    Actor {
        uid: row.try_get("uid").unwrap(),
        credentials_version: row.try_get("credentials_version").unwrap(),
        api_key: None,
    }
}

async fn version(site: &Site, fid: i64) -> i64 {
    site.scalar(&format!(
        "SELECT version FROM gonof_forum_forum WHERE fid={fid}"
    ))
    .await
}

async fn thread(site: &Site, author: i64, fid: i64, title: &str) -> i64 {
    raise_posting_quotas(site.db()).await.unwrap();
    ContentWriter::new(site.db().clone(), WriteConfig::default())
        .publish_at(
            author,
            NewThread {
                visibility: Default::default(),
                activity: Default::default(),
                fid,
                title: title.into(),
                body: format!("Body of {title}"),
                attachment_ids: vec![],
                request_key: identity::random_token(),
            },
            identity::now() - 60,
        )
        .await
        .unwrap()
        .tid
}

/// The board's stored fields as a save form, so a test changes only what it means to.
async fn input(site: &Site, fid: i64) -> ForumInput {
    let row = db::query("SELECT * FROM gonof_forum_forum WHERE fid=?")
        .bind(fid)
        .fetch_one(site.db().executor())
        .await
        .unwrap();
    ForumInput {
        fid,
        parent_fid: row
            .try_get::<Option<i64>, _>("parent_fid")
            .unwrap()
            .unwrap_or(0),
        name: row.try_get("name").unwrap(),
        slug: row.try_get("slug").unwrap(),
        description: row.try_get("description").unwrap(),
        guidance: row.try_get("guidance").unwrap(),
        displayorder: row.try_get("displayorder").unwrap(),
        status: row.try_get("status").unwrap(),
        visibility: db::text(&row, "visibility").unwrap(),
        version: row.try_get("version").unwrap(),
        ..Default::default()
    }
}

/// A merge into board B and a merge of B into C must not overlap: while one job still fills B, merging B away is
/// refused; a B-to-C job queued anyway (before that rule, or by an older process) waits until the threads have all
/// arrived, so none of them is left behind in the disabled B.
#[tokio::test]
async fn a_board_still_being_filled_is_merged_away_only_after_its_job() {
    let site = Site::new(false).await;
    let db = site.db();
    let admin = admin(&site).await;
    let (a, b, c) = (
        site.fid("releases").await,
        site.fid("daily").await,
        site.fid("hosting").await,
    );
    let from_a = thread(&site, admin.uid, a, "Started in A").await;
    let from_b = thread(&site, admin.uid, b, "Started in B").await;
    let first = organization::request_merge(db, admin, a, b, version(&site, a).await, "A into B")
        .await
        .unwrap();
    assert!(matches!(
        organization::request_merge(db, admin, b, c, version(&site, b).await, "B into C").await,
        Err(ContentWriteError::Conflict(message)) if message.contains("still moving threads into this forum")
    ));

    let now = identity::now();
    let payload =
        serde_json::json!({"source": b, "target": c, "uid": admin.uid, "reason": "B into C"});
    let older = db::query("INSERT INTO gonof_common_outbox(event_key,event_type,aggregate_type,aggregate_id,payload,dateline,available_at) VALUES(?,?,'forum',?,?,?,?)")
        .bind(format!("{}:{b}:older", organization::MERGE_EVENT))
        .bind(organization::MERGE_EVENT)
        .bind(b)
        .bind(payload.to_string())
        .bind(now)
        .bind(now)
        .execute(db.executor())
        .await
        .unwrap()
        .last_insert_id();
    // Another process holds the A-to-B job, so this round claims the B-to-C one.
    site.execute(&format!(
        "UPDATE gonof_common_outbox SET status='processing',lease_token='elsewhere',lease_until={} WHERE eventid={first}",
        now + 300
    ))
    .await;
    assert_eq!(
        organization::process_merges(db, &site.services.global, now)
            .await
            .unwrap(),
        MergeReport::default()
    );
    assert_eq!(
        site.scalar(&format!(
            "SELECT fid FROM gonof_forum_thread WHERE tid={from_b}"
        ))
        .await,
        b,
        "nothing moves while B is still being filled"
    );
    assert_eq!(
        site.scalar(&format!(
            "SELECT status FROM gonof_forum_forum WHERE fid={b}"
        ))
        .await,
        1
    );
    assert_eq!(
        site.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_common_outbox WHERE eventid={older} AND status='pending' AND attempts=0 AND available_at>{now}"
        ))
        .await,
        1,
        "the waiting job goes back to the queue without spending an attempt"
    );

    // The other process lets its lease run out: A-to-B finishes, then B-to-C when its wait is over.
    site.execute(&format!(
        "UPDATE gonof_common_outbox SET lease_until=1 WHERE eventid={first}"
    ))
    .await;
    let report = organization::process_merges(db, &site.services.global, now)
        .await
        .unwrap();
    assert_eq!((report.moved, report.finished), (1, 1));
    let report = organization::process_merges(db, &site.services.global, now + 60)
        .await
        .unwrap();
    assert_eq!((report.moved, report.finished), (2, 1));
    for tid in [from_a, from_b] {
        assert_eq!(
            site.scalar(&format!(
                "SELECT fid FROM gonof_forum_thread WHERE tid={tid}"
            ))
            .await,
            c,
            "thread {tid} ends in C"
        );
    }
    assert_eq!(
        site.scalar(&format!(
            "SELECT COUNT(*) FROM gonof_forum_forum WHERE fid IN ({a},{b}) AND status=0"
        ))
        .await,
        2
    );
    site.services.cleanup_ref().await.unwrap();
}

/// A board being merged away cannot take a child board: saving one under it is refused, and a child that got there
/// some other way stops the job instead of being disabled with its parent.
#[tokio::test]
async fn a_board_being_merged_away_takes_no_child_boards() {
    let site = Site::new(false).await;
    let db = site.db();
    let admin = admin(&site).await;
    let (source, target, child) = (
        site.fid("showcase").await,
        site.fid("daily").await,
        site.fid("ai").await,
    );
    thread(&site, admin.uid, source, "To be merged").await;
    let eventid = organization::request_merge(
        db,
        admin,
        source,
        target,
        version(&site, source).await,
        "Merge showcase",
    )
    .await
    .unwrap();
    let moved = ForumInput {
        parent_fid: source,
        ..input(&site, child).await
    };
    assert!(matches!(
        organization::save_forum(db, admin, &moved).await,
        Err(ContentWriteError::Validation(message)) if message.contains("is being merged into another one")
    ));

    site.execute(&format!(
        "UPDATE gonof_forum_forum SET parent_fid={source} WHERE fid={child}"
    ))
    .await;
    let report = organization::process_merges(db, &site.services.global, identity::now())
        .await
        .unwrap();
    assert_eq!((report.moved, report.finished), (1, 0));
    assert_eq!(
        site.scalar(&format!(
            "SELECT status FROM gonof_forum_forum WHERE fid={source}"
        ))
        .await,
        1,
        "the source board stays enabled with its child"
    );
    assert_eq!(
        site.text(&format!(
            "SELECT status FROM gonof_common_outbox WHERE eventid={eventid}"
        ))
        .await,
        "failed"
    );
    assert!(
        site.text(&format!(
            "SELECT last_error FROM gonof_common_outbox WHERE eventid={eventid}"
        ))
        .await
        .starts_with("the source forum has child forums")
    );
    site.changed();
    let home = site.get("/ai", None).await;
    assert_eq!(home.status, StatusCode::OK);
    site.services.cleanup_ref().await.unwrap();
}

/// Turns multi-domain sites on through the admin form and returns the main domain row.
async fn enable_domains(site: &Site) -> i64 {
    let answer = site
        .post("/manage/domains/switch", &site.admin, &[("enabled", "on")])
        .await;
    assert!(answer.status.is_redirection(), "{}", answer.body);
    DomainMap::load(site.db())
        .await
        .unwrap()
        .main(TEST_SITE_URL)
        .unwrap()
        .domain_id
}

async fn domain_version(site: &Site, domain_id: i64) -> i64 {
    site.scalar(&format!(
        "SELECT version FROM gonof_common_site_domain WHERE domain_id={domain_id}"
    ))
    .await
}

/// Saves a domain row through the admin form: `domain_id` 0 adds one; the main row takes only its boards.
async fn save_domain(
    site: &Site,
    domain_id: i64,
    version: i64,
    url: &str,
    canonical: &str,
    fids: &[i64],
) -> Answer {
    let mut fields: Vec<(String, String)> = vec![
        ("domain_id".into(), domain_id.to_string()),
        ("version".into(), version.to_string()),
        ("site_url".into(), url.into()),
        ("site_name".into(), "Other site".into()),
        ("canonical".into(), canonical.into()),
        ("enabled".into(), "1".into()),
    ];
    for fid in fids {
        fields.push((format!("forum_{fid}"), "1".into()));
    }
    let borrowed: Vec<(&str, &str)> = fields
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    site.post("/manage/domains", &site.admin, &borrowed).await
}

async fn top_level(site: &Site) -> Vec<i64> {
    db::query_scalar("SELECT fid FROM gonof_forum_forum WHERE parent_fid IS NULL ORDER BY fid")
        .fetch_all(site.db().executor())
        .await
        .unwrap()
}

/// Promoting a child board to the top level keeps it (and its threads) on the domains its parent was on; a parent
/// shown nowhere hands over the main domain, like a new top-level board gets.
#[tokio::test]
async fn a_promoted_child_board_stays_on_the_domains_of_its_parent() {
    let site = Site::new(false).await;
    let db = site.db();
    let admin = admin(&site).await;
    let main = enable_domains(&site).await;
    let (daily, ai, hosting, releases) = (
        site.fid("daily").await,
        site.fid("ai").await,
        site.fid("hosting").await,
        site.fid("releases").await,
    );
    // A second site shows daily too, pointing its canonical at the main domain.
    let answer = save_domain(&site, 0, 0, "http://demo2.test", "main", &[daily]).await;
    assert!(answer.status.is_redirection(), "{}", answer.body);
    for (child, parent) in [(ai, daily), (releases, hosting)] {
        organization::save_forum(
            db,
            admin,
            &ForumInput {
                parent_fid: parent,
                ..input(&site, child).await
            },
        )
        .await
        .unwrap();
    }
    // hosting is on no domain at all.
    let others: Vec<i64> = top_level(&site)
        .await
        .into_iter()
        .filter(|fid| *fid != hosting)
        .collect();
    let answer = save_domain(
        &site,
        main,
        domain_version(&site, main).await,
        TEST_SITE_URL,
        "self",
        &others,
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK);
    assert!(
        answer.body.contains("no enabled domain shows Hosting"),
        "{}",
        answer.body
    );
    let tid = thread(&site, admin.uid, ai, "Written in a child board").await;
    site.changed();
    assert_eq!(site.get("/ai", None).await.status, StatusCode::OK);
    assert_eq!(
        site.get(&format!("/thread-{tid}"), None).await.status,
        StatusCode::OK
    );

    for child in [ai, releases] {
        organization::save_forum(
            db,
            admin,
            &ForumInput {
                parent_fid: 0,
                ..input(&site, child).await
            },
        )
        .await
        .unwrap();
    }
    site.changed();
    assert_eq!(site.get("/ai", None).await.status, StatusCode::OK);
    assert_eq!(
        site.get(&format!("/thread-{tid}"), None).await.status,
        StatusCode::OK
    );
    assert_eq!(site.get("/releases", None).await.status, StatusCode::OK);
    let map = DomainMap::load(db).await.unwrap();
    let demo2 = map
        .rows()
        .iter()
        .find(|row| row.site_url == "http://demo2.test")
        .unwrap();
    assert!(demo2.forums.contains(&ai), "copied from daily");
    assert!(!demo2.forums.contains(&releases));
    let main_row = map.get(main).unwrap();
    assert!(main_row.forums.contains(&ai) && main_row.forums.contains(&releases));
    site.services.cleanup_ref().await.unwrap();
}

/// Saving a board changes which boards a domain links, so it gives the domain a new version: a domain form opened
/// before a new top-level board was created is refused instead of taking the new board off the site.
#[tokio::test]
async fn a_main_domain_form_opened_before_a_new_board_is_refused() {
    let site = Site::new(false).await;
    let main = enable_domains(&site).await;
    let opened = domain_version(&site, main).await;
    let shown = top_level(&site).await;
    let answer = site
        .post(
            "/manage/forums",
            &site.admin,
            &[
                ("fid", "0"),
                ("version", "0"),
                ("name", "Fresh"),
                ("slug", "fresh"),
                ("status", "1"),
                ("visibility", "public"),
            ],
        )
        .await;
    assert!(answer.status.is_redirection(), "{}", answer.body);
    assert_eq!(domain_version(&site, main).await, opened + 1);
    let answer = save_domain(&site, main, opened, TEST_SITE_URL, "self", &shown).await;
    assert_eq!(answer.status, StatusCode::CONFLICT, "{}", answer.body);
    assert!(answer.body.contains("This domain has been changed."));
    assert_eq!(site.get("/fresh", None).await.status, StatusCode::OK);

    // Turning it into a child board takes it off the domain, with another new version.
    let admin = admin(&site).await;
    let fresh = site.fid("fresh").await;
    organization::save_forum(
        site.db(),
        admin,
        &ForumInput {
            parent_fid: site.fid("daily").await,
            ..input(&site, fresh).await
        },
    )
    .await
    .unwrap();
    assert_eq!(domain_version(&site, main).await, opened + 2);
    site.services.cleanup_ref().await.unwrap();
}

/// Top-level boards (new or promoted) stop at the limit the domain links are sized for.
#[tokio::test]
async fn top_level_boards_stop_at_the_limit() {
    let site = Site::new(false).await;
    let db = site.db();
    let admin = admin(&site).await;
    let missing = organization::MAX_TOP_FORUMS - top_level(&site).await.len() as i64;
    let values: Vec<String> = (0..missing)
        .map(|n| format!("('filler{n}','Filler {n}','Filler','',100)"))
        .collect();
    site.execute(&format!(
        "INSERT INTO gonof_forum_forum(slug,name,description,guidance,displayorder) VALUES{}",
        values.join(",")
    ))
    .await;
    let fresh = ForumInput {
        name: "One too many".into(),
        slug: "one-too-many".into(),
        status: 1,
        visibility: "public".into(),
        ..Default::default()
    };
    assert!(matches!(
        organization::save_forum(db, admin, &fresh).await,
        Err(ContentWriteError::Validation(message)) if message.contains("at most 400 top-level forums")
    ));
    // Child boards are not counted; promoting one is.
    let child = organization::save_forum(
        db,
        admin,
        &ForumInput {
            parent_fid: site.fid("daily").await,
            ..fresh.clone()
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        organization::save_forum(db, admin, &ForumInput { parent_fid: 0, ..input(&site, child).await }).await,
        Err(ContentWriteError::Validation(message)) if message.contains("at most 400 top-level forums")
    ));
    assert_eq!(
        top_level(&site).await.len() as i64,
        organization::MAX_TOP_FORUMS
    );
    site.services.cleanup_ref().await.unwrap();
}

/// Domain links past the bound the reads are sized for (only possible by writing the database directly) are an error
/// instead of quietly dropping the links of the last domains.
#[tokio::test]
async fn domain_links_past_the_read_bound_fail_instead_of_being_cut_off() {
    let site = Site::new(false).await;
    enable_domains(&site).await;
    let missing = organization::MAX_TOP_FORUMS + 1 - top_level(&site).await.len() as i64;
    let values: Vec<String> = (0..missing)
        .map(|n| format!("('filler{n}','Filler {n}','Filler','',100)"))
        .collect();
    site.execute(&format!(
        "INSERT INTO gonof_forum_forum(slug,name,description,guidance,displayorder) VALUES{}",
        values.join(",")
    ))
    .await;
    let domains = site
        .scalar("SELECT COUNT(*) FROM gonof_common_site_domain")
        .await;
    let rows: Vec<String> = (domains..gono_forum_web::domains::MAX_DOMAINS as i64)
        .map(|n| format!("('http://site{n}.test','Site {n}','self',1,1,1)"))
        .collect();
    site.execute(&format!(
        "INSERT INTO gonof_common_site_domain(site_url,site_name,canonical,status,version,dateline) VALUES{}",
        rows.join(",")
    ))
    .await;
    site.execute("DELETE FROM gonof_common_site_domain_forum")
        .await;
    let domain_ids: Vec<i64> =
        db::query_scalar("SELECT domain_id FROM gonof_common_site_domain ORDER BY domain_id")
            .fetch_all(site.db().executor())
            .await
            .unwrap();
    let fids = top_level(&site).await;
    for domain_id in &domain_ids {
        let links: Vec<String> = fids
            .iter()
            .map(|fid| format!("({domain_id},{fid})"))
            .collect();
        site.execute(&format!(
            "INSERT INTO gonof_common_site_domain_forum(domain_id,fid) VALUES{}",
            links.join(",")
        ))
        .await;
    }
    let error = DomainMap::load(site.db()).await.unwrap_err().to_string();
    assert!(
        error.contains("gonof_common_site_domain_forum has more than 20,000 rows"),
        "{error}"
    );
    site.services.cleanup_ref().await.unwrap();
}

/// On a host that registered its root path, a subsite path must not take a board's page addresses (`/daily-2` is page
/// 2 of daily), and a new board must not take a subsite's path the same way.
#[tokio::test]
async fn subsite_paths_and_board_page_addresses_keep_apart() {
    let site = Site::new(false).await;
    let admin = admin(&site).await;
    enable_domains(&site).await;
    let answer = save_domain(
        &site,
        0,
        0,
        &format!("{TEST_SITE_URL}/daily-2"),
        "self",
        &[],
    )
    .await;
    assert_eq!(
        answer.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        answer.body
    );
    assert!(
        answer.body.contains("is a forum address"),
        "{}",
        answer.body
    );

    let answer = save_domain(&site, 0, 0, &format!("{TEST_SITE_URL}/news-3"), "self", &[]).await;
    assert!(answer.status.is_redirection(), "{}", answer.body);
    let news = ForumInput {
        name: "News".into(),
        slug: "news".into(),
        status: 1,
        visibility: "public".into(),
        ..Default::default()
    };
    assert!(matches!(
        organization::save_forum(site.db(), admin, &news).await,
        Err(ContentWriteError::Validation(message)) if message.contains("same as a sub-path registered")
    ));
    site.services.cleanup_ref().await.unwrap();
}

fn hidden_value<'a>(html: &'a str, name: &str) -> &'a str {
    let marker = format!("name=\"{name}\" value=\"");
    let at = html
        .find(&marker)
        .unwrap_or_else(|| panic!("no {name} in {html}"))
        + marker.len();
    &html[at..at + html[at..].find('"').unwrap()]
}

async fn thread_version(site: &Site, tid: i64) -> i64 {
    site.scalar(&format!(
        "SELECT version FROM gonof_forum_thread WHERE tid={tid}"
    ))
    .await
}

/// Archiving a thread into a disabled or admin-only board works, and the administrator is brought back to the move
/// page with a notice instead of to a thread page the site shows nobody.
#[tokio::test]
async fn moving_a_thread_out_of_sight_returns_to_the_move_page() {
    let site = Site::new(false).await;
    let admin = admin(&site).await;
    let daily = site.fid("daily").await;
    let (archive, staff) = (site.fid("releases").await, site.fid("hosting").await);
    site.execute(&format!(
        "UPDATE gonof_forum_forum SET status=0 WHERE fid={archive}"
    ))
    .await;
    site.execute(&format!(
        "UPDATE gonof_forum_forum SET visibility='private' WHERE fid={staff}"
    ))
    .await;
    site.changed();
    for target in [archive, staff] {
        let tid = thread(&site, admin.uid, daily, "To archive").await;
        let page = format!("/manage/threads/{tid}/organization");
        let form = site.get(&page, Some(&site.admin)).await;
        assert_eq!(form.status, StatusCode::OK);
        let target_fid = target.to_string();
        let answer = site
            .post(
                &format!("/manage/threads/{tid}/move"),
                &site.admin,
                &[
                    ("version", hidden_value(&form.body, "version")),
                    ("request_key", hidden_value(&form.body, "request_key")),
                    ("target_fid", &target_fid),
                    ("reason", "Archive"),
                ],
            )
            .await;
        assert!(answer.status.is_redirection(), "{}", answer.body);
        assert_eq!(
            answer.header(axum::http::header::LOCATION),
            format!("{page}?moved=1")
        );
        assert_eq!(
            site.scalar(&format!(
                "SELECT fid FROM gonof_forum_thread WHERE tid={tid}"
            ))
            .await,
            target
        );
        let notice = site
            .get(&format!("{page}?moved=1"), Some(&site.admin))
            .await;
        assert_eq!(notice.status, StatusCode::OK);
        assert!(
            notice
                .body
                .contains("Thread moved. Its new forum is disabled or admin-only")
        );
    }
    // A move into a board the site shows still opens the thread.
    let tid = thread(&site, admin.uid, daily, "Moved in sight").await;
    let form = site
        .get(
            &format!("/manage/threads/{tid}/organization"),
            Some(&site.admin),
        )
        .await;
    let target_fid = site.fid("ai").await.to_string();
    let answer = site
        .post(
            &format!("/manage/threads/{tid}/move"),
            &site.admin,
            &[
                ("version", hidden_value(&form.body, "version")),
                ("request_key", hidden_value(&form.body, "request_key")),
                ("target_fid", &target_fid),
                ("reason", "Better fit"),
            ],
        )
        .await;
    assert_eq!(
        answer.header(axum::http::header::LOCATION),
        format!("/thread-{tid}")
    );
    site.services.cleanup_ref().await.unwrap();
}

/// A move refused because the thread changed meanwhile shows the form again as it was sent (target and reason), with
/// the thread's current version; a move into the thread's own board is refused and leaves no audit entry.
#[tokio::test]
async fn a_refused_move_keeps_what_the_administrator_entered() {
    let site = Site::new(false).await;
    let admin = admin(&site).await;
    let daily = site.fid("daily").await;
    let tid = thread(&site, admin.uid, daily, "Contested").await;
    let page = format!("/manage/threads/{tid}/organization");
    let stale = site.get(&page, Some(&site.admin)).await;
    // Another tab moves the thread first.
    let other = site.get(&page, Some(&site.admin)).await;
    let ai = site.fid("ai").await.to_string();
    let answer = site
        .post(
            &format!("/manage/threads/{tid}/move"),
            &site.admin,
            &[
                ("version", hidden_value(&other.body, "version")),
                ("request_key", hidden_value(&other.body, "request_key")),
                ("target_fid", &ai),
                ("reason", "Other tab"),
            ],
        )
        .await;
    assert!(answer.status.is_redirection(), "{}", answer.body);
    let releases = site.fid("releases").await;
    let target_fid = releases.to_string();
    let answer = site
        .post(
            &format!("/manage/threads/{tid}/move"),
            &site.admin,
            &[
                ("version", hidden_value(&stale.body, "version")),
                ("request_key", hidden_value(&stale.body, "request_key")),
                ("target_fid", &target_fid),
                ("reason", "Belongs with the releases"),
            ],
        )
        .await;
    assert_eq!(answer.status, StatusCode::CONFLICT, "{}", answer.body);
    assert!(
        answer
            .body
            .contains(&format!("<option value=\"{releases}\" selected>")),
        "{}",
        answer.body
    );
    assert!(
        answer
            .body
            .contains(">Belongs with the releases</textarea>")
    );
    assert_eq!(
        hidden_value(&answer.body, "version"),
        thread_version(&site, tid).await.to_string()
    );
    assert_eq!(
        hidden_value(&answer.body, "request_key"),
        hidden_value(&stale.body, "request_key")
    );

    let moves = "SELECT COUNT(*) FROM gonof_common_log WHERE type='content.thread.move'";
    let before = site.scalar(moves).await;
    let fresh = site.get(&page, Some(&site.admin)).await;
    let answer = site
        .post(
            &format!("/manage/threads/{tid}/move"),
            &site.admin,
            &[
                ("version", hidden_value(&fresh.body, "version")),
                ("request_key", hidden_value(&fresh.body, "request_key")),
                ("target_fid", &ai),
                ("reason", "Leave it where it is"),
            ],
        )
        .await;
    assert_eq!(
        answer.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        answer.body
    );
    assert!(answer.body.contains("The thread is already in that forum."));
    assert_eq!(site.scalar(moves).await, before);
    site.services.cleanup_ref().await.unwrap();
}

async fn image(site: &Site, aid: i64, uid: i64, name: &str) {
    let now = identity::now();
    db::query(
        "INSERT INTO gonof_forum_attachment(aid,uid,request_key,request_hash) VALUES(?,?,?,?)",
    )
    .bind(aid)
    .bind(uid)
    .bind(identity::random_token())
    .bind(identity::random_token())
    .execute(site.db().executor())
    .await
    .unwrap();
    db::query("INSERT INTO gonof_forum_attachment_0(aid,filename,filesize,mime_type,content_sha256,attachment,storage_profile_id,description,alt_text,dateline,updated_at,lifecycle,scan_status,scan_detail) VALUES(?,?,10,'image/png',?,'test','test','','',?,?,'ready','clean','')")
        .bind(aid)
        .bind(name)
        .bind("a".repeat(64))
        .bind(now)
        .bind(now)
        .execute(site.db().executor())
        .await
        .unwrap();
}

/// The board icon picker offers only images a save accepts: not trashed and not past their retention period. A board's
/// current icon stays listed whatever its state, so the form shows what the board has.
#[tokio::test]
async fn the_icon_picker_leaves_out_images_a_save_refuses() {
    let site = Site::new(false).await;
    let admin = admin(&site).await;
    image(&site, 9101, admin.uid, "kept.png").await;
    image(&site, 9102, admin.uid, "trashed.png").await;
    image(&site, 9103, admin.uid, "expired.png").await;
    image(&site, 9104, admin.uid, "waiting.png").await;
    let now = identity::now();
    site.execute(&format!(
        "UPDATE gonof_forum_attachment_0 SET trashed_at={now} WHERE aid=9102"
    ))
    .await;
    site.execute(&format!(
        "UPDATE gonof_forum_attachment_0 SET purge_after={} WHERE aid=9103",
        now - 60
    ))
    .await;
    site.execute(&format!(
        "UPDATE gonof_forum_attachment_0 SET purge_after={} WHERE aid=9104",
        now + 86_400
    ))
    .await;
    let daily = site.fid("daily").await;
    let page = site
        .get(&format!("/manage/forums?fid={daily}"), Some(&site.admin))
        .await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(
        page.body
            .contains("<option value=\"9101\">kept.png</option>")
    );
    assert!(
        page.body
            .contains("<option value=\"9104\">waiting.png</option>")
    );
    assert!(
        !page.body.contains("trashed.png"),
        "a trashed image is offered"
    );
    assert!(
        !page.body.contains("expired.png"),
        "an expired image is offered"
    );

    // A board whose icon went to the trash still shows it as chosen.
    site.execute(&format!(
        "UPDATE gonof_forum_forum SET icon_aid=9102 WHERE fid={daily}"
    ))
    .await;
    site.changed();
    let page = site
        .get(&format!("/manage/forums?fid={daily}"), Some(&site.admin))
        .await;
    assert!(
        page.body
            .contains("<option value=\"9102\" selected>trashed.png</option>")
    );
    assert!(!page.body.contains("expired.png"));
    site.services.cleanup_ref().await.unwrap();
}

async fn get_on(site: &Site, host: &str, path: &str) -> Answer {
    site.send(
        axum::http::Request::builder()
            .uri(path)
            .header(axum::http::header::HOST, host)
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await
}

/// The deploy script probes `/healthz` at the listen address, which a multi-domain site may have registered only with a
/// sub-path: the health check answers there like at the root of any other host.
#[tokio::test]
async fn the_health_check_answers_on_a_host_registered_only_with_a_sub_path() {
    let site = Site::new(false).await;
    enable_domains(&site).await;
    let answer = save_domain(&site, 0, 0, "http://listen.test/bbs", "self", &[]).await;
    assert!(answer.status.is_redirection(), "{}", answer.body);
    let root = get_on(&site, "listen.test", "/healthz").await;
    assert_eq!(root.status, StatusCode::OK, "{}", root.body);
    assert_eq!(root.body, "ok\n");
    assert_eq!(
        get_on(&site, "listen.test", "/bbs/healthz").await.status,
        StatusCode::OK
    );
    assert_eq!(
        get_on(&site, "listen.test", "/privacy").await.status,
        StatusCode::NOT_FOUND
    );
    site.services.cleanup_ref().await.unwrap();
}

/// With every database connection taken, the health check reports the failure within its own budget instead of
/// waiting for a connection in the layers in front of it; the deploy script's probe gives up after five seconds.
#[tokio::test]
async fn the_health_check_answers_in_time_when_the_pool_is_exhausted() {
    let site = Site::new(false).await;
    let mut config = site.services.config.clone();
    config.database.max_connections = 1;
    config.database.acquire_timeout_seconds = 3;
    let small = db::Database::connect(&config.database).await.unwrap();
    let app = gono_forum_web::router::build_with_global(
        config,
        &site.services.root,
        small.clone(),
        site.services.cache.clone(),
        // A cold process cache: the settings must come from the database.
        gono_forum_web::cache::GlobalCache::new(site.services.cache.clone()),
    );
    let mysql = match small.pool.as_mysql() {
        Some(pool) => Some(pool.acquire().await.unwrap()),
        None => None,
    };
    let write = match small.pool.as_sqlite() {
        Some(pool) => Some(pool.acquire().await.unwrap()),
        None => None,
    };
    let read = match small.pool.as_sqlite_read() {
        Some(pool) => Some(pool.acquire().await.unwrap()),
        None => None,
    };
    let started = std::time::Instant::now();
    let response = tower::ServiceExt::oneshot(
        app,
        axum::http::Request::builder()
            .uri("/healthz")
            .header(axum::http::header::HOST, "127.0.0.1:58833")
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await
    .unwrap();
    let elapsed = started.elapsed();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "the health check took {elapsed:?}"
    );
    drop((mysql, write, read));
    small.pool.close().await;
    site.services.cleanup_ref().await.unwrap();
}

fn footer_sql(body: &str, backend: &str) -> usize {
    let footer = body
        .split_once("<p class=\"colophon__legal\" translate=\"no\">")
        .and_then(|(_, rest)| rest.split_once("</p>"))
        .map(|(footer, _)| footer)
        .unwrap_or_else(|| panic!("no footer: {body}"));
    let tail = footer
        .split_once(&format!("{backend} "))
        .unwrap_or_else(|| panic!("no {backend} count: {footer}"))
        .1;
    tail.chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap()
}

/// With the process cache off, resolving the site reads the settings and the domain table on every request: those reads
/// count in the footer and are listed in the debug panel like the rest of the request's SQL.
#[tokio::test]
async fn the_footer_and_the_debug_panel_count_what_resolving_the_site_reads() {
    let site = Site::new(false).await;
    enable_domains(&site).await;
    for (key, value) in [
        ("settings.debug.panel", "true"),
        ("settings.cache.ttl_seconds", "0"),
        ("settings.display.footer_metrics", "true"),
    ] {
        gono_forum_web::test_support::set_setting(site.db(), key, value)
            .await
            .unwrap();
    }
    site.changed();
    let page = site.get("/privacy", Some(&site.admin)).await;
    assert_eq!(page.status, StatusCode::OK);
    let panel = &page.body[page
        .body
        .find("<details id=\"gonof-debug\"")
        .expect("the administrator gets the panel")..];
    assert!(
        panel.contains("FROM gonof_common_site_domain_forum"),
        "the domain links site resolution read are listed"
    );
    let listed: usize = panel
        .split("data-debug-queries=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        footer_sql(&page.body, site.db().backend().label()),
        listed,
        "the footer counts the same statements the panel lists"
    );
    site.services.cleanup_ref().await.unwrap();
}

/// The debug layer runs before the sub-path prefix is stripped: pages under the prefix still get the panel, while
/// sensitive pages, robots.txt and static files under it get none.
#[tokio::test]
async fn a_sub_path_site_profiles_its_pages_but_not_sensitive_ones() {
    let site = Site::new(false).await;
    gono_forum_web::test_support::set_site_url(site.db(), &format!("{TEST_SITE_URL}/bbs"))
        .await
        .unwrap();
    gono_forum_web::test_support::set_setting(site.db(), "settings.debug.panel", "true")
        .await
        .unwrap();
    site.changed();
    // The forum under /bbs names its session cookie after the path (docs/install.md, "Reverse proxy").
    let session = format!("gonof_session_bbs__port58833={}", site.admin.0);
    let cookie = [("cookie", session.as_str())];
    let page = site.get_with("/bbs/privacy", None, &cookie).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("<details id=\"gonof-debug\""));
    for path in ["/bbs/my/api", "/bbs/robots.txt"] {
        let answer = site.get_with(path, None, &cookie).await;
        assert_eq!(answer.status, StatusCode::OK, "{path}");
        assert!(!answer.body.contains("gonof-debug"), "{path}");
    }
    site.services.cleanup_ref().await.unwrap();
}
