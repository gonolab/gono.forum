//! Optional sample content for a new site (docs/install.md, "Setup wizard"): ten members, five forums, 100 threads
//! and 1,000 replies, in one of the languages that have a sample.
//!
//! The text lives in `locales/<tag>/sample.json` (docs/translating.md, "Sample content"). The base language's file is
//! complete; another language's file holds only text, matched to the base by member name, forum slug and position,
//! so every language gets the same plan with its own words. [`plan`] turns the text into a fixed plan of who wrote
//! what, when and in reply to whom. Only the timestamps depend on the moment of import (everything happened during
//! the two months before it, so the site looks active); the authors, reply counts, bodies and nesting come out the
//! same every time. The plan is written by `seed::import_sample`, which goes through the same insert code as the
//! development fixture.
use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::Deserialize;

use super::{
    Post, Thread,
    seed::{NewForum, NewMember},
};
use crate::i18n::Lang;

pub const MEMBERS: usize = 10;
pub const FORUMS: usize = 5;
pub const THREADS: usize = 100;
pub const REPLIES: usize = 1000;

pub(super) const MINUTE: i64 = 60;
pub(super) const HOUR: i64 = 60 * MINUTE;
pub(super) const DAY: i64 = 24 * HOUR;
/// Threads start within this many days before the import; pinned threads are the oldest.
const THREAD_DAYS: i64 = 60;
/// Replies to a thread arrive within this many days of it.
const REPLY_DAYS: i64 = 21;
/// Each thread gets between these many replies.
const REPLIES_PER_THREAD: (usize, usize) = (3, 30);
/// The share of replies, in percent, that answer an earlier reply instead of the thread.
const NESTED_PERCENT: u64 = 30;
/// Member avatars cycle through the text tile colors (`.avatar-0` to `.avatar-5`).
const AVATAR_VARIANTS: usize = 6;
/// Fixed, so the same plan comes out on every site.
const SEED: u64 = 0x5eed_0f90_20f0_2a17;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    members: Vec<SourceMember>,
    forums: Vec<SourceForum>,
    /// Top-level replies that fit a thread in any forum.
    replies: Vec<String>,
    /// Short answers to another reply.
    follow_ups: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceMember {
    username: String,
    bio: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceForum {
    slug: String,
    name: String,
    description: String,
    guidance: String,
    threads: Vec<SourceThread>,
    /// Top-level replies that fit any thread of this forum.
    replies: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceThread {
    title: String,
    body: String,
    #[serde(default)]
    pinned: bool,
    #[serde(default)]
    featured: bool,
}

/// Another language's sample: the base's text in that language. Members are keyed by their base username and forums
/// by slug; threads and replies follow the base's order, so the structure (pins, features, pool sizes) stays the base's.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Translation {
    #[serde(deserialize_with = "entries")]
    members: Vec<(String, SourceMember)>,
    #[serde(deserialize_with = "entries")]
    forums: Vec<(String, TranslatedForum)>,
    replies: Vec<String>,
    follow_ups: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TranslatedForum {
    name: String,
    description: String,
    guidance: String,
    threads: Vec<TranslatedThread>,
    replies: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TranslatedThread {
    title: String,
    body: String,
}

/// A JSON object as its entries in file order, refusing a key that appears twice (a map would silently keep the
/// last one, dropping a translation).
fn entries<'de, D, T>(deserializer: D) -> std::result::Result<Vec<(String, T)>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Entries<T>(std::marker::PhantomData<T>);

    impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for Entries<T> {
        type Value = Vec<(String, T)>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("an object")
        }

        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut entries: Vec<(String, T)> = Vec::new();
            while let Some((key, value)) = map.next_entry::<String, T>()? {
                if entries.iter().any(|(seen, _)| *seen == key) {
                    return Err(serde::de::Error::custom(format_args!(
                        "{key} appears twice"
                    )));
                }
                entries.push((key, value));
            }
            Ok(entries)
        }
    }

    deserializer.deserialize_map(Entries(std::marker::PhantomData))
}

/// Members, forums and threads ready for import. Threads carry their forum's slug and their authors' preferred
/// names; thread ids and post ids start at 1, threads numbered in the order they were started and each thread's first
/// post followed by its replies.
pub(crate) struct Plan {
    pub members: Vec<NewMember>,
    pub forums: Vec<NewForum>,
    pub threads: Vec<Thread>,
}

/// SplitMix64: small, fast and the same on every platform, which is all a fixed plan needs.
pub(super) struct Rng(pub(super) u64);

impl Rng {
    pub(super) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A number in `0..n`; `n` is never 0.
    pub(super) fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    /// A number in `low..=high`.
    pub(super) fn between(&mut self, low: i64, high: i64) -> i64 {
        low + (self.next() % (high - low + 1) as u64) as i64
    }

    fn shuffle<T>(&mut self, items: &mut [T]) {
        for index in (1..items.len()).rev() {
            items.swap(index, self.below(index + 1));
        }
    }

    /// Any member other than `avoid`.
    fn other_member(&mut self, avoid: usize) -> usize {
        (avoid + 1 + self.below(MEMBERS - 1)) % MEMBERS
    }
}

/// A top-level reply and the conversation under it, while a thread's replies are planned.
#[derive(Clone, Copy)]
struct Conversation {
    pid: u64,
    author: usize,
    /// The newest nested reply under it: its pid and author.
    newest: Option<(u64, usize)>,
}

/// Draws texts without repeating one until all have been used.
struct Deck<'a> {
    texts: Vec<&'a str>,
}

impl<'a> Deck<'a> {
    fn new(rng: &mut Rng, texts: impl IntoIterator<Item = &'a String>) -> Self {
        let mut texts: Vec<&str> = texts.into_iter().map(String::as_str).collect();
        rng.shuffle(&mut texts);
        Self { texts }
    }

    fn draw(&mut self) -> &'a str {
        let text = self.texts.remove(0);
        self.texts.push(text);
        text
    }
}

/// The languages that have a sample, the base first, then by tag.
pub fn languages() -> impl Iterator<Item = Lang> {
    crate::i18n::SAMPLES
        .iter()
        .filter_map(|(tag, _)| Lang::parse(tag))
}

/// The language with exactly this tag, when it has a sample.
pub fn language(tag: &str) -> Option<Lang> {
    languages().find(|lang| lang.tag() == tag)
}

/// The tags of the languages that have a sample, for messages that list them.
pub fn language_tags() -> String {
    languages().map(Lang::tag).collect::<Vec<_>>().join(", ")
}

/// The raw `sample.json` of a language.
fn text(lang: Lang) -> Option<&'static str> {
    crate::i18n::SAMPLES
        .iter()
        .find(|(tag, _)| *tag == lang.tag())
        .map(|(_, text)| *text)
}

/// The sample in `lang`: the base's structure with the language's text.
fn source(lang: Lang) -> Result<Source> {
    let base = text(Lang::EN).context("the base language has no sample")?;
    let base: Source = serde_json::from_str(base).context("locales/en/sample.json")?;
    ensure!(
        base.members.len() == MEMBERS
            && base.forums.len() == FORUMS
            && base
                .forums
                .iter()
                .map(|forum| forum.threads.len())
                .sum::<usize>()
                == THREADS,
        "locales/en/sample.json must hold {MEMBERS} members and {THREADS} threads in {FORUMS} forums"
    );
    if lang == Lang::EN {
        return Ok(base);
    }
    let text = text(lang).ok_or_else(|| {
        anyhow!(
            "there is no sample content in {lang}; it exists in {}",
            language_tags()
        )
    })?;
    let translation: Translation =
        serde_json::from_str(text).with_context(|| format!("locales/{lang}/sample.json"))?;
    translate(base, translation).with_context(|| {
        format!("locales/{lang}/sample.json does not match locales/en/sample.json")
    })
}

/// The base sample with the translation's text in place of its own.
fn translate(base: Source, translation: Translation) -> Result<Source> {
    let Translation {
        members: mut member_texts,
        forums: mut forum_texts,
        replies,
        follow_ups,
    } = translation;
    let members = base
        .members
        .into_iter()
        .map(|member| {
            let at = member_texts
                .iter()
                .position(|(key, _)| *key == member.username)
                .ok_or_else(|| anyhow!("member {} is missing", member.username))?;
            Ok(member_texts.remove(at).1)
        })
        .collect::<Result<Vec<_>>>()?;
    if let Some((key, _)) = member_texts.first() {
        bail!("member {key} is not in the base");
    }
    let forums = base
        .forums
        .into_iter()
        .map(|forum| {
            let at = forum_texts
                .iter()
                .position(|(key, _)| *key == forum.slug)
                .ok_or_else(|| anyhow!("forum {} is missing", forum.slug))?;
            let texts = forum_texts.remove(at).1;
            ensure!(
                texts.threads.len() == forum.threads.len(),
                "forum {} has {} threads, the base has {}",
                forum.slug,
                texts.threads.len(),
                forum.threads.len()
            );
            ensure!(
                texts.replies.len() == forum.replies.len(),
                "forum {} has {} replies, the base has {}",
                forum.slug,
                texts.replies.len(),
                forum.replies.len()
            );
            Ok(SourceForum {
                slug: forum.slug,
                name: texts.name,
                description: texts.description,
                guidance: texts.guidance,
                threads: forum
                    .threads
                    .into_iter()
                    .zip(texts.threads)
                    .map(|(thread, text)| SourceThread {
                        title: text.title,
                        body: text.body,
                        pinned: thread.pinned,
                        featured: thread.featured,
                    })
                    .collect(),
                replies: texts.replies,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    if let Some((key, _)) = forum_texts.first() {
        bail!("forum {key} is not in the base");
    }
    ensure!(
        replies.len() == base.replies.len(),
        "{} replies, the base has {}",
        replies.len(),
        base.replies.len()
    );
    ensure!(
        follow_ups.len() == base.follow_ups.len(),
        "{} follow-ups, the base has {}",
        follow_ups.len(),
        base.follow_ups.len()
    );
    Ok(Source {
        members,
        forums,
        replies,
        follow_ups,
    })
}

/// The sample content in `lang` as if it had been written during the [`THREAD_DAYS`] before `now` (Unix seconds).
/// Every language gets the same plan; only the text differs.
pub(crate) fn plan(now: i64, lang: Lang) -> Result<Plan> {
    let source = source(lang)?;
    let mut rng = Rng(SEED);
    let members: Vec<NewMember> = source
        .members
        .iter()
        .enumerate()
        .map(|(index, member)| NewMember {
            username: member.username.clone(),
            bio: member.bio.clone(),
            avatar: index % AVATAR_VARIANTS,
            // A day apart, all before the first thread.
            regdate: now - (THREAD_DAYS + 30 - index as i64) * DAY,
        })
        .collect();

    // When each thread was started: pinned threads first, a few minutes apart, the rest spread over the period.
    let mut started = Vec::with_capacity(THREADS);
    let mut pinned = 0;
    for (forum, source_forum) in source.forums.iter().enumerate() {
        for thread in &source_forum.threads {
            let created = if thread.pinned {
                pinned += 1;
                now - THREAD_DAYS * DAY - (10 - pinned) * 10 * MINUTE
            } else {
                now - rng.between(2 * HOUR, (THREAD_DAYS - 1) * DAY)
            };
            started.push((created, forum, thread));
        }
    }
    started.sort_by_key(|(created, _, _)| *created);

    let counts = reply_counts(&mut rng);
    let mut threads = Vec::with_capacity(THREADS);
    let mut next_pid = 1_u64;
    for (index, ((created, forum, thread), count)) in started.into_iter().zip(counts).enumerate() {
        let source_forum = &source.forums[forum];
        let author = rng.below(MEMBERS);
        let first_pid = next_pid;
        next_pid += 1;
        let mut replies = Deck::new(&mut rng, source_forum.replies.iter().chain(&source.replies));
        let mut follow_ups = Deck::new(&mut rng, &source.follow_ups);
        let times = reply_times(&mut rng, created, now, count, REPLY_DAYS);
        let mut roots: Vec<Conversation> = Vec::new();
        let mut posts = Vec::with_capacity(count);
        let mut previous = author;
        for (offset, dateline) in times.into_iter().enumerate() {
            let pid = next_pid;
            next_pid += 1;
            let nested = !roots.is_empty() && rng.next() % 100 < NESTED_PERCENT;
            let (root_pid, reply_to_pid, poster, body) = if nested {
                let root = rng.below(roots.len());
                let Conversation {
                    pid: root_pid,
                    author: root_author,
                    newest,
                } = roots[root];
                // Half the time the answer goes to the newest reply in the conversation, otherwise to its start.
                let (target_pid, target_author) = match newest {
                    Some(newest) if rng.next().is_multiple_of(2) => newest,
                    _ => (root_pid, root_author),
                };
                let poster = rng.other_member(target_author);
                roots[root].newest = Some((pid, poster));
                (Some(root_pid), Some(target_pid), poster, follow_ups.draw())
            } else {
                let poster = rng.other_member(previous);
                roots.push(Conversation {
                    pid,
                    author: poster,
                    newest: None,
                });
                (None, None, poster, replies.draw())
            };
            previous = poster;
            posts.push(Post {
                pid,
                position: offset + 2,
                root_pid,
                reply_to_pid,
                author: members[poster].username.clone(),
                avatar: members[poster].avatar,
                body: body.to_owned(),
                dateline: u64::try_from(dateline)?,
                ..Post::default()
            });
        }
        let updated = posts.last().map_or(created, |post| post.dateline as i64);
        threads.push(Thread {
            id: u64::try_from(index + 1)?,
            fixture_forum: source_forum.slug.clone(),
            title: thread.title.clone(),
            author: members[author].username.clone(),
            avatar: members[author].avatar,
            created: u64::try_from(created)?,
            updated: u64::try_from(updated)?,
            views: (count + 1) * (8 + rng.below(23)) + rng.below(40),
            pinned: thread.pinned,
            featured: thread.featured,
            body: vec![thread.body.clone()],
            posts,
            first_pid: i64::try_from(first_pid)?,
            ..Thread::default()
        });
    }
    let forums = source
        .forums
        .into_iter()
        .map(|forum| NewForum {
            slug: forum.slug,
            name: forum.name,
            description: forum.description,
            guidance: forum.guidance,
            parent: None,
        })
        .collect();
    Ok(Plan {
        members,
        forums,
        threads,
    })
}

/// How many replies each thread gets, in the order the threads were started: uneven, within
/// [`REPLIES_PER_THREAD`], and [`REPLIES`] in all.
fn reply_counts(rng: &mut Rng) -> Vec<usize> {
    let (low, high) = REPLIES_PER_THREAD;
    let mut counts: Vec<usize> = (0..THREADS).map(|_| low + rng.below(15)).collect();
    let mut total: usize = counts.iter().sum();
    while total != REPLIES {
        let index = rng.below(THREADS);
        if total < REPLIES && counts[index] < high {
            counts[index] += 1;
            total += 1;
        } else if total > REPLIES && counts[index] > low {
            counts[index] -= 1;
            total -= 1;
        }
    }
    counts
}

/// Strictly increasing reply times after `created`, within `days` of it: most replies come soon after the thread, the
/// rest trail off, and all are before `now`.
pub(super) fn reply_times(
    rng: &mut Rng,
    created: i64,
    now: i64,
    count: usize,
    days: i64,
) -> Vec<i64> {
    let first = created + 5 * MINUTE;
    let last = (created + days * DAY).min(now - MINUTE);
    let span = (last - first).max(count as i64);
    let mut times: Vec<i64> = (0..count)
        .map(|_| {
            // The product of two uniform draws leans towards the start of the span.
            let a = rng.between(0, 1000);
            let b = rng.between(0, 1000);
            first + span * a * b / 1_000_000
        })
        .collect();
    times.sort_unstable();
    for index in 1..times.len() {
        if times[index] <= times[index - 1] {
            times[index] = times[index - 1] + 1;
        }
    }
    times
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};

    use super::*;

    const NOW: i64 = 1_800_000_000;

    #[test]
    fn the_base_and_simplified_chinese_have_a_sample() {
        let tags: Vec<&str> = languages().map(Lang::tag).collect();
        assert_eq!(tags.first(), Some(&"en"), "the base comes first");
        assert!(tags.contains(&"zh-CN"), "{tags:?}");
        assert_eq!(language("en"), Some(Lang::EN));
        assert_eq!(language("xx"), None);
        assert_eq!(language_tags(), tags.join(", "));
    }

    #[test]
    fn the_plan_has_the_promised_size() {
        for lang in languages() {
            let plan = plan(NOW, lang).unwrap();
            assert_eq!(plan.members.len(), MEMBERS, "{lang}");
            assert_eq!(plan.forums.len(), FORUMS, "{lang}");
            assert_eq!(plan.threads.len(), THREADS, "{lang}");
            assert_eq!(
                plan.threads
                    .iter()
                    .map(|thread| thread.posts.len())
                    .sum::<usize>(),
                REPLIES,
                "{lang}"
            );
            let posters: BTreeSet<&str> = plan
                .threads
                .iter()
                .flat_map(|thread| {
                    std::iter::once(thread.author.as_str())
                        .chain(thread.posts.iter().map(|post| post.author.as_str()))
                })
                .collect();
            assert_eq!(
                posters.len(),
                MEMBERS,
                "{lang}: every member writes something"
            );
            for forum in &plan.forums {
                assert!(
                    crate::organization::forum_fields(&forum.name, &forum.slug, "public", 1)
                        .is_some_and(|(_, slug)| slug == forum.slug),
                    "{lang}: {}",
                    forum.slug
                );
                assert_eq!(
                    plan.threads
                        .iter()
                        .filter(|thread| thread.fixture_forum == forum.slug)
                        .count(),
                    THREADS / FORUMS
                );
            }
        }
    }

    #[test]
    fn only_the_timestamps_depend_on_the_time_of_import() {
        let one = plan(NOW, Lang::EN).unwrap();
        let other = plan(NOW + 12_345, Lang::EN).unwrap();
        for (a, b) in one.threads.iter().zip(&other.threads) {
            assert_eq!((a.id, &a.title, &a.author), (b.id, &b.title, &b.author));
            assert_eq!(a.created + 12_345, b.created);
            for (x, y) in a.posts.iter().zip(&b.posts) {
                assert_eq!(
                    (x.pid, &x.author, &x.body, x.root_pid, x.reply_to_pid),
                    (y.pid, &y.author, &y.body, y.root_pid, y.reply_to_pid)
                );
                assert_eq!(x.dateline + 12_345, y.dateline);
            }
        }
    }

    #[test]
    fn threads_and_replies_are_well_formed() {
        for lang in languages() {
            well_formed(lang);
        }
    }

    fn well_formed(lang: Lang) {
        let plan = plan(NOW, lang).unwrap();
        let usernames: BTreeSet<&str> = plan.members.iter().map(|m| m.username.as_str()).collect();
        assert_eq!(usernames.len(), MEMBERS, "{lang}: usernames are unique");
        for member in &plan.members {
            crate::identity::rules::check_username(&member.username).unwrap();
            assert_eq!(
                crate::identity::normalize_username(&member.username).ok(),
                Some(member.username.clone()),
                "{lang}: usernames are written in their stored form (trimmed, NFKC, lowercase)"
            );
            assert!(member.bio.chars().count() <= 500);
            assert!(member.regdate < NOW - THREAD_DAYS * DAY);
        }
        let mut next_pid = 1;
        let mut previous_created = 0;
        for (index, thread) in plan.threads.iter().enumerate() {
            assert_eq!(thread.id, index as u64 + 1, "threads are numbered in order");
            assert!(thread.created >= previous_created);
            previous_created = thread.created;
            assert!(thread.created as i64 >= NOW - THREAD_DAYS * DAY - DAY);
            assert!((thread.created as i64) < NOW);
            assert!(usernames.contains(thread.author.as_str()));
            assert_eq!(thread.first_pid, next_pid);
            next_pid += 1;
            assert!(
                (REPLIES_PER_THREAD.0..=REPLIES_PER_THREAD.1).contains(&thread.posts.len()),
                "{}",
                thread.title
            );
            let mut by_pid = HashMap::new();
            let mut bodies = BTreeSet::new();
            let mut dateline = thread.created;
            for (offset, post) in thread.posts.iter().enumerate() {
                assert_eq!(post.pid as i64, next_pid);
                next_pid += 1;
                assert_eq!(post.position, offset + 2);
                assert!(post.dateline > dateline, "replies are in time order");
                dateline = post.dateline;
                assert!((post.dateline as i64) < NOW);
                assert!(
                    bodies.insert(&post.body),
                    "{lang}: no repeated reply in one thread"
                );
                match (post.root_pid, post.reply_to_pid) {
                    (None, None) => {}
                    (Some(root), Some(target)) => {
                        let root_post: &Post = by_pid[&root];
                        assert!(
                            root_post.root_pid.is_none(),
                            "the root is a top-level reply"
                        );
                        let target_post: &Post = by_pid[&target];
                        assert!(target == root || target_post.root_pid == Some(root));
                        assert_ne!(post.author, target_post.author, "nobody answers themselves");
                    }
                    shape => panic!("half-nested reply: {shape:?}"),
                }
                by_pid.insert(post.pid, post);
            }
            assert_eq!(thread.updated, dateline);
            assert!(thread.posts[0].root_pid.is_none());
        }
        let pinned = plan.threads.iter().filter(|thread| thread.pinned).count();
        assert!(pinned >= 1 && plan.threads[..pinned].iter().all(|t| t.pinned));
        assert!(plan.threads.iter().any(|thread| thread.featured));
        let nested = plan
            .threads
            .iter()
            .flat_map(|thread| &thread.posts)
            .filter(|post| post.root_pid.is_some())
            .count();
        assert!((200..=400).contains(&nested), "about 30% nested: {nested}");
    }

    /// The texts of every language keep the rules the import and the forms keep: lengths, nothing empty, and no text
    /// twice in a pool (a thread never shows the same reply twice).
    #[test]
    fn every_sample_text_fits_the_rules() {
        for lang in languages() {
            let source = source(lang).unwrap();
            let mut names = BTreeSet::new();
            for forum in &source.forums {
                assert!(
                    names.insert(forum.name.to_lowercase()),
                    "{lang}: forum names are unique ignoring case"
                );
                assert!(forum.name.chars().count() <= 50, "{lang}: {}", forum.name);
                for text in [&forum.description, &forum.guidance] {
                    assert!(!text.trim().is_empty(), "{lang}: {}", forum.slug);
                }
                for thread in &forum.threads {
                    let title = thread.title.trim();
                    assert!(
                        (1..=255).contains(&title.chars().count())
                            && !title.chars().any(char::is_control),
                        "{lang}: {title:?}"
                    );
                    assert!(
                        !thread.body.trim().is_empty() && thread.body.len() <= 60_000,
                        "{lang}: {title}"
                    );
                }
                let mut pool = BTreeSet::new();
                for reply in forum.replies.iter().chain(&source.replies) {
                    assert!(!reply.trim().is_empty(), "{lang}: an empty reply");
                    assert!(
                        pool.insert(reply.as_str()),
                        "{lang}: {reply:?} is twice in the replies of {}",
                        forum.slug
                    );
                }
            }
            let mut follow_ups = BTreeSet::new();
            for reply in &source.follow_ups {
                assert!(!reply.trim().is_empty(), "{lang}: an empty follow-up");
                assert!(
                    follow_ups.insert(reply.as_str()),
                    "{lang}: {reply:?} is twice in the follow-ups"
                );
            }
        }
    }

    /// A translation changes the words and nothing else: the same threads in the same forums, written by the same
    /// members at the same times, with the same replies in the same places.
    #[test]
    fn translations_follow_the_base_plan() {
        let base = plan(NOW, Lang::EN).unwrap();
        let base_index: HashMap<&str, usize> = base
            .members
            .iter()
            .enumerate()
            .map(|(index, member)| (member.username.as_str(), index))
            .collect();
        for lang in languages().filter(|lang| *lang != Lang::EN) {
            let translated = plan(NOW, lang).unwrap();
            let index: HashMap<&str, usize> = translated
                .members
                .iter()
                .enumerate()
                .map(|(index, member)| (member.username.as_str(), index))
                .collect();
            for (a, b) in base.members.iter().zip(&translated.members) {
                assert_eq!((a.avatar, a.regdate), (b.avatar, b.regdate), "{lang}");
            }
            let slugs = |plan: &Plan| -> Vec<String> {
                plan.forums.iter().map(|forum| forum.slug.clone()).collect()
            };
            assert_eq!(slugs(&base), slugs(&translated), "{lang}");
            for (a, b) in base.threads.iter().zip(&translated.threads) {
                assert_eq!(
                    (
                        a.id,
                        &a.fixture_forum,
                        base_index[a.author.as_str()],
                        a.created,
                        a.updated,
                        a.views,
                        a.pinned,
                        a.featured,
                        a.first_pid
                    ),
                    (
                        b.id,
                        &b.fixture_forum,
                        index[b.author.as_str()],
                        b.created,
                        b.updated,
                        b.views,
                        b.pinned,
                        b.featured,
                        b.first_pid
                    ),
                    "{lang}"
                );
                assert_ne!(a.title, b.title, "{lang}: thread {} is translated", a.id);
                assert_eq!(a.posts.len(), b.posts.len(), "{lang}");
                for (x, y) in a.posts.iter().zip(&b.posts) {
                    assert_eq!(
                        (
                            x.pid,
                            x.position,
                            x.root_pid,
                            x.reply_to_pid,
                            base_index[x.author.as_str()],
                            x.dateline
                        ),
                        (
                            y.pid,
                            y.position,
                            y.root_pid,
                            y.reply_to_pid,
                            index[y.author.as_str()],
                            y.dateline
                        ),
                        "{lang}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_base_sample_is_plain_english() {
        assert!(
            text(Lang::EN).unwrap().is_ascii(),
            "the base sample is plain English ASCII"
        );
    }

    /// The base sample as a translation of itself: every key and position filled in.
    fn identity_translation() -> serde_json::Value {
        let base = source(Lang::EN).unwrap();
        let members: serde_json::Map<String, serde_json::Value> = base
            .members
            .iter()
            .map(|member| {
                (
                    member.username.clone(),
                    serde_json::json!({"username": member.username, "bio": member.bio}),
                )
            })
            .collect();
        let forums: serde_json::Map<String, serde_json::Value> = base
            .forums
            .iter()
            .map(|forum| {
                let threads: Vec<serde_json::Value> = forum
                    .threads
                    .iter()
                    .map(|thread| serde_json::json!({"title": thread.title, "body": thread.body}))
                    .collect();
                (
                    forum.slug.clone(),
                    serde_json::json!({
                        "name": forum.name,
                        "description": forum.description,
                        "guidance": forum.guidance,
                        "threads": threads,
                        "replies": forum.replies,
                    }),
                )
            })
            .collect();
        serde_json::json!({
            "members": members,
            "forums": forums,
            "replies": base.replies,
            "follow_ups": base.follow_ups,
        })
    }

    fn translated(value: &serde_json::Value) -> Result<Source> {
        translate(
            source(Lang::EN).unwrap(),
            serde_json::from_value(value.clone())?,
        )
    }

    #[test]
    fn a_translation_must_match_the_base() {
        let full = identity_translation();
        let same = translated(&full).unwrap();
        assert_eq!(
            same.forums[1].threads[0].title,
            "How to ask a good question here"
        );
        assert!(same.forums[1].threads[0].pinned && same.forums[1].threads[0].featured);

        let mut missing = full.clone();
        missing["members"].as_object_mut().unwrap().remove("nora");
        let error = translated(&missing).err().expect("refused");
        assert!(
            error.to_string().contains("member nora is missing"),
            "{error}"
        );

        let mut extra = full.clone();
        extra["forums"]["jobs"] = full["forums"]["help"].clone();
        let error = translated(&extra).err().expect("refused");
        assert!(
            error.to_string().contains("forum jobs is not in the base"),
            "{error}"
        );

        let mut short = full.clone();
        short["forums"]["tech"]["threads"]
            .as_array_mut()
            .unwrap()
            .pop();
        let error = translated(&short).err().expect("refused");
        assert!(
            error
                .to_string()
                .contains("forum tech has 19 threads, the base has 20"),
            "{error}"
        );

        let mut pool = full.clone();
        pool["follow_ups"].as_array_mut().unwrap().pop();
        let error = translated(&pool).err().expect("refused");
        assert!(error.to_string().contains("follow-ups"), "{error}");

        let mut unknown = full.clone();
        unknown["forums"]["help"]["slug"] = "help".into();
        assert!(translated(&unknown).is_err(), "unknown fields are refused");

        // Written twice, a member would silently lose one of the two translations.
        let twice = r#"{"members": {"nora": {"username": "a", "bio": ""}, "nora": {"username": "b", "bio": ""}}, "forums": {}, "replies": [], "follow_ups": []}"#;
        let error = serde_json::from_str::<Translation>(twice)
            .err()
            .expect("a repeated key is refused");
        assert!(error.to_string().contains("nora appears twice"), "{error}");
    }
}
