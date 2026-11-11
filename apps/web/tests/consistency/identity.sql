-- Identity and moderation consistency check: read-only SELECTs, one row per item.
--
-- `test_support::check_script` runs this file against a test database after the tests have written through the real
-- code paths (`tests/identity_rules.rs` runs it at the end of every test, other tests use it too) and returns each
-- item with its amount. Every amount must be 0: anything else means a row that breaks the rules below reached the
-- database by bypassing code. The checks use MySQL syntax and run on MySQL only, on SQLite the helper returns no rows.
-- When you change a matching rule in code, change the check here too.
--
-- The schema has no triggers (docs/architecture.md, "Data"), so the rules on these 15 tables are guaranteed only by
-- code at write time: apps/web/src/identity/rules.rs, the validation at each entry point and the conditional updates
-- on each write path. Code only checks new writes and never corrects old values, so every stored row must satisfy the
-- same rules.
-- Each item checks the rules of its table clause by clause. Enumerations and "no leading or trailing whitespace"
-- compare bytes (code does not let through spellings that differ only by trailing spaces under PAD SPACE).
-- Upper length limits given by column widths (over-long values fail under STRICT_TRANS_TABLES) are enforced by the
-- database, so only lower limits, combinations and values are checked here.

-- 1. Members
SELECT 'members with invalid values' AS item, COUNT(*) AS amount
  FROM gonof_common_member
 WHERE NOT (emailstatus IN (0, 1) AND adminid IN (0, 1, 2, 3, 4, 5) AND freeze IN (0, 1)
   AND regdate >= 0 AND credentials_version > 0 AND avatar >= 0
   AND CHAR_LENGTH(username) BETWEEN 1 AND 50 AND CAST(username AS BINARY) = CAST(TRIM(username) AS BINARY)
   AND (email IS NULL OR (CHAR_LENGTH(email) BETWEEN 3 AND 255 AND CAST(email AS BINARY) = CAST(TRIM(email) AS BINARY)))
   AND (password IS NULL OR CHAR_LENGTH(password) >= 1))
UNION ALL
-- Matches `Credentials` in identity/rules.rs, which counts mobile numbers and third-party bindings: a member without a
-- password needs a mobile number or a third-party binding, or is a synthetic author (no email, unverified, regular
-- member). A member with a password needs an email, a mobile number or a third-party binding, or is the site owner
-- (adminid=1).
SELECT 'members with an invalid combination of credentials', COUNT(*)
  FROM gonof_common_member m
 WHERE NOT ((m.emailstatus = 0 OR m.email IS NOT NULL)
   AND ((m.password IS NULL
         AND (m.mobile IS NOT NULL
           OR EXISTS (SELECT 1 FROM gonof_common_member_external x WHERE x.uid = m.uid)
           OR (m.email IS NULL AND m.emailstatus = 0 AND m.adminid = 0)))
     OR (m.password IS NOT NULL
         AND (m.email IS NOT NULL OR m.mobile IS NOT NULL OR m.adminid = 1
           OR EXISTS (SELECT 1 FROM gonof_common_member_external x WHERE x.uid = m.uid)))))
UNION ALL
SELECT 'members with an invalid space visibility or account state', COUNT(*)
  FROM gonof_common_member
 WHERE CAST(space_visibility AS BINARY) NOT IN ('public', 'members', 'private')
    OR CAST(account_state AS BINARY) NOT IN ('active', 'deleting', 'deleted')
UNION ALL
SELECT 'deleted members that still keep an email, password, avatar, bio or staff role', COUNT(*)
  FROM gonof_common_member
 WHERE account_state = 'deleted'
   AND (email IS NOT NULL OR password IS NOT NULL OR avatar_aid IS NOT NULL OR bio <> '' OR adminid <> 0)
UNION ALL
SELECT 'rename records with invalid values', COUNT(*)
  FROM gonof_common_member_username_history
 WHERE NOT (CHAR_LENGTH(username) BETWEEN 1 AND 50 AND CAST(username AS BINARY) = CAST(TRIM(username) AS BINARY)
   AND dateline >= 0 AND expires_at > dateline)
UNION ALL
SELECT 'role applications whose request key or digest is not 64 characters', COUNT(*)
  FROM gonof_common_member_role_request
 WHERE CHAR_LENGTH(request_key) <> 64 OR CHAR_LENGTH(request_hash) <> 64
UNION ALL
-- 2. Tokens, API keys and invite codes
SELECT 'member tokens with invalid values or an invalid used/revoked combination', COUNT(*)
  FROM gonof_common_member_token
 WHERE NOT (CAST(purpose AS BINARY) IN ('email_verification', 'password_reset', 'email_change', 'appeal_access', 'privacy_access')
   AND CHAR_LENGTH(token_hash) = 64 AND CHAR_LENGTH(email) BETWEEN 3 AND 255
   AND dateline >= 0 AND expires_at > dateline
   AND (consumed_at IS NULL OR consumed_at BETWEEN dateline AND expires_at)
   AND (revoked_at IS NULL OR revoked_at >= dateline)
   AND (consumed_at IS NULL OR revoked_at IS NULL))
UNION ALL
SELECT 'API tokens with invalid values', COUNT(*)
  FROM gonof_common_member_api_key
 WHERE NOT (id > 0 AND CHAR_LENGTH(key_hash) = 64 AND CHAR_LENGTH(key_hint) BETWEEN 1 AND 16
   AND dateline >= 0 AND expires_at > dateline
   AND (last_used_at = 0 OR last_used_at >= dateline)
   AND (revoked_at IS NULL OR revoked_at >= dateline)
   AND (revoked_by IS NULL OR revoked_at IS NOT NULL))
UNION ALL
-- A used invite code has both a user and a registration time, in every other state both are empty.
SELECT 'invite codes with invalid values or usage records', COUNT(*)
  FROM gonof_common_invite
 WHERE NOT (CHAR_LENGTH(code) BETWEEN 1 AND 64 AND CAST(code AS BINARY) = CAST(TRIM(code) AS BINARY)
   AND CHAR_LENGTH(request_key) BETWEEN 1 AND 64
   AND dateline >= 0 AND endtime > dateline
   AND (regdateline IS NULL OR regdateline BETWEEN dateline AND endtime)
   AND CAST(status AS BINARY) IN ('available', 'used', 'revoked', 'frozen')
   AND ((status = 'used' AND fuid IS NOT NULL AND regdateline IS NOT NULL)
     OR (status <> 'used' AND fuid IS NULL AND regdateline IS NULL)))
UNION ALL
-- 3. Sanctions, reports and appeals
-- Revocation: a sanction that is not revoked has revoked_at 0 and no revoker. A revoked one has revoked_at not before
-- the start and a revoker, or is a freeze that ended automatically on expiry (no revoker).
SELECT 'sanctions with invalid values or an invalid revocation combination', COUNT(*)
  FROM gonof_common_member_sanction
 WHERE NOT (CAST(kind AS BINARY) IN ('warning', 'freeze') AND uid <> operator_uid
   AND CHAR_LENGTH(TRIM(reason)) BETWEEN 1 AND 500 AND starts_at > 0
   AND (expires_at = 0 OR expires_at > starts_at)
   AND (kind = 'freeze' OR expires_at = 0)
   AND request_key REGEXP '^[0-9a-f]{64}$'
   AND ((revoked_at = 0 AND revoked_by IS NULL)
     OR (revoked_at >= starts_at
       AND (revoked_by IS NOT NULL OR (kind = 'freeze' AND expires_at > 0 AND revoked_at >= expires_at)))))
UNION ALL
-- Handling state: a pending item has no handler, handling time or resolution. A handled one has all three, and the
-- handling time is not before submission.
SELECT 'reports with invalid values or an invalid handling combination', COUNT(*)
  FROM gonof_common_report
 WHERE NOT (CAST(target_type AS BINARY) IN ('thread', 'reply', 'profile', 'attachment') AND target_id > 0
   AND CHAR_LENGTH(TRIM(reason)) BETWEEN 1 AND 1000 AND dateline > 0
   AND request_key REGEXP '^[0-9a-f]{64}$'
   AND ((CAST(status AS BINARY) = 'pending' AND handler_uid IS NULL AND handled_at = 0 AND resolution = '')
     OR (CAST(status AS BINARY) IN ('resolved', 'dismissed') AND handler_uid IS NOT NULL AND handled_at >= dateline
       AND CHAR_LENGTH(TRIM(resolution)) BETWEEN 1 AND 1000)))
UNION ALL
SELECT 'appeals with invalid values or an invalid handling combination', COUNT(*)
  FROM gonof_common_member_appeal
 WHERE NOT (CHAR_LENGTH(TRIM(reason)) BETWEEN 1 AND 1000 AND dateline > 0
   AND request_key REGEXP '^[0-9a-f]{64}$'
   AND ((CAST(status AS BINARY) = 'pending' AND handler_uid IS NULL AND handled_at = 0 AND resolution = '')
     OR (CAST(status AS BINARY) IN ('accepted', 'rejected') AND handler_uid IS NOT NULL AND handled_at >= dateline
       AND CHAR_LENGTH(TRIM(resolution)) BETWEEN 1 AND 1000)))
UNION ALL
-- The appealed sanction belongs to the appellant, and the appeal was submitted within 30 days of the sanction start
-- (up to the end of day 30, the same as APPEAL_WINDOW in moderation/appeals.rs).
SELECT 'appeals against a sanction of another member or after the 30-day appeal period', COUNT(*)
  FROM gonof_common_member_appeal a
 WHERE NOT EXISTS (SELECT 1 FROM gonof_common_member_sanction s
                    WHERE s.id = a.sanction_id AND s.uid = a.uid
                      AND a.dateline >= s.starts_at AND a.dateline <= s.starts_at + 2592000)
UNION ALL
-- 4. Data requests
SELECT 'data requests with invalid values', COUNT(*)
  FROM gonof_common_data_request
 WHERE NOT (CAST(kind AS BINARY) IN ('export', 'close', 'erase')
   AND CAST(content_mode AS BINARY) IN ('retain', 'erase')
   AND CAST(status AS BINARY) IN ('pending', 'cooling', 'processing', 'ready', 'completed', 'cancelled', 'failed', 'expired')
   AND version >= 1 AND dateline >= 0 AND execute_after >= dateline AND finished_at >= 0
   AND cursor_pid >= 0 AND upper_pid >= 0
   AND request_key REGEXP '^[0-9a-f]{64}$' AND CHAR_LENGTH(request_hash) = 64)
UNION ALL
-- 5. Logs, outbox, settings and site assets
SELECT 'action log rows with invalid values', COUNT(*)
  FROM gonof_common_log
 WHERE NOT (CHAR_LENGTH(type) BETWEEN 1 AND 100 AND CHAR_LENGTH(source) BETWEEN 1 AND 32
   AND JSON_VALID(data) AND dateline >= 0)
UNION ALL
SELECT 'Outbox events with invalid values', COUNT(*)
  FROM gonof_common_outbox
 WHERE NOT (CHAR_LENGTH(event_key) BETWEEN 1 AND 160
   AND CHAR_LENGTH(event_type) BETWEEN 1 AND 80 AND CHAR_LENGTH(aggregate_type) BETWEEN 1 AND 40
   AND aggregate_id > 0 AND JSON_VALID(payload)
   AND dateline >= 0 AND available_at >= dateline AND attempts >= 0 AND lease_until >= 0
   AND (lease_token IS NULL OR CHAR_LENGTH(lease_token) BETWEEN 1 AND 64)
   AND CAST(status AS BINARY) IN ('pending', 'processing', 'sent', 'failed')
   AND (last_error IS NULL OR CHAR_LENGTH(last_error) <= 1000))
UNION ALL
SELECT 'settings with an empty key', COUNT(*)
  FROM gonof_common_settings
 WHERE CHAR_LENGTH(skey) NOT BETWEEN 1 AND 190
UNION ALL
SELECT 'site assets with an invalid slot or version', COUNT(*)
  FROM gonof_common_site_asset
 WHERE CAST(slot AS BINARY) NOT IN ('logo', 'favicon', 'app_icon', 'share', 'email') OR version < 1 OR dateline < 0
UNION ALL
-- Trigger total, left over from the old trigger removal checks. `test_support::check_script` filters this row out.
SELECT 'trigger count minus 63', COUNT(*) - 63
  FROM information_schema.TRIGGERS
 WHERE TRIGGER_SCHEMA = DATABASE();
