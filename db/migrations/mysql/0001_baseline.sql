-- Gono Forum database baseline: MySQL 5.7.44+, InnoDB / utf8mb4_bin (the presence table gonof_common_session is
-- MEMORY / ascii_bin). Business IDs and UTC Unix seconds are signed BIGINT; post bodies live only in gonof_forum_post.
--
-- 82 tables, created with mysqldump --default-character-set=utf8mb4 --no-data --skip-triggers --compact from an empty
-- 5.7.44 database (utf8mb4 / utf8mb4_bin), without the character_set_client lines and AUTO_INCREMENT=N; seed rows
-- exported with --no-create-info --complete-insert --skip-extended-insert --order-by-primary:
-- gonof_common_settings 139 rows, gonof_common_credit_rule 3, gonof_common_content_revision 1.
--
-- Tables are sorted by name. Foreign keys form cycles (members and attachments, threads and posts), so foreign key
-- checks are off while the tables are created and back on afterwards.
-- No triggers, routines, events or DEFINER: the project uses none (docs/architecture.md, "Data") and the migration
-- guard rejects them in every migration.
-- Released: changing a single byte makes SQLx refuse to start on the checksum; later schema changes go into new
-- migrations, starting at 0002, with a MySQL and an SQLite file of the same name.
-- SQLx submits the whole script as one multi-statement request.

SET FOREIGN_KEY_CHECKS = 0;

CREATE TABLE `gonof_common_content_revision` (
  `id` bigint(20) NOT NULL,
  `revision` bigint(20) NOT NULL DEFAULT '1',
  PRIMARY KEY (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_common_credit_account` (
  `uid` bigint(20) NOT NULL,
  `available` bigint(20) NOT NULL DEFAULT '0',
  `frozen` bigint(20) NOT NULL DEFAULT '0',
  `version` bigint(20) NOT NULL DEFAULT '1',
  PRIMARY KEY (`uid`),
  CONSTRAINT `gonof_common_credit_account_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_common_credit_ledger` (
  `id` bigint(20) NOT NULL AUTO_INCREMENT,
  `event_key` varchar(190) COLLATE utf8mb4_bin NOT NULL,
  `uid` bigint(20) NOT NULL,
  `available_delta` bigint(20) NOT NULL,
  `frozen_delta` bigint(20) NOT NULL,
  `available_after` bigint(20) NOT NULL,
  `frozen_after` bigint(20) NOT NULL,
  `kind` varchar(40) COLLATE utf8mb4_bin NOT NULL,
  `tid` bigint(20) DEFAULT NULL,
  `pid` bigint(20) DEFAULT NULL,
  `operator_uid` bigint(20) DEFAULT NULL,
  `reason` varchar(500) COLLATE utf8mb4_bin NOT NULL DEFAULT '',
  `dateline` bigint(20) NOT NULL,
  PRIMARY KEY (`id`),
  UNIQUE KEY `event_key` (`event_key`),
  KEY `pid` (`pid`),
  KEY `operator_uid` (`operator_uid`),
  KEY `gonof_credit_member_history` (`uid`,`dateline`,`id`),
  KEY `gonof_credit_rule_daily` (`uid`,`kind`,`dateline`),
  KEY `gonof_credit_thread_kind` (`tid`,`kind`,`id`),
  CONSTRAINT `gonof_common_credit_ledger_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_common_credit_ledger_ibfk_2` FOREIGN KEY (`tid`) REFERENCES `gonof_forum_thread` (`tid`),
  CONSTRAINT `gonof_common_credit_ledger_ibfk_3` FOREIGN KEY (`pid`) REFERENCES `gonof_forum_post` (`pid`),
  CONSTRAINT `gonof_common_credit_ledger_ibfk_4` FOREIGN KEY (`operator_uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_common_credit_rule` (
  `rule_key` varchar(24) COLLATE utf8mb4_bin NOT NULL,
  `enabled` bigint(20) NOT NULL DEFAULT '0',
  `amount` bigint(20) NOT NULL DEFAULT '1',
  `daily_limit` bigint(20) NOT NULL DEFAULT '1',
  `version` bigint(20) NOT NULL DEFAULT '1',
  PRIMARY KEY (`rule_key`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_common_data_request` (
  `id` bigint(20) NOT NULL AUTO_INCREMENT,
  `uid` bigint(20) NOT NULL,
  `kind` varchar(16) COLLATE utf8mb4_bin NOT NULL,
  `content_mode` varchar(16) COLLATE utf8mb4_bin NOT NULL DEFAULT 'retain',
  `status` varchar(16) COLLATE utf8mb4_bin NOT NULL,
  `request_key` varchar(64) COLLATE utf8mb4_bin NOT NULL,
  `request_hash` varchar(64) COLLATE utf8mb4_bin NOT NULL,
  `dateline` bigint(20) NOT NULL,
  `execute_after` bigint(20) NOT NULL,
  `finished_at` bigint(20) NOT NULL DEFAULT '0',
  `version` bigint(20) NOT NULL DEFAULT '1',
  `stage` varchar(24) COLLATE utf8mb4_bin NOT NULL DEFAULT 'start',
  `cursor_pid` bigint(20) NOT NULL DEFAULT '0',
  `upper_pid` bigint(20) NOT NULL DEFAULT '0',
  `artifact_key` varchar(160) COLLATE utf8mb4_bin DEFAULT NULL,
  `artifact_hash` varchar(64) COLLATE utf8mb4_bin DEFAULT NULL,
  `artifact_bytes` bigint(20) DEFAULT NULL,
  `artifact_expires_at` bigint(20) DEFAULT NULL,
  `last_error` varchar(1000) COLLATE utf8mb4_bin NOT NULL DEFAULT '',
  `active_slot` varchar(16) COLLATE utf8mb4_bin GENERATED ALWAYS AS ((case when (`status` in ('pending','cooling','processing','failed')) then (case when (`kind` = 'export') then 'export' else 'destructive' end) else NULL end)) STORED,
  PRIMARY KEY (`id`),
  UNIQUE KEY `uid` (`uid`,`request_key`),
  UNIQUE KEY `uid_2` (`uid`,`active_slot`),
  KEY `gonof_data_request_queue` (`status`,`execute_after`,`id`),
  KEY `gonof_data_request_owner` (`uid`,`id`),
  CONSTRAINT `gonof_common_data_request_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_common_friend_link` (
  `id` bigint(20) NOT NULL AUTO_INCREMENT,
  `status` varchar(16) COLLATE utf8mb4_bin NOT NULL,
  `category` varchar(16) COLLATE utf8mb4_bin NOT NULL,
  `name` varchar(30) COLLATE utf8mb4_bin NOT NULL,
  `url` varchar(500) COLLATE utf8mb4_bin NOT NULL,
  `domain` varchar(190) COLLATE utf8mb4_bin NOT NULL,
  `live_domain` varchar(190) COLLATE utf8mb4_bin DEFAULT NULL,
  `description` varchar(60) COLLATE utf8mb4_bin NOT NULL DEFAULT '',
  `backlink_url` varchar(500) COLLATE utf8mb4_bin DEFAULT NULL,
  `note` varchar(200) COLLATE utf8mb4_bin NOT NULL DEFAULT '',
  `home` bigint(20) NOT NULL DEFAULT '0',
  `displayorder` bigint(20) NOT NULL DEFAULT '0',
  `owner_uid` bigint(20) DEFAULT NULL,
  `request_key` varchar(64) COLLATE utf8mb4_bin DEFAULT NULL,
  `handler_uid` bigint(20) DEFAULT NULL,
  `resolution` varchar(200) COLLATE utf8mb4_bin NOT NULL DEFAULT '',
  `dateline` bigint(20) NOT NULL,
  `handled_at` bigint(20) DEFAULT NULL,
  `updated_at` bigint(20) NOT NULL,
  PRIMARY KEY (`id`),
  UNIQUE KEY `gonof_link_live_domain` (`live_domain`),
  UNIQUE KEY `gonof_link_request` (`owner_uid`,`request_key`),
  KEY `gonof_link_list` (`status`,`category`,`displayorder`,`id`),
  KEY `gonof_link_home` (`status`,`home`,`displayorder`,`id`),
  KEY `gonof_link_queue` (`status`,`id`),
  KEY `gonof_link_domain` (`domain`,`id`),
  KEY `handler_uid` (`handler_uid`),
  CONSTRAINT `gonof_common_friend_link_ibfk_1` FOREIGN KEY (`owner_uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_common_friend_link_ibfk_2` FOREIGN KEY (`handler_uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_common_growth_account` (
  `uid` bigint(20) NOT NULL,
  `growth` bigint(20) NOT NULL DEFAULT '0',
  `version` bigint(20) NOT NULL DEFAULT '0',
  PRIMARY KEY (`uid`),
  CONSTRAINT `gonof_common_growth_account_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_common_growth_ledger` (
  `id` bigint(20) NOT NULL AUTO_INCREMENT,
  `event_key` varchar(190) COLLATE utf8mb4_bin NOT NULL,
  `uid` bigint(20) NOT NULL,
  `delta` bigint(20) NOT NULL,
  `growth_after` bigint(20) NOT NULL,
  `kind` varchar(16) COLLATE utf8mb4_bin NOT NULL,
  `pid` bigint(20) DEFAULT NULL,
  `operator_uid` bigint(20) DEFAULT NULL,
  `reason` varchar(200) COLLATE utf8mb4_bin NOT NULL DEFAULT '',
  `dateline` bigint(20) NOT NULL,
  PRIMARY KEY (`id`),
  UNIQUE KEY `gonof_growth_event` (`event_key`),
  KEY `gonof_growth_member_history` (`uid`,`dateline`,`id`),
  KEY `gonof_growth_window` (`dateline`,`uid`,`delta`),
  KEY `pid` (`pid`),
  KEY `operator_uid` (`operator_uid`),
  CONSTRAINT `gonof_common_growth_ledger_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_common_growth_ledger_ibfk_2` FOREIGN KEY (`pid`) REFERENCES `gonof_forum_post` (`pid`),
  CONSTRAINT `gonof_common_growth_ledger_ibfk_3` FOREIGN KEY (`operator_uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_common_growth_post` (
  `pid` bigint(20) NOT NULL,
  `uid` bigint(20) NOT NULL,
  `points_base` bigint(20) NOT NULL,
  `points_like` bigint(20) NOT NULL,
  `points_digest` bigint(20) NOT NULL,
  `points_accept` bigint(20) NOT NULL,
  `credited` bigint(20) NOT NULL DEFAULT '0',
  `revision` bigint(20) NOT NULL DEFAULT '0',
  `dateline` bigint(20) NOT NULL,
  PRIMARY KEY (`pid`),
  KEY `uid` (`uid`),
  CONSTRAINT `gonof_common_growth_post_ibfk_1` FOREIGN KEY (`pid`) REFERENCES `gonof_forum_post` (`pid`),
  CONSTRAINT `gonof_common_growth_post_ibfk_2` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_common_honor` (
  `id` bigint(20) NOT NULL AUTO_INCREMENT,
  `name` varchar(20) COLLATE utf8mb4_bin NOT NULL,
  `description` varchar(60) COLLATE utf8mb4_bin NOT NULL,
  `displayorder` bigint(20) NOT NULL DEFAULT '0',
  `status` bigint(20) NOT NULL DEFAULT '1',
  `version` bigint(20) NOT NULL DEFAULT '1',
  PRIMARY KEY (`id`),
  UNIQUE KEY `name` (`name`),
  KEY `gonof_honor_order` (`displayorder`,`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_common_invite` (
  `id` bigint(20) NOT NULL AUTO_INCREMENT,
  `uid` bigint(20) NOT NULL,
  `code` varchar(64) COLLATE utf8mb4_bin NOT NULL,
  `request_key` varchar(64) COLLATE utf8mb4_bin NOT NULL,
  `fuid` bigint(20) DEFAULT NULL,
  `dateline` bigint(20) NOT NULL,
  `endtime` bigint(20) NOT NULL,
  `regdateline` bigint(20) DEFAULT NULL,
  `status` varchar(16) COLLATE utf8mb4_bin NOT NULL DEFAULT 'available',
  PRIMARY KEY (`id`),
  UNIQUE KEY `code` (`code`),
  UNIQUE KEY `uid` (`uid`,`request_key`),
  UNIQUE KEY `fuid` (`fuid`),
  KEY `gonof_invite_owner_created` (`uid`,`dateline`),
  KEY `gonof_invite_owner_available` (`uid`,`status`,`endtime`),
  KEY `gonof_invite_owner_id` (`uid`,`id`),
  CONSTRAINT `gonof_common_invite_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_common_invite_ibfk_2` FOREIGN KEY (`fuid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_common_invite_batch` (
  `uid` bigint(20) NOT NULL,
  `request_key` varchar(64) COLLATE utf8mb4_bin NOT NULL,
  `request_hash` char(64) COLLATE utf8mb4_bin NOT NULL,
  `invites` bigint(20) NOT NULL,
  `dateline` bigint(20) NOT NULL,
  PRIMARY KEY (`uid`,`request_key`),
  CONSTRAINT `gonof_common_invite_batch_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_common_localized_text` (
  `kind` varchar(32) COLLATE utf8mb4_bin NOT NULL,
  `language` varchar(16) COLLATE utf8mb4_bin NOT NULL,
  `title` varchar(120) COLLATE utf8mb4_bin NOT NULL,
  `body` text COLLATE utf8mb4_bin NOT NULL,
  `updated_at` bigint(20) NOT NULL,
  PRIMARY KEY (`kind`,`language`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_common_log` (
  `id` bigint(20) NOT NULL AUTO_INCREMENT,
  `uid` bigint(20) DEFAULT NULL,
  `operationuid` bigint(20) DEFAULT NULL,
  `type` varchar(100) COLLATE utf8mb4_bin NOT NULL,
  `data` text COLLATE utf8mb4_bin NOT NULL,
  `source` varchar(32) COLLATE utf8mb4_bin NOT NULL,
  `dateline` bigint(20) NOT NULL,
  `fid` bigint(20) DEFAULT NULL,
  `to_fid` bigint(20) DEFAULT NULL,
  `pid` bigint(20) DEFAULT NULL,
  PRIMARY KEY (`id`),
  KEY `gonof_log_member_created` (`uid`,`dateline`),
  KEY `gonof_log_operator_created` (`operationuid`,`dateline`),
  KEY `gonof_log_type_created` (`type`,`dateline`),
  KEY `gonof_log_post_created` (`pid`,`dateline`),
  CONSTRAINT `gonof_common_log_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_common_log_ibfk_2` FOREIGN KEY (`operationuid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_common_member` (
  `uid` bigint(20) NOT NULL AUTO_INCREMENT,
  `username` varchar(50) COLLATE utf8mb4_bin NOT NULL,
  `avatar` bigint(20) NOT NULL DEFAULT '0',
  `email` varchar(255) COLLATE utf8mb4_bin DEFAULT NULL,
  `mobile` varchar(16) COLLATE utf8mb4_bin DEFAULT NULL,
  `password` varchar(512) COLLATE utf8mb4_bin DEFAULT NULL,
  `emailstatus` bigint(20) NOT NULL DEFAULT '0',
  `adminid` bigint(20) NOT NULL DEFAULT '0',
  `freeze` bigint(20) NOT NULL DEFAULT '0',
  `mute_until` bigint(20) NOT NULL DEFAULT '0',
  `regdate` bigint(20) NOT NULL DEFAULT '0',
  `credentials_version` bigint(20) NOT NULL DEFAULT '1',
  `bio` varchar(500) COLLATE utf8mb4_bin NOT NULL DEFAULT '',
  `avatar_aid` bigint(20) DEFAULT NULL,
  `unread_notifications` bigint(20) NOT NULL DEFAULT '0',
  `space_visibility` varchar(16) COLLATE utf8mb4_bin NOT NULL DEFAULT 'public',
  `account_state` varchar(16) COLLATE utf8mb4_bin NOT NULL DEFAULT 'active',
  PRIMARY KEY (`uid`),
  UNIQUE KEY `username` (`username`),
  UNIQUE KEY `gonof_member_email` (`email`),
  UNIQUE KEY `gonof_member_mobile` (`mobile`),
  KEY `gonof_member_avatar_attachment` (`avatar_aid`),
  CONSTRAINT `gonof_member_avatar_fk` FOREIGN KEY (`avatar_aid`) REFERENCES `gonof_forum_attachment` (`aid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_common_member_api_key` (
  `id` bigint(20) NOT NULL AUTO_INCREMENT,
  `uid` bigint(20) NOT NULL,
  `key_hash` char(64) COLLATE utf8mb4_bin NOT NULL,
  `key_hint` varchar(16) COLLATE utf8mb4_bin NOT NULL,
  `dateline` bigint(20) NOT NULL,
  `expires_at` bigint(20) NOT NULL,
  `last_used_at` bigint(20) NOT NULL DEFAULT '0',
  `last_used_ip` varchar(45) COLLATE utf8mb4_bin NOT NULL DEFAULT '',
  `revoked_at` bigint(20) DEFAULT NULL,
  `revoked_by` bigint(20) DEFAULT NULL,
  PRIMARY KEY (`id`),
  UNIQUE KEY `key_hash` (`key_hash`),
  KEY `gonof_member_api_key_owner` (`uid`,`revoked_at`),
  KEY `revoked_by` (`revoked_by`),
  CONSTRAINT `gonof_common_member_api_key_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_common_member_api_key_ibfk_2` FOREIGN KEY (`revoked_by`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_common_member_appeal` (
  `id` bigint(20) NOT NULL AUTO_INCREMENT,
  `uid` bigint(20) NOT NULL,
  `sanction_id` bigint(20) NOT NULL,
  `reason` varchar(1000) COLLATE utf8mb4_bin NOT NULL,
  `status` varchar(16) COLLATE utf8mb4_bin NOT NULL DEFAULT 'pending',
  `handler_uid` bigint(20) DEFAULT NULL,
  `resolution` varchar(1000) COLLATE utf8mb4_bin NOT NULL DEFAULT '',
  `dateline` bigint(20) NOT NULL,
  `handled_at` bigint(20) NOT NULL DEFAULT '0',
  `request_key` varchar(64) COLLATE utf8mb4_bin NOT NULL,
  PRIMARY KEY (`id`),
  UNIQUE KEY `sanction_id` (`sanction_id`),
  UNIQUE KEY `uid` (`uid`,`request_key`),
  KEY `handler_uid` (`handler_uid`),
  KEY `gonof_appeal_queue` (`status`,`id`),
  KEY `gonof_appeal_owner` (`uid`,`id`),
  CONSTRAINT `gonof_common_member_appeal_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_common_member_appeal_ibfk_2` FOREIGN KEY (`sanction_id`) REFERENCES `gonof_common_member_sanction` (`id`),
  CONSTRAINT `gonof_common_member_appeal_ibfk_3` FOREIGN KEY (`handler_uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_common_member_count` (
  `uid` bigint(20) NOT NULL,
  `threads` bigint(20) NOT NULL DEFAULT '0',
  `replies` bigint(20) NOT NULL DEFAULT '0',
  `digests` bigint(20) NOT NULL DEFAULT '0',
  PRIMARY KEY (`uid`),
  CONSTRAINT `gonof_common_member_count_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_common_member_external` (
  `provider` varchar(16) COLLATE utf8mb4_bin NOT NULL,
  `subject` varchar(255) COLLATE utf8mb4_bin NOT NULL,
  `uid` bigint(20) NOT NULL,
  `display` varchar(255) COLLATE utf8mb4_bin NOT NULL DEFAULT '',
  `dateline` bigint(20) NOT NULL,
  `last_login_at` bigint(20) NOT NULL DEFAULT '0',
  PRIMARY KEY (`provider`,`subject`),
  UNIQUE KEY `gonof_member_external_uid` (`uid`,`provider`),
  CONSTRAINT `gonof_common_member_external_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_common_member_honor` (
  `uid` bigint(20) NOT NULL,
  `honor_id` bigint(20) NOT NULL,
  `granted_by` bigint(20) NOT NULL,
  `granted_at` bigint(20) NOT NULL,
  `expires_at` bigint(20) DEFAULT NULL,
  PRIMARY KEY (`uid`,`honor_id`),
  KEY `gonof_member_honor_group` (`honor_id`,`granted_at`,`uid`),
  KEY `gonof_member_honor_expiry` (`expires_at`),
  KEY `granted_by` (`granted_by`),
  CONSTRAINT `gonof_member_honor_granted_by_fk` FOREIGN KEY (`granted_by`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_member_honor_group_fk` FOREIGN KEY (`honor_id`) REFERENCES `gonof_common_honor` (`id`),
  CONSTRAINT `gonof_member_honor_uid_fk` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_common_member_presence` (
  `uid` bigint(20) NOT NULL,
  `last_seen` bigint(20) NOT NULL,
  PRIMARY KEY (`uid`),
  CONSTRAINT `gonof_common_member_presence_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_common_member_recovery_code` (
  `uid` bigint(20) NOT NULL,
  `code_hash` char(64) COLLATE utf8mb4_bin NOT NULL,
  `dateline` bigint(20) NOT NULL,
  PRIMARY KEY (`uid`,`code_hash`),
  CONSTRAINT `gonof_common_member_recovery_code_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_common_member_register` (
  `uid` bigint(20) NOT NULL,
  `site` varchar(500) COLLATE utf8mb4_bin DEFAULT NULL,
  `channel` varchar(64) COLLATE utf8mb4_bin DEFAULT NULL,
  `inviter_uid` bigint(20) DEFAULT NULL,
  `invite_prefix` varchar(64) COLLATE utf8mb4_bin DEFAULT NULL,
  PRIMARY KEY (`uid`),
  KEY `gonof_member_register_site` (`site`),
  KEY `gonof_member_register_channel` (`channel`),
  KEY `gonof_member_register_inviter` (`inviter_uid`),
  KEY `gonof_member_register_prefix` (`invite_prefix`),
  CONSTRAINT `gonof_common_member_register_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_common_member_register_ibfk_2` FOREIGN KEY (`inviter_uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_common_member_role_request` (
  `uid` bigint(20) NOT NULL,
  `request_key` varchar(64) COLLATE utf8mb4_bin NOT NULL,
  `request_hash` varchar(64) COLLATE utf8mb4_bin NOT NULL,
  `target_uid` bigint(20) NOT NULL,
  PRIMARY KEY (`uid`,`request_key`),
  KEY `target_uid` (`target_uid`),
  CONSTRAINT `gonof_common_member_role_request_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_common_member_role_request_ibfk_2` FOREIGN KEY (`target_uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_common_member_sanction` (
  `id` bigint(20) NOT NULL AUTO_INCREMENT,
  `uid` bigint(20) NOT NULL,
  `kind` varchar(16) COLLATE utf8mb4_bin NOT NULL,
  `reason` varchar(500) COLLATE utf8mb4_bin NOT NULL,
  `operator_uid` bigint(20) NOT NULL,
  `starts_at` bigint(20) NOT NULL,
  `expires_at` bigint(20) NOT NULL DEFAULT '0',
  `revoked_at` bigint(20) NOT NULL DEFAULT '0',
  `revoked_by` bigint(20) DEFAULT NULL,
  `request_key` varchar(64) COLLATE utf8mb4_bin NOT NULL,
  PRIMARY KEY (`id`),
  UNIQUE KEY `operator_uid` (`operator_uid`,`request_key`),
  KEY `revoked_by` (`revoked_by`),
  KEY `gonof_sanction_member` (`uid`,`kind`,`revoked_at`,`id`),
  KEY `gonof_sanction_expiry` (`kind`,`revoked_at`,`expires_at`,`id`),
  CONSTRAINT `gonof_common_member_sanction_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_common_member_sanction_ibfk_2` FOREIGN KEY (`operator_uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_common_member_sanction_ibfk_3` FOREIGN KEY (`revoked_by`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_common_member_token` (
  `id` bigint(20) NOT NULL AUTO_INCREMENT,
  `uid` bigint(20) NOT NULL,
  `purpose` varchar(32) COLLATE utf8mb4_bin NOT NULL,
  `token_hash` varchar(64) COLLATE utf8mb4_bin NOT NULL,
  `email` varchar(255) COLLATE utf8mb4_bin NOT NULL,
  `dateline` bigint(20) NOT NULL,
  `expires_at` bigint(20) NOT NULL,
  `consumed_at` bigint(20) DEFAULT NULL,
  `revoked_at` bigint(20) DEFAULT NULL,
  PRIMARY KEY (`id`),
  UNIQUE KEY `token_hash` (`token_hash`),
  KEY `gonof_member_token_active` (`uid`,`purpose`,`consumed_at`,`revoked_at`),
  CONSTRAINT `gonof_common_member_token_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_common_member_two_factor` (
  `uid` bigint(20) NOT NULL,
  `totp_secret` varchar(255) COLLATE utf8mb4_bin DEFAULT NULL,
  `totp_step` bigint(20) NOT NULL DEFAULT '0',
  `totp_enabled_at` bigint(20) NOT NULL DEFAULT '0',
  `failed_attempts` bigint(20) NOT NULL DEFAULT '0',
  `locked_until` bigint(20) NOT NULL DEFAULT '0',
  `enabled_at` bigint(20) NOT NULL,
  PRIMARY KEY (`uid`),
  CONSTRAINT `gonof_common_member_two_factor_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_common_member_username_history` (
  `username` varchar(50) COLLATE utf8mb4_bin NOT NULL,
  `uid` bigint(20) NOT NULL,
  `dateline` bigint(20) NOT NULL,
  `expires_at` bigint(20) NOT NULL,
  PRIMARY KEY (`username`),
  KEY `gonof_username_history_member` (`uid`,`dateline`),
  CONSTRAINT `gonof_common_member_username_history_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_common_member_verification` (
  `uid` bigint(20) NOT NULL,
  `category` varchar(16) COLLATE utf8mb4_bin NOT NULL,
  `title` varchar(60) COLLATE utf8mb4_bin NOT NULL,
  `site_url` varchar(500) COLLATE utf8mb4_bin DEFAULT NULL,
  `granted_by` bigint(20) NOT NULL,
  `granted_at` bigint(20) NOT NULL,
  `reviewed_at` bigint(20) NOT NULL,
  `expires_at` bigint(20) DEFAULT NULL,
  PRIMARY KEY (`uid`),
  KEY `gonof_verification_directory` (`category`,`granted_at`,`uid`),
  KEY `gonof_verification_expiry` (`expires_at`),
  KEY `granted_by` (`granted_by`),
  CONSTRAINT `gonof_common_member_verification_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_common_member_verification_ibfk_2` FOREIGN KEY (`granted_by`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_common_outbox` (
  `eventid` bigint(20) NOT NULL AUTO_INCREMENT,
  `event_key` varchar(160) COLLATE utf8mb4_bin NOT NULL,
  `event_type` varchar(80) COLLATE utf8mb4_bin NOT NULL,
  `aggregate_type` varchar(40) COLLATE utf8mb4_bin NOT NULL,
  `aggregate_id` bigint(20) NOT NULL,
  `payload` text COLLATE utf8mb4_bin NOT NULL,
  `dateline` bigint(20) NOT NULL,
  `available_at` bigint(20) NOT NULL,
  `attempts` bigint(20) NOT NULL DEFAULT '0',
  `lease_token` varchar(64) COLLATE utf8mb4_bin DEFAULT NULL,
  `lease_until` bigint(20) NOT NULL DEFAULT '0',
  `status` varchar(16) COLLATE utf8mb4_bin NOT NULL DEFAULT 'pending',
  `last_error` varchar(1000) COLLATE utf8mb4_bin DEFAULT NULL,
  PRIMARY KEY (`eventid`),
  UNIQUE KEY `event_key` (`event_key`),
  KEY `gonof_outbox_delivery` (`status`,`available_at`,`lease_until`,`eventid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_common_report` (
  `id` bigint(20) NOT NULL AUTO_INCREMENT,
  `uid` bigint(20) NOT NULL,
  `target_type` varchar(16) COLLATE utf8mb4_bin NOT NULL,
  `target_id` bigint(20) NOT NULL,
  `reason` varchar(1000) COLLATE utf8mb4_bin NOT NULL,
  `status` varchar(16) COLLATE utf8mb4_bin NOT NULL DEFAULT 'pending',
  `handler_uid` bigint(20) DEFAULT NULL,
  `resolution` varchar(1000) COLLATE utf8mb4_bin NOT NULL DEFAULT '',
  `dateline` bigint(20) NOT NULL,
  `handled_at` bigint(20) NOT NULL DEFAULT '0',
  `request_key` varchar(64) COLLATE utf8mb4_bin NOT NULL,
  PRIMARY KEY (`id`),
  UNIQUE KEY `uid` (`uid`,`request_key`),
  KEY `handler_uid` (`handler_uid`),
  KEY `gonof_report_queue` (`status`,`id`),
  KEY `gonof_report_owner` (`uid`,`id`),
  KEY `gonof_report_target` (`target_type`,`target_id`,`id`),
  CONSTRAINT `gonof_common_report_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_common_report_ibfk_2` FOREIGN KEY (`handler_uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_common_session` (
  `sid` char(64) COLLATE ascii_bin NOT NULL,
  `uid` bigint(20) NOT NULL,
  `lastactivity` bigint(20) NOT NULL,
  `client_ip` varchar(45) COLLATE ascii_bin NOT NULL DEFAULT '',
  PRIMARY KEY (`sid`) USING HASH,
  KEY `gonof_session_member` (`uid`) USING BTREE,
  KEY `gonof_session_activity` (`lastactivity`) USING BTREE
) ENGINE=MEMORY DEFAULT CHARSET=ascii COLLATE=ascii_bin;

CREATE TABLE `gonof_common_settings` (
  `skey` varchar(190) COLLATE utf8mb4_bin NOT NULL,
  `svalue` text COLLATE utf8mb4_bin NOT NULL,
  PRIMARY KEY (`skey`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_common_site_asset` (
  `slot` varchar(24) COLLATE utf8mb4_bin NOT NULL,
  `aid` bigint(20) DEFAULT NULL,
  `version` bigint(20) NOT NULL DEFAULT '1',
  `dateline` bigint(20) NOT NULL,
  PRIMARY KEY (`slot`),
  KEY `aid` (`aid`),
  CONSTRAINT `gonof_common_site_asset_ibfk_1` FOREIGN KEY (`aid`) REFERENCES `gonof_forum_attachment` (`aid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_common_site_domain` (
  `domain_id` bigint(20) NOT NULL AUTO_INCREMENT,
  `site_url` varchar(255) COLLATE utf8mb4_bin NOT NULL,
  `site_name` varchar(80) COLLATE utf8mb4_bin DEFAULT NULL,
  `canonical` varchar(8) COLLATE utf8mb4_bin NOT NULL DEFAULT 'self',
  `logo_aid` bigint(20) DEFAULT NULL,
  `favicon_aid` bigint(20) DEFAULT NULL,
  `verification` text COLLATE utf8mb4_bin,
  `login_methods` varchar(64) COLLATE utf8mb4_bin DEFAULT NULL,
  `participation` varchar(64) COLLATE utf8mb4_bin DEFAULT NULL,
  `status` bigint(20) NOT NULL DEFAULT '1',
  `version` bigint(20) NOT NULL DEFAULT '1',
  `dateline` bigint(20) NOT NULL,
  `language` varchar(16) COLLATE utf8mb4_bin DEFAULT NULL,
  PRIMARY KEY (`domain_id`),
  UNIQUE KEY `site_url` (`site_url`),
  KEY `logo_aid` (`logo_aid`),
  KEY `favicon_aid` (`favicon_aid`),
  CONSTRAINT `gonof_site_domain_favicon_fk` FOREIGN KEY (`favicon_aid`) REFERENCES `gonof_forum_attachment` (`aid`),
  CONSTRAINT `gonof_site_domain_logo_fk` FOREIGN KEY (`logo_aid`) REFERENCES `gonof_forum_attachment` (`aid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_common_site_domain_forum` (
  `domain_id` bigint(20) NOT NULL,
  `fid` bigint(20) NOT NULL,
  PRIMARY KEY (`domain_id`,`fid`),
  KEY `fid` (`fid`),
  CONSTRAINT `gonof_site_domain_forum_domain_fk` FOREIGN KEY (`domain_id`) REFERENCES `gonof_common_site_domain` (`domain_id`),
  CONSTRAINT `gonof_site_domain_forum_fid_fk` FOREIGN KEY (`fid`) REFERENCES `gonof_forum_forum` (`fid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_common_sms_code` (
  `id` bigint(20) NOT NULL AUTO_INCREMENT,
  `mobile` varchar(16) COLLATE utf8mb4_bin NOT NULL,
  `purpose` varchar(16) COLLATE utf8mb4_bin NOT NULL,
  `code_hash` char(64) COLLATE utf8mb4_bin NOT NULL,
  `dateline` bigint(20) NOT NULL,
  `expires_at` bigint(20) NOT NULL,
  `attempts` bigint(20) NOT NULL DEFAULT '0',
  `consumed_at` bigint(20) DEFAULT NULL,
  `client_ip` varchar(45) COLLATE utf8mb4_bin NOT NULL DEFAULT '',
  PRIMARY KEY (`id`),
  KEY `gonof_sms_code_mobile` (`mobile`,`dateline`),
  KEY `gonof_sms_code_dateline` (`dateline`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_forum_activity` (
  `tid` bigint(20) NOT NULL,
  `owner_uid` bigint(20) NOT NULL,
  `kind` bigint(20) NOT NULL,
  `budget` bigint(20) NOT NULL DEFAULT '0',
  `remaining` bigint(20) NOT NULL DEFAULT '0',
  `deadline` bigint(20) NOT NULL DEFAULT '0',
  `state` varchar(16) COLLATE utf8mb4_bin NOT NULL DEFAULT 'open',
  `created_at` bigint(20) NOT NULL,
  `settled_at` bigint(20) NOT NULL DEFAULT '0',
  PRIMARY KEY (`tid`),
  KEY `owner_uid` (`owner_uid`),
  KEY `gonof_activity_due` (`state`,`deadline`,`tid`),
  CONSTRAINT `gonof_forum_activity_ibfk_1` FOREIGN KEY (`tid`) REFERENCES `gonof_forum_thread` (`tid`),
  CONSTRAINT `gonof_forum_activity_ibfk_2` FOREIGN KEY (`owner_uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_forum_activity_draw` (
  `tid` bigint(20) NOT NULL,
  `seed` varchar(64) COLLATE utf8mb4_bin NOT NULL,
  `eligible` longtext COLLATE utf8mb4_bin NOT NULL,
  `winners` longtext COLLATE utf8mb4_bin NOT NULL,
  `dateline` bigint(20) NOT NULL,
  PRIMARY KEY (`tid`),
  CONSTRAINT `gonof_forum_activity_draw_ibfk_1` FOREIGN KEY (`tid`) REFERENCES `gonof_forum_lottery` (`tid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_forum_attachment` (
  `aid` bigint(20) NOT NULL AUTO_INCREMENT,
  `uid` bigint(20) NOT NULL,
  `request_key` varchar(64) COLLATE utf8mb4_bin NOT NULL,
  `request_hash` varchar(64) COLLATE utf8mb4_bin NOT NULL,
  PRIMARY KEY (`aid`),
  UNIQUE KEY `uid` (`uid`,`request_key`),
  KEY `gonof_attachment_owner` (`uid`,`aid`),
  CONSTRAINT `gonof_forum_attachment_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_forum_attachment_0` (
  `aid` bigint(20) NOT NULL,
  `filename` varchar(255) COLLATE utf8mb4_bin NOT NULL,
  `filesize` bigint(20) NOT NULL,
  `mime_type` varchar(127) COLLATE utf8mb4_bin NOT NULL,
  `content_sha256` varchar(64) COLLATE utf8mb4_bin NOT NULL,
  `attachment` varchar(1024) COLLATE utf8mb4_bin NOT NULL,
  `storage_profile_id` varchar(64) COLLATE utf8mb4_bin NOT NULL,
  `width` bigint(20) DEFAULT NULL,
  `height` bigint(20) DEFAULT NULL,
  `description` text COLLATE utf8mb4_bin NOT NULL,
  `alt_text` text COLLATE utf8mb4_bin NOT NULL,
  `dateline` bigint(20) NOT NULL,
  `updated_at` bigint(20) NOT NULL,
  `checked_at` bigint(20) DEFAULT NULL,
  `purge_after` bigint(20) DEFAULT NULL,
  `lifecycle` varchar(16) COLLATE utf8mb4_bin NOT NULL DEFAULT 'pending',
  `scan_status` varchar(16) COLLATE utf8mb4_bin NOT NULL DEFAULT 'pending',
  `scan_detail` text COLLATE utf8mb4_bin NOT NULL,
  `trashed_at` bigint(20) DEFAULT NULL,
  PRIMARY KEY (`aid`),
  KEY `gonof_attachment_purge_due` (`lifecycle`,`purge_after`),
  CONSTRAINT `gonof_forum_attachment_0_ibfk_1` FOREIGN KEY (`aid`) REFERENCES `gonof_forum_attachment` (`aid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_forum_attachment_reference` (
  `aid` bigint(20) NOT NULL,
  `pid` bigint(20) NOT NULL,
  `dateline` bigint(20) NOT NULL,
  PRIMARY KEY (`aid`,`pid`),
  KEY `gonof_attachment_reference_post` (`pid`,`aid`),
  CONSTRAINT `gonof_forum_attachment_reference_ibfk_1` FOREIGN KEY (`aid`) REFERENCES `gonof_forum_attachment` (`aid`),
  CONSTRAINT `gonof_forum_attachment_reference_ibfk_2` FOREIGN KEY (`pid`) REFERENCES `gonof_forum_post` (`pid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_forum_forum` (
  `fid` bigint(20) NOT NULL AUTO_INCREMENT,
  `slug` varchar(64) COLLATE utf8mb4_bin NOT NULL,
  `name` varchar(50) COLLATE utf8mb4_bin NOT NULL,
  `description` text COLLATE utf8mb4_bin NOT NULL,
  `guidance` text COLLATE utf8mb4_bin NOT NULL,
  `displayorder` bigint(20) NOT NULL DEFAULT '0',
  `status` bigint(20) NOT NULL DEFAULT '1',
  `visibility` varchar(24) COLLATE utf8mb4_bin NOT NULL DEFAULT 'public',
  `parent_fid` bigint(20) DEFAULT NULL,
  `icon_aid` bigint(20) DEFAULT NULL,
  `version` bigint(20) NOT NULL DEFAULT '1',
  PRIMARY KEY (`fid`),
  UNIQUE KEY `slug` (`slug`),
  KEY `gonof_forum_icon_fk` (`icon_aid`),
  KEY `gonof_forum_parent_order` (`parent_fid`,`displayorder`,`fid`),
  CONSTRAINT `gonof_forum_icon_fk` FOREIGN KEY (`icon_aid`) REFERENCES `gonof_forum_attachment` (`aid`),
  CONSTRAINT `gonof_forum_parent_fk` FOREIGN KEY (`parent_fid`) REFERENCES `gonof_forum_forum` (`fid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_forum_lottery` (
  `tid` bigint(20) NOT NULL,
  `shares` bigint(20) NOT NULL,
  `entry_mode` varchar(8) COLLATE utf8mb4_bin NOT NULL DEFAULT 'reply',
  `requirements` text COLLATE utf8mb4_bin,
  `review` bigint(20) NOT NULL DEFAULT '0',
  `early_draw` bigint(20) NOT NULL DEFAULT '0',
  `planned_deadline` bigint(20) NOT NULL DEFAULT '0',
  PRIMARY KEY (`tid`),
  CONSTRAINT `gonof_forum_lottery_ibfk_1` FOREIGN KEY (`tid`) REFERENCES `gonof_forum_activity` (`tid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_forum_lottery_award` (
  `tid` bigint(20) NOT NULL,
  `prize_no` bigint(20) NOT NULL,
  `slot_no` bigint(20) NOT NULL,
  `code_ciphertext` varchar(512) COLLATE utf8mb4_bin DEFAULT NULL,
  `uid` bigint(20) DEFAULT NULL,
  `assigned_at` bigint(20) NOT NULL DEFAULT '0',
  `revised_at` bigint(20) NOT NULL DEFAULT '0',
  PRIMARY KEY (`tid`,`prize_no`,`slot_no`),
  UNIQUE KEY `gonof_lottery_award_winner` (`tid`,`uid`),
  KEY `gonof_lottery_award_member` (`uid`,`tid`),
  CONSTRAINT `gonof_forum_lottery_award_ibfk_1` FOREIGN KEY (`tid`, `prize_no`) REFERENCES `gonof_forum_lottery_prize` (`tid`, `prize_no`),
  CONSTRAINT `gonof_forum_lottery_award_ibfk_2` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_forum_lottery_entry` (
  `tid` bigint(20) NOT NULL,
  `uid` bigint(20) NOT NULL,
  `source_pid` bigint(20) NOT NULL,
  `amount` bigint(20) NOT NULL DEFAULT '0',
  `dateline` bigint(20) NOT NULL,
  PRIMARY KEY (`tid`,`uid`),
  KEY `uid` (`uid`),
  KEY `source_pid` (`source_pid`),
  CONSTRAINT `gonof_forum_lottery_entry_ibfk_1` FOREIGN KEY (`tid`) REFERENCES `gonof_forum_lottery` (`tid`),
  CONSTRAINT `gonof_forum_lottery_entry_ibfk_2` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_forum_lottery_entry_ibfk_3` FOREIGN KEY (`source_pid`) REFERENCES `gonof_forum_post` (`pid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_forum_lottery_exclusion` (
  `tid` bigint(20) NOT NULL,
  `uid` bigint(20) NOT NULL,
  `reason` varchar(200) COLLATE utf8mb4_bin NOT NULL,
  `dateline` bigint(20) NOT NULL,
  PRIMARY KEY (`tid`,`uid`),
  KEY `gonof_lottery_exclusion_member` (`uid`,`tid`),
  CONSTRAINT `gonof_forum_lottery_exclusion_ibfk_1` FOREIGN KEY (`tid`) REFERENCES `gonof_forum_lottery` (`tid`),
  CONSTRAINT `gonof_forum_lottery_exclusion_ibfk_2` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_forum_lottery_prize` (
  `tid` bigint(20) NOT NULL,
  `prize_no` bigint(20) NOT NULL,
  `name` varchar(60) COLLATE utf8mb4_bin NOT NULL,
  `winners` bigint(20) NOT NULL,
  `kind` varchar(8) COLLATE utf8mb4_bin NOT NULL,
  `amount` bigint(20) NOT NULL DEFAULT '0',
  `note` varchar(200) COLLATE utf8mb4_bin NOT NULL DEFAULT '',
  PRIMARY KEY (`tid`,`prize_no`),
  CONSTRAINT `gonof_forum_lottery_prize_ibfk_1` FOREIGN KEY (`tid`) REFERENCES `gonof_forum_lottery` (`tid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_forum_lottery_signup` (
  `tid` bigint(20) NOT NULL,
  `uid` bigint(20) NOT NULL,
  `amount` bigint(20) NOT NULL DEFAULT '0',
  `dateline` bigint(20) NOT NULL,
  PRIMARY KEY (`tid`,`uid`),
  KEY `gonof_lottery_signup_member` (`uid`,`tid`),
  CONSTRAINT `gonof_forum_lottery_signup_ibfk_1` FOREIGN KEY (`tid`) REFERENCES `gonof_forum_lottery` (`tid`),
  CONSTRAINT `gonof_forum_lottery_signup_ibfk_2` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_forum_moderator` (
  `uid` bigint(20) NOT NULL,
  `fid` bigint(20) NOT NULL,
  PRIMARY KEY (`uid`,`fid`),
  KEY `fid` (`fid`),
  CONSTRAINT `gonof_forum_moderator_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_forum_moderator_ibfk_2` FOREIGN KEY (`fid`) REFERENCES `gonof_forum_forum` (`fid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_forum_news` (
  `tid` bigint(20) NOT NULL,
  `source_url` varchar(2048) COLLATE utf8mb4_bin NOT NULL,
  `source_name` varchar(255) COLLATE utf8mb4_bin NOT NULL,
  `source_author` varchar(255) COLLATE utf8mb4_bin NOT NULL,
  PRIMARY KEY (`tid`),
  CONSTRAINT `gonof_forum_news_ibfk_1` FOREIGN KEY (`tid`) REFERENCES `gonof_forum_activity` (`tid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_forum_poll` (
  `tid` bigint(20) NOT NULL,
  `max_choices` bigint(20) NOT NULL DEFAULT '1',
  `voters` bigint(20) NOT NULL DEFAULT '0',
  PRIMARY KEY (`tid`),
  CONSTRAINT `gonof_forum_poll_ibfk_1` FOREIGN KEY (`tid`) REFERENCES `gonof_forum_activity` (`tid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_forum_poll_option` (
  `tid` bigint(20) NOT NULL,
  `option_no` bigint(20) NOT NULL,
  `label` varchar(255) COLLATE utf8mb4_bin NOT NULL,
  `votes` bigint(20) NOT NULL DEFAULT '0',
  PRIMARY KEY (`tid`,`option_no`),
  CONSTRAINT `gonof_forum_poll_option_ibfk_1` FOREIGN KEY (`tid`) REFERENCES `gonof_forum_poll` (`tid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_forum_poll_vote` (
  `tid` bigint(20) NOT NULL,
  `uid` bigint(20) NOT NULL,
  `choices` text COLLATE utf8mb4_bin NOT NULL,
  `request_key` varchar(64) COLLATE utf8mb4_bin NOT NULL,
  `dateline` bigint(20) NOT NULL,
  PRIMARY KEY (`tid`,`uid`),
  KEY `uid` (`uid`),
  CONSTRAINT `gonof_forum_poll_vote_ibfk_1` FOREIGN KEY (`tid`) REFERENCES `gonof_forum_poll` (`tid`),
  CONSTRAINT `gonof_forum_poll_vote_ibfk_2` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_forum_post` (
  `pid` bigint(20) NOT NULL,
  `fid` bigint(20) NOT NULL,
  `tid` bigint(20) NOT NULL,
  `authorid` bigint(20) DEFAULT NULL,
  `first` bigint(20) NOT NULL DEFAULT '0',
  `position` bigint(20) NOT NULL,
  `root_pid` bigint(20) DEFAULT NULL,
  `reply_to_pid` bigint(20) DEFAULT NULL,
  `message` longtext COLLATE utf8mb4_bin NOT NULL,
  `dateline` bigint(20) NOT NULL DEFAULT '0',
  `invisible` bigint(20) NOT NULL DEFAULT '0',
  `root_invisible` bigint(20) NOT NULL DEFAULT '0',
  `status` bigint(20) NOT NULL DEFAULT '0',
  `sub_replies_count` bigint(20) NOT NULL DEFAULT '0',
  `reactions` bigint(20) NOT NULL DEFAULT '0',
  `downvotes` bigint(20) NOT NULL DEFAULT '0',
  `deleted_at` bigint(20) NOT NULL DEFAULT '0',
  `version` bigint(20) NOT NULL DEFAULT '1',
  `edited_at` bigint(20) NOT NULL DEFAULT '0',
  `root_position` bigint(20) DEFAULT NULL,
  `sub_position` bigint(20) DEFAULT NULL,
  `erased_at` bigint(20) NOT NULL DEFAULT '0',
  `first_tid` bigint(20) DEFAULT NULL,
  PRIMARY KEY (`tid`,`position`),
  UNIQUE KEY `pid` (`pid`,`tid`,`fid`),
  UNIQUE KEY `pid_2` (`pid`),
  UNIQUE KEY `gonof_post_pid_thread` (`pid`,`tid`),
  UNIQUE KEY `gonof_post_root_position` (`tid`,`root_position`),
  UNIQUE KEY `gonof_post_sub_position` (`root_pid`,`sub_position`),
  UNIQUE KEY `first_tid` (`first_tid`),
  KEY `root_pid` (`root_pid`,`tid`,`fid`),
  KEY `reply_to_pid` (`reply_to_pid`,`tid`,`fid`),
  KEY `gonof_post_thread_root` (`tid`,`root_pid`,`position`),
  KEY `gonof_post_author_created` (`authorid`,`first`,`dateline`),
  KEY `gonof_post_thread_forum_fk` (`tid`,`fid`),
  KEY `gonof_post_dateline` (`dateline`),
  CONSTRAINT `gonof_forum_post_ibfk_1` FOREIGN KEY (`pid`) REFERENCES `gonof_forum_post_tableid` (`pid`),
  CONSTRAINT `gonof_forum_post_ibfk_3` FOREIGN KEY (`authorid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_post_reply_thread_fk` FOREIGN KEY (`reply_to_pid`, `tid`) REFERENCES `gonof_forum_post` (`pid`, `tid`),
  CONSTRAINT `gonof_post_root_thread_fk` FOREIGN KEY (`root_pid`, `tid`) REFERENCES `gonof_forum_post` (`pid`, `tid`),
  CONSTRAINT `gonof_post_thread_forum_fk` FOREIGN KEY (`tid`, `fid`) REFERENCES `gonof_forum_thread` (`tid`, `fid`) ON UPDATE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_forum_post_downvote` (
  `pid` bigint(20) NOT NULL,
  `uid` bigint(20) NOT NULL,
  `dateline` bigint(20) NOT NULL,
  PRIMARY KEY (`pid`,`uid`),
  KEY `gonof_post_downvote_member` (`uid`,`pid`),
  CONSTRAINT `gonof_forum_post_downvote_ibfk_1` FOREIGN KEY (`pid`) REFERENCES `gonof_forum_post` (`pid`),
  CONSTRAINT `gonof_forum_post_downvote_ibfk_2` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_forum_post_reaction` (
  `pid` bigint(20) NOT NULL,
  `uid` bigint(20) NOT NULL,
  `dateline` bigint(20) NOT NULL,
  PRIMARY KEY (`pid`,`uid`),
  KEY `uid` (`uid`),
  KEY `gonof_reaction_dateline` (`dateline`),
  CONSTRAINT `gonof_forum_post_reaction_ibfk_1` FOREIGN KEY (`pid`) REFERENCES `gonof_forum_post` (`pid`),
  CONSTRAINT `gonof_forum_post_reaction_ibfk_2` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_forum_post_request` (
  `uid` bigint(20) NOT NULL,
  `request_key` varchar(64) COLLATE utf8mb4_bin NOT NULL,
  `request_hash` varchar(64) COLLATE utf8mb4_bin NOT NULL,
  `pid` bigint(20) NOT NULL,
  `dateline` bigint(20) NOT NULL DEFAULT '0',
  PRIMARY KEY (`uid`,`request_key`),
  KEY `pid` (`pid`),
  KEY `gonof_post_request_dateline` (`dateline`),
  CONSTRAINT `gonof_forum_post_request_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_forum_post_request_ibfk_2` FOREIGN KEY (`pid`) REFERENCES `gonof_forum_post` (`pid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_forum_post_revision` (
  `pid` bigint(20) NOT NULL,
  `version` bigint(20) NOT NULL,
  `editor_uid` bigint(20) NOT NULL,
  `subject` varchar(255) COLLATE utf8mb4_bin DEFAULT NULL,
  `message` text COLLATE utf8mb4_bin NOT NULL,
  `dateline` bigint(20) NOT NULL,
  PRIMARY KEY (`pid`,`version`),
  KEY `editor_uid` (`editor_uid`),
  CONSTRAINT `gonof_forum_post_revision_ibfk_1` FOREIGN KEY (`pid`) REFERENCES `gonof_forum_post` (`pid`),
  CONSTRAINT `gonof_forum_post_revision_ibfk_2` FOREIGN KEY (`editor_uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_forum_post_tableid` (
  `pid` bigint(20) NOT NULL AUTO_INCREMENT,
  PRIMARY KEY (`pid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_forum_redpacket` (
  `tid` bigint(20) NOT NULL,
  `shares` bigint(20) NOT NULL,
  `remaining_shares` bigint(20) NOT NULL,
  `distribution` varchar(12) COLLATE utf8mb4_bin NOT NULL DEFAULT 'equal',
  PRIMARY KEY (`tid`),
  CONSTRAINT `gonof_forum_redpacket_ibfk_1` FOREIGN KEY (`tid`) REFERENCES `gonof_forum_activity` (`tid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_forum_redpacket_claim` (
  `tid` bigint(20) NOT NULL,
  `uid` bigint(20) NOT NULL,
  `source_pid` bigint(20) NOT NULL,
  `amount` bigint(20) NOT NULL,
  `dateline` bigint(20) NOT NULL,
  PRIMARY KEY (`tid`,`uid`),
  KEY `uid` (`uid`),
  KEY `source_pid` (`source_pid`),
  CONSTRAINT `gonof_forum_redpacket_claim_ibfk_1` FOREIGN KEY (`tid`) REFERENCES `gonof_forum_redpacket` (`tid`),
  CONSTRAINT `gonof_forum_redpacket_claim_ibfk_2` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_forum_redpacket_claim_ibfk_3` FOREIGN KEY (`source_pid`) REFERENCES `gonof_forum_post` (`pid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_forum_restore_request` (
  `id` bigint(20) NOT NULL AUTO_INCREMENT,
  `uid` bigint(20) NOT NULL,
  `pid` bigint(20) NOT NULL,
  `state` varchar(16) COLLATE utf8mb4_bin NOT NULL,
  `hidden_by` bigint(20) DEFAULT NULL,
  `reason` varchar(1000) COLLATE utf8mb4_bin NOT NULL,
  `status` varchar(16) COLLATE utf8mb4_bin NOT NULL DEFAULT 'pending',
  `handler_uid` bigint(20) DEFAULT NULL,
  `resolution` varchar(500) COLLATE utf8mb4_bin NOT NULL DEFAULT '',
  `handled_at` bigint(20) NOT NULL DEFAULT '0',
  `dateline` bigint(20) NOT NULL,
  `request_key` varchar(64) COLLATE utf8mb4_bin NOT NULL,
  PRIMARY KEY (`id`),
  UNIQUE KEY `gonof_restore_request_key` (`uid`,`request_key`),
  KEY `gonof_restore_owner` (`uid`,`dateline`,`id`),
  KEY `gonof_restore_target` (`pid`,`status`,`id`),
  KEY `gonof_restore_queue` (`status`,`id`),
  KEY `hidden_by` (`hidden_by`),
  KEY `handler_uid` (`handler_uid`),
  CONSTRAINT `gonof_forum_restore_request_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_forum_restore_request_ibfk_2` FOREIGN KEY (`pid`) REFERENCES `gonof_forum_post` (`pid`),
  CONSTRAINT `gonof_forum_restore_request_ibfk_3` FOREIGN KEY (`hidden_by`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_forum_restore_request_ibfk_4` FOREIGN KEY (`handler_uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_forum_reward` (
  `tid` bigint(20) NOT NULL,
  `accepted_pid` bigint(20) DEFAULT NULL,
  PRIMARY KEY (`tid`),
  KEY `accepted_pid` (`accepted_pid`),
  CONSTRAINT `gonof_forum_reward_ibfk_1` FOREIGN KEY (`tid`) REFERENCES `gonof_forum_activity` (`tid`),
  CONSTRAINT `gonof_forum_reward_ibfk_2` FOREIGN KEY (`accepted_pid`) REFERENCES `gonof_forum_post` (`pid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_forum_thread` (
  `tid` bigint(20) NOT NULL AUTO_INCREMENT,
  `fid` bigint(20) NOT NULL,
  `authorid` bigint(20) DEFAULT NULL,
  `subject` varchar(255) COLLATE utf8mb4_bin NOT NULL,
  `dateline` bigint(20) NOT NULL DEFAULT '0',
  `lastpost` bigint(20) NOT NULL DEFAULT '0',
  `views` bigint(20) NOT NULL DEFAULT '0',
  `favorites` bigint(20) NOT NULL DEFAULT '0',
  `tips` bigint(20) NOT NULL DEFAULT '0',
  `tip_total` bigint(20) NOT NULL DEFAULT '0',
  `displayorder` bigint(20) NOT NULL DEFAULT '0',
  `digest` bigint(20) NOT NULL DEFAULT '0',
  `closed` bigint(20) NOT NULL DEFAULT '0',
  `readperm` bigint(20) NOT NULL DEFAULT '0',
  `status` bigint(20) NOT NULL DEFAULT '0',
  `replies` bigint(20) NOT NULL DEFAULT '0',
  `maxposition` bigint(20) NOT NULL DEFAULT '1',
  `visibility` varchar(24) COLLATE utf8mb4_bin NOT NULL DEFAULT 'public',
  `publication_state` varchar(24) COLLATE utf8mb4_bin NOT NULL DEFAULT 'published',
  `deleted_at` bigint(20) NOT NULL DEFAULT '0',
  `version` bigint(20) NOT NULL DEFAULT '1',
  `category` bigint(20) NOT NULL DEFAULT '0',
  `root_replies` bigint(20) NOT NULL DEFAULT '0',
  `valid_replies` bigint(20) NOT NULL DEFAULT '0',
  `last_reply_pid` bigint(20) DEFAULT NULL,
  `last_reply_author` varchar(50) COLLATE utf8mb4_bin NOT NULL DEFAULT '',
  `last_reply_summary` varchar(255) COLLATE utf8mb4_bin NOT NULL DEFAULT '',
  `author` varchar(50) COLLATE utf8mb4_bin NOT NULL DEFAULT '',
  `family_fid` bigint(20) NOT NULL,
  `cache_revision` bigint(20) NOT NULL DEFAULT '1',
  PRIMARY KEY (`tid`),
  UNIQUE KEY `tid` (`tid`,`fid`),
  KEY `authorid` (`authorid`),
  KEY `gonof_thread_last_reply_fk` (`last_reply_pid`),
  KEY `gonof_thread_global_latest` (`displayorder`,`lastpost`,`tid`,`visibility`,`fid`,`authorid`),
  KEY `gonof_thread_global_created` (`displayorder`,`dateline`,`tid`,`visibility`,`fid`,`authorid`),
  KEY `gonof_thread_family_latest` (`family_fid`,`displayorder`,`lastpost`,`tid`,`visibility`,`fid`,`authorid`),
  KEY `gonof_thread_family_created` (`family_fid`,`displayorder`,`dateline`,`tid`,`visibility`,`fid`,`authorid`),
  KEY `gonof_thread_forum_latest` (`fid`,`displayorder`,`lastpost`,`tid`,`visibility`,`authorid`),
  KEY `gonof_thread_forum_created` (`fid`,`displayorder`,`dateline`,`tid`,`visibility`,`authorid`),
  KEY `gonof_thread_category_latest` (`category`,`displayorder`,`lastpost`,`tid`,`visibility`,`fid`,`authorid`),
  KEY `gonof_thread_category_created` (`category`,`displayorder`,`dateline`,`tid`,`visibility`,`fid`,`authorid`),
  KEY `gonof_thread_family_category_latest` (`family_fid`,`category`,`displayorder`,`lastpost`,`tid`,`visibility`,`fid`,`authorid`),
  KEY `gonof_thread_family_category_created` (`family_fid`,`category`,`displayorder`,`dateline`,`tid`,`visibility`,`fid`,`authorid`),
  KEY `gonof_thread_forum_category_latest` (`fid`,`category`,`displayorder`,`lastpost`,`tid`,`visibility`,`authorid`),
  KEY `gonof_thread_forum_category_created` (`fid`,`category`,`displayorder`,`dateline`,`tid`,`visibility`,`authorid`),
  KEY `gonof_thread_featured` (`digest`,`displayorder`,`lastpost`,`tid`,`visibility`,`family_fid`,`fid`,`authorid`),
  KEY `gonof_thread_global_active` (`lastpost`,`displayorder`,`replies`,`tid`,`visibility`,`family_fid`,`fid`,`authorid`),
  CONSTRAINT `gonof_forum_thread_ibfk_1` FOREIGN KEY (`fid`) REFERENCES `gonof_forum_forum` (`fid`),
  CONSTRAINT `gonof_forum_thread_ibfk_2` FOREIGN KEY (`authorid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_thread_last_reply_fk` FOREIGN KEY (`last_reply_pid`) REFERENCES `gonof_forum_post` (`pid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_forum_thread_count` (
  `fid` bigint(20) NOT NULL,
  `visibility` varchar(24) COLLATE utf8mb4_bin NOT NULL,
  `category` bigint(20) NOT NULL,
  `threads` bigint(20) NOT NULL,
  PRIMARY KEY (`fid`,`visibility`,`category`),
  CONSTRAINT `gonof_forum_thread_count_ibfk_1` FOREIGN KEY (`fid`) REFERENCES `gonof_forum_forum` (`fid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_forum_thread_image` (
  `tid` bigint(20) NOT NULL,
  `position` bigint(20) NOT NULL,
  `aid` bigint(20) NOT NULL,
  PRIMARY KEY (`tid`,`position`),
  KEY `gonof_thread_image_attachment` (`aid`),
  CONSTRAINT `gonof_forum_thread_image_ibfk_1` FOREIGN KEY (`tid`) REFERENCES `gonof_forum_thread` (`tid`),
  CONSTRAINT `gonof_forum_thread_image_ibfk_2` FOREIGN KEY (`aid`) REFERENCES `gonof_forum_attachment` (`aid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_home_favorite` (
  `uid` bigint(20) NOT NULL,
  `tid` bigint(20) NOT NULL,
  `dateline` bigint(20) NOT NULL,
  PRIMARY KEY (`uid`,`tid`),
  KEY `tid` (`tid`),
  KEY `gonof_favorite_list` (`uid`,`dateline`,`tid`),
  CONSTRAINT `gonof_home_favorite_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_home_favorite_ibfk_2` FOREIGN KEY (`tid`) REFERENCES `gonof_forum_thread` (`tid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_home_feed_token` (
  `uid` bigint(20) NOT NULL,
  `token_hash` char(64) COLLATE utf8mb4_bin NOT NULL,
  `token_hint` char(8) COLLATE utf8mb4_bin NOT NULL,
  `include_members` bigint(20) NOT NULL DEFAULT '0',
  `dateline` bigint(20) NOT NULL,
  PRIMARY KEY (`uid`),
  UNIQUE KEY `gonof_feed_token_hash` (`token_hash`),
  CONSTRAINT `gonof_home_feed_token_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_home_forum_filter` (
  `uid` bigint(20) NOT NULL,
  `fid` bigint(20) NOT NULL,
  `filter_mode` varchar(8) COLLATE utf8mb4_bin NOT NULL,
  `dateline` bigint(20) NOT NULL,
  PRIMARY KEY (`uid`,`fid`),
  KEY `fid` (`fid`),
  CONSTRAINT `gonof_home_forum_filter_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_home_forum_filter_ibfk_2` FOREIGN KEY (`fid`) REFERENCES `gonof_forum_forum` (`fid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_home_forum_follow` (
  `id` bigint(20) NOT NULL AUTO_INCREMENT,
  `uid` bigint(20) NOT NULL,
  `fid` bigint(20) NOT NULL,
  `dateline` bigint(20) NOT NULL,
  PRIMARY KEY (`id`),
  UNIQUE KEY `uid` (`uid`,`fid`),
  KEY `gonof_forum_follow_delivery` (`fid`,`id`),
  CONSTRAINT `gonof_home_forum_follow_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_home_forum_follow_ibfk_2` FOREIGN KEY (`fid`) REFERENCES `gonof_forum_forum` (`fid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_home_interaction_request` (
  `uid` bigint(20) NOT NULL,
  `request_key` varchar(64) COLLATE utf8mb4_bin NOT NULL,
  `request_hash` varchar(64) COLLATE utf8mb4_bin NOT NULL,
  `dateline` bigint(20) NOT NULL,
  PRIMARY KEY (`uid`,`request_key`),
  KEY `gonof_interaction_request_dateline` (`dateline`),
  CONSTRAINT `gonof_home_interaction_request_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_home_member_block` (
  `uid` bigint(20) NOT NULL,
  `blocked_uid` bigint(20) NOT NULL,
  `dateline` bigint(20) NOT NULL,
  PRIMARY KEY (`uid`,`blocked_uid`),
  KEY `blocked_uid` (`blocked_uid`),
  CONSTRAINT `gonof_home_member_block_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_home_member_block_ibfk_2` FOREIGN KEY (`blocked_uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_home_member_follow` (
  `uid` bigint(20) NOT NULL,
  `followed_uid` bigint(20) NOT NULL,
  `dateline` bigint(20) NOT NULL,
  PRIMARY KEY (`uid`,`followed_uid`),
  KEY `gonof_member_follow_mine` (`uid`,`dateline`,`followed_uid`),
  KEY `gonof_member_follow_fans` (`followed_uid`,`dateline`,`uid`),
  CONSTRAINT `gonof_home_member_follow_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_home_member_follow_ibfk_2` FOREIGN KEY (`followed_uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_home_message` (
  `id` bigint(20) NOT NULL AUTO_INCREMENT,
  `thread_id` bigint(20) NOT NULL,
  `sender_uid` bigint(20) NOT NULL,
  `body` text COLLATE utf8mb4_bin NOT NULL,
  `dateline` bigint(20) NOT NULL,
  `event_key` varchar(100) COLLATE utf8mb4_bin NOT NULL,
  PRIMARY KEY (`id`),
  UNIQUE KEY `event_key` (`event_key`),
  KEY `gonof_message_thread` (`thread_id`,`id`),
  KEY `sender_uid` (`sender_uid`),
  CONSTRAINT `gonof_home_message_ibfk_1` FOREIGN KEY (`thread_id`) REFERENCES `gonof_home_message_thread` (`id`),
  CONSTRAINT `gonof_home_message_ibfk_2` FOREIGN KEY (`sender_uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_home_message_member` (
  `thread_id` bigint(20) NOT NULL,
  `uid` bigint(20) NOT NULL,
  `counterpart_uid` bigint(20) NOT NULL,
  `unread` bigint(20) NOT NULL DEFAULT '0',
  `last_read_id` bigint(20) NOT NULL DEFAULT '0',
  `last_dateline` bigint(20) NOT NULL,
  PRIMARY KEY (`thread_id`,`uid`),
  KEY `gonof_message_member_inbox` (`uid`,`last_dateline`,`thread_id`),
  KEY `gonof_message_member_unread` (`uid`,`unread`),
  KEY `counterpart_uid` (`counterpart_uid`),
  CONSTRAINT `gonof_home_message_member_ibfk_1` FOREIGN KEY (`thread_id`) REFERENCES `gonof_home_message_thread` (`id`),
  CONSTRAINT `gonof_home_message_member_ibfk_2` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_home_message_member_ibfk_3` FOREIGN KEY (`counterpart_uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_home_message_thread` (
  `id` bigint(20) NOT NULL AUTO_INCREMENT,
  `pair_key` varchar(48) COLLATE utf8mb4_bin NOT NULL,
  `last_message_id` bigint(20) NOT NULL DEFAULT '0',
  `dateline` bigint(20) NOT NULL,
  PRIMARY KEY (`id`),
  UNIQUE KEY `pair_key` (`pair_key`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_home_notification` (
  `id` bigint(20) NOT NULL AUTO_INCREMENT,
  `uid` bigint(20) NOT NULL,
  `authorid` bigint(20) NOT NULL,
  `pid` bigint(20) DEFAULT NULL,
  `type` varchar(40) COLLATE utf8mb4_bin NOT NULL DEFAULT 'reply',
  `event_key` varchar(100) COLLATE utf8mb4_bin NOT NULL,
  `dateline` bigint(20) NOT NULL,
  `read_at` bigint(20) NOT NULL DEFAULT '0',
  `report_id` bigint(20) DEFAULT NULL,
  `sanction_id` bigint(20) DEFAULT NULL,
  `reason` varchar(1200) COLLATE utf8mb4_bin NOT NULL DEFAULT '',
  PRIMARY KEY (`id`),
  UNIQUE KEY `uid` (`uid`,`event_key`),
  KEY `authorid` (`authorid`),
  KEY `pid` (`pid`),
  KEY `gonof_notification_list` (`uid`,`dateline`,`id`),
  KEY `gonof_notification_unread` (`uid`,`read_at`,`dateline`,`id`),
  KEY `gonof_notification_report_fk` (`report_id`),
  KEY `gonof_notification_sanction_fk` (`sanction_id`),
  CONSTRAINT `gonof_home_notification_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_home_notification_ibfk_2` FOREIGN KEY (`authorid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_home_notification_ibfk_3` FOREIGN KEY (`pid`) REFERENCES `gonof_forum_post` (`pid`),
  CONSTRAINT `gonof_notification_report_fk` FOREIGN KEY (`report_id`) REFERENCES `gonof_common_report` (`id`),
  CONSTRAINT `gonof_notification_sanction_fk` FOREIGN KEY (`sanction_id`) REFERENCES `gonof_common_member_sanction` (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_home_notification_preference` (
  `uid` bigint(20) NOT NULL,
  `site_reply` bigint(20) NOT NULL DEFAULT '1',
  `site_quote` bigint(20) NOT NULL DEFAULT '1',
  `site_mention` bigint(20) NOT NULL DEFAULT '1',
  `site_thread` bigint(20) NOT NULL DEFAULT '1',
  `site_forum` bigint(20) NOT NULL DEFAULT '1',
  `email_reply` bigint(20) NOT NULL DEFAULT '0',
  `email_quote` bigint(20) NOT NULL DEFAULT '0',
  `email_mention` bigint(20) NOT NULL DEFAULT '0',
  `email_thread` bigint(20) NOT NULL DEFAULT '0',
  `email_forum` bigint(20) NOT NULL DEFAULT '0',
  `version` bigint(20) NOT NULL DEFAULT '1',
  `updated_at` bigint(20) NOT NULL,
  `email_message` bigint(20) NOT NULL DEFAULT '0',
  `email_login` bigint(20) NOT NULL DEFAULT '1',
  PRIMARY KEY (`uid`),
  CONSTRAINT `gonof_home_notification_preference_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_home_reading_preference` (
  `uid` bigint(20) NOT NULL,
  `list_sort` varchar(16) COLLATE utf8mb4_bin DEFAULT NULL,
  `thread_view` varchar(16) COLLATE utf8mb4_bin DEFAULT NULL,
  `title_latest` bigint(20) DEFAULT NULL,
  `title_new_tab` bigint(20) DEFAULT NULL,
  `version` bigint(20) NOT NULL DEFAULT '1',
  `updated_at` bigint(20) NOT NULL,
  `avatar_shape` varchar(16) COLLATE utf8mb4_bin DEFAULT NULL,
  `custom_css` text COLLATE utf8mb4_bin,
  `custom_css_version` bigint(20) NOT NULL DEFAULT '0',
  PRIMARY KEY (`uid`),
  CONSTRAINT `gonof_home_reading_preference_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_home_thread_follow` (
  `id` bigint(20) NOT NULL AUTO_INCREMENT,
  `uid` bigint(20) NOT NULL,
  `tid` bigint(20) NOT NULL,
  `dateline` bigint(20) NOT NULL,
  PRIMARY KEY (`id`),
  UNIQUE KEY `uid` (`uid`,`tid`),
  KEY `gonof_thread_follow_delivery` (`tid`,`id`),
  CONSTRAINT `gonof_home_thread_follow_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`),
  CONSTRAINT `gonof_home_thread_follow_ibfk_2` FOREIGN KEY (`tid`) REFERENCES `gonof_forum_thread` (`tid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

CREATE TABLE `gonof_rank_snapshot` (
  `board` varchar(16) COLLATE utf8mb4_bin NOT NULL,
  `period` varchar(8) COLLATE utf8mb4_bin NOT NULL,
  `position` bigint(20) NOT NULL,
  `uid` bigint(20) NOT NULL,
  `score` bigint(20) NOT NULL,
  `computed_at` bigint(20) NOT NULL,
  PRIMARY KEY (`board`,`period`,`position`),
  KEY `uid` (`uid`),
  CONSTRAINT `gonof_rank_snapshot_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `gonof_common_member` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin ROW_FORMAT=DYNAMIC;

SET FOREIGN_KEY_CHECKS = 1;

-- Seed rows: site settings, credit rules, site-wide content revision.
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
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.seo.robots','User-agent: *\nDisallow: /manage\nDisallow: /my\nDisallow: /data\nDisallow: /appeals\nDisallow: /search\nDisallow: /api\nDisallow: /login\nDisallow: /register\nDisallow: /forgot-password\nDisallow: /reset-password\nDisallow: /verify-email');
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
