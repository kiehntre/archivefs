-- Migration 0003: per-source-folder scan status columns.
--
-- Durable, per-folder "what happened last time we tried to scan this
-- source" state - the multi-source milestone's Sources page shows this
-- immediately on open without forcing a fresh scan. Deliberately DB-owned
-- rather than config-owned: this is scan-history-derived fact (like
-- scan_runs already is), not user intent (which stays in config.toml as
-- each source's `enabled` flag).
--
-- last_scan_status is 'success' or 'failed' - free text detail (including
-- distinguishing "unavailable" from "permission denied") lives in
-- last_scan_error, since that classification is done in Rust from the
-- underlying io::Error, not re-derived from SQL. last_archive_count is the
-- exact count persisted by that successful scan, not a live re-count, so
-- it still reflects the last known state while a source is offline.
ALTER TABLE source_folders ADD COLUMN last_scan_status TEXT;
ALTER TABLE source_folders ADD COLUMN last_scan_error TEXT;
ALTER TABLE source_folders ADD COLUMN last_scan_at TEXT;
ALTER TABLE source_folders ADD COLUMN last_successful_scan_at TEXT;
ALTER TABLE source_folders ADD COLUMN last_archive_count INTEGER;
