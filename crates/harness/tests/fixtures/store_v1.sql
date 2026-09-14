-- Convert a populated current Store into the exact schema shipped as version 1.
-- Tests use public Store APIs to create valid task and session aggregates first,
-- then apply this fixture so the migration starts from realistic serialized data.
PRAGMA foreign_keys=OFF;
BEGIN IMMEDIATE;
DROP TABLE audit_epochs;
DROP TABLE cohort_ledger_uses;
DROP TABLE cohort_ledgers;
DROP TABLE adaptive_score_reports;
DROP TABLE campaigns;
DROP TABLE evolution_evidence;
DROP TABLE evolution_artifact_reservations;
DROP TABLE evaluation_cohorts;
DROP TABLE harness_target_revisions;
DROP TABLE harness_targets;
DROP TABLE harness_revisions;
ALTER TABLE events RENAME TO events_v2;
CREATE TABLE events(
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    aggregate TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('task','session')),
    revision INTEGER NOT NULL,
    event BLOB NOT NULL,
    hash TEXT NOT NULL,
    UNIQUE(aggregate,kind,revision)
) STRICT;
INSERT INTO events(sequence,aggregate,kind,revision,event,hash)
SELECT sequence,aggregate,kind,revision,event,hash FROM events_v2 ORDER BY sequence;
DROP TABLE events_v2;
PRAGMA user_version=1;
COMMIT;
PRAGMA foreign_keys=ON;
