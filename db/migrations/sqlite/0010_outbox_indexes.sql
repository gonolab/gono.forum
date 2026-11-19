-- Two indexes on the Outbox; same structure as db/migrations/mysql/0010_outbox_indexes.sql.
CREATE INDEX `gonof_common_outbox__gonof_outbox_claim` ON `gonof_common_outbox` (`event_type`, `status`, `available_at`, `eventid`);
CREATE INDEX `gonof_common_outbox__gonof_outbox_aggregate` ON `gonof_common_outbox` (`aggregate_type`, `aggregate_id`);
