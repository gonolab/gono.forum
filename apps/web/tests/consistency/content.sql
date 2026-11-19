-- Content consistency check: read-only SELECTs, one row per item.
--
-- `test_support::check_script` runs this file against a test database after the tests have written through the real
-- code paths (`tests/thread_copies.rs` runs it at the end of every test, other tests use it too) and returns each item
-- with its amount. Every amount must be 0: anything else means a row that breaks the rules below reached the database
-- by bypassing code. The checks use MySQL syntax and run on MySQL only, on SQLite the helper returns no rows.
-- When you change a matching rule in code, change the check here too.
--
-- The schema has no triggers (docs/architecture.md, "Data"): the copies on the thread row, first_tid on the first
-- post and the value rules are guaranteed only by code at write time (content/copies.rs, PostShape in
-- content/writes.rs, organization::forum_fields). Code rewrites a copy only when its source table changes and never
-- corrects old values, so stored rows must already agree, otherwise an old deviation would become permanent. Strings
-- always compare bytes: code does not let through spellings that differ only by trailing spaces under PAD SPACE.
-- The thread row carries no copies of the avatar, the forum name and URL, or the inherited forum visibility and
-- enabled state: read paths take them by fid from the forum cache or the forum table, and avatars by uid, so they are
-- not checked here. The thread row keeps only three copies: the author name, the last replier and the top-level
-- forum id (docs/queries.md, "Copy columns").

-- 1. Copies agree with their source tables
-- For a deleted account the author copy is the placeholder name used below, written as its UTF-8 bytes (the stored
-- marker `content::copies::DELETED_AUTHOR`).
SELECT 'threads whose author name copy disagrees with the member table' AS item, COUNT(*) AS amount
  FROM gonof_forum_thread t
  LEFT JOIN gonof_common_member m ON m.uid = t.authorid
 WHERE NOT (CASE WHEN m.account_state = 'deleted'
     THEN CAST(t.author AS BINARY) = X'E5B7B2E6B3A8E99480E794A8E688B7'
     ELSE CAST(t.author AS BINARY) = CAST(COALESCE(m.username, '') AS BINARY) END)
UNION ALL
-- When the last replier is a deleted member, the placeholder name (what code writes) counts as consistent, and so
-- does the username.
SELECT 'threads whose last replier disagrees with the author of the last reply', COUNT(*)
  FROM gonof_forum_thread t
  JOIN gonof_forum_post p ON p.pid = t.last_reply_pid
  LEFT JOIN gonof_common_member m ON m.uid = p.authorid
 WHERE NOT (CAST(t.last_reply_author AS BINARY) = CAST(COALESCE(m.username, '') AS BINARY)
    OR ((m.uid IS NULL OR m.account_state = 'deleted') AND CAST(t.last_reply_author AS BINARY) = X'E5B7B2E6B3A8E99480E794A8E688B7'))
UNION ALL
SELECT 'threads with a last replier but no last reply', COUNT(*)
  FROM gonof_forum_thread
 WHERE last_reply_pid IS NULL AND (last_reply_author <> '' OR last_reply_uid IS NOT NULL)
UNION ALL
-- The last replier's uid is the author of the last reply, and NULL when that account is deleted or the reply has no
-- author.
SELECT 'threads whose last replier uid disagrees with the author of the last reply', COUNT(*)
  FROM gonof_forum_thread t
  JOIN gonof_forum_post p ON p.pid = t.last_reply_pid
  LEFT JOIN gonof_common_member m ON m.uid = p.authorid
 WHERE NOT (CASE WHEN m.uid IS NULL OR m.account_state = 'deleted'
     THEN t.last_reply_uid IS NULL
     ELSE COALESCE(t.last_reply_uid, 0) = m.uid END)
UNION ALL
-- A thread whose first post is hidden by moderation has displayorder -1, the reserved states (unpublished, private,
-- read permission set) have -2, and everything else is not negative.
SELECT 'threads whose negative displayorder disagrees with the hidden or reserved state of the first post', COUNT(*)
  FROM gonof_forum_thread t
  JOIN gonof_forum_post p ON p.first_tid = t.tid
 WHERE (p.invisible <> 0 AND t.displayorder <> -1)
    OR (p.invisible = 0 AND (t.publication_state <> 'published' OR t.visibility NOT IN ('public', 'members') OR t.readperm <> 0) AND t.displayorder <> -2)
    OR (p.invisible = 0 AND t.publication_state = 'published' AND t.visibility IN ('public', 'members') AND t.readperm = 0 AND t.displayorder < 0)
UNION ALL
-- Thread withdrawal and first post withdrawal are always written in the same transaction (read paths only look at
-- deleted_at on the first post itself).
SELECT 'threads whose withdrawal disagrees with the withdrawal of the first post', COUNT(*)
  FROM gonof_forum_thread t
  JOIN gonof_forum_post p ON p.first_tid = t.tid
 WHERE t.deleted_at <> p.deleted_at
UNION ALL
-- The top-level forum id is the id of the parent forum, or the forum itself for a top-level forum.
SELECT 'threads whose top-level forum id copy disagrees with the forum table', COUNT(*)
  FROM gonof_forum_thread t
  JOIN gonof_forum_forum f ON f.fid = t.fid
 WHERE t.family_fid <> COALESCE(f.parent_fid, f.fid)
UNION ALL
-- A nested reply carries the moderation-hidden state of its parent reply, top-level replies and first posts carry 0.
SELECT 'nested replies whose copy of the parent hidden state disagrees with the parent', COUNT(*)
  FROM gonof_forum_post p
  LEFT JOIN gonof_forum_post parent ON parent.pid = p.root_pid
 WHERE p.root_invisible <> COALESCE(parent.invisible, 0)
UNION ALL
-- The forum thread count stores the number of listable threads per (forum, thread visibility, category).
SELECT 'forum thread count cells that disagree with the listable threads', (
    SELECT COUNT(*) FROM (
      SELECT fid, visibility, category, COUNT(*) AS n FROM gonof_forum_thread WHERE displayorder >= 0 GROUP BY fid, visibility, category
    ) a LEFT JOIN gonof_forum_thread_count c ON c.fid = a.fid AND c.visibility = a.visibility AND c.category = a.category
    WHERE c.threads IS NULL
  ) + (
    SELECT COUNT(*) FROM gonof_forum_thread_count c
    WHERE c.threads <> (SELECT COUNT(*) FROM gonof_forum_thread t WHERE t.fid = c.fid AND t.visibility = c.visibility AND t.category = c.category AND t.displayorder >= 0)
  )
UNION ALL
-- The member count holds the threads and replies of the member that are neither withdrawn nor hidden, and the
-- digest threads among them.
SELECT 'members whose counters disagree with their own posts', COUNT(*)
  FROM gonof_common_member m
  LEFT JOIN gonof_common_member_count c ON c.uid = m.uid
  LEFT JOIN (
    SELECT p.authorid AS uid, SUM(p.first = 1) AS threads, SUM(p.first = 0) AS replies, SUM(p.first = 1 AND t.digest > 0) AS digests
      FROM gonof_forum_post p JOIN gonof_forum_thread t ON t.tid = p.tid
     WHERE p.authorid IS NOT NULL AND p.invisible = 0 AND p.deleted_at = 0
     GROUP BY p.authorid
  ) a ON a.uid = m.uid
 WHERE COALESCE(c.threads, 0) <> COALESCE(a.threads, 0)
    OR COALESCE(c.replies, 0) <> COALESCE(a.replies, 0)
    OR COALESCE(c.digests, 0) <> COALESCE(a.digests, 0)
UNION ALL
-- The leaderboard totals (`ranks::counts`) hold each member's threads and replies that the all-time boards count
-- (not hidden, withdrawn or erased, floor not hidden; thread listed and public; board and parent board public and
-- enabled) and the likes members other than the author gave those posts, counted from the like rows.
SELECT 'members whose leaderboard totals disagree with their public posts and likes', COUNT(*)
  FROM gonof_common_member m
  LEFT JOIN gonof_rank_member c ON c.uid = m.uid
  LEFT JOIN (
    SELECT p.authorid AS uid, SUM(p.first = 1) AS threads, SUM(p.first = 0) AS replies, SUM(COALESCE(l.likes, 0)) AS likes
      FROM gonof_forum_post p
      JOIN gonof_forum_thread t ON t.tid = p.tid
      JOIN gonof_forum_forum f ON f.fid = t.fid
      LEFT JOIN gonof_forum_forum pf ON pf.fid = f.parent_fid
      LEFT JOIN (
        SELECT r.pid, COUNT(*) AS likes
          FROM gonof_forum_post_reaction r JOIN gonof_forum_post rp ON rp.pid = r.pid
         WHERE r.uid <> rp.authorid
         GROUP BY r.pid
      ) l ON l.pid = p.pid
     WHERE p.authorid IS NOT NULL AND p.invisible = 0 AND p.deleted_at = 0 AND p.erased_at = 0 AND p.root_invisible = 0
       AND t.displayorder >= 0 AND t.visibility = 'public' AND f.status = 1 AND f.visibility = 'public'
       AND (pf.fid IS NULL OR (pf.status = 1 AND pf.visibility = 'public'))
     GROUP BY p.authorid
  ) a ON a.uid = m.uid
 WHERE COALESCE(c.threads, 0) <> COALESCE(a.threads, 0)
    OR COALESCE(c.replies, 0) <> COALESCE(a.replies, 0)
    OR COALESCE(c.likes, 0) <> COALESCE(a.likes, 0)
UNION ALL
-- The like and downvote counts on posts, and the favorite count, tip count and tip total on threads, agree with the
-- detail rows and the tip ledger.
SELECT 'posts whose like or dislike count disagrees with the votes', COUNT(*)
  FROM gonof_forum_post p
 WHERE p.reactions <> (SELECT COUNT(*) FROM gonof_forum_post_reaction r WHERE r.pid = p.pid)
    OR p.downvotes <> (SELECT COUNT(*) FROM gonof_forum_post_downvote d WHERE d.pid = p.pid)
UNION ALL
SELECT 'threads whose favorite count, tip count or tip total disagrees with the rows', COUNT(*)
  FROM gonof_forum_thread t
 WHERE t.favorites <> (SELECT COUNT(*) FROM gonof_home_favorite f WHERE f.tid = t.tid)
    OR t.tips <> (SELECT COUNT(*) FROM gonof_common_credit_ledger l WHERE l.tid = t.tid AND l.kind = 'tip_in')
    OR t.tip_total <> (SELECT COALESCE(SUM(l.available_delta), 0) FROM gonof_common_credit_ledger l WHERE l.tid = t.tid AND l.kind = 'tip_in')
UNION ALL
-- 2. Counts and derived values
SELECT 'threads without exactly one first post', COUNT(*)
  FROM gonof_forum_thread t
 WHERE (SELECT COUNT(*) FROM gonof_forum_post p WHERE p.tid = t.tid AND p.first = 1) <> 1
UNION ALL
SELECT 'posts whose first_tid disagrees with the first-post flag', COUNT(*)
  FROM gonof_forum_post
 WHERE NOT (first_tid <=> IF(first = 1, tid, NULL))
UNION ALL
SELECT 'threads whose reply count, top-level reply count or highest position disagrees with the posts', COUNT(*)
  FROM gonof_forum_thread t
  JOIN (SELECT tid, SUM(first = 0) AS replies, SUM(first = 0 AND root_pid IS NULL) AS root_replies,
               MAX(position) AS maxposition
          FROM gonof_forum_post GROUP BY tid) c ON c.tid = t.tid
 WHERE t.replies <> c.replies OR t.root_replies <> c.root_replies OR t.maxposition <> c.maxposition
UNION ALL
SELECT 'top-level replies whose nested reply count disagrees with the nested replies', COUNT(*)
  FROM gonof_forum_post p
 WHERE p.root_pid IS NULL
   AND p.sub_replies_count <> (SELECT COUNT(*) FROM gonof_forum_post c WHERE c.root_pid = p.pid)
UNION ALL
-- Same definition as content::LIVE_REPLY: not the first post, not hidden, not withdrawn, and the
-- top-level reply it belongs to is not hidden.
SELECT 'threads whose visible reply count disagrees with the visible replies', COUNT(*)
  FROM gonof_forum_thread t
 WHERE t.valid_replies <> (SELECT COUNT(*) FROM gonof_forum_post p
         LEFT JOIN gonof_forum_post parent ON parent.pid = p.root_pid
        WHERE p.tid = t.tid AND p.first = 0 AND p.invisible = 0 AND p.deleted_at = 0
          AND COALESCE(parent.invisible, 0) = 0)
UNION ALL
SELECT 'threads whose last reply belongs to another thread', COUNT(*)
  FROM gonof_forum_thread t
 WHERE t.last_reply_pid IS NOT NULL
   AND NOT EXISTS (SELECT 1 FROM gonof_forum_post p WHERE p.pid = t.last_reply_pid AND p.tid = t.tid AND p.first = 0)
UNION ALL
-- 3. Values and shapes (guaranteed by how code builds rows)
SELECT 'threads with invalid values', COUNT(*)
  FROM gonof_forum_thread
 WHERE NOT (version > 0 AND category BETWEEN 0 AND 5 AND root_replies >= 0 AND root_replies <= replies
   AND valid_replies >= 0 AND cache_revision > 0 AND CHAR_LENGTH(subject) BETWEEN 1 AND 255
   AND dateline >= 0 AND lastpost >= 0 AND views >= 0 AND replies >= 0
   AND digest >= 0 AND closed >= 0 AND readperm >= 0 AND status >= 0
   AND maxposition >= 1 AND deleted_at >= 0
   AND CAST(visibility AS BINARY) IN ('public', 'members', 'private')
   AND CAST(publication_state AS BINARY) IN ('draft', 'scheduled', 'published'))
UNION ALL
-- Shape: the first post is at position 1 with no reply target and no layer positions. A top-level reply has a
-- position above 1 and a top-level sequence number. A nested reply has both a parent and a target, a position within
-- its layer, and no nested replies of its own. No post replies to itself. An erased post has an empty body and is
-- withdrawn.
SELECT 'posts with invalid values or shape', COUNT(*)
  FROM gonof_forum_post p
 WHERE NOT (p.version > 0 AND p.edited_at >= 0 AND p.pid > 0 AND p.first IN (0, 1)
   AND ((p.first = 1 AND p.position = 1 AND p.root_pid IS NULL AND p.reply_to_pid IS NULL
         AND p.root_position IS NULL AND p.sub_position IS NULL)
     OR (p.first = 0 AND p.position > 1 AND p.root_pid IS NULL AND p.reply_to_pid IS NULL
         AND COALESCE(p.root_position, 0) > 0 AND p.sub_position IS NULL)
     OR (p.first = 0 AND p.position > 1 AND p.root_pid IS NOT NULL AND p.reply_to_pid IS NOT NULL
         AND p.root_position IS NULL AND COALESCE(p.sub_position, 0) > 0 AND p.sub_replies_count = 0))
   AND COALESCE(p.root_pid, 0) <> p.pid AND COALESCE(p.reply_to_pid, 0) <> p.pid
   AND p.dateline >= 0 AND p.status >= 0 AND p.sub_replies_count >= 0 AND p.deleted_at >= 0
   AND p.erased_at >= 0 AND (p.erased_at = 0 OR (p.message = '' AND p.deleted_at > 0)))
UNION ALL
-- Two-level replies: the parent is a top-level reply in the same thread with a smaller position, and the target is the
-- parent itself or a nested reply in the same layer, also with a smaller position.
SELECT 'nested replies that break the two-level reply structure', COUNT(*)
  FROM gonof_forum_post c
  LEFT JOIN gonof_forum_post r ON r.pid = c.root_pid
  LEFT JOIN gonof_forum_post g ON g.pid = c.reply_to_pid
 WHERE c.root_pid IS NOT NULL
   AND NOT (r.pid IS NOT NULL AND r.tid = c.tid AND r.first = 0 AND r.root_pid IS NULL AND r.position < c.position
        AND g.pid IS NOT NULL AND g.tid = c.tid AND (g.pid = c.root_pid OR g.root_pid <=> c.root_pid)
        AND g.position < c.position)
UNION ALL
SELECT 'forums with invalid values or nesting', COUNT(*)
  FROM gonof_forum_forum f
  LEFT JOIN gonof_forum_forum p ON p.fid = f.parent_fid
 WHERE CAST(f.visibility AS BINARY) NOT IN ('public', 'members', 'private')
    OR f.status NOT IN (0, 1) OR f.version < 1
    OR CHAR_LENGTH(f.name) NOT BETWEEN 1 AND 50
    OR CHAR_LENGTH(f.slug) NOT BETWEEN 1 AND 64
    OR CAST(f.slug AS BINARY) <> CAST(LOWER(TRIM(f.slug)) AS BINARY)
    OR (f.parent_fid IS NOT NULL AND (f.parent_fid = f.fid OR p.fid IS NULL OR p.parent_fid IS NOT NULL))
UNION ALL
-- The site content revision has exactly one row, id=1, with a positive revision. Without that row, the increments on
-- structural events fail and roll back.
SELECT 'site content revision is not a single row id=1 with a positive revision', 1
       - (SELECT COUNT(*) FROM gonof_common_content_revision WHERE id = 1 AND revision > 0)
       + (SELECT COUNT(*) FROM gonof_common_content_revision WHERE id <> 1)
UNION ALL
-- Trigger total, left over from the old trigger removal checks. `test_support::check_script` filters this row out.
SELECT 'trigger count minus 90', COUNT(*) - 90
  FROM information_schema.TRIGGERS
 WHERE TRIGGER_SCHEMA = DATABASE();
