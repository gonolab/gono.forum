-- The uid of the thread's last replier; same structure as db/migrations/mysql/0004_thread_last_reply_uid.sql.
ALTER TABLE `gonof_forum_thread` ADD COLUMN `last_reply_uid` INTEGER DEFAULT NULL REFERENCES `gonof_common_member` (`uid`);
CREATE INDEX `gonof_forum_thread__gonof_thread_last_reply_uid_fk` ON `gonof_forum_thread` (`last_reply_uid`);

-- Backfill: the author of the post last_reply_pid points at, unless that account is deleted.
UPDATE `gonof_forum_thread`
   SET `last_reply_uid` = (
       SELECT p.`authorid`
         FROM `gonof_forum_post` p
         JOIN `gonof_common_member` m ON m.`uid` = p.`authorid`
        WHERE p.`pid` = `gonof_forum_thread`.`last_reply_pid` AND m.`account_state` <> 'deleted')
 WHERE `last_reply_pid` IS NOT NULL;
