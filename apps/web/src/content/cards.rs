//! In-site thread cards: a thread link standing as its own paragraph in a post body is replaced, according to the
//! reader's permissions, by a summary card.
use crate::db::Database;
use crate::markdown::{self, CardSlot, TopicCard};
use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet};

/// Maximum number of distinct threads expanded on one page; extra candidates stay plain links. Each post has its own
/// limit as well ([`markdown::MAX_CARDS_PER_POST`]).
pub const MAX_CARDS_PER_PAGE: usize = 20;
/// Length in characters of the first-post excerpt in a card.
pub const CARD_EXCERPT_CHARS: usize = 100;

/// Thread ids of a page's card candidates in order of appearance, deduplicated, at most [`MAX_CARDS_PER_PAGE`].
pub fn page_tids<'a>(slots: impl IntoIterator<Item = &'a CardSlot>) -> Vec<i64> {
    let mut seen = BTreeSet::new();
    let mut tids = Vec::new();
    for slot in slots {
        if tids.len() == MAX_CARDS_PER_PAGE {
            break;
        }
        if seen.insert(slot.tid) {
            tids.push(slot.tid);
        }
    }
    tids
}

/// Thread card HTML visible to the reader, keyed by thread id. One query fetches by primary key with the same
/// conditions as reading ([`super::thread_visible_sql`]), additionally excluding withdrawn and permanently purged
/// first posts and authors the reader has blocked; threads that cannot be read do not appear in the result.
/// `viewer_uid` may only come from an authenticated member context; `forums` are the forums the reader can read (the
/// forum cache in the page frame). The author comes from the copy on the thread row (written as "Deleted user" on
/// account deletion), and the forum name is taken from `forums` by `fid`, without joining the forum or member
/// tables.
pub async fn topic_cards(
    db: &Database,
    viewer_uid: Option<i64>,
    forums: &[crate::organization::ForumRecord],
    tids: &[i64],
) -> Result<BTreeMap<i64, String>> {
    if tids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let blocked = match viewer_uid {
        Some(uid) => super::blocked_authors(db, uid).await?,
        None => Vec::new(),
    };
    let readable: Vec<i64> = forums.iter().map(|forum| forum.fid).collect();
    let sql = format!(
        "SELECT t.tid,t.fid,t.subject,t.replies,t.dateline,t.author,firstpost.message
         FROM gonof_forum_thread t
         JOIN gonof_forum_post firstpost ON firstpost.first_tid=t.tid
         WHERE t.tid IN ({}) AND {} AND t.deleted_at=0 AND firstpost.erased_at=0{}",
        vec!["?"; tids.len()].join(","),
        super::thread_visible_sql(super::Readable {
            uid: viewer_uid,
            forums: &readable,
        }),
        super::not_blocked("t.authorid", &blocked),
    );
    let mut query = crate::db::query(&sql);
    for tid in tids {
        query = query.bind(tid);
    }
    let clock = crate::clock::current();
    let mut cards = BTreeMap::new();
    for row in query.fetch_all(db.executor()).await? {
        let author = super::copies::shown_name(crate::db::text(&row, "author")?);
        let card = TopicCard {
            tid: row.try_get("tid")?,
            subject: crate::db::text(&row, "subject")?,
            forum_name: super::forum_name(forums, row.try_get("fid")?),
            author,
            replies: row.try_get("replies")?,
            time: crate::clock::datetime(row.try_get("dateline")?, &clock),
            excerpt: markdown::card_excerpt(&crate::db::text(&row, "message")?, CARD_EXCERPT_CHARS),
        };
        cards.insert(card.tid, markdown::card_html(&card));
    }
    Ok(cards)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_tids_keep_order_dedupe_and_stop_at_the_page_limit() {
        let rendered: Vec<_> = (0..30)
            .map(|n| {
                markdown::render_page(
                    &format!("[a](/thread-{})\n\n[b](/thread-{})", n % 25 + 1, 7),
                    &BTreeMap::new(),
                    None,
                    "",
                )
            })
            .collect();
        let tids = page_tids(rendered.iter().flat_map(|r| &r.cards));
        assert_eq!(tids.len(), MAX_CARDS_PER_PAGE);
        assert_eq!(&tids[..3], &[1, 7, 2]);
        assert_eq!(tids.iter().collect::<BTreeSet<_>>().len(), tids.len());
    }
}
