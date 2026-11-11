-- When each leaderboard was last computed; same structure as db/migrations/mysql/0002_rank_board.sql.
CREATE TABLE `gonof_rank_board` (
  `board` TEXT NOT NULL CHECK (length(`board`) <= 16),
  `period` TEXT NOT NULL CHECK (length(`period`) <= 8),
  `computed_at` INTEGER NOT NULL,
  PRIMARY KEY (`board`, `period`)
) STRICT;
