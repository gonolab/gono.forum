-- Gono Forum database baseline (SQLite 3.46+): STRICT tables, text compared as BINARY. Same structure and seed rows as
-- db/migrations/mysql/0001_baseline.sql, from which it is generated mechanically: varchar/char get a character-count
-- CHECK, text gets a 65535-byte CHECK, single-column auto-increment keys use AUTOINCREMENT, and every index is named
-- "table__original_name" (SQLite index names are unique per database). tests/sqlite_schema.rs compares both backends
-- item by item after migrating.
--
-- No triggers, routines, events or views: the project uses none (docs/architecture.md, "Data") and the migration guard
-- rejects them in both directories.
-- Released: changing a single byte makes SQLx refuse to start on the checksum; later schema changes are appended with
-- the same number as on MySQL, starting at 0002.

CREATE TABLE `gonof_common_content_revision` (
  `id` INTEGER NOT NULL,
  `revision` INTEGER NOT NULL DEFAULT 1,
  PRIMARY KEY (`id`)
) STRICT;

CREATE TABLE `gonof_common_credit_account` (
  `uid` INTEGER NOT NULL,
  `available` INTEGER NOT NULL DEFAULT 0,
  `frozen` INTEGER NOT NULL DEFAULT 0,
  `version` INTEGER NOT NULL DEFAULT 1,
  PRIMARY KEY (`uid`),
  CONSTRAINT `gonof_common_credit_account_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;

CREATE TABLE `gonof_common_credit_ledger` (
  `id` INTEGER PRIMARY KEY AUTOINCREMENT,
  `event_key` TEXT NOT NULL CHECK (length(`event_key`) <= 190),
  `uid` INTEGER NOT NULL,
  `available_delta` INTEGER NOT NULL,
  `frozen_delta` INTEGER NOT NULL,
  `available_after` INTEGER NOT NULL,
  `frozen_after` INTEGER NOT NULL,
  `kind` TEXT NOT NULL CHECK (length(`kind`) <= 40),
  `tid` INTEGER DEFAULT NULL,
  `pid` INTEGER DEFAULT NULL,
  `operator_uid` INTEGER DEFAULT NULL,
  `reason` TEXT NOT NULL DEFAULT '' CHECK (length(`reason`) <= 500),
  `dateline` INTEGER NOT NULL,
  CONSTRAINT `gonof_common_credit_ledger_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_common_credit_ledger_ibfk_2` FOREIGN KEY (`tid`) REFERENCES `gonof_forum_thread` (`tid`),
  CONSTRAINT `gonof_common_credit_ledger_ibfk_3` FOREIGN KEY (`pid`) REFERENCES `gonof_forum_post` (`pid`),
  CONSTRAINT `gonof_common_credit_ledger_ibfk_4` FOREIGN KEY (`operator_uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE UNIQUE INDEX `gonof_common_credit_ledger__event_key` ON `gonof_common_credit_ledger` (`event_key`);
CREATE INDEX `gonof_common_credit_ledger__pid` ON `gonof_common_credit_ledger` (`pid`);
CREATE INDEX `gonof_common_credit_ledger__operator_uid` ON `gonof_common_credit_ledger` (`operator_uid`);
CREATE INDEX `gonof_common_credit_ledger__gonof_credit_member_history` ON `gonof_common_credit_ledger` (`uid`, `dateline`, `id`);
CREATE INDEX `gonof_common_credit_ledger__gonof_credit_rule_daily` ON `gonof_common_credit_ledger` (`uid`, `kind`, `dateline`);
CREATE INDEX `gonof_common_credit_ledger__gonof_credit_thread_kind` ON `gonof_common_credit_ledger` (`tid`, `kind`, `id`);

CREATE TABLE `gonof_common_credit_rule` (
  `rule_key` TEXT NOT NULL CHECK (length(`rule_key`) <= 24),
  `enabled` INTEGER NOT NULL DEFAULT 0,
  `amount` INTEGER NOT NULL DEFAULT 1,
  `daily_limit` INTEGER NOT NULL DEFAULT 1,
  `version` INTEGER NOT NULL DEFAULT 1,
  PRIMARY KEY (`rule_key`)
) STRICT;

CREATE TABLE `gonof_common_data_request` (
  `id` INTEGER PRIMARY KEY AUTOINCREMENT,
  `uid` INTEGER NOT NULL,
  `kind` TEXT NOT NULL CHECK (length(`kind`) <= 16),
  `content_mode` TEXT NOT NULL DEFAULT 'retain' CHECK (length(`content_mode`) <= 16),
  `status` TEXT NOT NULL CHECK (length(`status`) <= 16),
  `request_key` TEXT NOT NULL CHECK (length(`request_key`) <= 64),
  `request_hash` TEXT NOT NULL CHECK (length(`request_hash`) <= 64),
  `dateline` INTEGER NOT NULL,
  `execute_after` INTEGER NOT NULL,
  `finished_at` INTEGER NOT NULL DEFAULT 0,
  `version` INTEGER NOT NULL DEFAULT 1,
  `stage` TEXT NOT NULL DEFAULT 'start' CHECK (length(`stage`) <= 24),
  `cursor_pid` INTEGER NOT NULL DEFAULT 0,
  `upper_pid` INTEGER NOT NULL DEFAULT 0,
  `artifact_key` TEXT DEFAULT NULL CHECK (length(`artifact_key`) <= 160),
  `artifact_hash` TEXT DEFAULT NULL CHECK (length(`artifact_hash`) <= 64),
  `artifact_bytes` INTEGER DEFAULT NULL,
  `artifact_expires_at` INTEGER DEFAULT NULL,
  `last_error` TEXT NOT NULL DEFAULT '' CHECK (length(`last_error`) <= 1000),
  `active_slot` TEXT GENERATED ALWAYS AS ((case when (`status` in ('pending','cooling','processing','failed')) then (case when (`kind` = 'export') then 'export' else 'destructive' end) else NULL end)) STORED,
  CONSTRAINT `gonof_common_data_request_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE UNIQUE INDEX `gonof_common_data_request__uid` ON `gonof_common_data_request` (`uid`, `request_key`);
CREATE UNIQUE INDEX `gonof_common_data_request__uid_2` ON `gonof_common_data_request` (`uid`, `active_slot`);
CREATE INDEX `gonof_common_data_request__gonof_data_request_queue` ON `gonof_common_data_request` (`status`, `execute_after`, `id`);
CREATE INDEX `gonof_common_data_request__gonof_data_request_owner` ON `gonof_common_data_request` (`uid`, `id`);

CREATE TABLE `gonof_common_friend_link` (
  `id` INTEGER PRIMARY KEY AUTOINCREMENT,
  `status` TEXT NOT NULL CHECK (length(`status`) <= 16),
  `category` TEXT NOT NULL CHECK (length(`category`) <= 16),
  `name` TEXT NOT NULL CHECK (length(`name`) <= 30),
  `url` TEXT NOT NULL CHECK (length(`url`) <= 500),
  `domain` TEXT NOT NULL CHECK (length(`domain`) <= 190),
  `live_domain` TEXT DEFAULT NULL CHECK (length(`live_domain`) <= 190),
  `description` TEXT NOT NULL DEFAULT '' CHECK (length(`description`) <= 60),
  `backlink_url` TEXT DEFAULT NULL CHECK (length(`backlink_url`) <= 500),
  `note` TEXT NOT NULL DEFAULT '' CHECK (length(`note`) <= 200),
  `home` INTEGER NOT NULL DEFAULT 0,
  `displayorder` INTEGER NOT NULL DEFAULT 0,
  `owner_uid` INTEGER DEFAULT NULL,
  `request_key` TEXT DEFAULT NULL CHECK (length(`request_key`) <= 64),
  `handler_uid` INTEGER DEFAULT NULL,
  `resolution` TEXT NOT NULL DEFAULT '' CHECK (length(`resolution`) <= 200),
  `dateline` INTEGER NOT NULL,
  `handled_at` INTEGER DEFAULT NULL,
  `updated_at` INTEGER NOT NULL,
  CONSTRAINT `gonof_common_friend_link_ibfk_1` FOREIGN KEY (`owner_uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_common_friend_link_ibfk_2` FOREIGN KEY (`handler_uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE UNIQUE INDEX `gonof_common_friend_link__gonof_link_live_domain` ON `gonof_common_friend_link` (`live_domain`);
CREATE UNIQUE INDEX `gonof_common_friend_link__gonof_link_request` ON `gonof_common_friend_link` (`owner_uid`, `request_key`);
CREATE INDEX `gonof_common_friend_link__gonof_link_list` ON `gonof_common_friend_link` (`status`, `category`, `displayorder`, `id`);
CREATE INDEX `gonof_common_friend_link__gonof_link_home` ON `gonof_common_friend_link` (`status`, `home`, `displayorder`, `id`);
CREATE INDEX `gonof_common_friend_link__gonof_link_queue` ON `gonof_common_friend_link` (`status`, `id`);
CREATE INDEX `gonof_common_friend_link__gonof_link_domain` ON `gonof_common_friend_link` (`domain`, `id`);
CREATE INDEX `gonof_common_friend_link__handler_uid` ON `gonof_common_friend_link` (`handler_uid`);

CREATE TABLE `gonof_common_growth_account` (
  `uid` INTEGER NOT NULL,
  `growth` INTEGER NOT NULL DEFAULT 0,
  `version` INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (`uid`),
  CONSTRAINT `gonof_common_growth_account_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;

CREATE TABLE `gonof_common_growth_ledger` (
  `id` INTEGER PRIMARY KEY AUTOINCREMENT,
  `event_key` TEXT NOT NULL CHECK (length(`event_key`) <= 190),
  `uid` INTEGER NOT NULL,
  `delta` INTEGER NOT NULL,
  `growth_after` INTEGER NOT NULL,
  `kind` TEXT NOT NULL CHECK (length(`kind`) <= 16),
  `pid` INTEGER DEFAULT NULL,
  `operator_uid` INTEGER DEFAULT NULL,
  `reason` TEXT NOT NULL DEFAULT '' CHECK (length(`reason`) <= 200),
  `dateline` INTEGER NOT NULL,
  CONSTRAINT `gonof_common_growth_ledger_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_common_growth_ledger_ibfk_2` FOREIGN KEY (`pid`) REFERENCES `gonof_forum_post` (`pid`),
  CONSTRAINT `gonof_common_growth_ledger_ibfk_3` FOREIGN KEY (`operator_uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE UNIQUE INDEX `gonof_common_growth_ledger__gonof_growth_event` ON `gonof_common_growth_ledger` (`event_key`);
CREATE INDEX `gonof_common_growth_ledger__gonof_growth_member_history` ON `gonof_common_growth_ledger` (`uid`, `dateline`, `id`);
CREATE INDEX `gonof_common_growth_ledger__gonof_growth_window` ON `gonof_common_growth_ledger` (`dateline`, `uid`, `delta`);
CREATE INDEX `gonof_common_growth_ledger__pid` ON `gonof_common_growth_ledger` (`pid`);
CREATE INDEX `gonof_common_growth_ledger__operator_uid` ON `gonof_common_growth_ledger` (`operator_uid`);

CREATE TABLE `gonof_common_growth_post` (
  `pid` INTEGER NOT NULL,
  `uid` INTEGER NOT NULL,
  `points_base` INTEGER NOT NULL,
  `points_like` INTEGER NOT NULL,
  `points_digest` INTEGER NOT NULL,
  `points_accept` INTEGER NOT NULL,
  `credited` INTEGER NOT NULL DEFAULT 0,
  `revision` INTEGER NOT NULL DEFAULT 0,
  `dateline` INTEGER NOT NULL,
  PRIMARY KEY (`pid`),
  CONSTRAINT `gonof_common_growth_post_ibfk_1` FOREIGN KEY (`pid`) REFERENCES `gonof_forum_post` (`pid`),
  CONSTRAINT `gonof_common_growth_post_ibfk_2` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE INDEX `gonof_common_growth_post__uid` ON `gonof_common_growth_post` (`uid`);

CREATE TABLE `gonof_common_honor` (
  `id` INTEGER PRIMARY KEY AUTOINCREMENT,
  `name` TEXT NOT NULL CHECK (length(`name`) <= 20),
  `description` TEXT NOT NULL CHECK (length(`description`) <= 60),
  `displayorder` INTEGER NOT NULL DEFAULT 0,
  `status` INTEGER NOT NULL DEFAULT 1,
  `version` INTEGER NOT NULL DEFAULT 1
) STRICT;
CREATE UNIQUE INDEX `gonof_common_honor__name` ON `gonof_common_honor` (`name`);
CREATE INDEX `gonof_common_honor__gonof_honor_order` ON `gonof_common_honor` (`displayorder`, `id`);

CREATE TABLE `gonof_common_invite` (
  `id` INTEGER PRIMARY KEY AUTOINCREMENT,
  `uid` INTEGER NOT NULL,
  `code` TEXT NOT NULL CHECK (length(`code`) <= 64),
  `request_key` TEXT NOT NULL CHECK (length(`request_key`) <= 64),
  `fuid` INTEGER DEFAULT NULL,
  `dateline` INTEGER NOT NULL,
  `endtime` INTEGER NOT NULL,
  `regdateline` INTEGER DEFAULT NULL,
  `status` TEXT NOT NULL DEFAULT 'available' CHECK (length(`status`) <= 16),
  CONSTRAINT `gonof_common_invite_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_common_invite_ibfk_2` FOREIGN KEY (`fuid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE UNIQUE INDEX `gonof_common_invite__code` ON `gonof_common_invite` (`code`);
CREATE UNIQUE INDEX `gonof_common_invite__uid` ON `gonof_common_invite` (`uid`, `request_key`);
CREATE UNIQUE INDEX `gonof_common_invite__fuid` ON `gonof_common_invite` (`fuid`);
CREATE INDEX `gonof_common_invite__gonof_invite_owner_created` ON `gonof_common_invite` (`uid`, `dateline`);
CREATE INDEX `gonof_common_invite__gonof_invite_owner_available` ON `gonof_common_invite` (`uid`, `status`, `endtime`);
CREATE INDEX `gonof_common_invite__gonof_invite_owner_id` ON `gonof_common_invite` (`uid`, `id`);

CREATE TABLE `gonof_common_invite_batch` (
  `uid` INTEGER NOT NULL,
  `request_key` TEXT NOT NULL CHECK (length(`request_key`) <= 64),
  `request_hash` TEXT NOT NULL CHECK (length(`request_hash`) <= 64),
  `invites` INTEGER NOT NULL,
  `dateline` INTEGER NOT NULL,
  PRIMARY KEY (`uid`, `request_key`),
  CONSTRAINT `gonof_common_invite_batch_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;

CREATE TABLE `gonof_common_localized_text` (
  `kind` TEXT NOT NULL CHECK (length(`kind`) <= 32),
  `language` TEXT NOT NULL CHECK (length(`language`) <= 16),
  `title` TEXT NOT NULL CHECK (length(`title`) <= 120),
  `body` TEXT NOT NULL CHECK (length(CAST(`body` AS BLOB)) <= 65535),
  `updated_at` INTEGER NOT NULL,
  PRIMARY KEY (`kind`, `language`)
) STRICT;

CREATE TABLE `gonof_common_log` (
  `id` INTEGER PRIMARY KEY AUTOINCREMENT,
  `uid` INTEGER DEFAULT NULL,
  `operationuid` INTEGER DEFAULT NULL,
  `type` TEXT NOT NULL CHECK (length(`type`) <= 100),
  `data` TEXT NOT NULL CHECK (length(CAST(`data` AS BLOB)) <= 65535),
  `source` TEXT NOT NULL CHECK (length(`source`) <= 32),
  `dateline` INTEGER NOT NULL,
  `fid` INTEGER DEFAULT NULL,
  `to_fid` INTEGER DEFAULT NULL,
  `pid` INTEGER DEFAULT NULL,
  CONSTRAINT `gonof_common_log_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_common_log_ibfk_2` FOREIGN KEY (`operationuid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE INDEX `gonof_common_log__gonof_log_member_created` ON `gonof_common_log` (`uid`, `dateline`);
CREATE INDEX `gonof_common_log__gonof_log_operator_created` ON `gonof_common_log` (`operationuid`, `dateline`);
CREATE INDEX `gonof_common_log__gonof_log_type_created` ON `gonof_common_log` (`type`, `dateline`);
CREATE INDEX `gonof_common_log__gonof_log_post_created` ON `gonof_common_log` (`pid`, `dateline`);

CREATE TABLE `gonof_common_member` (
  `uid` INTEGER PRIMARY KEY AUTOINCREMENT,
  `username` TEXT NOT NULL CHECK (length(`username`) <= 50),
  `avatar` INTEGER NOT NULL DEFAULT 0,
  `email` TEXT DEFAULT NULL CHECK (length(`email`) <= 255),
  `mobile` TEXT DEFAULT NULL CHECK (length(`mobile`) <= 16),
  `password` TEXT DEFAULT NULL CHECK (length(`password`) <= 512),
  `emailstatus` INTEGER NOT NULL DEFAULT 0,
  `adminid` INTEGER NOT NULL DEFAULT 0,
  `freeze` INTEGER NOT NULL DEFAULT 0,
  `mute_until` INTEGER NOT NULL DEFAULT 0,
  `regdate` INTEGER NOT NULL DEFAULT 0,
  `credentials_version` INTEGER NOT NULL DEFAULT 1,
  `bio` TEXT NOT NULL DEFAULT '' CHECK (length(`bio`) <= 500),
  `avatar_aid` INTEGER DEFAULT NULL,
  `unread_notifications` INTEGER NOT NULL DEFAULT 0,
  `space_visibility` TEXT NOT NULL DEFAULT 'public' CHECK (length(`space_visibility`) <= 16),
  `account_state` TEXT NOT NULL DEFAULT 'active' CHECK (length(`account_state`) <= 16),
  CONSTRAINT `gonof_member_avatar_fk` FOREIGN KEY (`avatar_aid`) REFERENCES `gonof_forum_attachment` (`aid`)
) STRICT;
CREATE UNIQUE INDEX `gonof_common_member__username` ON `gonof_common_member` (`username`);
CREATE UNIQUE INDEX `gonof_common_member__gonof_member_email` ON `gonof_common_member` (`email`);
CREATE UNIQUE INDEX `gonof_common_member__gonof_member_mobile` ON `gonof_common_member` (`mobile`);
CREATE INDEX `gonof_common_member__gonof_member_avatar_attachment` ON `gonof_common_member` (`avatar_aid`);

CREATE TABLE `gonof_common_member_api_key` (
  `id` INTEGER PRIMARY KEY AUTOINCREMENT,
  `uid` INTEGER NOT NULL,
  `key_hash` TEXT NOT NULL CHECK (length(`key_hash`) <= 64),
  `key_hint` TEXT NOT NULL CHECK (length(`key_hint`) <= 16),
  `dateline` INTEGER NOT NULL,
  `expires_at` INTEGER NOT NULL,
  `last_used_at` INTEGER NOT NULL DEFAULT 0,
  `last_used_ip` TEXT NOT NULL DEFAULT '' CHECK (length(`last_used_ip`) <= 45),
  `revoked_at` INTEGER DEFAULT NULL,
  `revoked_by` INTEGER DEFAULT NULL,
  CONSTRAINT `gonof_common_member_api_key_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_common_member_api_key_ibfk_2` FOREIGN KEY (`revoked_by`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE UNIQUE INDEX `gonof_common_member_api_key__key_hash` ON `gonof_common_member_api_key` (`key_hash`);
CREATE INDEX `gonof_common_member_api_key__gonof_member_api_key_owner` ON `gonof_common_member_api_key` (`uid`, `revoked_at`);
CREATE INDEX `gonof_common_member_api_key__revoked_by` ON `gonof_common_member_api_key` (`revoked_by`);

CREATE TABLE `gonof_common_member_appeal` (
  `id` INTEGER PRIMARY KEY AUTOINCREMENT,
  `uid` INTEGER NOT NULL,
  `sanction_id` INTEGER NOT NULL,
  `reason` TEXT NOT NULL CHECK (length(`reason`) <= 1000),
  `status` TEXT NOT NULL DEFAULT 'pending' CHECK (length(`status`) <= 16),
  `handler_uid` INTEGER DEFAULT NULL,
  `resolution` TEXT NOT NULL DEFAULT '' CHECK (length(`resolution`) <= 1000),
  `dateline` INTEGER NOT NULL,
  `handled_at` INTEGER NOT NULL DEFAULT 0,
  `request_key` TEXT NOT NULL CHECK (length(`request_key`) <= 64),
  CONSTRAINT `gonof_common_member_appeal_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_common_member_appeal_ibfk_2` FOREIGN KEY (`sanction_id`) REFERENCES `gonof_common_member_sanction` (`id`),
  CONSTRAINT `gonof_common_member_appeal_ibfk_3` FOREIGN KEY (`handler_uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE UNIQUE INDEX `gonof_common_member_appeal__sanction_id` ON `gonof_common_member_appeal` (`sanction_id`);
CREATE UNIQUE INDEX `gonof_common_member_appeal__uid` ON `gonof_common_member_appeal` (`uid`, `request_key`);
CREATE INDEX `gonof_common_member_appeal__handler_uid` ON `gonof_common_member_appeal` (`handler_uid`);
CREATE INDEX `gonof_common_member_appeal__gonof_appeal_queue` ON `gonof_common_member_appeal` (`status`, `id`);
CREATE INDEX `gonof_common_member_appeal__gonof_appeal_owner` ON `gonof_common_member_appeal` (`uid`, `id`);

CREATE TABLE `gonof_common_member_count` (
  `uid` INTEGER NOT NULL,
  `threads` INTEGER NOT NULL DEFAULT 0,
  `replies` INTEGER NOT NULL DEFAULT 0,
  `digests` INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (`uid`),
  CONSTRAINT `gonof_common_member_count_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;

CREATE TABLE `gonof_common_member_external` (
  `provider` TEXT NOT NULL CHECK (length(`provider`) <= 16),
  `subject` TEXT NOT NULL CHECK (length(`subject`) <= 255),
  `uid` INTEGER NOT NULL,
  `display` TEXT NOT NULL DEFAULT '' CHECK (length(`display`) <= 255),
  `dateline` INTEGER NOT NULL,
  `last_login_at` INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (`provider`, `subject`),
  CONSTRAINT `gonof_common_member_external_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE UNIQUE INDEX `gonof_common_member_external__gonof_member_external_uid` ON `gonof_common_member_external` (`uid`, `provider`);

CREATE TABLE `gonof_common_member_honor` (
  `uid` INTEGER NOT NULL,
  `honor_id` INTEGER NOT NULL,
  `granted_by` INTEGER NOT NULL,
  `granted_at` INTEGER NOT NULL,
  `expires_at` INTEGER DEFAULT NULL,
  PRIMARY KEY (`uid`, `honor_id`),
  CONSTRAINT `gonof_member_honor_granted_by_fk` FOREIGN KEY (`granted_by`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_member_honor_group_fk` FOREIGN KEY (`honor_id`) REFERENCES `gonof_common_honor` (`id`),
  CONSTRAINT `gonof_member_honor_uid_fk` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE INDEX `gonof_common_member_honor__gonof_member_honor_group` ON `gonof_common_member_honor` (`honor_id`, `granted_at`, `uid`);
CREATE INDEX `gonof_common_member_honor__gonof_member_honor_expiry` ON `gonof_common_member_honor` (`expires_at`);
CREATE INDEX `gonof_common_member_honor__granted_by` ON `gonof_common_member_honor` (`granted_by`);

CREATE TABLE `gonof_common_member_presence` (
  `uid` INTEGER NOT NULL,
  `last_seen` INTEGER NOT NULL,
  PRIMARY KEY (`uid`),
  CONSTRAINT `gonof_common_member_presence_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;

CREATE TABLE `gonof_common_member_recovery_code` (
  `uid` INTEGER NOT NULL,
  `code_hash` TEXT NOT NULL CHECK (length(`code_hash`) <= 64),
  `dateline` INTEGER NOT NULL,
  PRIMARY KEY (`uid`, `code_hash`),
  CONSTRAINT `gonof_common_member_recovery_code_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;

CREATE TABLE `gonof_common_member_register` (
  `uid` INTEGER NOT NULL,
  `site` TEXT DEFAULT NULL CHECK (length(`site`) <= 500),
  `channel` TEXT DEFAULT NULL CHECK (length(`channel`) <= 64),
  `inviter_uid` INTEGER DEFAULT NULL,
  `invite_prefix` TEXT DEFAULT NULL CHECK (length(`invite_prefix`) <= 64),
  PRIMARY KEY (`uid`),
  CONSTRAINT `gonof_common_member_register_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_common_member_register_ibfk_2` FOREIGN KEY (`inviter_uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE INDEX `gonof_common_member_register__gonof_member_register_site` ON `gonof_common_member_register` (`site`);
CREATE INDEX `gonof_common_member_register__gonof_member_register_channel` ON `gonof_common_member_register` (`channel`);
CREATE INDEX `gonof_common_member_register__gonof_member_register_inviter` ON `gonof_common_member_register` (`inviter_uid`);
CREATE INDEX `gonof_common_member_register__gonof_member_register_prefix` ON `gonof_common_member_register` (`invite_prefix`);

CREATE TABLE `gonof_common_member_role_request` (
  `uid` INTEGER NOT NULL,
  `request_key` TEXT NOT NULL CHECK (length(`request_key`) <= 64),
  `request_hash` TEXT NOT NULL CHECK (length(`request_hash`) <= 64),
  `target_uid` INTEGER NOT NULL,
  PRIMARY KEY (`uid`, `request_key`),
  CONSTRAINT `gonof_common_member_role_request_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_common_member_role_request_ibfk_2` FOREIGN KEY (`target_uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE INDEX `gonof_common_member_role_request__target_uid` ON `gonof_common_member_role_request` (`target_uid`);

CREATE TABLE `gonof_common_member_sanction` (
  `id` INTEGER PRIMARY KEY AUTOINCREMENT,
  `uid` INTEGER NOT NULL,
  `kind` TEXT NOT NULL CHECK (length(`kind`) <= 16),
  `reason` TEXT NOT NULL CHECK (length(`reason`) <= 500),
  `operator_uid` INTEGER NOT NULL,
  `starts_at` INTEGER NOT NULL,
  `expires_at` INTEGER NOT NULL DEFAULT 0,
  `revoked_at` INTEGER NOT NULL DEFAULT 0,
  `revoked_by` INTEGER DEFAULT NULL,
  `request_key` TEXT NOT NULL CHECK (length(`request_key`) <= 64),
  CONSTRAINT `gonof_common_member_sanction_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_common_member_sanction_ibfk_2` FOREIGN KEY (`operator_uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_common_member_sanction_ibfk_3` FOREIGN KEY (`revoked_by`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE UNIQUE INDEX `gonof_common_member_sanction__operator_uid` ON `gonof_common_member_sanction` (`operator_uid`, `request_key`);
CREATE INDEX `gonof_common_member_sanction__revoked_by` ON `gonof_common_member_sanction` (`revoked_by`);
CREATE INDEX `gonof_common_member_sanction__gonof_sanction_member` ON `gonof_common_member_sanction` (`uid`, `kind`, `revoked_at`, `id`);
CREATE INDEX `gonof_common_member_sanction__gonof_sanction_expiry` ON `gonof_common_member_sanction` (`kind`, `revoked_at`, `expires_at`, `id`);

CREATE TABLE `gonof_common_member_token` (
  `id` INTEGER PRIMARY KEY AUTOINCREMENT,
  `uid` INTEGER NOT NULL,
  `purpose` TEXT NOT NULL CHECK (length(`purpose`) <= 32),
  `token_hash` TEXT NOT NULL CHECK (length(`token_hash`) <= 64),
  `email` TEXT NOT NULL CHECK (length(`email`) <= 255),
  `dateline` INTEGER NOT NULL,
  `expires_at` INTEGER NOT NULL,
  `consumed_at` INTEGER DEFAULT NULL,
  `revoked_at` INTEGER DEFAULT NULL,
  CONSTRAINT `gonof_common_member_token_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE UNIQUE INDEX `gonof_common_member_token__token_hash` ON `gonof_common_member_token` (`token_hash`);
CREATE INDEX `gonof_common_member_token__gonof_member_token_active` ON `gonof_common_member_token` (`uid`, `purpose`, `consumed_at`, `revoked_at`);

CREATE TABLE `gonof_common_member_two_factor` (
  `uid` INTEGER NOT NULL,
  `totp_secret` TEXT DEFAULT NULL CHECK (length(`totp_secret`) <= 255),
  `totp_step` INTEGER NOT NULL DEFAULT 0,
  `totp_enabled_at` INTEGER NOT NULL DEFAULT 0,
  `failed_attempts` INTEGER NOT NULL DEFAULT 0,
  `locked_until` INTEGER NOT NULL DEFAULT 0,
  `enabled_at` INTEGER NOT NULL,
  PRIMARY KEY (`uid`),
  CONSTRAINT `gonof_common_member_two_factor_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;

CREATE TABLE `gonof_common_member_username_history` (
  `username` TEXT NOT NULL CHECK (length(`username`) <= 50),
  `uid` INTEGER NOT NULL,
  `dateline` INTEGER NOT NULL,
  `expires_at` INTEGER NOT NULL,
  PRIMARY KEY (`username`),
  CONSTRAINT `gonof_common_member_username_history_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE INDEX `gonof_common_member_username_history__gonof_username_history_member` ON `gonof_common_member_username_history` (`uid`, `dateline`);

CREATE TABLE `gonof_common_member_verification` (
  `uid` INTEGER NOT NULL,
  `category` TEXT NOT NULL CHECK (length(`category`) <= 16),
  `title` TEXT NOT NULL CHECK (length(`title`) <= 60),
  `site_url` TEXT DEFAULT NULL CHECK (length(`site_url`) <= 500),
  `granted_by` INTEGER NOT NULL,
  `granted_at` INTEGER NOT NULL,
  `reviewed_at` INTEGER NOT NULL,
  `expires_at` INTEGER DEFAULT NULL,
  PRIMARY KEY (`uid`),
  CONSTRAINT `gonof_common_member_verification_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_common_member_verification_ibfk_2` FOREIGN KEY (`granted_by`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE INDEX `gonof_common_member_verification__gonof_verification_directory` ON `gonof_common_member_verification` (`category`, `granted_at`, `uid`);
CREATE INDEX `gonof_common_member_verification__gonof_verification_expiry` ON `gonof_common_member_verification` (`expires_at`);
CREATE INDEX `gonof_common_member_verification__granted_by` ON `gonof_common_member_verification` (`granted_by`);

CREATE TABLE `gonof_common_outbox` (
  `eventid` INTEGER PRIMARY KEY AUTOINCREMENT,
  `event_key` TEXT NOT NULL CHECK (length(`event_key`) <= 160),
  `event_type` TEXT NOT NULL CHECK (length(`event_type`) <= 80),
  `aggregate_type` TEXT NOT NULL CHECK (length(`aggregate_type`) <= 40),
  `aggregate_id` INTEGER NOT NULL,
  `payload` TEXT NOT NULL CHECK (length(CAST(`payload` AS BLOB)) <= 65535),
  `dateline` INTEGER NOT NULL,
  `available_at` INTEGER NOT NULL,
  `attempts` INTEGER NOT NULL DEFAULT 0,
  `lease_token` TEXT DEFAULT NULL CHECK (length(`lease_token`) <= 64),
  `lease_until` INTEGER NOT NULL DEFAULT 0,
  `status` TEXT NOT NULL DEFAULT 'pending' CHECK (length(`status`) <= 16),
  `last_error` TEXT DEFAULT NULL CHECK (length(`last_error`) <= 1000)
) STRICT;
CREATE UNIQUE INDEX `gonof_common_outbox__event_key` ON `gonof_common_outbox` (`event_key`);
CREATE INDEX `gonof_common_outbox__gonof_outbox_delivery` ON `gonof_common_outbox` (`status`, `available_at`, `lease_until`, `eventid`);

CREATE TABLE `gonof_common_report` (
  `id` INTEGER PRIMARY KEY AUTOINCREMENT,
  `uid` INTEGER NOT NULL,
  `target_type` TEXT NOT NULL CHECK (length(`target_type`) <= 16),
  `target_id` INTEGER NOT NULL,
  `reason` TEXT NOT NULL CHECK (length(`reason`) <= 1000),
  `status` TEXT NOT NULL DEFAULT 'pending' CHECK (length(`status`) <= 16),
  `handler_uid` INTEGER DEFAULT NULL,
  `resolution` TEXT NOT NULL DEFAULT '' CHECK (length(`resolution`) <= 1000),
  `dateline` INTEGER NOT NULL,
  `handled_at` INTEGER NOT NULL DEFAULT 0,
  `request_key` TEXT NOT NULL CHECK (length(`request_key`) <= 64),
  CONSTRAINT `gonof_common_report_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_common_report_ibfk_2` FOREIGN KEY (`handler_uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE UNIQUE INDEX `gonof_common_report__uid` ON `gonof_common_report` (`uid`, `request_key`);
CREATE INDEX `gonof_common_report__handler_uid` ON `gonof_common_report` (`handler_uid`);
CREATE INDEX `gonof_common_report__gonof_report_queue` ON `gonof_common_report` (`status`, `id`);
CREATE INDEX `gonof_common_report__gonof_report_owner` ON `gonof_common_report` (`uid`, `id`);
CREATE INDEX `gonof_common_report__gonof_report_target` ON `gonof_common_report` (`target_type`, `target_id`, `id`);

CREATE TABLE `gonof_common_session` (
  `sid` TEXT NOT NULL CHECK (length(`sid`) <= 64),
  `uid` INTEGER NOT NULL,
  `lastactivity` INTEGER NOT NULL,
  `client_ip` TEXT NOT NULL DEFAULT '' CHECK (length(`client_ip`) <= 45),
  PRIMARY KEY (`sid`)
) STRICT;
CREATE INDEX `gonof_common_session__gonof_session_member` ON `gonof_common_session` (`uid`);
CREATE INDEX `gonof_common_session__gonof_session_activity` ON `gonof_common_session` (`lastactivity`);

CREATE TABLE `gonof_common_settings` (
  `skey` TEXT NOT NULL CHECK (length(`skey`) <= 190),
  `svalue` TEXT NOT NULL CHECK (length(CAST(`svalue` AS BLOB)) <= 65535),
  PRIMARY KEY (`skey`)
) STRICT;

CREATE TABLE `gonof_common_site_asset` (
  `slot` TEXT NOT NULL CHECK (length(`slot`) <= 24),
  `aid` INTEGER DEFAULT NULL,
  `version` INTEGER NOT NULL DEFAULT 1,
  `dateline` INTEGER NOT NULL,
  PRIMARY KEY (`slot`),
  CONSTRAINT `gonof_common_site_asset_ibfk_1` FOREIGN KEY (`aid`) REFERENCES `gonof_forum_attachment` (`aid`)
) STRICT;
CREATE INDEX `gonof_common_site_asset__aid` ON `gonof_common_site_asset` (`aid`);

CREATE TABLE `gonof_common_site_domain` (
  `domain_id` INTEGER PRIMARY KEY AUTOINCREMENT,
  `site_url` TEXT NOT NULL CHECK (length(`site_url`) <= 255),
  `site_name` TEXT DEFAULT NULL CHECK (length(`site_name`) <= 80),
  `canonical` TEXT NOT NULL DEFAULT 'self' CHECK (length(`canonical`) <= 8),
  `logo_aid` INTEGER DEFAULT NULL,
  `favicon_aid` INTEGER DEFAULT NULL,
  `verification` TEXT CHECK (length(CAST(`verification` AS BLOB)) <= 65535),
  `login_methods` TEXT DEFAULT NULL CHECK (length(`login_methods`) <= 64),
  `participation` TEXT DEFAULT NULL CHECK (length(`participation`) <= 64),
  `status` INTEGER NOT NULL DEFAULT 1,
  `version` INTEGER NOT NULL DEFAULT 1,
  `dateline` INTEGER NOT NULL,
  `language` TEXT DEFAULT NULL CHECK (length(`language`) <= 16),
  CONSTRAINT `gonof_site_domain_favicon_fk` FOREIGN KEY (`favicon_aid`) REFERENCES `gonof_forum_attachment` (`aid`),
  CONSTRAINT `gonof_site_domain_logo_fk` FOREIGN KEY (`logo_aid`) REFERENCES `gonof_forum_attachment` (`aid`)
) STRICT;
CREATE UNIQUE INDEX `gonof_common_site_domain__site_url` ON `gonof_common_site_domain` (`site_url`);
CREATE INDEX `gonof_common_site_domain__logo_aid` ON `gonof_common_site_domain` (`logo_aid`);
CREATE INDEX `gonof_common_site_domain__favicon_aid` ON `gonof_common_site_domain` (`favicon_aid`);

CREATE TABLE `gonof_common_site_domain_forum` (
  `domain_id` INTEGER NOT NULL,
  `fid` INTEGER NOT NULL,
  PRIMARY KEY (`domain_id`, `fid`),
  CONSTRAINT `gonof_site_domain_forum_domain_fk` FOREIGN KEY (`domain_id`) REFERENCES `gonof_common_site_domain` (`domain_id`),
  CONSTRAINT `gonof_site_domain_forum_fid_fk` FOREIGN KEY (`fid`) REFERENCES `gonof_forum_forum` (`fid`)
) STRICT;
CREATE INDEX `gonof_common_site_domain_forum__fid` ON `gonof_common_site_domain_forum` (`fid`);

CREATE TABLE `gonof_common_sms_code` (
  `id` INTEGER PRIMARY KEY AUTOINCREMENT,
  `mobile` TEXT NOT NULL CHECK (length(`mobile`) <= 16),
  `purpose` TEXT NOT NULL CHECK (length(`purpose`) <= 16),
  `code_hash` TEXT NOT NULL CHECK (length(`code_hash`) <= 64),
  `dateline` INTEGER NOT NULL,
  `expires_at` INTEGER NOT NULL,
  `attempts` INTEGER NOT NULL DEFAULT 0,
  `consumed_at` INTEGER DEFAULT NULL,
  `client_ip` TEXT NOT NULL DEFAULT '' CHECK (length(`client_ip`) <= 45)
) STRICT;
CREATE INDEX `gonof_common_sms_code__gonof_sms_code_mobile` ON `gonof_common_sms_code` (`mobile`, `dateline`);
CREATE INDEX `gonof_common_sms_code__gonof_sms_code_dateline` ON `gonof_common_sms_code` (`dateline`);

CREATE TABLE `gonof_forum_activity` (
  `tid` INTEGER NOT NULL,
  `owner_uid` INTEGER NOT NULL,
  `kind` INTEGER NOT NULL,
  `budget` INTEGER NOT NULL DEFAULT 0,
  `remaining` INTEGER NOT NULL DEFAULT 0,
  `deadline` INTEGER NOT NULL DEFAULT 0,
  `state` TEXT NOT NULL DEFAULT 'open' CHECK (length(`state`) <= 16),
  `created_at` INTEGER NOT NULL,
  `settled_at` INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (`tid`),
  CONSTRAINT `gonof_forum_activity_ibfk_1` FOREIGN KEY (`tid`) REFERENCES `gonof_forum_thread` (`tid`),
  CONSTRAINT `gonof_forum_activity_ibfk_2` FOREIGN KEY (`owner_uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE INDEX `gonof_forum_activity__owner_uid` ON `gonof_forum_activity` (`owner_uid`);
CREATE INDEX `gonof_forum_activity__gonof_activity_due` ON `gonof_forum_activity` (`state`, `deadline`, `tid`);

CREATE TABLE `gonof_forum_activity_draw` (
  `tid` INTEGER NOT NULL,
  `seed` TEXT NOT NULL CHECK (length(`seed`) <= 64),
  `eligible` TEXT NOT NULL,
  `winners` TEXT NOT NULL,
  `dateline` INTEGER NOT NULL,
  PRIMARY KEY (`tid`),
  CONSTRAINT `gonof_forum_activity_draw_ibfk_1` FOREIGN KEY (`tid`) REFERENCES `gonof_forum_lottery` (`tid`)
) STRICT;

CREATE TABLE `gonof_forum_attachment` (
  `aid` INTEGER PRIMARY KEY AUTOINCREMENT,
  `uid` INTEGER NOT NULL,
  `request_key` TEXT NOT NULL CHECK (length(`request_key`) <= 64),
  `request_hash` TEXT NOT NULL CHECK (length(`request_hash`) <= 64),
  CONSTRAINT `gonof_forum_attachment_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE UNIQUE INDEX `gonof_forum_attachment__uid` ON `gonof_forum_attachment` (`uid`, `request_key`);
CREATE INDEX `gonof_forum_attachment__gonof_attachment_owner` ON `gonof_forum_attachment` (`uid`, `aid`);

CREATE TABLE `gonof_forum_attachment_0` (
  `aid` INTEGER NOT NULL,
  `filename` TEXT NOT NULL CHECK (length(`filename`) <= 255),
  `filesize` INTEGER NOT NULL,
  `mime_type` TEXT NOT NULL CHECK (length(`mime_type`) <= 127),
  `content_sha256` TEXT NOT NULL CHECK (length(`content_sha256`) <= 64),
  `attachment` TEXT NOT NULL CHECK (length(`attachment`) <= 1024),
  `storage_profile_id` TEXT NOT NULL CHECK (length(`storage_profile_id`) <= 64),
  `width` INTEGER DEFAULT NULL,
  `height` INTEGER DEFAULT NULL,
  `description` TEXT NOT NULL CHECK (length(CAST(`description` AS BLOB)) <= 65535),
  `alt_text` TEXT NOT NULL CHECK (length(CAST(`alt_text` AS BLOB)) <= 65535),
  `dateline` INTEGER NOT NULL,
  `updated_at` INTEGER NOT NULL,
  `checked_at` INTEGER DEFAULT NULL,
  `purge_after` INTEGER DEFAULT NULL,
  `lifecycle` TEXT NOT NULL DEFAULT 'pending' CHECK (length(`lifecycle`) <= 16),
  `scan_status` TEXT NOT NULL DEFAULT 'pending' CHECK (length(`scan_status`) <= 16),
  `scan_detail` TEXT NOT NULL CHECK (length(CAST(`scan_detail` AS BLOB)) <= 65535),
  `trashed_at` INTEGER DEFAULT NULL,
  PRIMARY KEY (`aid`),
  CONSTRAINT `gonof_forum_attachment_0_ibfk_1` FOREIGN KEY (`aid`) REFERENCES `gonof_forum_attachment` (`aid`)
) STRICT;
CREATE INDEX `gonof_forum_attachment_0__gonof_attachment_purge_due` ON `gonof_forum_attachment_0` (`lifecycle`, `purge_after`);

CREATE TABLE `gonof_forum_attachment_reference` (
  `aid` INTEGER NOT NULL,
  `pid` INTEGER NOT NULL,
  `dateline` INTEGER NOT NULL,
  PRIMARY KEY (`aid`, `pid`),
  CONSTRAINT `gonof_forum_attachment_reference_ibfk_1` FOREIGN KEY (`aid`) REFERENCES `gonof_forum_attachment` (`aid`),
  CONSTRAINT `gonof_forum_attachment_reference_ibfk_2` FOREIGN KEY (`pid`) REFERENCES `gonof_forum_post` (`pid`)
) STRICT;
CREATE INDEX `gonof_forum_attachment_reference__gonof_attachment_reference_post` ON `gonof_forum_attachment_reference` (`pid`, `aid`);

CREATE TABLE `gonof_forum_forum` (
  `fid` INTEGER PRIMARY KEY AUTOINCREMENT,
  `slug` TEXT NOT NULL CHECK (length(`slug`) <= 64),
  `name` TEXT NOT NULL CHECK (length(`name`) <= 50),
  `description` TEXT NOT NULL CHECK (length(CAST(`description` AS BLOB)) <= 65535),
  `guidance` TEXT NOT NULL CHECK (length(CAST(`guidance` AS BLOB)) <= 65535),
  `displayorder` INTEGER NOT NULL DEFAULT 0,
  `status` INTEGER NOT NULL DEFAULT 1,
  `visibility` TEXT NOT NULL DEFAULT 'public' CHECK (length(`visibility`) <= 24),
  `parent_fid` INTEGER DEFAULT NULL,
  `icon_aid` INTEGER DEFAULT NULL,
  `version` INTEGER NOT NULL DEFAULT 1,
  CONSTRAINT `gonof_forum_icon_fk` FOREIGN KEY (`icon_aid`) REFERENCES `gonof_forum_attachment` (`aid`),
  CONSTRAINT `gonof_forum_parent_fk` FOREIGN KEY (`parent_fid`) REFERENCES `gonof_forum_forum` (`fid`)
) STRICT;
CREATE UNIQUE INDEX `gonof_forum_forum__slug` ON `gonof_forum_forum` (`slug`);
CREATE INDEX `gonof_forum_forum__gonof_forum_icon_fk` ON `gonof_forum_forum` (`icon_aid`);
CREATE INDEX `gonof_forum_forum__gonof_forum_parent_order` ON `gonof_forum_forum` (`parent_fid`, `displayorder`, `fid`);

CREATE TABLE `gonof_forum_lottery` (
  `tid` INTEGER NOT NULL,
  `shares` INTEGER NOT NULL,
  `entry_mode` TEXT NOT NULL DEFAULT 'reply' CHECK (length(`entry_mode`) <= 8),
  `requirements` TEXT CHECK (length(CAST(`requirements` AS BLOB)) <= 65535),
  `review` INTEGER NOT NULL DEFAULT 0,
  `early_draw` INTEGER NOT NULL DEFAULT 0,
  `planned_deadline` INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (`tid`),
  CONSTRAINT `gonof_forum_lottery_ibfk_1` FOREIGN KEY (`tid`) REFERENCES `gonof_forum_activity` (`tid`)
) STRICT;

CREATE TABLE `gonof_forum_lottery_award` (
  `tid` INTEGER NOT NULL,
  `prize_no` INTEGER NOT NULL,
  `slot_no` INTEGER NOT NULL,
  `code_ciphertext` TEXT DEFAULT NULL CHECK (length(`code_ciphertext`) <= 512),
  `uid` INTEGER DEFAULT NULL,
  `assigned_at` INTEGER NOT NULL DEFAULT 0,
  `revised_at` INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (`tid`, `prize_no`, `slot_no`),
  CONSTRAINT `gonof_forum_lottery_award_ibfk_1` FOREIGN KEY (`tid`, `prize_no`) REFERENCES `gonof_forum_lottery_prize` (`tid`, `prize_no`),
  CONSTRAINT `gonof_forum_lottery_award_ibfk_2` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE UNIQUE INDEX `gonof_forum_lottery_award__gonof_lottery_award_winner` ON `gonof_forum_lottery_award` (`tid`, `uid`);
CREATE INDEX `gonof_forum_lottery_award__gonof_lottery_award_member` ON `gonof_forum_lottery_award` (`uid`, `tid`);

CREATE TABLE `gonof_forum_lottery_entry` (
  `tid` INTEGER NOT NULL,
  `uid` INTEGER NOT NULL,
  `source_pid` INTEGER NOT NULL,
  `amount` INTEGER NOT NULL DEFAULT 0,
  `dateline` INTEGER NOT NULL,
  PRIMARY KEY (`tid`, `uid`),
  CONSTRAINT `gonof_forum_lottery_entry_ibfk_1` FOREIGN KEY (`tid`) REFERENCES `gonof_forum_lottery` (`tid`),
  CONSTRAINT `gonof_forum_lottery_entry_ibfk_2` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_forum_lottery_entry_ibfk_3` FOREIGN KEY (`source_pid`) REFERENCES `gonof_forum_post` (`pid`)
) STRICT;
CREATE INDEX `gonof_forum_lottery_entry__uid` ON `gonof_forum_lottery_entry` (`uid`);
CREATE INDEX `gonof_forum_lottery_entry__source_pid` ON `gonof_forum_lottery_entry` (`source_pid`);

CREATE TABLE `gonof_forum_lottery_exclusion` (
  `tid` INTEGER NOT NULL,
  `uid` INTEGER NOT NULL,
  `reason` TEXT NOT NULL CHECK (length(`reason`) <= 200),
  `dateline` INTEGER NOT NULL,
  PRIMARY KEY (`tid`, `uid`),
  CONSTRAINT `gonof_forum_lottery_exclusion_ibfk_1` FOREIGN KEY (`tid`) REFERENCES `gonof_forum_lottery` (`tid`),
  CONSTRAINT `gonof_forum_lottery_exclusion_ibfk_2` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE INDEX `gonof_forum_lottery_exclusion__gonof_lottery_exclusion_member` ON `gonof_forum_lottery_exclusion` (`uid`, `tid`);

CREATE TABLE `gonof_forum_lottery_prize` (
  `tid` INTEGER NOT NULL,
  `prize_no` INTEGER NOT NULL,
  `name` TEXT NOT NULL CHECK (length(`name`) <= 60),
  `winners` INTEGER NOT NULL,
  `kind` TEXT NOT NULL CHECK (length(`kind`) <= 8),
  `amount` INTEGER NOT NULL DEFAULT 0,
  `note` TEXT NOT NULL DEFAULT '' CHECK (length(`note`) <= 200),
  PRIMARY KEY (`tid`, `prize_no`),
  CONSTRAINT `gonof_forum_lottery_prize_ibfk_1` FOREIGN KEY (`tid`) REFERENCES `gonof_forum_lottery` (`tid`)
) STRICT;

CREATE TABLE `gonof_forum_lottery_signup` (
  `tid` INTEGER NOT NULL,
  `uid` INTEGER NOT NULL,
  `amount` INTEGER NOT NULL DEFAULT 0,
  `dateline` INTEGER NOT NULL,
  PRIMARY KEY (`tid`, `uid`),
  CONSTRAINT `gonof_forum_lottery_signup_ibfk_1` FOREIGN KEY (`tid`) REFERENCES `gonof_forum_lottery` (`tid`),
  CONSTRAINT `gonof_forum_lottery_signup_ibfk_2` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE INDEX `gonof_forum_lottery_signup__gonof_lottery_signup_member` ON `gonof_forum_lottery_signup` (`uid`, `tid`);

CREATE TABLE `gonof_forum_moderator` (
  `uid` INTEGER NOT NULL,
  `fid` INTEGER NOT NULL,
  PRIMARY KEY (`uid`, `fid`),
  CONSTRAINT `gonof_forum_moderator_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_forum_moderator_ibfk_2` FOREIGN KEY (`fid`) REFERENCES `gonof_forum_forum` (`fid`)
) STRICT;
CREATE INDEX `gonof_forum_moderator__fid` ON `gonof_forum_moderator` (`fid`);

CREATE TABLE `gonof_forum_news` (
  `tid` INTEGER NOT NULL,
  `source_url` TEXT NOT NULL CHECK (length(`source_url`) <= 2048),
  `source_name` TEXT NOT NULL CHECK (length(`source_name`) <= 255),
  `source_author` TEXT NOT NULL CHECK (length(`source_author`) <= 255),
  PRIMARY KEY (`tid`),
  CONSTRAINT `gonof_forum_news_ibfk_1` FOREIGN KEY (`tid`) REFERENCES `gonof_forum_activity` (`tid`)
) STRICT;

CREATE TABLE `gonof_forum_poll` (
  `tid` INTEGER NOT NULL,
  `max_choices` INTEGER NOT NULL DEFAULT 1,
  `voters` INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (`tid`),
  CONSTRAINT `gonof_forum_poll_ibfk_1` FOREIGN KEY (`tid`) REFERENCES `gonof_forum_activity` (`tid`)
) STRICT;

CREATE TABLE `gonof_forum_poll_option` (
  `tid` INTEGER NOT NULL,
  `option_no` INTEGER NOT NULL,
  `label` TEXT NOT NULL CHECK (length(`label`) <= 255),
  `votes` INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (`tid`, `option_no`),
  CONSTRAINT `gonof_forum_poll_option_ibfk_1` FOREIGN KEY (`tid`) REFERENCES `gonof_forum_poll` (`tid`)
) STRICT;

CREATE TABLE `gonof_forum_poll_vote` (
  `tid` INTEGER NOT NULL,
  `uid` INTEGER NOT NULL,
  `choices` TEXT NOT NULL CHECK (length(CAST(`choices` AS BLOB)) <= 65535),
  `request_key` TEXT NOT NULL CHECK (length(`request_key`) <= 64),
  `dateline` INTEGER NOT NULL,
  PRIMARY KEY (`tid`, `uid`),
  CONSTRAINT `gonof_forum_poll_vote_ibfk_1` FOREIGN KEY (`tid`) REFERENCES `gonof_forum_poll` (`tid`),
  CONSTRAINT `gonof_forum_poll_vote_ibfk_2` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE INDEX `gonof_forum_poll_vote__uid` ON `gonof_forum_poll_vote` (`uid`);

CREATE TABLE `gonof_forum_post` (
  `pid` INTEGER NOT NULL,
  `fid` INTEGER NOT NULL,
  `tid` INTEGER NOT NULL,
  `authorid` INTEGER DEFAULT NULL,
  `first` INTEGER NOT NULL DEFAULT 0,
  `position` INTEGER NOT NULL,
  `root_pid` INTEGER DEFAULT NULL,
  `reply_to_pid` INTEGER DEFAULT NULL,
  `message` TEXT NOT NULL,
  `dateline` INTEGER NOT NULL DEFAULT 0,
  `invisible` INTEGER NOT NULL DEFAULT 0,
  `root_invisible` INTEGER NOT NULL DEFAULT 0,
  `status` INTEGER NOT NULL DEFAULT 0,
  `sub_replies_count` INTEGER NOT NULL DEFAULT 0,
  `reactions` INTEGER NOT NULL DEFAULT 0,
  `downvotes` INTEGER NOT NULL DEFAULT 0,
  `deleted_at` INTEGER NOT NULL DEFAULT 0,
  `version` INTEGER NOT NULL DEFAULT 1,
  `edited_at` INTEGER NOT NULL DEFAULT 0,
  `root_position` INTEGER DEFAULT NULL,
  `sub_position` INTEGER DEFAULT NULL,
  `erased_at` INTEGER NOT NULL DEFAULT 0,
  `first_tid` INTEGER DEFAULT NULL,
  PRIMARY KEY (`tid`, `position`),
  CONSTRAINT `gonof_forum_post_ibfk_1` FOREIGN KEY (`pid`) REFERENCES `gonof_forum_post_tableid` (`pid`),
  CONSTRAINT `gonof_forum_post_ibfk_3` FOREIGN KEY (`authorid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_post_reply_thread_fk` FOREIGN KEY (`reply_to_pid`, `tid`) REFERENCES `gonof_forum_post` (`pid`, `tid`),
  CONSTRAINT `gonof_post_root_thread_fk` FOREIGN KEY (`root_pid`, `tid`) REFERENCES `gonof_forum_post` (`pid`, `tid`),
  CONSTRAINT `gonof_post_thread_forum_fk` FOREIGN KEY (`tid`, `fid`) REFERENCES `gonof_forum_thread` (`tid`, `fid`) ON UPDATE CASCADE
) STRICT;
CREATE UNIQUE INDEX `gonof_forum_post__pid` ON `gonof_forum_post` (`pid`, `tid`, `fid`);
CREATE UNIQUE INDEX `gonof_forum_post__pid_2` ON `gonof_forum_post` (`pid`);
CREATE UNIQUE INDEX `gonof_forum_post__gonof_post_pid_thread` ON `gonof_forum_post` (`pid`, `tid`);
CREATE UNIQUE INDEX `gonof_forum_post__gonof_post_root_position` ON `gonof_forum_post` (`tid`, `root_position`);
CREATE UNIQUE INDEX `gonof_forum_post__gonof_post_sub_position` ON `gonof_forum_post` (`root_pid`, `sub_position`);
CREATE UNIQUE INDEX `gonof_forum_post__first_tid` ON `gonof_forum_post` (`first_tid`);
CREATE INDEX `gonof_forum_post__root_pid` ON `gonof_forum_post` (`root_pid`, `tid`, `fid`);
CREATE INDEX `gonof_forum_post__reply_to_pid` ON `gonof_forum_post` (`reply_to_pid`, `tid`, `fid`);
CREATE INDEX `gonof_forum_post__gonof_post_thread_root` ON `gonof_forum_post` (`tid`, `root_pid`, `position`);
CREATE INDEX `gonof_forum_post__gonof_post_author_created` ON `gonof_forum_post` (`authorid`, `first`, `dateline`);
CREATE INDEX `gonof_forum_post__gonof_post_thread_forum_fk` ON `gonof_forum_post` (`tid`, `fid`);
CREATE INDEX `gonof_forum_post__gonof_post_dateline` ON `gonof_forum_post` (`dateline`);

CREATE TABLE `gonof_forum_post_downvote` (
  `pid` INTEGER NOT NULL,
  `uid` INTEGER NOT NULL,
  `dateline` INTEGER NOT NULL,
  PRIMARY KEY (`pid`, `uid`),
  CONSTRAINT `gonof_forum_post_downvote_ibfk_1` FOREIGN KEY (`pid`) REFERENCES `gonof_forum_post` (`pid`),
  CONSTRAINT `gonof_forum_post_downvote_ibfk_2` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE INDEX `gonof_forum_post_downvote__gonof_post_downvote_member` ON `gonof_forum_post_downvote` (`uid`, `pid`);

CREATE TABLE `gonof_forum_post_reaction` (
  `pid` INTEGER NOT NULL,
  `uid` INTEGER NOT NULL,
  `dateline` INTEGER NOT NULL,
  PRIMARY KEY (`pid`, `uid`),
  CONSTRAINT `gonof_forum_post_reaction_ibfk_1` FOREIGN KEY (`pid`) REFERENCES `gonof_forum_post` (`pid`),
  CONSTRAINT `gonof_forum_post_reaction_ibfk_2` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE INDEX `gonof_forum_post_reaction__uid` ON `gonof_forum_post_reaction` (`uid`);
CREATE INDEX `gonof_forum_post_reaction__gonof_reaction_dateline` ON `gonof_forum_post_reaction` (`dateline`);

CREATE TABLE `gonof_forum_post_request` (
  `uid` INTEGER NOT NULL,
  `request_key` TEXT NOT NULL CHECK (length(`request_key`) <= 64),
  `request_hash` TEXT NOT NULL CHECK (length(`request_hash`) <= 64),
  `pid` INTEGER NOT NULL,
  `dateline` INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (`uid`, `request_key`),
  CONSTRAINT `gonof_forum_post_request_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_forum_post_request_ibfk_2` FOREIGN KEY (`pid`) REFERENCES `gonof_forum_post` (`pid`)
) STRICT;
CREATE INDEX `gonof_forum_post_request__pid` ON `gonof_forum_post_request` (`pid`);
CREATE INDEX `gonof_forum_post_request__gonof_post_request_dateline` ON `gonof_forum_post_request` (`dateline`);

CREATE TABLE `gonof_forum_post_revision` (
  `pid` INTEGER NOT NULL,
  `version` INTEGER NOT NULL,
  `editor_uid` INTEGER NOT NULL,
  `subject` TEXT DEFAULT NULL CHECK (length(`subject`) <= 255),
  `message` TEXT NOT NULL CHECK (length(CAST(`message` AS BLOB)) <= 65535),
  `dateline` INTEGER NOT NULL,
  PRIMARY KEY (`pid`, `version`),
  CONSTRAINT `gonof_forum_post_revision_ibfk_1` FOREIGN KEY (`pid`) REFERENCES `gonof_forum_post` (`pid`),
  CONSTRAINT `gonof_forum_post_revision_ibfk_2` FOREIGN KEY (`editor_uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE INDEX `gonof_forum_post_revision__editor_uid` ON `gonof_forum_post_revision` (`editor_uid`);

CREATE TABLE `gonof_forum_post_tableid` (
  `pid` INTEGER PRIMARY KEY AUTOINCREMENT
) STRICT;

CREATE TABLE `gonof_forum_redpacket` (
  `tid` INTEGER NOT NULL,
  `shares` INTEGER NOT NULL,
  `remaining_shares` INTEGER NOT NULL,
  `distribution` TEXT NOT NULL DEFAULT 'equal' CHECK (length(`distribution`) <= 12),
  PRIMARY KEY (`tid`),
  CONSTRAINT `gonof_forum_redpacket_ibfk_1` FOREIGN KEY (`tid`) REFERENCES `gonof_forum_activity` (`tid`)
) STRICT;

CREATE TABLE `gonof_forum_redpacket_claim` (
  `tid` INTEGER NOT NULL,
  `uid` INTEGER NOT NULL,
  `source_pid` INTEGER NOT NULL,
  `amount` INTEGER NOT NULL,
  `dateline` INTEGER NOT NULL,
  PRIMARY KEY (`tid`, `uid`),
  CONSTRAINT `gonof_forum_redpacket_claim_ibfk_1` FOREIGN KEY (`tid`) REFERENCES `gonof_forum_redpacket` (`tid`),
  CONSTRAINT `gonof_forum_redpacket_claim_ibfk_2` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_forum_redpacket_claim_ibfk_3` FOREIGN KEY (`source_pid`) REFERENCES `gonof_forum_post` (`pid`)
) STRICT;
CREATE INDEX `gonof_forum_redpacket_claim__uid` ON `gonof_forum_redpacket_claim` (`uid`);
CREATE INDEX `gonof_forum_redpacket_claim__source_pid` ON `gonof_forum_redpacket_claim` (`source_pid`);

CREATE TABLE `gonof_forum_restore_request` (
  `id` INTEGER PRIMARY KEY AUTOINCREMENT,
  `uid` INTEGER NOT NULL,
  `pid` INTEGER NOT NULL,
  `state` TEXT NOT NULL CHECK (length(`state`) <= 16),
  `hidden_by` INTEGER DEFAULT NULL,
  `reason` TEXT NOT NULL CHECK (length(`reason`) <= 1000),
  `status` TEXT NOT NULL DEFAULT 'pending' CHECK (length(`status`) <= 16),
  `handler_uid` INTEGER DEFAULT NULL,
  `resolution` TEXT NOT NULL DEFAULT '' CHECK (length(`resolution`) <= 500),
  `handled_at` INTEGER NOT NULL DEFAULT 0,
  `dateline` INTEGER NOT NULL,
  `request_key` TEXT NOT NULL CHECK (length(`request_key`) <= 64),
  CONSTRAINT `gonof_forum_restore_request_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_forum_restore_request_ibfk_2` FOREIGN KEY (`pid`) REFERENCES `gonof_forum_post` (`pid`),
  CONSTRAINT `gonof_forum_restore_request_ibfk_3` FOREIGN KEY (`hidden_by`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_forum_restore_request_ibfk_4` FOREIGN KEY (`handler_uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE UNIQUE INDEX `gonof_forum_restore_request__gonof_restore_request_key` ON `gonof_forum_restore_request` (`uid`, `request_key`);
CREATE INDEX `gonof_forum_restore_request__gonof_restore_owner` ON `gonof_forum_restore_request` (`uid`, `dateline`, `id`);
CREATE INDEX `gonof_forum_restore_request__gonof_restore_target` ON `gonof_forum_restore_request` (`pid`, `status`, `id`);
CREATE INDEX `gonof_forum_restore_request__gonof_restore_queue` ON `gonof_forum_restore_request` (`status`, `id`);
CREATE INDEX `gonof_forum_restore_request__hidden_by` ON `gonof_forum_restore_request` (`hidden_by`);
CREATE INDEX `gonof_forum_restore_request__handler_uid` ON `gonof_forum_restore_request` (`handler_uid`);

CREATE TABLE `gonof_forum_reward` (
  `tid` INTEGER NOT NULL,
  `accepted_pid` INTEGER DEFAULT NULL,
  PRIMARY KEY (`tid`),
  CONSTRAINT `gonof_forum_reward_ibfk_1` FOREIGN KEY (`tid`) REFERENCES `gonof_forum_activity` (`tid`),
  CONSTRAINT `gonof_forum_reward_ibfk_2` FOREIGN KEY (`accepted_pid`) REFERENCES `gonof_forum_post` (`pid`)
) STRICT;
CREATE INDEX `gonof_forum_reward__accepted_pid` ON `gonof_forum_reward` (`accepted_pid`);

CREATE TABLE `gonof_forum_thread` (
  `tid` INTEGER PRIMARY KEY AUTOINCREMENT,
  `fid` INTEGER NOT NULL,
  `authorid` INTEGER DEFAULT NULL,
  `subject` TEXT NOT NULL CHECK (length(`subject`) <= 255),
  `dateline` INTEGER NOT NULL DEFAULT 0,
  `lastpost` INTEGER NOT NULL DEFAULT 0,
  `views` INTEGER NOT NULL DEFAULT 0,
  `favorites` INTEGER NOT NULL DEFAULT 0,
  `tips` INTEGER NOT NULL DEFAULT 0,
  `tip_total` INTEGER NOT NULL DEFAULT 0,
  `displayorder` INTEGER NOT NULL DEFAULT 0,
  `digest` INTEGER NOT NULL DEFAULT 0,
  `closed` INTEGER NOT NULL DEFAULT 0,
  `readperm` INTEGER NOT NULL DEFAULT 0,
  `status` INTEGER NOT NULL DEFAULT 0,
  `replies` INTEGER NOT NULL DEFAULT 0,
  `maxposition` INTEGER NOT NULL DEFAULT 1,
  `visibility` TEXT NOT NULL DEFAULT 'public' CHECK (length(`visibility`) <= 24),
  `publication_state` TEXT NOT NULL DEFAULT 'published' CHECK (length(`publication_state`) <= 24),
  `deleted_at` INTEGER NOT NULL DEFAULT 0,
  `version` INTEGER NOT NULL DEFAULT 1,
  `category` INTEGER NOT NULL DEFAULT 0,
  `root_replies` INTEGER NOT NULL DEFAULT 0,
  `valid_replies` INTEGER NOT NULL DEFAULT 0,
  `last_reply_pid` INTEGER DEFAULT NULL,
  `last_reply_author` TEXT NOT NULL DEFAULT '' CHECK (length(`last_reply_author`) <= 50),
  `last_reply_summary` TEXT NOT NULL DEFAULT '' CHECK (length(`last_reply_summary`) <= 255),
  `author` TEXT NOT NULL DEFAULT '' CHECK (length(`author`) <= 50),
  `family_fid` INTEGER NOT NULL,
  `cache_revision` INTEGER NOT NULL DEFAULT 1,
  CONSTRAINT `gonof_forum_thread_ibfk_1` FOREIGN KEY (`fid`) REFERENCES `gonof_forum_forum` (`fid`),
  CONSTRAINT `gonof_forum_thread_ibfk_2` FOREIGN KEY (`authorid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_thread_last_reply_fk` FOREIGN KEY (`last_reply_pid`) REFERENCES `gonof_forum_post` (`pid`)
) STRICT;
CREATE UNIQUE INDEX `gonof_forum_thread__tid` ON `gonof_forum_thread` (`tid`, `fid`);
CREATE INDEX `gonof_forum_thread__authorid` ON `gonof_forum_thread` (`authorid`);
CREATE INDEX `gonof_forum_thread__gonof_thread_last_reply_fk` ON `gonof_forum_thread` (`last_reply_pid`);
CREATE INDEX `gonof_forum_thread__gonof_thread_global_latest` ON `gonof_forum_thread` (`displayorder`, `lastpost`, `tid`, `visibility`, `fid`, `authorid`);
CREATE INDEX `gonof_forum_thread__gonof_thread_global_created` ON `gonof_forum_thread` (`displayorder`, `dateline`, `tid`, `visibility`, `fid`, `authorid`);
CREATE INDEX `gonof_forum_thread__gonof_thread_family_latest` ON `gonof_forum_thread` (`family_fid`, `displayorder`, `lastpost`, `tid`, `visibility`, `fid`, `authorid`);
CREATE INDEX `gonof_forum_thread__gonof_thread_family_created` ON `gonof_forum_thread` (`family_fid`, `displayorder`, `dateline`, `tid`, `visibility`, `fid`, `authorid`);
CREATE INDEX `gonof_forum_thread__gonof_thread_forum_latest` ON `gonof_forum_thread` (`fid`, `displayorder`, `lastpost`, `tid`, `visibility`, `authorid`);
CREATE INDEX `gonof_forum_thread__gonof_thread_forum_created` ON `gonof_forum_thread` (`fid`, `displayorder`, `dateline`, `tid`, `visibility`, `authorid`);
CREATE INDEX `gonof_forum_thread__gonof_thread_category_latest` ON `gonof_forum_thread` (`category`, `displayorder`, `lastpost`, `tid`, `visibility`, `fid`, `authorid`);
CREATE INDEX `gonof_forum_thread__gonof_thread_category_created` ON `gonof_forum_thread` (`category`, `displayorder`, `dateline`, `tid`, `visibility`, `fid`, `authorid`);
CREATE INDEX `gonof_forum_thread__gonof_thread_family_category_latest` ON `gonof_forum_thread` (`family_fid`, `category`, `displayorder`, `lastpost`, `tid`, `visibility`, `fid`, `authorid`);
CREATE INDEX `gonof_forum_thread__gonof_thread_family_category_created` ON `gonof_forum_thread` (`family_fid`, `category`, `displayorder`, `dateline`, `tid`, `visibility`, `fid`, `authorid`);
CREATE INDEX `gonof_forum_thread__gonof_thread_forum_category_latest` ON `gonof_forum_thread` (`fid`, `category`, `displayorder`, `lastpost`, `tid`, `visibility`, `authorid`);
CREATE INDEX `gonof_forum_thread__gonof_thread_forum_category_created` ON `gonof_forum_thread` (`fid`, `category`, `displayorder`, `dateline`, `tid`, `visibility`, `authorid`);
CREATE INDEX `gonof_forum_thread__gonof_thread_featured` ON `gonof_forum_thread` (`digest`, `displayorder`, `lastpost`, `tid`, `visibility`, `family_fid`, `fid`, `authorid`);
CREATE INDEX `gonof_forum_thread__gonof_thread_global_active` ON `gonof_forum_thread` (`lastpost`, `displayorder`, `replies`, `tid`, `visibility`, `family_fid`, `fid`, `authorid`);

CREATE TABLE `gonof_forum_thread_count` (
  `fid` INTEGER NOT NULL,
  `visibility` TEXT NOT NULL CHECK (length(`visibility`) <= 24),
  `category` INTEGER NOT NULL,
  `threads` INTEGER NOT NULL,
  PRIMARY KEY (`fid`, `visibility`, `category`),
  CONSTRAINT `gonof_forum_thread_count_ibfk_1` FOREIGN KEY (`fid`) REFERENCES `gonof_forum_forum` (`fid`)
) STRICT;

CREATE TABLE `gonof_forum_thread_image` (
  `tid` INTEGER NOT NULL,
  `position` INTEGER NOT NULL,
  `aid` INTEGER NOT NULL,
  PRIMARY KEY (`tid`, `position`),
  CONSTRAINT `gonof_forum_thread_image_ibfk_1` FOREIGN KEY (`tid`) REFERENCES `gonof_forum_thread` (`tid`),
  CONSTRAINT `gonof_forum_thread_image_ibfk_2` FOREIGN KEY (`aid`) REFERENCES `gonof_forum_attachment` (`aid`)
) STRICT;
CREATE INDEX `gonof_forum_thread_image__gonof_thread_image_attachment` ON `gonof_forum_thread_image` (`aid`);

CREATE TABLE `gonof_home_favorite` (
  `uid` INTEGER NOT NULL,
  `tid` INTEGER NOT NULL,
  `dateline` INTEGER NOT NULL,
  PRIMARY KEY (`uid`, `tid`),
  CONSTRAINT `gonof_home_favorite_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_home_favorite_ibfk_2` FOREIGN KEY (`tid`) REFERENCES `gonof_forum_thread` (`tid`)
) STRICT;
CREATE INDEX `gonof_home_favorite__tid` ON `gonof_home_favorite` (`tid`);
CREATE INDEX `gonof_home_favorite__gonof_favorite_list` ON `gonof_home_favorite` (`uid`, `dateline`, `tid`);

CREATE TABLE `gonof_home_feed_token` (
  `uid` INTEGER NOT NULL,
  `token_hash` TEXT NOT NULL CHECK (length(`token_hash`) <= 64),
  `token_hint` TEXT NOT NULL CHECK (length(`token_hint`) <= 8),
  `include_members` INTEGER NOT NULL DEFAULT 0,
  `dateline` INTEGER NOT NULL,
  PRIMARY KEY (`uid`),
  CONSTRAINT `gonof_home_feed_token_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE UNIQUE INDEX `gonof_home_feed_token__gonof_feed_token_hash` ON `gonof_home_feed_token` (`token_hash`);

CREATE TABLE `gonof_home_forum_filter` (
  `uid` INTEGER NOT NULL,
  `fid` INTEGER NOT NULL,
  `filter_mode` TEXT NOT NULL CHECK (length(`filter_mode`) <= 8),
  `dateline` INTEGER NOT NULL,
  PRIMARY KEY (`uid`, `fid`),
  CONSTRAINT `gonof_home_forum_filter_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_home_forum_filter_ibfk_2` FOREIGN KEY (`fid`) REFERENCES `gonof_forum_forum` (`fid`)
) STRICT;
CREATE INDEX `gonof_home_forum_filter__fid` ON `gonof_home_forum_filter` (`fid`);

CREATE TABLE `gonof_home_forum_follow` (
  `id` INTEGER PRIMARY KEY AUTOINCREMENT,
  `uid` INTEGER NOT NULL,
  `fid` INTEGER NOT NULL,
  `dateline` INTEGER NOT NULL,
  CONSTRAINT `gonof_home_forum_follow_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_home_forum_follow_ibfk_2` FOREIGN KEY (`fid`) REFERENCES `gonof_forum_forum` (`fid`)
) STRICT;
CREATE UNIQUE INDEX `gonof_home_forum_follow__uid` ON `gonof_home_forum_follow` (`uid`, `fid`);
CREATE INDEX `gonof_home_forum_follow__gonof_forum_follow_delivery` ON `gonof_home_forum_follow` (`fid`, `id`);

CREATE TABLE `gonof_home_interaction_request` (
  `uid` INTEGER NOT NULL,
  `request_key` TEXT NOT NULL CHECK (length(`request_key`) <= 64),
  `request_hash` TEXT NOT NULL CHECK (length(`request_hash`) <= 64),
  `dateline` INTEGER NOT NULL,
  PRIMARY KEY (`uid`, `request_key`),
  CONSTRAINT `gonof_home_interaction_request_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE INDEX `gonof_home_interaction_request__gonof_interaction_request_dateline` ON `gonof_home_interaction_request` (`dateline`);

CREATE TABLE `gonof_home_member_block` (
  `uid` INTEGER NOT NULL,
  `blocked_uid` INTEGER NOT NULL,
  `dateline` INTEGER NOT NULL,
  PRIMARY KEY (`uid`, `blocked_uid`),
  CONSTRAINT `gonof_home_member_block_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_home_member_block_ibfk_2` FOREIGN KEY (`blocked_uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE INDEX `gonof_home_member_block__blocked_uid` ON `gonof_home_member_block` (`blocked_uid`);

CREATE TABLE `gonof_home_member_follow` (
  `uid` INTEGER NOT NULL,
  `followed_uid` INTEGER NOT NULL,
  `dateline` INTEGER NOT NULL,
  PRIMARY KEY (`uid`, `followed_uid`),
  CONSTRAINT `gonof_home_member_follow_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_home_member_follow_ibfk_2` FOREIGN KEY (`followed_uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE INDEX `gonof_home_member_follow__gonof_member_follow_mine` ON `gonof_home_member_follow` (`uid`, `dateline`, `followed_uid`);
CREATE INDEX `gonof_home_member_follow__gonof_member_follow_fans` ON `gonof_home_member_follow` (`followed_uid`, `dateline`, `uid`);

CREATE TABLE `gonof_home_message` (
  `id` INTEGER PRIMARY KEY AUTOINCREMENT,
  `thread_id` INTEGER NOT NULL,
  `sender_uid` INTEGER NOT NULL,
  `body` TEXT NOT NULL CHECK (length(CAST(`body` AS BLOB)) <= 65535),
  `dateline` INTEGER NOT NULL,
  `event_key` TEXT NOT NULL CHECK (length(`event_key`) <= 100),
  CONSTRAINT `gonof_home_message_ibfk_1` FOREIGN KEY (`thread_id`) REFERENCES `gonof_home_message_thread` (`id`),
  CONSTRAINT `gonof_home_message_ibfk_2` FOREIGN KEY (`sender_uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE UNIQUE INDEX `gonof_home_message__event_key` ON `gonof_home_message` (`event_key`);
CREATE INDEX `gonof_home_message__gonof_message_thread` ON `gonof_home_message` (`thread_id`, `id`);
CREATE INDEX `gonof_home_message__sender_uid` ON `gonof_home_message` (`sender_uid`);

CREATE TABLE `gonof_home_message_member` (
  `thread_id` INTEGER NOT NULL,
  `uid` INTEGER NOT NULL,
  `counterpart_uid` INTEGER NOT NULL,
  `unread` INTEGER NOT NULL DEFAULT 0,
  `last_read_id` INTEGER NOT NULL DEFAULT 0,
  `last_dateline` INTEGER NOT NULL,
  PRIMARY KEY (`thread_id`, `uid`),
  CONSTRAINT `gonof_home_message_member_ibfk_1` FOREIGN KEY (`thread_id`) REFERENCES `gonof_home_message_thread` (`id`),
  CONSTRAINT `gonof_home_message_member_ibfk_2` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_home_message_member_ibfk_3` FOREIGN KEY (`counterpart_uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE INDEX `gonof_home_message_member__gonof_message_member_inbox` ON `gonof_home_message_member` (`uid`, `last_dateline`, `thread_id`);
CREATE INDEX `gonof_home_message_member__gonof_message_member_unread` ON `gonof_home_message_member` (`uid`, `unread`);
CREATE INDEX `gonof_home_message_member__counterpart_uid` ON `gonof_home_message_member` (`counterpart_uid`);

CREATE TABLE `gonof_home_message_thread` (
  `id` INTEGER PRIMARY KEY AUTOINCREMENT,
  `pair_key` TEXT NOT NULL CHECK (length(`pair_key`) <= 48),
  `last_message_id` INTEGER NOT NULL DEFAULT 0,
  `dateline` INTEGER NOT NULL
) STRICT;
CREATE UNIQUE INDEX `gonof_home_message_thread__pair_key` ON `gonof_home_message_thread` (`pair_key`);

CREATE TABLE `gonof_home_notification` (
  `id` INTEGER PRIMARY KEY AUTOINCREMENT,
  `uid` INTEGER NOT NULL,
  `authorid` INTEGER NOT NULL,
  `pid` INTEGER DEFAULT NULL,
  `type` TEXT NOT NULL DEFAULT 'reply' CHECK (length(`type`) <= 40),
  `event_key` TEXT NOT NULL CHECK (length(`event_key`) <= 100),
  `dateline` INTEGER NOT NULL,
  `read_at` INTEGER NOT NULL DEFAULT 0,
  `report_id` INTEGER DEFAULT NULL,
  `sanction_id` INTEGER DEFAULT NULL,
  `reason` TEXT NOT NULL DEFAULT '' CHECK (length(`reason`) <= 1200),
  CONSTRAINT `gonof_home_notification_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_home_notification_ibfk_2` FOREIGN KEY (`authorid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_home_notification_ibfk_3` FOREIGN KEY (`pid`) REFERENCES `gonof_forum_post` (`pid`),
  CONSTRAINT `gonof_notification_report_fk` FOREIGN KEY (`report_id`) REFERENCES `gonof_common_report` (`id`),
  CONSTRAINT `gonof_notification_sanction_fk` FOREIGN KEY (`sanction_id`) REFERENCES `gonof_common_member_sanction` (`id`)
) STRICT;
CREATE UNIQUE INDEX `gonof_home_notification__uid` ON `gonof_home_notification` (`uid`, `event_key`);
CREATE INDEX `gonof_home_notification__authorid` ON `gonof_home_notification` (`authorid`);
CREATE INDEX `gonof_home_notification__pid` ON `gonof_home_notification` (`pid`);
CREATE INDEX `gonof_home_notification__gonof_notification_list` ON `gonof_home_notification` (`uid`, `dateline`, `id`);
CREATE INDEX `gonof_home_notification__gonof_notification_unread` ON `gonof_home_notification` (`uid`, `read_at`, `dateline`, `id`);
CREATE INDEX `gonof_home_notification__gonof_notification_report_fk` ON `gonof_home_notification` (`report_id`);
CREATE INDEX `gonof_home_notification__gonof_notification_sanction_fk` ON `gonof_home_notification` (`sanction_id`);

CREATE TABLE `gonof_home_notification_preference` (
  `uid` INTEGER NOT NULL,
  `site_reply` INTEGER NOT NULL DEFAULT 1,
  `site_quote` INTEGER NOT NULL DEFAULT 1,
  `site_mention` INTEGER NOT NULL DEFAULT 1,
  `site_thread` INTEGER NOT NULL DEFAULT 1,
  `site_forum` INTEGER NOT NULL DEFAULT 1,
  `email_reply` INTEGER NOT NULL DEFAULT 0,
  `email_quote` INTEGER NOT NULL DEFAULT 0,
  `email_mention` INTEGER NOT NULL DEFAULT 0,
  `email_thread` INTEGER NOT NULL DEFAULT 0,
  `email_forum` INTEGER NOT NULL DEFAULT 0,
  `version` INTEGER NOT NULL DEFAULT 1,
  `updated_at` INTEGER NOT NULL,
  `email_message` INTEGER NOT NULL DEFAULT 0,
  `email_login` INTEGER NOT NULL DEFAULT 1,
  PRIMARY KEY (`uid`),
  CONSTRAINT `gonof_home_notification_preference_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;

CREATE TABLE `gonof_home_reading_preference` (
  `uid` INTEGER NOT NULL,
  `list_sort` TEXT DEFAULT NULL CHECK (length(`list_sort`) <= 16),
  `thread_view` TEXT DEFAULT NULL CHECK (length(`thread_view`) <= 16),
  `title_latest` INTEGER DEFAULT NULL,
  `title_new_tab` INTEGER DEFAULT NULL,
  `version` INTEGER NOT NULL DEFAULT 1,
  `updated_at` INTEGER NOT NULL,
  `avatar_shape` TEXT DEFAULT NULL CHECK (length(`avatar_shape`) <= 16),
  `custom_css` TEXT CHECK (length(CAST(`custom_css` AS BLOB)) <= 65535),
  `custom_css_version` INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (`uid`),
  CONSTRAINT `gonof_home_reading_preference_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;

CREATE TABLE `gonof_home_thread_follow` (
  `id` INTEGER PRIMARY KEY AUTOINCREMENT,
  `uid` INTEGER NOT NULL,
  `tid` INTEGER NOT NULL,
  `dateline` INTEGER NOT NULL,
  CONSTRAINT `gonof_home_thread_follow_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_home_thread_follow_ibfk_2` FOREIGN KEY (`tid`) REFERENCES `gonof_forum_thread` (`tid`)
) STRICT;
CREATE UNIQUE INDEX `gonof_home_thread_follow__uid` ON `gonof_home_thread_follow` (`uid`, `tid`);
CREATE INDEX `gonof_home_thread_follow__gonof_thread_follow_delivery` ON `gonof_home_thread_follow` (`tid`, `id`);

CREATE TABLE `gonof_rank_snapshot` (
  `board` TEXT NOT NULL CHECK (length(`board`) <= 16),
  `period` TEXT NOT NULL CHECK (length(`period`) <= 8),
  `position` INTEGER NOT NULL,
  `uid` INTEGER NOT NULL,
  `score` INTEGER NOT NULL,
  `computed_at` INTEGER NOT NULL,
  PRIMARY KEY (`board`, `period`, `position`),
  CONSTRAINT `gonof_rank_snapshot_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) STRICT;
CREATE INDEX `gonof_rank_snapshot__uid` ON `gonof_rank_snapshot` (`uid`);

-- Seed rows: site settings, credit rules, site-wide content revision (identical to the MySQL baseline).
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('identity.bootstrap_admin','pending');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('identity.username_namespace','1');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.account.email_change_ttl_hours','24');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.account.forgot_email_per_hour','5');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.account.forgot_email_per_minute','1');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.account.forgot_ip_per_hour','20');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.account.login_ip_per_5min','30');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.account.login_name_per_5min','10');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.account.password_change_account_per_5min','5');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.account.password_change_ip_per_5min','30');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.account.password_min_length','12');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.account.reset_ip_per_5min','30');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.account.reset_token_per_5min','10');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.account.reset_ttl_minutes','60');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.account.session_absolute_days','30');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.account.session_idle_days','14');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.account.two_factor_ip_per_5min','30');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.account.verification_ttl_hours','24');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.api.auth_failures_per_5min','30');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.api.enabled','true');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.api.key_ttl_days','365');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.api.max_body_kib','128');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.api.requests_per_minute','120');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.api.writes_per_minute','20');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.cache.forum_statistics_seconds','600');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.cache.list_ttl_seconds','60');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.cache.page_ttl_seconds','60');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.cache.statistics_seconds','600');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.cache.ttl_seconds','3600');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.debug.panel','false');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.display.footer_metrics','true');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.display.highlight_count','3');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.display.highlight_min_likes','1');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.display.hot_count','10');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.display.hot_days','7');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.display.latest_count','10');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.display.list_thumbnails','3');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.display.ranks_shown','50');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.display.recent_members','8');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.display.replies_per_page','50');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.display.sub_replies_per_page','20');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.display.threads_per_page','20');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.domains.enabled','false');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.email.enabled','false');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.feed.items','30');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.forum.directory_statistics','true');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.governance.appeal_handling_days','7');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.governance.appeal_window_days','30');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.governance.restoration_cooldown_days','30');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.governance.restoration_due_days','7');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.growth.accept','5');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.growth.activity_level','1');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.growth.digest','10');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.growth.invite_level','1');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.growth.level_names','');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.growth.levels','0,20,80,200,500,1200,3000,8000');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.growth.like','1');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.growth.link_level','1');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.growth.reply','1');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.growth.thread','3');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.invite.active_limit','5');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.invite.daily_limit','5');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.invite.enabled','true');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.invite.prefixes','');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.invite.ttl_days','7');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.limit.interaction_per_hour','600');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.limit.interaction_per_minute','60');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.limit.message_peer_per_minute','10');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.limit.message_per_minute','30');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.limit.report_daily','10');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.limit.restoration_daily','5');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.limit.social_per_5min','60');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.links.applications','true');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.links.apply_ip_per_hour','20');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.links.apply_per_hour','5');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.links.home_count','12');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.links.live_per_member','2');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.links.min_account_days','30');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.links.reject_cooldown_days','7');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.login.external_two_factor','require');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.login.methods','');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.login.participation','email');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.maintenance.outbox_retention_days','30');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.maintenance.prepared_mail_retention_days','7');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.maintenance.request_retention_days','30');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.navigation.body','');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.navigation.title','');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.navigation.updated_at','0');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.posting.body_max_bytes','60000');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.posting.daily_replies','20');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.posting.daily_threads','5');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.posting.duplicate_check','false');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.posting.duplicate_window_minutes','60');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.posting.edit_window_hours','24');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.posting.first_day_replies','5');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.posting.first_day_threads','1');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.posting.hourly_limit','0');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.posting.new_member_hours','24');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.posting.new_member_link_block','false');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.posting.new_member_link_days','3');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.posting.reply_interval_seconds','0');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.posting.thread_interval_seconds','0');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.ranks.refresh_seconds','60');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.reading.avatar_shape','rounded');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.reading.list_sort','latest');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.reading.thread_view','nested');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.reading.title_latest','false');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.reading.title_new_tab','false');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.seo.indexing','false');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.seo.robots','User-agent: *
Disallow: /manage
Disallow: /my
Disallow: /data
Disallow: /appeals
Disallow: /search
Disallow: /api
Disallow: /login
Disallow: /register
Disallow: /forgot-password
Disallow: /reset-password
Disallow: /verify-email');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.seo.verification','');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.signup.ip_hourly_limit','20');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.signup.mode','invite');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.site.brand_text','');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.site.contact_email','');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.site.credit_name','');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.site.description','');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.site.index_name','');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.site.language','en');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.site.name','');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.site.read_only','false');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.site.read_only_message','');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.site.url','');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.sms.code_ttl_minutes','5');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.sms.ip_hourly_limit','10');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.sms.max_attempts','5');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.sms.mobile_daily_limit','5');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.sms.mobile_interval_seconds','60');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.sms.site_daily_limit','500');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.time.clock24','true');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.time.date_format','locale');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.time.offset_minutes','480');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.time.relative','false');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.time.relative_hours','72');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.tip.amounts','1,5,10,20');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.tip.daily_limit','100');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.tip.enabled','true');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.username.cooldown_days','30');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.username.history_days','90');
INSERT INTO `gonof_common_credit_rule` (`rule_key`, `enabled`, `amount`, `daily_limit`, `version`) VALUES ('reply',0,1,1,1);
INSERT INTO `gonof_common_credit_rule` (`rule_key`, `enabled`, `amount`, `daily_limit`, `version`) VALUES ('signin',0,1,1,1);
INSERT INTO `gonof_common_credit_rule` (`rule_key`, `enabled`, `amount`, `daily_limit`, `version`) VALUES ('thread',0,1,1,1);
INSERT INTO `gonof_common_content_revision` (`id`, `revision`) VALUES (1,1);
