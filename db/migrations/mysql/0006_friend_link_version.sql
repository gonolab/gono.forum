-- A friend link's version, raised by every write to the row. The admin forms carry the version they were read at,
-- and saving from a form whose version no longer matches is refused, so an older form cannot silently put back fields
-- another administrator just changed (links.rs, `decide`).
ALTER TABLE `gonof_common_friend_link`
  ADD `version` bigint(20) NOT NULL DEFAULT '0';
