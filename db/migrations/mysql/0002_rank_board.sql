-- When each leaderboard was last computed. The time used to be read from the board's first place, so a board that came
-- out empty (nobody earned a like in the last 24 hours) kept no time: its page said "being generated" for good, and
-- every process aggregated it again on each round. One row per board and period, written in the transaction that
-- replaces the board's places.
CREATE TABLE `gonof_rank_board` (
  `board` varchar(16) COLLATE utf8mb4_bin NOT NULL,
  `period` varchar(8) COLLATE utf8mb4_bin NOT NULL,
  `computed_at` bigint(20) NOT NULL,
  PRIMARY KEY (`board`,`period`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;
