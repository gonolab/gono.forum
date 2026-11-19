-- Per-member totals behind the all-time leaderboards; same structure and backfill as
-- db/migrations/mysql/0009_rank_member.sql.
CREATE TABLE `gonof_rank_member` (
  `uid` INTEGER NOT NULL,
  `threads` INTEGER NOT NULL DEFAULT 0,
  `replies` INTEGER NOT NULL DEFAULT 0,
  `likes` INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (`uid`),
  CONSTRAINT `gonof_rank_member_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE INDEX `gonof_rank_member__gonof_rank_member_threads` ON `gonof_rank_member` (`threads`, `uid`);
CREATE INDEX `gonof_rank_member__gonof_rank_member_replies` ON `gonof_rank_member` (`replies`, `uid`);
CREATE INDEX `gonof_rank_member__gonof_rank_member_likes` ON `gonof_rank_member` (`likes`, `uid`);

CREATE INDEX `gonof_common_growth_account__gonof_growth_rank` ON `gonof_common_growth_account` (`growth`, `uid`);

INSERT INTO `gonof_rank_member` (`uid`, `threads`, `replies`, `likes`)
SELECT p.`authorid`, SUM(p.`first` = 1), SUM(p.`first` = 0), SUM(p.`reactions` - (r.`uid` IS NOT NULL))
  FROM `gonof_forum_post` p
  JOIN `gonof_forum_thread` t ON t.`tid` = p.`tid`
  JOIN `gonof_forum_forum` f ON f.`fid` = t.`fid`
  LEFT JOIN `gonof_forum_forum` pf ON pf.`fid` = f.`parent_fid`
  LEFT JOIN `gonof_forum_post_reaction` r ON r.`pid` = p.`pid` AND r.`uid` = p.`authorid`
 WHERE p.`authorid` IS NOT NULL AND p.`invisible` = 0 AND p.`deleted_at` = 0 AND p.`erased_at` = 0
   AND p.`root_invisible` = 0 AND t.`displayorder` >= 0 AND t.`visibility` = 'public'
   AND f.`status` = 1 AND f.`visibility` = 'public'
   AND (pf.`fid` IS NULL OR (pf.`status` = 1 AND pf.`visibility` = 'public'))
 GROUP BY p.`authorid`;
