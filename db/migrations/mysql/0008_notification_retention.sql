-- Retention of site notifications (docs/queries.md, "Notification retention"): a read notification is
-- deleted this many days after it was read, an unread one this many days after it arrived. Both are settings of
-- Tools & maintenance › Maintenance; the cleanup deletes in bounded batches and keeps the unread counts in step.
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.maintenance.notification_read_retention_days','30');
INSERT INTO `gonof_common_settings` (`skey`, `svalue`) VALUES ('settings.maintenance.notification_unread_retention_days','180');
