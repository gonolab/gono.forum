-- A board's color and built-in glyph (docs/skins.md, "Board colors and glyphs"): `color` is empty or `#rrggbb` in
-- lowercase, `glyph` empty or the name of a built-in board glyph (organization/glyphs.rs). The code checks both on
-- every save; the server writes the board stylesheet from them.
ALTER TABLE `gonof_forum_forum`
  ADD `color` varchar(7) COLLATE utf8mb4_bin NOT NULL DEFAULT '',
  ADD `glyph` varchar(32) COLLATE utf8mb4_bin NOT NULL DEFAULT '';
