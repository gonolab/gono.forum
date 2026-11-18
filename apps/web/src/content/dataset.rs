//! Content datasets for `gono-forum-cli check-content`, `import-content` and `export-sample` (docs/content-import.md):
//! boards, members, threads and the replies written for each thread, prepared outside the forum.
//!
//! [`check`] reads a dataset directory and reports every problem with its file, line and thread key, so a whole
//! dataset can be fixed in one pass; it needs no database or configuration. [`plan`] turns a dataset that passed into
//! rows with generated ids, times and view counts, which `seed::import_dataset` writes. [`write_sample`] writes the
//! setup wizard's sample content in this format, as a starting point for a dataset of one's own.
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::{
    Post, Thread,
    sample::{DAY, HOUR, Rng, reply_times},
    seed::{NewForum, NewMember},
    writes::{MAX_BODY_BYTES, MAX_TITLE_CHARS},
};

pub const FORMAT: &str = "gono-forum-content";
pub const VERSION: u32 = 1;
pub const MANIFEST_FILE: &str = "manifest.json";
pub const BOARDS_FILE: &str = "boards.json";
pub const USERS_FILE: &str = "users.jsonl";
pub const THREADS_DIR: &str = "threads";
/// More threads per file are accepted but pointed out: smaller files are easier to produce and check in batches.
const THREADS_PER_FILE: usize = 1000;
const KEY_MAX_CHARS: usize = 64;
const SLUG_MAX_BYTES: usize = 64;
const BOARD_NAME_MAX_CHARS: usize = 50;
/// Board descriptions and guidance are TEXT columns.
const BOARD_TEXT_MAX_BYTES: usize = 65_535;
const BIO_MAX_CHARS: usize = 500;
/// Threads start during this many days before the import.
const PERIOD_DAYS: i64 = 365;
/// Replies to a thread arrive within this many days of it.
const REPLY_DAYS: i64 = 30;
/// A member registers between one day and this many days before their first post.
const JOINED_BEFORE_DAYS: i64 = 60;
const AVATAR_VARIANTS: usize = 6;
const SEED: u64 = 0xc0de_da7a_5e7a_0001;
/// Names that read like staff accounts: allowed, but pointed out.
const STAFF_NAMES: &[&str] = &[
    "admin",
    "administrator",
    "moderator",
    "mod",
    "support",
    "staff",
    "system",
    "root",
    "official",
];

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: String,
    version: u32,
    language: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BoardEntry {
    slug: String,
    name: String,
    description: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    guidance: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct UserEntry {
    username: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    bio: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ThreadEntry {
    key: String,
    board: String,
    author: String,
    title: String,
    body: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pinned: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    featured: bool,
    replies: Vec<ReplyEntry>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReplyEntry {
    id: u64,
    author: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reply_to: Option<u64>,
    body: String,
}

/// One problem: where (file, line, thread key, reply id) and what.
#[derive(Clone, Debug)]
pub struct Problem {
    pub place: String,
    pub message: String,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.place, self.message)
    }
}

/// What [`check`] found. Errors stop an import; warnings do not.
#[derive(Debug, Default)]
pub struct Report {
    pub errors: Vec<Problem>,
    pub warnings: Vec<Problem>,
    pub boards: usize,
    pub members: usize,
    pub threads: usize,
    pub replies: usize,
    /// Replies that answer another reply.
    pub nested: usize,
}

impl Report {
    fn error(&mut self, place: impl Into<String>, message: impl Into<String>) {
        self.errors.push(Problem {
            place: place.into(),
            message: message.into(),
        });
    }

    fn warning(&mut self, place: impl Into<String>, message: impl Into<String>) {
        self.warnings.push(Problem {
            place: place.into(),
            message: message.into(),
        });
    }

    /// The report as printed by the command-line tool, with at most `limit` errors and `limit` warnings listed.
    pub fn render(&self, limit: usize) -> String {
        let mut text = format!(
            "{} boards, {} members, {} threads, {} replies ({} of them answer another reply).\n",
            self.boards, self.members, self.threads, self.replies, self.nested
        );
        for (label, problems) in [("errors", &self.errors), ("warnings", &self.warnings)] {
            if problems.is_empty() {
                continue;
            }
            text.push_str(&format!("\n{} {label}:\n", problems.len()));
            for problem in problems.iter().take(limit) {
                text.push_str(&format!("  {problem}\n"));
            }
            if problems.len() > limit {
                text.push_str(&format!("  ... and {} more\n", problems.len() - limit));
            }
        }
        text
    }
}

/// A dataset that passed [`check`], with names in stored form.
pub struct Dataset {
    boards: Vec<NewForum>,
    /// (username, bio)
    members: Vec<(String, String)>,
    threads: Vec<DatasetThread>,
}

struct DatasetThread {
    board: String,
    author: String,
    title: String,
    body: String,
    pinned: bool,
    featured: bool,
    replies: Vec<DatasetReply>,
}

struct DatasetReply {
    author: String,
    body: String,
    /// Index of the earlier reply it answers.
    reply_to: Option<usize>,
}

/// The result of [`check`]: the report, and the dataset when there were no errors.
pub struct Checked {
    pub report: Report,
    pub dataset: Option<Dataset>,
}

/// Reads a file as UTF-8 text with LF line endings, reporting what is wrong with it; `None` when it cannot be read.
fn read_text(report: &mut Report, dir: &Path, name: &str) -> Option<String> {
    let bytes = match std::fs::read(dir.join(name)) {
        Ok(bytes) => bytes,
        Err(error) => {
            report.error(name, format!("cannot be read: {error}"));
            return None;
        }
    };
    let Ok(text) = String::from_utf8(bytes) else {
        report.error(name, "is not valid UTF-8");
        return None;
    };
    if text.starts_with('\u{feff}') {
        report.error(
            name,
            "starts with a byte order mark; save it as UTF-8 without one",
        );
    }
    if text.contains('\r') {
        report.error(name, "has CR or CRLF line endings; use LF");
    }
    Some(text.trim_start_matches('\u{feff}').replace("\r\n", "\n"))
}

/// serde_json's message without its own "at line L column C" (its line is not the file's for JSON lines).
fn json_message(error: &serde_json::Error) -> String {
    let text = error.to_string();
    match text.rsplit_once(" at line ") {
        Some((message, _)) => format!("{message} (column {})", error.column()),
        None => text,
    }
}

/// One JSON object per line; returns (line number, value) for the lines that parse.
fn json_lines<T: DeserializeOwned>(report: &mut Report, name: &str, text: &str) -> Vec<(usize, T)> {
    let mut values = Vec::new();
    let body = text.strip_suffix('\n').unwrap_or(text);
    if body.is_empty() {
        return values;
    }
    for (index, line) in body.split('\n').enumerate() {
        let place = format!("{name}:{}", index + 1);
        if line.trim().is_empty() {
            report.error(place, "empty line; every line must hold one JSON object");
            continue;
        }
        match serde_json::from_str(line) {
            Ok(value) => values.push((index + 1, value)),
            Err(error) => report.error(place, json_message(&error)),
        }
    }
    values
}

/// What is wrong with a thread or reply body, if anything.
fn body_problem(body: &str) -> Option<String> {
    if body.trim().is_empty() {
        return Some("the body is empty".into());
    }
    if body.len() > MAX_BODY_BYTES {
        return Some(format!(
            "the body is {} bytes; at most {MAX_BODY_BYTES}",
            body.len()
        ));
    }
    if body.contains('\0') {
        return Some("the body contains a NUL character".into());
    }
    crate::markdown::imported_body_problem(body)
        .map(|problem| format!("the body contains {problem}"))
}

fn slug_problem(slug: &str) -> Option<&'static str> {
    if slug.is_empty() {
        Some("slug is empty")
    } else if slug.len() > SLUG_MAX_BYTES {
        Some("slug is longer than 64 characters")
    } else if !slug
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        Some("slug may only use lowercase letters, digits and -")
    } else if slug
        .rsplit_once('-')
        .is_some_and(|(_, n)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    {
        Some("slug cannot end in - and digits; it would clash with page addresses such as /help-2")
    } else if crate::urls::reserved_segment(slug) {
        Some("slug is one of the site's own addresses; choose another")
    } else {
        None
    }
}

fn check_boards(report: &mut Report, entries: Vec<BoardEntry>) -> Vec<NewForum> {
    let mut boards = Vec::with_capacity(entries.len());
    // slug -> (position, has a parent, children so far)
    let mut seen: HashMap<String, (usize, bool, i64)> = HashMap::new();
    let mut names: HashMap<String, usize> = HashMap::new();
    let mut tops = 0_i64;
    for (index, entry) in entries.into_iter().enumerate() {
        let position = index + 1;
        let place = format!("{BOARDS_FILE}, board {position} ({})", entry.slug);
        let mut valid = true;
        if entry.parent.is_none() {
            tops += 1;
            if tops == crate::organization::MAX_TOP_FORUMS + 1 {
                report.error(
                    &place,
                    format!(
                        "more than {} top-level boards",
                        crate::organization::MAX_TOP_FORUMS
                    ),
                );
                valid = false;
            }
        }
        if let Some(problem) = slug_problem(&entry.slug) {
            report.error(&place, problem);
            valid = false;
        } else if let Some((other, _, _)) = seen.get(&entry.slug) {
            report.error(&place, format!("slug is also used by board {other}"));
            valid = false;
        }
        let name = entry.name.trim();
        if name.is_empty() {
            report.error(&place, "name is empty");
            valid = false;
        } else if name.chars().count() > BOARD_NAME_MAX_CHARS {
            report.error(&place, "name is longer than 50 characters");
            valid = false;
        } else if let Some(other) = names.insert(name.to_lowercase(), position) {
            report.error(&place, format!("name is the same as board {other}"));
            valid = false;
        }
        if entry.description.trim().is_empty() {
            report.error(&place, "description is empty");
            valid = false;
        }
        for (field, text) in [
            ("description", &entry.description),
            ("guidance", &entry.guidance),
        ] {
            if text.len() > BOARD_TEXT_MAX_BYTES {
                report.error(
                    &place,
                    format!("{field} is longer than {BOARD_TEXT_MAX_BYTES} bytes"),
                );
                valid = false;
            }
        }
        if let Some(parent) = &entry.parent {
            match seen.get_mut(parent) {
                _ if *parent == entry.slug => {
                    report.error(&place, "a board cannot be its own parent");
                    valid = false;
                }
                None => {
                    report.error(
                        &place,
                        format!("parent {parent} is not a board listed before this one"),
                    );
                    valid = false;
                }
                Some((_, true, _)) => {
                    report.error(
                        &place,
                        format!("parent {parent} is a child board itself; boards have at most two levels"),
                    );
                    valid = false;
                }
                Some((_, false, children)) => {
                    *children += 1;
                    if *children > crate::organization::MAX_CHILD_FORUMS {
                        report.error(
                            &place,
                            format!(
                                "parent {parent} would have more than {} child boards",
                                crate::organization::MAX_CHILD_FORUMS
                            ),
                        );
                        valid = false;
                    }
                }
            }
        }
        if valid
            && crate::organization::forum_fields(name, &entry.slug, "public", 1)
                .is_none_or(|(_, slug)| slug != entry.slug)
        {
            report.error(&place, "name or slug is not accepted by the forum");
            valid = false;
        }
        seen.entry(entry.slug.clone())
            .or_insert((position, entry.parent.is_some(), 0));
        if valid {
            boards.push(NewForum {
                slug: entry.slug,
                name: name.to_owned(),
                description: entry.description,
                guidance: entry.guidance,
                parent: entry.parent,
            });
        }
    }
    boards
}

fn check_users(report: &mut Report, entries: Vec<(usize, UserEntry)>) -> Vec<(String, String)> {
    let mut members = Vec::with_capacity(entries.len());
    let mut seen: HashMap<String, usize> = HashMap::new();
    for (line, entry) in entries {
        let place = format!("{USERS_FILE}:{line} ({})", entry.username);
        let Ok(username) = crate::identity::normalize_username(&entry.username) else {
            report.error(
                place,
                "username must be 1-50 characters with no spaces, @, / or \\",
            );
            continue;
        };
        if let Some(other) = seen.insert(username.clone(), line) {
            report.error(
                &place,
                format!("username is the same as line {other} (names are compared in lowercase)"),
            );
            continue;
        }
        if STAFF_NAMES.contains(&username.as_str()) {
            report.warning(&place, "username reads like a staff account");
        }
        if entry.bio.chars().count() > BIO_MAX_CHARS {
            report.error(&place, "bio is longer than 500 characters");
            continue;
        }
        members.push((username, entry.bio));
    }
    members
}

/// The author's stored name if it is a member of the dataset.
fn author(
    report: &mut Report,
    place: &str,
    members: &HashSet<String>,
    name: &str,
) -> Option<String> {
    match crate::identity::normalize_username(name) {
        Ok(username) if members.contains(&username) => Some(username),
        _ => {
            report.error(place, format!("author {name} is not in {USERS_FILE}"));
            None
        }
    }
}

fn check_thread(
    report: &mut Report,
    place: &str,
    entry: ThreadEntry,
    boards: &HashSet<&str>,
    members: &HashSet<String>,
) -> Option<DatasetThread> {
    let mut valid = true;
    if !boards.contains(entry.board.as_str()) {
        report.error(
            place,
            format!("board {} is not in {BOARDS_FILE}", entry.board),
        );
        valid = false;
    }
    let thread_author = author(report, place, members, &entry.author);
    let title = entry.title.trim();
    if title.is_empty() {
        report.error(place, "title is empty");
        valid = false;
    } else if title.chars().count() > MAX_TITLE_CHARS {
        report.error(
            place,
            format!("title is longer than {MAX_TITLE_CHARS} characters"),
        );
        valid = false;
    } else if title.chars().any(char::is_control) {
        report.error(
            place,
            "title contains a control character such as a line break",
        );
        valid = false;
    }
    if let Some(problem) = body_problem(&entry.body) {
        report.error(place, problem);
        valid = false;
    }
    let mut replies = Vec::with_capacity(entry.replies.len());
    // reply id -> (index, author)
    let mut ids: HashMap<u64, (usize, Option<String>)> = HashMap::new();
    for (index, reply) in entry.replies.into_iter().enumerate() {
        let reply_place = format!("{place} reply {}", reply.id);
        if reply.id == 0 {
            report.error(&reply_place, "reply id must be a positive integer");
            valid = false;
        } else if ids.contains_key(&reply.id) {
            report.error(&reply_place, "reply id appears twice in this thread");
            valid = false;
        }
        let reply_author = author(report, &reply_place, members, &reply.author);
        if let Some(problem) = body_problem(&reply.body) {
            report.error(&reply_place, problem);
            valid = false;
        }
        let reply_to = match reply.reply_to {
            None => None,
            Some(target) => match ids.get(&target) {
                Some((target_index, target_author)) => {
                    if reply_author.is_some() && *target_author == reply_author {
                        report.warning(
                            &reply_place,
                            format!("answers a reply by the same author (reply {target})"),
                        );
                    }
                    Some(*target_index)
                }
                None => {
                    report.error(
                        &reply_place,
                        format!("reply_to {target} is not an earlier reply in this thread"),
                    );
                    valid = false;
                    None
                }
            },
        };
        report.replies += 1;
        if reply.reply_to.is_some() {
            report.nested += 1;
        }
        valid &= reply_author.is_some();
        ids.entry(reply.id).or_insert((index, reply_author.clone()));
        replies.push(DatasetReply {
            author: reply_author.unwrap_or_default(),
            body: reply.body,
            reply_to,
        });
    }
    (valid && thread_author.is_some()).then(|| DatasetThread {
        board: entry.board,
        author: thread_author.unwrap_or_default(),
        title: title.to_owned(),
        body: entry.body,
        pinned: entry.pinned,
        featured: entry.featured,
        replies,
    })
}

/// Reads and checks the dataset in `dir` (docs/content-import.md, "Checks"). Never touches a database.
pub fn check(dir: &Path) -> Checked {
    let mut report = Report::default();
    if let Some(text) = read_text(&mut report, dir, MANIFEST_FILE) {
        match serde_json::from_str::<Manifest>(&text) {
            Ok(manifest) => {
                if manifest.format != FORMAT {
                    report.error(MANIFEST_FILE, format!("format must be \"{FORMAT}\""));
                }
                if manifest.version != VERSION {
                    report.error(
                        MANIFEST_FILE,
                        format!(
                            "version {} is not supported; this tool reads version {VERSION}",
                            manifest.version
                        ),
                    );
                }
                if manifest.language.trim().is_empty() {
                    report.error(MANIFEST_FILE, "language is empty");
                }
            }
            Err(error) => report.error(
                format!("{MANIFEST_FILE}:{}", error.line()),
                json_message(&error),
            ),
        }
    }
    let boards = match read_text(&mut report, dir, BOARDS_FILE) {
        Some(text) => match serde_json::from_str::<Vec<BoardEntry>>(&text) {
            Ok(entries) => {
                if entries.is_empty() {
                    report.error(BOARDS_FILE, "lists no boards");
                }
                report.boards = entries.len();
                check_boards(&mut report, entries)
            }
            Err(error) => {
                report.error(
                    format!("{BOARDS_FILE}:{}", error.line()),
                    json_message(&error),
                );
                Vec::new()
            }
        },
        None => Vec::new(),
    };
    let members = match read_text(&mut report, dir, USERS_FILE) {
        Some(text) => {
            let entries = json_lines::<UserEntry>(&mut report, USERS_FILE, &text);
            if entries.is_empty() {
                report.error(USERS_FILE, "lists no users");
            }
            report.members = entries.len();
            check_users(&mut report, entries)
        }
        None => Vec::new(),
    };
    let board_slugs: HashSet<&str> = boards.iter().map(|board| board.slug.as_str()).collect();
    let member_names: HashSet<String> = members.iter().map(|(name, _)| name.clone()).collect();

    let mut files: Vec<String> = match std::fs::read_dir(dir.join(THREADS_DIR)) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect(),
        Err(error) => {
            report.error(
                format!("{THREADS_DIR}/"),
                format!("cannot be read: {error}"),
            );
            Vec::new()
        }
    };
    files.sort();
    let mut threads = Vec::new();
    let mut keys: HashMap<String, String> = HashMap::new();
    let mut thread_files = 0;
    for file in files {
        let name = format!("{THREADS_DIR}/{file}");
        if !file.ends_with(".jsonl") {
            if !file.starts_with('.') {
                report.warning(&name, "is not a .jsonl file and is skipped");
            }
            continue;
        }
        thread_files += 1;
        let Some(text) = read_text(&mut report, dir, &name) else {
            continue;
        };
        let entries = json_lines::<ThreadEntry>(&mut report, &name, &text);
        if entries.len() > THREADS_PER_FILE {
            report.warning(
                &name,
                format!(
                    "holds {} threads; at most {THREADS_PER_FILE} per file is easier to handle",
                    entries.len()
                ),
            );
        }
        for (line, entry) in entries {
            report.threads += 1;
            let place = format!("{name}:{line} {}", entry.key);
            let key_valid = (1..=KEY_MAX_CHARS).contains(&entry.key.len())
                && entry
                    .key
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-');
            if !key_valid {
                report.error(&place, "key must be 1-64 letters, digits or -");
            } else if let Some(other) = keys.get(&entry.key) {
                report.error(&place, format!("key is also used at {other}"));
            } else {
                keys.insert(entry.key.clone(), format!("{name}:{line}"));
            }
            if let Some(thread) =
                check_thread(&mut report, &place, entry, &board_slugs, &member_names)
                && key_valid
            {
                threads.push(thread);
            }
        }
    }
    if thread_files == 0
        && report
            .errors
            .iter()
            .all(|problem| problem.place != format!("{THREADS_DIR}/"))
    {
        report.error(format!("{THREADS_DIR}/"), "has no .jsonl files");
    }
    let dataset = report.errors.is_empty().then_some(Dataset {
        boards,
        members,
        threads,
    });
    Checked { report, dataset }
}

/// Forums, members and threads ready for `seed::import_dataset`.
pub(crate) struct Plan {
    pub forums: Vec<NewForum>,
    pub members: Vec<NewMember>,
    pub threads: Vec<Thread>,
}

/// Gives the dataset ids, times and view counts as if it had been written during the [`PERIOD_DAYS`] before `now`.
/// Threads are numbered in the order they were started, each thread's first post followed by its replies; members are
/// numbered in the order they joined, each one shortly before their first post. Everything is the same on every run
/// except for the shift with `now`.
pub(crate) fn plan(dataset: &Dataset, now: i64) -> Result<Plan> {
    let mut rng = Rng(SEED);
    let mut order: Vec<(i64, usize)> = dataset
        .threads
        .iter()
        .enumerate()
        .map(|(index, _)| (now - rng.between(HOUR, PERIOD_DAYS * DAY), index))
        .collect();
    order.sort();
    let mut first_post: HashMap<&str, i64> = HashMap::new();
    let mut threads = Vec::with_capacity(dataset.threads.len());
    let mut next_pid = 1_u64;
    for (number, (created, index)) in order.into_iter().enumerate() {
        let source = &dataset.threads[index];
        let first_pid = next_pid;
        next_pid += 1;
        let times = reply_times(&mut rng, created, now, source.replies.len(), REPLY_DAYS);
        record(&mut first_post, &source.author, created);
        // (pid, root pid) of each reply so far.
        let mut placed: Vec<(u64, Option<u64>)> = Vec::with_capacity(source.replies.len());
        let mut posts = Vec::with_capacity(source.replies.len());
        for (offset, (reply, dateline)) in source.replies.iter().zip(times).enumerate() {
            let pid = next_pid;
            next_pid += 1;
            let (root_pid, reply_to_pid) = match reply.reply_to {
                None => (None, None),
                Some(target) => {
                    let (target_pid, target_root) = placed[target];
                    (Some(target_root.unwrap_or(target_pid)), Some(target_pid))
                }
            };
            placed.push((pid, root_pid));
            record(&mut first_post, &reply.author, dateline);
            posts.push(Post {
                pid,
                position: offset + 2,
                root_pid,
                reply_to_pid,
                author: reply.author.clone(),
                body: reply.body.clone(),
                dateline: u64::try_from(dateline)?,
                ..Post::default()
            });
        }
        let updated = posts.last().map_or(created, |post| post.dateline as i64);
        let views = (posts.len() + 1) * (8 + rng.below(23)) + rng.below(40);
        threads.push(Thread {
            id: u64::try_from(number + 1)?,
            fixture_forum: source.board.clone(),
            title: source.title.clone(),
            author: source.author.clone(),
            created: u64::try_from(created)?,
            updated: u64::try_from(updated)?,
            views,
            pinned: source.pinned,
            featured: source.featured,
            body: vec![source.body.clone()],
            posts,
            first_pid: i64::try_from(first_pid)?,
            ..Thread::default()
        });
    }
    let mut members: Vec<NewMember> = dataset
        .members
        .iter()
        .map(|(username, bio)| {
            let regdate = match first_post.get(username.as_str()) {
                Some(first) => first - rng.between(DAY, JOINED_BEFORE_DAYS * DAY),
                None => now - rng.between(DAY, PERIOD_DAYS * DAY),
            };
            NewMember {
                username: username.clone(),
                bio: bio.clone(),
                avatar: 0,
                regdate: regdate.max(1),
            }
        })
        .collect();
    members.sort_by_key(|member| member.regdate);
    for (index, member) in members.iter_mut().enumerate() {
        member.avatar = index % AVATAR_VARIANTS;
    }
    let forums = dataset
        .boards
        .iter()
        .map(|board| NewForum {
            slug: board.slug.clone(),
            name: board.name.clone(),
            description: board.description.clone(),
            guidance: board.guidance.clone(),
            parent: board.parent.clone(),
        })
        .collect();
    Ok(Plan {
        forums,
        members,
        threads,
    })
}

/// Keeps the earliest time each member posted.
fn record<'a>(first_post: &mut HashMap<&'a str, i64>, author: &'a str, time: i64) {
    let entry = first_post.entry(author).or_insert(time);
    *entry = (*entry).min(time);
}

/// Writes the setup wizard's sample content (`content::sample`) in `lang`, one of `sample::languages()`, as a dataset
/// in `dir`, which must not exist yet or be empty. Threads are grouped by board, oldest first; keys are the board slug
/// and a number. Nothing in the files depends on when this runs. Returns the number of threads written.
pub fn write_sample(dir: &Path, lang: crate::i18n::Lang) -> Result<usize> {
    if dir.exists() {
        ensure!(
            std::fs::read_dir(dir)
                .with_context(|| format!("cannot read {}", dir.display()))?
                .next()
                .is_none(),
            "{} is not empty; choose a new directory",
            dir.display()
        );
    }
    // Any fixed moment will do: the dataset format has no times. Planned before anything is written, so a language
    // without a sample leaves no directory behind.
    let plan = super::sample::plan(2_000_000_000, lang)?;
    std::fs::create_dir_all(dir.join(THREADS_DIR))
        .with_context(|| format!("cannot create {}", dir.display()))?;
    let manifest = Manifest {
        format: FORMAT.into(),
        version: VERSION,
        language: lang.tag().into(),
    };
    write_file(
        dir,
        MANIFEST_FILE,
        &(serde_json::to_string_pretty(&manifest)? + "\n"),
    )?;
    let boards: Vec<BoardEntry> = plan
        .forums
        .iter()
        .map(|forum| BoardEntry {
            slug: forum.slug.clone(),
            name: forum.name.clone(),
            description: forum.description.clone(),
            guidance: forum.guidance.clone(),
            parent: forum.parent.clone(),
        })
        .collect();
    write_file(
        dir,
        BOARDS_FILE,
        &(serde_json::to_string_pretty(&boards)? + "\n"),
    )?;
    let mut users = String::new();
    for member in &plan.members {
        users.push_str(&serde_json::to_string(&UserEntry {
            username: member.username.clone(),
            bio: member.bio.clone(),
        })?);
        users.push('\n');
    }
    write_file(dir, USERS_FILE, &users)?;
    let mut lines = String::new();
    let mut written = 0;
    for forum in &plan.forums {
        let mut number = 0;
        for thread in plan
            .threads
            .iter()
            .filter(|thread| thread.fixture_forum == forum.slug)
        {
            number += 1;
            // The sample numbers posts from 1 in order; a reply's id is its place among the thread's replies.
            let ids: HashMap<u64, u64> = thread
                .posts
                .iter()
                .enumerate()
                .map(|(index, post)| (post.pid, index as u64 + 1))
                .collect();
            let entry = ThreadEntry {
                key: format!("{}-{number:03}", forum.slug),
                board: forum.slug.clone(),
                author: thread.author.clone(),
                title: thread.title.clone(),
                body: thread.body.join("\n\n"),
                pinned: thread.pinned,
                featured: thread.featured,
                replies: thread
                    .posts
                    .iter()
                    .map(|post| ReplyEntry {
                        id: ids[&post.pid],
                        author: post.author.clone(),
                        reply_to: post.reply_to_pid.map(|pid| ids[&pid]),
                        body: post.body.clone(),
                    })
                    .collect(),
            };
            lines.push_str(&serde_json::to_string(&entry)?);
            lines.push('\n');
            written += 1;
        }
    }
    write_file(dir, &format!("{THREADS_DIR}/threads-001.jsonl"), &lines)?;
    Ok(written)
}

fn write_file(dir: &Path, name: &str, text: &str) -> Result<()> {
    let path = dir.join(name);
    std::fs::write(&path, text).with_context(|| format!("cannot write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    const NOW: i64 = 1_900_000_000;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "gonof_dataset_{}_{}",
                std::process::id(),
                &crate::identity::random_token()[..16]
            ));
            Self(path)
        }

        fn write(&self, name: &str, text: &str) {
            let path = self.0.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn example() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/content")
    }

    fn manifest() -> &'static str {
        "{\"format\": \"gono-forum-content\", \"version\": 1, \"language\": \"en\"}\n"
    }

    /// A small valid dataset: a parent board with a child, three members and two threads, one with a conversation
    /// three replies deep.
    fn small(scratch: &Scratch) {
        scratch.write(MANIFEST_FILE, manifest());
        scratch.write(
            BOARDS_FILE,
            r#"[
  {"slug": "community", "name": "Community", "description": "Everything about us"},
  {"slug": "intros", "name": "Introductions", "description": "Say hello", "parent": "community"},
  {"slug": "help", "name": "Help", "description": "Questions", "guidance": "Say what you tried."}
]
"#,
        );
        scratch.write(
            USERS_FILE,
            "{\"username\":\"Nora\",\"bio\":\"Backend developer.\"}\n{\"username\":\"felix\"}\n{\"username\":\"priya\"}\n{\"username\":\"lurker\"}\n",
        );
        scratch.write(
            "threads/threads-001.jsonl",
            concat!(
                r#"{"key":"help-1","board":"help","author":"nora","title":"Git: undo the last commit?","body":"Is `git reset --soft HEAD~1` right?","replies":[{"id":1,"author":"felix","body":"Yes, it keeps your changes."},{"id":2,"author":"Nora","reply_to":1,"body":"Thanks! Do I need to force-push?"},{"id":3,"author":"felix","reply_to":2,"body":"Only if you pushed it."},{"id":7,"author":"priya","body":"Or make the branch first."}]}"#,
                "\n",
                r#"{"key":"intros-1","board":"intros","author":"priya","title":"Hello from Priya","body":"I design products.\n\n<spoiler>I also bake.</spoiler>","pinned":true,"featured":true,"replies":[]}"#,
                "\n"
            ),
        );
    }

    #[test]
    fn a_valid_dataset_passes_and_is_planned_in_time_order() {
        let scratch = Scratch::new();
        small(&scratch);
        let checked = check(&scratch.0);
        assert!(
            checked.report.errors.is_empty(),
            "{}",
            checked.report.render(50)
        );
        assert!(
            checked.report.warnings.is_empty(),
            "{}",
            checked.report.render(50)
        );
        let report = &checked.report;
        assert_eq!(
            (
                report.boards,
                report.members,
                report.threads,
                report.replies,
                report.nested
            ),
            (3, 4, 2, 4, 2)
        );
        let plan = plan(checked.dataset.as_ref().unwrap(), NOW).unwrap();
        assert_eq!(plan.forums[1].parent.as_deref(), Some("community"));
        // Threads are numbered in time order; every thread's first post comes before its replies.
        let mut next_pid = 1;
        for (index, thread) in plan.threads.iter().enumerate() {
            assert_eq!(thread.id, index as u64 + 1);
            assert_eq!(thread.first_pid, next_pid);
            next_pid += 1 + thread.posts.len() as i64;
            assert!(
                (thread.created as i64) < NOW && (thread.created as i64) >= NOW - PERIOD_DAYS * DAY
            );
            let mut time = thread.created;
            for post in &thread.posts {
                assert!(post.dateline > time && (post.dateline as i64) < NOW);
                time = post.dateline;
            }
        }
        if plan.threads.len() == 2 {
            assert!(plan.threads[0].created <= plan.threads[1].created);
        }
        let help = plan
            .threads
            .iter()
            .find(|t| t.fixture_forum == "help")
            .unwrap();
        let pids: Vec<u64> = help.posts.iter().map(|post| post.pid).collect();
        let shape: Vec<(Option<u64>, Option<u64>)> = help
            .posts
            .iter()
            .map(|post| (post.root_pid, post.reply_to_pid))
            .collect();
        assert_eq!(
            shape,
            [
                (None, None),
                (Some(pids[0]), Some(pids[0])),
                // An answer to a nested reply stays in the same conversation.
                (Some(pids[0]), Some(pids[1])),
                (None, None),
            ]
        );
        assert_eq!(
            help.posts[1].author, "nora",
            "names are stored in lowercase"
        );
        let intro = plan
            .threads
            .iter()
            .find(|t| t.fixture_forum == "intros")
            .unwrap();
        assert!(intro.pinned && intro.featured && intro.posts.is_empty());
        // Members join in order, each before their first post; one who never posts still gets a date.
        let mut joined = 0;
        for member in &plan.members {
            assert!(member.regdate >= joined);
            joined = member.regdate;
            let first = plan
                .threads
                .iter()
                .flat_map(|thread| {
                    std::iter::once((thread.author.as_str(), thread.created)).chain(
                        thread
                            .posts
                            .iter()
                            .map(|post| (post.author.as_str(), post.dateline)),
                    )
                })
                .filter(|(author, _)| *author == member.username)
                .map(|(_, time)| time as i64)
                .min();
            if let Some(first) = first {
                assert!(member.regdate < first, "{}", member.username);
            }
        }
        assert!(
            plan.members
                .iter()
                .any(|member| member.username == "lurker")
        );
        // The same dataset gives the same plan apart from the shift in time.
        let again = super::plan(checked.dataset.as_ref().unwrap(), NOW + 100).unwrap();
        for (a, b) in plan.threads.iter().zip(&again.threads) {
            assert_eq!((&a.title, a.created + 100), (&b.title, b.created));
        }
    }

    #[test]
    fn boards_stop_at_the_top_level_limit() {
        let scratch = Scratch::new();
        small(&scratch);
        let limit = crate::organization::MAX_TOP_FORUMS;
        let mut boards = vec![
            r#"{"slug": "community", "name": "Community", "description": "Everything about us"}"#.to_owned(),
            r#"{"slug": "intros", "name": "Introductions", "description": "Say hello", "parent": "community"}"#.to_owned(),
            r#"{"slug": "help", "name": "Help", "description": "Questions"}"#.to_owned(),
        ];
        // Two top-level boards above, the rest up to one past the limit; the child board does not count.
        for n in 3..=limit + 1 {
            boards.push(format!(
                r#"{{"slug": "extra{n}", "name": "Extra {n}", "description": "More"}}"#
            ));
        }
        scratch.write(BOARDS_FILE, &format!("[{}]\n", boards.join(",\n")));
        let checked = check(&scratch.0);
        let errors: Vec<String> = checked
            .report
            .errors
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            errors,
            [format!(
                "boards.json, board {} (extra{}): more than {limit} top-level boards",
                limit + 2,
                limit + 1
            )]
        );
    }

    #[test]
    fn problems_name_their_file_line_and_thread() {
        let scratch = Scratch::new();
        scratch.write(
            MANIFEST_FILE,
            "{\"format\": \"something\", \"version\": 2, \"language\": \"en\"}\n",
        );
        scratch.write(
            BOARDS_FILE,
            r#"[
  {"slug": "Help", "name": "Help", "description": "x"},
  {"slug": "search", "name": "Search", "description": "x"},
  {"slug": "tips-2", "name": "Tips", "description": "x"},
  {"slug": "child", "name": "Child", "description": "x", "parent": "later"},
  {"slug": "later", "name": "Later", "description": ""},
  {"slug": "ok", "name": "OK", "description": "Fine"},
  {"slug": "ok", "name": "Other", "description": "Fine"}
]
"#,
        );
        scratch.write(
            USERS_FILE,
            "{\"username\":\"nora\"}\n\n{\"username\":\"NORA\"}\n{\"username\":\"has space\"}\n{\"username\":\"admin\"}\n{\"username\":\"felix\",\"bio\":\"x\",\"age\":3}\n",
        );
        scratch.write(
            "threads/a.jsonl",
            concat!(
                r#"{"key":"t1","board":"ok","author":"nora","title":"Fine","body":"Look: ![x](https://example.com/a.png)","replies":[{"id":1,"author":"ghost","body":"Hi"},{"id":2,"author":"admin","reply_to":5,"body":"<b>bold</b>"},{"id":2,"author":"nora","body":"[me](member:1)"}]}"#,
                "\n",
                r#"{"key":"t1","board":"nowhere","author":"nora","title":"  ","body":"x","replies":[]}"#,
                "\n",
                r#"{"key":"t 3","board":"ok","author":"admin","title":"Self","body":"x","replies":[{"id":1,"author":"admin","body":"a"},{"id":2,"author":"admin","reply_to":1,"body":"b"}]}"#,
                "\n",
                r#"{"key":"t4","board":"ok","author":"nora","title":"Typo","body":"x","pined":true,"replies":[]}"#,
                "\n"
            ),
        );
        scratch.write("threads/notes.txt", "ignored");
        let checked = check(&scratch.0);
        assert!(checked.dataset.is_none());
        let errors: Vec<String> = checked
            .report
            .errors
            .iter()
            .map(ToString::to_string)
            .collect();
        let warnings: Vec<String> = checked
            .report
            .warnings
            .iter()
            .map(ToString::to_string)
            .collect();
        for expected in [
            "manifest.json: format must be \"gono-forum-content\"",
            "manifest.json: version 2 is not supported",
            "boards.json, board 1 (Help): slug may only use lowercase letters",
            "boards.json, board 2 (search): slug is one of the site's own addresses",
            "boards.json, board 3 (tips-2): slug cannot end in - and digits",
            "boards.json, board 4 (child): parent later is not a board listed before this one",
            "boards.json, board 5 (later): description is empty",
            "boards.json, board 7 (ok): slug is also used by board 6",
            "users.jsonl:2: empty line",
            "users.jsonl:3 (NORA): username is the same as line 1",
            "users.jsonl:4 (has space): username must be 1-50 characters",
            "users.jsonl:6: unknown field `age`",
            "threads/a.jsonl:1 t1: the body contains an image",
            "threads/a.jsonl:1 t1 reply 1: author ghost is not in users.jsonl",
            "threads/a.jsonl:1 t1 reply 2: reply_to 5 is not an earlier reply in this thread",
            "threads/a.jsonl:1 t1 reply 2: the body contains HTML other than <spoiler>",
            "threads/a.jsonl:1 t1 reply 2: reply id appears twice in this thread",
            "threads/a.jsonl:1 t1 reply 2: the body contains an in-site reference link",
            "threads/a.jsonl:2 t1: key is also used at threads/a.jsonl:1",
            "threads/a.jsonl:2 t1: board nowhere is not in boards.json",
            "threads/a.jsonl:2 t1: title is empty",
            "threads/a.jsonl:3 t 3: key must be 1-64 letters, digits or -",
            "threads/a.jsonl:4: unknown field `pined`",
        ] {
            assert!(
                errors.iter().any(|error| error.starts_with(expected)),
                "missing error {expected:?} in:\n{}",
                errors.join("\n")
            );
        }
        for expected in [
            "users.jsonl:5 (admin): username reads like a staff account",
            "threads/a.jsonl:3 t 3 reply 2: answers a reply by the same author (reply 1)",
            "threads/notes.txt: is not a .jsonl file and is skipped",
        ] {
            assert!(
                warnings.iter().any(|warning| warning.starts_with(expected)),
                "missing warning {expected:?} in:\n{}",
                warnings.join("\n")
            );
        }
        let rendered = checked.report.render(3);
        assert!(
            rendered.contains(&format!("{} errors:", errors.len())),
            "{rendered}"
        );
        assert!(
            rendered.contains(&format!("... and {} more", errors.len() - 3)),
            "{rendered}"
        );
    }

    #[test]
    fn missing_files_and_line_endings_are_reported() {
        let scratch = Scratch::new();
        scratch.write(MANIFEST_FILE, "\u{feff}{\"format\": \"gono-forum-content\", \"version\": 1, \"language\": \"en\"}\r\n");
        let checked = check(&scratch.0);
        let errors: Vec<String> = checked
            .report
            .errors
            .iter()
            .map(ToString::to_string)
            .collect();
        for expected in [
            "manifest.json: starts with a byte order mark",
            "manifest.json: has CR or CRLF line endings",
            "boards.json: cannot be read",
            "users.jsonl: cannot be read",
            "threads/: cannot be read",
        ] {
            assert!(
                errors.iter().any(|error| error.starts_with(expected)),
                "missing {expected:?} in:\n{}",
                errors.join("\n")
            );
        }
    }

    /// `examples/content/<tag>/` is the sample of each language that has one, exactly as `export-sample` writes it,
    /// and nothing else.
    #[test]
    fn the_examples_are_the_wizard_samples_and_pass_the_checks() {
        let mut expected = Vec::new();
        for lang in super::super::sample::languages() {
            let scratch = Scratch::new();
            assert_eq!(
                write_sample(&scratch.0, lang).unwrap(),
                super::super::sample::THREADS
            );
            let example = example().join(lang.tag());
            for name in [
                MANIFEST_FILE,
                BOARDS_FILE,
                USERS_FILE,
                "threads/threads-001.jsonl",
            ] {
                let fresh = std::fs::read_to_string(scratch.0.join(name)).unwrap();
                let committed = std::fs::read_to_string(example.join(name)).unwrap_or_default();
                assert!(
                    fresh == committed,
                    "examples/content/{lang}/{name} is out of date with locales/{lang}/sample.json; run gono-forum-cli export-sample --language={lang} into an empty directory and copy the files over"
                );
                expected.push(format!("{lang}/{name}"));
            }
            assert!(
                fresh_manifest_names(&scratch.0, lang),
                "the manifest names the language"
            );

            let checked = check(&example);
            assert!(
                checked.report.errors.is_empty() && checked.report.warnings.is_empty(),
                "{lang}: {}",
                checked.report.render(50)
            );
            let report = &checked.report;
            assert_eq!(
                (
                    report.boards,
                    report.members,
                    report.threads,
                    report.replies
                ),
                (
                    super::super::sample::FORUMS,
                    super::super::sample::MEMBERS,
                    super::super::sample::THREADS,
                    super::super::sample::REPLIES
                ),
                "{lang}"
            );
            // Writing again into a directory that now has files is refused.
            assert!(write_sample(&scratch.0, lang).is_err());
        }
        let mut committed: Vec<String> = walk(&example());
        committed.sort();
        expected.sort();
        assert_eq!(
            committed, expected,
            "examples/content holds only the exported files, one directory per language with a sample"
        );
    }

    fn fresh_manifest_names(dir: &Path, lang: crate::i18n::Lang) -> bool {
        let manifest: Manifest =
            serde_json::from_str(&std::fs::read_to_string(dir.join(MANIFEST_FILE)).unwrap())
                .unwrap();
        manifest.language == lang.tag()
    }

    fn walk(dir: &Path) -> Vec<String> {
        let mut found = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type().unwrap().is_dir() {
                found.extend(
                    walk(&entry.path())
                        .into_iter()
                        .map(|inner| format!("{name}/{inner}")),
                );
            } else {
                found.push(name);
            }
        }
        found
    }
}
