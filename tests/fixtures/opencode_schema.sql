-- OpenCode opencode.db schema fixture.
--
-- Captured live, read-only, schema only (no rows) via:
--   sqlite3 "file:~/.local/share/opencode/opencode.db?mode=ro" .schema
--
-- Path note: hermon.py's default is the Linux XDG path
-- ~/.local/share/opencode/opencode.db. On this machine that same path
-- exists and holds the live database, so no macOS-specific path
-- (e.g. ~/Library/Application Support/opencode/) was needed.
--
-- Trimmed to the tables hermon.py reads (OpenCodeSource, hermon.py:615):
-- `session`, `message`, and `part`, plus their indexes. The live database
-- has many other tables (project, todo, session_share, control_account,
-- account, account_state, event_sequence, event, workspace,
-- session_message, data_migration, permission, project_directory,
-- session_input, session_context_epoch, migration) that hermon never
-- queries; they are omitted here for a smaller, faithful fixture. Foreign
-- keys on the kept tables that reference an omitted table (e.g.
-- session.project_id -> project(id)) are left as-is; SQLite does not
-- require the referenced table to exist to create the table.
CREATE TABLE `message` (
          `id` text PRIMARY KEY,
          `session_id` text NOT NULL,
          `time_created` integer NOT NULL,
          `time_updated` integer NOT NULL,
          `data` text NOT NULL,
          CONSTRAINT `fk_message_session_id_session_id_fk` FOREIGN KEY (`session_id`) REFERENCES `session`(`id`) ON DELETE CASCADE
        );
CREATE TABLE `part` (
          `id` text PRIMARY KEY,
          `message_id` text NOT NULL,
          `session_id` text NOT NULL,
          `time_created` integer NOT NULL,
          `time_updated` integer NOT NULL,
          `data` text NOT NULL,
          CONSTRAINT `fk_part_message_id_message_id_fk` FOREIGN KEY (`message_id`) REFERENCES `message`(`id`) ON DELETE CASCADE
        );
CREATE TABLE `session` (
          `id` text PRIMARY KEY,
          `project_id` text NOT NULL,
          `parent_id` text,
          `slug` text NOT NULL,
          `directory` text NOT NULL,
          `title` text NOT NULL,
          `version` text NOT NULL,
          `share_url` text,
          `summary_additions` integer,
          `summary_deletions` integer,
          `summary_files` integer,
          `summary_diffs` text,
          `revert` text,
          `permission` text,
          `time_created` integer NOT NULL,
          `time_updated` integer NOT NULL,
          `time_compacting` integer,
          `time_archived` integer, `workspace_id` text, `path` text, `agent` text, `model` text, `cost` real DEFAULT 0 NOT NULL, `tokens_input` integer DEFAULT 0 NOT NULL, `tokens_output` integer DEFAULT 0 NOT NULL, `tokens_reasoning` integer DEFAULT 0 NOT NULL, `tokens_cache_read` integer DEFAULT 0 NOT NULL, `tokens_cache_write` integer DEFAULT 0 NOT NULL, `metadata` text,
          CONSTRAINT `fk_session_project_id_project_id_fk` FOREIGN KEY (`project_id`) REFERENCES `project`(`id`) ON DELETE CASCADE
        );
CREATE INDEX `part_session_idx` ON `part` (`session_id`);
CREATE INDEX `session_project_idx` ON `session` (`project_id`);
CREATE INDEX `session_parent_idx` ON `session` (`parent_id`);
CREATE INDEX `session_workspace_idx` ON `session` (`workspace_id`);
CREATE INDEX `message_session_time_created_id_idx` ON `message` (`session_id`,`time_created`,`id`);
CREATE INDEX `part_message_id_id_idx` ON `part` (`message_id`,`id`);
