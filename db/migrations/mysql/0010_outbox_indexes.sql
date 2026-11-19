-- Two indexes on the Outbox (docs/queries.md, "Outbox claims"). `gonof_outbox_claim` gives each worker the due events
-- of its own types in due order, one short range per type and status, so a claim no longer reads the backlog of other
-- queues or the sent rows kept for the retention period. `gonof_outbox_aggregate` finds a member's events when an
-- account is erased, instead of a scan of the whole table.
--
-- Both are built in place without blocking reads and writes (docs/install.md, "Upgrades"); the statement fails rather
-- than fall back to a copy that would lock the table.
ALTER TABLE `gonof_common_outbox`
  ADD KEY `gonof_outbox_claim` (`event_type`,`status`,`available_at`,`eventid`),
  ADD KEY `gonof_outbox_aggregate` (`aggregate_type`,`aggregate_id`),
  ALGORITHM=INPLACE, LOCK=NONE;
