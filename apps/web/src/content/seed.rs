use crate::db::Tx;
use std::collections::{BTreeMap, HashMap};

use anyhow::{Context, Result, ensure};
use unicode_normalization::UnicodeNormalization;

use crate::db::Database;

use super::{Post, Thread, fixtures};

/// A forum of the development and test fixtures.
pub struct FixtureForum {
    pub name: &'static str,
    pub slug: &'static str,
    pub description: &'static str,
    pub guidance: &'static str,
    /// `#rrggbb` in lower case, as the board form stores it (docs/skins.md, "Board colors and glyphs").
    pub color: &'static str,
    /// A built-in glyph name (`organization::glyphs`).
    pub glyph: &'static str,
}

/// The 6 forums of the development and test fixtures, each with a color and a glyph so that skins which use them
/// (docs/skins.md, "Built-in skins") show them.
///
/// **Fixtures only** (tests and the development data imported by `gono-forum-cli seed-demo` attach threads to these
/// slugs). A fresh install starts with no forums, and the administrator creates them by hand at `/manage/forums`,
/// unless the optional sample content was chosen in the setup wizard ([`import_sample`]), which brings forums of its
/// own.
pub const FIXTURE_FORUMS: [FixtureForum; 6] = [
    FixtureForum {
        name: "Daily",
        slug: "daily",
        description: "Everyday chat, work and light conversation",
        guidance: "Check-ins, questions, notes from work and everyday life.",
        color: "#2f6fde",
        glyph: "coffee",
    },
    FixtureForum {
        name: "Showcase",
        slug: "showcase",
        description: "Projects, open source, gadgets and write-ups",
        guidance: "Share what you built: open-source releases, launches and tinkering notes.",
        color: "#e07a1f",
        glyph: "star",
    },
    FixtureForum {
        name: "AI",
        slug: "ai",
        description: "Models, prompts, AI-assisted coding and agents",
        guidance: "Model APIs, running models locally, prompting and workflow automation.",
        color: "#8656d8",
        glyph: "lightbulb",
    },
    FixtureForum {
        name: "Hosting",
        slug: "hosting",
        description: "Servers, cloud, domains, networking and operations",
        guidance: "Server reviews, containers, DNS, reverse proxies and operations practice.",
        color: "#1f9a9a",
        glyph: "globe",
    },
    FixtureForum {
        name: "Releases",
        slug: "releases",
        description: "Release notes and milestones of community projects",
        guidance: "Release records, changelogs, milestones and major announcements.",
        color: "#2e9d57",
        glyph: "rocket",
    },
    FixtureForum {
        name: "Meta",
        slug: "meta",
        description: "Announcements, rules, bug reports and feature requests",
        guidance: "Questions about the community, interface suggestions, rules and announcements.",
        color: "#d6456f",
        glyph: "megaphone",
    },
];

/// For fixtures: creates [`FIXTURE_FORUMS`] only when the forum table is empty; does nothing if the table already has
/// forums.
///
/// Missing forums must not be filled in by slug: after a slug is changed the original slug is "missing", and a forum
/// with the same name would be created again. `ON DUPLICATE KEY` is kept only so the later caller of two concurrent
/// calls does not error.
pub async fn seed_forums(db: &Database) -> Result<()> {
    let mut transaction = db.begin_write().await?;
    let existing = crate::db::query("SELECT fid FROM gonof_forum_forum LIMIT 1")
        .fetch_optional(db.observe(&mut *transaction))
        .await?;
    if existing.is_some() {
        return Ok(());
    }
    let suffix = "ON DUPLICATE KEY UPDATE slug = slug";
    let sql = &format!(
        "INSERT INTO gonof_forum_forum (slug, name, description, guidance, displayorder, color, glyph)
         VALUES (?, ?, ?, ?, ?, ?, ?) {suffix}"
    );
    for (order, forum) in FIXTURE_FORUMS.iter().enumerate() {
        // Row rules such as name length are checked in code (there is no validation trigger); they share one set of
        // checks with saving in the admin area.
        ensure!(
            crate::organization::forum_fields(forum.name, forum.slug, "public", 1).is_some(),
            "fixture forum name or address is wrong: {}",
            forum.slug
        );
        let (color, glyph) = board_look(forum.color, forum.glyph, forum.slug)?;
        crate::db::query(sql)
            .bind(forum.slug)
            .bind(forum.name)
            .bind(forum.description)
            .bind(forum.guidance)
            .bind(i64::try_from(order)?)
            .bind(color)
            .bind(glyph)
            .execute(db.observe(&mut *transaction))
            .await?;
    }
    transaction.commit().await?;
    Ok(())
}

/// Development fixture (`gono-forum-cli seed-demo`): when the forum table is empty, first creates the fixture forums,
/// then imports the fixture threads.
pub async fn seed_fixture(db: &Database) -> Result<()> {
    seed_forums(db).await?;
    import_fixture(db, &fixtures()).await
}

/// Explicitly imports stable development data; content, members, PIDs and counts are all committed in the same write
/// transaction.
/// Stacking fixtures onto, or overwriting, a database that already has business data is not allowed.
pub async fn import_fixture(db: &Database, threads: &[Thread]) -> Result<()> {
    let mut transaction = db.begin_write().await?;
    for table in [
        "gonof_common_member",
        "gonof_forum_thread",
        "gonof_forum_post",
        "gonof_forum_post_tableid",
    ] {
        let count: i64 = crate::db::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(db.observe(&mut *transaction))
            .await?;
        ensure!(
            count == 0,
            "the development fixture only imports into an empty content database: {table} has data"
        );
    }
    let mut members = BTreeMap::new();
    for thread in threads {
        members
            .entry(normalize(&thread.author))
            .or_insert(thread.avatar);
        for post in &thread.posts {
            members
                .entry(normalize(&post.author))
                .or_insert(post.avatar);
        }
    }
    let member_sql = "INSERT INTO gonof_common_member (uid, username, avatar) VALUES (?, ?, ?)";
    let mut member_ids = HashMap::new();
    for (index, (username, avatar)) in members.into_iter().enumerate() {
        // Member row rules, checked in code (there is no validation trigger): username 1–50
        // characters with no leading or trailing whitespace (normalize trims first and then applies NFKC; a few
        // compatibility characters expand to a leading space, which is caught here); synthetic authors have no
        // password, email or administrative role.
        crate::identity::rules::check_username(&username)
            .context("invalid development fixture username")?;
        let uid = i64::try_from(index + 1)?;
        crate::db::query(member_sql)
            .bind(uid)
            .bind(&username)
            .bind(i64::try_from(avatar)?)
            .execute(db.observe(&mut *transaction))
            .await?;
        member_ids.insert(username, uid);
    }
    write_threads(db, &mut transaction, threads, &member_ids).await?;
    super::copies::rebuild_listing(db, &mut transaction).await?;
    transaction.commit().await?;
    Ok(())
}

/// Content tables that must be empty before the sample content or a content dataset goes in: both bring their own
/// forums and number their threads and posts from 1.
const CONTENT_TABLES: [&str; 4] = [
    "gonof_forum_forum",
    "gonof_forum_thread",
    "gonof_forum_post",
    "gonof_forum_post_tableid",
];

/// Whether [`import_sample`] can run: the site has no forums, threads or posts yet. Members are allowed (the setup
/// wizard creates the administrator first).
pub async fn sample_fits(db: &Database) -> Result<bool> {
    for table in CONTENT_TABLES {
        let row = crate::db::query(&format!("SELECT 1 FROM {table} LIMIT 1"))
            .fetch_optional(db.executor())
            .await?;
        if row.is_some() {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Refuses, inside the import's first transaction, a site that already has forums, threads or posts.
///
/// Imports go one at a time: the administrators' lock row comes first (as for other one-at-a-time saves), then reads
/// of the latest committed rows. Without it two MySQL imports into an empty site both found it empty, both committed
/// their forums and members, and the one that then lost the race for thread 1 left all of them behind.
async fn ensure_no_content(db: &Database, transaction: &mut Tx<'static>, what: &str) -> Result<()> {
    crate::db::query(
        "SELECT skey FROM gonof_common_settings WHERE skey='identity.bootstrap_admin' FOR UPDATE",
    )
    .fetch_one(db.observe(&mut **transaction))
    .await?;
    for table in CONTENT_TABLES {
        let row = crate::db::query(&format!("SELECT 1 FROM {table} LIMIT 1 LOCK IN SHARE MODE"))
            .fetch_optional(db.observe(&mut **transaction))
            .await?;
        ensure!(
            row.is_none(),
            "{what} only goes into a site without forums, threads or posts: {table} has data"
        );
    }
    Ok(())
}

/// A forum for [`import_sample`] or [`import_dataset`]; `parent` is the slug of a top-level forum earlier in the same
/// list. `color` and `glyph` may be empty (docs/skins.md, "Board colors and glyphs").
pub(crate) struct NewForum {
    pub slug: String,
    pub name: String,
    pub description: String,
    pub guidance: String,
    pub parent: Option<String>,
    pub color: String,
    pub glyph: String,
}

/// A forum's color and glyph as the board form stores them, through the same checks (`organization::board_color`,
/// `organization::board_glyph`): either may be empty, and a color is kept in lower case. Anything the form would refuse
/// stops the import.
fn board_look(color: &str, glyph: &str, slug: &str) -> Result<(String, String)> {
    let color = crate::organization::board_color(color)
        .with_context(|| format!("forum {slug}: the color {color:?} is not #rrggbb"))?;
    let glyph = crate::organization::board_glyph(glyph)
        .with_context(|| format!("forum {slug}: {glyph:?} is not a built-in glyph"))?;
    Ok((color, glyph))
}

/// The username limit of `identity::rules::check_username`, in characters.
const USERNAME_MAX_CHARS: usize = 50;

/// A synthetic author for [`import_sample`] or [`import_dataset`]: no password, e-mail or role, so nobody can sign in
/// as them. `username` is the preferred name in stored form (lowercase).
pub(crate) struct NewMember {
    pub username: String,
    pub bio: String,
    pub avatar: usize,
    pub regdate: i64,
}

/// A member written by [`import_dataset`], for `gono-forum-cli import-content --members-out`.
#[derive(Clone, Debug)]
pub struct ImportedMember {
    pub uid: i64,
    pub username: String,
    pub bio: String,
    pub regdate: i64,
}

/// Creates `forums` in order, the list position as the display order. Organization rules the admin area enforces are
/// checked here too (there is no validation trigger): the name and address, the color and glyph, at most two levels,
/// at most [`crate::organization::MAX_CHILD_FORUMS`] children per parent and at most
/// [`crate::organization::MAX_TOP_FORUMS`] top-level forums (the site has none before an import).
async fn insert_forums(
    db: &Database,
    transaction: &mut Tx<'static>,
    forums: &[NewForum],
) -> Result<()> {
    let sql = "INSERT INTO gonof_forum_forum (slug, name, description, guidance, displayorder, parent_fid, color, glyph)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)";
    // slug -> (fid, whether it is top-level, children so far)
    let mut created: HashMap<&str, (i64, bool, i64)> = HashMap::new();
    let tops = forums.iter().filter(|forum| forum.parent.is_none()).count();
    ensure!(
        i64::try_from(tops)? <= crate::organization::MAX_TOP_FORUMS,
        "more than {} top-level forums",
        crate::organization::MAX_TOP_FORUMS
    );
    for (order, forum) in forums.iter().enumerate() {
        ensure!(
            crate::organization::forum_fields(&forum.name, &forum.slug, "public", 1)
                .is_some_and(|(_, slug)| slug == forum.slug),
            "forum name or address is wrong: {}",
            forum.slug
        );
        let (color, glyph) = board_look(&forum.color, &forum.glyph, &forum.slug)?;
        let parent = match &forum.parent {
            None => None,
            Some(parent) => {
                let (fid, top_level, children) =
                    created.get_mut(parent.as_str()).with_context(|| {
                        format!(
                            "the parent {parent} of forum {} is not created before it",
                            forum.slug
                        )
                    })?;
                ensure!(*top_level, "forum {} would be a third level", forum.slug);
                *children += 1;
                ensure!(
                    *children <= crate::organization::MAX_CHILD_FORUMS,
                    "forum {parent} would have more than {} child forums",
                    crate::organization::MAX_CHILD_FORUMS
                );
                Some(*fid)
            }
        };
        crate::db::query(sql)
            .bind(&forum.slug)
            .bind(&forum.name)
            .bind(&forum.description)
            .bind(&forum.guidance)
            .bind(i64::try_from(order)?)
            .bind(parent)
            .bind(color)
            .bind(glyph)
            .execute(db.observe(&mut **transaction))
            .await?;
        let fid: i64 = crate::db::query_scalar("SELECT fid FROM gonof_forum_forum WHERE slug = ?")
            .bind(&forum.slug)
            .fetch_one(db.observe(&mut **transaction))
            .await?;
        created.insert(&forum.slug, (fid, parent.is_none(), 0));
    }
    Ok(())
}

/// Creates `members` in order; they take the next free uids after the existing members. A preferred name that is
/// already taken (say, by the administrator) gets the lowest free number appended, shortened first when the name and
/// the number would pass the username limit. Returns the members as written, in the same order.
async fn insert_members(
    db: &Database,
    transaction: &mut Tx<'static>,
    members: &[NewMember],
) -> Result<Vec<ImportedMember>> {
    let taken_sql = "SELECT uid FROM gonof_common_member WHERE username = ?";
    let member_sql =
        "INSERT INTO gonof_common_member (username, avatar, bio, regdate) VALUES (?, ?, ?, ?)";
    let mut written = Vec::with_capacity(members.len());
    for member in members {
        let preferred = normalize(&member.username);
        let mut username = preferred.clone();
        let mut suffix = 1;
        while crate::db::query(taken_sql)
            .bind(&username)
            .fetch_optional(db.observe(&mut **transaction))
            .await?
            .is_some()
        {
            suffix += 1;
            ensure!(suffix < 1000, "no free name for member {preferred}");
            username = numbered(&preferred, suffix);
        }
        // Member row rules, checked in code (there is no validation trigger): the username, and no password, e-mail
        // or role for a synthetic author.
        crate::identity::rules::check_username(&username).context("invalid member username")?;
        crate::db::query(member_sql)
            .bind(&username)
            .bind(i64::try_from(member.avatar)?)
            .bind(&member.bio)
            .bind(member.regdate)
            .execute(db.observe(&mut **transaction))
            .await?;
        let uid: i64 = crate::db::query_scalar(taken_sql)
            .bind(&username)
            .fetch_one(db.observe(&mut **transaction))
            .await?;
        written.push(ImportedMember {
            uid,
            username,
            bio: member.bio.clone(),
            regdate: member.regdate,
        });
    }
    Ok(written)
}

/// `name` with `number` appended, the name shortened so the result stays within the username limit.
fn numbered(name: &str, number: usize) -> String {
    let number = number.to_string();
    let room = USERNAME_MAX_CHARS.saturating_sub(number.len());
    format!("{}{number}", name.chars().take(room).collect::<String>())
}

/// Looks up uids by the preferred names the threads were written with.
fn member_ids(members: &[NewMember], written: &[ImportedMember]) -> HashMap<String, i64> {
    members
        .iter()
        .zip(written)
        .map(|(member, written)| (normalize(&member.username), written.uid))
        .collect()
}

/// Adds the optional sample content (`content::sample`, docs/install.md, "Setup wizard") in `lang`, one of
/// `sample::languages()`: its forums, members, threads and replies, dated during the two months before `now`, in one
/// write transaction. Refused unless the site has no forums, threads or posts yet.
pub async fn import_sample(db: &Database, now: i64, lang: crate::i18n::Lang) -> Result<()> {
    let plan = super::sample::plan(now, lang)?;
    let mut transaction = db.begin_write().await?;
    ensure_no_content(db, &mut transaction, "sample content").await?;
    insert_forums(db, &mut transaction, &plan.forums).await?;
    let written = insert_members(db, &mut transaction, &plan.members).await?;
    let ids = member_ids(&plan.members, &written);
    write_threads(db, &mut transaction, &plan.threads, &ids).await?;
    super::copies::rebuild_listing(db, &mut transaction).await?;
    transaction.commit().await?;
    Ok(())
}

/// Threads written per transaction by [`import_dataset`]: large enough to keep the import quick, small enough that
/// SQLite's single writer and MySQL's undo log are never held for long.
const DATASET_BATCH: usize = 200;

/// Imports a content dataset that passed `content::dataset::check` (docs/content-import.md): forums and members in
/// one transaction, then the threads [`DATASET_BATCH`] at a time, then the listing copies and counts once at the end.
/// Refused unless the site has no forums, threads or posts yet. `progress` hears the number of threads written so far
/// and the total after each batch.
///
/// The import is not one transaction: if it stops halfway, the site keeps what was written, and the refusal above
/// keeps a second run from stacking on it; start again from a fresh database.
pub async fn import_dataset(
    db: &Database,
    dataset: &super::dataset::Dataset,
    now: i64,
    mut progress: impl FnMut(usize, usize),
) -> Result<Vec<ImportedMember>> {
    let plan = super::dataset::plan(dataset, now)?;
    let mut transaction = db.begin_write().await?;
    ensure_no_content(db, &mut transaction, "a content dataset").await?;
    insert_forums(db, &mut transaction, &plan.forums).await?;
    let written = insert_members(db, &mut transaction, &plan.members).await?;
    transaction.commit().await?;
    let ids = member_ids(&plan.members, &written);
    let total = plan.threads.len();
    let mut done = 0;
    for batch in plan.threads.chunks(DATASET_BATCH) {
        let mut transaction = db.begin_write().await?;
        write_threads(db, &mut transaction, batch, &ids).await?;
        transaction.commit().await?;
        done += batch.len();
        progress(done, total);
    }
    let mut transaction = db.begin_write().await?;
    super::copies::rebuild_listing(db, &mut transaction).await?;
    transaction.commit().await?;
    Ok(written)
}

/// Writes `threads` with their posts. Authors are looked up in `member_ids` by their normalized name and forums by
/// slug; threads and posts keep the ids they carry. A thread with `first_pid` set uses it for its first post; the
/// others take the free PIDs after the highest reply PID. Hidden state, counts and the parent-floor copy of nested
/// replies are left to the caller's `copies::rebuild_listing` once everything is written. Every post gets a growth row
/// that earns nothing for the import itself (`growth::record_imported_post`).
async fn write_threads(
    db: &Database,
    transaction: &mut Tx<'static>,
    threads: &[Thread],
    member_ids: &HashMap<String, i64>,
) -> Result<()> {
    let forums: HashMap<String, i64> = crate::db::query("SELECT slug, fid FROM gonof_forum_forum")
        .fetch_all(db.observe(&mut **transaction))
        .await?
        .into_iter()
        .map(|row| Ok((row.try_get("slug")?, row.try_get("fid")?)))
        .collect::<Result<_>>()?;
    let growth = crate::runtime::read_in(db, transaction).await?.growth;
    // Versioned reply IDs stay unchanged; first posts use the unallocated global PIDs after them.
    let mut next_first_pid = threads
        .iter()
        .flat_map(|thread| &thread.posts)
        .map(|post| post.pid)
        .max()
        .unwrap_or(0);
    // Denormalized copy columns are written all at once (`content::copies`); the three permission columns and the
    // top-level forum ID have no defaults. Hidden state, counts and the parent-floor copy of nested replies are
    // recomputed by `copies::rebuild_listing` after everything is written, following the rules of the write paths.
    let thread_sql = "INSERT INTO gonof_forum_thread
         (tid, fid, authorid, subject, dateline, lastpost, views, displayorder, digest, replies, maxposition, root_replies,
          author, family_fid)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)";
    for thread in threads {
        let fid = *forums.get(&thread.fixture_forum).context(
            "the development fixture refers to forums that do not exist; seed the forums first",
        )?;
        let forum = super::copies::ForumCopies::load(db, transaction, fid).await?;
        let tid = i64::try_from(thread.id)?;
        // Fixtures write primary keys explicitly; the session has NO_AUTO_VALUE_ON_ZERO on, so 0 would be stored as is
        // (no trigger backstops this).
        ensure!(
            tid > 0,
            "development fixture thread ids must be positive: {tid}"
        );
        let uid = *member_ids
            .get(&normalize(&thread.author))
            .with_context(|| format!("thread {tid} has an unknown author {}", thread.author))?;
        let author = super::copies::AuthorCopies::load(db, transaction, uid).await?;
        let maxposition = thread
            .posts
            .iter()
            .map(|post| post.position)
            .max()
            .unwrap_or(1);
        crate::db::query(thread_sql)
            .bind(tid)
            .bind(fid)
            .bind(uid)
            .bind(&thread.title)
            .bind(i64::try_from(thread.created)?)
            .bind(i64::try_from(thread.updated)?)
            .bind(i64::try_from(thread.views)?)
            .bind(i64::from(thread.pinned))
            .bind(i64::from(thread.featured))
            .bind(i64::try_from(thread.posts.len())?)
            .bind(i64::try_from(maxposition)?)
            .bind(i64::try_from(
                thread.posts.iter().filter(|p| p.root_pid.is_none()).count(),
            )?)
            .bind(&author.username)
            .bind(forum.family_fid)
            .execute(db.observe(&mut **transaction))
            .await?;
        let first_pid = if thread.first_pid > 0 {
            u64::try_from(thread.first_pid)?
        } else {
            next_first_pid = next_first_pid
                .checked_add(1)
                .context("development fixture PID out of range")?;
            next_first_pid
        };
        insert_post(
            db,
            transaction,
            fid,
            tid,
            uid,
            true,
            &Post {
                pid: first_pid,
                position: 1,
                root_position: 0,
                sub_position: 0,
                sub_replies_count: 0,
                root_floor: 0,
                parent_label: String::new(),
                root_pid: None,
                reply_to_pid: None,
                author: thread.author.clone(),
                avatar: thread.avatar,
                avatar_aid: 0,
                body: thread.body.join("\n\n"),
                time: String::new(),
                dateline: thread.created,
                author_deleted: false,
                authorid: Some(uid),
                version: 1,
                withdrawn: false,
                erased: false,
                blocked: false,
                hidden: false,
                edited_at: 0,
            },
            0,
        )
        .await?;
        crate::growth::record_imported_post(
            db,
            transaction,
            &growth,
            uid,
            i64::try_from(first_pid)?,
            true,
            thread.featured,
            i64::try_from(thread.created)?,
        )
        .await?;
        let mut posts: Vec<_> = thread.posts.iter().collect();
        posts.sort_by_key(|post| post.position);
        let mut root_ordinal = 0_i64;
        let mut children_ordinals = HashMap::<u64, i64>::new();
        let children_counts = nested_counts(&thread.posts);
        for post in posts {
            let uid = *member_ids.get(&normalize(&post.author)).with_context(|| {
                format!("post {} has an unknown author {}", post.pid, post.author)
            })?;
            let children = children_counts.get(&post.pid).copied().unwrap_or(0);
            let mut post = post.clone();
            if let Some(root) = post.root_pid {
                let ordinal = children_ordinals.entry(root).or_default();
                *ordinal += 1;
                post.sub_position = *ordinal as usize;
            } else {
                root_ordinal += 1;
                post.root_position = root_ordinal as usize;
            }
            insert_post(db, transaction, fid, tid, uid, false, &post, children).await?;
            crate::growth::record_imported_post(
                db,
                transaction,
                &growth,
                uid,
                i64::try_from(post.pid)?,
                false,
                false,
                i64::try_from(post.dateline)?,
            )
            .await?;
        }
        super::recount_effective_replies(db, transaction, tid).await?;
    }
    Ok(())
}

/// The number of nested replies under each floor's first reply, counted in one pass: a thread with 10,000 replies
/// would otherwise be scanned once per reply inside the write transaction.
fn nested_counts(posts: &[Post]) -> HashMap<u64, usize> {
    let mut counts = HashMap::new();
    for root in posts.iter().filter_map(|post| post.root_pid) {
        *counts.entry(root).or_default() += 1;
    }
    counts
}

fn normalize(value: &str) -> String {
    value.trim().nfkc().collect::<String>().to_lowercase()
}

#[allow(clippy::too_many_arguments)]
async fn insert_post(
    db: &Database,
    transaction: &mut Tx<'static>,
    fid: i64,
    tid: i64,
    uid: i64,
    first: bool,
    post: &Post,
    children: usize,
) -> Result<()> {
    let pid = i64::try_from(post.pid)?;
    ensure!(
        pid > 0,
        "development fixture post ids must be positive: {pid}"
    );
    let shape = super::writes::PostShape {
        pid,
        tid,
        position: i64::try_from(post.position)?,
        root_pid: post.root_pid.map(i64::try_from).transpose()?,
        reply_to_pid: post.reply_to_pid.map(i64::try_from).transpose()?,
        root_position: (!first && post.root_pid.is_none())
            .then(|| i64::try_from(post.root_position))
            .transpose()?,
        sub_position: post
            .root_pid
            .map(|_| i64::try_from(post.sub_position))
            .transpose()?,
        sub_replies_count: i64::try_from(children)?,
    };
    ensure!(
        shape.first() == first,
        "the first post of a development fixture thread must be floor 1: {pid}"
    );
    shape.check()?;
    let allocator_sql = "INSERT INTO gonof_forum_post_tableid (pid) VALUES (?)";
    crate::db::query(allocator_sql)
        .bind(pid)
        .execute(db.observe(&mut **transaction))
        .await?;
    let sql = "INSERT INTO gonof_forum_post
         (pid, fid, tid, authorid, first, first_tid, position, root_pid, reply_to_pid, message, dateline, sub_replies_count,root_position,sub_position)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,?,?)";
    crate::db::query(sql)
        .bind(pid)
        .bind(fid)
        .bind(tid)
        .bind(uid)
        .bind(i64::from(first))
        .bind(shape.first_tid())
        .bind(shape.position)
        .bind(shape.root_pid)
        .bind(shape.reply_to_pid)
        .bind(&post.body)
        .bind(i64::try_from(post.dateline)?)
        .bind(shape.sub_replies_count)
        .bind(shape.root_position)
        .bind(shape.sub_position)
        .execute(db.observe(&mut **transaction))
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nested replies are counted in one pass over the thread: the import used to scan the whole thread once per
    /// reply, so one floor with 60,000 nested replies took billions of comparisons inside the write transaction.
    #[test]
    fn nested_replies_are_counted_in_one_pass() {
        let mut posts = vec![Post {
            pid: 1,
            ..Post::default()
        }];
        posts.extend((2..=60_001).map(|pid| Post {
            pid,
            root_pid: Some(1),
            ..Post::default()
        }));
        posts.push(Post {
            pid: 70_000,
            ..Post::default()
        });
        posts.push(Post {
            pid: 70_001,
            root_pid: Some(70_000),
            ..Post::default()
        });
        let started = std::time::Instant::now();
        let counts = nested_counts(&posts);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "took {:?}",
            started.elapsed()
        );
        assert_eq!(counts.get(&1), Some(&60_000));
        assert_eq!(counts.get(&70_000), Some(&1));
        assert_eq!(counts.get(&2), None, "a nested reply opens no floor");
        assert_eq!(counts.len(), 2);
    }

    /// The fixture forums carry a color and a glyph each, stored as the board form would store them and all
    /// different, so a skin that uses them tells the boards apart.
    #[test]
    fn the_fixture_forums_have_distinct_colors_and_glyphs() {
        let mut colors = std::collections::BTreeSet::new();
        let mut glyphs = std::collections::BTreeSet::new();
        for forum in &FIXTURE_FORUMS {
            assert_eq!(
                board_look(forum.color, forum.glyph, forum.slug).unwrap(),
                (forum.color.to_owned(), forum.glyph.to_owned()),
                "{}",
                forum.slug
            );
            assert!(!forum.color.is_empty() && !forum.glyph.is_empty());
            assert!(colors.insert(forum.color), "{}", forum.slug);
            assert!(glyphs.insert(forum.glyph), "{}", forum.slug);
        }
    }

    /// Imported forums go through the board form's checks for color and glyph.
    #[test]
    fn a_forum_look_is_checked_like_the_board_form() {
        assert_eq!(
            board_look("", "", "a").unwrap(),
            (String::new(), String::new())
        );
        assert_eq!(
            board_look(" #0F766E ", "chat", "a").unwrap(),
            ("#0f766e".to_owned(), "chat".to_owned())
        );
        for (color, glyph) in [
            ("0f766e", ""),
            ("#0f766", ""),
            ("#0f766g", ""),
            ("teal", ""),
            ("", "kitten"),
            ("", "Chat"),
        ] {
            assert!(
                board_look(color, glyph, "a").is_err(),
                "{color:?} {glyph:?}"
            );
        }
    }

    /// A taken name with a number appended stays within the username limit.
    #[test]
    fn numbered_names_are_shortened_to_the_username_limit() {
        let long = "n".repeat(USERNAME_MAX_CHARS);
        assert_eq!(numbered(&long, 2), format!("{}2", "n".repeat(49)));
        assert_eq!(numbered(&long, 999), format!("{}999", "n".repeat(47)));
        assert_eq!(numbered("nora", 2), "nora2");
        let wide = "名".repeat(USERNAME_MAX_CHARS);
        assert_eq!(numbered(&wide, 12).chars().count(), USERNAME_MAX_CHARS);
    }
}
