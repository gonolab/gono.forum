mod support;

use axum::{Router, http::StatusCode};
use gono_forum_web::{
    content::{self, MemberContentKind},
    db::Database,
    project_root, router, urls,
};

const ORIGIN: &str = "http://127.0.0.1:58833";
const MEMBER: &str = "member-pages";
/// A non-ASCII username: the Chinese for "Coins-Four-Star Member" (two characters, a hyphen, four characters).
const CHINESE_MEMBER: &str = "\u{91d1}\u{5e01}-\u{56db}\u{661f}\u{4f1a}\u{5458}";
const EMPTY_UID: i64 = 9000;

struct Fixture {
    services: gono_forum_web::test_support::TestFixture,
    app: Router,
    db: Database,
    uid: i64,
    chinese_uid: i64,
    expected_threads: Vec<u64>,
    expected_posts: Vec<u64>,
}

impl Fixture {
    async fn new() -> Self {
        let services = gono_forum_web::test_support::TestFixture::migrated()
            .await
            .unwrap();
        let db = services.db.clone();
        let config = services.config.clone();
        content::seed_forums(&db).await.unwrap();
        let mut discussion = content::fixtures()
            .into_iter()
            .find(|thread| thread.id == 103)
            .unwrap();
        discussion.author = MEMBER.into();
        discussion.title = "MEMBER_TOPIC_103".into();
        discussion.body = vec!["MEMBER_OPENING_103".into()];
        discussion.created = 1_700_000_000;
        discussion.updated = discussion.created + 10;
        for post in &mut discussion.posts {
            post.author = MEMBER.into();
            post.body = format!("MEMBER_REPLY_{}", post.pid);
            post.dateline = discussion.created + post.position as u64 % 3;
        }
        let mut post_order = discussion
            .posts
            .iter()
            .map(|post| (post.dateline, post.pid))
            .collect::<Vec<_>>();
        post_order.sort_unstable();
        post_order.reverse();
        let expected_posts = post_order.into_iter().map(|(_, pid)| pid).collect();
        let mut threads = vec![discussion.clone()];
        for tid in 201..=242 {
            let mut thread = discussion.clone();
            thread.id = tid;
            thread.title = format!("MEMBER_TOPIC_{tid}");
            thread.body = vec![format!("MEMBER_OPENING_{tid}")];
            thread.created += tid % 3;
            thread.posts.clear();
            if tid == 202 {
                thread.fixture_forum = "daily".into();
            }
            if tid == 241 {
                thread.author = "other-member".into();
                let mut post = discussion.posts[0].clone();
                post.pid = 241002;
                post.author = "other-member".into();
                post.body = "OTHER_MEMBER_REPLY".into();
                post.sub_replies_count = 0;
                thread.posts.push(post);
            }
            if tid == 242 {
                thread.author = CHINESE_MEMBER.into();
            }
            threads.push(thread);
        }
        // First-post PIDs are assigned in import order; threads with the same time sort in reverse of that order.
        let mut thread_order = threads
            .iter()
            .enumerate()
            .filter(|(_, thread)| thread.author == MEMBER)
            .map(|(index, thread)| (thread.created, index, thread.id))
            .collect::<Vec<_>>();
        thread_order.sort_unstable();
        thread_order.reverse();
        let expected_threads = thread_order.into_iter().map(|(_, _, tid)| tid).collect();
        content::import_fixture(&db, &threads).await.unwrap();
        gono_forum_web::db::query(&format!(
            "INSERT INTO gonof_common_member(uid,username) VALUES({EMPTY_UID},'empty-member')"
        ))
        .execute(db.executor())
        .await
        .unwrap();
        let mut uids = Vec::new();
        for name in [MEMBER, CHINESE_MEMBER] {
            uids.push(
                gono_forum_web::db::query_scalar::<i64>(
                    "SELECT uid FROM gonof_common_member WHERE username=?",
                )
                .bind(name)
                .fetch_one(db.executor())
                .await
                .unwrap(),
            );
        }
        Self {
            app: router::build(config, &project_root(), db.clone()),
            services,
            db,
            uid: uids[0],
            chinese_uid: uids[1],
            expected_threads,
            expected_posts,
        }
    }

    async fn all(
        &self,
        kind: MemberContentKind,
        expected_total: usize,
    ) -> Vec<content::MemberPost> {
        let mut posts = Vec::new();
        for page in 1..=expected_total.div_ceil(20).max(1) {
            let result = content::member_content(
                &self.db,
                self.uid,
                kind,
                page,
                20,
                gono_forum_web::test_support::SEEDED_PAGING,
            )
            .await
            .unwrap();
            assert_eq!(result.total, expected_total);
            assert_eq!(result.posts.len(), (expected_total - posts.len()).min(20));
            posts.extend(result.posts);
        }
        posts
    }

    async fn page(&self, path: &str, htmx: bool) -> String {
        let (status, headers, body) = support::fetch(&self.app, path, htmx).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(headers["cache-control"], "private, no-store");
        assert_eq!(headers["vary"], "HX-Request");
        assert!(body.starts_with("<!doctype html>"));
        assert!(body.contains(&format!("href=\"{ORIGIN}{path}\"")));
        body
    }
}

fn listed_pids(body: &str) -> Vec<u64> {
    body.split("data-member-pid=\"")
        .skip(1)
        .map(|part| part.split('"').next().unwrap().parse().unwrap())
        .collect()
}

#[tokio::test]
async fn member_collections_are_separate_bounded_and_stably_ordered() {
    let fixture = Fixture::new().await;
    let threads = fixture.all(MemberContentKind::Threads, 41).await;
    assert_eq!(
        threads.iter().map(|post| post.tid).collect::<Vec<_>>(),
        fixture.expected_threads
    );
    assert!(threads.iter().all(|post| post.position == 1));
    let posts = fixture.all(MemberContentKind::Posts, 78).await;
    assert_eq!(
        posts.iter().map(|post| post.pid).collect::<Vec<_>>(),
        fixture.expected_posts
    );
    assert!(
        posts
            .iter()
            .all(|post| post.position > 1 && post.tid == 103)
    );
    let overview = content::public_member(
        &fixture.db,
        MEMBER,
        gono_forum_web::test_support::SEEDED_PAGING,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(overview.posts.len(), 20);
    assert!(overview.posts.iter().any(|post| post.position == 1));
    assert!(overview.posts.iter().any(|post| post.position > 1));
    for (kind, path, expected) in [("thread", "threads", threads), ("post", "posts", posts)] {
        for (page, chunk) in expected.chunks(20).enumerate() {
            let suffix = if page == 0 {
                String::new()
            } else {
                format!("-{}", page + 1)
            };
            let path = format!("{}/{path}{suffix}", urls::space(fixture.uid));
            for htmx in [false, true] {
                let body = fixture.page(&path, htmx).await;
                let view = if kind == "thread" { "threads" } else { "posts" };
                assert!(body.contains(&format!("data-member-view=\"{view}\"")));
                assert!(body.contains(&format!("data-member-total=\"{}\"", expected.len())));
                assert_eq!(
                    listed_pids(&body),
                    chunk.iter().map(|post| post.pid).collect::<Vec<_>>()
                );
                assert_eq!(
                    body.matches(&format!("data-member-kind=\"{kind}\""))
                        .count(),
                    chunk.len()
                );
                let space = urls::space(fixture.uid);
                assert!(body.contains(&format!("href=\"{space}/threads\"")));
                assert!(body.contains(&format!("href=\"{space}/posts\"")));
                if page + 1 < expected.len().div_ceil(20) {
                    assert!(body.contains(&format!(
                        "href=\"{space}/{view}-{}\" rel=\"next\"",
                        page + 2
                    )));
                }
                assert!(!body.contains("OTHER_MEMBER_REPLY"));
            }
        }
    }
    let overview = fixture.page(&urls::space(fixture.uid), false).await;
    assert!(overview.contains("data-member-view=\"overview\""));
    assert_eq!(listed_pids(&overview).len(), 20);
    fixture.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn reply_excerpts_are_rendered_text_without_spoilers_or_reference_syntax() {
    let fixture = Fixture::new().await;
    let pid = fixture.expected_posts[0];
    let body = format!(
        "[@oldname](member:{}) replied **bold** ![image](attachment:8)\n\n<spoiler>\n\nSecret ending\n\n</spoiler>\n\na < b",
        fixture.chinese_uid
    );
    gono_forum_web::db::query("UPDATE gonof_forum_post SET message=? WHERE pid=?")
        .bind(&body)
        .bind(i64::try_from(pid).unwrap())
        .execute(fixture.db.executor())
        .await
        .unwrap();
    let page = fixture
        .page(&format!("{}/posts", urls::space(fixture.uid)), false)
        .await;
    let row = page
        .split(&format!("data-member-pid=\"{pid}\""))
        .nth(1)
        .unwrap()
        .split("</li>")
        .next()
        .unwrap();
    assert!(
        row.contains(&format!(">@{CHINESE_MEMBER} replied bold a &#60; b</div>")),
        "{row}"
    );
    for leaked in [
        "oldname",
        "Secret",
        "spoiler",
        "member:",
        "attachment:",
        "**",
    ] {
        assert!(!row.contains(leaked), "{leaked}: {row}");
    }
    fixture.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn member_collection_routes_normalize_uids_and_validate_page_boundaries() {
    let fixture = Fixture::new().await;
    let uid = fixture.uid;
    let space = urls::space(uid);
    let empty = urls::space(EMPTY_UID);
    for (path, target) in [
        (format!("{space}/threads-1"), format!("{space}/threads")),
        (format!("{space}/posts-01"), format!("{space}/posts")),
        (format!("{space}/threads-002"), format!("{space}/threads-2")),
        (
            format!("/space/0{uid}/posts-02"),
            format!("{space}/posts-2"),
        ),
        (
            format!("/space/00{uid}/threads"),
            format!("{space}/threads"),
        ),
        (format!("/space/0{uid}"), space.clone()),
    ] {
        let (status, headers, _) = support::fetch(&fixture.app, &path, false).await;
        assert_eq!(status, StatusCode::MOVED_PERMANENTLY, "{path}");
        assert_eq!(headers["location"], target);
        fixture.page(&target, false).await;
    }
    for path in [
        format!("{space}/threads-0"),
        format!("{space}/threads-4"),
        format!("{space}/posts-5"),
        format!("{space}/posts--1"),
        format!("{space}/threads-18446744073709551615"),
        format!("{space}/posts-18446744073709551616"),
        format!("{space}/threads-invalid"),
        format!("{space}/unknown"),
        format!("{space}/threads/extra"),
        format!("/space/{uid}-2"),
        "/space".into(),
        "/space/0".into(),
        "/space/-1".into(),
        "/space/member-pages".into(),
        "/space/99999999/threads".into(),
        format!("{empty}/threads-2"),
        format!("{empty}/posts-2"),
    ] {
        assert_eq!(
            support::fetch(&fixture.app, &path, false).await.0,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    for view in ["threads", "posts"] {
        let listing = fixture.page(&format!("{empty}/{view}"), false).await;
        assert!(listed_pids(&listing).is_empty());
        assert!(listing.contains(&format!("data-member-view=\"{view}\"")));
        // The username is Chinese, but the URL still only has the ASCII uid and needs no path encoding.
        let path = format!("{}/{view}", urls::space(fixture.chinese_uid));
        let chinese = fixture.page(&path, false).await;
        assert!(chinese.contains(CHINESE_MEMBER));
        assert_eq!(listed_pids(&chinese).len(), usize::from(view == "threads"));
        let alias = format!("/space/0{}/{view}-01", fixture.chinese_uid);
        let (status, headers, _) = support::fetch(&fixture.app, &alias, false).await;
        assert_eq!(status, StatusCode::MOVED_PERMANENTLY);
        assert_eq!(headers["location"], path);
    }
    fixture.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn member_reply_links_reach_late_root_floors_and_child_pages() {
    let fixture = Fixture::new().await;
    let posts = fixture.all(MemberContentKind::Posts, 78).await;
    for (pid, expected_href, floor) in [
        (103052, "/thread-103-2#floor-52", 52),
        (
            103076,
            "/thread-103?sub_root=103002&sub_page=2#floor-76",
            76,
        ),
    ] {
        let index = posts.iter().position(|post| post.pid == pid).unwrap();
        assert_eq!(posts[index].href, expected_href);
        let page = index / 20 + 1;
        let member_path = if page == 1 {
            format!("{}/posts", urls::space(fixture.uid))
        } else {
            format!("{}/posts-{page}", urls::space(fixture.uid))
        };
        let member = fixture.page(&member_path, false).await;
        assert!(member.contains(&expected_href.replace('&', "&#38;")));
        let (path, _) = expected_href.split_once('#').unwrap();
        let (status, _, body) = support::fetch(&fixture.app, path, false).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains(&format!("id=\"floor-{floor}\"")));
        assert!(body.contains(&format!("MEMBER_REPLY_{pid}")));
    }
    fixture.services.cleanup().await.unwrap();
}

#[tokio::test]
async fn member_collection_counts_and_pages_obey_content_visibility() {
    let fixture = Fixture::new().await;
    for sql in [
        "UPDATE gonof_forum_post SET deleted_at=1 WHERE tid=103 AND first=1",
        "UPDATE gonof_forum_thread SET deleted_at=1 WHERE tid=103",
        "UPDATE gonof_forum_post SET invisible=-1 WHERE tid=201 AND first=1",
        "UPDATE gonof_forum_forum SET visibility='private' WHERE slug='daily'",
        "UPDATE gonof_forum_post SET invisible=-1 WHERE pid=103004",
        "UPDATE gonof_forum_post SET deleted_at=1 WHERE pid IN (103002,103003)",
    ] {
        gono_forum_web::db::query(sql)
            .execute(fixture.db.executor())
            .await
            .unwrap();
    }
    // The read path looks at the copy columns on thread and post rows (forum, displayorder, the nested replies'
    // `root_invisible`): after raw SQL, sync them through the real copy code.
    gono_forum_web::test_support::resync_copies(&fixture.db)
        .await
        .unwrap();
    gono_forum_web::test_support::refresh_listing(&fixture.db)
        .await
        .unwrap();
    let threads = fixture.all(MemberContentKind::Threads, 38).await;
    assert!(
        threads
            .iter()
            .all(|post| ![103, 201, 202].contains(&post.tid))
    );
    let surviving = fixture.all(MemberContentKind::Posts, 75).await;
    assert!(
        surviving
            .iter()
            .all(|post| ![103002, 103003, 103004].contains(&post.pid))
    );
    assert!(surviving.iter().any(|post| post.pid == 103056));
    assert!(surviving.iter().any(|post| post.pid == 103076));
    gono_forum_web::db::query("UPDATE gonof_forum_post SET invisible=-1 WHERE pid=103002")
        .execute(fixture.db.executor())
        .await
        .unwrap();
    gono_forum_web::test_support::refresh_listing(&fixture.db)
        .await
        .unwrap();
    let posts = fixture.all(MemberContentKind::Posts, 51).await;
    assert!(
        posts
            .iter()
            .all(|post| (103005..=103055).contains(&post.pid))
    );
    assert!(posts.iter().all(|post| post.subject == "Thread withdrawn"));
    // The tab's pagination total reads the member count table (`gonof_common_member_count`): the
    // member's own posts that are not withdrawn or hidden, the same number for every viewer, including those in
    // private forums; entries the viewer cannot read are still not listed, so the total may exceed what is listed
    // and the last few pages may not be full.
    let counted = |first: i64| {
        format!(
            "SELECT COUNT(*) FROM gonof_forum_post WHERE authorid={} AND first={first} AND invisible=0 AND deleted_at=0",
            fixture.uid
        )
    };
    let stats = content::member_counts(&fixture.db, fixture.uid)
        .await
        .unwrap();
    for (total, first) in [(stats.threads, 1), (stats.replies, 0)] {
        let raw: i64 = gono_forum_web::db::query_scalar(&counted(first))
            .fetch_one(fixture.db.executor())
            .await
            .unwrap();
        assert_eq!(total, usize::try_from(raw).unwrap());
    }
    assert!(stats.threads > threads.len() && stats.replies > posts.len());
    for (view, total, expected) in [
        ("threads", stats.threads, threads),
        ("posts", stats.replies, posts),
    ] {
        let pages = total.div_ceil(20).max(1);
        let mut rendered = Vec::new();
        for page in 1..=pages {
            let suffix = if page == 1 {
                String::new()
            } else {
                format!("-{page}")
            };
            let body = fixture
                .page(
                    &format!("{}/{view}{suffix}", urls::space(fixture.uid)),
                    false,
                )
                .await;
            assert!(body.contains(&format!("data-member-total=\"{total}\"")));
            rendered.extend(listed_pids(&body));
            for hidden in [
                "MEMBER_TOPIC_103",
                "MEMBER_OPENING_103",
                "MEMBER_TOPIC_201",
                "MEMBER_TOPIC_202",
                "MEMBER_REPLY_103002",
                "MEMBER_REPLY_103003",
                "MEMBER_REPLY_103004",
                "MEMBER_REPLY_103056",
                "MEMBER_REPLY_103076",
            ] {
                assert!(!body.contains(hidden), "{hidden} leaked into {view}");
            }
        }
        assert_eq!(
            rendered,
            expected.iter().map(|post| post.pid).collect::<Vec<_>>()
        );
        assert_eq!(
            support::fetch(
                &fixture.app,
                &format!("{}/{view}-{}", urls::space(fixture.uid), pages + 1),
                false
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
    }
    gono_forum_web::db::query("UPDATE gonof_forum_post SET invisible=-1 WHERE tid=103 AND first=1")
        .execute(fixture.db.executor())
        .await
        .unwrap();
    gono_forum_web::test_support::refresh_listing(&fixture.db)
        .await
        .unwrap();
    assert!(fixture.all(MemberContentKind::Posts, 0).await.is_empty());
    let empty = fixture
        .page(&format!("{}/posts", urls::space(fixture.uid)), false)
        .await;
    assert!(listed_pids(&empty).is_empty());
    fixture.services.cleanup().await.unwrap();
}
