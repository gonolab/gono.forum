-- A board's color and built-in glyph; same structure as db/migrations/mysql/0005_board_color_glyph.sql.
ALTER TABLE `gonof_forum_forum` ADD `color` TEXT NOT NULL DEFAULT '' CHECK (length(`color`) <= 7);
ALTER TABLE `gonof_forum_forum` ADD `glyph` TEXT NOT NULL DEFAULT '' CHECK (length(`glyph`) <= 32);
