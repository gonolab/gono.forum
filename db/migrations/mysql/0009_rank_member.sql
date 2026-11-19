-- Per-member totals behind the all-time threads, replies and likes leaderboards (docs/queries.md, "Leaderboards"):
-- counted exactly like the boards count, only public content anonymous visitors can read (the post is not hidden,
-- withdrawn or erased and its floor is not hidden; the thread is listed and public; its board and the board's parent
-- are public and enabled), with likes from members other than the author. The write paths keep it in step in their
-- own transactions (ranks::counts), so a refresh reads the first places from the (total, uid) indexes instead of
-- aggregating every post. Members without counted content have no row.
CREATE TABLE `gonof_rank_member` (
  `uid` bigint(20) NOT NULL,
  `threads` bigint(20) NOT NULL DEFAULT '0',
  `replies` bigint(20) NOT NULL DEFAULT '0',
  `likes` bigint(20) NOT NULL DEFAULT '0',
  PRIMARY KEY (`uid`),
  KEY `gonof_rank_member_threads` (`threads`,`uid`),
  KEY `gonof_rank_member_replies` (`replies`,`uid`),
  KEY `gonof_rank_member_likes` (`likes`,`uid`),
  CONSTRAINT `gonof_rank_member_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

-- The all-time growth board reads its first places from this index the same way.
ALTER TABLE `gonof_common_growth_account`
  ADD KEY `gonof_growth_rank` (`growth`,`uid`);

-- Backfill from the posts. A like count is the post's `reactions` less the author's own like.
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
