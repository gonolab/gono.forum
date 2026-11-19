-- An edit request's post version; same structure as db/migrations/mysql/0007_post_request_version.sql.
ALTER TABLE `gonof_forum_post_request` ADD COLUMN `version` INTEGER DEFAULT NULL;
