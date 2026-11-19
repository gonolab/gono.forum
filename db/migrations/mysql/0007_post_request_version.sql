-- The post version an API edit left behind, kept with its request record so that replaying the edit returns the
-- version that edit wrote, not a later one (content/edits.rs, `replay`). NULL for every other write that keeps its
-- request here (posting, replies, moderation actions, likes, tips, activities), and for edit records written before
-- this migration: replaying one of those falls back to the post's current version.
ALTER TABLE `gonof_forum_post_request`
  ADD `version` bigint(20) DEFAULT NULL;
