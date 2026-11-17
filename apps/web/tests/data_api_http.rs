//! `GET /data/v1/<source>` (docs/data.md): the envelope, the fields of every source against the tables of
//! docs/data.md, the visitor's view, and the security rules (read only, 401 for personal sources, `no-store`,
//! `nosniff`, no CORS, no JSONP, same site only).
#[path = "support/site.rs"]
mod site;

use std::collections::BTreeSet;

use axum::http::{StatusCode, header};
use gono_forum_web::{i18n::Lang, project_root};
use serde_json::Value;
use site::Site;

/// A site with the development fixture, one of each thing the member sources list, and a computed leaderboard.
async fn fixture() -> Site {
    Site::prepared(true, |db| async move {
        let uid = |sql: &'static str| {
            let db = db.clone();
            async move {
                gono_forum_web::db::query_scalar::<i64>(sql)
                    .fetch_one(db.executor())
                    .await
                    .unwrap()
            }
        };
        let author = uid("SELECT authorid FROM gonof_forum_thread WHERE tid=103").await;
        let daily = uid("SELECT fid FROM gonof_forum_forum WHERE slug='daily'").await;
        for sql in [
            "INSERT INTO gonof_common_honor (id,name,description,displayorder,status,version) VALUES (99,'Helpers','Answered many questions',0,1,1)".to_owned(),
            format!("INSERT INTO gonof_common_member_honor (uid,honor_id,granted_by,granted_at,expires_at) VALUES ({author},99,{author},1000,NULL)"),
            format!("INSERT INTO gonof_common_member_verification (uid,category,title,site_url,granted_by,granted_at,reviewed_at,expires_at) VALUES ({author},'blogger','Writes about tools','https://blog.example.org/',{author},1000,1000,NULL)"),
            format!("INSERT INTO gonof_forum_moderator (uid,fid) VALUES ({author},{daily})"),
            format!("UPDATE gonof_common_member SET adminid=3 WHERE uid={author}"),
            "INSERT INTO gonof_common_friend_link (status,category,name,url,domain,live_domain,description,home,displayorder,dateline,updated_at) VALUES ('active','forum','Example','https://example.org/','example.org','example.org','An example site',1,0,1000,1000)".to_owned(),
        ] {
            gono_forum_web::db::query(&sql)
                .execute(db.executor())
                .await
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
        }
        gono_forum_web::ranks::refresh(&db, gono_forum_web::identity::now(), 0)
            .await
            .unwrap();
    })
    .await
}

/// The field names a table of docs/data.md lists: the backticked names of its first (or only listed) column, outside
/// parentheses, so `` `uid`, `name`, `href`, `avatar` (the member) `` gives the four names.
fn names(cell: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut depth = 0;
    let mut chars = cell.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '(' | '{' | '[' => depth += 1,
            ')' | '}' | ']' => depth -= 1,
            '`' => {
                let name: String = chars.by_ref().take_while(|ch| *ch != '`').collect();
                if depth == 0
                    && !name.is_empty()
                    && name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
                {
                    names.insert(name);
                }
            }
            _ => {}
        }
    }
    names
}

/// The fields docs/data.md documents for each source: the `| Field | Value |` table under its heading, and the
/// `| Source | Fields |` table of the personal sources.
fn documented() -> Vec<(String, BTreeSet<String>)> {
    let text = std::fs::read_to_string(project_root().join("docs/data.md")).unwrap();
    let mut found = Vec::new();
    let mut source: Option<String> = None;
    let mut in_fields = false;
    for line in text.lines() {
        if let Some(heading) = line.strip_prefix("### ") {
            source = heading
                .split('`')
                .nth(1)
                .filter(|name| !name.starts_with("me."))
                .map(str::to_owned);
            in_fields = false;
            continue;
        }
        if line.starts_with("| Field | Value |") {
            in_fields = true;
            found.push((source.clone().unwrap(), BTreeSet::new()));
            continue;
        }
        if line.starts_with("| Source | Fields |") {
            in_fields = false;
            continue;
        }
        let cells: Vec<&str> = line.split(" | ").collect();
        if line.starts_with("| `me.") && cells.len() >= 2 {
            let name = cells[0]
                .trim_start_matches("| ")
                .trim_matches('`')
                .to_owned();
            found.push((name, names(cells[1])));
            continue;
        }
        if in_fields {
            if !line.starts_with('|') {
                in_fields = false;
            } else if !line.starts_with("| ---") {
                found.last_mut().unwrap().1.extend(names(cells[0]));
            }
        }
    }
    found
}

fn keys(value: &Value) -> BTreeSet<String> {
    value.as_object().unwrap().keys().cloned().collect()
}

#[tokio::test]
async fn every_source_returns_the_fields_docs_data_md_lists() {
    let site = fixture().await;
    let thread_params = [
        ("threads", "sort=hot&days=30"),
        ("boards", "counts=true&last_post=true"),
        ("members.recent", ""),
        ("members.rank", "board=threads&period=all"),
        ("members.honors", ""),
        ("members.verified", "kind=blogger"),
        ("moderators", ""),
        ("site.stats", ""),
        ("site.links", "place=directory"),
        ("thread.outline", "tid=103"),
        ("me.card", ""),
        ("me.counts", ""),
        ("me.checkin", ""),
    ];
    let daily = site.fid("daily").await;
    let documented = documented();
    assert_eq!(
        documented
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        thread_params
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>(),
        "docs/data.md documents every source once, in order"
    );
    // Recent activity, so the hot list has something whatever day the test runs.
    site.execute(&format!(
        "UPDATE gonof_forum_thread SET lastpost={}, cache_revision=cache_revision+1 WHERE tid IN (101,103,105)",
        gono_forum_web::identity::now()
    ))
    .await;
    site.changed();
    for ((source, query), (_, fields)) in thread_params.iter().zip(&documented) {
        let query = if *source == "moderators" {
            format!("board={daily}")
        } else {
            (*query).to_owned()
        };
        let path = format!("/data/v1/{source}?{query}");
        let answer = site.get(&path, Some(&site.member)).await;
        assert_eq!(answer.status, StatusCode::OK, "{path}: {}", answer.body);
        let body = answer.json();
        let mut envelope: BTreeSet<String> = ["v", "source", "params", "items", "generated_at"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        if *source == "members.rank" {
            envelope.insert("computed_at".into());
        }
        assert_eq!(keys(&body), envelope, "{path}");
        assert_eq!(body["v"], 1);
        assert_eq!(body["source"], *source);
        let items = body["items"].as_array().unwrap();
        assert!(!items.is_empty(), "{path} returns something: {body}");
        for item in items {
            assert_eq!(&keys(item), fields, "{path}: {item}");
        }
    }
    site.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn records_follow_the_visitors_view() {
    let site = fixture().await;
    let hot = |body: &Value| -> Vec<i64> {
        body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["tid"].as_i64().unwrap())
            .collect()
    };
    site.execute(&format!(
        "UPDATE gonof_forum_thread SET lastpost={}, cache_revision=cache_revision+1 WHERE tid IN (101,103,105)",
        gono_forum_web::identity::now()
    ))
    .await;
    // A members-only thread is listed for members only.
    site.execute("UPDATE gonof_forum_thread SET visibility='members', cache_revision=cache_revision+1 WHERE tid=105")
        .await;
    gono_forum_web::test_support::refresh_listing(site.db())
        .await
        .unwrap();
    site.changed();
    let path = "/data/v1/threads?sort=hot&days=30&limit=20";
    let anonymous = site.get(path, None).await.json();
    let member = site.get(path, Some(&site.member)).await.json();
    assert!(!hot(&anonymous).contains(&105), "{anonymous}");
    assert!(hot(&member).contains(&105), "{member}");
    let record = member["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["tid"] == 105)
        .unwrap();
    assert_eq!(record["members_only"], true);
    assert_eq!(record["href"], "/thread-105");
    assert!(record["created"]["at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(anonymous["params"]["scope"], "site");
    assert_eq!(anonymous["params"]["days"], 30);
    // A member the visitor blocked is not listed.
    let author = site
        .scalar("SELECT authorid FROM gonof_forum_thread WHERE tid=103")
        .await;
    let blocker = site
        .scalar("SELECT MAX(uid) FROM gonof_common_member")
        .await;
    site.execute(&format!(
        "INSERT INTO gonof_home_member_block (uid,blocked_uid,dateline) VALUES ({blocker},{author},1000)"
    ))
    .await;
    let blocked = site.get(path, Some(&site.member)).await.json();
    assert!(!hot(&blocked).contains(&103), "{blocked}");
    let honors = site
        .get("/data/v1/members.honors?honor=99", Some(&site.member))
        .await
        .json();
    assert_eq!(
        honors["items"][0]["members"].as_array().unwrap().len(),
        0,
        "{honors}"
    );
    // A board the visitor cannot read is not found.
    let private = site.fid("daily").await;
    site.execute(&format!(
        "UPDATE gonof_forum_forum SET visibility='private', version=version+1 WHERE fid={private}"
    ))
    .await;
    site.changed();
    let refused = site
        .get(&format!("/data/v1/threads?scope=board:{private}"), None)
        .await;
    assert_eq!(refused.status, StatusCode::NOT_FOUND);
    assert_eq!(refused.json()["code"], "not_found");
    site.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn the_endpoint_is_read_only_private_and_same_site() {
    let site = fixture().await;
    let answer = site
        .get_with(
            "/data/v1/threads",
            None,
            &[("origin", "https://elsewhere.example")],
        )
        .await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    assert_eq!(
        answer.header(header::CONTENT_TYPE),
        "application/json; charset=utf-8"
    );
    assert!(answer.header(header::CACHE_CONTROL).contains("no-store"));
    assert_eq!(answer.header(header::X_CONTENT_TYPE_OPTIONS), "nosniff");
    assert!(
        answer.headers.get(header::SET_COOKIE).is_none(),
        "no cookie"
    );
    for (name, _) in &answer.headers {
        assert!(
            !name.as_str().starts_with("access-control-"),
            "no CORS header: {name}"
        );
    }
    // Personal sources need a session and never redirect.
    for source in ["me.card", "me.counts", "me.checkin"] {
        let refused = site.get(&format!("/data/v1/{source}"), None).await;
        assert_eq!(refused.status, StatusCode::UNAUTHORIZED, "{source}");
        assert!(refused.header(header::LOCATION).is_empty());
        assert_eq!(refused.json()["code"], "unauthorized");
        assert_eq!(refused.json()["error"], Lang::EN.data_error_unauthorized());
        assert!(refused.header(header::CACHE_CONTROL).contains("no-store"));
        let own = site
            .get(&format!("/data/v1/{source}"), Some(&site.member))
            .await;
        assert_eq!(own.status, StatusCode::OK, "{source}: {}", own.body);
    }
    // No JSONP, no unknown or repeated parameters.
    for path in [
        "/data/v1/threads?callback=x",
        "/data/v1/threads?limit=5&limit=6",
        "/data/v1/threads?limit=50",
        "/data/v1/threads?scope=current",
    ] {
        let refused = site.get(path, None).await;
        assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{path}");
        let body = refused.json();
        assert_eq!(keys(&body), ["code", "error"].map(String::from).into());
        assert_eq!(body["code"], "invalid_param");
    }
    let unknown = site.get("/data/v1/everything", None).await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    // Cross-site requests are refused, same-site ones answered.
    let cross = site
        .get_with(
            "/data/v1/threads",
            None,
            &[("sec-fetch-site", "cross-site")],
        )
        .await;
    assert_eq!(cross.status, StatusCode::FORBIDDEN);
    assert_eq!(cross.json()["code"], "cross_site");
    let same = site
        .get_with(
            "/data/v1/threads",
            None,
            &[("sec-fetch-site", "same-origin")],
        )
        .await;
    assert_eq!(same.status, StatusCode::OK);
    // Read only.
    let posted = site.post("/data/v1/threads", &site.member, &[]).await;
    assert_eq!(
        posted.status,
        StatusCode::METHOD_NOT_ALLOWED,
        "{}",
        posted.body
    );
    assert_eq!(posted.json()["code"], "method_not_allowed");
    site.services.cleanup().await.unwrap();
}
