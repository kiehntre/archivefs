ALTER TABLE archives ADD COLUMN identity_report_json BLOB;
ALTER TABLE archives ADD COLUMN identity_report_size_bytes INTEGER;
ALTER TABLE archives ADD COLUMN identity_report_modified_time_unix_seconds INTEGER;
