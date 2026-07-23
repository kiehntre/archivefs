ALTER TABLE scan_runs ADD COLUMN archives_unchanged INTEGER NOT NULL DEFAULT 0;
ALTER TABLE scan_runs ADD COLUMN skipped_unsupported_extension INTEGER NOT NULL DEFAULT 0;
ALTER TABLE scan_runs ADD COLUMN skipped_ambiguous_platform INTEGER NOT NULL DEFAULT 0;
