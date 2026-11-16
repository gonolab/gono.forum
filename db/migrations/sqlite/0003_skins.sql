-- Skins imported in the admin area; same structure as db/migrations/mysql/0003_skins.sql.
CREATE TABLE `gonof_common_skin` (
  `skin_id` TEXT NOT NULL CHECK (length(`skin_id`) <= 32),
  `skin_version` TEXT NOT NULL CHECK (length(`skin_version`) <= 32),
  `manifest` TEXT NOT NULL CHECK (length(CAST(`manifest` AS BLOB)) <= 65535),
  `palettes` TEXT NOT NULL CHECK (length(CAST(`palettes` AS BLOB)) <= 16777215),
  `css` TEXT NOT NULL CHECK (length(CAST(`css` AS BLOB)) <= 16777215),
  `content_hash` TEXT NOT NULL CHECK (length(`content_hash`) <= 64),
  `warnings` TEXT NOT NULL CHECK (length(CAST(`warnings` AS BLOB)) <= 65535),
  `imported_at` INTEGER NOT NULL,
  `imported_by` INTEGER DEFAULT NULL,
  `version` INTEGER NOT NULL DEFAULT 1,
  PRIMARY KEY (`skin_id`),
  CONSTRAINT `gonof_skin_imported_by_fk` FOREIGN KEY (`imported_by`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE INDEX `gonof_common_skin__gonof_skin_imported_by_fk` ON `gonof_common_skin` (`imported_by`);

-- The site's layout document, which holds the skin choice; empty means the built-in default skin.
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.layout.body','');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.layout.title','');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.layout.updated_at','0');
