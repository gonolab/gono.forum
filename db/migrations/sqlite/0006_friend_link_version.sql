-- A friend link's version; same structure as db/migrations/mysql/0006_friend_link_version.sql.
ALTER TABLE `gonof_common_friend_link` ADD COLUMN `version` INTEGER NOT NULL DEFAULT 0;
