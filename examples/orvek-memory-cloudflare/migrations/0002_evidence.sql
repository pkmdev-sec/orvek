-- Legacy records remain unscoped and unverified.
ALTER TABLE memories ADD COLUMN metadata TEXT NOT NULL DEFAULT '{}';
