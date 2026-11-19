-- Retention of site notifications; same rows as db/migrations/mysql/0008_notification_retention.sql.
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.maintenance.notification_read_retention_days','30');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.maintenance.notification_unread_retention_days','180');
