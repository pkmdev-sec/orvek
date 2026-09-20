-- Owning record identities survive export and distinguish independent databases.
UPDATE memories SET metadata = json_set(metadata, '$.ownership_id', lower(hex(randomblob(16))))
WHERE json_extract(metadata, '$.ownership_id') IS NULL;
