-- Skins imported in the admin area (docs/skins.md, "Importing"): one row per skin id. The manifest is stored
-- normalized as JSON, the palettes as one JSON object by name, the stylesheet as written. content_hash is the
-- SHA-256 of the skin pack; warnings holds the warnings the administrator confirmed on import, as codes. The size
-- limits (manifest 16 KiB, palettes 128 KiB, stylesheet 256 KiB) are checked by the code.
CREATE TABLE `gonof_common_skin` (
  `skin_id` varchar(32) COLLATE utf8mb4_bin NOT NULL,
  `skin_version` varchar(32) COLLATE utf8mb4_bin NOT NULL,
  `manifest` text COLLATE utf8mb4_bin NOT NULL,
  `palettes` mediumtext COLLATE utf8mb4_bin NOT NULL,
  `css` mediumtext COLLATE utf8mb4_bin NOT NULL,
  `content_hash` char(64) COLLATE utf8mb4_bin NOT NULL,
  `warnings` text COLLATE utf8mb4_bin NOT NULL,
  `imported_at` bigint(20) NOT NULL,
  `imported_by` bigint(20) DEFAULT NULL,
  `version` bigint(20) NOT NULL DEFAULT '1',
  PRIMARY KEY (`skin_id`),
  KEY `gonof_skin_imported_by_fk` (`imported_by`),
  CONSTRAINT `gonof_skin_imported_by_fk` FOREIGN KEY (`imported_by`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

-- The site's layout document (docs/skins.md, "Choosing a skin"), which holds the skin choice; read by the page frame
-- with the settings, like the navigation document. Empty means the built-in default skin.
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.layout.body','');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.layout.title','');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.layout.updated_at','0');
