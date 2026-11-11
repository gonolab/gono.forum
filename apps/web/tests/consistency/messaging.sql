-- Messaging and attachment consistency check: read-only SELECTs, one row per item.
--
-- `test_support::check_script` runs this file against a test database after the tests have written through the real
-- code paths (`tests/messaging_rules.rs` runs it at the end of every test, other tests use it too) and returns each
-- item with its amount. Every amount must be 0: anything else means a row that breaks the rules below reached the
-- database by bypassing code. The checks use MySQL syntax and run on MySQL only, on SQLite the helper returns no rows.
-- When you change a matching rule in code, change the check here too.
--
-- Covers private messages, follows, blocks, favorites, likes, requests, revisions and attachments. The schema has no
-- triggers (docs/architecture.md, "Data"), so these rules are guaranteed only by code at write time: the value checks
-- at each write entry point, the conditional updates on private messages and attachments, and the source scan in
-- db/source_guards.rs. Code only checks new writes and never corrects old values, so every stored row must satisfy the
-- same rules. Enumerations and hex strings compare bytes.
-- Upper length limits given by column widths (over-long values fail under STRICT_TRANS_TABLES) are enforced by the
-- database, so only lower limits, combinations and values are checked here. The private message body is TEXT, so its
-- upper limit is checked too.

-- 1. Attachments
SELECT 'attachments whose request key or digest is not 64 characters' AS item, COUNT(*) AS amount
  FROM gonof_forum_attachment
 WHERE CHAR_LENGTH(request_key) <> 64 OR CHAR_LENGTH(request_hash) <> 64
UNION ALL
SELECT 'attachment rows with invalid values', COUNT(*)
  FROM gonof_forum_attachment_0
 WHERE NOT (CHAR_LENGTH(filename) BETWEEN 1 AND 255 AND filesize >= 0
   AND CHAR_LENGTH(mime_type) BETWEEN 1 AND 127 AND CHAR_LENGTH(content_sha256) = 64
   AND CHAR_LENGTH(attachment) BETWEEN 1 AND 1024 AND CHAR_LENGTH(storage_profile_id) BETWEEN 1 AND 64
   AND ((width IS NULL AND height IS NULL) OR (width IS NOT NULL AND height IS NOT NULL AND width > 0 AND height > 0))
   AND CAST(lifecycle AS BINARY) IN ('pending', 'processing', 'ready', 'rejected', 'purging', 'purged')
   AND CAST(scan_status AS BINARY) IN ('pending', 'clean', 'rejected', 'error'))
UNION ALL
-- The update, check and purge times are never earlier than the upload time (writes take the later of the given time
-- and the upload time).
SELECT 'attachment rows dated before their upload', COUNT(*)
  FROM gonof_forum_attachment_0
 WHERE NOT (dateline >= 0 AND updated_at >= dateline
   AND (checked_at IS NULL OR checked_at >= dateline)
   AND (purge_after IS NULL OR purge_after >= dateline))
UNION ALL
SELECT 'attachment references, likes or favorites with a negative time', (SELECT COUNT(*) FROM gonof_forum_attachment_reference WHERE dateline < 0)
       + (SELECT COUNT(*) FROM gonof_forum_post_reaction WHERE dateline < 0)
       + (SELECT COUNT(*) FROM gonof_home_favorite WHERE dateline < 0)
UNION ALL
-- 2. Requests and revisions
SELECT 'post requests whose request key or digest is not 64 characters', COUNT(*)
  FROM gonof_forum_post_request
 WHERE CHAR_LENGTH(request_key) <> 64 OR CHAR_LENGTH(request_hash) <> 64
UNION ALL
SELECT 'interaction requests whose request key or digest is not 64 lowercase hex characters', COUNT(*)
  FROM gonof_home_interaction_request
 WHERE NOT (uid > 0 AND request_key REGEXP '^[a-f0-9]{64}$' AND request_hash REGEXP '^[a-f0-9]{64}$' AND dateline >= 0)
UNION ALL
SELECT 'post revisions with an invalid version or time', COUNT(*)
  FROM gonof_forum_post_revision
 WHERE NOT (version > 0 AND dateline >= 0)
UNION ALL
-- 3. Follows and blocks
SELECT 'thread or forum follows with invalid values', (SELECT COUNT(*) FROM gonof_home_thread_follow WHERE NOT (uid > 0 AND tid > 0 AND dateline >= 0))
       + (SELECT COUNT(*) FROM gonof_home_forum_follow WHERE NOT (uid > 0 AND fid > 0 AND dateline >= 0))
UNION ALL
SELECT 'blocks of oneself or with invalid values', COUNT(*)
  FROM gonof_home_member_block
 WHERE NOT (uid > 0 AND blocked_uid > 0 AND uid <> blocked_uid AND dateline >= 0)
UNION ALL
-- 4. Private messages
SELECT 'private messages with an invalid body, event key or time', COUNT(*)
  FROM gonof_home_message
 WHERE NOT (CHAR_LENGTH(body) BETWEEN 1 AND 2000 AND CHAR_LENGTH(event_key) BETWEEN 1 AND 100 AND dateline >= 0)
UNION ALL
SELECT 'private message state rows with invalid values', COUNT(*)
  FROM gonof_home_message_member
 WHERE NOT (counterpart_uid <> uid AND unread >= 0 AND last_read_id >= 0 AND last_dateline >= 0)
UNION ALL
-- The conversation key is "smaller uid:larger uid", both parts positive integers without leading zeros.
SELECT 'conversations whose pair key is malformed or out of order', COUNT(*)
  FROM gonof_home_message_thread
 WHERE NOT (pair_key REGEXP '^[1-9][0-9]*:[1-9][0-9]*$'
   AND CAST(SUBSTRING_INDEX(pair_key, ':', 1) AS UNSIGNED) < CAST(SUBSTRING_INDEX(pair_key, ':', -1) AS UNSIGNED)
   AND last_message_id >= 0 AND dateline >= 0)
UNION ALL
-- Participants: every conversation has exactly two state rows, each the counterpart of the other, and they are
-- exactly the two sides of the conversation key. The sender is one of the two sides. The last message of a
-- conversation is its message with the highest id (sending writes the message and advances it in the same
-- transaction, and private messages are never deleted).
SELECT 'conversations whose state rows disagree with the two members of the pair key', COUNT(*)
  FROM gonof_home_message_thread t
 WHERE (SELECT COUNT(*) FROM gonof_home_message_member m
         WHERE m.thread_id = t.id
           AND CAST(CONCAT(LEAST(m.uid, m.counterpart_uid), ':', GREATEST(m.uid, m.counterpart_uid)) AS BINARY) = CAST(t.pair_key AS BINARY)) <> 2
    OR (SELECT COUNT(*) FROM gonof_home_message_member m WHERE m.thread_id = t.id) <> 2
UNION ALL
SELECT 'private messages whose sender is not one of the two members of the conversation', COUNT(*)
  FROM gonof_home_message m
  JOIN gonof_home_message_thread t ON t.id = m.thread_id
 WHERE CAST(m.sender_uid AS CHAR) NOT IN (SUBSTRING_INDEX(t.pair_key, ':', 1), SUBSTRING_INDEX(t.pair_key, ':', -1))
UNION ALL
SELECT 'conversations whose last message is not their newest message', COUNT(*)
  FROM gonof_home_message_thread t
 WHERE t.last_message_id <> COALESCE((SELECT MAX(m.id) FROM gonof_home_message m WHERE m.thread_id = t.id), 0)
UNION ALL
-- Trigger total, left over from the old trigger removal checks. `test_support::check_script` filters this row out.
SELECT 'trigger count (28 before deployment, 0 after)', COUNT(*)
  FROM information_schema.TRIGGERS
 WHERE TRIGGER_SCHEMA = DATABASE();
