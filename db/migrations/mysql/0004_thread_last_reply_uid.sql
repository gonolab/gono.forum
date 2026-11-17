-- The uid of the thread's last replier, a copy column next to last_reply_pid and last_reply_author (docs/queries.md,
-- "Copy columns"), so lists and data sources can show the last replier's avatar without joining posts or members.
-- NULL when the thread has no live reply, or when the last reply's author is deleted or unknown. Written with the
-- other last-reply copies by the reply statement, content::refresh_effective_replies and copies::member_changed.
ALTER TABLE `gonof_forum_thread`
  ADD `last_reply_uid` bigint(20) DEFAULT NULL,
  ADD KEY `gonof_thread_last_reply_uid_fk` (`last_reply_uid`),
  ADD CONSTRAINT `gonof_thread_last_reply_uid_fk` FOREIGN KEY (`last_reply_uid`) REFERENCES `gonof_common_member` (`uid`);

-- Backfill: the author of the post last_reply_pid points at, unless that account is deleted.
UPDATE `gonof_forum_thread` t
  JOIN `gonof_forum_post` p ON p.`pid` = t.`last_reply_pid`
  JOIN `gonof_common_member` m ON m.`uid` = p.`authorid`
   SET t.`last_reply_uid` = p.`authorid`
 WHERE m.`account_state` <> 'deleted';
